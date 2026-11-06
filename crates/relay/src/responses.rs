//! HTTP Responses adapter. Explicit compaction uses account-authenticated HTTP.
pub(crate) mod affinity;
mod body;
mod compaction;
mod compatibility;
pub(crate) mod continuation;
mod control;
pub(crate) mod diagnostics;
mod events;
mod execute;
pub use execute::run;
pub(crate) use execute::run_profiled;
mod request;
pub(crate) mod reuse;
#[cfg(test)]
mod routing_tests;
mod search;
mod service_tier;
mod storage;
mod tool_catalog;
pub mod websocket;
use crate::{
    admin::AdminState,
    error::{Error, Result},
    inference::{CancelOnDrop, RequestJob},
    operations::ClientAccess,
    provider::Provider,
    rpc::Rpc,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
pub(crate) use body::decode as decode_body;
pub use request::Request;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
pub use storage::Store;
/// Admission and native execution share effort precedence. Direct upstream
/// payloads are not rewritten; their configuration history remains intact.
pub(crate) fn admission_reasoning_effort(
    store: &Store,
    access: &ClientAccess,
    value: &Value,
) -> Option<String> {
    let input = value["input"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let all_turns = value["reasoning"]["context"] == "all_turns";
    // Usually the latest control item is in this request; avoid copying the
    // full conversation merely to read one setting. A direct upstream chain
    // need not have a local continuation, in which case only its wire input is known.
    let previous = if (all_turns || value["reasoning"]["effort"].is_null())
        && !input
            .iter()
            .any(|item| item["type"] == "configuration_update")
    {
        value["previous_response_id"]
            .as_str()
            .and_then(|id| store.continuation(access, id).ok())
    } else {
        None
    };
    let history = previous
        .as_ref()
        .map_or(input, |previous| previous.history.as_slice());
    request::effective_effort(&value["reasoning"], history, all_turns)
        .as_str()
        .map(str::to_owned)
}
pub(crate) fn route_account(
    store: &Store,
    ops: &crate::operations::Operations,
    access: &ClientAccess,
    value: &Value,
) -> Result<Option<uuid::Uuid>> {
    let binding = if let Some(id) = value["previous_response_id"].as_str() {
        store.continuation(access, id)?.binding
    } else {
        store.binding_for_history(
            access,
            value["model"].as_str().unwrap_or(""),
            value["input"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        )?
    };
    if let Some(binding) = &binding {
        if ops
            .configured_provider(binding.account)
            .as_ref()
            .map(reuse::provider_digest)
            .transpose()?
            != Some(binding.provider)
        {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "原响应上游配置已改变，请开始新会话。",
            ));
        }
    }
    Ok(binding.map(|b| b.account))
}
pub(crate) fn remember_upstream_output(
    store: &Store,
    access: &ClientAccess,
    account: uuid::Uuid,
    model: &str,
    provider: &Provider,
    history: &[Value],
    response: &Value,
) -> Result<()> {
    store.remember_exchange(
        access,
        history,
        response,
        &storage::Binding {
            account,
            model: model.into(),
            provider: reuse::provider_digest(provider)?,
        },
    )
}

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) fn invalid(message: &str) -> Error {
    Error::new("INVALID_INPUT", message)
}

pub(crate) fn native_compaction_requires_transport() -> Error {
    Error::new(
        "INVALID_INPUT",
        "显式压缩必须通过原生账户认证的 HTTP 传输执行。",
    )
}

pub(crate) fn reservation_bytes(
    rpc: Option<&Rpc>,
    access: &ClientAccess,
    request: &Request,
    history: &[Value],
    session: Option<&reuse::Session>,
) -> Result<usize> {
    let offset = session
        .map(|s| s.continuation(access, request, history))
        .transpose()?
        .flatten();
    if let Some(session) = session.filter(|s| !s.calls.is_empty()) {
        let replies = continuation::replies(session, history, offset.unwrap_or(0))
            .ok_or_else(|| invalid("工具结果与待答线程不匹配。"))?;
        return replies.iter().try_fold(0usize, |sum, (_, value)| {
            Ok(sum.saturating_add(serde_json::to_vec(value)?.len()))
        });
    }
    let mut items = tool_catalog::native_input(
        history,
        request.tool_choice == "none",
        &request.tools,
        offset,
    );
    if let Some(rpc) = rpc {
        items.extend(rpc.location_search_item(&request.tools, request.tool_choice == "none"));
    }
    let initial = if session.is_none() {
        serde_json::to_vec(&request.dynamic_tools())?.len()
            + request.instructions.as_ref().map_or(0, String::len)
    } else {
        0
    };
    items.iter().try_fold(initial, |sum, item| {
        Ok(sum.saturating_add(serde_json::to_vec(item)?.len()))
    })
}

