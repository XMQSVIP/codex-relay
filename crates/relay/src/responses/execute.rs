//! Native turn orchestration: prepare once, collect events, then settle ownership.
//! HTTP delivery and request leases remain in the Responses adapter.
use super::{Request, continuation, events, reuse, send};
use crate::{error::Result, provider::Provider, rpc::Rpc, streaming::StreamSummary};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod prepare;
mod stream;
#[cfg(test)]
mod tests;

// The turn guard travels with its event receiver across tool waits. Dropping it
// retains process protection until interruption/cleanup has been attempted.
struct PreparedTurn {
    guard: continuation::TurnGuard,
    directory: Option<PathBuf>,
    native_context: reuse::Context,
    tracker: Option<crate::profiles::resources::Tracker>,
    tool_echoes: HashSet<String>,
    can_keep_tools: bool,
    default_effort: Option<String>,
    default_summary: Option<String>,
    default_service_tier: Option<Option<String>>,
}

struct TurnOutput {
    output: events::Output,
    callbacks: Vec<continuation::Call>,
    turn_ended: bool,
}

pub async fn run(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &Path,
    request: &Request,
    history: &[Value],
    outgoing: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<StreamSummary> {
    run_profiled(
        rpc, provider, cwd, request, history, outgoing, cancel, None, None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_profiled(
    rpc: &Rpc,
    provider: &Provider,
    cwd: &Path,
    request: &Request,
    history: &[Value],
    outgoing: mpsc::Sender<Value>,
    cancel: CancellationToken,
    timing: Option<&mut crate::execution::PreflightTiming>,
    mut reuse: Option<reuse::Reuse<'_>>,
) -> Result<StreamSummary> {
    let resolved = request.with_history(history)?;
    let request = &resolved;
    let mut prepared = prepare::prepare_turn(
        rpc, provider, cwd, request, history, &outgoing, &cancel, timing, &mut reuse,
    )
    .await?;
    let mut collected = TurnOutput {
        output: events::Output::new(request),
        callbacks: Vec::new(),
        turn_ended: false,
    };
    // Keep all guards in this future while collecting. A response boundary may
    // hand off a still-running turn; only turn/completed confirms native cleanup.
    let work = stream::collect(
        rpc,
        request,
        &outgoing,
        &cancel,
        &mut prepared,
        &mut collected,
    )
    .await;
    let PreparedTurn {
        guard: mut turn_guard,
        mut directory,
        native_context,
        can_keep_tools,
        default_effort,
        default_summary,
        default_service_tier,
        ..
    } = prepared;
    let thread = turn_guard.thread.clone();
    let turn = turn_guard.turn.clone();
    let TurnOutput {
        mut output,
        callbacks,
        turn_ended,
    } = collected;
    // Final output validation must join the same failure/cleanup path as stream
    // errors, before a live turn can be parked or a success summary is returned.
    let work = work.and_then(|()| output.validate_completed());
    if let Some(control) = &request.control {
        control.finish();
    }
    // A shared host cannot be killed to cancel one caller. Wait for that turn's
    // completion event; an uncertain interruption retires the host after draining.
    let mut cleanup_confirmed = turn_ended;
    let history_changed = request
        .control
        .as_ref()
        .is_some_and(|control| !control.cache_reusable());
    let parked = can_keep_tools
        && !history_changed
        && native_context.reusable()
        && !turn_ended
        && work.is_ok()
        && output.can_retain_tools()
        && !cancel.is_cancelled()
        && history.len() + output.items().len() <= 1024
        && continuation::calls_match(&callbacks, output.items());
    if !parked && !turn_ended && (output.has_calls() || work.is_err()) {
        cleanup_confirmed = turn_guard.cleanup().await;
    }
    if !parked {
        rpc.clear_thread_tool_callbacks(&thread);
    }
    if cleanup_confirmed {
        let owner = turn_guard.disarm();
        if let Some(continuation) = reuse.as_mut().and_then(|r| r.continuation.as_mut()) {
            continuation.owner = owner;
        }
    }
    if let Err(error) = work {
        let _ = send(&outgoing, output.failed(&error), &cancel).await;
        if let Ok(mut summary) = output.summary(&thread, &turn) {
            summary.cleanup_confirmed = cleanup_confirmed;
            summary.outcome = if crate::execution::cancelled(&error.code) {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            summary.error_code = Some(error.code);
            summary.error_message = Some(error.message);
            summary.native_status = Some(
                if summary.outcome == "cancelled" {
                    "interrupted"
                } else {
                    "failed"
                }
                .into(),
            );
            return Ok(summary);
        }
        return Err(error);
    }
    let mut summary = output.summary(&thread, &turn)?;
    summary.cleanup_confirmed = cleanup_confirmed;
    if parked {
        summary.native_status = Some("waiting_for_tool".into());
    }
    let completed = output.completed()?;
    if let Some(reuse) = &mut reuse {
        if summary.outcome == "completed"
            && request.reuse_allowed
            && !history_changed
            && (!output.has_calls() || parked)
            && native_context.reusable()
            && (crate::native_cache::enabled()
                || (request.tools.is_empty() && reuse::text_history(history)))
            && history.len() + completed["response"]["output"].as_array().unwrap().len() <= 1024
        {
            *reuse.session = Some(reuse::Session::new(
                thread,
                directory.take().unwrap(),
                reuse.access,
                request,
                history,
                completed["response"]["output"].as_array().unwrap(),
            )?);
            reuse.session.as_mut().unwrap().default_effort = default_effort;
            reuse.session.as_mut().unwrap().default_summary = default_summary;
            reuse.session.as_mut().unwrap().default_service_tier = default_service_tier;
            reuse.session.as_mut().unwrap().native_checkpoint = native_context.checkpoint();
            if parked {
                reuse.session.as_mut().unwrap().calls = callbacks;
            }
        }
    }
    if let Err(error) = send(&outgoing, completed, &cancel).await {
        // Terminal delivery can fail after all native events were consumed.
        // Retain their usage/cleanup evidence and revoke any provisional session.
        if let Some(reuse) = &mut reuse {
            *reuse.session = None;
        }
        if parked {
            summary.cleanup_confirmed = turn_guard.cleanup().await;
            rpc.clear_thread_tool_callbacks(&turn_guard.thread);
            if summary.cleanup_confirmed {
                let owner = turn_guard.disarm();
                if let Some(continuation) = reuse.as_mut().and_then(|r| r.continuation.as_mut()) {
                    continuation.owner = owner;
                }
            }
            // Cleanup may race natural completion; do not invent its status.
            summary.native_status = None;
        }
        summary.outcome = if crate::execution::cancelled(&error.code) {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        summary.error_code = Some(error.code);
        summary.error_message = Some(error.message);
        return Ok(summary);
    }
    if parked {
        let continuation = reuse
            .as_mut()
            .and_then(|r| r.continuation.as_mut())
            .unwrap();
        continuation.waiting = Some(continuation::Waiting::new(turn_guard));
    }
    Ok(summary)
}
