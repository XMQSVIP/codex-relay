//! Native client sessions: RPC only. Model HTTP/TLS/WebSocket traffic is owned by app-server.
use crate::{
    admin::AdminState,
    error::{Error, Result},
    operations::ClientAccess,
    provider::Provider,
    rpc::Rpc,
    streaming::StreamSummary,
};
use axum::{
    extract::{
        Extension, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    model: String,
    #[serde(default, rename = "dynamicTools")]
    tools: Vec<Value>,
    #[serde(default, rename = "capabilities")]
    features: Vec<crate::operations::Feature>,
    #[serde(default, rename = "developerInstructions")]
    instructions: Option<String>,
}
fn invalid() -> Error {
    Error::new("INVALID_INPUT", "原生会话消息格式或操作不支持。")
}
fn start_params(value: &Value) -> Result<Start> {
    crate::media::budget(value, 1024 * 1024)?;
    if value["method"] != "thread/start" || !(value["id"].is_string() || value["id"].is_number()) {
        return Err(invalid());
    }
    let mut s: Start = serde_json::from_value(value["params"].clone()).map_err(|_| invalid())?;
    if s.model.is_empty() || s.model.len() > 256 || s.tools.len() > 128 {
        return Err(invalid());
    }
    s.tools = crate::protocol_tools::from_native(&s.tools)?;
    if s.tools.iter().any(|t| t["type"] == "namespace")
        && !s
            .features
            .contains(&crate::operations::Feature::NamespacedTools)
    {
        s.features.push(crate::operations::Feature::NamespacedTools);
    }
    if s.features.iter().any(|f| {
        matches!(
            f,
            crate::operations::Feature::WebSearch | crate::operations::Feature::ImageGeneration
        )
    }) {
        return Err(Error::new(
            "INVALID_INPUT",
            "原生 WebSocket 的搜索和生图暂未开放，请使用 Responses 工具入口。",
        ));
    }
    Ok(s)
}
pub(crate) fn required_features(start: &Value) -> Result<Vec<crate::operations::Feature>> {
    Ok(start_params(start)?.features)
}
pub async fn endpoint(
    State(state): State<Arc<AdminState>>,
    Extension(access): Extension<ClientAccess>,
    Extension(context): Extension<crate::execution::RequestContext>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(crate::media::BODY_LIMIT)
        .max_frame_size(crate::media::BODY_LIMIT)
        .on_upgrade(move |socket| async move {
            let task = state
                .tasks
                .clone()
                .spawn(socket_session(socket, state, access, context));
            let _ = task.await;
        })
}
async fn emit(tx: &mpsc::Sender<Value>, v: Value) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), tx.send(v))
        .await
        .map_err(|_| Error::new("STREAM_BACKPRESSURE", "客户端读取超时。"))?
        .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。"))
}
async fn socket_session(
    socket: WebSocket,
    state: Arc<AdminState>,
    access: ClientAccess,
    context: crate::execution::RequestContext,
) {
    let cancel = state.shutdown.child_token();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Value>(16);
    let (in_tx, mut incoming) = mpsc::channel::<Value>(16);
    let debug = std::sync::Arc::new(std::sync::Mutex::new(crate::debug_capture::Capture::new(
        state.operations.clone(),
        context.id,
        "websocket",
    )));
    let read_debug = debug.clone();
    let read_cancel = cancel.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) => {
                        if let Some(capture) = &mut *read_debug.lock().unwrap() {
                            capture.request_frame(&v);
                        }
                        #[cfg(test)]
                        crate::wire_capture::record_native_frame("received", &v);
                        if in_tx.send(v).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Message::Ping(_) | Message::Pong(_) => {}
                _ => break,
            }
        }
        read_cancel.cancel();
    });
    let write_debug = debug.clone();
    let write_cancel = cancel.clone();
    let writer = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            if let Some(capture) = &mut *write_debug.lock().unwrap() {
                capture.response_frame(&v);
            }
            if !tokio::time::timeout(
                Duration::from_secs(5),
                sink.send(Message::Text(v.to_string().into())),
            )
            .await
            .is_ok_and(|r| r.is_ok())
            {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), sink.close()).await;
        write_cancel.cancel();
    });
    let mut original = None;
    let result=async {
        let start=tokio::select! { _=cancel.cancelled()=>return Err(Error::new("STREAM_CANCELLED","会话已取消。")), r=tokio::time::timeout(Duration::from_secs(15),incoming.recv())=>r.map_err(|_|invalid())?.ok_or_else(invalid)? };
        original=Some(crate::operations::request_content::request_snapshot(&start));
        let s=start_params(&start)?;
        if !access.allows(&s.model){return Err(Error::new("MODEL_NOT_ALLOWED","此密钥未授权该模型。"));}
        let lease=tokio::select! { _=cancel.cancelled()=>return Err(Error::new("STREAM_CANCELLED","会话已取消。")), r=state.operations.acquire_inference(&access,crate::operations::RoutingRequest{id:context.id,model:s.model.clone(),streaming:true,endpoint:context.endpoint.clone(),client_transport:Some("ws".into()),reasoning_effort:None,service_tier:None,user_agent:context.user_agent.clone(),subagent_kind:context.subagent_kind.clone(),features:s.features.clone(),class:crate::operations::RequestClass::NativeSession,soft_affinity:None,parent_affinity:None})=>r? };
        let account=lease.account_id.ok_or_else(invalid)?;
        let provider=lease.provider.clone().ok_or_else(invalid)?;
        if let Err(e)=state.operations.check_inference_route(account,&access,&provider,&s.model){return lease.complete(Err(e));}
        let _registration = state.profiles.register_inference(account,cancel.clone())?;
        let job=crate::inference::RequestJob{account,access,provider,model:s.model,prompt:String::new(),client_payload:None};
        let result=state.profiles.direct_session(&job,&state.operations,start,&mut incoming,tx.clone(),cancel.clone()).await;
        let error=result.error();
        lease.finish_execution(&result,None)?;
        match error {Some(e)=>Err(e),None=>Ok(())}
    }.await;
    if result.is_ok() {
        if let Some(capture) = &mut *debug.lock().unwrap() {
            capture.complete();
        }
    }
    if let Err(e) = result {
        let response = json!({"method":"bridge/error","params":{"code":e.code,"message":e.message,"requestId":context.id}});
        let _ = state.operations.record_failure_content(
            context.id,
            original.as_ref(),
            &response,
            "websocket",
            None,
        );
        let _ = emit(&tx, response).await;
    }
    drop(tx);
    reader.abort();
    let _ = reader.await;
    let _ = writer.await;
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &Path,
    start: Value,
    incoming: &mut mpsc::Receiver<Value>,
    outgoing: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<StreamSummary> {
    let request = start_params(&start)?;
    let dynamic = crate::protocol_tools::native(&request.tools);
    let params = thread_params(provider, cwd, dynamic, request.instructions.as_deref())?;
    let mut events = rpc.subscribe();
    rpc.enable_client_tools();
    let started = rpc.request("thread/start", params).await?;
    verify_policy(&started, provider)?;
    let thread = started["thread"]["id"]
        .as_str()
        .ok_or_else(invalid)?
        .to_owned();
    emit(&outgoing,json!({"id":start["id"],"result":{"thread":{"id":thread},"model":started["model"],"modelProvider":started["modelProvider"],"transport":"native-direct"}})).await?;
    let mut active: Option<String> = None;
    let mut last_turn = String::new();
    let mut pending = HashSet::new();
    let mut usage = None;
    let mut response_model = None;
    let mut failed = false;
    let mut native_error = None;
    let mut interrupted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    let result:Result<()>=async {
        loop {
            tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err(Error::new("STREAM_CANCELLED","原生会话已取消。")),
                _=outgoing.closed()=>return Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                _=tokio::time::sleep_until(deadline)=>return Err(Error::new("NATIVE_SESSION_TIMEOUT","原生会话达到 30 分钟上限。")),
                event=events.recv()=>{
                    let event=event.map_err(|_|Error::new("STREAM_EVENTS_LOST","原生事件中断。"))?;
                    let method=event["method"].as_str().unwrap_or("");
                    if method=="bridge/callbackRejected" {return Err(Error::new("NATIVE_CALLBACK_DENIED","原生后端请求了未开放的服务器能力。"));}
                    if event["params"]["threadId"]!=thread {continue;}
                    if method=="item/tool/call" {
                        let p=&event["params"];
                        if !crate::protocol_tools::allows(&request.tools,p["namespace"].as_str(),p["tool"].as_str().unwrap_or("")) || p["turnId"].as_str()!=active.as_deref() || pending.len()>=128 {return Err(Error::new("NATIVE_TOOL_SCOPE","原生工具回调不属于当前会话。"));}
                        pending.insert(event["id"].to_string());
                    }
                    if method=="model/rerouted" && event["params"]["turnId"].as_str()==active.as_deref() {
                        if let Some(model)=crate::streaming::reported_model(&event["params"]["toModel"]) {response_model=Some(model);}
                    }
                    if method=="thread/tokenUsage/updated" {usage=Some(crate::streaming::Usage::parse(&event["params"]["tokenUsage"]["total"])?);}
                    if method=="turn/completed" {
                        if event["params"]["turn"]["id"].as_str()!=active.as_deref(){return Err(invalid());}
                        failed |= event["params"]["turn"]["status"]=="failed";
                        if event["params"]["turn"]["status"]=="failed"{native_error=Some(crate::execution::native_failure(&event["params"]["turn"]["error"]).code);}
                        interrupted |= event["params"]["turn"]["status"]=="interrupted";
                        active=None;pending.clear();rpc.clear_tool_callbacks();
                    }
                    emit(&outgoing,event).await?;
                    if provider.is_chatgpt_account() && native_error.as_deref() == Some("UPSTREAM_RATE_LIMITED") {
                        return Err(Error::new("UPSTREAM_RATE_LIMITED", "原生账户已限流，请等待冷却后重建会话。"));
                    }
                },
                message=incoming.recv()=>{
                    let message=message.ok_or_else(||Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。"))?;
                    if message.get("method").is_none() {
                        if !pending.remove(&message["id"].to_string()){return Err(Error::new("NATIVE_TOOL_ID","工具回复标识不属于当前待处理回调。"));}
                        let result=&message["result"];
                        if !result["success"].is_boolean() || !result["contentItems"].as_array().is_some_and(|a|a.iter().all(|v|v["type"]=="inputText" && v["text"].is_string())) {return Err(invalid());}
                        rpc.answer_tool(message["id"].clone(),result.clone()).await?;
                        continue;
                    }
                    let id=&message["id"];
                    if !(id.is_string()||id.is_number()){return Err(invalid());}
                    match message["method"].as_str().unwrap_or("") {
                        "session/close"=>{emit(&outgoing,json!({"id":id,"result":{}})).await?;return Ok(());},
                        "turn/start"=>{
                            if active.is_some(){return Err(Error::new("NATIVE_TURN_ACTIVE","请先完成当前轮次。"));}
                            let p=&message["params"];
                            if p.as_object().is_none_or(|m|m.keys().any(|k|k!="input")) {return Err(invalid());}
                            let input=p["input"].as_array().filter(|a|!a.is_empty()&&a.len()<=1024).ok_or_else(invalid)?;
                            for item in input {
                                let allowed=if item["type"]=="text" && item["text"].is_string(){&["type","text","text_elements"][..]}
                                else if item["type"]=="image" && request.features.contains(&crate::operations::Feature::ImageInput){crate::media::inline_image(item["url"].as_str().ok_or_else(invalid)?)?;&["type","url","detail"][..]}
                                else{return Err(invalid());};
                                if item.as_object().is_none_or(|m|m.keys().any(|k|!allowed.contains(&k.as_str()))){return Err(invalid());}
                            }
                            crate::media::budget(&p["input"],1024*1024)?;
                            let turn=rpc.request("turn/start",json!({"threadId":thread,"input":p["input"]})).await?;
                            let turn_id=turn["turn"]["id"].as_str().ok_or_else(invalid)?.to_owned();
                            last_turn=turn_id.clone();active=Some(turn_id);
                            emit(&outgoing,json!({"id":id,"result":turn})).await?;
                        },
                        "turn/interrupt"=>{
                            let turn=active.as_ref().ok_or_else(invalid)?;
                            let r=rpc.request("turn/interrupt",json!({"threadId":thread,"turnId":turn})).await?;
                            emit(&outgoing,json!({"id":id,"result":r})).await?;
                        },
                        _=>return Err(invalid()),
                    }
                }
            }
        }
    }.await;
    if let Some(turn) = active {
        interrupted = true;
        let _ = rpc
            .request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
            .await;
    }
    let error_code = result
        .as_ref()
        .err()
        .map(|e| e.code.clone())
        .or(native_error);
    if let Some(code) = &error_code {
        if crate::execution::cancelled(code) {
            interrupted = true;
        } else {
            failed = true;
        }
    }
    Ok(StreamSummary {
        response_model,
        response_service_tier: None,
        thread_id: thread,
        turn_id: last_turn,
        item_ids: vec![],
        outcome: if failed {
            "failed"
        } else if interrupted {
            "cancelled"
        } else {
            "completed"
        }
        .into(),
        native_status: None,
        error_code,
        error_message: None,
        delta_count: 0,
        text_bytes: 0,
        first_delta_at: None,
        usage,
        interrupt_acknowledged: false,
        cleanup_confirmed: true,
        guard: crate::guard::Stats {
            state: "not-used-native-direct".into(),
            ..Default::default()
        },
    })
}

