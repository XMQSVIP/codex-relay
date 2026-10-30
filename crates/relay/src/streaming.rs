//! Text-only Responses event adapter. Upstream tool filtering is still required.
use crate::{
    error::{Error, Result},
    guard::{Guard, Route, Stats},
    provider::{self, Provider},
    rpc::Rpc,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

const MAX_TEXT: usize = 1024 * 1024;
const MAX_ITEMS: usize = 128;
fn invalid() -> Error {
    Error::new("STREAM_PROTOCOL", "原生文本事件格式或顺序无效。")
}
fn id(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(invalid)
}
fn phase(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .filter(|s| ["commentary", "partial_answer", "final_answer"].contains(s))
        .map(|s| Some(s.into()))
        .ok_or_else(invalid)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
}
impl Usage {
    pub(crate) fn from_response(value: &Value) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        Self::parse(&json!({
            "inputTokens":value["input_tokens"], "outputTokens":value["output_tokens"], "totalTokens":value["total_tokens"],
            "cachedInputTokens":value["input_tokens_details"]["cached_tokens"],
            "reasoningOutputTokens":value["output_tokens_details"]["reasoning_tokens"]
        })).map(Some)
    }
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let count = |key: &str| {
            value[key]
                .as_u64()
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(invalid)
        };
        let optional = |key: &str| {
            if value.get(key).is_none_or(Value::is_null) {
                Ok(None)
            } else {
                count(key).map(Some)
            }
        };
        let result = Self {
            input_tokens: count("inputTokens")?,
            output_tokens: count("outputTokens")?,
            total_tokens: count("totalTokens")?,
            cached_input_tokens: optional("cachedInputTokens")?,
            reasoning_output_tokens: optional("reasoningOutputTokens")?,
            cache_write_input_tokens: optional("cacheWriteInputTokens")?,
        };
        if result.input_tokens.checked_add(result.output_tokens) != Some(result.total_tokens)
            || result
                .cached_input_tokens
                .is_some_and(|n| n > result.input_tokens)
            || result
                .cache_write_input_tokens
                .is_some_and(|n| n > result.input_tokens)
            || result
                .reasoning_output_tokens
                .is_some_and(|n| n > result.output_tokens)
        {
            return Err(invalid());
        }
        Ok(result)
    }
    pub(crate) fn responses(&self) -> Value {
        let mut value = json!({"input_tokens":self.input_tokens,"output_tokens":self.output_tokens,"total_tokens":self.total_tokens});
        if let Some(count) = self.cached_input_tokens {
            value["input_tokens_details"] = json!({"cached_tokens":count});
        }
        if let Some(count) = self.reasoning_output_tokens {
            value["output_tokens_details"] = json!({"reasoning_tokens":count});
        }
        value
    }
}
struct Message {
    id: String,
    text: String,
    phase: Option<String>,
    done: bool,
}
impl Message {
    fn output(&self) -> Value {
        let mut value = json!({"id":self.id,"type":"message","role":"assistant","status":if self.done {"completed"} else {"in_progress"},"content":[part(&self.text)]});
        if let Some(phase) = &self.phase {
            value["phase"] = phase.clone().into();
        }
        value
    }
}
fn part(text: &str) -> Value {
    json!({"type":"output_text","text":text,"annotations":[],"logprobs":[]})
}

