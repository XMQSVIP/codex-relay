//! Normalized quota evidence. No credentials or upstream response documents are stored.
use super::{AccountPolicy, Inner, Operations, runtime, storage};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;
mod benefits;
mod usage;
pub use benefits::{
    Credits, QuotaReading, ResetCredits, ResetRequest, normalize_reading, reset_context,
};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub bucket: String,
    pub window: String,
    pub used_percent: Option<f64>,
    pub duration_mins: Option<u64>,
    pub resets_at: Option<u64>,
    pub reached: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSnapshot {
    #[serde(default)]
    pub credits: Option<Credits>,
    #[serde(default)]
    pub reset_credits: Option<ResetCredits>,
    pub status: String,
    pub checked_at: u64,
    #[serde(default)]
    pub attempted_at: u64,
    pub windows: Vec<QuotaWindow>,
    pub error_code: Option<String>,
    pub blocked: bool,
    pub retry_at: Option<u64>,
    pub candidate_at: Option<u64>,
    pub probe_at: u64,
    #[serde(default)]
    pub probe_request: Option<Uuid>,
    pub identity: String,
    pub generation: String,
    #[serde(default)]
    pub verified: bool,
}

pub fn identity(account: Uuid, credential: Option<&crate::oauth::Credential>) -> String {
    credential
        .and_then(|c| {
            c.identity
                .subject
                .as_ref()
                .map(|_| crate::oauth::fingerprint(&c.identity))
        })
        .unwrap_or_else(|| format!("account:{account}"))
}

pub fn normalize(raw: &Value) -> Result<Vec<QuotaWindow>> {
    let buckets: Vec<(&str, &Value)> = if let Some(map) = raw["rateLimitsByLimitId"].as_object() {
        map.iter().take(32).map(|(k, v)| (k.as_str(), v)).collect()
    } else if raw["rateLimits"].is_object() {
        vec![(
            raw["rateLimits"]["limitId"].as_str().unwrap_or("codex"),
            &raw["rateLimits"],
        )]
    } else {
        return Err(Error::new(
            "QUOTA_UNSUPPORTED",
            "原生版本未提供可识别的额度数据。",
        ));
    };
    let mut windows = vec![];
    for (bucket, value) in buckets {
        if bucket.len() > 128 {
            continue;
        }
        // Only the documented general Codex bucket can constrain the whole account.
        // Other metered buckets have no reliable model mapping in this gateway.
        let reason = value["rateLimitReachedType"]
            .as_str()
            .or_else(|| value["rateLimitReachedType"]["type"].as_str());
        let credits =
            value["credits"]["hasCredits"] == true || value["credits"]["unlimited"] == true;
        let reached = bucket == "codex"
            && reason.is_some_and(|s| {
                matches!(
                    s,
                    "credits_depleted"
                        | "workspace_limit_reached"
                        | "usage_limit_reached"
                        | "spend_control_limit_reached"
                ) || s == "rate_limit_reached" && !credits
            });
        let mut found = false;
        for name in ["primary", "secondary"] {
            let v = &value[name];
            if !v.is_object() {
                continue;
            }
            found = true;
            windows.push(QuotaWindow {
                bucket: bucket.into(),
                window: name.into(),
                used_percent: v["usedPercent"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0),
                duration_mins: v["windowDurationMins"].as_u64(),
                resets_at: v["resetsAt"].as_u64().and_then(|s| s.checked_mul(1000)),
                reached,
            });
        }
        if reached && !found {
            windows.push(QuotaWindow {
                bucket: bucket.into(),
                reached,
                ..Default::default()
            });
        }
    }
    Ok(windows)
}