pub(crate) fn thread_params(
    provider: &Provider,
    cwd: &Path,
    dynamic: Vec<Value>,
    instructions: Option<&str>,
) -> Result<Value> {
    // Only execution policy is overridden. Keep native base URL, proxy, TLS,
    // compression, retries and HTTP/WebSocket selection unchanged.
    let mut config = crate::provider::text_policy(&provider.mcp_servers)?;
    for key in [
        "features.enable_request_compression",
        "features.unbounded_connection_retries",
    ] {
        config.remove(key);
    }
    // Current native builds otherwise hide dynamic functions behind functions.exec.
    // Direct calls preserve the callback boundary needed by stateless Responses.
    let mut direct_namespaces = vec!["functions".to_owned()];
    for tool in &dynamic {
        if tool["type"] == "namespace" {
            if let Some(name) = tool["name"].as_str() {
                direct_namespaces.push(name.to_owned());
            }
        }
    }
    direct_namespaces.sort();
    direct_namespaces.dedup();
    config.insert("features.code_mode".into(), json!({"enabled":false,"direct_only_tool_namespaces":direct_namespaces,"excluded_tool_namespaces":["collaboration","clock"]}));
    let mut params = json!({"model":provider.model,"modelProvider":provider.id,"allowProviderModelFallback":false,"cwd":cwd,"ephemeral":true,"approvalPolicy":"untrusted","sandbox":"read-only","dynamicTools":dynamic,"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],"experimentalRawEvents":true,"config":config});
    if let Some(instructions) = instructions {
        params["developerInstructions"] = instructions.into();
    }
    Ok(params)
}

pub(crate) fn verify_policy(started: &Value, provider: &Provider) -> Result<()> {
    if started["model"] != provider.model
        || started["modelProvider"] != provider.id
        || started["sandbox"]["type"] != "readOnly"
        || started["approvalPolicy"] != "untrusted"
    {
        return Err(Error::new("NATIVE_POLICY", "原生后端未确认会话策略。"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