pub async fn create(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
    body::JsonBody(mut value): body::JsonBody,
) -> Response {
    access.apply_fast_mode(&mut value);
    let request = match Request::parse(value) {
        Ok(r) => r,
        Err(e) => return crate::client_api::client_error(e),
    };
    respond(state, access, request, context).await
}
pub async fn compact(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
    body::JsonBody(mut value): body::JsonBody,
) -> Response {
    let original = crate::operations::request_content::request_snapshot(&value);
    access.apply_fast_mode(&mut value);
    value["stream"] = json!(false);
    value["store"] = json!(false);
    let compaction_body = value.clone();
    let mut request = match Request::parse(value) {
        Ok(request) => request,
        Err(error) => return crate::client_api::client_error(error),
    };
    request.original = Arc::new(original);
    request.compact = true;
    request.compact_endpoint = true;
    request.compaction_body = Some(compaction_body);
    respond(state, access, request, context).await
}
async fn respond(
    state: Arc<AdminState>,
    access: ClientAccess,
    request: Request,
    context: crate::execution::RequestContext,
) -> Response {
    let stream = request.stream;
    let result = start(state, access, request, context, None).await;
    let (mut rx, guard, id) = match result {
        Ok(v) => v,
        Err(e) => return crate::client_api::client_error(e),
    };
    let mut response = if stream {
        let stream = futures_util::stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|v| {
                let event = Event::default()
                    .event(v["type"].as_str().unwrap_or("error"))
                    .data(v.to_string());
                (Ok::<_, Infallible>(event), (rx, guard))
            })
        });
        Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
            .into_response()
    } else {
        let mut terminal = None;
        while let Some(v) = rx.recv().await {
            match v["type"].as_str() {
                Some("response.completed" | "response.failed" | "response.incomplete") => {
                    terminal = Some(Json(v["response"].clone()).into_response());
                    break;
                }
                Some("error") => {
                    terminal = Some(crate::client_api::client_error(Error::new(
                        v["code"].as_str().unwrap_or("UPSTREAM_INVALID_RESPONSE"),
                        v["message"].as_str().unwrap_or("原生响应失败。"),
                    )));
                    break;
                }
                _ => {}
            }
        }
        drop(guard);
        terminal.unwrap_or_else(|| {
            crate::client_api::client_error(Error::new(
                "UPSTREAM_INVALID_RESPONSE",
                "原生响应没有终态。",
            ))
        })
    };
    response
        .headers_mut()
        .insert("x-request-id", id.parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
}

async fn start(
    state: Arc<AdminState>,
    access: ClientAccess,
    request: Request,
    context: crate::execution::RequestContext,
    cache: Option<Arc<Store>>,
) -> Result<(mpsc::Receiver<Value>, CancelOnDrop, String)> {
    let original = request.original.clone();
    let id = context.id;
    let websocket = cache.is_some();
    let result = start_inner(state.clone(), access, request, context, cache).await;
    if let Err(error) = &result {
        let (status, body) = crate::client_api::client_error_payload(error);
        let mut body = if websocket {
            websocket::error_event(error.clone())
        } else {
            body
        };
        if websocket && let Some(lane) = original["body"]["stream_id"].as_str() {
            body["stream_id"] = json!(lane);
        }
        let _ = state.operations.record_failure_content(
            id,
            Some(&original),
            &body,
            if websocket { "websocket" } else { "http" },
            if websocket {
                None
            } else {
                Some(status.as_u16())
            },
        );
    }
    result
}

