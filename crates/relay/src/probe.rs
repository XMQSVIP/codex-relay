use crate::{
    backend::{Backend, unused_port},
    error::{Error, Result},
    guard::{Guard, Stats},
    installation::Installation,
    provider::{self, Provider},
    rpc::Rpc,
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub state: String,
    pub detail: String,
}
impl Check {
    pub fn new(id: &str, passed: bool, detail: &str) -> Self {
        Self {
            id: id.into(),
            state: if passed { "passed" } else { "blocked" }.into(),
            detail: detail.into(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub purpose: String,
    pub checked_at: u64,
    pub checks: Vec<Check>,
    pub results: Vec<Value>,
    pub gate: Value,
    pub inference_enabled: bool,
}
impl Report {
    pub fn new(purpose: &str) -> Self {
        Self {
            purpose: purpose.into(),
            checked_at: crate::now_ms(),
            checks: Vec::new(),
            results: Vec::new(),
            gate: json!({"ready":false,"missing":["same_desktop_instance","live_text_before_completion","exact_turn_correlation","confirmed_interrupt","tools_disabled_before_dispatch"]}),
            inference_enabled: false,
        }
    }
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.state != "blocked")
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub thread_id: String,
    pub turn_id: String,
    pub item_ids: Vec<String>,
    pub status: String,
    pub delta_count: u32,
    pub text_characters: usize,
    pub first_delta_at: Option<u64>,
    pub terminal_at: u64,
    pub exact_correlation: bool,
    pub tool_items: u32,
    pub interrupt_acknowledged: bool,
    pub guard: Option<Stats>,
}

pub async fn isolated_home(prefix: &str) -> Result<(PathBuf, PathBuf)> {
    let home = std::env::current_dir()?
        .join(".cache/probes")
        .join(format!("rust-{prefix}-{}", uuid::Uuid::new_v4()));
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await?;
    Ok((home, cwd))
}
pub async fn inspect_provider(installation: &Installation) -> Result<Value> {
    let cwd = std::env::current_dir()?.join(".cache/service-workspace");
    tokio::fs::create_dir_all(&cwd).await?;
    let backend = Backend::start(installation, &cwd, None, None, false).await?;
    let result = provider::read(&backend.rpc, &cwd).await;
    backend.stop().await?;
    Ok(
        json!({"purpose":"effective-lan-provider-inspection","provider":result?,"inferenceDispatched":false,"checkedAt":crate::now_ms()}),
    )
}
pub async fn shared(installation: &Installation) -> Result<Report> {
    let (home, cwd) = isolated_home("shared").await?;
    let backend = Backend::start(
        installation,
        &cwd,
        Some(&home),
        Some(unused_port().await?),
        true,
    )
    .await?;
    let mut report = Report::new("rust-isolated-shared-protocol");
    let result=async{
        let account=backend.rpc.request("account/read",json!({"refreshToken":false})).await?;
        report.checks.push(Check::new("isolated_no_account",account["account"].is_null(),"隔离目录没有登录账户。"));
        let wrong=Rpc::connect(backend.endpoint.as_ref().unwrap(),&"x".repeat(64)).await;
        let rejected=wrong.is_err();if let Ok(rpc)=wrong{rpc.close();}
        report.checks.push(Check::new("reject_wrong_token",rejected,"错误令牌在初始化之前被拒绝。"));
        let observer=Rpc::connect(backend.endpoint.as_ref().unwrap(),backend.token.as_ref().unwrap()).await?;
        let initialized = observer.initialize(installation.client_info()?).await?;
        report.results.push(json!({"clientInfo":installation.client_info()?,"userAgent":backend.user_agent,"observerUserAgent":initialized["userAgent"]}));
        let started=backend.rpc.request("thread/start",json!({"cwd":cwd,"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","dynamicTools":[],"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[]})).await?;
        let loaded=observer.request("thread/loaded/list",json!({"limit":100})).await?;observer.close();
        report.checks.push(Check::new("two_clients_share_thread",loaded["data"].as_array().is_some_and(|a|a.contains(&started["thread"]["id"])),"两个认证连接可以看到同一内存会话。"));
        report.checks.push(Check::new("thread_permission_acknowledgment",started["approvalPolicy"]=="never"&&started["sandbox"]["type"]=="readOnly","原生后端确认候选权限；不等于所有工具已禁用。"));
        Ok::<(),Error>(())
    }.await;
    if let Err(error) = result {
        report
            .checks
            .push(Check::new(&error.code, false, &error.message));
    }
    backend.stop().await?;
    Ok(report)
}

pub async fn guarded_turn(
    rpc: &Rpc,
    guard: &Guard,
    provider: &Provider,
    cwd: &Path,
    prompt: &str,
    interrupt: bool,
) -> Result<TurnSummary> {
    let route = guard.route(&provider.model)?;
    let config = provider::overrides(provider, &route.base_url)?;
    let result = run_turn(
        rpc,
        provider,
        cwd,
        prompt,
        interrupt,
        Some(config.into()),
        false,
    )
    .await;
    if result.is_err() {
        guard.release(&route);
        return result;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while route.snapshot().state == "active" && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut result = result?;
    result.guard = Some(route.snapshot());
    guard.release(&route);
    Ok(result)
}
async fn run_turn(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &Path,
    prompt: &str,
    interrupt: bool,
    config: Option<Value>,
    allow_test_tools: bool,
) -> Result<TurnSummary> {
    let mut params = json!({"modelProvider":provider.id,"model":provider.model,"allowProviderModelFallback":false,"cwd":cwd,"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","dynamicTools":[],"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[]});
    if let Some(config) = config {
        params["config"] = config;
    }
    let started = rpc.request("thread/start", params).await?;
    if started["model"] != provider.model
        || started["modelProvider"] != provider.id
        || started["approvalPolicy"] != "never"
        || started["sandbox"]["type"] != "readOnly"
    {
        return Err(Error::new(
            "CHECK_THREAD_POLICY",
            "后端没有确认指定模型、提供方和权限。",
        ));
    }
    let thread_id = started["thread"]["id"]
        .as_str()
        .ok_or_else(|| Error::new("CHECK_PROTOCOL", "原生会话标识缺失。"))?
        .to_owned();
    let mut events = rpc.subscribe();
    let turn=rpc.request("turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":prompt,"text_elements":[]}]})).await?;
    let turn_id = turn["turn"]["id"]
        .as_str()
        .ok_or_else(|| Error::new("CHECK_PROTOCOL", "原生轮次标识缺失。"))?
        .to_owned();
    let mut summary = TurnSummary {
        thread_id: thread_id.clone(),
        turn_id: turn_id.clone(),
        item_ids: Vec::new(),
        status: "unknown".into(),
        delta_count: 0,
        text_characters: 0,
        first_delta_at: None,
        terminal_at: 0,
        exact_correlation: true,
        tool_items: 0,
        interrupt_acknowledged: false,
        guard: None,
    };
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let event = events
                .recv()
                .await
                .map_err(|_| Error::new("CHECK_EVENTS_LOST", "原生事件流中断或溢出。"))?;
            let p = &event["params"];
            if p["threadId"] != thread_id {
                continue;
            }
            let method = event["method"].as_str().unwrap_or("");
            if ["turn/started", "turn/completed"].contains(&method) {
                if p["turn"]["id"] != turn_id {
                    return Err(Error::new("CHECK_CORRELATION", "原生轮次标识不匹配。"));
                }
                if method == "turn/completed" {
                    summary.status = p["turn"]["status"].as_str().unwrap_or("unknown").into();
                    summary.terminal_at = crate::now_ms();
                    return Ok(());
                }
            } else if ["item/started", "item/completed", "item/agentMessage/delta"]
                .contains(&method)
            {
                if p["turnId"] != turn_id {
                    return Err(Error::new("CHECK_CORRELATION", "原生事件轮次不匹配。"));
                }
                if method == "item/agentMessage/delta" {
                    let item_id = p["itemId"].as_str().unwrap_or("");
                    if !summary.item_ids.iter().any(|id| id == item_id) {
                        return Err(Error::new("CHECK_CORRELATION", "文本没有对应的助手消息。"));
                    }
                    let delta = p["delta"]
                        .as_str()
                        .ok_or_else(|| Error::new("CHECK_PROTOCOL", "文本增量格式无效。"))?;
                    if !delta.is_empty() {
                        summary.delta_count += 1;
                        summary.text_characters += delta.chars().count();
                        summary.first_delta_at.get_or_insert(crate::now_ms());
                        if interrupt && !summary.interrupt_acknowledged {
                            rpc.request(
                                "turn/interrupt",
                                json!({"threadId":thread_id,"turnId":turn_id}),
                            )
                            .await?;
                            summary.interrupt_acknowledged = true;
                        }
                    }
                } else {
                    let kind = p["item"]["type"].as_str().unwrap_or("");
                    if kind == "agentMessage" {
                        let id = p["item"]["id"]
                            .as_str()
                            .ok_or_else(|| Error::new("CHECK_PROTOCOL", "助手消息 ID 缺失。"))?
                            .to_owned();
                        if !summary.item_ids.contains(&id) {
                            summary.item_ids.push(id);
                        }
                    } else if !["userMessage", "reasoning"].contains(&kind) {
                        summary.tool_items += 1;
                        if !allow_test_tools {
                            return Err(Error::new("CHECK_TOOL_ITEM", "受保护会话出现工具事件。"));
                        }
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(Error::new("CHECK_TIMEOUT", "未收到确认的原生终态。")));
    if let Err(error) = result {
        let _ = rpc
            .request(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
            )
            .await;
        return Err(error);
    }
    summary.exact_correlation = !summary.item_ids.is_empty();
    Ok(summary)
}

#[derive(Default)]
pub(crate) struct Fixture {
    pub(crate) scenario: Mutex<String>,
    pub(crate) seen: Mutex<Vec<Value>>,
    cancelled: AtomicBool,
    pub(crate) exec_seen: AtomicBool,
    pub(crate) resource_seen: AtomicBool,
}
pub(crate) async fn fixture(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let scenario = state.scenario.lock().unwrap().clone();
    let authenticated = headers
        .get("authorization")
        .is_some_and(|v| v == "Bearer SYNTHETIC_BEARER")
        && headers
            .get("x-openai-actor-authorization")
            .is_some_and(|v| v == "SYNTHETIC_ACTOR");
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let turn_metadata = header("x-codex-turn-metadata")
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null);
    let native_links = json!({
        "sessionId":header("session-id"),"threadId":header("thread-id"),
        "requestId":header("x-client-request-id"),"windowId":header("x-codex-window-id"),
        "metadataSessionId":turn_metadata["session_id"],"metadataThreadId":turn_metadata["thread_id"],
        "metadataTurnId":turn_metadata["turn_id"],"metadataWindowId":turn_metadata["window_id"],
        "hasInstallationId":turn_metadata["installation_id"].as_str().is_some_and(|s|!s.is_empty()),
        "promptCacheKey":body["prompt_cache_key"],
        "headerNames":crate::native_metadata::REQUEST_HEADERS.iter().filter(|name|headers.contains_key(**name)).collect::<Vec<_>>()
    });
    let previous = state
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["scenario"] == scenario)
        .count();
    let input = body["input"].as_array().cloned().unwrap_or_default();
    for item in &input {
        if item["call_id"] == "code" && item["output"].to_string().contains("RUST_GUARD_CANARY") {
            state.exec_seen.store(true, Ordering::SeqCst);
        }
        if item["call_id"] == "resource" && item["output"].to_string().contains("resources") {
            state.resource_seen.store(true, Ordering::SeqCst);
        }
    }
    state.seen.lock().unwrap().push(json!({"model":body["model"],"scenario":scenario,"authenticated":authenticated,"nativeLinks":native_links,"userAgent":headers.get("user-agent").and_then(|v|v.to_str().ok()),"originator":headers.get("originator").and_then(|v|v.to_str().ok()),"toolsStripped":body["tools"]==json!([])&&body["tool_choice"]=="none"&&!input.iter().any(|i|i["type"]=="additional_tools"),"hasTools":input.iter().any(|i|i["type"]=="additional_tools")||body["tools"].as_array().is_some_and(|a|!a.is_empty())}));
    if scenario == "control" && previous > 0 {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{"message":"Synthetic control finished."}})),
        )
            .into_response();
    }
    let sse = |v: Value| format!("data: {v}\n\n");
    let tool = json!({"type":"custom_tool_call","id":"fc_code","call_id":"code","name":"exec","input":"text('RUST_GUARD_CANARY');"});
    let body = if ["tools", "control", "client_tools"].contains(&scenario.as_str()) {
        let resource = json!({"type":"function_call","id":"fc_resource","call_id":"resource","name":"list_mcp_resources","arguments":"{}"});
        Body::from(format!(
            "{}{}{}",
            sse(json!({"type":"response.output_item.done","item":tool})),
            sse(json!({"type":"response.output_item.done","item":resource})),
            sse(
                json!({"type":"response.completed","response":{"id":"r","object":"response","status":"completed","model":body["model"],"output":[tool,resource]}})
            )
        ))
    } else if scenario == "unknown" {
        Body::from(sse(json!({"type":"response.unapproved_event","item":tool})))
    } else {
        let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(8);
        let state = state.clone();
        tokio::spawn(async move {
            let item = json!({"id":"m","type":"message","role":"assistant","phase":"final_answer","content":[]});
            for event in [
                json!({"type":"response.created","response":{"id":"r","output":[],"status":"in_progress"}}),
                json!({"type":"response.output_item.added","output_index":0,"item":item}),
                json!({"type":"response.output_text.delta","item_id":"m","output_index":0,"content_index":0,"delta":"Rust 原生验证🌍"}),
            ] {
                if tx.send(Ok(Bytes::from(sse(event)))).await.is_err() {
                    return;
                }
            }
            if scenario == "truncated" {
                return;
            }
            tokio::select! {_=tx.closed()=>{if scenario=="interrupt"{state.cancelled.store(true,Ordering::SeqCst);}return;},_=tokio::time::sleep(Duration::from_millis(if scenario=="interrupt"{10000}else{350}))=>{}}
            let complete = json!({"id":"m","type":"message","role":"assistant","phase":"final_answer","status":"completed","content":[{"type":"output_text","text":"Rust 原生验证🌍","annotations":[]}]});
            for event in [
                json!({"type":"response.output_item.done","output_index":0,"item":complete}),
                json!({"type":"response.completed","response":{"id":"r","status":"completed","output":[complete],"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,"input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":1}}}}),
            ] {
                if tx.send(Ok(Bytes::from(sse(event)))).await.is_err() {
                    return;
                }
            }
        });
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

/// Real native backend, synthetic loopback model. Never reads personal credentials.
pub async fn stream(installation: &Installation) -> Result<Report> {
    use crate::streaming::{StreamRequest, encode_sse, guarded_stream};
    let (home, cwd) = isolated_home("stream").await?;
    let state = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream = format!("http://127.0.0.1:{}/v1", listener.local_addr()?.port());
    let profile = Provider {
        execution: Default::default(),
        id: "native_probe".into(),
        model: "gpt-6.1-sol".into(),
        base_url: upstream.clone(),
        auth: "native-bearer".into(),
        forward_headers: vec![
            "authorization".into(),
            "x-openai-actor-authorization".into(),
        ],
        required_headers: vec![
            "authorization".into(),
            "x-openai-actor-authorization".into(),
        ],
        mcp_servers: vec![],
    };
    tokio::fs::write(home.join("config.toml"), format!("model=\"{}\"\nmodel_provider=\"native_probe\"\n[model_providers.native_probe]\nname=\"Synthetic probe\"\nbase_url=\"{upstream}\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_BEARER\"\nhttp_headers={{\"X-OpenAI-Actor-Authorization\"=\"SYNTHETIC_ACTOR\"}}\n", profile.model)).await?;
    let pid = Arc::new(AtomicU32::new(0));
    let guard = Guard::fixture(
        upstream,
        profile.forward_headers.clone(),
        pid.clone(),
        installation.app_server_executable.clone(),
    )
    .await?;
    let backend = Backend::start(installation, &cwd, Some(&home), None, true).await?;
    pid.store(backend.pid, Ordering::SeqCst);
    let fixture_state = state.clone();
    let cancel_server = CancellationToken::new();
    let server_token = cancel_server.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(fixture))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
                .with_state(fixture_state),
        )
        .with_graceful_shutdown(server_token.cancelled_owned())
        .await;
    });
    let mut report = Report::new("rust-native-responses-text-stream");
    report.gate = json!({"ready":false,"syntheticUpstream":true,"missing":["real_account_inference","external_api_auth_and_routing","chat_completions_mapping"]});
    let result = async {
        for case in ["text", "cancel", "disconnect", "timeout", "truncated", "unknown", "tools"] {
            *state.scenario.lock().unwrap() = if ["cancel", "disconnect", "timeout"].contains(&case) { "interrupt" } else { case }.into();
            state.cancelled.store(false, Ordering::SeqCst);
            state.seen.lock().unwrap().clear();
            let cancel = CancellationToken::new();
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let producer = guarded_stream(&backend.rpc, &guard, StreamRequest {
                provider: &profile, cwd: &cwd, prompt: "Local synthetic streaming validation.",
                    timeout: Duration::from_secs(if case == "timeout" { 5 } else { 90 }),
            }, tx, cancel.clone());
            let consumer = async {
                let mut output = vec![];
                while let Some(event) = rx.recv().await {
                    let is_delta = event["type"] == "response.output_text.delta";
                    output.push(event);
                    if is_delta && case == "cancel" { cancel.cancel(); }
                    if is_delta && case == "disconnect" { break; }
                }
                drop(rx);
                output
            };
            let (summary, output) = tokio::join!(producer, consumer);
            let summary = summary?;
            let ordered = output.iter().enumerate().all(|(i,e)| e["sequence_number"] == i && encode_sse(e).is_ok());
            let completed = output.iter().any(|e| e["type"] == "response.completed");
            let native_ids = state.seen.lock().unwrap().iter().all(|s| {
                s["authenticated"] == true && s["toolsStripped"] == true
                    && s["nativeLinks"]["threadId"] == summary.thread_id
                    && s["nativeLinks"]["metadataTurnId"] == summary.turn_id
            });
            let shared_checks = ordered && native_ids && summary.guard.upstream_requests == 1 && summary.cleanup_confirmed;
            let passed = shared_checks && match case {
                "text" => {
                    let text: String = output.iter().filter(|e| e["type"] == "response.output_text.delta").filter_map(|e| e["delta"].as_str()).collect();
                    completed && summary.outcome == "completed" && text == "Rust 原生验证🌍"
                        && summary.usage.as_ref().is_some_and(|u| u.input_tokens == 10 && u.output_tokens == 4 && u.total_tokens == 14 && u.cached_input_tokens == Some(2) && u.reasoning_output_tokens == Some(1))
                        && summary.first_delta_at.zip(summary.guard.upstream_ended_at).is_some_and(|(a,b)| a < b)
                        && output.last().is_some_and(|e| e["response"]["id"] == summary.turn_id && e["response"]["output"][0]["id"] == summary.item_ids[0])
                }
                "cancel" | "disconnect" | "timeout" => {
                    for _ in 0..80 {
                        if state.cancelled.load(Ordering::SeqCst) { break; }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    let code = match case { "cancel" => "STREAM_CANCELLED", "disconnect" => "STREAM_CONSUMER_CLOSED", _ => "STREAM_TIMEOUT" };
                    !completed && summary.error_code.as_deref() == Some(code)
                        && summary.interrupt_acknowledged && summary.native_status.as_deref() == Some("interrupted")
                        && summary.guard.state == "cancelled" && state.cancelled.load(Ordering::SeqCst)
                }
                "truncated" => !completed && summary.outcome == "failed" && summary.guard.failure_code.as_deref() == Some("GUARD_TRUNCATED"),
                "unknown" | "tools" => !completed && summary.outcome == "failed" && summary.guard.failure_code.as_deref() == Some("GUARD_EVENT_BLOCKED"),
                _ => false,
            };
            report.checks.push(Check::new(&format!("stream_{case}"), passed, match case {
                "text" => "原生文本实时映射为 SSE，保留线程、轮次、消息 ID，并核对合成用量。",
                "cancel" => "显式取消后中断原生轮次并关闭上游。",
                "disconnect" => "下游断开后中断原生轮次并关闭上游。",
                "timeout" => "执行超时后中断原生轮次并关闭上游。",
                "truncated" => "截断响应以失败结束，不生成成功终态。",
                "unknown" => "未知上游事件被拒绝，不生成成功终态。",
                _ => "工具响应在原生执行前阻断，不生成成功终态。",
            }));
            report.results.push(json!({"case":case,"summary":summary,"eventTypes":output.iter().map(|e| &e["type"]).collect::<Vec<_>>(),"syntheticUsage":true}));
            if !summary.cleanup_confirmed { break; }
        }
        Ok::<(), Error>(())
    }.await;
    if let Err(error) = result {
        report
            .checks
            .push(Check::new(&error.code, false, &error.message));
    }
    let stopped = backend.stop().await;
    guard.close().await;
    cancel_server.cancel();
    server.abort();
    stopped?;
    Ok(report)
}

pub async fn guard(installation: &Installation) -> Result<Report> {
    let (home, cwd) = isolated_home("guard").await?;
    let state = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream = format!("http://127.0.0.1:{}/v1", listener.local_addr()?.port());
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let fixture_state = state.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(fixture))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
                .with_state(fixture_state),
        )
        .with_graceful_shutdown(token.cancelled_owned())
        .await;
    });
    let profile = Provider {
        execution: Default::default(),
        id: "native_probe".into(),
        model: "gpt-6.1-sol".into(),
        base_url: upstream.clone(),
        auth: "native-bearer".into(),
        forward_headers: vec![
            "authorization".into(),
            "x-openai-actor-authorization".into(),
        ],
        required_headers: vec![
            "authorization".into(),
            "x-openai-actor-authorization".into(),
        ],
        mcp_servers: vec![],
    };
    tokio::fs::write(home.join("config.toml"),format!("model=\"{}\"\nmodel_provider=\"native_probe\"\n[model_providers.native_probe]\nname=\"Synthetic probe\"\nbase_url=\"{upstream}\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_BEARER\"\nhttp_headers={{\"X-OpenAI-Actor-Authorization\"=\"SYNTHETIC_ACTOR\"}}\n",profile.model)).await?;
    let pid = Arc::new(AtomicU32::new(0));
    let guard = Guard::fixture(
        upstream,
        profile.forward_headers.clone(),
        pid.clone(),
        installation.app_server_executable.clone(),
    )
    .await?;
    let backend = Backend::start(installation, &cwd, Some(&home), None, true).await?;
    pid.store(backend.pid, Ordering::SeqCst);
    let mut report = Report::new("rust-isolated-native-guard-and-provider");
    let result = async {
        *state.scenario.lock().unwrap() = "control".into();
        let _ = run_turn(
            &backend.rpc,
            &profile,
            &cwd,
            "Synthetic control only.",
            false,
            Some(provider::text_policy(&[])?.into()),
            true,
        )
        .await?;
        report.checks.push(Check::new(
            "unguarded_control_reproduces_tools",
            state.exec_seen.load(Ordering::SeqCst) && state.resource_seen.load(Ordering::SeqCst),
            "无过滤对照复现代码与资源查询处理。",
        ));
        let mut native_links_preserved = true;
        for scenario in ["tools", "text", "interrupt", "unknown", "truncated"] {
            *state.scenario.lock().unwrap() = scenario.into();
            let turn = guarded_turn(
                &backend.rpc,
                &guard,
                &profile,
                &cwd,
                "Local synthetic Rust protocol test.",
                scenario == "interrupt",
            )
            .await?;
            let stats = turn.guard.as_ref().unwrap();
            native_links_preserved &= state.seen.lock().unwrap().iter().any(|seen| {
                let links = &seen["nativeLinks"];
                seen["scenario"] == scenario
                    && ["sessionId", "threadId", "metadataSessionId", "metadataThreadId", "promptCacheKey"].iter().all(|key| links[*key] == turn.thread_id)
                    && links["metadataTurnId"] == turn.turn_id
                    && links["requestId"].as_str().is_some_and(|s|!s.is_empty())
                    && links["windowId"].as_str().is_some_and(|s|!s.is_empty())
                    && links["windowId"] == links["metadataWindowId"]
                    && links["hasInstallationId"] == true
                    && links["headerNames"].as_array().is_some_and(|names| names.len() == crate::native_metadata::REQUEST_HEADERS.len())
            });
            let passed = match scenario {
                "tools" => {
                    turn.status == "failed"
                        && turn.tool_items == 0
                        && stats.forwarded_events == 0
                        && stats.failure_code.as_deref() == Some("GUARD_EVENT_BLOCKED")
                }
                "text" => {
                    turn.status == "completed"
                        && turn.exact_correlation
                        && turn
                            .first_delta_at
                            .zip(stats.upstream_ended_at)
                            .is_some_and(|(a, b)| a < b)
                }
                "interrupt" => {
                    for _ in 0..80 {
                        if state.cancelled.load(Ordering::SeqCst) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    turn.status == "interrupted"
                        && turn.interrupt_acknowledged
                        && stats.state == "cancelled"
                        && state.cancelled.load(Ordering::SeqCst)
                }
                "unknown" => {
                    turn.status == "failed"
                        && stats.failure_code.as_deref() == Some("GUARD_EVENT_BLOCKED")
                }
                "truncated" => {
                    turn.status == "failed"
                        && stats.failure_code.as_deref() == Some("GUARD_TRUNCATED")
                }
                _ => false,
            };
            report.checks.push(Check::new(
                &format!("guard_{scenario}"),
                passed,
                "真实原生后端 + 本地合成响应；校验文本、工具阻断或取消终态。",
            ));
            report.results.push(serde_json::to_value(turn)?);
        }
        report.checks.push(Check::new(
            "native_conversation_metadata_preserved",
            native_links_preserved,
            "五条过滤请求保留原生会话、线程、轮次、窗口及缓存关联；与 app-server 返回 ID 核对。",
        ));
        let seen = state.seen.lock().unwrap().clone();
        let clean = ["tools", "text", "interrupt", "unknown", "truncated"]
            .iter()
            .all(|scenario| {
                let matching: Vec<_> = seen.iter().filter(|v| v["scenario"] == *scenario).collect();
                matching.len() == 1
                    && matching
                        .iter()
                        .all(|v| v["toolsStripped"] == true && v["authenticated"] == true)
            });
        report.checks.push(Check::new(
            "native_auth_and_no_tools",
            clean,
            "受保护请求保留原生认证且移除工具声明。",
        ));
        *state.scenario.lock().unwrap() = "ordinary".into();
        let ordinary = run_turn(
            &backend.rpc,
            &profile,
            &cwd,
            "Ordinary synthetic control.",
            false,
            None,
            true,
        )
        .await?;
        let original = state.seen.lock().unwrap().iter().any(|v| {
            v["scenario"] == "ordinary"
                && v["hasTools"] == true
                && v["toolsStripped"] == false
                && v["authenticated"] == true
        });
        report.checks.push(Check::new(
            "ordinary_provider_unchanged",
            ordinary.status == "completed" && original,
            "同进程普通会话保留原路由与工具。",
        ));
        let client_info = installation.client_info()?;
        let seen = state.seen.lock().unwrap().clone();
        let native = seen.iter().find(|v| v["scenario"] == "ordinary");
        let identity_preserved = native.is_some_and(|native| {
            native["originator"] == client_info.name
                && backend.user_agent.as_deref().is_some_and(|ua| native["userAgent"] == ua)
                && ["tools", "text", "interrupt", "unknown", "truncated"].iter().all(|scenario| {
                    seen.iter().filter(|v| v["scenario"] == *scenario).count() == 1
                })
                && seen.iter().all(|v| {
                    v["originator"] == native["originator"]
                        && v["userAgent"] == native["userAgent"]
                })
        });
        report.checks.push(Check::new(
            "native_client_headers_preserved",
            identity_preserved,
            "本地上游收到的原生 originator 和 User-Agent 在直接与过滤路径完全一致。",
        ));
        report.results.push(json!({"clientInfo":client_info,"userAgent":backend.user_agent,"upstreamClientHeaders":seen.iter().map(|v|json!({"scenario":v["scenario"],"originator":v["originator"],"userAgent":v["userAgent"]})).collect::<Vec<_>>()}));
        let route = guard.route(&profile.model)?;
        let config = provider::overrides(&profile, &route.base_url)?;
        guard.close().await;
        *state.scenario.lock().unwrap() = "offline".into();
        let offline = run_turn(
            &backend.rpc,
            &profile,
            &cwd,
            "Offline synthetic check.",
            false,
            Some(config.into()),
            false,
        )
        .await?;
        let no_fallback = !state
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["scenario"] == "offline");
        report.checks.push(Check::new(
            "offline_no_fallback",
            offline.status == "failed" && no_fallback,
            "过滤器离线后不直连原服务。",
        ));
        Ok::<(), Error>(())
    }
    .await;
    if let Err(error) = result {
        report
            .checks
            .push(Check::new(&error.code, false, &error.message));
    }
    let stopped = backend.stop().await;
    guard.close().await;
    cancel.cancel();
    server.abort();
    stopped?;
    Ok(report)
}
