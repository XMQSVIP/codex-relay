//! One user-facing service switch; worker controls remain available for maintenance.

use super::{AdminState, routing::inspect_access};
use crate::{error::Result, operations::Lease};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(super) type PendingStops =
    std::collections::HashMap<Uuid, (Uuid, tokio_util::sync::CancellationToken)>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActivationInput {
    pub(super) revision: u64,
    pub(super) enabled: bool,
}

pub(super) async fn set_activation(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<ActivationInput>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    activate_locked(&state, id, input).await
}
pub(super) async fn activate_locked(
    state: &Arc<AdminState>,
    id: Uuid,
    input: ActivationInput,
) -> Result<Json<Value>> {
    let configured = state.operations.activation_provider(id, input.revision)?;
    if input.enabled {
        state.profiles.require_no_login(id)?;
    }
    let inspected = if input.enabled && configured.is_none() {
        state.operations.require_account_idle(id)?;
        Some(inspect_access(state, id).await?)
    } else {
        None
    };
    state.operations.set_account_activation(
        id,
        input.revision,
        input.enabled,
        inspected.as_ref().map(|(provider, _)| provider.clone()),
    )?;
    if let Some((_, caps)) = inspected {
        state.operations.publish_capabilities(id, caps)?;
    }
    if input.enabled {
        let task_state = state.clone();
        state.tasks.spawn(async move {
            let _ = super::quota::refresh_one(&task_state, id).await;
        });
    }
    if let Some((_, cancel)) = state.account_stops.lock().unwrap().remove(&id) {
        cancel.cancel();
    }
    if input.enabled {
        schedule_start(state, id);
    }
    let mut warning = None;
    if !input.enabled {
        if state.operations.disabled_account_idle(id) {
            warning = stop_disabled(state, id)
                .await
                .err()
                .map(|_| "账户已停用，但实例停止失败。请在操作区重试停止实例。");
        } else {
            let generation = Uuid::new_v4();
            let cancel = state.shutdown.child_token();
            state
                .account_stops
                .lock()
                .unwrap()
                .insert(id, (generation, cancel.clone()));
            let task_state = state.clone();
            state.tasks.spawn(async move {
                loop {
                    let ready = tokio::select! {
                        _ = cancel.cancelled() => false,
                        ready = task_state.operations.wait_for_disabled_account_idle(id) => ready,
                    };
                    if !ready {
                        break;
                    }
                    let _setup = tokio::select! {
                        _ = cancel.cancelled() => break,
                        setup = task_state.account_setup.lock() => setup,
                    };
                    // An enable or a maintenance operation may have won the setup lock.
                    if cancel.is_cancelled() {
                        break;
                    }
                    if !task_state.operations.disabled_account_idle(id) {
                        continue;
                    }
                    if let Err(error) = stop_disabled(&task_state, id).await {
                        eprintln!("account_disable_stop_failed: {id} {}", error.code);
                    }
                    break;
                }
                let mut stops = task_state.account_stops.lock().unwrap();
                if stops
                    .get(&id)
                    .is_some_and(|(current, _)| *current == generation)
                {
                    stops.remove(&id);
                }
            });
        }
    }
    Ok(Json(
        json!({"operations":state.operations_snapshot(),"warning":warning}),
    ))
}

/// Start one worker for each configured account after the service is listening.
pub(super) fn start_enabled(state: &Arc<AdminState>) {
    for account in state.operations.snapshot()["accounts"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if account["enabled"] == true
            && account["route"].is_object()
            && let Some(id) = account["id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
        {
            schedule_start(state, id);
        }
    }
}

pub(super) fn schedule_start(state: &Arc<AdminState>, id: Uuid) {
    super::subscription::schedule(state, id);
    let weak = Arc::downgrade(state);
    let cancel = state.shutdown.clone();
    state.tasks.spawn(async move {
        loop {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let result = ensure_started(&state, id).await;
            match result {
                Ok(()) => return,
                Err(error) if error.code == "CONCURRENCY_LIMIT" || error.code == "BUSY" => {}
                Err(error) => {
                    eprintln!("account_auto_start_failed: {id} {}", error.code);
                    return;
                }
            }
            drop(state);
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            }
        }
    });
}

async fn ensure_started(state: &Arc<AdminState>, id: Uuid) -> Result<()> {
    let setup = tokio::select! {
        _ = state.shutdown.cancelled() => return Ok(()),
        setup = state.account_setup.lock() => setup,
    };
    if state.shutdown.is_cancelled()
        || state.operations.account_scheduling_enabled(id).ok() != Some(true)
        || state.operations.configured_provider(id).is_none()
        || state.profiles.is_api_key(id).await.unwrap_or(true)
        || state.profiles.has_ready_response(id).await
    {
        return Ok(());
    }
    let lease = state
        .operations
        .begin_account_maintenance(id, "management", "auto_start")?;
    // The maintenance lease now protects this account; unrelated accounts need not wait for native I/O.
    drop(setup);
    lease.complete(state.profiles.prewarm_response(id, &state.operations).await)
}

