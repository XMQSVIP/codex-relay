//! Standalone desktop Images API. Native auth resolves the account; image HTTP
//! uses its provider and proxy without changing the Responses transport.
use crate::{
    admin::AdminState,
    error::{Error, Result},
    execution::RequestContext,
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
use std::{sync::Arc, time::Duration};

const LIMIT: usize = 32 * 1024 * 1024;
pub(crate) const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Generate,
    Edit,
}
impl Operation {
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Generate => "images/generations",
            Self::Edit => "images/edits",
        }
    }
    pub(crate) fn action(self) -> &'static str {
        match self {
            Self::Generate => "images.generations",
            Self::Edit => "images.edits",
        }
    }
    fn class(self) -> RequestClass {
        match self {
            Self::Generate => RequestClass::ImageGeneration,
            Self::Edit => RequestClass::ImageEdit,
        }
    }
}
pub(crate) struct ImageRequest {
    pub operation: Operation,
    pub model: String,
    pub content_type: String,
    pub body: Bytes,
}
pub(crate) struct ImageResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub value: Value,
}
impl ImageResponse {
    pub(crate) fn error(&self) -> Option<Error> {
        (!self.status.is_success()).then(|| {
            Error::new(
                match self.status.as_u16() {
                    400 | 404 | 405 | 413 | 415 | 422 => "UPSTREAM_REQUEST_REJECTED",
                    401 | 403 => "UPSTREAM_AUTH_FAILED",
                    429 => "UPSTREAM_RATE_LIMITED",
                    _ => "UPSTREAM_REQUEST_FAILED",
                },
                &format!("上游图片服务返回 HTTP {}。", self.status.as_u16()),
            )
        })
    }
    fn response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        for name in [
            "retry-after",
            "x-codex-imagegen-request-id",
            "x-codex-relay-upstream-transport",
        ] {
            if let Some(value) = self.headers.get(name) {
                response.headers_mut().insert(name, value.clone());
            }
        }
        response
    }
}
fn invalid(message: &str) -> Error {
    Error::new("INVALID_INPUT", message)
}

impl ImageRequest {
    async fn parse(request: Request, operation: Operation) -> Result<Self> {
        if request
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "图片接口暂不接受压缩请求体。",
            ));
        }
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut body = to_bytes(request.into_body(), LIMIT)
            .await
            .map_err(|_| Error::new("PAYLOAD_TOO_LARGE", "图片请求超过 32 MiB 或读取失败。"))?;
        let mut fields = serde_json::Map::new();
        let mut images = 0;
        if content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
        {
            fields = serde_json::from_slice::<Value>(&body)
                .map_err(|_| invalid("图片请求不是有效 JSON。"))?
                .as_object()
                .cloned()
                .ok_or_else(|| invalid("图片请求必须是 JSON 对象。"))?;
            // Authorize and forward the same model even when incoming JSON has
            // duplicate keys. Multipart bytes (including file boundaries) stay exact.
            body = serde_json::to_vec(&fields)?.into();
            images = usize::from(
                fields
                    .get("image")
                    .or_else(|| fields.get("images"))
                    .is_some_and(|v| !v.is_null()),
            );
        } else if matches!(operation, Operation::Edit) {
            let boundary = multer::parse_boundary(&content_type).map_err(|_| {
                Error::new(
                    "UNSUPPORTED_MEDIA_TYPE",
                    "图片编辑须使用 JSON 或 multipart/form-data。",
                )
            })?;
            let stream = futures_util::stream::once(std::future::ready(Ok::<
                _,
                std::convert::Infallible,
            >(body.clone())));
            let mut multipart = multer::Multipart::new(stream, boundary);
            let mut count = 0;
            while let Some(field) = multipart
                .next_field()
                .await
                .map_err(|_| invalid("图片编辑 multipart 格式无效。"))?
            {
                count += 1;
                if count > 64 {
                    return Err(invalid("图片编辑字段过多。"));
                }
                let name = field.name().unwrap_or("").to_owned();
                let data = field
                    .bytes()
                    .await
                    .map_err(|_| invalid("图片编辑字段无法读取。"))?;
                if matches!(name.as_str(), "image" | "image[]" | "images" | "images[]") {
                    if data.is_empty() {
                        return Err(invalid("上传图片不能为空。"));
                    }
                    images += 1;
                } else if matches!(name.as_str(), "model" | "prompt" | "stream") {
                    let text = std::str::from_utf8(&data)
                        .map_err(|_| invalid("图片参数须为 UTF-8 文本。"))?;
                    let value = if name == "stream" {
                        serde_json::from_str(text).map_err(|_| invalid("stream 必须为布尔值。"))?
                    } else {
                        json!(text)
                    };
                    if fields.insert(name, value).is_some() {
                        return Err(invalid("图片参数不可重复。"));
                    }
                }
            }
        } else {
            return Err(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "图片生成须使用 application/json。",
            ));
        }
        let model = fields
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
            })
            .ok_or_else(|| invalid("请提供有效的图片 model。"))?
            .to_owned();
        if !fields
            .get("prompt")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 32_000)
        {
            return Err(invalid("prompt 须为 1–32000 字符的文本。"));
        }
        if fields
            .get("stream")
            .is_some_and(|v| !v.is_null() && *v != false)
        {
            return Err(invalid(
                "图片接口当前支持完整 JSON 结果，请使用 stream=false。",
            ));
        }
        if matches!(operation, Operation::Edit) && images == 0 {
            return Err(invalid("图片编辑缺少输入图片。"));
        }
        Ok(Self {
            operation,
            model,
            content_type,
            body,
        })
    }
}

