//! HTTP Responses adapter. Explicit compaction uses account-authenticated HTTP.
pub(crate) mod affinity;
mod body;
mod compaction;
mod compatibility;
pub(crate) mod continuation;
mod control;
pub(crate) mod diagnostics;
mod events;
mod request;
pub(crate) mod reuse;
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
    streaming::StreamSummary,
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
use std::{convert::Infallible, path::Path as FilePath, sync::Arc, time::Duration};
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
        .flatten()
        .unwrap_or(0);
    if let Some(session) = session.filter(|s| !s.calls.is_empty()) {
        let replies = continuation::replies(session, history, offset)
            .ok_or_else(|| invalid("工具结果与待答线程不匹配。"))?;
        return replies.iter().try_fold(0usize, |sum, (_, value)| {
            Ok(sum.saturating_add(serde_json::to_vec(value)?.len()))
        });
    }
    let mut items = tool_catalog::native_history_with_catalog(
        &history[offset..],
        request.tool_choice == "none",
        &request.tools,
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
        let execution_cancel=cancel.child_token();
        let (native_tx,mut native_rx)=mpsc::channel::<Value>(16);
        let pump=async {
            let mut terminal=None;
            let mut sequence=0;
            let mut access_error=None;
            while let Some(mut event)=native_rx.recv().await {
                if request.compact_endpoint && event["type"]=="response.completed" { event["response"]["object"]=json!("response.compaction"); }
                sequence=event["sequence_number"].as_u64().unwrap_or(sequence)+1;
                if let Err(error)=state.operations.check_inference_route(account,&access,&job.provider,&request.model) {
                    access_error=Some(error);execution_cancel.cancel();break;
                }
                if matches!(event["type"].as_str(),Some("response.completed"|"response.failed"|"response.incomplete")) {terminal=Some(event);continue;}
                if send(&tx,event,&cancel).await.is_err(){cancel.cancel();break;}
            }
            (terminal,sequence,access_error)
        };
        let work=state.profiles.direct_response(&job,&state.operations,&request,&history,native_tx,execution_cancel.clone(),&context);
        let (result,(terminal,sequence,access_error))=tokio::join!(work,pump);
        let mut response_error=access_error.or_else(||state.operations.check_inference_route(account,&access,&job.provider,&request.model).err());
        let access_denied=response_error.is_some();
        let incomplete=result.model.as_ref().is_ok_and(|s|s.outcome=="incomplete") && result.cleanup_error.is_none();
        let mut completed_affinity=None;
        if !access_denied && (result.error().is_none() || incomplete) {
            if let Some(event)=&terminal {
                if let Some(control)=&request.control {history.extend(control.additions());}
                response_error=state.responses.remember_exchange(&access,&history,&event["response"],&current_binding).err();
                if request.compact {history=if request.tools.is_empty(){vec![]}else{vec![json!({"type":"additional_tools","role":"developer","tools":request.tools})]};}
                completed_affinity=if crate::native_cache::enabled() { None } else { affinity::completed(&request,&history,&event["response"]) };
                if response_error.is_none() {if let Some(cache)=&cache {response_error=cache.save_bound(&access,history,&event["response"],Some(current_binding),None).err();}}
            } else {response_error=Some(Error::new("UPSTREAM_INVALID_RESPONSE","原生响应没有终态。"));}
        }
        let settled=lease.finish_execution(&result,response_error.as_ref());
        if settled.is_ok() && response_error.is_none() && result.error().is_none() {
            if let Some(digest)=completed_affinity {state.operations.remember_soft_affinity(&access,&request.model,digest,account,&job.provider);}
        }
        let failure=response_error.or(if incomplete {None}else{result.error()}).or_else(||settled.err());
        let output=if let Some(e)=failure {
            if e.code=="NATIVE_TOOL_SCOPE" {eprintln!("{}",json!({"event":"native_tool_scope_rejected","requestId":context.id,"code":e.code,"message":e.message}));}
            if let Some(mut event)=terminal {
                if access_denied || request.compact {event["response"]["output"]=json!([]);}
                event["type"]=json!("response.failed");event["response"]["status"]=json!("failed");event["response"]["error"]=json!({"code":e.code,"message":e.message});event
            } else {json!({"type":"error","sequence_number":sequence,"code":e.code,"message":e.message,"param":null,"request_id":context.id})}
        } else {terminal.unwrap()};
        if output["type"]!="response.completed" {
        let (transport,status,body)=if cache.is_some() {
            let mut body=output.clone();
            if body["type"]=="error" && body["error"].is_null() {
                body["error"]=websocket::error_event(Error::new(body["code"].as_str().unwrap_or("UPSTREAM_REQUEST_FAILED"),body["message"].as_str().unwrap_or("原生请求失败。")))["error"].clone();
            }
            if let Some(lane)=request.original["body"]["stream_id"].as_str() {body["stream_id"]=json!(lane);}
            ("websocket",None,body)
        } else if request.stream { ("sse",Some(200),output.clone()) }
        else if output["type"]=="error" {
            let (status,body)=crate::client_api::client_error_payload(&Error::new(output["code"].as_str().unwrap_or("UPSTREAM_INVALID_RESPONSE"),output["message"].as_str().unwrap_or("原生响应失败。")));
            ("http",Some(status.as_u16()),body)
        } else { ("http",Some(200),output["response"].clone()) };
            let body=if request.compact {json!({"type":output["type"],"error":{"code":output.get("code").unwrap_or(&output["response"]["error"]["code"])}})}else{body};
            let _=state.operations.record_failure_content(context.id,Some(&request.original),&body,transport,status);
        }
        // A policy/user cancellation stops native work, but an attached consumer
        // must still receive its failure terminal. Disconnected consumers fail
        // the bounded send immediately; service shutdown cancels delivery too.
        let keep_native=output["type"]=="response.completed" && !cancel.is_cancelled() && !execution_cancel.is_cancelled();
        if keep_native { context.native_handoff.publish(); }
        let delivered=send(&tx,output,&state.shutdown).await.is_ok();
        context.native_handoff.finish(keep_native && delivered);
        let _=state.operations.record_delivery(context.id,delivered);
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
    tokio::select! {_ = cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","请求已取消。")), r=tokio::time::timeout(Duration::from_secs(5),tx.send(v))=>r.map_err(|_|Error::new("STREAM_BACKPRESSURE","客户端读取超时。"))?.map_err(|_|Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。"))}
}

