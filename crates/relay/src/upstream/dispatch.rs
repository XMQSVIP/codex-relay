//! Admission occurs once, before native-only validation. Direct traffic keeps upstream semantics.
use super::{
    bindings::{self, Binding},
    transport,
};
use crate::{
    admin::AdminState,
    error::{Error, Result},
    execution::{ExecutionOutcome, RequestContext},
    operations::{ClientAccess, Feature, Lease, RequestClass, RoutingRequest},
    provider::Execution,
};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub(crate) struct Raw {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub value: Value,
}
pub(crate) fn has_routes(state: &AdminState, access: &ClientAccess) -> bool {
    state.operations.has_api_key_routes(access.group_id)
}
impl Raw {
    pub(crate) fn json(path: &str, mut value: Value) -> Result<Self> {
        if let Some(o) = value.as_object_mut() {
            o.remove("type");
            o.remove("stream_id");
            o.remove("event_id");
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        Ok(Self {
            method: Method::POST,
            path: path.into(),
            headers,
            body: serde_json::to_vec(&value)?.into(),
            value,
        })
    }
    async fn read(request: Request, path: String) -> Result<Self> {
        let (parts, body) = request.into_parts();
        let body = to_bytes(body, crate::media::BODY_LIMIT)
            .await
            .map_err(|_| Error::new("PAYLOAD_TOO_LARGE", "请求正文超过 32 MiB。"))?;
        let mut headers = parts.headers;
        let ct = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let (body, value) = if ct.split(';').next().is_some_and(|v| {
            v.trim().eq_ignore_ascii_case("application/json") || v.trim().ends_with("+json")
        }) {
            let encoding = headers
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let value = crate::responses::decode_body(&body, encoding)?;
            headers.remove(header::CONTENT_ENCODING);
            (serde_json::to_vec(&value)?.into(), value)
        } else if path == "images/edits" {
            let boundary = multer::parse_boundary(&ct)
                .map_err(|_| super::invalid("图片编辑须为 JSON 或 multipart/form-data。"))?;
            let stream = futures_util::stream::once(std::future::ready(Ok::<
                _,
                std::convert::Infallible,
            >(body.clone())));
            let mut multipart = multer::Multipart::new(stream, boundary);
            let mut value = json!({});
            while let Some(field) = multipart
                .next_field()
                .await
                .map_err(|_| super::invalid("multipart 格式无效。"))?
            {
                let name = field.name().unwrap_or("").to_owned();
                if matches!(name.as_str(), "model" | "stream") {
                    if value.get(&name).is_some() {
                        return Err(super::invalid("图片参数不可重复。"));
                    }
                    let text = field
                        .text()
                        .await
                        .map_err(|_| super::invalid("图片参数无效。"))?;
                    value[&name] = if name == "stream" {
                        serde_json::from_str(&text)
                            .map_err(|_| super::invalid("stream 须为布尔值。"))?
                    } else {
                        json!(text)
                    };
                }
            }
            (body, value)
        } else {
            return Err(Error::new("UNSUPPORTED_MEDIA_TYPE", "请求须为 JSON。"));
        };
        Ok(Self {
            method: parts.method,
            path,
            headers,
            body,
            value,
        })
    }
}
fn bound_account(
    state: &AdminState,
    access: &ClientAccess,
    value: &Value,
    model: &str,
) -> Result<Option<uuid::Uuid>> {
    let previous = value["previous_response_id"]
        .as_str()
        .map(|id| bindings::lookup(&state.operations, access, id))
        .transpose()?
        .flatten();
    let pinned = if let Some(previous) = &previous {
        if previous.model != model {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "更换模型需提交完整历史。",
            ));
        }
        state.operations.check_inference_route(
            previous.account,
            access,
            &previous.provider,
            model,
        )?;
        let mut history = value.clone();
        history
            .as_object_mut()
            .unwrap()
            .remove("previous_response_id");
        if crate::responses::route_account(&state.responses, &state.operations, access, &history)?
            .is_some_and(|id| id != previous.account)
        {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "历史混合了不同账户的响应。",
            ));
        }
        Some(previous.account)
    } else {
        crate::responses::route_account(&state.responses, &state.operations, access, value)?
    };
    Ok(pinned)
}

