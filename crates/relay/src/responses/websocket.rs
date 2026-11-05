//! Responses wire protocol on the ingress socket, sharing the HTTP execution path.
mod steering;
use super::{Request, Store, control::Control, storage::Continuation};
use crate::{
    admin::AdminState,
    error::{Error, Result},
    inference::CancelOnDrop,
    operations::ClientAccess,
};
use axum::{
    extract::{
        Extension, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

type LaneId = Option<String>;
#[derive(Default)]
struct Lane {
    queue: VecDeque<Value>,
    active: Option<(Arc<Control>, Option<String>)>,
}
enum Signal {
    Event(LaneId, Value),
    Done(LaneId),
}
type Recent = (HashMap<String, (LaneId, Arc<Control>)>, VecDeque<String>);

fn interrupt_event(
    frame: &Value,
    lanes: &HashMap<LaneId, Lane>,
    recent: &Recent,
) -> impl std::future::Future<Output = Value> + use<> {
    let response = frame["response_id"].clone();
    let mut selected_lane = None;
    let selected = (|| {
        let response_id = response
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| super::invalid("response.interrupt 缺少有效 response_id。"))?;
        if frame["mode"] != "discard_partial_items" {
            return Err(super::invalid(
                "response.interrupt.mode 仅支持 discard_partial_items。",
            ));
        }
        let (lane, control) = lanes
            .iter()
            .find_map(|(lane, state)| {
                state
                    .active
                    .as_ref()
                    .filter(|(_, id)| id.as_deref() == Some(response_id))
                    .map(|(control, _)| (lane.clone(), control.clone()))
            })
            .or_else(|| recent.0.get(response_id).cloned())
            .ok_or_else(|| Error::new("RESPONSE_NOT_ACTIVE", "当前连接没有该响应。"))?;
        if frame.get("stream_id").is_some() && lane_id(frame)? != lane {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "中断的 stream_id 与 response_id 不匹配。",
            ));
        }
        selected_lane = lane;
        Ok(control)
    })();
    async move {
        let result: Result<()> = match selected {
            Ok(control) => control.interrupt().await,
            Err(error) => Err(error),
        };
        let mut event = match result {
            Ok(()) => json!({"type":"response.interrupt.accepted","response_id":response}),
            Err(error) => {
                json!({"type":"response.interrupt.failed","response_id":response,"error":{"code":error.code,"message":error.message}})
            }
        };
        if let Some(lane) = selected_lane {
            event["stream_id"] = json!(lane);
        }
        event
    }
}
pub async fn endpoint(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let cache = Arc::new(Store::default());
    if let Err(error) = state.responses.connect_cache(&cache) {
        return crate::client_api::client_error(error);
    }
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    upgrade
        .max_message_size(crate::media::BODY_LIMIT)
        .max_frame_size(crate::media::BODY_LIMIT)
        .on_upgrade(move |socket| async move {
            let task = state.tasks.clone().spawn(async move {
                let _ = session(
                    socket,
                    state,
                    access,
                    key,
                    context.user_agent,
                    context.subagent_kind,
                    context.identity_headers,
                    cache,
                )
                .await;
            });
            let _ = task.await;
        })
}
fn lane_id(frame: &Value) -> Result<LaneId> {
    match frame.get("stream_id") {
        None => Ok(None),
        Some(Value::String(id))
            if !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)) =>
        {
            Ok(Some(id.clone()))
        }
        _ => Err(super::invalid(
            "stream_id 必须是 1–256 个字母、数字、下划线、点或连字符。",
        )),
    }
}
pub(super) fn error_event(error: Error) -> Value {
    let code = if error.code == "RESPONSE_NOT_FOUND" {
        "previous_response_not_found"
    } else {
        &error.code
    };
    let kind = match error.code.as_str() {
        "INVALID_API_KEY" => "authentication_error",
        "MODEL_NOT_ALLOWED" | "API_KEY_GROUP_DISABLED" => "permission_error",
        "QUEUE_FULL" | "CONCURRENCY_LIMIT" | "RESPONSE_STORAGE_FULL" | "UPSTREAM_RATE_LIMITED" => {
            "rate_limit_error"
        }
        code if ["UPSTREAM_", "NATIVE_", "RPC_", "BACKEND_", "STREAM_"]
            .iter()
            .any(|p| code.starts_with(p)) =>
        {
            "server_error"
        }
        _ => "invalid_request_error",
    };
    json!({"type":"error","code":code,"message":error.message,"error":{"type":kind,"code":code,"message":error.message,"param":null}})
}
async fn emit(tx: &mpsc::Sender<Signal>, lane: &LaneId, event: Value) -> Result<()> {
    let _ = DEBUG_CAPTURE.try_with(|capture| {
        if let Some(capture) = &mut *capture.lock().unwrap() {
            let mut frame = event.clone();
            if let Some(lane) = lane {
                frame["stream_id"] = json!(lane);
            }
            capture.response_frame(&frame);
        }
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        tx.send(Signal::Event(lane.clone(), event)),
    )
    .await
    .map_err(|_| Error::new("STREAM_BACKPRESSURE", "WebSocket 客户端读取超时。"))?
    .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "WebSocket 已关闭。"))
}
fn inherit_warmup(cache: &Store, global: &Store, access: &ClientAccess, frame: &mut Value) {
    if let Some(id) = frame["previous_response_id"].as_str() {
        if let Ok(Some(defaults)) = cache
            .defaults(access, id)
            .or_else(|_| global.defaults(access, id))
        {
            for (name, value) in defaults.as_object().unwrap() {
                frame
                    .as_object_mut()
                    .unwrap()
                    .entry(name)
                    .or_insert_with(|| value.clone());
            }
        }
    }
}
tokio::task_local! {
    static DEBUG_CAPTURE: std::sync::Mutex<Option<crate::debug_capture::Capture>>;
}
#[allow(clippy::too_many_arguments)]
async fn execute(
    state: Arc<AdminState>,
    access: ClientAccess,
    cache: Arc<Store>,
    frame: Value,
    lane: LaneId,
    control: Arc<Control>,
    tx: mpsc::Sender<Signal>,
    cancel: CancellationToken,
    user_agent: Option<String>,
    subagent_kind: Option<String>,
    identity_headers: super::affinity::identity::Headers,
) -> Result<()> {
    let id = uuid::Uuid::new_v4();
    let mut capture = crate::debug_capture::Capture::new(state.operations.clone(), id, "websocket");
    if let Some(capture) = &mut capture {
        capture.request_frame(&frame);
    }
    DEBUG_CAPTURE
        .scope(std::sync::Mutex::new(capture), async move {
            let debug_lane = lane.clone();
            let result = execute_inner(
                state,
                access,
                cache,
                frame,
                lane,
                control,
                tx,
                cancel,
                user_agent,
                subagent_kind,
                identity_headers,
                id,
            )
            .await;
            DEBUG_CAPTURE.with(|capture| {
                if let Some(capture) = &mut *capture.lock().unwrap() {
                    if let Err(error) = &result {
                        let mut event = error_event(error.clone());
                        if let Some(lane) = debug_lane {
                            event["stream_id"] = json!(lane);
                        }
                        capture.response_frame(&event);
                    } else {
                        capture.complete();
                    }
                }
            });
            result
        })
        .await
}
#[allow(clippy::too_many_arguments)]
async fn execute_inner(
    state: Arc<AdminState>,
    access: ClientAccess,
    cache: Arc<Store>,
    mut frame: Value,
    lane: LaneId,
    control: Arc<Control>,
    tx: mpsc::Sender<Signal>,
    cancel: CancellationToken,
    user_agent: Option<String>,
    subagent_kind: Option<String>,
    identity_headers: super::affinity::identity::Headers,
    debug_id: uuid::Uuid,
) -> Result<()> {
    let subagent_kind = crate::subagent::from_body(&frame).or(subagent_kind);
    let generate = match frame.get("generate") {
        None => true,
        Some(Value::Bool(value)) => *value,
        _ => return Err(super::invalid("generate 必须是布尔值。")),
    };
    let direct_candidate = crate::upstream::dispatch::has_routes(&state, &access)
        && !frame["previous_response_id"]
            .as_str()
            .is_some_and(|id| cache.continuation(&access, id).is_ok());
    let native_prewarm = if !generate && direct_candidate {
        crate::upstream::dispatch::native_prewarm_route(&state, &access, &frame)?
            .map(|(account, provider)| {
                Ok::<_, Error>(super::storage::Binding {
                    account,
                    model: frame["model"].as_str().unwrap().into(),
                    provider: super::reuse::provider_digest(&provider)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    let mut admission = None;
    if native_prewarm.is_none() && direct_candidate {
        let mut context = crate::execution::RequestContext {
            id: debug_id,
            endpoint: Some("/v1/responses".into()),
            websocket: true,
            identity_headers,
            user_agent: user_agent.clone(),
            subagent_kind: subagent_kind.clone(),
            ..Default::default()
        };
        let mut direct_frame = frame.clone();
        access.apply_fast_mode(&mut direct_frame);
        direct_frame["stream"] = json!(true);
        let raw = crate::upstream::dispatch::Raw::json("responses", direct_frame)?;
        let lease = crate::upstream::dispatch::prepare(&state, &access, &context, &raw).await?;
        if lease
            .provider
            .as_ref()
            .is_some_and(|p| p.execution == crate::provider::Execution::ApiKey)
        {
            let response = crate::upstream::dispatch::forward(
                state.clone(),
                access.clone(),
                context,
                raw,
                lease,
            )
            .await;
            let status = response.status();
            let sse = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            let mut stream = response.into_body().into_data_stream();
            let mut parser = crate::upstream::dispatch::Events::default();
            let mut document = Vec::new();
            loop {
                let chunk =
                    tokio::select! {_=cancel.cancelled()=>return Ok(()),chunk=stream.next()=>chunk};
                let Some(chunk) = chunk else {
                    break;
                };
                let chunk =
                    chunk.map_err(|_| Error::new("UPSTREAM_CONNECTION_FAILED", "上游流中断。"))?;
                if sse {
                    for event in parser.push(&chunk)? {
                        emit(&tx, &lane, event).await?;
                    }
                } else {
                    if document.len() + chunk.len() > 64 * 1024 * 1024 {
                        return Err(super::invalid("上游响应过大。"));
                    }
                    document.extend_from_slice(&chunk);
                }
            }
            if !sse {
                let value: Value = serde_json::from_slice(&document)
                    .unwrap_or_else(|_| json!({"message":"上游未返回 JSON。"}));
                let event = if status.is_success() {
                    json!({"type":"response.completed","response":value})
                } else {
                    json!({"type":"error","status":status.as_u16(),"error":value.get("error").unwrap_or(&value)})
                };
                emit(&tx, &lane, event).await?;
            }
            return Ok(());
        }
        if !generate {
            return lease.complete(Err(Error::new(
                "ROUTE_CHANGED",
                "预热期间路由已改变，请重新预热。",
            )));
        }
        context.admit(lease);
        admission = context.admission;
    }
    let original = Arc::new(crate::operations::request_content::request_snapshot(&frame));
    inherit_warmup(&cache, &state.responses, &access, &mut frame);
    if frame.get("input").is_none() {
        frame["input"] = json!([]);
    }
    // Keep client warmup defaults separate from the key's current forced tier.
    let mut effective_frame = frame.clone();
    access.apply_fast_mode(&mut effective_frame);
    let mut request = Request::parse(effective_frame)?;
    // Warmup defaults must not turn a later unmarked request into a subagent.
    request.subagent_kind = subagent_kind.clone();
    request.original = original;
    if !access.allows(&request.model) {
        return Err(Error::new("MODEL_NOT_ALLOWED", "此密钥未授权该模型。"));
    }
    if !generate {
        let prior = match &request.previous {
            Some(id) => cache
                .continuation(&access, id)
                .or_else(|_| state.responses.continuation(&access, id))?,
            None => Continuation {
                history: vec![],
                binding: None,
            },
        };
        if prior
            .binding
            .as_ref()
            .is_some_and(|b| b.model != request.model)
        {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "预热不能更改已有响应链的模型。",
            ));
        }
        let mut history = prior.history;
        history.extend(request.input.clone());
        let binding = state
            .responses
            .binding_for_history(&access, &request.model, &history)?
            .or(prior.binding)
            .or(native_prewarm);
        super::request::validate_history(&history)?;
        let resolved = request.with_history(&history)?;
        resolved.required_features(&history)?;
        let mut defaults = frame.as_object().unwrap().clone();
        for key in [
            "type",
            "generate",
            "stream_id",
            "previous_response_id",
            "input",
            "event_id",
            "store",
            "stream",
        ] {
            defaults.remove(key);
        }
        let response = json!({"id":format!("resp_{}",uuid::Uuid::new_v4().simple()),"object":"response","created_at":crate::now_ms()/1000,"status":"completed","model":request.model,"output":[],"usage":null,"tools":request.tools,"store":request.store,"previous_response_id":request.previous});
        cache.save_bound(&access, history, &response, binding, Some(json!(defaults)))?;
        cache.pin_latest(lane.as_deref(), response["id"].as_str().unwrap());
        let mut initial = response.clone();
        initial["status"] = json!("in_progress");
        emit(
            &tx,
            &lane,
            json!({"type":"response.created","sequence_number":0,"response":initial}),
        )
        .await?;
        return emit(
            &tx,
            &lane,
            json!({"type":"response.completed","sequence_number":1,"response":response}),
        )
        .await;
    }
    request.stream = true;
    request.control = Some(control);
    let context = crate::execution::RequestContext {
        id: debug_id,
        endpoint: Some("/v1/responses".into()),
        websocket: true,
        identity_headers,
        user_agent,
        subagent_kind,
        admission,
        ..Default::default()
    };
    let (mut events, guard, _) = tokio::select! {
        _=cancel.cancelled()=>return Ok(()),
        result=super::start(state,access,request,context,Some(cache.clone()))=>result?,
    };
    let _guard = guard;
    loop {
        let event = tokio::select! {_=cancel.cancelled()=>break,event=events.recv()=>event};
        let Some(mut event) = event else {
            break;
        };
        if matches!(
            event["type"].as_str(),
            Some("response.completed" | "response.incomplete")
        ) {
            if let Some(id) = event["response"]["id"].as_str() {
                cache.pin_latest(lane.as_deref(), id);
            }
        }
        if event["type"] == "error" && event["error"].is_null() {
            event["error"] = error_event(Error::new(
                event["code"].as_str().unwrap_or("UPSTREAM_REQUEST_FAILED"),
                event["message"].as_str().unwrap_or("原生请求失败。"),
            ))["error"]
                .clone();
        }
        emit(&tx, &lane, event).await?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn session(
    socket: WebSocket,
    state: Arc<AdminState>,
    owner: ClientAccess,
    key: String,
    user_agent: Option<String>,
    subagent_kind: Option<String>,
    identity_headers: super::affinity::identity::Headers,
    cache: Arc<Store>,
) -> Result<()> {
    let cancel = state.shutdown.child_token();
    let _guard = CancelOnDrop(cancel.clone());
    let (mut sink, mut incoming) = socket.split();
    let mut lanes: HashMap<LaneId, Lane> = HashMap::new();
    let mut recent: Recent = Default::default();
    let (tx, mut rx) = mpsc::channel(32);
    let mut tasks = JoinSet::new();
    let (steering, worker) = steering::SteeringQueue::new(tx.clone(), cancel.clone());
    tasks.spawn(worker);
    let mut queued_bytes = 0usize;
    let mut event_ids = HashSet::new();
    let mut check = tokio::time::interval(Duration::from_secs(10));
    let lifetime = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(lifetime);
    loop {
        // FIFO within a lane; different lanes share the normal admission limits.
        for (id, lane) in &mut lanes {
            if lane.active.is_some() {
                continue;
            }
            let Some(frame) = lane.queue.pop_front() else {
                continue;
            };
            queued_bytes = queued_bytes.saturating_sub(serde_json::to_vec(&frame)?.len());
            let access = match state.operations.authenticate_api_key(&key) {
                Ok(access) if access.key_id == owner.key_id => access,
                _ => {
                    cancel.cancel();
                    break;
                }
            };
            let control = Arc::new(Control::default());
            control.set_authorization(state.operations.clone(), access.clone());
            let user_agent = user_agent.clone();
            let subagent_kind = subagent_kind.clone();
            lane.active = Some((control.clone(), None));
            let (state, cache, tx, cancel, id) = (
                state.clone(),
                cache.clone(),
                tx.clone(),
                cancel.clone(),
                id.clone(),
            );
            tasks.spawn(async move {
                let result = execute(
                    state,
                    access,
                    cache,
                    frame,
                    id.clone(),
                    control.clone(),
                    tx.clone(),
                    cancel,
                    user_agent,
                    subagent_kind,
                    identity_headers,
                )
                .await;
                control.finish();
                if let Err(error) = result {
                    let _ = emit(&tx, &id, error_event(error)).await;
                }
                let _ = tx.send(Signal::Done(id)).await;
            });
        }
        let outbound = tokio::select! {
            _=cancel.cancelled()=>break,
            _=&mut lifetime=>break,
            _=check.tick()=>{
                if !state.operations.authenticate_api_key(&key).is_ok_and(|a|a.key_id==owner.key_id){break;}
                continue;
            },
            _=tasks.join_next(),if !tasks.is_empty()=>continue,
            signal=rx.recv()=>{
                match signal {
                    Some(Signal::Done(id))=>{
                        if let Some(lane)=lanes.get_mut(&id) {
                            if let Some((control,Some(response)))=lane.active.take() {
                                recent.1.push_back(response.clone());recent.0.insert(response,(id,control));
                                while recent.1.len()>128 {if let Some(old)=recent.1.pop_front(){recent.0.remove(&old);}}
                            }
                        }
                        continue;
                    }
                    Some(Signal::Event(id,mut event))=>{
                        if event["type"]=="response.created" {if let Some(Lane{active:Some((_,response)),..})=lanes.get_mut(&id){*response=event["response"]["id"].as_str().map(str::to_owned);}}
                        if let Some(id)=id{event["stream_id"]=json!(id);}Some(Message::Text(event.to_string().into()))
                    },
                    None=>break,
                }
            },
            frame=incoming.next()=>{
                let Some(Ok(frame))=frame else {break;};
                let text=match frame {Message::Text(text)=>text,Message::Ping(bytes)=>{
                    if !tokio::time::timeout(Duration::from_secs(5),sink.send(Message::Pong(bytes))).await.is_ok_and(|r|r.is_ok()){break;}continue;
                },Message::Pong(_)=>continue,Message::Close(_)=>break,_=>{
                    let message=Message::Text(error_event(super::invalid("Responses WebSocket 仅接受 JSON 文本消息。")).to_string().into());
                    if !tokio::time::timeout(Duration::from_secs(5),sink.send(message)).await.is_ok_and(|r|r.is_ok()){break;}continue;
                }};
                let result=async {
                    let access=state.operations.authenticate_api_key(&key)?;
                    if access.key_id!=owner.key_id{return Err(Error::new("INVALID_API_KEY","连接的密钥身份已变化。"));}
                    let frame:Value=serde_json::from_str(&text).map_err(|_|super::invalid("WebSocket 消息必须是 JSON。"))?;
                    if frame["type"]=="response.interrupt" {
                        if tasks.len()>=96 {return Err(Error::new("QUEUE_FULL","WebSocket 控制请求达到容量上限。"));}
                        let event=interrupt_event(&frame,&lanes,&recent);
                        let tx=tx.clone();
                        tasks.spawn(async move {let _=emit(&tx,&None,event.await).await;});
                        return Ok(None);
                    }
                    let id=lane_id(&frame)?;
                    match frame["type"].as_str() {
                        Some("response.create")=>{
                            if !lanes.contains_key(&id) && id.is_some() && lanes.keys().filter(|id|id.is_some()).count()>=32 {return Err(super::invalid("单连接最多 32 个命名 stream_id。"));}
                            let bytes=text.len();
                            if queued_bytes+bytes>64*1024*1024 || lanes.values().map(|l|l.queue.len()).sum::<usize>()>=64 {return Err(Error::new("QUEUE_FULL","WebSocket 待处理请求达到容量上限。"));}
                            if let Some(event_id)=frame["event_id"].as_str(){if event_id.len()>256 || event_ids.len()>=4096 || !event_ids.insert(event_id.to_owned()){return Err(super::invalid("event_id 重复或达到连接容量上限。"));}}
                            // Use serialized size consistently for queue accounting.
                            queued_bytes+=serde_json::to_vec(&frame)?.len();lanes.entry(id).or_default().queue.push_back(frame);Ok(None)
                        },
                        Some("response.steer")=>{
                            let (control,response)=lanes.get(&id).and_then(|l|l.active.as_ref()).ok_or_else(||Error::new("RESPONSE_NOT_ACTIVE","该 stream_id 没有进行中的响应。"))?;
                            if frame["response_id"].as_str().is_some_and(|id|response.as_deref()!=Some(id)){return Err(Error::new("RESPONSE_CHAIN_MISMATCH","追加输入的 response_id 不匹配。"));}
                            steering.enqueue(control.clone(), id.clone(), frame["input"].clone(), text.len())?;Ok(None)
                        },
                        _=>Err(super::invalid("不支持的 Responses WebSocket 消息类型。")),
                    }
                }.await;
                match result {Ok(value)=>value,Err(error)=>{
                    let mut event=error_event(error);
                    if let Ok(frame)=serde_json::from_str::<Value>(&text){if let Ok(Some(id))=lane_id(&frame){event["stream_id"]=json!(id);}}
                    Some(Message::Text(event.to_string().into()))
                }}
            }
        };
        if let Some(message) = outbound {
            if !tokio::time::timeout(Duration::from_secs(5), sink.send(message))
                .await
                .is_ok_and(|r| r.is_ok())
            {
                break;
            }
        }
    }
    cancel.cancel();
    tasks.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn interrupts_are_scoped_idempotent_and_require_native_acknowledgement() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (client, peer) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client);
        let rpc = crate::rpc::Rpc::from_io(read, write);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let task = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let v: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["method"], "turn/interrupt");
                assert_eq!(v["params"]["threadId"], "thread-one");
                observed.fetch_add(1, Ordering::SeqCst);
                write
                    .write_all(format!("{}\n", json!({"id":v["id"],"result":{}})).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let control = Arc::new(Control::default());
        control.activate(&rpc, "thread-one", "turn-one");
        let mut lanes = HashMap::new();
        lanes.insert(
            Some("one".into()),
            Lane {
                queue: VecDeque::new(),
                active: Some((control.clone(), Some("resp-one".into()))),
            },
        );
        let recent = Recent::default();
        for frame in [
            json!({"type":"response.interrupt","response_id":"other","mode":"discard_partial_items"}),
            json!({"type":"response.interrupt","response_id":"resp-one","stream_id":"other","mode":"discard_partial_items"}),
            json!({"type":"response.interrupt","response_id":"resp-one","mode":"unknown"}),
        ] {
            assert_eq!(
                interrupt_event(&frame, &lanes, &recent).await["type"],
                "response.interrupt.failed"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let frame = json!({"type":"response.interrupt","response_id":"resp-one","mode":"discard_partial_items"});
        for _ in 0..2 {
            let result = interrupt_event(&frame, &lanes, &recent).await;
            assert_eq!(result["type"], "response.interrupt.accepted");
            assert_eq!(result["stream_id"], "one");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        control.finish();
        let recent = (
            HashMap::from([("resp-one".into(), (Some("one".into()), control))]),
            VecDeque::new(),
        );
        assert_eq!(
            interrupt_event(&frame, &HashMap::new(), &recent).await["type"],
            "response.interrupt.accepted"
        );
        rpc.close();
        task.abort();
    }
    #[test]
    fn stream_names_and_error_codes_follow_the_wire_protocol() {
        assert_eq!(lane_id(&json!({})).unwrap(), None);
        assert_eq!(
            lane_id(&json!({"stream_id":"a.b-1"})).unwrap(),
            Some("a.b-1".into())
        );
        for id in [json!(""), json!(null), json!("a b"), json!(1)] {
            assert!(lane_id(&json!({"stream_id":id})).is_err());
        }
        assert_eq!(
            error_event(Error::new("RESPONSE_NOT_FOUND", "missing"))["error"]["code"],
            "previous_response_not_found"
        );
    }
}
