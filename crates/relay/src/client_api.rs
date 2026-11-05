//! Client credentials are independent of the local management session.
use crate::{admin::AdminState, error::Error, operations::ClientAccess};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn client_error_payload(error: &Error) -> (StatusCode, Value) {
    let (status, kind) = match error.code.as_str() {
        "INVALID_API_KEY" => (StatusCode::UNAUTHORIZED, "authentication_error"),
        "API_KEY_GROUP_DISABLED" | "MODEL_NOT_ALLOWED" | "ORIGIN_REJECTED" => {
            (StatusCode::FORBIDDEN, "permission_error")
        }
        "INVALID_INPUT" | "SERVICE_TIER_UNAVAILABLE" => {
            (StatusCode::BAD_REQUEST, "invalid_request_error")
        }
        "ENDPOINT_NOT_SUPPORTED" => (StatusCode::NOT_FOUND, "invalid_request_error"),
        "METHOD_NOT_ALLOWED" => (StatusCode::METHOD_NOT_ALLOWED, "invalid_request_error"),
        "UNSUPPORTED_MEDIA_TYPE" => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "invalid_request_error"),
        "RESPONSE_CHAIN_MISMATCH" | "RESPONSE_NOT_ACTIVE" | "RESPONSE_CONTINUATION_BUSY" => {
            (StatusCode::CONFLICT, "invalid_request_error")
        }
        "PAYLOAD_TOO_LARGE" => (StatusCode::PAYLOAD_TOO_LARGE, "invalid_request_error"),
        "RESPONSE_NOT_FOUND" => (StatusCode::NOT_FOUND, "invalid_request_error"),
        "RESPONSE_STORAGE_FULL" => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        "QUEUE_FULL" | "CONCURRENCY_LIMIT" => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        "QUEUE_TIMEOUT" | "UPSTREAM_TIMEOUT" => (StatusCode::GATEWAY_TIMEOUT, "server_error"),
        "UPSTREAM_RATE_LIMITED" => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        "UPSTREAM_AUTH_FAILED"
        | "UPSTREAM_REQUEST_REJECTED"
        | "UPSTREAM_CONNECTION_FAILED"
        | "UPSTREAM_INVALID_RESPONSE" => (StatusCode::BAD_GATEWAY, "server_error"),
        "REQUEST_CANCELLED" => (StatusCode::CONFLICT, "server_error"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
    };
    (
        status,
        json!({"error":{"message":error.message,"type":kind,"param":Value::Null,"code":error.code}}),
    )
}
pub(crate) fn client_error(error: Error) -> Response {
    let (status, body) = client_error_payload(&error);
    let mut response = (status, Json(body)).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS && error.code != "UPSTREAM_RATE_LIMITED" {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    }
    response
}
async fn request_context(mut request: Request, next: Next) -> Response {
    let context = crate::execution::RequestContext {
        endpoint: Some(
            request
                .extensions()
                .get::<axum::extract::OriginalUri>()
                .map_or_else(|| request.uri().path(), |uri| uri.0.path())
                .to_owned(),
        ),
        identity_headers: crate::responses::affinity::identity::Headers::parse(request.headers()),
        subagent_kind: crate::subagent::from_headers(request.headers()),
        user_agent: request
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                let mut agent: String = value.chars().take(512).collect();
                if value.len() > agent.len() {
                    agent.push('…');
                }
                agent
            }),
        ..Default::default()
    };
    let id = context.id.to_string();
    request.extensions_mut().insert(context);
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-request-id", id.parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
async fn usage(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
) -> Response {
    match state
        .operations
        .run_history_query(move |ops| ops.client_usage(&access))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => client_error(e),
    }
}
async fn authorize(
    State(state): State<Arc<AdminState>>,
    Extension(policy): Extension<crate::network::AccessPolicy>,
    mut request: Request,
    next: Next,
) -> Response {
    if !policy.allows_with_public(request.headers(), state.access.public()) {
        return client_error(Error::new(
            "ORIGIN_REJECTED",
            "请求的 Host 或 Origin 未被此 API 入口允许。",
        ));
    }
    let headers = request.headers();
    let token = if headers.get_all(header::AUTHORIZATION).iter().count() == 1 {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
    } else {
        None
    };
    let access = match state.operations.authenticate_api_key(token.unwrap_or("")) {
        Ok(access) => access,
        Err(error) => return client_error(error),
    };
    request.extensions_mut().insert(access);
    next.run(request).await
}
async fn models(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
) -> Json<Value> {
    crate::upstream::refresh_models(&state, &access).await;
    Json(
        json!({"object":"list","data":state.operations.inference_models(Some(&access)).iter().map(|id|json!({"id":id,"object":"model","created":0,"owned_by":"native-provider"})).collect::<Vec<_>>()}),
    )
}
pub fn router(state: Arc<AdminState>) -> Router<Arc<AdminState>> {
    let policy = state.local_access_policy();
    router_with_policy(state, policy)
}
async fn unsupported(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
) -> Response {
    rejected_endpoint(
        state,
        access,
        context,
        "ENDPOINT_NOT_SUPPORTED",
        "中转站未实现此 API 路径，请检查客户端接口配置。",
    )
}
async fn wrong_method(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
) -> Response {
    rejected_endpoint(
        state,
        access,
        context,
        "METHOD_NOT_ALLOWED",
        "此 API 不支持当前 HTTP 方法。",
    )
}
fn rejected_endpoint(
    state: Arc<AdminState>,
    access: ClientAccess,
    context: crate::execution::RequestContext,
    code: &str,
    message: &str,
) -> Response {
    // 只记录已认证请求；不持久化任意 URL、查询字符串或请求正文中的凭据。
    let error = Error::new(code, message);
    let error = state
        .operations
        .record_client_rejection(&context, &access, "api.rejected", None, &error)
        .err()
        .unwrap_or(error);
    client_error(error)
}
pub(crate) fn router_with_policy(
    state: Arc<AdminState>,
    policy: crate::network::AccessPolicy,
) -> Router<Arc<AdminState>> {
    Router::new()
        .route("/models", get(models))
        .route("/usage", get(usage))
        .route("/images/generations", post(crate::images::generate))
        .route("/images/edits", post(crate::images::edit))
        .route("/alpha/search", post(crate::search::create))
        .route("/native", get(crate::native_session::endpoint))
        .route(
            "/responses",
            post(crate::responses::create).get(crate::responses::websocket::endpoint),
        )
        .route("/responses/compact", post(crate::responses::compact))
        .route(
            "/responses/{id}",
            get(crate::responses::retrieve).delete(crate::responses::delete),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::upstream::dispatch::middleware,
        ))
        .fallback(unsupported)
        .method_not_allowed_fallback(wrong_method)
        // fallback 也必须经过认证，不能再落入外层管理台静态文件服务。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::debug_capture::middleware,
        ))
        .layer(middleware::from_fn_with_state(state, authorize))
        .layer(DefaultBodyLimit::max(crate::media::BODY_LIMIT))
        .layer(Extension(policy))
        .layer(middleware::from_fn(request_context))
}