pub struct TextState {
    pub thread_id: String,
    pub turn_id: String,
    model: String,
    created_at: u64,
    messages: Vec<Message>,
    item_kinds: HashMap<String, String>,
    pub usage: Option<Usage>,
    pub status: Option<String>,
    pub delta_count: u32,
    pub first_delta_at: Option<u64>,
    bytes: usize,
    sequence: u64,
    poisoned: bool,
}
impl TextState {
    pub fn new(thread_id: &str, turn_id: &str, model: &str, created_at: u64) -> Result<Self> {
        id(&json!({"id":thread_id}), "id")?;
        id(&json!({"id":turn_id}), "id")?;
        if model.is_empty() || model.len() > 256 {
            return Err(invalid());
        }
        Ok(Self {
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
            model: model.into(),
            created_at,
            messages: vec![],
            item_kinds: HashMap::new(),
            usage: None,
            status: None,
            delta_count: 0,
            first_delta_at: None,
            bytes: 0,
            sequence: 0,
            poisoned: false,
        })
    }
    fn event(&mut self, mut value: Value) -> Value {
        value["sequence_number"] = self.sequence.into();
        self.sequence += 1;
        value
    }
    pub fn created(&mut self) -> Result<Value> {
        if self.sequence != 0 || self.poisoned {
            return Err(invalid());
        }
        let response = self.response("in_progress");
        Ok(self.event(json!({"type":"response.created","response":response})))
    }
    pub fn response(&self, status: &str) -> Value {
        json!({"id":self.turn_id,"object":"response","created_at":self.created_at,"model":self.model,"status":status,"output":self.messages.iter().map(Message::output).collect::<Vec<_>>(),"usage":self.usage.as_ref().map(Usage::responses),"error":Value::Null,"incomplete_details":Value::Null,"tools":[],"tool_choice":"none","parallel_tool_calls":false,"store":false,"metadata":{"native_thread_id":self.thread_id}})
    }
    pub fn failed(&mut self, code: &str) -> Value {
        self.poisoned = true;
        let mut response = self.response("failed");
        response["error"] = json!({"code":"server_error","message":format!("Text stream ended without success ({code}).")});
        self.event(json!({"type":"response.failed","response":response}))
    }
    fn append(&mut self, index: usize, delta: &str) -> Result<Option<Value>> {
        if self.bytes.saturating_add(delta.len()) > MAX_TEXT {
            return Err(Error::new("STREAM_OUTPUT_LIMIT", "文本输出超过本地上限。"));
        }
        if delta.is_empty() {
            return Ok(None);
        }
        self.bytes += delta.len();
        self.messages[index].text.push_str(delta);
        self.delta_count += 1;
        self.first_delta_at.get_or_insert(crate::now_ms());
        Ok(Some(self.event(json!({"type":"response.output_text.delta","item_id":self.messages[index].id,"output_index":index,"content_index":0,"delta":delta,"logprobs":[]}))))
    }
    pub fn accept(&mut self, event: &Value) -> Result<Vec<Value>> {
        if self.poisoned {
            return Err(Error::new("STREAM_CLOSED", "文本适配器已关闭。"));
        }
        let result = self.accept_inner(event);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn accept_inner(&mut self, event: &Value) -> Result<Vec<Value>> {
        let method = event["method"].as_str().ok_or_else(invalid)?;
        if method == "bridge/callbackRejected" {
            return Err(Error::new(
                "STREAM_TOOL_EVENT",
                "原生后端尝试调用未开放的客户端能力。",
            ));
        }
        let p = &event["params"];
        if p["threadId"] != self.thread_id {
            return Ok(vec![]);
        }
        if self.status.is_some() || self.sequence == 0 {
            return Err(invalid());
        }
        if method.starts_with("turn/")
            || method.starts_with("item/")
            || ["thread/tokenUsage/updated", "error", "model/rerouted"].contains(&method)
        {
            let turn = if ["turn/started", "turn/completed"].contains(&method) {
                &p["turn"]["id"]
            } else {
                &p["turnId"]
            };
            if turn != &self.turn_id {
                return Err(Error::new("STREAM_CORRELATION", "原生事件不属于当前轮次。"));
            }
        }
        match method {
            "turn/started" => {
                if p["turn"]["status"] != "inProgress" {
                    return Err(invalid());
                }
                Ok(vec![])
            }
            "item/started" => {
                let item = &p["item"];
                let key = id(item, "id")?;
                let kind = item["type"].as_str().ok_or_else(invalid)?;
                if self.item_kinds.len() >= MAX_ITEMS || self.item_kinds.contains_key(&key) {
                    return Err(invalid());
                }
                if !["agentMessage", "userMessage", "reasoning"].contains(&kind) {
                    return Err(Error::new(
                        "STREAM_TOOL_EVENT",
                        "受保护文本轮次出现工具或未知 item。",
                    ));
                }
                self.item_kinds.insert(key.clone(), kind.into());
                if kind != "agentMessage" {
                    return Ok(vec![]);
                }
                let phase = phase(&item["phase"])?;
                let initial = item["text"].as_str().ok_or_else(invalid)?;
                let index = self.messages.len();
                self.messages.push(Message {
                    id: key.clone(),
                    text: String::new(),
                    phase,
                    done: false,
                });
                let mut message = self.messages[index].output();
                message["content"] = json!([]);
                let mut out=vec![self.event(json!({"type":"response.output_item.added","output_index":index,"item":message})),self.event(json!({"type":"response.content_part.added","item_id":key,"output_index":index,"content_index":0,"part":part("")}))];
                if let Some(event) = self.append(index, initial)? {
                    out.push(event);
                }
                Ok(out)
            }
            "item/agentMessage/delta" => {
                let key = id(p, "itemId")?;
                let index = self
                    .messages
                    .iter()
                    .position(|m| m.id == key && !m.done)
                    .ok_or_else(invalid)?;
                Ok(self
                    .append(index, p["delta"].as_str().ok_or_else(invalid)?)?
                    .into_iter()
                    .collect())
            }
            "item/completed" => {
                let item = &p["item"];
                let key = id(item, "id")?;
                let kind = item["type"].as_str().ok_or_else(invalid)?;
                if self.item_kinds.get(&key).map(String::as_str) != Some(kind) {
                    return Err(invalid());
                }
                if kind != "agentMessage" {
                    return Ok(vec![]);
                }
                let index = self
                    .messages
                    .iter()
                    .position(|m| m.id == key && !m.done)
                    .ok_or_else(invalid)?;
                let final_text = item["text"].as_str().ok_or_else(invalid)?;
                let final_phase = phase(&item["phase"])?;
                if self.messages[index].phase.is_some() && self.messages[index].phase != final_phase
                {
                    return Err(invalid());
                }
                let suffix = final_text
                    .strip_prefix(&self.messages[index].text)
                    .ok_or_else(|| {
                        Error::new("STREAM_TEXT_MISMATCH", "完成消息与已发送的文本不一致。")
                    })?;
                let mut out: Vec<_> = self.append(index, suffix)?.into_iter().collect();
                self.messages[index].done = true;
                self.messages[index].phase = final_phase;
                out.push(self.event(json!({"type":"response.output_text.done","item_id":key,"output_index":index,"content_index":0,"text":final_text,"logprobs":[]})));
                out.push(self.event(json!({"type":"response.content_part.done","item_id":key,"output_index":index,"content_index":0,"part":part(final_text)})));
                let message = self.messages[index].output();
                out.push(self.event(
                    json!({"type":"response.output_item.done","output_index":index,"item":message}),
                ));
                Ok(out)
            }
            "thread/tokenUsage/updated" => {
                self.usage = Some(Usage::parse(&p["tokenUsage"]["last"])?);
                Ok(vec![])
            }
            "turn/completed" => {
                let status = p["turn"]["status"]
                    .as_str()
                    .filter(|s| ["completed", "failed", "interrupted"].contains(s))
                    .ok_or_else(invalid)?;
                self.status = Some(status.into());
                if status == "completed" {
                    if self.messages.is_empty() || self.messages.iter().any(|m| !m.done) {
                        return Err(invalid());
                    }
                    let response = self.response("completed");
                    Ok(vec![self.event(
                        json!({"type":"response.completed","response":response}),
                    )])
                } else {
                    Ok(vec![])
                }
            }
            "error" => Err(Error::new(
                "STREAM_NATIVE_FAILED",
                "原生轮次返回错误，详细内容未对外转发。",
            )),
            "model/rerouted" => Err(Error::new(
                "STREAM_MODEL_CHANGED",
                "原生轮次更改了指定模型。",
            )),
            "thread/closed" => Err(Error::new("STREAM_DISCONNECTED", "原生线程提前关闭。")),
            "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta"
            | "thread/status/changed" => Ok(vec![]),
            _ if method.starts_with("item/") || method.starts_with("turn/") => Err(invalid()),
            _ => Ok(vec![]),
        }
    }
}

pub fn encode_sse(event: &Value) -> Result<String> {
    let kind = event["type"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() < 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._".contains(&b))
        })
        .ok_or_else(invalid)?;
    Ok(format!(
        "event: {kind}\ndata: {}\n\n",
        serde_json::to_string(event)?
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSummary {
    pub response_model: Option<String>,
    /// Actual tier explicitly reported by the upstream, never the requested tier.
    pub response_service_tier: Option<String>,
    pub thread_id: String,
    pub turn_id: String,
    pub item_ids: Vec<String>,
    pub outcome: String,
    pub native_status: Option<String>,
    pub error_code: Option<String>,
    /// Relay-generated diagnostic for delivery; never persist it with usage metadata.
    #[serde(skip)]
    pub error_message: Option<String>,
    pub delta_count: u32,
    pub text_bytes: usize,
    pub first_delta_at: Option<u64>,
    pub usage: Option<Usage>,
    pub interrupt_acknowledged: bool,
    pub cleanup_confirmed: bool,
    pub guard: Stats,
}
/// Keep only bounded model identifiers explicitly reported by the upstream.
pub(crate) fn reported_model(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|model| {
            !model.is_empty()
                && model.len() <= 256
                && model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        })
        .map(str::to_owned)
}

pub struct StreamRequest<'a> {
    pub provider: &'a Provider,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub timeout: Duration,
}

async fn deliver(
    sender: &mpsc::Sender<Value>,
    event: Value,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _=cancel.cancelled()=>Err(Error::new("STREAM_CANCELLED","调用方取消了当前请求。")),
        result=tokio::time::timeout(Duration::from_secs(5),sender.send(event))=>match result {
            Ok(Ok(()))=>Ok(()), Ok(Err(_))=>Err(Error::new("STREAM_CONSUMER_CLOSED","下游已断开。")),
            Err(_)=>Err(Error::new("STREAM_BACKPRESSURE","下游读取过慢，已停止本轮输出。")),
        }
    }
}
async fn interrupt(
    rpc: &Rpc,
    events: &mut broadcast::Receiver<Value>,
    thread: &str,
    turn: &str,
) -> (bool, Option<String>) {
    let acknowledged = rpc
        .request_timeout(
            "turn/interrupt",
            json!({"threadId":thread,"turnId":turn}),
            Duration::from_secs(5),
        )
        .await
        .is_ok();
    let terminal = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(e)
                    if e["method"] == "turn/completed"
                        && e["params"]["threadId"] == thread
                        && e["params"]["turn"]["id"] == turn =>
                {
                    return e["params"]["turn"]["status"]
                        .as_str()
                        .filter(|s| ["completed", "interrupted", "failed"].contains(s))
                        .map(str::to_owned);
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .unwrap_or(None);
    (acknowledged, terminal)
}

struct RouteLease<'a>(&'a Guard, Route);
impl Drop for RouteLease<'_> {
    fn drop(&mut self) {
        self.0.release(&self.1);
    }
}

