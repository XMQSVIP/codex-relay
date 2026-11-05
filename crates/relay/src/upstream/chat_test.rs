//! An isolated transfer: no response bindings, pooled sockets or content journal.
use super::{Config, Connections, bindings::Binding, dispatch::Events, transport};
use crate::{
    error::{Error, Result},
    operations::ClientAccess,
    streaming::StreamSummary,
};
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, header},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

pub(crate) async fn run(
    config: Config,
    client: reqwest::Client,
    id: Uuid,
    model: String,
    reasoning_effort: Option<String>,
    history: Vec<Value>,
    outgoing: mpsc::Sender<Value>,
) -> Result<StreamSummary> {
    let mut value = json!({"model":model,"input":history,"stream":true,"store":false,"tools":[],"tool_choice":"none"});
    if let Some(effort) = reasoning_effort {
        value["reasoning"] = json!({"effort":effort});
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    let binding = Binding {
        native_tools: false,
        account: id,
        model: model.clone(),
        provider: config.provider(),
        websocket: false,
    };
    // This scope owns its connection and never remembers response IDs.
    let access = ClientAccess {
        fast_mode: false,
        key_id: Uuid::new_v4(),
        group_id: Uuid::nil(),
        models: vec![],
    };
    let connections = Connections::default();
    let mut transfer = transport::send(
        &config,
        &client,
        Method::POST,
        "responses",
        &headers,
        Bytes::from(serde_json::to_vec(&value)?),
        &value,
        &binding,
        None,
        &access,
        &connections,
    )
    .await?;
    if !transfer.status.is_success() {
        return Err(transport::status_error(transfer.status));
    }
    if !transfer
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"))
    {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "上游未返回文本事件流。",
        ));
    }
    outgoing
        .send(json!({"type":"test.ready","model":model}))
        .await
        .map_err(|_| closed())?;
    let mut events = Events::default();
    let mut first = None;
    while let Some(chunk) = transfer.chunks.next().await {
        for event in events.push(&chunk?)? {
            match event["type"].as_str() {
                Some("response.output_text.delta") => {
                    first.get_or_insert(crate::now_ms());
                    outgoing.send(event).await.map_err(|_| closed())?;
                }
                Some("response.completed") => return Ok(super::summary(&event["response"], first)),
                Some("response.failed" | "response.incomplete" | "error") => {
                    return Err(Error::new(
                        "UPSTREAM_REQUEST_FAILED",
                        "上游未能完成测试回复。",
                    ));
                }
                _ => {}
            }
        }
    }
    Err(Error::new(
        "UPSTREAM_INVALID_RESPONSE",
        "上游流提前断开，未收到完成事件。",
    ))
}
fn closed() -> Error {
    Error::new("STREAM_CONSUMER_CLOSED", "测试窗口已关闭。")
}