pub(super) fn maintain_enabled(state: &Arc<AdminState>) {
    let weak = Arc::downgrade(state);
    let shutdown = state.shutdown.clone();
    state.tasks.spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {},
            }
            let Some(state) = weak.upgrade() else {
                break;
            };
            let ids: Vec<_> = state.operations.snapshot()["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["enabled"] == true)
                .filter_map(|a| a["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()))
                .collect();
            for id in ids {
                if shutdown.is_cancelled() {
                    break;
                }
                if let Err(error) = ensure_started(&state, id).await {
                    if !matches!(error.code.as_str(), "CONCURRENCY_LIMIT" | "BUSY") {
                        eprintln!("account_standby_failed: {id} {}", error.code);
                    }
                }
            }
        }
    });
}

async fn stop_disabled(state: &AdminState, id: Uuid) -> Result<()> {
    state.upstream_connections.invalidate(id).await;
    let lease = state
        .operations
        .begin(Some(id), None, "management", "stop_disabled", false)?;
    lease.complete(state.profiles.stop(id).await)
}

impl AdminState {
    // Disabled accounts still support explicit login, inspection and worker debugging.
    // Paused-account maintenance still reserves capacity atomically on admission.
    pub(super) async fn account_lease(&self, id: Uuid, kind: &str, action: &str) -> Result<Lease> {
        self.profiles.require_no_login(id)?;
        if self.operations.proxy_blocked(id) {
            return Err(crate::error::Error::new(
                "BUSY",
                "账户代理正在切换，请稍后重试。",
            ));
        }
        if !self.operations.account_scheduling_enabled(id)? {
            self.operations.require_account_idle(id)?;
            self.operations.begin_account_maintenance(id, kind, action)
        } else {
            self.operations.acquire(Some(id), None, kind, action).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn configured(state: &AdminState) -> Uuid {
        let account = state.profiles.create("auto start fixture").await.unwrap();
        state
            .operations
            .register_accounts(&[(account.id, account.name)])
            .unwrap();
        let provider = serde_json::from_value(json!({"id":"fixture","model":"test-model","baseUrl":"http://127.0.0.1:9/v1","auth":"none","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
        let rev = state.operations.snapshot()["revision"].as_u64().unwrap();
        state
            .operations
            .set_inference_route(account.id, rev, Some(provider))
            .unwrap();
        account.id
    }

    #[tokio::test]
    async fn disabling_before_start_wins_and_never_launches_a_worker() {
        let dir = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let id = configured(&state).await;
        let setup = state.account_setup.lock().await;
        schedule_start(&state, id);
        let rev = state.operations.snapshot()["revision"].as_u64().unwrap();
        state
            .operations
            .set_account_activation(id, rev, false, None)
            .unwrap();
        drop(setup);
        state.tasks.close();
        state.tasks.wait().await;
        assert!(!state.profiles.has_worker(id).await);
        assert_eq!(state.operations.snapshot()["active"], 0);
        assert_eq!(
            state.operations.statistics(Default::default()).unwrap()["summary"]["total"],
            0
        );
    }

    #[tokio::test]
    #[ignore = "requires installed native app-server; starts isolated signed-out workers without inference"]
    async fn signed_out_accounts_do_not_keep_unusable_standby_workers() {
        // Run this opt-in test alone: the production installation is prepared
        // relative to the workspace, while Cargo runs tests in the crate folder.
        std::env::set_current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("accounts");
        let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
            .await
            .unwrap();
        let id = configured(&state).await;
        let rev = state.operations.snapshot()["revision"].as_u64().unwrap();
        let _ = activate_locked(
            &state,
            id,
            ActivationInput {
                revision: rev,
                enabled: true,
            },
        )
        .await
        .unwrap();
        state.tasks.close();
        state.tasks.wait().await;
        let first = state.profiles.list().await;
        assert!(first[0].pid.is_none());
        assert_eq!(first[0].clients.len(), 0);
        schedule_start(&state, id);
        schedule_start(&state, id);
        state.tasks.close();
        state.tasks.wait().await;
        assert!(state.profiles.list().await[0].pid.is_none());
        state.shutdown.cancel();
        state.profiles.shutdown().await;
        drop(state);

        let state = AdminState::new_with_network(4317, root, None, vec![])
            .await
            .unwrap();
        start_enabled(&state);
        state.tasks.close();
        state.tasks.wait().await;
        assert!(state.profiles.list().await[0].pid.is_none());
        assert_eq!(state.profiles.list().await[0].clients.len(), 0);
        assert_eq!(
            state.operations.snapshot()["accounts"][0]["maxConcurrency"],
            5
        );
        let rev = state.operations.snapshot()["revision"].as_u64().unwrap();
        let _ = activate_locked(
            &state,
            id,
            ActivationInput {
                revision: rev,
                enabled: false,
            },
        )
        .await
        .unwrap();
        state.tasks.close();
        state.tasks.wait().await;
        assert!(!state.profiles.has_worker(id).await);
        assert_eq!(
            state.operations.statistics(Default::default()).unwrap()["summary"]["inferenceRequests"],
            0
        );
        state.shutdown.cancel();
        state.profiles.shutdown().await;
    }
}