pub(crate) async fn generate(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Response {
    handle(state, access, context, request, Operation::Generate).await
}
pub(crate) async fn edit(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Response {
    handle(state, access, context, request, Operation::Edit).await
}
async fn handle(
    state: Arc<AdminState>,
    access: ClientAccess,
    context: RequestContext,
    request: Request,
    operation: Operation,
) -> Response {
    let request = match ImageRequest::parse(request, operation).await {
        Ok(request) => request,
        Err(error) => {
            if let Err(storage) = state.operations.record_client_rejection(
                &context,
                &access,
                operation.action(),
                None,
                &error,
            ) {
                return crate::client_api::client_error(storage);
            }
            return crate::client_api::client_error(error);
        }
    };
    let model = request.model.clone();
    let lease = match if let Some(lease) = context.take_admission() {
        Ok(lease)
    } else {
        state
            .operations
            .acquire_inference(
                &access,
                RoutingRequest {
                    id: context.id,
                    endpoint: context.endpoint.clone(),
                    client_transport: Some("http".into()),
                    model: model.clone(),
                    user_agent: context.user_agent.clone(),
                    subagent_kind: context.subagent_kind.clone(),
                    class: operation.class(),
                    features: if matches!(operation, Operation::Edit) {
                        vec![Feature::ImageGeneration, Feature::ImageInput]
                    } else {
                        vec![Feature::ImageGeneration]
                    },
                    ..Default::default()
                },
            )
            .await
    } {
        Ok(lease) => lease,
        Err(error) => {
            let _ = state.operations.record_client_rejection(
                &context,
                &access,
                operation.action(),
                Some(&model),
                &error,
            );
            return crate::client_api::client_error(error);
        }
    };
    let account = lease.account_id.expect("inference selects an account");
    let provider = lease
        .provider
        .clone()
        .expect("inference selects a provider");
    let cancel = state.shutdown.child_token();
    let guard = CancelOnDrop(cancel.clone());
    let registration = match state.profiles.register_inference(account, cancel.clone()) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = lease.complete::<()>(Err(error.clone()));
            return crate::client_api::client_error(error);
        }
    };
    let job = RequestJob {
        account,
        access,
        provider,
        model,
        prompt: String::new(),
        client_payload: None,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let work = async {
            state.check_route_loop(&job.provider.base_url).await?;
            state
                .profiles
                .image_request(&job, &state.operations, &request, cancel.clone())
                .await
        };
        let result = work.await;
        let settled = lease.finish_image(&result);
        let result = settled.and(result);
        let response = match result {
            Ok(response) => response.response(),
            Err(error) => crate::client_api::client_error(error),
        };
        let delivered = tx.send(response).is_ok();
        let _ = state.operations.record_delivery(context.id, delivered);
    });
    let response = rx.await.unwrap_or_else(|_| {
        crate::client_api::client_error(Error::new(
            "UPSTREAM_REQUEST_FAILED",
            "图片请求没有返回终态。",
        ))
    });
    drop(guard);
    response
}

