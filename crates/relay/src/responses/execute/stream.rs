//! Collect one response boundary without releasing or transferring the native turn.
use super::super::{Request, continuation, invalid, send, tool_catalog};
use super::{PreparedTurn, TurnOutput};
use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) async fn collect(
    rpc: &Rpc,
    request: &Request,
    outgoing: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
    prepared: &mut PreparedTurn,
    state: &mut TurnOutput,
) -> Result<()> {
    let PreparedTurn {
        guard,
        native_context,
        tracker,
        tool_echoes,
        can_keep_tools,
        ..
    } = prepared;
    let thread = guard.thread.as_str();
    let turn = guard.turn.as_str();
    let notifications = guard.events.as_mut().unwrap();
    let can_keep_tools = *can_keep_tools;
    let TurnOutput {
        output,
        callbacks,
        turn_ended,
    } = state;
    let mut budget_items = HashSet::new();
    let mut response_boundary = false;
    let mut callback_deadline = None;
    for event in output.created() {
        send(outgoing, event, cancel).await?;
    }
    let deadline = tokio::time::sleep(Duration::from_secs(1800));
    tokio::pin!(deadline);
    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
            _ = outgoing.closed() => return Err(Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。")),
            _ = rpc.closed() => return Err(Error::new("RPC_CLOSED", "原生后端已断开。")),
            _ = &mut deadline => return Err(Error::new("UPSTREAM_TIMEOUT", "原生请求超过 30 分钟。")),
            _ = async {
                if let Some(at) = callback_deadline {
                    tokio::time::sleep_until(at).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => return Ok(()),
            event = notifications.recv() => {
                event.map_err(|_| Error::new("STREAM_EVENTS_LOST", "原生事件中断。"))?
            }
        };
        let method = event["method"].as_str().unwrap_or("");
        if method == "bridge/callbackRejected" {
            return Err(Error::new(
                "NATIVE_CALLBACK_DENIED",
                "原生请求了未开放的服务器能力。",
            ));
        }
        let p = &event["params"];
        // Media is validated from inline data only. savedPath describes a file
        // managed by Codex (often under its home), never a relay file to read.
        if p["threadId"] != thread {
            continue;
        }
        if method == "thread/closed" {
            return Err(Error::new("NATIVE_THREAD_CLOSED", "原生对话线程已关闭。"));
        }
        if p.get("turnId").is_some_and(|v| v != turn) {
            continue;
        }
        if method == "turn/completed" && p["turn"]["id"] != turn {
            continue;
        }
        // Native cleanup and output validity are independent. Record the
        // matching terminal event before adapters can reject incomplete tools.
        // Once consumed, this confirmation cannot be awaited again on error.
        if method == "turn/completed" {
            *turn_ended = true;
        }
        native_context.observe(method, p, output.has_calls() || !callbacks.is_empty())?;
        if method == "rawResponseItem/completed"
            && matches!(
                p["item"]["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            )
            && p["item"]["call_id"]
                .as_str()
                .is_some_and(|id| tool_echoes.remove(id))
        {
            continue;
        }
        if method == "rawResponseItem/completed"
            && let Some(tracker) = &tracker
        {
            let digest = crate::profiles::warm::digest(&p["item"])?;
            if budget_items.insert(digest) {
                tracker.output(serde_json::to_vec(&p["item"])?.len());
            }
        }
        if method == "item/tool/call"
            && request
                .client_tool_identity(p["namespace"].as_str(), p["tool"].as_str().unwrap_or(""))
                .is_none()
        {
            return Err(tool_catalog::scope_error(
                "原生工具不属于调用方注册的函数。",
                p["namespace"].as_str(),
                p["tool"].as_str().unwrap_or(""),
            ));
        }
        if method == "item/tool/call" && can_keep_tools {
            if p["turnId"] != turn {
                return Err(invalid("原生工具回调轮次不匹配。"));
            }
            let callback = continuation::callback(request, &event)?;
            if callbacks
                .iter()
                .any(|c: &continuation::Call| c.id == callback.id || c.rpc_id == callback.rpc_id)
            {
                return Err(invalid("原生返回重复工具回调。"));
            }
            callbacks.push(callback);
        }
        for value in output.consume(&event, request)? {
            send(outgoing, value, cancel).await?;
        }
        // A Responses request ends at the upstream response boundary when tools
        // are yielded. Never answer callbacks with fabricated tool results.
        if method == "rawResponse/completed" && output.has_calls() {
            response_boundary = true;
        }
        // Native tools can run alongside client callbacks. Their model-response
        // boundary is not the end of execution; wait for the native result before
        // starting the callback grace period or handing the response to the client.
        if output.has_pending_native_tools() {
            callback_deadline = None;
        } else if response_boundary && callback_deadline.is_none() {
            callback_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
        }
        if (response_boundary
            && !output.has_pending_native_tools()
            && (!can_keep_tools
                || !native_context.reusable()
                || !output.can_retain_tools()
                || continuation::calls_match(callbacks, output.items())))
            || method == "turn/completed"
        {
            if method == "turn/completed"
                && p["turn"]["status"] == "interrupted"
                && request
                    .control
                    .as_ref()
                    .is_some_and(|c| c.interrupt_requested())
            {
                output.interrupted();
                return Ok(());
            }
            if method == "turn/completed" && p["turn"]["status"] != "completed" {
                return Err(crate::execution::native_failure(&p["turn"]["error"]));
            }
            return Ok(());
        }
    }
}
