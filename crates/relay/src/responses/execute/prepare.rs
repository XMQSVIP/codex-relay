//! Validate the owned history and settings before starting or resuming a turn.
use super::super::{
    Request, compaction, continuation, invalid, native_compaction_requires_transport, reuse,
    service_tier, tool_catalog,
};
use super::PreparedTurn;
use crate::{
    error::{Error, Result},
    provider::Provider,
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_turn(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &Path,
    request: &Request,
    history: &[Value],
    outgoing: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
    mut timing: Option<&mut crate::execution::PreflightTiming>,
    reuse: &mut Option<reuse::Reuse<'_>>,
) -> Result<PreparedTurn> {
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
    let native_context = reuse::Context::new(
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
    let tool_echoes: std::collections::HashSet<String> = previous
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
    let (directory, retained_thread) = match (previous, offset) {
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
    let (notifications, resumed_waiting) = match incoming {
        Some(waiting) => (None, Some(waiting)),
        None => (Some(rpc.subscribe_thread(&thread)), None),
    };
    if let Some(tracker) = &tracker {
        tracker.bind(&thread)?;
    }
    if let Some(timing) = &mut timing {
        timing.mark("thread_start");
    }
    let mut items = tool_catalog::native_input(
        history,
        request.tool_choice == "none",
        &request.tools,
        offset,
    );
    items.extend(rpc.location_search_item(&request.tools, request.tool_choice == "none"));
    if resumed_waiting.is_none() {
        compaction::inject(rpc, &thread, items).await?;
    }
    if let Some(timing) = &mut timing {
        timing.mark("history_injection");
    }
    turn_params["threadId"] = json!(thread);
    if let Some(timing) = &mut timing {
        timing.mark("turn_parameters");
        if resumed_waiting.is_some() {
            timing.submitted_as("native_tool_results_submit");
        } else {
            timing.submitted();
        }
    }
    let model_started_at = std::sync::atomic::AtomicU64::new(0);
    let resumed = resumed_waiting.is_some();
    let turn_guard = if let Some(mut waiting) = resumed_waiting {
        if waiting.guard.thread != thread {
            return Err(invalid("续聊线程不匹配。"));
        }
        let replies = replies.ok_or_else(|| invalid("工具结果与待处理回调不匹配。"))?;
        // Claiming only establishes ownership at that instant. Catalog discovery
        // and live settings can await while native completion/output is queued.
        if !waiting.unchanged() {
            return Err(Error::new(
                "NATIVE_TURN_UNAVAILABLE",
                "原生待答轮次已结束或上下文已变化，未提交工具结果。",
            ));
        }
        if let Some(control) = &request.control {
            control.activate(rpc, &thread, &waiting.guard.turn);
        }
        if let Some(mut update) =
            pending_update.filter(|value| value.as_object().is_some_and(|v| !v.is_empty()))
        {
            update["threadId"] = json!(thread);
            update["turnId"] = json!(waiting.guard.turn);
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
        if !waiting.unchanged() {
            if let Some(control) = &request.control {
                control.finish();
            }
            return Err(Error::new(
                "NATIVE_TURN_UNAVAILABLE",
                "原生待答轮次已结束或上下文已变化，未提交工具结果。",
            ));
        }
        // Recheck after each capacity wait without consuming output produced by
        // earlier replies; collection owns those events once the batch is sent.
        for (id, result) in replies {
            let sent = tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","请求已取消。")),
                _=outgoing.closed()=>Err(Error::new("STREAM_CONSUMER_CLOSED","客户端已断开。")),
                _=rpc.closed()=>Err(Error::new("RPC_CLOSED","原生连接已关闭。")),
                sent=rpc.answer_tool_checked(id,result, || {
                    if cancel.is_cancelled() {
                        Err(Error::new("REQUEST_CANCELLED", "请求已取消。"))
                    } else if outgoing.is_closed() {
                        Err(Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。"))
                    } else if rpc.is_closed() {
                        Err(Error::new("RPC_CLOSED", "原生连接已关闭。"))
                    } else if !waiting.submission_valid() {
                        Err(Error::new("NATIVE_TURN_UNAVAILABLE", "原生待答轮次已结束或上下文已变化，未提交剩余工具结果。"))
                    } else {
                        let _ = model_started_at.compare_exchange(0, crate::now_ms(), std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed);
                        Ok(())
                    }
                })=>sent,
            };
            if let Err(error) = sent {
                if let Some(control) = &request.control {
                    control.finish();
                }
                return Err(error);
            }
        }
        waiting.guard
    } else {
        let turn = rpc
            .request_checked("turn/start", turn_params, || {
                if cancel.is_cancelled() {
                    Err(Error::new("REQUEST_CANCELLED", "请求已取消。"))
                } else if outgoing.is_closed() {
                    Err(Error::new("STREAM_CONSUMER_CLOSED", "客户端已断开。"))
                } else {
                    // Repeated after automatic context injection, immediately
                    // before enqueueing the actual turn/start.
                    model_started_at.store(crate::now_ms(), std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                }
            })
            .await?;
        let turn = turn["turn"]["id"]
            .as_str()
            .ok_or_else(|| invalid("原生轮次缺少标识。"))?
            .to_owned();
        continuation::TurnGuard::new(
            rpc.clone(),
            thread.clone(),
            turn,
            notifications.unwrap(),
            reuse
                .as_mut()
                .and_then(|r| r.continuation.as_mut())
                .and_then(|c| c.owner.take()),
        )
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
    Ok(PreparedTurn {
        model_started_at: model_started_at.load(std::sync::atomic::Ordering::Relaxed),
        guard: turn_guard,
        directory,
        native_context,
        tracker,
        tool_echoes,
        can_keep_tools,
        default_effort,
        default_summary,
        default_service_tier,
    })
}
