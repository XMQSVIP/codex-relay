use super::AdminState;
use crate::error::{Error, Result};
use axum::{
    Json,
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::{Stream, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    model: Option<String>,
    #[serde(rename = "reasoningEffort")]
    reasoning_effort: Option<String>,
    messages: Vec<Message>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    role: String,
    content: String,
}
impl Input {
    fn validate(&self) -> Result<()> {
        if self.reasoning_effort.as_deref().is_some_and(|effort| {
            ![
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&effort)
        }) {
            return Err(Error::new("INVALID_INPUT", "测试推理强度无效。"));
        }
        if self.model.as_ref().is_some_and(|m| {
            m.is_empty()
                || m.len() > 128
                || !m
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        }) {
            return Err(Error::new("INVALID_INPUT", "测试模型 ID 格式无效。"));
        }
        if self.messages.is_empty()
            || self.messages.len() > 31
            || self.messages.len() % 2 == 0
            || self.messages.iter().enumerate().any(|(i, m)| {
                m.role != if i % 2 == 0 { "user" } else { "assistant" }
                    || m.content.trim().is_empty()
                    || m.content.len() > 32768
            })
            || self.messages.iter().map(|m| m.content.len()).sum::<usize>() > 48 * 1024
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "请提交交替的用户与助手文本，最后一条为用户消息；最多 16 轮、单条 32 KiB、合计 48 KiB。",
            ));
        }
        Ok(())
    }
}

pub(super) async fn start(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    input.validate()?;
    let cancel = state.shutdown.child_token();
    let guard = crate::inference::CancelOnDrop(cancel.clone());
    let (tx, rx) = mpsc::channel::<Value>(32);
    state.tasks.clone().spawn(async move {
        let started=Instant::now();
        let work=run(&state,id,input,&tx,&cancel,started).await;
        if let Err(error)=work {
            let _=send(&tx,json!({"type":"error","code":error.code,"message":error.message,"totalMs":started.elapsed().as_millis()}),&cancel).await;
        }
    });
    // The guard belongs to the response body, including while admission is queued.
    let events = stream::unfold((rx, guard), |(mut rx, guard)| async move {
        rx.recv()
            .await
            .map(|value| (Ok(Event::default().data(value.to_string())), (rx, guard)))
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

async fn send(tx: &mpsc::Sender<Value>, value: Value, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        _=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","测试已停止。")),
        result=tokio::time::timeout(Duration::from_secs(5),tx.send(value))=>match result {
            Ok(Ok(()))=>Ok(()),
            _=>{cancel.cancel();Err(Error::new("STREAM_CONSUMER_CLOSED","测试窗口已关闭或读取超时。"))}
        }
    }
}

async fn run(
    state: &Arc<AdminState>,
    id: Uuid,
    input: Input,
    tx: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
    started: Instant,
) -> Result<()> {
    let lease = tokio::select! {
        _=cancel.cancelled()=>return Ok(()),
        result=state.account_lease(id,"diagnostic","chat_test")=>result?,
    };
    let request_id = lease.request_id();
    let _registration = match state.profiles.register_inference(id, cancel.clone()) {
        Ok(registration) => registration,
        Err(error) => return lease.complete(Err(error)),
    };
    if let Some(provider) = state.operations.configured_provider(id) {
        if let Err(error) = state.check_route_loop(&provider.base_url).await {
            return lease.complete(Err(error));
        }
    }
    let history = input
        .messages
        .into_iter()
        .map(|m| json!({"role":m.role,"content":m.content}))
        .collect();
    let (native_tx, mut native_rx) = mpsc::channel(32);
    let execution = state.profiles.test_chat(
        id,
        input.model,
        input.reasoning_effort,
        history,
        native_tx,
        cancel.clone(),
    );
    let forward = async {
        let mut first = None;
        let mut bytes = 0usize;
        while let Some(value) = native_rx.recv().await {
            let event = match value["type"].as_str() {
                Some("test.ready") => {
                    Some(json!({"type":"ready","model":value["model"],"requestId":request_id}))
                }
                Some("response.output_text.delta") => {
                    let text = value["delta"].as_str().unwrap_or("");
                    if !text.is_empty() {
                        first.get_or_insert(started.elapsed().as_millis() as u64);
                    }
                    bytes += text.len();
                    if bytes > 1024 * 1024 {
                        cancel.cancel();
                        return Err(Error::new("OUTPUT_LIMIT", "测试回复超过 1 MiB，已停止。"));
                    }
                    Some(json!({"type":"delta","text":text}))
                }
                _ => None,
            };
            if let Some(event) = event {
                send(tx, event, cancel).await?;
            }
        }
        Ok(first)
    };
    let (outcome, forwarded) = tokio::join!(execution, forward);
    let forwarding_error = match &forwarded {
        Err(error) => Some(error.clone()),
        Ok(None) if outcome.error().is_none() => Some(Error::new(
            "EMPTY_RESPONSE",
            "请求结束，但没有收到文本回复。",
        )),
        _ => None,
    };
    let settled = lease.finish_execution(&outcome, forwarding_error.as_ref());
    let _ = state
        .operations
        .record_delivery(request_id, !tx.is_closed());
    if let Some(error) = outcome.error() {
        return Err(error);
    }
    settled?;
    if let Some(error) = forwarding_error {
        return Err(error);
    }
    let first = forwarded?;
    let summary = outcome.model?;
    send(
        tx,
        json!({"type":"done","requestId":request_id,"model":summary.response_model,
        "firstResponseMs":first,"totalMs":started.elapsed().as_millis(),"usage":summary.usage}),
        cancel,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bounded_plain_text_turns_are_accepted() {
        for value in [
            json!({"messages":[]}),
            json!({"messages":[{"role":"system","content":"x"}]}),
            json!({"messages":[{"role":"user","content":"x"}],"model":"bad model"}),
            json!({"messages":[{"role":"user","content":"x".repeat(32769)}]}),
            json!({"messages":[{"role":"user","content":"x"},{"role":"assistant","content":"y"}]}),
        ] {
            assert!(
                serde_json::from_value::<Input>(value)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let input:Input=serde_json::from_value(json!({"model":"test-model","messages":[{"role":"user","content":"你好"},{"role":"assistant","content":"OK"},{"role":"user","content":"继续"}]})).unwrap();
        assert!(input.validate().is_ok());
        assert!(serde_json::from_value::<Input>(json!({"messages":[],"tools":[]})).is_err());
    }
}
