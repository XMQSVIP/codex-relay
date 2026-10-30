//! Save desired settings and immediately restart this account, cancelling active work.
use super::AdminState;
use crate::{app_server_proxy::ProxyConfig, error::Result};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProxyInput {
    revision: u64,
    config: ProxyConfig,
}

pub(super) async fn set_proxy(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<ProxyInput>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    state.profiles.require_no_login(id)?;
    if let Some(generation) =
        state
            .operations
            .set_account_proxy(id, input.revision, input.config)?
    {
        let task_state = state.clone();
        state.tasks.spawn(async move {
            apply(task_state, id, generation).await;
        });
    }
    Ok(Json(state.operations_snapshot()))
}

pub(super) async fn apply(state: Arc<AdminState>, id: Uuid, generation: u64) {
    let _setup = tokio::select! {
        _ = state.shutdown.cancelled() => return,
        setup = state.account_setup.lock() => setup,
    };
    if !state.operations.proxy_current(id, generation) {
        return;
    }
    // Never wait for a busy slot here: stop() must cancel the request that holds it first.
    let running = state.profiles.has_worker(id).await;
    let Some(restart) = state.operations.begin_proxy_switch(id, generation, running) else {
        return;
    };
    let result = async {
        state.profiles.stop(id).await?;
        if state.profiles.is_api_key(id).await? {
            state.upstream_connections.invalidate(id).await;
            return Ok(());
        }
        // The account remains behind the admission barrier while first detection
        // completes. A failed detection is advisory, not a restart failure.
        tokio::select! {
            _ = state.shutdown.cancelled() => return Ok(()),
            _ = state.operations.resolve_location(id, false) => {},
        }
        if restart && !state.shutdown.is_cancelled() {
            state.profiles.start(id).await?;
        }
        Ok(())
    }
    .await;
    state.operations.finish_proxy_switch(id, generation, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server_proxy::ProxyMode;
    use std::time::Duration;

    async fn wait_state(state: &AdminState, expected: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.operations.snapshot()["accounts"][0]["proxyStatus"]["state"] != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cleanup_failure_blocks_admission_and_retry_recovers_without_starting_stopped_accounts()
    {
        let dir = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let account = state.profiles.create("cleanup retry").await.unwrap();
        state
            .operations
            .register_accounts(&[(account.id, account.name.clone())])
            .unwrap();
        // A missing profile makes real cleanup fail without using a native installation.
        state.profiles.delete(account.id).await.unwrap();
        let config = ProxyConfig {
            mode: ProxyMode::Direct,
            url: String::new(),
        };
        let input = ProxyInput {
            revision: state.operations.snapshot()["revision"].as_u64().unwrap(),
            config: config.clone(),
        };
        let _ = set_proxy(State(state.clone()), Path(account.id), Json(input))
            .await
            .unwrap();
        wait_state(&state, "failed").await;
        assert!(state.operations.proxy_blocked(account.id));
        assert_eq!(
            state.operations.snapshot()["accounts"][0]["proxy"]["mode"],
            "direct"
        );
        state
            .profiles
            .create_local_once(&account.name, account.id)
            .await
            .unwrap();
        let input = ProxyInput {
            revision: state.operations.snapshot()["revision"].as_u64().unwrap(),
            config,
        };
        let _ = set_proxy(State(state.clone()), Path(account.id), Json(input))
            .await
            .unwrap();
        wait_state(&state, "applied").await;
        assert!(!state.operations.proxy_blocked(account.id));
        assert!(!state.profiles.has_worker(account.id).await);
    }

    #[tokio::test]
    async fn saving_proxy_cancels_active_inference_immediately_and_only_for_this_account() {
        let dir = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let a = state.profiles.create("A").await.unwrap();
        let b = state.profiles.create("B").await.unwrap();
        state
            .operations
            .register_accounts(&[(a.id, a.name), (b.id, b.name)])
            .unwrap();
        let cancelled = tokio_util::sync::CancellationToken::new();
        let untouched = tokio_util::sync::CancellationToken::new();
        let registration = state
            .profiles
            .register_inference(a.id, cancelled.clone())
            .unwrap();
        let other = state
            .profiles
            .register_inference(b.id, untouched.clone())
            .unwrap();
        let active = state
            .operations
            .begin(Some(a.id), None, "inference", "test", true)
            .unwrap();
        let input = ProxyInput {
            revision: state.operations.snapshot()["revision"].as_u64().unwrap(),
            config: ProxyConfig {
                mode: ProxyMode::Direct,
                url: String::new(),
            },
        };
        let _ = set_proxy(State(state.clone()), Path(a.id), Json(input))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), cancelled.cancelled())
            .await
            .unwrap();
        wait_state(&state, "applied").await;
        assert!(!untouched.is_cancelled());
        assert_eq!(state.operations.snapshot()["accounts"][0]["active"], 1);
        active.complete(Ok(())).unwrap();
        drop((registration, other));
    }
}
