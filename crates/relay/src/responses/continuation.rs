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

/// Owns native lifetime protection, independently of the HTTP request's Lease.
/// Move the guard into Waiting when yielding tools; dropping an armed guard
/// interrupts only this turn and holds Activity until cleanup has been attempted.
pub(crate) struct TurnGuard {
    pub rpc: Rpc,
    pub thread: String,
    pub turn: String,
    pub owner: Option<crate::profiles::warm::Activity>,
    pub(super) events: Option<broadcast::Receiver<Value>>,
    completed: bool,
}
impl TurnGuard {
    pub(crate) fn new(
        rpc: Rpc,
        thread: String,
        turn: String,
        events: broadcast::Receiver<Value>,
        owner: Option<crate::profiles::warm::Activity>,
    ) -> Self {
        Self {
            rpc,
            thread,
            turn,
            owner,
            events: Some(events),
            completed: false,
        }
    }

    // Keep the original receiver through active execution, parking and Drop.
    // Re-subscribing here would discard a completion already queued for this turn.
    pub(super) async fn cleanup(&mut self) -> bool {
        // Borrow while awaited: cancelling this attempt must leave the receiver
        // and any queued terminal available to the guard's eventual Drop.
        self.completed = self.completed
            || interrupt(
                &self.rpc,
                &self.thread,
                &self.turn,
                self.events.as_mut().unwrap(),
            )
            .await;
        self.completed
    }

    /// Transfer protection only after native completion has been confirmed.
    /// The recipient still owns the read lease until it finishes bookkeeping.
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
            let completed = self.completed;
            let mut events = self.events.take().unwrap();
            tokio::spawn(async move {
                let clean = completed || interrupt(&rpc, &thread, &turn, &mut events).await;
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
    invalidated: bool,
    submission_events: Option<broadcast::Receiver<Value>>,
}

impl Waiting {
    pub(crate) fn new(guard: TurnGuard) -> Self {
        Self {
            guard,
            invalidated: false,
            submission_events: None,
        }
    }

    pub(crate) async fn cleanup(&mut self) -> bool {
        self.guard.cleanup().await
    }

    /// After the first reply, output and new callbacks may legitimately arrive
    /// while the remaining replies wait for transport capacity. Observe lifetime
    /// changes separately so collection still receives every output and usage event.
    pub(super) fn submission_valid(&mut self) -> bool {
        if self.invalidated || self.guard.completed {
            return false;
        }
        if self.submission_events.is_none() {
            // Subscribe before checking the original queue: resubscribe alone
            // skips queued events, while doing it afterwards leaves a race.
            let events = self.guard.events.as_ref().unwrap().resubscribe();
            if !self.unchanged() {
                return false;
            }
            self.submission_events = Some(events);
        }
        loop {
            match self.submission_events.as_mut().unwrap().try_recv() {
                Ok(event) => {
                    let method = event["method"].as_str();
                    let params = &event["params"];
                    let same_thread = params["threadId"] == self.guard.thread;
                    let same_turn = params
                        .get("turnId")
                        .is_none_or(|id| id.as_str() == Some(self.guard.turn.as_str()));
                    let completed = same_thread
                        && method == Some("turn/completed")
                        && params["turn"]["id"] == self.guard.turn;
                    self.guard.completed |= completed;
                    if completed
                        || method == Some("bridge/callbackRejected")
                        || (same_thread
                            && (method == Some("thread/closed")
                                || (same_turn
                                    && (method == Some("thread/compacted")
                                        || (matches!(
                                            method,
                                            Some("item/started" | "item/completed")
                                        ) && params["item"]["type"] == "contextCompaction")))))
                    {
                        self.invalidated = true;
                        return false;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => return true,
                Err(_) => {
                    self.invalidated = true;
                    return false;
                }
            }
        }
    }

    /// A parked turn may emit status notifications, but cannot change context or
    /// generate more output before the next request owns it. Check at claim as
    /// well as in the expiry task, so claiming cannot race the periodic check.
    pub(crate) fn unchanged(&mut self) -> bool {
        if self.invalidated || self.guard.completed {
            return false;
        }
        loop {
            match self.guard.events.as_mut().unwrap().try_recv() {
                Ok(event) => {
                    let method = event["method"].as_str();
                    if method == Some("turn/completed")
                        && event["params"]["threadId"] == self.guard.thread
                        && event["params"]["turn"]["id"] == self.guard.turn
                    {
                        self.guard.completed = true;
                    }
                    if matches!(
                        method,
                        Some(
                            "turn/completed"
                                | "thread/closed"
                                | "thread/compacted"
                                | "item/agentMessage/delta"
                                | "item/reasoning/summaryTextDelta"
                                | "item/reasoning/textDelta"
                                | "rawResponse/completed"
                                | "rawResponseItem/completed"
                                | "item/tool/call"
                                | "bridge/callbackRejected"
                        )
                    ) || (matches!(method, Some("item/started" | "item/completed"))
                        && event["params"]["item"]["type"] == "contextCompaction")
                    {
                        // The background checker releases this lock before
                        // revoking the entry. A racing claim must still see the
                        // invalidity after its triggering event was consumed.
                        self.invalidated = true;
                        return false;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => return true,
                Err(_) => {
                    self.invalidated = true;
                    return false;
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Continuation {
    pub tracker: Option<crate::profiles::resources::Tracker>,
    /// Claimed waiting turn. Preparation must validate the whole result batch
    /// before changing live settings or answering any callback.
    pub incoming: Option<Waiting>,
    /// Produced waiting turn. Profiles retains it until delivery is committed;
    /// HTTP completion alone never disarms its native cleanup guard.
    pub waiting: Option<Waiting>,
    /// Moves into TurnGuard at submission and comes back on confirmed completion.
    pub owner: Option<crate::profiles::warm::Activity>,
}

pub(crate) async fn interrupt(
    rpc: &Rpc,
    thread: &str,
    turn: &str,
    events: &mut broadcast::Receiver<Value>,
) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let request = rpc.request("turn/interrupt", json!({"threadId":thread,"turnId":turn}));
        tokio::pin!(request);
        let mut requested = false;
        loop {
            // Completion is authoritative even if interruption races natural
            // completion, is rejected, or its acknowledgement never arrives.
            // Drain an already queued terminal event before sending an interrupt.
            let event = tokio::select! {
                biased;
                event = events.recv() => match event {
                    Ok(event) => event,
                    // Lost output still invalidates the response, but cleanup
                    // can continue until a subsequent exact terminal confirms it.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return false,
                },
                _ = &mut request, if !requested => { requested = true; continue; },
            };
            if event["method"] == "turn/completed"
                && event["params"]["threadId"] == thread
                && event["params"]["turn"]["id"] == turn
            {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false)
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
mod pre_submit_tests;
#[cfg(test)]
mod tests;
