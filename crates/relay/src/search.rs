//! Codex 桌面独立搜索协议。保留查询、引用与加密结果，不把搜索伪装成 Responses 推理。
use crate::{
    admin::AdminState,
    error::{Error, Result},
    execution::{ExecutionOutcome, RequestContext},
    inference::{CancelOnDrop, RequestJob},
    operations::{ClientAccess, Feature, RequestClass, RoutingRequest},
};
use axum::{
    body::{Bytes, to_bytes},
    extract::{Extension, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) struct SearchRequest {
    pub model: String,
    pub binding: String,
    pub body: Bytes,
}
pub(crate) struct SearchResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub value: Value,
}
impl SearchRequest {
    async fn parse(request: Request) -> Result<Self> {
        let ct = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !ct
            .split(';')
            .next()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "搜索请求须使用 application/json。",
            ));
        }
        let encoding = request
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = to_bytes(request.into_body(), crate::media::BODY_LIMIT)
            .await
            .map_err(|_| Error::new("PAYLOAD_TOO_LARGE", "搜索请求超过 32 MiB 或读取失败。"))?;
        let value = crate::responses::decode_body(&body, &encoding)?;
        let id = value["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
            .ok_or_else(|| Error::new("INVALID_INPUT", "请提供有效的搜索会话 id。"))?;
        let model = value["model"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 256
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._:/-".contains(&c))
            })
            .ok_or_else(|| Error::new("INVALID_INPUT", "请提供有效的搜索 model。"))?
            .to_owned();
        if !value["commands"].is_object()
            && !value["input"].is_string()
            && !value["input"].is_array()
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "搜索请求缺少 commands 对象或 input 上下文。",
            ));
        }
        if value
            .get("stream")
            .is_some_and(|v| !v.is_null() && *v != false)
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "独立搜索仅支持完整 JSON 响应。",
            ));
        }
        // 只保存会话标识的摘要；规范化 JSON 确保准入校验与实际转发采用相同字段值。
        Ok(Self {
            model,
            binding: hex::encode(Sha256::digest(id.as_bytes())),
            body: serde_json::to_vec(&value)?.into(),
        })
    }
}
impl SearchResponse {
    fn error(&self) -> Option<Error> {
        if !self.status.is_success() {
            Some(crate::upstream::status_error(self.status))
        } else if !self.value["error"].is_null() {
            Some(Error::new("UPSTREAM_REQUEST_FAILED", "上游搜索返回错误。"))
        } else {
            None
        }
    }
    fn response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        for name in [
            "retry-after",
            "x-request-id",
            "x-codex-relay-upstream-transport",
        ] {
            if let Some(value) = self.headers.get(name) {
                response.headers_mut().insert(
                    if name == "x-request-id" {
                        "x-upstream-request-id"
                    } else {
                        name
                    },
                    value.clone(),
                );
            }
        }
        response
    }
}
pub(crate) async fn read_response(mut response: reqwest::Response) -> Result<SearchResponse> {
    let status = response.status();
    let headers = response.headers().clone();
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(crate::upstream::http_error)?
    {
        if data.len() + chunk.len() > crate::images::RESPONSE_LIMIT {
            return Err(Error::new("PAYLOAD_TOO_LARGE", "搜索响应超过 64 MiB。"));
        }
        data.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&data).unwrap_or_else(|_|json!({"error":{"code":"SEARCH_UPSTREAM_HTTP","message":format!("上游搜索返回 HTTP {}，响应不是 JSON。",status.as_u16())}}));
    if status.is_success() && (!value["output"].is_string() || !value["error"].is_null()) {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "上游搜索未返回有效的 output 结果。",
        ));
    }
    // 不解析或重写 results / encrypted_output，未来字段与引用 ID 原样保留。
    Ok(SearchResponse {
        status,
        headers,
        body: serde_json::to_vec(&value)?.into(),
        value,
    })
}

pub(crate) async fn create(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Response {
    let request = match SearchRequest::parse(request).await {
        Ok(request) => request,
        Err(error) => return reject(&state, &access, &context, None, error),
    };
    let pinned = match crate::upstream::search_account(
        &state.operations,
        &access,
        &request.binding,
        &request.model,
    ) {
        Ok(account) => account,
        Err(error) => return reject(&state, &access, &context, Some(&request.model), error),
    };
    let lease = match state
        .operations
        .acquire_inference_on(
            &access,
            RoutingRequest {
                id: context.id,
                endpoint: context.endpoint.clone(),
                client_transport: Some("http".into()),
                model: request.model.clone(),
                user_agent: context.user_agent.clone(),
                subagent_kind: context.subagent_kind.clone(),
                class: RequestClass::Search,
                features: vec![Feature::WebSearch],
                ..Default::default()
            },
            pinned,
        )
        .await
    {
        Ok(lease) => lease,
        Err(error) => return reject(&state, &access, &context, Some(&request.model), error),
    };
    let job = RequestJob {
        account: lease.account_id.expect("inference selects an account"),
        access,
        provider: lease
            .provider
            .clone()
            .expect("inference selects a provider"),
        model: request.model.clone(),
        prompt: String::new(),
        client_payload: None,
    };
    let cancel = state.shutdown.child_token();
    let guard = CancelOnDrop(cancel.clone());
    let registration = match state
        .profiles
        .register_inference(job.account, cancel.clone())
    {
        Ok(registration) => registration,
        Err(error) => {
            let _ = lease.complete::<()>(Err(error.clone()));
            return crate::client_api::client_error(error);
        }
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let result = async {
            state.check_route_loop(&job.provider.base_url).await?;
            // 账户和传输模式在发送前绑定。失败/超时不会切换账号或模式重放。
            state.profiles.search_request(&job, &state.operations, &request, cancel).await
        }.await;
        let error = result.as_ref().err().cloned().or_else(|| result.as_ref().ok().and_then(SearchResponse::error));
        let outcome = match &error {
            Some(error) => ExecutionOutcome::failed(error.clone()),
            None => ExecutionOutcome { model: Ok(crate::upstream::summary(&result.as_ref().unwrap().value, None)), cleanup_error: None },
        };
        let settled = lease.finish_execution(&outcome, None);
        if error.is_some() {
            let (status, body) = match &result {
                Ok(response) => (response.status.as_u16(), response.value.clone()),
                Err(error) => { let (status, body) = crate::client_api::client_error_payload(error); (status.as_u16(), body) }
            };
            // 搜索输入可能携带完整对话；错误详情只保留模型和脱敏标记。
            let _ = state.operations.record_failure_content(context.id, Some(&json!({"body":{"model":job.model,"input":"[已省略搜索指令与上下文]"},"textOmitted":true})), &body, "http", Some(status));
        }
        let response = match settled.and(result) { Ok(response) => response.response(), Err(error) => crate::client_api::client_error(error) };
        let _ = state.operations.record_delivery(context.id, tx.send(response).is_ok());
    });
    let response = rx.await.unwrap_or_else(|_| {
        crate::client_api::client_error(Error::new(
            "UPSTREAM_REQUEST_FAILED",
            "搜索请求没有返回终态。",
        ))
    });
    drop(guard);
    response
}
fn reject(
    state: &AdminState,
    access: &ClientAccess,
    context: &RequestContext,
    model: Option<&str>,
    error: Error,
) -> Response {
    let error = state
        .operations
        .record_client_rejection(context, access, "web.search", model, &error)
        .err()
        .unwrap_or(error);
    crate::client_api::client_error(error)
}
