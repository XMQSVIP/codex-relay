//! Native callbacks remain unanswered between Responses requests. No history bodies are retained.
use super::{Request, reuse::Session};
use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Call {
    pub id: String,
    pub rpc_id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
    pub arguments: [u8; 32],
}

pub(crate) struct TurnGuard {
    pub rpc: Rpc,
    pub thread: String,
    pub turn: String,
    pub owner: Option<crate::profiles::warm::Activity>,
}
impl TurnGuard {
    pub fn disarm(&mut self) -> Option<crate::profiles::warm::Activity> {
        self.owner.take()
    }
}
impl Drop for TurnGuard {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let rpc = self.rpc.clone();
            let thread = self.thread.clone();
            let turn = self.turn.clone();
            tokio::spawn(async move {
                let mut events = rpc.subscribe_thread(&thread);
                let clean = interrupt(&rpc, &thread, &turn, &mut events).await;
                rpc.clear_thread_tool_callbacks(&thread);
                if crate::native_cache::enabled() {
                    crate::profiles::resources::close_context(&owner.backend, &owner.slot, &thread)
                        .await;
                } else {
                    let _ = rpc
                        .request("thread/unsubscribe", json!({"threadId":thread}))
                        .await;
                }
                if !clean {
                    owner.retire();
                }
            });
        }
    }
}

pub(crate) struct Waiting {
    pub guard: TurnGuard,
    pub events: broadcast::Receiver<Value>,
}

impl Waiting {
    /// A parked turn may emit status notifications, but cannot change context or
    /// generate more output before the next request owns it. Check at claim as
    /// well as in the expiry task, so claiming cannot race the periodic check.
    pub(crate) fn unchanged(&mut self) -> bool {
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    let method = event["method"].as_str();
                    if matches!(
                        method,
                        Some(
                            "turn/completed"
                                | "thread/closed"
                                | "thread/compacted"
                                | "rawResponse/completed"
                                | "rawResponseItem/completed"
                                | "item/tool/call"
                                | "bridge/callbackRejected"
                        )
                    ) || (matches!(method, Some("item/started" | "item/completed"))
                        && event["params"]["item"]["type"] == "contextCompaction")
                    {
                        return false;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => return true,
                Err(_) => return false,
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Continuation {
    pub tracker: Option<crate::profiles::resources::Tracker>,
    pub incoming: Option<Waiting>,
    pub waiting: Option<Waiting>,
    pub owner: Option<crate::profiles::warm::Activity>,
}

pub(crate) async fn interrupt(
    rpc: &Rpc,
    thread: &str,
    turn: &str,
    events: &mut broadcast::Receiver<Value>,
) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        rpc.request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
            .await?;
        loop {
            let event = events
                .recv()
                .await
                .map_err(|_| Error::new("RPC_CLOSED", "原生事件已关闭。"))?;
            if event["method"] == "turn/completed"
                && event["params"]["threadId"] == thread
                && event["params"]["turn"]["id"] == turn
            {
                return Ok::<_, Error>(());
            }
        }
    })
    .await
    .is_ok_and(|r| r.is_ok())
}

pub(crate) fn callback(request: &Request, event: &Value) -> Result<Call> {
    let p = &event["params"];
    if !event["id"].is_string() && !event["id"].is_number() {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "工具回调缺少有效 RPC ID。",
        ));
    }
    let (namespace, name) = request
        .client_tool_identity(p["namespace"].as_str(), p["tool"].as_str().unwrap_or(""))
        .ok_or_else(|| Error::new("NATIVE_TOOL_SCOPE", "工具回调不属于当前请求。"))?;
    let id = p["callId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::new("UPSTREAM_INVALID_RESPONSE", "工具回调缺少 callId。"))?;
    Ok(Call {
        id: id.into(),
        rpc_id: event["id"].to_string(),
        arguments: crate::profiles::warm::digest(&p["arguments"])?,
        custom: request.is_custom_tool(namespace.as_deref(), &name),
        name,
        namespace,
    })
}