async fn start_inner(
    state: Arc<AdminState>,
    access: ClientAccess,
    mut request: Request,
    context: crate::execution::RequestContext,
    cache: Option<Arc<Store>>,
) -> Result<(mpsc::Receiver<Value>, CancelOnDrop, String)> {
    if crate::native_cache::enabled() {
        affinity::apply(&mut request, context.identity_headers);
        request.native_observation = context.native_observation.clone();
    }
    if !access.allows(&request.model) {
        return Err(Error::new("MODEL_NOT_ALLOWED", "此密钥未授权该模型。"));
    }
    let previous = match &request.previous {
        Some(id) => Some(
            match cache.as_ref().map(|cache| cache.continuation(&access, id)) {
                Some(Ok(previous)) => previous,
                _ => state.responses.continuation(&access, id)?,
            },
        ),
        None => None,
    };
    let mut binding = previous.as_ref().and_then(|p| p.binding.clone());
    if binding.as_ref().is_some_and(|b| b.model != request.model) {
        return Err(Error::new(
            "RESPONSE_CHAIN_MISMATCH",
            "更换模型需省略 previous_response_id 并提交完整历史。",
        ));
    }
    let mut history = previous.map(|p| p.history).unwrap_or_default();
    history.extend(request.input.clone());
    if let Some(inferred) =
        state
            .responses
            .binding_for_history(&access, &request.model, &history)?
    {
        if binding.as_ref().is_some_and(|b| b != &inferred) {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "历史与 previous_response_id 的账户绑定不一致。",
            ));
        }
        binding = Some(inferred);
    }
    request::validate_history(&history)?;
    let mut request = request.with_history(&history)?;
    if request.compact {
        request.parameter_handling = vec![
            json!({"path":"*","action":"adapted","reason":"显式压缩通过原生账户认证的 HTTP 传输执行；支持的模型参数和工具定义直接传给上游，本地身份仅用于选路"}),
        ];
    }
    if let Some(control) = &request.control {
        control.prepare(&history);
    }
    if serde_json::to_vec(&history)?.len() > crate::media::BODY_LIMIT {
        return Err(invalid("会话历史超过 32 MiB；请精简历史后重试。"));
    }
    state
        .profiles
        .check_continuation_busy(&access, &request, &history)?;
    let lease = if let Some(lease) = context.take_admission() {
        lease
    } else {
        let admission = state.operations.acquire_inference_on(
            &access,
            crate::operations::RoutingRequest {
                id: context.id,
                model: request.model.clone(),
                streaming: request.stream,
                endpoint: context.endpoint.clone(),
                client_transport: context.endpoint.as_ref().map(|_| {
                    if context.websocket {
                        "ws"
                    } else if request.stream {
                        "sse"
                    } else {
                        "http"
                    }
                    .into()
                }),
                reasoning_effort: request.reasoning["effort"].as_str().map(str::to_owned),
                service_tier: request.service_tier.clone(),
                user_agent: context.user_agent.clone(),
                subagent_kind: request
                    .subagent_kind
                    .clone()
                    .or_else(|| context.subagent_kind.clone()),
                features: request.required_features(&history)?,
                class: if request.required_features(&history)?.iter().any(|f| {
                    matches!(
                        f,
                        crate::operations::Feature::ImageInput
                            | crate::operations::Feature::ImageGeneration
                    )
                }) {
                    crate::operations::RequestClass::Multimodal
                } else if request.tools.is_empty() {
                    crate::operations::RequestClass::Text
                } else {
                    crate::operations::RequestClass::Tools
                },
                parent_affinity: affinity::parent(&context, &request),
                soft_affinity: if crate::native_cache::enabled() || binding.is_none() {
                    affinity::routing(&context, &request, &history)
                } else {
                    None
                },
            },
            binding.as_ref().map(|b| b.account),
        );
        state
            .profiles
            .continuation_admission(&access, &request, &history, admission)
            .await?
    };
    let account = lease
        .account_id
        .ok_or_else(|| invalid("调度未选择实例。"))?;
    let provider = lease
        .provider
        .clone()
        .ok_or_else(|| invalid("实例路由缺失。"))?;
    let current_binding = storage::Binding {
        account,
        model: request.model.clone(),
        provider: reuse::provider_digest(&provider)?,
    };
    if let Err(error) = state.check_route_loop(&provider.base_url).await {
        return lease.complete(Err(error));
    }
    if binding.as_ref().is_some_and(|b| b != &current_binding) {
        return lease.complete(Err(Error::new(
            "RESPONSE_CHAIN_MISMATCH",
            "原响应账户或路由已变化；请重新开始会话。",
        )));
    }
    if let Err(e) =
        state
            .operations
            .check_inference_route(account, &access, &provider, &request.model)
    {
        return lease.complete(Err(e));
    }
    if let Err(error) = state.operations.record_upstream_transport(
        lease.request_id(),
        if request.compact { "http" } else { "native" },
    ) {
        return lease.complete(Err(error));
    }
    let id = lease.request_id().to_string();
    if let Err(error) = state
        .responses
        .renew(&access, &request.model, &history, &current_binding)
    {
        return lease.complete(Err(error));
    }
    if let Err(error) = state
        .operations
        .record_parameter_handling(lease.request_id(), &request.parameter_handling)
    {
        return lease.complete(Err(error));
    }
    let cancel = state.shutdown.child_token();
    let guard = CancelOnDrop(cancel.clone());
    let registration = state.profiles.register_inference(account, cancel.clone())?;
    let job = RequestJob {
        account,
        access: access.clone(),
        provider,
        model: request.model.clone(),
        prompt: String::new(),
        client_payload: None,
    };
    let (tx, rx) = mpsc::channel(16);
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let execution_cancel = cancel.child_token();
        let (native_tx, mut native_rx) = mpsc::channel::<Value>(16);
        let pump = async {
            let mut terminal = None;
            let mut sequence = 0;
            let mut access_error = None;
            while let Some(mut event) = native_rx.recv().await {
                if request.compact_endpoint && event["type"] == "response.completed" {
                    event["response"]["object"] = json!("response.compaction");
                }
                sequence = event["sequence_number"].as_u64().unwrap_or(sequence) + 1;
                if let Err(error) = state.operations.check_inference_route(
                    account,
                    &access,
                    &job.provider,
                    &request.model,
                ) {
                    access_error = Some(error);
                    execution_cancel.cancel();
                    break;
                }
                if matches!(
                    event["type"].as_str(),
                    Some("response.completed" | "response.failed" | "response.incomplete")
                ) {
                    terminal = Some(event);
                    continue;
                }
                if send(&tx, event, &cancel).await.is_err() {
                    cancel.cancel();
                    break;
                }
            }
            (terminal, sequence, access_error)
        };
        let work = state.profiles.direct_response(
            &job,
            &state.operations,
            &request,
            &history,
            native_tx,
            execution_cancel.clone(),
            &context,
        );
        let (result, (terminal, sequence, access_error)) = tokio::join!(work, pump);
        let mut response_error = access_error.or_else(|| {
            state
                .operations
                .check_inference_route(account, &access, &job.provider, &request.model)
                .err()
        });
        let access_denied = response_error.is_some();
        let incomplete = result
            .model
            .as_ref()
            .is_ok_and(|s| s.outcome == "incomplete")
            && result.cleanup_error.is_none();
        let mut completed_affinity = None;
        if !access_denied && (result.error().is_none() || incomplete) {
            if let Some(event) = &terminal {
                if let Some(control) = &request.control {
                    history.extend(control.additions());
                }
                response_error = state
                    .responses
                    .remember_exchange(&access, &history, &event["response"], &current_binding)
                    .err();
                if request.compact {
                    history = if request.tools.is_empty() {
                        vec![]
                    } else {
                        vec![
                            json!({"type":"additional_tools","role":"developer","tools":request.tools}),
                        ]
                    };
                }
                completed_affinity = if crate::native_cache::enabled() {
                    None
                } else {
                    affinity::completed(&request, &history, &event["response"])
                };
                if response_error.is_none() {
                    if let Some(cache) = &cache {
                        response_error = cache
                            .save_bound(
                                &access,
                                history,
                                &event["response"],
                                Some(current_binding),
                                None,
                            )
                            .err();
                    }
                }
            } else {
                response_error = Some(Error::new(
                    "UPSTREAM_INVALID_RESPONSE",
                    "原生响应没有终态。",
                ));
            }
        }
        let settled = lease.finish_execution(&result, response_error.as_ref());
        if settled.is_ok() && response_error.is_none() && result.error().is_none() {
            if let Some(digest) = completed_affinity {
                state.operations.remember_soft_affinity(
                    &access,
                    &request.model,
                    digest,
                    account,
                    &job.provider,
                );
            }
        }
        let failure = response_error
            .or(if incomplete { None } else { result.error() })
            .or_else(|| settled.err());
        let output = if let Some(e) = failure {
            if e.code == "NATIVE_TOOL_SCOPE" {
                eprintln!(
                    "{}",
                    json!({"event":"native_tool_scope_rejected","requestId":context.id,"code":e.code,"message":e.message})
                );
            }
            if let Some(mut event) = terminal {
                if access_denied || request.compact {
                    event["response"]["output"] = json!([]);
                }
                event["type"] = json!("response.failed");
                event["response"]["status"] = json!("failed");
                event["response"]["error"] = json!({"code":e.code,"message":e.message});
                event
            } else {
                json!({"type":"error","sequence_number":sequence,"code":e.code,"message":e.message,"param":null,"request_id":context.id})
            }
        } else {
            terminal.unwrap()
        };
        if output["type"] != "response.completed" {
            let (transport, status, body) = if cache.is_some() {
                let mut body = output.clone();
                if body["type"] == "error" && body["error"].is_null() {
                    let wire_error = websocket::error_event(Error::new(
                        body["code"].as_str().unwrap_or("UPSTREAM_REQUEST_FAILED"),
                        body["message"].as_str().unwrap_or("原生请求失败。"),
                    ));
                    body["status"] = wire_error["status"].clone();
                    body["error"] = wire_error["error"].clone();
                }
                if let Some(lane) = request.original["body"]["stream_id"].as_str() {
                    body["stream_id"] = json!(lane);
                }
                ("websocket", None, body)
            } else if request.stream {
                ("sse", Some(200), output.clone())
            } else if output["type"] == "error" {
                let (status, body) = crate::client_api::client_error_payload(&Error::new(
                    output["code"]
                        .as_str()
                        .unwrap_or("UPSTREAM_INVALID_RESPONSE"),
                    output["message"].as_str().unwrap_or("原生响应失败。"),
                ));
                ("http", Some(status.as_u16()), body)
            } else {
                ("http", Some(200), output["response"].clone())
            };
            let body = if request.compact {
                json!({"type":output["type"],"error":{"code":output.get("code").unwrap_or(&output["response"]["error"]["code"])}})
            } else {
                body
            };
            let _ = state.operations.record_failure_content(
                context.id,
                Some(&request.original),
                &body,
                transport,
                status,
            );
        }
        // A policy/user cancellation stops native work, but an attached consumer
        // must still receive its failure terminal. Disconnected consumers fail
        // the bounded send immediately; service shutdown cancels delivery too.
        let keep_native = output["type"] == "response.completed"
            && !cancel.is_cancelled()
            && !execution_cancel.is_cancelled();
        if keep_native {
            context.native_handoff.publish();
        }
        let delivered = send(&tx, output, &state.shutdown).await.is_ok();
        context.native_handoff.finish(keep_native && delivered);
        let _ = state.operations.record_delivery(context.id, delivered);
    });
    Ok((rx, guard, id))
}

pub async fn retrieve(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Path(id): Path<String>,
) -> Response {
    match state.responses.get(&access, &id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => crate::client_api::client_error(e),
    }
}
pub async fn delete(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Path(id): Path<String>,
) -> Response {
    match state.responses.delete(&access, &id) {
        Ok(()) => Json(json!({"id":id,"object":"response.deleted","deleted":true})).into_response(),
        Err(e) => crate::client_api::client_error(e),
    }
}
async fn send(tx: &mpsc::Sender<Value>, v: Value, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
        result = tokio::time::timeout(Duration::from_secs(5), tx.send(v)) => {
            result
                .map_err(|_| Error::new("STREAM_BACKPRESSURE", "客户端读取超时。"))?
                .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。"))
        }
    }
}