pub(super) fn current<'a>(inner: &'a Inner, account: &AccountPolicy) -> Option<&'a QuotaSnapshot> {
    inner.runtime.quotas.get(&account.id).filter(|q| {
        account
            .route
            .as_ref()
            .is_some_and(|p| q.generation == runtime::fingerprint(p))
    })
}
pub(super) fn available(inner: &Inner, account: &AccountPolicy) -> bool {
    let Some(q) = current(inner, account).filter(|q| q.blocked) else {
        return true;
    };
    let now = crate::now_ms();
    q.verified
        && q.retry_at.is_some_and(|t| t <= now)
        && now.saturating_sub(q.probe_at) >= 300_000
        && !inner.active.values().any(|a| {
            a.account
                .and_then(|id| inner.runtime.quotas.get(&id))
                .is_some_and(|other| other.identity == q.identity)
        })
}
pub(super) fn claim(inner: &mut Inner, id: Option<Uuid>, request: Uuid) {
    let identity = id
        .and_then(|id| inner.runtime.quotas.get(&id))
        .filter(|q| q.blocked)
        .map(|q| q.identity.clone());
    if let Some(identity) = identity {
        for q in inner
            .runtime
            .quotas
            .values_mut()
            .filter(|q| q.identity == identity)
        {
            q.probe_at = crate::now_ms();
            q.probe_request = Some(request);
        }
        if persist(inner).is_err() {
            inner.storage_healthy = false;
        }
    }
}
fn persist(inner: &mut Inner) -> Result<()> {
    let tx = inner.db.transaction().map_err(storage)?;
    for (id, q) in &inner.runtime.quotas {
        tx.execute(
            "INSERT OR REPLACE INTO account_quotas VALUES(?1,?2)",
            rusqlite::params![id.to_string(), serde_json::to_string(q)?],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)?;
    inner.changed.notify_waiters();
    Ok(())
}
pub(super) fn load(db: &rusqlite::Connection) -> Result<HashMap<Uuid, QuotaSnapshot>> {
    let mut stmt = db
        .prepare("SELECT account_id,document FROM account_quotas")
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?;
    let mut result = HashMap::new();
    for row in rows {
        let (id, doc) = row.map_err(storage)?;
        if let (Ok(id), Ok(mut value)) = (
            Uuid::parse_str(&id),
            serde_json::from_str::<QuotaSnapshot>(&doc),
        ) {
            value.verified = false;
            value.probe_request = None;
            result.insert(id, value);
        }
    }
    Ok(result)
}
pub(super) fn view(inner: &Inner, account: &AccountPolicy) -> Value {
    current(inner,account).map_or_else(|| json!({"status":"unknown","windows":[],"blocked":false,"stale":true}), |q| {
        json!({"status":q.status,"checkedAt":if q.checked_at==0 {None}else{Some(q.checked_at)},"lastAttemptAt":q.attempted_at,"windows":usage::windows(inner, account.id, &q.windows, crate::now_ms()),"errorCode":q.error_code,
            "credits":q.credits,"resetCredits":q.reset_credits,"resetContext":if q.verified && !q.identity.starts_with("account:") {Some(reset_context(account.id, &q.identity, &q.generation))}else{None},"blocked":q.blocked,"retryAt":q.retry_at,"stale":!q.verified || q.status!="supported" || crate::now_ms().saturating_sub(q.checked_at)>300_000})
    })
}
impl Operations {
    pub(crate) fn require_reset_context(
        &self,
        id: Uuid,
        provider: &crate::provider::Provider,
        identity: &str,
        context: &str,
    ) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let generation = runtime::fingerprint(provider);
        let valid = !identity.starts_with("account:")
            && inner.runtime.quotas.get(&id).is_some_and(|q| {
                q.verified
                    && q.identity == identity
                    && q.generation == generation
                    && reset_context(id, identity, &generation) == context
            });
        if !valid {
            return Err(Error::new(
                "ROUTE_CHANGED",
                "账户身份或登录已变化，请刷新额度后重试。",
            ));
        }
        Ok(())
    }
    pub(crate) fn quota_identity(&self, id: Uuid, provider: &crate::provider::Provider) -> String {
        let inner = self.inner.lock().unwrap();
        inner
            .runtime
            .quotas
            .get(&id)
            .filter(|q| q.verified && q.generation == runtime::fingerprint(provider))
            .map(|q| q.identity.clone())
            .unwrap_or_else(|| format!("account:{id}"))
    }
    pub(crate) fn quota_read_failed(
        &self,
        id: Uuid,
        provider: &crate::provider::Provider,
        code: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let generation = runtime::fingerprint(provider);
        if !inner
            .settings
            .accounts
            .iter()
            .any(|a| a.id == id && a.route.as_ref() == Some(provider))
        {
            return Ok(());
        }
        let q = inner
            .runtime
            .quotas
            .entry(id)
            .or_insert_with(|| QuotaSnapshot {
                identity: format!("account:{id}"),
                generation: generation.clone(),
                ..Default::default()
            });
        if q.generation == generation {
            q.status = "error".into();
            q.error_code = Some(code.into());
            q.attempted_at = crate::now_ms();
        }
        persist(&mut inner)
    }
    pub fn quota(&self, id: Uuid) -> Result<Value> {
        let inner = self.inner.lock().unwrap();
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        Ok(view(&inner, account))
    }
    pub fn publish_quota(
        &self,
        id: Uuid,
        provider: &crate::provider::Provider,
        identity: String,
        result: Result<Vec<QuotaWindow>>,
    ) -> Result<()> {
        self.publish_quota_reading(
            id,
            provider,
            identity,
            result.map(|windows| QuotaReading {
                windows,
                ..Default::default()
            }),
        )
    }
    pub fn publish_quota_reading(
        &self,
        id: Uuid,
        provider: &crate::provider::Provider,
        identity: String,
        result: Result<QuotaReading>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let generation = runtime::fingerprint(provider);
        if !inner
            .settings
            .accounts
            .iter()
            .any(|a| a.id == id && a.route.as_ref() == Some(provider))
        {
            return Err(Error::new("ROUTE_CHANGED", "额度读取期间账户配置已变化。"));
        }
        if let Ok(reading) = &result {
            // Display metadata must never make an otherwise valid quota read fail.
            let _ = super::subscription::observe(
                &mut inner,
                id,
                provider,
                &identity,
                reading.plan_type.as_deref(),
                true,
            );
        }
        let now = crate::now_ms();
        let mut q = inner
            .runtime
            .quotas
            .get(&id)
            .filter(|q| q.generation == generation && q.identity == identity)
            .cloned()
            .unwrap_or_else(|| QuotaSnapshot {
                identity: identity.clone(),
                generation: generation.clone(),
                ..Default::default()
            });
        // Reuse known evidence for another route backed by the same confirmed identity.
        if !q.blocked {
            if let Some(other) = inner
                .runtime
                .quotas
                .values()
                .find(|q| q.identity == identity && q.blocked)
            {
                q.blocked = true;
                q.retry_at = other.retry_at;
                q.probe_at = other.probe_at;
            }
        }
        q.verified = true;
        q.attempted_at = now;
        match result {
            Ok(reading) => {
                if reading.complete || reading.credits.is_some() {
                    q.credits = reading.credits;
                }
                if reading.complete || reading.reset_credits.is_some() {
                    q.reset_credits = reading.reset_credits;
                }
                let mut windows = reading.windows;
                for old in &q.windows {
                    if old.window.is_empty()
                        && windows
                            .iter()
                            .any(|w| w.bucket == old.bucket && !w.window.is_empty())
                    {
                        continue;
                    }
                    if !windows
                        .iter()
                        .any(|w| w.bucket == old.bucket && w.window == old.window)
                    {
                        windows.push(old.clone());
                    }
                }
                q.status = "supported".into();
                q.error_code = None;
                q.checked_at = now;
                if windows.iter().any(|w| w.reached) {
                    q.blocked = true;
                    q.candidate_at = None;
                    q.retry_at = windows
                        .iter()
                        .filter(|w| w.reached)
                        .filter_map(|w| w.resets_at)
                        .max();
                } else if q.blocked
                    && windows.iter().any(|w| w.bucket == "codex")
                    && windows
                        .iter()
                        .filter(|w| w.bucket == "codex")
                        .all(|w| w.used_percent.is_some_and(|p| p < 100.0))
                {
                    if q.candidate_at
                        .is_some_and(|t| now.saturating_sub(t) >= 30_000)
                    {
                        q.blocked = false;
                        q.retry_at = None;
                        q.candidate_at = None;
                    } else {
                        q.candidate_at.get_or_insert(now);
                    }
                } else {
                    q.candidate_at = None;
                }
                q.windows = windows;
            }
            Err(e) => {
                q.status = if e.code == "QUOTA_UNSUPPORTED" {
                    "unsupported"
                } else {
                    "error"
                }
                .into();
                q.error_code = Some(e.code);
            }
        }
        for other in inner
            .runtime
            .quotas
            .values_mut()
            .filter(|v| v.identity == identity)
        {
            let generation = other.generation.clone();
            let verified = other.verified;
            *other = q.clone();
            other.generation = generation;
            other.verified = verified;
        }
        inner.runtime.quotas.insert(id, q);
        persist(&mut inner)
    }
    pub(crate) fn quota_success(&self, request: Uuid) {
        let mut inner = self.inner.lock().unwrap();
        let identity = inner
            .runtime
            .quotas
            .values()
            .find(|q| q.blocked && q.probe_request == Some(request))
            .map(|q| q.identity.clone());
        if let Some(identity) = identity {
            for q in inner
                .runtime
                .quotas
                .values_mut()
                .filter(|q| q.identity == identity)
            {
                q.blocked = false;
                q.retry_at = None;
                q.candidate_at = None;
            }
            if persist(&mut inner).is_err() {
                inner.storage_healthy = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider() -> crate::provider::Provider {
        serde_json::from_value(json!({"id":"test","model":"test","baseUrl":"http://127.0.0.1:9/v1","auth":"none","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap()
    }
    fn fixture() -> (
        tempfile::TempDir,
        std::sync::Arc<Operations>,
        Uuid,
        crate::provider::Provider,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let id = Uuid::new_v4();
        let p = provider();
        ops.register_accounts(&[(id, "test".into())]).unwrap();
        ops.set_inference_route(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            Some(p.clone()),
        )
        .unwrap();
        (dir, ops, id, p)
    }
    fn raw(percent: f64, reached: bool) -> Value {
        json!({"rateLimits":{"limitId":"codex","rateLimitReachedType":if reached {Some("rate_limit_reached")}else{None},"primary":{"usedPercent":percent,"windowDurationMins":300,"resetsAt":1}}})
    }
    #[test]
    fn benefits_follow_identity_and_partial_notifications_preserve_them() {
        let (_dir, ops, id, p) = fixture();
        let mut value = raw(20.0, false);
        value["rateLimits"]["credits"] = json!({"balance":"12.5","hasCredits":true});
        value["rateLimitResetCredits"] = json!({"availableCount":2,"credits":null});
        ops.publish_quota_reading(id, &p, "identity".into(), normalize_reading(&value, true))
            .unwrap();
        let view = ops.quota(id).unwrap();
        assert_eq!(view["credits"]["balance"], "12.5");
        let context = view["resetContext"].as_str().unwrap();
        assert!(
            ops.require_reset_context(id, &p, "identity", context)
                .is_ok()
        );
        assert!(
            ops.require_reset_context(id, &p, "different", context)
                .is_err()
        );
        ops.publish_quota_reading(
            id,
            &p,
            "identity".into(),
            normalize_reading(&raw(21.0, false), false),
        )
        .unwrap();
        assert_eq!(ops.quota(id).unwrap()["resetCredits"]["availableCount"], 2);
        ops.publish_quota_reading(
            id,
            &p,
            "different".into(),
            normalize_reading(&raw(22.0, false), false),
        )
        .unwrap();
        assert!(ops.quota(id).unwrap()["resetCredits"].is_null());
        assert!(
            ops.require_reset_context(id, &p, "different", context)
                .is_err()
        );
    }
    #[test]
    fn percentage_and_unknown_buckets_do_not_block() {
        assert!(!normalize(&raw(100.0, false)).unwrap()[0].reached);
        let mut value = raw(100.0, true);
        value["rateLimits"]["limitId"] = "other".into();
        assert!(!normalize(&value).unwrap()[0].reached);
        value = raw(100.0, true);
        value["rateLimits"]["credits"] = json!({"hasCredits":true});
        assert!(!normalize(&value).unwrap()[0].reached);
        assert!(normalize(&json!({})).is_err());
        assert_eq!(
            normalize(&raw(50.0, false)).unwrap()[0].resets_at,
            Some(1000)
        );
    }
    #[test]
    fn evidence_survives_restart_and_requires_two_observations() {
        let (dir, ops, id, p) = fixture();
        ops.publish_quota(id, &p, "identity".into(), normalize(&raw(100.0, true)))
            .unwrap();
        drop(ops);
        let ops = Operations::open(dir.path()).unwrap();
        assert_eq!(ops.quota(id).unwrap()["blocked"], true);
        {
            let inner = ops.inner.lock().unwrap();
            assert!(!available(&inner, &inner.settings.accounts[0]));
        }
        ops.publish_quota(id, &p, "identity".into(), normalize(&raw(1.0, false)))
            .unwrap();
        assert_eq!(ops.quota(id).unwrap()["blocked"], true);
        ops.inner
            .lock()
            .unwrap()
            .runtime
            .quotas
            .get_mut(&id)
            .unwrap()
            .candidate_at = Some(crate::now_ms() - 31_000);
        ops.publish_quota(id, &p, "identity".into(), normalize(&raw(2.0, false)))
            .unwrap();
        assert_eq!(ops.quota(id).unwrap()["blocked"], false);
    }
    #[test]
    fn shared_identity_probe_is_reserved_once_and_only_its_success_recovers() {
        let (_dir, ops, id, p) = fixture();
        let second = Uuid::new_v4();
        ops.register_accounts(&[(second, "second".into())]).unwrap();
        ops.set_inference_route(
            second,
            ops.snapshot()["revision"].as_u64().unwrap(),
            Some(p.clone()),
        )
        .unwrap();
        ops.publish_quota(id, &p, "same".into(), normalize(&raw(100.0, true)))
            .unwrap();
        ops.publish_quota(second, &p, "same".into(), normalize(&raw(100.0, true)))
            .unwrap();
        let request = Uuid::new_v4();
        {
            let mut inner = ops.inner.lock().unwrap();
            assert!(available(&inner, &inner.settings.accounts[0]));
            claim(&mut inner, Some(id), request);
            assert!(!available(&inner, &inner.settings.accounts[1]));
        }
        ops.quota_success(Uuid::new_v4());
        assert_eq!(ops.quota(id).unwrap()["blocked"], true);
        ops.quota_success(request);
        assert_eq!(ops.quota(second).unwrap()["blocked"], false);
    }
    #[test]
    fn credentials_and_unknown_data_cannot_reuse_or_clear_evidence() {
        let (_dir, ops, id, p) = fixture();
        ops.publish_quota(id, &p, "identity".into(), normalize(&raw(100.0, true)))
            .unwrap();
        ops.publish_quota(id, &p, "identity".into(), normalize(&json!({})))
            .unwrap();
        assert_eq!(ops.quota(id).unwrap()["blocked"], true);
        ops.invalidate_credentials(id).unwrap();
        assert_eq!(ops.quota(id).unwrap()["status"], "unknown");
        assert!(
            ops.publish_quota(id, &p, "identity".into(), normalize(&raw(100.0, true)))
                .is_err()
        );
    }
}
