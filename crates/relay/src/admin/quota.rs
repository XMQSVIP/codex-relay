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

const ACTIVE_REFRESH_MS: u64 = 5 * 60_000;
const IDLE_REFRESH_MS: u64 = 15 * 60_000;
const IDLE_AFTER_MS: u64 = 30 * 60_000;
const TRANSIENT_RETRY_MS: u64 = 60_000;

fn refresh_due(quota: &Value, now: u64, recently_used: impl FnOnce() -> bool) -> bool {
    let Some(last) = quota["lastAttemptAt"].as_u64().filter(|at| *at > 0) else {
        return true;
    };
    let age = now.saturating_sub(last);
    // Retry only quota reads after transient failures; the shared refresh
    // limiter and credential-change serialization still apply to every attempt.
    if quota["status"] == "error"
        && matches!(
            quota["errorCode"].as_str(),
            Some("RPC_TIMEOUT" | "QUOTA_TIMEOUT" | "RPC_CLOSED")
        )
        && age >= TRANSIENT_RETRY_MS
    {
        return true;
    }
    if age >= IDLE_REFRESH_MS
        || (quota["blocked"] == true
            && quota["retryAt"].as_u64().is_some_and(|at| at <= now)
            && age >= 30_000)
    {
        return true;
    }
    age >= ACTIVE_REFRESH_MS && recently_used()
}

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
    Ok(Json(state.operations.quota(id)?))
}
pub(super) async fn refresh(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    refresh_one(&state, id).await?;
    Ok(Json(state.operations.quota(id)?))
}
pub(super) async fn consume_reset(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<crate::operations::quota::ResetRequest>,
) -> Result<Json<Value>> {
    if state.profiles.is_api_key(id).await? {
        return Err(Error::new(
            "INVALID_INPUT",
            "API Key 账户不适用 ChatGPT 额度重置。",
        ));
    }
    request.validate()?;
    let operation_state = state.clone();
    state.tasks.spawn(async move {
        let state = operation_state;
        let slot = state.quota_refreshes.slots.lock().await.entry(id).or_default().clone();
        let mut last = slot.lock().await;
        let _permit = state.quota_refreshes.permits.acquire().await.map_err(|_| Error::new("SERVICE_DRAINING", "服务正在退出。"))?;
        let _setup = state.account_setup.lock().await;
        let provider = state.operations.configured_provider(id).ok_or_else(|| Error::new("INFERENCE_DISABLED", "请先配置账户接入。"))?;
        if !provider.auth.starts_with(crate::oauth::AUTH) && provider.auth != "chatgpt" {
            return Err(Error::new("RESET_UNSUPPORTED", "此提供方不支持 ChatGPT 重置卡。"));
        }
        let lease = state.operations.begin(Some(id), None, "management", "quota_reset", false)?;
        state.operations.prepare_reset(id, &request)?;
        let result = state.profiles.consume_quota_reset(id, &state.operations, &provider, &request).await;
        record_reset(&state, id, &result)?;
        *last = crate::now_ms();
        match result {
            Ok((outcome, refreshed)) => {
                lease.complete(Ok(()))?;
                Ok(Json(serde_json::json!({"outcome":outcome,"refreshed":refreshed,"quota":state.operations.quota(id)?})))
            }
            Err(error) => { let _ = lease.complete::<()>(Err(error.clone())); Err(error) }
        }
    }).await.map_err(|_| Error::new("RESET_UNCERTAIN", "连接中断，请使用原请求核验结果。"))?
}
pub(super) async fn read_automation(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut value = serde_json::to_value(state.operations.reset_automation(id)?)?;
    value.as_object_mut().unwrap().remove("identity");
    value.as_object_mut().unwrap().remove("settled");
    Ok(Json(value))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AutomationUpdate {
    enabled: bool,
    revision: u64,
}
pub(super) async fn update_automation(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<AutomationUpdate>,
) -> Result<Json<Value>> {
    if state.profiles.is_api_key(id).await? {
        return Err(Error::new(
            "INVALID_INPUT",
            "API Key 账户不适用 ChatGPT 额度重置。",
        ));
    }
    state
        .operations
        .set_reset_automation(id, input.enabled, input.revision)?;
    read_automation(State(state), Path(id)).await
}
fn record_reset(state: &AdminState, id: Uuid, result: &Result<(String, bool)>) -> Result<()> {
    match result {
        Ok((outcome, _)) => state.operations.finish_reset(id, outcome, true),
        Err(error) => state.operations.finish_reset(id, &error.code, false),
    }
}
async fn auto_reset(state: &Arc<AdminState>, id: Uuid) -> Result<()> {
    if state.profiles.is_api_key(id).await? {
        return Ok(());
    }
    let slot = state
        .quota_refreshes
        .slots
        .lock()
        .await
        .entry(id)
        .or_default()
        .clone();
    let mut last = slot.lock().await;
    let _permit = state
        .quota_refreshes
        .permits
        .acquire()
        .await
        .map_err(|_| Error::new("SERVICE_DRAINING", "服务正在退出。"))?;
    let _setup = state.account_setup.lock().await;
    if !state.operations.reset_auto_due(id, crate::now_ms()) {
        return Ok(());
    }
    let provider = state
        .operations
        .configured_provider(id)
        .ok_or_else(|| Error::new("INFERENCE_DISABLED", "请先配置账户接入。"))?;
    let lease = state
        .operations
        .begin(Some(id), None, "management", "quota_reset_auto", false)?;
    let result = async {
        // Re-read live inventory before planning; never consume from a stale UI snapshot.
        if let Err(error) = state
            .profiles
            .read_quota(id, &state.operations, &provider)
            .await
        {
            state
                .operations
                .quota_read_failed(id, &provider, &error.code)?;
            state.operations.defer_auto_reset(id)?;
            return Err(error);
        }
        let Some(request) = state.operations.plan_auto_reset(id, crate::now_ms())? else {
            return state.operations.defer_auto_reset(id);
        };
        state.operations.prepare_reset(id, &request)?;
        let result = state
            .profiles
            .consume_quota_reset(id, &state.operations, &provider, &request)
            .await;
        record_reset(state, id, &result)?;
        result.map(|_| ())
    }
    .await;
    *last = crate::now_ms();
    lease.complete(result)
}
pub(super) async fn refresh_one(state: &Arc<AdminState>, id: Uuid) -> Result<()> {
    if state.profiles.is_api_key(id).await? {
        return Ok(());
    }
    let slot = state
        .quota_refreshes
        .slots
        .lock()
        .await
        .entry(id)
        .or_default()
        .clone();
    let mut last = slot.lock().await;
    if crate::now_ms().saturating_sub(*last) < 30_000 {
        return Ok(());
    }
    if state
        .operations
        .quota(id)
        .ok()
        .and_then(|q| q["lastAttemptAt"].as_u64())
        .is_some_and(|t| crate::now_ms().saturating_sub(t) < 30_000)
    {
        return Ok(());
    }
    let _permit = state
        .quota_refreshes
        .permits
        .acquire()
        .await
        .map_err(|_| Error::new("SERVICE_DRAINING", "服务正在退出。"))?;
    let _setup = state.account_setup.lock().await;
    let provider = state
        .operations
        .configured_provider(id)
        .ok_or_else(|| Error::new("INFERENCE_DISABLED", "请先配置账号接入。"))?;
    let lease = state
        .operations
        .begin(Some(id), None, "management", "quota_refresh", false)?;
    *last = crate::now_ms();
    let result = state
        .profiles
        .read_quota(id, &state.operations, &provider)
        .await;
    if let Err(error) = &result {
        state
            .operations
            .quota_read_failed(id, &provider, &error.code)?;
    }
    lease.complete(result)
}
pub(super) fn start(state: &Arc<AdminState>) {
    let weak = Arc::downgrade(state);
    let cancel = state.shutdown.clone();
    state.tasks.spawn(async move {
        loop {
            tokio::select! { _=cancel.cancelled()=>break, _=tokio::time::sleep(std::time::Duration::from_secs(30))=>{} }
            let Some(state)=weak.upgrade() else { break; };
            let snapshot=state.operations.snapshot();
            for account in snapshot["accounts"].as_array().into_iter().flatten() {
                if account["enabled"]!=true || !account["route"].is_object() { continue; }
                let Some(id)=account["id"].as_str().and_then(|v|Uuid::parse_str(v).ok()) else { continue; };
                let q=state.operations.quota(id).unwrap_or_default();
                let now=crate::now_ms();
                if state.operations.reset_auto_due(id, now) {
                    // Do not cancel an in-flight redemption; its durable key survives shutdown.
                    let _ = auto_reset(&state, id).await;
                    if cancel.is_cancelled() { return; }
                    continue;
                }
                if state.operations.subscription_due(id, now, false) {
                    // Due subscription reads also use a separate backend; ongoing
                    // inference must not indefinitely postpone their refresh.
                    super::subscription::schedule(&state, id);
                }
                // Quota reads use a separate backend and do not reserve inference
                // capacity. A continuously busy account must still be refreshed.
                if !refresh_due(&q, now, || state.operations.account_recently_used(id, now.saturating_sub(IDLE_AFTER_MS)).unwrap_or(true)) { continue; }
                tokio::select! { _=cancel.cancelled()=>return, _=refresh_one(&state,id)=>{} }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_refresh_uses_five_minutes_when_active_and_fifteen_when_idle() {
        let last = 10_000_000;
        let quota = json!({"lastAttemptAt": last});
        assert!(!refresh_due(&quota, last + ACTIVE_REFRESH_MS - 1, || true));
        assert!(refresh_due(&quota, last + ACTIVE_REFRESH_MS, || true));
        assert!(!refresh_due(&quota, last + ACTIVE_REFRESH_MS, || false));
        assert!(!refresh_due(&quota, last + IDLE_REFRESH_MS - 1, || false));
        assert!(refresh_due(&quota, last + IDLE_REFRESH_MS, || false));
    }

    #[test]
    fn quota_refresh_preserves_initial_read_and_recovery_checks() {
        let last = 10_000_000;
        assert!(refresh_due(&json!({}), last, || false));
        let mut quota = json!({"lastAttemptAt":last,"blocked":true,"retryAt":last+30_000});
        assert!(!refresh_due(&quota, last + 29_999, || false));
        assert!(refresh_due(&quota, last + 30_000, || false));
        quota["retryAt"] = json!(last + 60_000);
        assert!(!refresh_due(&quota, last + 30_000, || false));
    }

    #[test]
    fn transient_quota_failures_retry_before_the_normal_idle_interval() {
        let last = 10_000_000;
        for code in ["RPC_TIMEOUT", "QUOTA_TIMEOUT", "RPC_CLOSED"] {
            let quota = json!({"lastAttemptAt":last,"status":"error","errorCode":code});
            assert!(!refresh_due(&quota, last + TRANSIENT_RETRY_MS - 1, || {
                false
            }));
            assert!(refresh_due(&quota, last + TRANSIENT_RETRY_MS, || false));
        }
        for code in [
            "OAUTH_NOT_SIGNED_IN",
            "OAUTH_TOKEN_EXPIRED",
            "ROUTE_CHANGED",
            "RPC_REMOTE",
        ] {
            let quota = json!({"lastAttemptAt":last,"status":"error","errorCode":code});
            assert!(!refresh_due(&quota, last + TRANSIENT_RETRY_MS, || false));
        }
        let recovered = json!({"lastAttemptAt":last,"status":"supported","errorCode":null});
        assert!(!refresh_due(&recovered, last + TRANSIENT_RETRY_MS, || {
            false
        }));
    }
}