/// Local prewarm has no model work and must not occupy a scheduling lease.
/// Prefer a native route when available, preserving existing response bindings.
pub(crate) fn native_prewarm_route(
    state: &AdminState,
    access: &ClientAccess,
    value: &Value,
) -> Result<Option<(uuid::Uuid, crate::provider::Provider)>> {
    let model = value["model"]
        .as_str()
        .ok_or_else(|| super::invalid("请提供有效的 model。"))?;
    if !access.allows(model) {
        return Err(Error::new("MODEL_NOT_ALLOWED", "此密钥未授权该模型。"));
    }
    let selected = if let Some(account) = bound_account(state, access, value, model)? {
        let provider = state
            .operations
            .configured_provider(account)
            .ok_or_else(|| Error::new("ROUTE_CHANGED", "原响应路由已改变。"))?;
        (account, provider)
    } else {
        match state
            .operations
            .native_inference_route(access.group_id, model)
        {
            Ok(route) => route,
            Err(error) if error.code == "INFERENCE_DISABLED" => return Ok(None),
            Err(error) => return Err(error),
        }
    };
    if selected.1.execution != Execution::Native {
        return Ok(None);
    }
    state
        .operations
        .check_inference_route(selected.0, access, &selected.1, model)?;
    Ok(Some(selected))
}

pub(crate) async fn prepare(
    state: &Arc<AdminState>,
    access: &ClientAccess,
    context: &RequestContext,
    raw: &Raw,
) -> Result<Lease> {
    let model = raw.value["model"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| super::invalid("请提供有效的 model。"))?;
    let pinned = bound_account(state, access, &raw.value, model)?;
    let mut features = vec![];
    let mut class = if raw.value["tools"].as_array().is_some_and(|v| !v.is_empty()) {
        RequestClass::Tools
    } else {
        RequestClass::Text
    };
    if raw.path.starts_with("images/") {
        features.push(Feature::ImageGeneration);
        class = if raw.path == "images/edits" {
            features.push(Feature::ImageInput);
            RequestClass::ImageEdit
        } else {
            RequestClass::ImageGeneration
        };
    } else {
        features = super::requirements::features(&raw.value);
    }
    let reasoning_effort = if raw.path == "responses" {
        crate::responses::admission_reasoning_effort(&state.responses, access, &raw.value)
    } else {
        raw.value["reasoning"]["effort"].as_str().map(str::to_owned)
    };
    let native_request = if raw.path == "responses" && crate::native_cache::enabled() {
        crate::responses::Request::parse(raw.value.clone())
            .ok()
            .map(|mut request| {
                if let Some(effort) = &reasoning_effort {
                    request.reasoning["effort"] = json!(effort);
                }
                crate::responses::affinity::apply(&mut request, context.identity_headers);
                request.native_observation = context.native_observation.clone();
                request
            })
    } else {
        None
    };
    let admission = state.operations.acquire_inference_on(
        access,
        RoutingRequest {
            id: context.id,
            model: model.into(),
            streaming: raw.value["stream"] == true,
            endpoint: context.endpoint.clone(),
            client_transport: context.endpoint.as_ref().map(|_| {
                if context.websocket {
                    "ws"
                } else if raw.value["stream"] == true {
                    "sse"
                } else {
                    "http"
                }
                .into()
            }),
            reasoning_effort,
            service_tier: raw.value["service_tier"].as_str().map(str::to_owned),
            user_agent: context.user_agent.clone(),
            subagent_kind: crate::subagent::from_body(&raw.value)
                .or_else(|| context.subagent_kind.clone()),
            features,
            class,
            parent_affinity: native_request
                .as_ref()
                .and_then(|request| crate::responses::affinity::parent(context, request)),
            soft_affinity: if crate::native_cache::enabled() || pinned.is_none() {
                native_request.as_ref().and_then(|request| {
                    crate::responses::affinity::routing(context, request, &request.input)
                })
            } else {
                None
            },
        },
        pinned,
    );
    if let Some(request) = native_request {
        state
            .profiles
            .continuation_admission(access, &request, &request.input, admission)
            .await
    } else {
        admission.await
    }
}
pub(crate) async fn middleware(
    State(state): State<Arc<AdminState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(access) = request.extensions().get::<ClientAccess>().cloned() else {
        return next.run(request).await;
    };
    if !has_routes(&state, &access) {
        return next.run(request).await;
    }
    let path = request
        .uri()
        .path()
        .trim_start_matches("/v1/")
        .trim_start_matches('/')
        .to_owned();
    let context = request
        .extensions()
        .get::<RequestContext>()
        .cloned()
        .unwrap_or_default();
    if matches!(*request.method(), Method::GET | Method::DELETE)
        && path.starts_with("responses/")
        && path != "responses/compact"
    {
        return match bindings::lookup(&state.operations, &access, &path[10..]) {
            Ok(Some(binding)) => {
                retrieve(
                    state,
                    access,
                    context,
                    request.method().clone(),
                    path,
                    binding,
                )
                .await
            }
            Ok(None) => next.run(request).await,
            Err(e) => crate::client_api::client_error(e),
        };
    }
    if request.method() != Method::POST
        || ![
            "responses",
            "responses/compact",
            "images/generations",
            "images/edits",
        ]
        .contains(&path.as_str())
    {
        return next.run(request).await;
    }
    let uri = request.uri().clone();
    let version = request.version();
    let extensions = request.extensions().clone();
    let mut raw = match Raw::read(request, path).await {
        Ok(v) => v,
        Err(e) => return crate::client_api::client_error(e),
    };
    if ["responses", "responses/compact"].contains(&raw.path.as_str())
        && access.apply_fast_mode(&mut raw.value)
    {
        raw.body = match serde_json::to_vec(&raw.value) {
            Ok(bytes) => bytes.into(),
            Err(error) => return crate::client_api::client_error(error.into()),
        };
        raw.headers.remove(header::CONTENT_LENGTH);
    }
    let lease = match prepare(&state, &access, &context, &raw).await {
        Ok(v) => v,
        Err(e) => return crate::client_api::client_error(e),
    };
    if lease
        .provider
        .as_ref()
        .is_some_and(|p| p.execution == Execution::ApiKey)
    {
        return forward(state, access, context, raw, lease).await;
    }
    let mut forwarded = Request::new(Body::from(raw.body));
    *forwarded.method_mut() = raw.method;
    *forwarded.uri_mut() = uri;
    *forwarded.version_mut() = version;
    *forwarded.headers_mut() = raw.headers;
    *forwarded.extensions_mut() = extensions;
    let mut context = context;
    context.admit(lease);
    forwarded.extensions_mut().insert(context.clone());
    let response = next.run(forwarded).await;
    if let Some(lease) = context.take_admission() {
        let _ = lease.complete::<()>(Err(super::invalid("原生请求校验失败。")));
    }
    response
}
async fn retrieve(
    state: Arc<AdminState>,
    access: ClientAccess,
    context: RequestContext,
    method: Method,
    path: String,
    binding: Binding,
) -> Response {
    let work = async {
        state.operations.check_inference_route(
            binding.account,
            &access,
            &binding.provider,
            &binding.model,
        )?;
        let lease = state
            .operations
            .acquire_inference_on(
                &access,
                RoutingRequest {
                    id: context.id,
                    model: binding.model.clone(),
                    endpoint: context.endpoint.clone(),
                    client_transport: Some("http".into()),
                    ..Default::default()
                },
                Some(binding.account),
            )
            .await?;
        Ok(lease)
    }
    .await;
    match work {
        Ok(lease) => {
            forward(
                state,
                access,
                context,
                Raw {
                    method,
                    path,
                    headers: HeaderMap::new(),
                    body: Bytes::new(),
                    value: json!({"model":binding.model}),
                },
                lease,
            )
            .await
        }
        Err(e) => crate::client_api::client_error(e),
    }
}