pub(crate) fn calls_match(calls: &[Call], output: &[Value]) -> bool {
    let emitted: Vec<_> = output
        .iter()
        .filter(|v| {
            matches!(
                v["type"].as_str(),
                Some("function_call" | "custom_tool_call")
            )
        })
        .collect();
    !calls.is_empty()
        && calls.len() == emitted.len()
        && calls.iter().all(|call| {
            emitted
                .iter()
                .filter(|v| {
                    v["call_id"] == call.id
                        && v["name"] == call.name
                        && v["namespace"].as_str() == call.namespace.as_deref()
                        && (v["type"] == "custom_tool_call") == call.custom
                        && argument_digest(v).is_some_and(|digest| digest == call.arguments)
                })
                .count()
                == 1
        })
}

fn argument_digest(item: &Value) -> Option<[u8; 32]> {
    let arguments = if item["type"] == "custom_tool_call" {
        json!({"input":item["input"].as_str()?})
    } else {
        serde_json::from_str::<Value>(item["arguments"].as_str()?).ok()?
    };
    crate::profiles::warm::digest(&arguments).ok()
}

pub(crate) fn eligible(output: &[Value]) -> bool {
    output.iter().any(|v| {
        matches!(
            v["type"].as_str(),
            Some("function_call" | "custom_tool_call")
        )
    }) && output.iter().all(|v| {
        !matches!(
            v["type"].as_str(),
            Some("tool_search_call" | "tool_search_output")
        )
    }) && output
        .iter()
        .filter(|v| {
            matches!(
                v["type"].as_str(),
                Some("function_call" | "custom_tool_call")
            )
        })
        .all(|v| argument_digest(v).is_some())
}

/// Validate the complete batch before any callback is answered. Supplemental code-mode
/// notifications and image detail overrides keep the full-history compatibility path.
pub(crate) fn replies(
    session: &Session,
    history: &[Value],
    offset: usize,
) -> Option<Vec<(Value, Value)>> {
    let mut outputs = std::collections::HashMap::new();
    for item in history.get(offset..)? {
        if crate::native_cache::enabled() && item["type"] == "configuration_update" {
            // The resolved effort is applied by turn/settings/update before any
            // callback is answered. Keep controls in history, not in the reply count.
            super::request::configuration_effort(item).ok()?;
        } else {
            let id = item["call_id"].as_str()?;
            if outputs.insert(id, item).is_some() {
                return None;
            }
        }
    }
    if outputs.len() != session.calls.len() || outputs.is_empty() {
        return None;
    }
    let mut replies = Vec::new();
    for call in &session.calls {
        let item = outputs.get(call.id.as_str())?;
        if item["type"]
            != if call.custom {
                "custom_tool_call_output"
            } else {
                "function_call_output"
            }
            || item["name"].as_str().is_some_and(|n| n != call.name)
            || item["namespace"]
                .as_str()
                .is_some_and(|n| Some(n) != call.namespace.as_deref())
        {
            return None;
        }
        let content = if let Some(text) = item["output"].as_str() {
            vec![json!({"type":"inputText","text":text})]
        } else {
            let mut content = Vec::new();
            for part in item["output"].as_array()? {
                match part["type"].as_str()? {
                    "input_text" => {
                        content.push(json!({"type":"inputText","text":part["text"].as_str()?}))
                    }
                    "input_image" if part["detail"].is_null() || part["detail"] == "auto" => {
                        content.push(
                            json!({"type":"inputImage","imageUrl":part["image_url"].as_str()?}),
                        )
                    }
                    _ => return None,
                }
            }
            content
        };
        let id = serde_json::from_str::<Value>(&call.rpc_id).ok()?;
        let result = json!({"contentItems":content,"success":true});
        if serde_json::to_vec(&json!({"id":id,"result":result}))
            .ok()?
            .len()
            > crate::rpc::MAX_BYTES
        {
            return None;
        }
        replies.push((id, result));
    }
    Some(replies)
}

#[cfg(test)]
mod tests;