/// The caller retains the RPC/backend until this future completes and retires it
/// on any error or if cleanup_confirmed is false. Use cancellation tokens;
/// do not abort the task. A dedicated backend prevents cross-request callbacks.
pub async fn guarded_stream(
    rpc: &Rpc,
    guard: &Guard,
    request: StreamRequest<'_>,
    sender: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<StreamSummary> {
    guarded_stream_inner(rpc, guard, request, sender, cancel, false).await
}

pub async fn guarded_shared_stream(
    rpc: &Rpc,
    guard: &Guard,
    request: StreamRequest<'_>,
    sender: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<StreamSummary> {
    guarded_stream_inner(rpc, guard, request, sender, cancel, true).await
}

async fn guarded_stream_inner(
    rpc: &Rpc,
    guard: &Guard,
    request: StreamRequest<'_>,
    sender: mpsc::Sender<Value>,
    cancel: CancellationToken,
    shared: bool,
) -> Result<StreamSummary> {
    let mut owned_thread = None;
    let mut result = async {
    if request.prompt.trim().is_empty()
        || request.prompt.len() > 16 * 1024
        || request.timeout.is_zero()
        || request.timeout > Duration::from_secs(300)
    {
        return Err(Error::new("INVALID_INPUT", "文本或执行超时不符合限制。"));
    }
    if cancel.is_cancelled() || sender.is_closed() {
        return Err(Error::new("STREAM_CANCELLED", "请求在派发前已取消。"));
    }
    let lease = RouteLease(guard, guard.route(&request.provider.model)?);
    let route = &lease.1;
    let config = provider::overrides(request.provider, &route.base_url)?;
    let started=rpc.request("thread/start",json!({"modelProvider":request.provider.id,"model":request.provider.model,"allowProviderModelFallback":false,"cwd":request.cwd,"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","dynamicTools":[],"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],"config":config})).await;
    let started = started?;
    if started["model"] != request.provider.model
        || started["modelProvider"] != request.provider.id
        || started["approvalPolicy"] != "never"
        || started["sandbox"]["type"] != "readOnly"
    {
        return Err(Error::new("STREAM_POLICY", "原生线程未确认文本策略。"));
    }
    let thread = id(&started["thread"], "id")?;
    owned_thread = Some(thread.clone());
    if cancel.is_cancelled() || sender.is_closed() {
        return Err(Error::new("STREAM_CANCELLED", "请求在轮次派发前已取消。"));
    }
    let mut events = rpc.subscribe();
    let uncertain = || {
        Error::new(
            "STREAM_START_UNCERTAIN",
            "轮次启动结果未知，必须回收专用后端；未重发请求。",
        )
    };
    let turn=rpc.request("turn/start",json!({"threadId":thread,"input":[{"type":"text","text":request.prompt,"text_elements":[]}]})).await.map_err(|_| uncertain())?;
    let turn_id = id(&turn["turn"], "id").map_err(|_| uncertain())?;
    let created_at = turn["turn"]["startedAt"]
        .as_u64()
        .unwrap_or_else(|| crate::now_ms() / 1000);
    let mut state = TextState::new(&thread, &turn_id, &request.provider.model, created_at)?;
    let deadline = tokio::time::Instant::now() + request.timeout;
    let result=tokio::time::timeout_at(deadline,async {
        let created = state.created()?;
        if !guard.client_mode() { deliver(&sender,created,&cancel).await?; }
        loop {
            let event=tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err(Error::new("STREAM_CANCELLED","调用方取消了当前请求。")),
                _=sender.closed()=>return Err(Error::new("STREAM_CONSUMER_CLOSED","下游已断开。")),
                _=guard.client_completed()=>return Ok(vec![]),
                event=events.recv()=>event.map_err(|_|Error::new("STREAM_EVENTS_LOST","原生事件中断或积压溢出。"))?,
            };
            let outgoing=state.accept(&event)?;
            if state.status.is_some() {return Ok(outgoing);}
            if !guard.client_mode() { for output in outgoing {deliver(&sender,output,&cancel).await?;} }
        }
    }).await.unwrap_or_else(|_|Err(Error::new("STREAM_TIMEOUT","文本轮次执行超时。")));
    let mut error = result.as_ref().err().cloned();
    let client_completed = guard.is_client_completed();
    if error.is_none() && !client_completed && state.status.as_deref() != Some("completed") {
        error = Some(Error::new("STREAM_NATIVE_FAILED", "原生轮次未成功完成。"));
    }
    let (acknowledged, terminal) = if (error.is_some() || client_completed) && state.status.is_none() {
        interrupt(rpc, &mut events, &thread, &turn_id).await
    } else {
        (false, state.status.clone())
    };
    if error.is_none() && client_completed && (!acknowledged || terminal.as_deref() != Some("interrupted")) {
        error = Some(Error::new("STREAM_CLEANUP_UNCONFIRMED", "上游已完成，但原生轮次未确认停止。"));
    }
    state.status = terminal.clone();
    let close_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while route.snapshot().state == "active" && tokio::time::Instant::now() < close_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stats = route.snapshot();
    let clean = terminal.is_some() && stats.state != "active";
    if error.is_none() && (stats.state != "completed" || stats.upstream_requests != 1) {
        error = Some(Error::new(
            "STREAM_GUARD_INCOMPLETE",
            "过滤层没有确认完整终态。",
        ));
    }
    if let Some(failure) = error.as_mut() {
        *failure = classify_failure(&failure.code, &stats);
    }
    if let Some(error) = &error {
        let _ = sender.try_send(state.failed(&error.code));
    } else if !guard.client_mode() {
        for output in result.unwrap_or_default() {
            if let Err(e) = deliver(&sender, output, &cancel).await {
                error = Some(e);
                break;
            }
        }
    }
    let metrics = guard.client_metrics();
    Ok(StreamSummary {
        response_model: stats.response_model.clone(),
        response_service_tier: None,
        thread_id: thread,
        turn_id,
        item_ids: metrics.as_ref().map(|m| m.item_ids.clone()).unwrap_or_else(||state.messages.iter().map(|m| m.id.clone()).collect()),
        outcome: if error.is_none() {
            "completed"
        } else if error.as_ref().is_some_and(|e| {
            ["STREAM_CANCELLED", "STREAM_CONSUMER_CLOSED"].contains(&e.code.as_str())
        }) {
            "cancelled"
        } else {
            "failed"
        }
        .into(),
        native_status: state.status,
        error_message: error.as_ref().map(|e| e.message.clone()),
        error_code: error.map(|e| e.code),
        delta_count: metrics.as_ref().map_or(state.delta_count, |m| m.delta_count),
        text_bytes: metrics.as_ref().map_or(state.bytes, |m| m.text_bytes),
        first_delta_at: metrics.as_ref().map_or(state.first_delta_at, |m| m.first_delta_at),
        usage: metrics.as_ref().map_or(state.usage, |m| m.usage.clone()),
        interrupt_acknowledged: acknowledged,
        cleanup_confirmed: clean,
        guard: stats,
    })
    }.await;
    if shared {
        if let Some(thread) = owned_thread {
            let released = rpc
                .request("thread/unsubscribe", json!({"threadId":thread}))
                .await
                .is_ok();
            if !released {
                if let Ok(summary) = &mut result {
                    summary.cleanup_confirmed = false;
                }
            }
        }
    }
    result
}

/// Only local codes and the observed HTTP status are exposed; raw upstream
/// bodies and native error strings can contain credentials or request content.
pub fn classify_failure(code: &str, stats: &Stats) -> Error {
    if [
        "STREAM_NATIVE_FAILED",
        "STREAM_GUARD_INCOMPLETE",
        "UPSTREAM_UNAVAILABLE",
        "UPSTREAM_AUTH_FAILED",
        "UPSTREAM_RATE_LIMITED",
        "UPSTREAM_TIMEOUT",
        "UPSTREAM_REQUEST_REJECTED",
        "UPSTREAM_CONNECTION_FAILED",
        "UPSTREAM_INVALID_RESPONSE",
    ]
    .contains(&code)
    {
        if let Some(status) = stats.upstream_status.filter(|s| *s != 200) {
            let (code, detail) = match status {
                401 | 403 => (
                    "UPSTREAM_AUTH_FAILED",
                    "上游拒绝原生凭据，请检查上游账户与授权。",
                ),
                429 => (
                    "UPSTREAM_RATE_LIMITED",
                    "上游拒绝请求，请检查上游额度或限流状态。",
                ),
                408 | 504 => ("UPSTREAM_TIMEOUT", "上游响应超时，请检查上游服务。"),
                500..=599 => (
                    "UPSTREAM_UNAVAILABLE",
                    "上游服务暂不可用，请检查上游账户、模型及运行状态。",
                ),
                _ => (
                    "UPSTREAM_REQUEST_REJECTED",
                    "上游未接受请求，请检查上游模型与接口配置。",
                ),
            };
            return Error::new(code, &format!("上游返回 HTTP {status}。{detail}"));
        }
        match stats.failure_code.as_deref() {
            Some("GUARD_TIMEOUT") => return Error::new("UPSTREAM_TIMEOUT", "等待上游响应超时。"),
            Some("GUARD_UPSTREAM") => {
                return Error::new(
                    if stats.upstream_status == Some(200) {
                        "UPSTREAM_INVALID_RESPONSE"
                    } else {
                        "UPSTREAM_CONNECTION_FAILED"
                    },
                    "上游连接失败、流中断或未返回有效 SSE，请检查上游服务。",
                );
            }
            _ => {}
        }
    }
    let message = match code {
        "STREAM_CANCELLED" => "请求已取消。",
        "STREAM_TIMEOUT" => "原生轮次执行超时。",
        "STREAM_OUTPUT_LIMIT" => "文本输出超过本地上限。",
        "STREAM_MODEL_CHANGED" => "原生轮次更改了指定模型。",
        "STREAM_DISCONNECTED" => "原生连接提前关闭。",
        _ => "原生轮次未成功完成，请根据请求 ID 查看中转站诊断日志。",
    };
    Error::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_delivery_handles_cancel_disconnect_and_stalled_reader() {
        let (tx, rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        deliver(&tx, json!({}), &cancel).await.unwrap();
        assert_eq!(
            deliver(&tx, json!({}), &cancel).await.unwrap_err().code,
            "STREAM_BACKPRESSURE"
        );
        cancel.cancel();
        assert_eq!(
            deliver(&tx, json!({}), &cancel).await.unwrap_err().code,
            "STREAM_CANCELLED"
        );
        drop(rx);
        assert_eq!(
            deliver(&tx, json!({}), &CancellationToken::new())
                .await
                .unwrap_err()
                .code,
            "STREAM_CONSUMER_CLOSED"
        );
    }
}