pub async fn run(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &FilePath,
    request: &Request,
    history: &[Value],
    outgoing: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<StreamSummary> {
    run_profiled(
        rpc, provider, cwd, request, history, outgoing, cancel, None, None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_profiled(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &FilePath,
    request: &Request,
    history: &[Value],
    outgoing: mpsc::Sender<Value>,
    cancel: CancellationToken,
    mut timing: Option<&mut crate::execution::PreflightTiming>,
    mut reuse: Option<reuse::Reuse<'_>>,
) -> Result<StreamSummary> {
    let resolved = request.with_history(history)?;
    let request = &resolved;
    if cancel.is_cancelled() {
        return Err(Error::new("REQUEST_CANCELLED", "请求已取消。"));
    }
    if request.compact {
        return Err(native_compaction_requires_transport());
    }
    tokio::select! {
        _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
        result = service_tier::verify(rpc, request) => result?,
    }
    rpc.enable_client_tools();
    let previous = reuse.as_mut().and_then(|reuse| reuse.session.take());
    let tracker = reuse
        .as_ref()
        .and_then(|r| r.continuation.as_ref())
        .and_then(|c| c.tracker.clone());
    if let Some(control) = &request.control {
        control.set_tracker(tracker.clone());
    }
    let mut native_context = reuse::Context::new(
        previous
            .as_ref()
            .and_then(|session| session.native_checkpoint),
    );
    let mut default_effort = previous
        .as_ref()
        .filter(|session| !session.model_changed(request))
        .and_then(|session| session.default_effort.clone());
    let default_summary = previous
        .as_ref()
        .and_then(|session| session.default_summary.clone())
        .or_else(|| {
            reuse
                .as_ref()
                .and_then(|reuse| reuse.default_summary.clone())
        });
    let mut default_service_tier = previous
        .as_ref()
        .and_then(|session| session.default_service_tier.clone());
    let pending_update = previous
        .as_ref()
        .filter(|session| !session.calls.is_empty())
        .map(|session| session.pending_update(request))
        .transpose()?;
    let mut turn_params = json!({"input":[]});
    request.apply_turn(&mut turn_params);
    if let Some(previous) = &previous {
        if previous.model_changed(request) {
            turn_params["model"] = json!(request.model);
        }
        previous.apply_turn_defaults(request, &mut turn_params);
    }
    let offset = match (&previous, &reuse) {
        (Some(previous), Some(reuse)) => previous.continuation(reuse.access, request, history)?,
        _ => None,
    };
    let replies = previous
        .as_ref()
        .filter(|s| !s.calls.is_empty())
        .and_then(|s| continuation::replies(s, history, offset?));
    let mut tool_echoes: std::collections::HashSet<String> = previous
        .as_ref()
        .map(|s| s.calls.iter().map(|c| c.id.clone()).collect())
        .unwrap_or_default();
    let incoming = reuse
        .as_mut()
        .and_then(|r| r.continuation.as_mut())
        .and_then(|c| c.incoming.take());
    let can_keep_tools = crate::native_cache::enabled()
        && request.reuse_allowed
        && reuse.as_ref().is_some_and(|r| r.continuation.is_some());
    let (mut directory, retained_thread) = match (previous, offset) {
        (Some(previous), Some(_)) => (Some(previous.directory), Some(previous.thread)),
        (previous, _) => {
            if previous.is_some() {
                // Only an exact continuation can reuse a native thread. A shared
                // process selects that session before entering this function.
                return Err(Error::new("NATIVE_REUSE_MISMATCH", "原生对话缓存不匹配。"));
            }
            let directory = if reuse.is_some() {
                Some(
                    tempfile::Builder::new()
                        .prefix("conversation-")
                        .tempdir_in(cwd)?
                        .keep(),
                )
            } else {
                None
            };
            (directory, None)
        }
    };
    let cwd = directory.as_deref().unwrap_or(cwd);
    let mut params = crate::native_session::thread_params(
        provider,
        cwd,
        request.dynamic_tools(),
        request.instructions.as_deref(),
    )?;
    request.apply_thread(&mut params);
    if tracker.is_some() {
        params["config"]["thread_unload_delay_secs"] = json!(0);
    }
    if request.tool_choice != "none"
        && request
            .tools
            .iter()
            .any(|t| t["type"] == "image_generation")
    {
        params["config"]["features.image_generation"] = json!(true);
    }
    let thread = if let Some(thread) = retained_thread {
        if let Some(timing) = &mut timing {
            timing.reused_thread = true;
        }
        thread
    } else {
        let started = rpc.request("thread/start", params).await?;
        crate::native_session::verify_policy(&started, provider)?;
        default_effort = started["reasoningEffort"].as_str().map(str::to_owned);
        default_service_tier = match started.get("serviceTier") {
            Some(Value::Null) => Some(None),
            Some(Value::String(tier)) => Some(Some(tier.clone())),
            _ => None,
        };
        started["thread"]["id"]
            .as_str()
            .ok_or_else(|| invalid("原生线程缺少标识。"))?
            .to_owned()
    };
    let (mut notifications, resumed_guard) = match incoming {
        Some(waiting) => (waiting.events, Some(waiting.guard)),
        None => (rpc.subscribe_thread(&thread), None),
    };
    if let Some(tracker) = &tracker {
        tracker.bind(&thread)?;
    }
    if let Some(timing) = &mut timing {
        timing.mark("thread_start");
    }
    let mut items = tool_catalog::native_history_with_catalog(
        &history[offset.unwrap_or(0)..],
        request.tool_choice == "none",
        &request.tools,
    );
    items.extend(rpc.location_search_item(&request.tools, request.tool_choice == "none"));
    if resumed_guard.is_none() {
        compaction::inject(rpc, &thread, items).await?;
    }
    if let Some(timing) = &mut timing {
        timing.mark("history_injection");
    }
    turn_params["threadId"] = json!(thread);
    if let Some(timing) = &mut timing {
        timing.mark("turn_parameters");
        if resumed_guard.is_some() {
            timing.submitted_as("native_tool_results_submit");
        } else {
            timing.submitted();
        }
    }
    let resumed = resumed_guard.is_some();
    let mut turn_guard = if let Some(guard) = resumed_guard {
        if guard.thread != thread {
            return Err(invalid("续聊线程不匹配。"));
        }
        let replies = replies.ok_or_else(|| invalid("工具结果与待处理回调不匹配。"))?;
        if let Some(control) = &request.control {
            control.activate(rpc, &thread, &guard.turn);
        }
        if let Some(mut update) =
            pending_update.filter(|value| value.as_object().is_some_and(|v| !v.is_empty()))
        {
            update["threadId"] = json!(thread);
            update["turnId"] = json!(guard.turn);
            // Admission, ownership and the entire result batch are validated
            // before changing the live turn, and before replying to any tool.
            let updated = tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","请求已取消。")),
                _=outgoing.closed()=>Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                result=rpc.request("turn/settings/update", update)=>result,
            };
            let updated = updated.and_then(|value| {
                if value["status"] == "applied" {
                    Ok(())
                } else {
                    Err(Error::new(
                        "NATIVE_TURN_UNAVAILABLE",
                        "原生待答轮次未接受参数更新。",
                    ))
                }
            });
            if let Err(error) = updated {
                if let Some(control) = &request.control {
                    control.finish();
                }
                return Err(error);
            }
        }
        for (id, result) in replies {
            let sent = tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","请求已取消。")),
                _=outgoing.closed()=>Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                sent=rpc.answer_tool(id,result)=>sent,
            };
            if let Err(error) = sent {
                if let Some(control) = &request.control {
                    control.finish();
                }
                return Err(error);
            }
        }
        guard
    } else {
        let turn = rpc.request("turn/start", turn_params).await?;
        let turn = turn["turn"]["id"]
            .as_str()
            .ok_or_else(|| invalid("原生轮次缺少标识。"))?
            .to_owned();
        continuation::TurnGuard {
            rpc: rpc.clone(),
            thread: thread.clone(),
            turn,
            owner: reuse
                .as_mut()
                .and_then(|r| r.continuation.as_mut())
                .and_then(|c| c.owner.take()),
        }
    };
    if let Some(timing) = &timing
        && !resumed
    {
        timing.acknowledged();
    }
    let turn = turn_guard.turn.clone();
    if let Some(control) = &request.control {
        control.activate(rpc, &thread, &turn);
    }
    let mut output = events::Output::new(request);
    let mut budget_items = std::collections::HashSet::new();
    let mut turn_ended = false;
    let mut callbacks = Vec::new();
    let mut response_boundary = false;
    let mut callback_deadline = None;
    let work=async {
        for event in output.created(){send(&outgoing,event,&cancel).await?;}
        let deadline=tokio::time::sleep(Duration::from_secs(1800));tokio::pin!(deadline);
        loop {
            let event=tokio::select!{
                _=cancel.cancelled()=>return Err(Error::new("REQUEST_CANCELLED","请求已取消。")),
                _=outgoing.closed()=>return Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                _=rpc.closed()=>return Err(Error::new("RPC_CLOSED","原生后端已断开。")),
                _=&mut deadline=>return Err(Error::new("UPSTREAM_TIMEOUT","原生请求超过 30 分钟。")),
                _=async { if let Some(at)=callback_deadline { tokio::time::sleep_until(at).await; } else { std::future::pending::<()>().await; } }=>return Ok(()),
                e=notifications.recv()=>e.map_err(|_|Error::new("STREAM_EVENTS_LOST","原生事件中断。"))?
            };
            let method=event["method"].as_str().unwrap_or("");
            if method=="bridge/callbackRejected" {return Err(Error::new("NATIVE_CALLBACK_DENIED","原生请求了未开放的服务器能力。"));}
            let p=&event["params"];
            if method=="item/completed" && p["item"]["type"]=="imageGeneration" {
                if let Some(path)=p["item"]["savedPath"].as_str(){crate::media::validate_artifact(cwd,std::path::Path::new(path))?;}
            }
            if p["threadId"]!=thread {continue;}
            if method=="thread/closed" {return Err(Error::new("NATIVE_THREAD_CLOSED","原生对话线程已关闭。"));}
            if p.get("turnId").is_some_and(|v|v!=&turn){continue;}
            if method=="turn/completed" && p["turn"]["id"]!=turn {continue;}
            native_context.observe(method,p,output.has_calls() || !callbacks.is_empty())?;
            if method=="rawResponseItem/completed" && matches!(p["item"]["type"].as_str(),Some("function_call_output"|"custom_tool_call_output"))
                && p["item"]["call_id"].as_str().is_some_and(|id|tool_echoes.remove(id)) {continue;}
            if method=="rawResponseItem/completed" && let Some(tracker)=&tracker {
                let digest=crate::profiles::warm::digest(&p["item"])?;
                if budget_items.insert(digest) {tracker.output(serde_json::to_vec(&p["item"])?.len());}
            }
            if method=="item/tool/call" && request.client_tool_identity(p["namespace"].as_str(),p["tool"].as_str().unwrap_or("")).is_none() {return Err(tool_catalog::scope_error("原生工具不属于调用方注册的函数。",p["namespace"].as_str(),p["tool"].as_str().unwrap_or("")));}
            if method=="item/tool/call" && can_keep_tools {
                if p["turnId"]!=turn {return Err(invalid("原生工具回调轮次不匹配。"));}
                let callback=continuation::callback(request,&event)?;
                if callbacks.iter().any(|c: &continuation::Call|c.id==callback.id || c.rpc_id==callback.rpc_id) {return Err(invalid("原生返回重复工具回调。"));}
                callbacks.push(callback);
            }
            for value in output.consume(&event,request)?{send(&outgoing,value,&cancel).await?;}
            // A Responses request ends at the upstream response boundary when tools
            // are yielded. Never answer callbacks with fabricated tool results.
            if method=="rawResponse/completed" && output.has_calls() { response_boundary=true; callback_deadline=Some(tokio::time::Instant::now()+Duration::from_secs(5)); }
            if (response_boundary && (!can_keep_tools || !native_context.reusable() || !continuation::eligible(output.items()) || continuation::calls_match(&callbacks,output.items()))) || method=="turn/completed" {
                turn_ended = method=="turn/completed";
                if method=="turn/completed" && p["turn"]["status"]=="interrupted" && request.control.as_ref().is_some_and(|c|c.interrupt_requested()) {
                    output.interrupted();return Ok(());
                }
                if method=="turn/completed" && p["turn"]["status"]!="completed" {return Err(crate::execution::native_failure(&p["turn"]["error"]));}
                return Ok(());
            }
        }
    }.await;
    if let Some(control) = &request.control {
        control.finish();
    }
    // A shared host cannot be killed to cancel one caller. Wait for that turn's
    // completion event; an uncertain interruption retires the host after draining.
    let mut cleanup_confirmed = turn_ended;
    let history_changed = request
        .control
        .as_ref()
        .is_some_and(|control| !control.cache_reusable());
    let parked = can_keep_tools
        && !history_changed
        && native_context.reusable()
        && !turn_ended
        && work.is_ok()
        && !cancel.is_cancelled()
        && history.len() + output.items().len() <= 1024
        && continuation::calls_match(&callbacks, output.items());
    if !parked && !turn_ended && (output.has_calls() || work.is_err()) {
        let acknowledged = rpc
            .request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
            .await
            .is_ok();
        if acknowledged {
            cleanup_confirmed = tokio::time::timeout(Duration::from_secs(5), async {
                while let Ok(event) = notifications.recv().await {
                    if event["method"] == "turn/completed"
                        && event["params"]["threadId"] == thread
                        && event["params"]["turn"]["id"] == turn
                    {
                        return true;
                    }
                }
                false
            })
            .await
            .unwrap_or(false);
        }
    }
    if !parked {
        rpc.clear_thread_tool_callbacks(&thread);
    }
    if cleanup_confirmed {
        let owner = turn_guard.disarm();
        if let Some(continuation) = reuse.as_mut().and_then(|r| r.continuation.as_mut()) {
            continuation.owner = owner;
        }
    }
    if let Err(error) = work {
        let _ = send(&outgoing, output.failed(&error), &cancel).await;
        if let Ok(mut summary) = output.summary(&thread, &turn) {
            summary.cleanup_confirmed = cleanup_confirmed;
            summary.outcome = if crate::execution::cancelled(&error.code) {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            summary.error_code = Some(error.code);
            summary.error_message = Some(error.message);
            summary.native_status = Some(
                if summary.outcome == "cancelled" {
                    "interrupted"
                } else {
                    "failed"
                }
                .into(),
            );
            return Ok(summary);
        }
        return Err(error);
    }
    let mut summary = output.summary(&thread, &turn)?;
    summary.cleanup_confirmed = cleanup_confirmed;
    if parked {
        summary.native_status = Some("waiting_for_tool".into());
    }
    let completed = output.completed()?;
    if let Some(reuse) = &mut reuse {
        if summary.outcome == "completed"
            && request.reuse_allowed
            && !history_changed
            && (!output.has_calls() || parked)
            && native_context.reusable()
            && (crate::native_cache::enabled()
                || (request.tools.is_empty() && reuse::text_history(history)))
            && history.len() + completed["response"]["output"].as_array().unwrap().len() <= 1024
        {
            *reuse.session = Some(reuse::Session::new(
                thread,
                directory.take().unwrap(),
                reuse.access,
                request,
                history,
                completed["response"]["output"].as_array().unwrap(),
            )?);
            reuse.session.as_mut().unwrap().default_effort = default_effort;
            reuse.session.as_mut().unwrap().default_summary = default_summary;
            reuse.session.as_mut().unwrap().default_service_tier = default_service_tier;
            reuse.session.as_mut().unwrap().native_checkpoint = native_context.checkpoint();
            if parked {
                reuse.session.as_mut().unwrap().calls = callbacks;
            }
        }
    }
    if parked {
        let continuation = reuse
            .as_mut()
            .and_then(|r| r.continuation.as_mut())
            .unwrap();
        continuation.waiting = Some(continuation::Waiting {
            guard: turn_guard,
            events: notifications,
        });
    }
    send(&outgoing, completed, &cancel).await?;
    Ok(summary)
}