pub(crate) async fn read_response(mut response: reqwest::Response) -> Result<ImageResponse> {
    let status = response.status();
    let headers = response.headers().clone();
    let mut data = Vec::new();
    if response
        .content_length()
        .is_some_and(|n| n > RESPONSE_LIMIT as u64)
    {
        return Err(Error::new("PAYLOAD_TOO_LARGE", "上游图片结果超过 64 MiB。"));
    }
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if data.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(Error::new("PAYLOAD_TOO_LARGE", "上游图片结果超过 64 MiB。"));
        }
        data.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice::<Value>(&data).unwrap_or_else(|_| json!({"error":{"code":"IMAGE_UPSTREAM_HTTP","message":format!("上游图片接口返回 HTTP {}，响应不是 JSON。", status.as_u16())}}));
    if status.is_success()
        && !value["data"].as_array().is_some_and(|a| {
            !a.is_empty()
                && a.iter().all(|i| {
                    i["b64_json"].as_str().is_some_and(|s| !s.is_empty())
                        || i["url"].as_str().is_some_and(|s| !s.is_empty())
                })
        })
    {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "上游图片接口未返回有效图片结果。",
        ));
    }
    let body = serde_json::to_vec(&value)?.into();
    Ok(ImageResponse {
        status,
        headers,
        body,
        value,
    })
}
pub(crate) fn http_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::new("UPSTREAM_TIMEOUT", "图片服务请求超时。")
    } else {
        Error::new("UPSTREAM_CONNECTION_FAILED", "图片服务连接失败。")
    }
}
pub(crate) const TIMEOUT: Duration = Duration::from_secs(1800);

#[cfg(test)]
mod tests {
    use super::*;
    fn request(content_type: &str, body: impl Into<axum::body::Body>) -> Request {
        Request::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(body.into())
            .unwrap()
    }
    #[tokio::test]
    async fn image_requests_preserve_wire_bytes_and_reject_ambiguous_input() {
        let raw = r#"{ "model":"gpt-image-2", "prompt":"puppy", "background":"transparent", "quality":"auto" }"#;
        let parsed = ImageRequest::parse(request("application/json", raw), Operation::Generate)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&parsed.body).unwrap(),
            serde_json::from_str::<Value>(raw).unwrap()
        );
        assert_eq!(parsed.model, "gpt-image-2");
        for value in [
            json!({}),
            json!({"model":"gpt-image-2","prompt":""}),
            json!({"model":"gpt-image-2","prompt":"puppy","stream":true}),
            json!({"model":"gpt-image-2","prompt":"puppy","stream":"false"}),
        ] {
            assert!(
                ImageRequest::parse(
                    request("application/json", value.to_string()),
                    Operation::Generate
                )
                .await
                .is_err()
            );
        }
        let body = b"--fixture\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-2\r\n--fixture\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nedit\r\n--fixture\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"puppy.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\0\xff\r\n--fixture--\r\n";
        let parsed = ImageRequest::parse(
            request("multipart/form-data; boundary=fixture", body.as_slice()),
            Operation::Edit,
        )
        .await
        .unwrap();
        assert_eq!(parsed.body.as_ref(), body);
        assert!(
            ImageRequest::parse(request("application/json", raw), Operation::Edit)
                .await
                .is_err()
        );
    }
}