/// Incremental SSE observer. Forwarding does not depend on recognizing every event.
#[derive(Default)]
pub(crate) struct Events {
    pending: Vec<u8>,
}
impl Events {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 64 * 1024 * 1024 {
            return Err(Error::new("UPSTREAM_INVALID_RESPONSE", "上游事件过大。"));
        }
        let mut out = vec![];
        loop {
            let boundary = self
                .pending
                .windows(2)
                .position(|p| p == b"\n\n")
                .map(|i| (i, 2))
                .into_iter()
                .chain(
                    self.pending
                        .windows(4)
                        .position(|p| p == b"\r\n\r\n")
                        .map(|i| (i, 4)),
                )
                .min_by_key(|(i, _)| *i);
            let Some((i, n)) = boundary else {
                break;
            };
            let chunk = self.pending.drain(..i + n).collect::<Vec<_>>();
            if let Ok(text) = std::str::from_utf8(&chunk) {
                let data = text
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(v) = serde_json::from_str(&data) {
                    out.push(v);
                }
            }
        }
        Ok(out)
    }
}
pub(crate) async fn forward(
    state: Arc<AdminState>,
    access: ClientAccess,
    context: RequestContext,
    raw: Raw,
    lease: Lease,
) -> Response {
    let account = lease.account_id.unwrap();
    let provider = lease.provider.clone().unwrap();
    let cancel = state.shutdown.child_token();
    let registration = match state.profiles.register_inference(account, cancel.clone()) {
        Ok(r) => r,
        Err(e) => return crate::client_api::client_error(e),
    };
    let setup = async {
        state.check_route_loop(&provider.base_url).await?;
        state.operations.check_inference_route(
            account,
            &access,
            &provider,
            raw.value["model"].as_str().unwrap_or(""),
        )?;
        let config = state.profiles.upstream(account).await?;
        if config.provider() != provider {
            return Err(Error::new("ROUTE_CHANGED", "上游配置已改变，请重新请求。"));
        }
        let client = state
            .operations
            .account_proxy(account)
            .resolved()
            .await?
            .client()?;
        let binding = Binding {
            native_tools: false,
            account,
            model: raw.value["model"].as_str().unwrap_or("").into(),
            provider: provider.clone(),
            websocket: false,
        };
        let previous = raw.value["previous_response_id"]
            .as_str()
            .map(|id| bindings::lookup(&state.operations, &access, id))
            .transpose()?
            .flatten();
        let transfer = transport::send(
            &config,
            &client,
            raw.method.clone(),
            &raw.path,
            &raw.headers,
            raw.body.clone(),
            &raw.value,
            &binding,
            previous.as_ref(),
            &access,
            &state.upstream_connections,
        )
        .await?;
        Ok::<_, Error>((transfer, binding))
    };
    let (transfer, mut binding) = match tokio::select! {_=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","请求已取消。")),r=setup=>r}
    {
        Ok(v) => v,
        Err(e) => {
            let _ = lease.complete::<()>(Err(e.clone()));
            return crate::client_api::client_error(e);
        }
    };
    binding.websocket = transfer.link.is_some();
    let protocol = if binding.websocket {
        "ws"
    } else if transfer
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        })
    {
        "sse"
    } else {
        "http"
    };
    // Submission has already happened; diagnostics must not interrupt delivery.
    let _ = state
        .operations
        .record_upstream_transport(context.id, protocol);
    let status = transfer.status;
    let mut response_headers = transfer.headers.clone();
    if let Some(id) = response_headers.get("x-request-id").cloned() {
        response_headers.insert("x-upstream-request-id", id);
    }
    let aggregate = binding.websocket && raw.value["stream"] != true;
    if aggregate {
        response_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes>>(16);
    let stream_cancel = cancel.clone();
    let delivery = Delivery {
        state: state.clone(),
        access,
        context,
        raw,
        binding,
        transfer,
        lease,
        cancel,
        _registration: registration,
    };
    state.tasks.spawn(delivery.run(tx));
    let stream = futures_util::stream::unfold(
        (rx, crate::inference::CancelOnDrop(stream_cancel)),
        |(mut rx, guard)| async move { rx.recv().await.map(|v| (v, (rx, guard))) },
    );
    let body = Body::from_stream(stream.map(|r| r.map_err(|e| std::io::Error::other(e.code))));
    let mut response = if aggregate {
        match to_bytes(body, 64 * 1024 * 1024).await {
            Ok(bytes) => {
                let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                let status = if value["type"] == "error" {
                    value["status"]
                        .as_u64()
                        .and_then(|n| u16::try_from(n).ok())
                        .and_then(|n| axum::http::StatusCode::from_u16(n).ok())
                        .filter(|s| s.is_client_error() || s.is_server_error())
                        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY)
                } else {
                    status
                };
                (status, bytes).into_response()
            }
            Err(_) => {
                return crate::client_api::client_error(Error::new(
                    "UPSTREAM_CONNECTION_FAILED",
                    "上游响应中断；请求未重放。",
                ));
            }
        }
    } else {
        (status, body).into_response()
    };
    *response.headers_mut() = response_headers;
    response
}
struct Delivery {
    state: Arc<AdminState>,
    access: ClientAccess,
    context: RequestContext,
    raw: Raw,
    binding: Binding,
    transfer: transport::Transfer,
    lease: Lease,
    cancel: tokio_util::sync::CancellationToken,
    _registration: crate::profiles::InferenceRegistration,
}
impl Delivery {
    async fn run(mut self, tx: mpsc::Sender<Result<Bytes>>) {
        let status = self.transfer.status;
        let sse = self
            .transfer
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let aggregate = self.binding.websocket && self.raw.value["stream"] != true;
        let mut parser = Events::default();
        let mut document = Vec::new();
        let mut last = Value::Null;
        let mut first = None;
        let mut terminal = false;
        let mut complete = false;
        let mut event_error = None;
        let deadline = tokio::time::sleep(Duration::from_secs(1800));
        tokio::pin!(deadline);
        let mut authorization = tokio::time::interval(Duration::from_secs(1));
        let result:Result<()>=async {
            loop {
                let chunk=tokio::select! {
                    _=self.cancel.cancelled()=>return Err(Error::new("REQUEST_CANCELLED","请求已取消。")),
                    _=&mut deadline=>return Err(Error::new("UPSTREAM_TIMEOUT","上游请求超过 30 分钟。")),
                    _=tx.closed()=>return Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                    _=authorization.tick()=>{self.state.operations.revalidate_api_key(&self.access)?;continue;},
                    chunk=self.transfer.chunks.next()=>chunk,
                };
                let Some(chunk)=chunk else {break;};
                let chunk=chunk?;
                if sse {
                    for value in parser.push(&chunk)? {
                        let kind=value["type"].as_str().unwrap_or("");
                        if first.is_none() && kind.ends_with(".delta") {first=Some(crate::now_ms());}
                        if value["response"].is_object() {self.remember(&value["response"],matches!(kind,"response.completed"|"response.incomplete")).await?;}
                        if matches!(kind,"response.completed"|"response.incomplete"|"response.failed"|"error"|"image_generation.completed"|"image_edit.completed") {
                            terminal=true;
                            last=if value["response"].is_object(){value["response"].clone()}else{value.clone()};
                        }
                        if matches!(kind,"response.failed"|"error") {event_error=Some(Error::new("UPSTREAM_REQUEST_FAILED","上游返回失败事件。"));}
                        if kind=="response.incomplete" {event_error=Some(Error::new("UPSTREAM_INCOMPLETE","上游响应未完成。"));}
                    }
                } else {
                    if document.len()+chunk.len()>64*1024*1024 {return Err(Error::new("PAYLOAD_TOO_LARGE","上游响应超过 64 MiB。"));}
                    document.extend_from_slice(&chunk);
                }
                if !aggregate {send(&tx,Ok(chunk)).await?;}
            }
            if !sse {
                last=serde_json::from_slice(&document).unwrap_or(Value::Null);
                if status.is_success() && self.raw.method==Method::POST {
                    let accepted=matches!(last["status"].as_str(),Some("completed"|"incomplete")) || (last["object"]=="response.compaction" && last["error"].is_null());
                    self.remember(&last,accepted).await?;
                }
                if last["status"]=="failed" || last["error"].is_object() {event_error=Some(Error::new("UPSTREAM_REQUEST_FAILED","上游响应失败。"));}
                if last["status"]=="incomplete" {event_error=Some(Error::new("UPSTREAM_INCOMPLETE","上游响应未完成。"));}
            }
            if status.is_success() && self.raw.method==Method::DELETE {
                bindings::delete(&self.state.operations,&self.access,self.raw.path.trim_start_matches("responses/"))?;
            }
            if aggregate {send(&tx,Ok(serde_json::to_vec(&last)?.into())).await?;}
            complete=!status.is_success() || !sse || terminal;
            if !status.is_success(){return Err(transport::status_error(status));}
            if sse && !terminal{return Err(Error::new("UPSTREAM_INVALID_RESPONSE","上游流未返回终态。"));}
            if let Some(error)=event_error.take(){return Err(error);}
            Ok(())
        }.await;
        if let Err(error) = &result {
            if !complete {
                let _ = send(&tx, Err(error.clone())).await;
            }
        }
        let mut summary = super::summary(&last, first);
        if let Err(error) = result {
            summary.outcome = if crate::execution::cancelled(&error.code) {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            summary.error_code = Some(error.code);
            summary.error_message = Some(error.message);
        }
        let outcome = ExecutionOutcome {
            model: Ok(summary),
            cleanup_error: None,
        };
        let _ = self.lease.finish_execution(&outcome, None);
        let _ = self
            .state
            .operations
            .record_delivery(self.context.id, !tx.is_closed());
    }
    async fn remember(&mut self, response: &Value, accepted: bool) -> Result<()> {
        if self.raw.path == "responses" {
            if let Some(id) = response["id"].as_str() {
                bindings::save(&self.state.operations, &self.access, id, &self.binding)?;
                if let Some(link) = &self.transfer.link {
                    self.state
                        .upstream_connections
                        .remember(
                            self.binding.account,
                            &self.access,
                            &self.binding.provider.id,
                            id,
                            link.clone(),
                        )
                        .await;
                }
            }
        }
        crate::responses::remember_upstream_output(
            &self.state.responses,
            &self.access,
            self.binding.account,
            &self.binding.model,
            &self.binding.provider,
            if accepted {
                self.raw.value["input"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            } else {
                &[]
            },
            response,
        )
    }
}
async fn send(tx: &mpsc::Sender<Result<Bytes>>, value: Result<Bytes>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), tx.send(value))
        .await
        .map_err(|_| Error::new("STREAM_BACKPRESSURE", "客户端读取超时。"))?
        .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。"))
}
