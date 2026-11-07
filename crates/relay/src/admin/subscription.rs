use super::AdminState;
use crate::error::{Error, Result};
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

pub(super) struct Refreshes {
    slots: Mutex<HashMap<Uuid, Arc<Mutex<u64>>>>,
    permits: Semaphore,
}
impl Default for Refreshes {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            permits: Semaphore::new(2),
        }
    }
}

pub(super) async fn read(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(state.operations.subscription(id)?))
}
pub(super) async fn refresh(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    state.operations.subscription(id)?;
    // Disconnecting the browser must not cancel cleanup or allow duplicate requests.
    let task_state = state.clone();
    state
        .tasks
        .spawn(async move { refresh_one(&task_state, id, true).await })
        .await
        .map_err(|_| Error::new("SERVICE_SHUTDOWN", "订阅查询已结束。"))??;
    read(State(state), Path(id)).await
}
pub(super) fn schedule(state: &Arc<AdminState>, id: Uuid) {
    let state = state.clone();
    state.tasks.clone().spawn(async move {
        let _ = refresh_one(&state, id, false).await;
    });
}
pub(super) async fn refresh_one(state: &Arc<AdminState>, id: Uuid, manual: bool) -> Result<()> {
    if !state
        .operations
        .subscription_due(id, crate::now_ms(), manual)
    {
        return Ok(());
    }
    let slot = state
        .subscription_refreshes
        .slots
        .lock()
        .await
        .entry(id)
        .or_default()
        .clone();
    let mut last = slot.lock().await;
    let now = crate::now_ms();
    if now.saturating_sub(*last) < crate::operations::subscription::MANUAL_INTERVAL_MS
        || !state.operations.subscription_due(id, now, manual)
    {
        return Ok(());
    }
    let _permit = state
        .subscription_refreshes
        .permits
        .acquire()
        .await
        .map_err(|_| Error::new("SERVICE_SHUTDOWN", "服务正在退出。"))?;
    let _setup = state.account_setup.lock().await;
    if !state
        .operations
        .subscription_due(id, crate::now_ms(), manual)
    {
        return Ok(());
    }
    state.profiles.require_no_login(id)?;
    if state.operations.proxy_blocked(id) {
        return Err(Error::new("BUSY", "账户代理正在切换，请稍后刷新订阅。"));
    }
    let provider = state
        .operations
        .configured_provider(id)
        .ok_or_else(|| Error::new("INFERENCE_DISABLED", "请先检测账户接入。"))?;
    // The separate backend only reads subscription metadata. Keep account_setup
    // through cleanup to serialize credential changes without blocking inference.
    let lease =
        state
            .operations
            .begin(Some(id), None, "management", "subscription_refresh", false)?;
    *last = crate::now_ms();
    let result = state
        .profiles
        .refresh_subscription(id, &state.operations, &provider, manual)
        .await;
    if let Err(error) = &result {
        if matches!(
            error.code.as_str(),
            "OAUTH_NOT_SIGNED_IN"
                | "PROVIDER_AUTH_MISSING"
                | "OAUTH_IDENTITY_MISMATCH"
                | "OAUTH_IDENTITY_CHANGED"
                | "ROUTE_CHANGED"
        ) {
            state.operations.clear_subscription(id)?;
        }
        state
            .operations
            .subscription_setup_failed(id, &provider, *last, &error.code)?;
    }
    lease.complete(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn subscription_api_reads_cache_and_coalesces_refreshes_without_starting_workers() {
        let dir = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let account = state
            .profiles
            .create_native_config("fixture")
            .await
            .unwrap();
        let id = account.id;
        let ops = &state.operations;
        ops.register_accounts(&[(id, account.name)]).unwrap();
        let provider = serde_json::from_value(json!({"id":"openai","model":"test","baseUrl":"http://127.0.0.1:9/backend-api","auth":"native-chatgpt:fixture","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
        ops.set_inference_route(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            Some(provider),
        )
        .unwrap();
        // No installation/credentials exist. A list poll must not try to start one.
        let listed = state.profiles.list().await;
        assert!(listed[0].pid.is_none());
        assert!(listed[0].plan_type.is_none());
        let provider = ops.configured_provider(id).unwrap();
        ops.observe_subscription(id, &provider, "fixture-identity", Some("pro"))
            .unwrap();
        let now = crate::now_ms();
        ops.subscription_attempt(id, &provider, "fixture-identity", now, true)
            .unwrap();
        ops.finish_subscription(
            id,
            &provider,
            "fixture-identity",
            now,
            Ok(crate::operations::subscription::Reading {
                starts_at: None,
                expires_at: now + 86400000,
                will_renew: Some(true),
            }),
        )
        .unwrap();
        let app = super::super::router(state.clone(), dir.path().join("web"));
        for suffix in ["", "/refresh", "/refresh"] {
            let request = Request::builder()
                .method(if suffix.is_empty() { "GET" } else { "POST" })
                .uri(format!("/api/accounts/{id}/subscription{suffix}"))
                .header("host", "127.0.0.1:4317")
                .header("origin", "http://127.0.0.1:4317")
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert!(response.status().is_success());
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                    .unwrap();
            assert_eq!(body["planType"], "pro");
            assert!(!body.to_string().contains("fixture-identity"));
        }
        let (a, b) = tokio::join!(refresh_one(&state, id, true), refresh_one(&state, id, true));
        a.unwrap();
        b.unwrap();
        assert!(state.profiles.list().await[0].pid.is_none());
        assert_eq!(ops.subscription(id).unwrap()["lastAttemptAt"], now);
    }
}
