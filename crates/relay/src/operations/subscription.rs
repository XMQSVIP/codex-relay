//! Display-only subscription evidence. Never used by admission or quota decisions.
use super::{Inner, Operations, runtime, storage};
use crate::{
    error::{Error, Result},
    provider::Provider,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

pub const RETRY_MS: u64 = 24 * 60 * 60 * 1000;
pub const MANUAL_INTERVAL_MS: u64 = 60_000;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    identity: String,
    generation: String,
    pub plan_type: Option<String>,
    upstream_plan: bool,
    starts_at: Option<u64>,
    expires_at: Option<u64>,
    will_renew: Option<bool>,
    checked_at: u64,
    attempted_at: u64,
    next_attempt_at: u64,
    error_code: Option<String>,
    #[serde(default)]
    initial_failures: u8,
    #[serde(skip)]
    verified: bool,
}

impl Snapshot {
    fn failed(&mut self, now: u64, code: &str) {
        let delay = if self.checked_at == 0
            && matches!(
                code,
                "SUBSCRIPTION_TIMEOUT"
                    | "SUBSCRIPTION_UNAVAILABLE"
                    | "RPC_TIMEOUT"
                    | "RPC_CLOSED"
                    | "BACKEND_START"
                    | "BACKEND_SPAWN"
                    | "IO_ERROR"
            ) {
            let delay = [MANUAL_INTERVAL_MS, 5 * 60_000, 15 * 60_000]
                .get(usize::from(self.initial_failures))
                .copied()
                .unwrap_or(RETRY_MS);
            self.initial_failures = self.initial_failures.saturating_add(1);
            delay
        } else {
            RETRY_MS
        };
        self.error_code = Some(code.into());
        self.next_attempt_at = now.saturating_add(delay);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Reading {
    pub starts_at: Option<u64>,
    pub expires_at: u64,
    pub will_renew: Option<bool>,
}

pub(crate) fn plan(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && !s.chars().any(char::is_control)
                && !s.eq_ignore_ascii_case("unknown")
        })
        .map(str::to_owned)
}

fn generalized(current: &str, next: &str) -> bool {
    let key = |s: &str| s.to_ascii_lowercase().replace(['_', '-', ' '], "");
    matches!(
        (key(current).as_str(), key(next).as_str()),
        ("prolite" | "promax", "pro")
            | (
                "selfservebusinessprolite" | "selfservebusinessusagebased",
                "team" | "business"
            )
            | (
                "ent26" | "enterprisecbpautomation" | "enterprisecbpusagebased",
                "business" | "enterprise"
            )
            | ("eduplus" | "edupro", "edu" | "education")
    )
}

pub(super) fn load(db: &Connection) -> Result<HashMap<Uuid, Snapshot>> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS account_subscriptions (account_id TEXT PRIMARY KEY, document TEXT NOT NULL)").map_err(storage)?;
    let mut stmt = db
        .prepare("SELECT account_id, document FROM account_subscriptions")
        .map_err(storage)?;
    let mut values = HashMap::new();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?
    {
        let (id, document) = row.map_err(storage)?;
        if let (Ok(id), Ok(value)) = (Uuid::parse_str(&id), serde_json::from_str(&document)) {
            values.insert(id, value);
        }
    }
    Ok(values)
}

fn persist(inner: &mut Inner, id: Uuid, value: Snapshot) -> Result<()> {
    inner.db.execute("INSERT INTO account_subscriptions VALUES (?1,?2) ON CONFLICT(account_id) DO UPDATE SET document=excluded.document",
        rusqlite::params![id.to_string(), serde_json::to_string(&value)?]).map_err(storage)?;
    inner.runtime.subscriptions.insert(id, value);
    Ok(())
}

pub(super) fn observe(
    inner: &mut Inner,
    id: Uuid,
    provider: &Provider,
    identity: &str,
    value: Option<&str>,
    upstream: bool,
) -> Result<()> {
    if !provider.is_chatgpt_account() {
        return clear(inner, id);
    }
    if identity.starts_with("account:") {
        return Ok(());
    }
    let generation = runtime::fingerprint(provider);
    let mut snapshot = inner
        .runtime
        .subscriptions
        .get(&id)
        .filter(|s| s.identity == identity && s.generation == generation)
        .cloned()
        .unwrap_or_else(|| Snapshot {
            identity: identity.into(),
            generation,
            ..Default::default()
        });
    snapshot.verified = true;
    if let Some(value) = plan(value) {
        if (upstream || !snapshot.upstream_plan)
            && !snapshot
                .plan_type
                .as_ref()
                .is_some_and(|old| generalized(old, &value))
        {
            snapshot.plan_type = Some(value);
            snapshot.upstream_plan = upstream;
        }
    }
    // Repeated route reads must not write the database on every inference.
    if inner
        .runtime
        .subscriptions
        .get(&id)
        .is_some_and(|old| serde_json::to_value(old).ok() == serde_json::to_value(&snapshot).ok())
    {
        inner.runtime.subscriptions.insert(id, snapshot);
        return Ok(());
    }
    persist(inner, id, snapshot)
}

pub(super) fn clear(inner: &mut Inner, id: Uuid) -> Result<()> {
    inner
        .db
        .execute(
            "DELETE FROM account_subscriptions WHERE account_id=?1",
            [id.to_string()],
        )
        .map_err(storage)?;
    inner.runtime.subscriptions.remove(&id);
    Ok(())
}

pub(super) fn retain_routes(inner: &mut Inner, next: &super::Settings) -> Result<()> {
    let removed: Vec<_> = inner
        .runtime
        .subscriptions
        .iter()
        .filter(|(id, s)| {
            let old = inner
                .settings
                .accounts
                .iter()
                .find(|a| a.id == **id)
                .and_then(|a| a.route.as_ref());
            let new = next
                .accounts
                .iter()
                .find(|a| a.id == **id)
                .and_then(|a| a.route.as_ref());
            old != new
                && !new.is_some_and(|p| {
                    p.is_chatgpt_account() && runtime::fingerprint(p) == s.generation
                })
        })
        .map(|(id, _)| *id)
        .collect();
    for id in removed {
        clear(inner, id)?;
    }
    Ok(())
}

fn current(inner: &Inner, id: Uuid) -> Option<&Snapshot> {
    let route = inner
        .settings
        .accounts
        .iter()
        .find(|a| a.id == id)?
        .route
        .as_ref()?;
    inner
        .runtime
        .subscriptions
        .get(&id)
        .filter(|s| route.is_chatgpt_account() && s.generation == runtime::fingerprint(route))
}

impl Operations {
    pub(crate) fn observe_subscription(
        &self,
        id: Uuid,
        provider: &Provider,
        identity: &str,
        value: Option<&str>,
    ) -> Result<()> {
        observe(
            &mut self.inner.lock().unwrap(),
            id,
            provider,
            identity,
            value,
            false,
        )
    }
    pub(crate) fn clear_subscription(&self, id: Uuid) -> Result<()> {
        clear(&mut self.inner.lock().unwrap(), id)
    }
    pub fn subscription(&self, id: Uuid) -> Result<Value> {
        let inner = self.inner.lock().unwrap();
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if account
            .route
            .as_ref()
            .is_some_and(|p| !p.is_chatgpt_account())
        {
            return Ok(json!({"status":"unsupported"}));
        }
        let Some(s) = current(&inner, id).filter(|s| s.verified) else {
            return Ok(json!({"status":"unknown"}));
        };
        Ok(
            json!({"status": if s.checked_at == 0 { if s.attempted_at > 0 {"error"} else {"unknown"} } else if s.error_code.is_some() || s.expires_at.is_some_and(|t| t <= crate::now_ms()) {"stale"} else {"current"},
            "planType":s.plan_type,"startsAt":s.starts_at,"expiresAt":s.expires_at,"willRenew":s.will_renew,
            "checkedAt":s.checked_at,"lastAttemptAt":s.attempted_at,"nextAttemptAt":s.next_attempt_at,"errorCode":s.error_code}),
        )
    }
    pub(crate) fn subscription_due(&self, id: Uuid, now: u64, manual: bool) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(account) = inner.settings.accounts.iter().find(|a| a.id == id) else {
            return false;
        };
        if !account
            .route
            .as_ref()
            .is_some_and(Provider::is_chatgpt_account)
            || (!manual && !account.enabled)
        {
            return false;
        }
        let Some(s) = current(&inner, id) else {
            return true;
        };
        if s.attempted_at > 0 && now.saturating_sub(s.attempted_at) < MANUAL_INTERVAL_MS {
            return false;
        }
        manual
            || (!s.verified && s.attempted_at == 0)
            || now >= s.next_attempt_at
            || !s.verified && s.error_code.is_none()
    }
    pub(crate) fn subscription_attempt(
        &self,
        id: Uuid,
        provider: &Provider,
        identity: &str,
        now: u64,
        manual: bool,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let Some(mut s) = current(&inner, id)
            .filter(|s| {
                s.verified
                    && s.identity == identity
                    && s.generation == runtime::fingerprint(provider)
            })
            .cloned()
        else {
            return Err(Error::new("ROUTE_CHANGED", "订阅查询前账户身份已变化。"));
        };
        if (s.attempted_at > 0 && now.saturating_sub(s.attempted_at) < MANUAL_INTERVAL_MS)
            || (!manual && now < s.next_attempt_at)
        {
            return Ok(false);
        }
        s.attempted_at = now;
        s.next_attempt_at = now.saturating_add(RETRY_MS);
        persist(&mut inner, id, s)?;
        Ok(true)
    }
    pub(crate) fn finish_subscription(
        &self,
        id: Uuid,
        provider: &Provider,
        identity: &str,
        attempt: u64,
        result: Result<Reading>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let Some(mut s) = current(&inner, id)
            .filter(|s| {
                s.verified
                    && s.identity == identity
                    && s.generation == runtime::fingerprint(provider)
                    && s.attempted_at == attempt
            })
            .cloned()
        else {
            return Err(Error::new("ROUTE_CHANGED", "订阅查询期间账户身份已变化。"));
        };
        match result {
            Ok(r) if r.expires_at <= attempt => {
                // An old billing period is not evidence that the current plan ended.
                // Keep an existing snapshot rather than replacing it with an older date.
                if s.expires_at.is_none() {
                    s.starts_at = r.starts_at;
                    s.expires_at = Some(r.expires_at);
                    s.will_renew = r.will_renew;
                    s.checked_at = attempt;
                }
                s.error_code = Some("SUBSCRIPTION_PERIOD_UNCONFIRMED".into());
                s.next_attempt_at = attempt.saturating_add(RETRY_MS);
            }
            Ok(r) => {
                s.starts_at = r.starts_at;
                s.expires_at = Some(r.expires_at);
                s.will_renew = r.will_renew;
                s.checked_at = attempt;
                s.error_code = None;
                s.initial_failures = 0;
                s.next_attempt_at = r.expires_at;
            }
            Err(e) => {
                s.failed(attempt, &e.code);
            }
        }
        persist(&mut inner, id, s)
    }
    pub(crate) fn subscription_setup_failed(
        &self,
        id: Uuid,
        provider: &Provider,
        now: u64,
        code: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner
            .settings
            .accounts
            .iter()
            .any(|a| a.id == id && a.route.as_ref() == Some(provider))
        {
            return Ok(());
        }
        let mut s = current(&inner, id).cloned().unwrap_or_else(|| Snapshot {
            generation: runtime::fingerprint(provider),
            ..Default::default()
        });
        s.attempted_at = now;
        s.failed(now, code);
        persist(&mut inner, id, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        std::sync::Arc<Operations>,
        Uuid,
        Provider,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let id = Uuid::new_v4();
        let p: Provider = serde_json::from_value(json!({"id":"openai","model":"test","baseUrl":"https://chatgpt.com/backend-api","auth":"native-chatgpt:fixture","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
        ops.register_accounts(&[(id, "subscription".into())])
            .unwrap();
        ops.set_inference_route(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            Some(p.clone()),
        )
        .unwrap();
        ops.observe_subscription(id, &p, "identity", Some("plus"))
            .unwrap();
        (dir, ops, id, p)
    }
    fn reading(expiry: u64) -> Reading {
        Reading {
            starts_at: None,
            expires_at: expiry,
            will_renew: Some(true),
        }
    }

    #[test]
    fn subscription_expiry_retry_manual_throttle_and_restart() {
        let (dir, ops, id, p) = fixture();
        let now = crate::now_ms();
        let expiry = now + 30 * RETRY_MS;
        assert!(ops.subscription_due(id, now, false));
        assert!(
            ops.subscription_attempt(id, &p, "identity", now, false)
                .unwrap()
        );
        ops.finish_subscription(id, &p, "identity", now, Ok(reading(expiry)))
            .unwrap();
        assert!(!ops.subscription_due(id, expiry - 1, false));
        assert!(ops.subscription_due(id, expiry, false));
        assert!(!ops.subscription_due(id, now + 59_999, true));
        assert!(ops.subscription_due(id, now + 60_000, true));
        drop(ops);
        let ops = Operations::open(dir.path()).unwrap();
        assert_eq!(
            ops.subscription(id).unwrap()["status"],
            "unknown",
            "persisted identity must be revalidated"
        );
        ops.observe_subscription(id, &p, "identity", Some("plus"))
            .unwrap();
        assert_eq!(ops.subscription(id).unwrap()["expiresAt"], expiry);
        assert!(
            !ops.subscription_attempt(id, &p, "identity", now + 60_000, false)
                .unwrap()
        );
        assert!(
            ops.subscription_attempt(id, &p, "identity", expiry, false)
                .unwrap()
        );
        ops.finish_subscription(
            id,
            &p,
            "identity",
            expiry,
            Err(Error::new("SUBSCRIPTION_TIMEOUT", "fixture")),
        )
        .unwrap();
        assert_eq!(ops.subscription(id).unwrap()["expiresAt"], expiry);
        assert!(!ops.subscription_due(id, expiry + RETRY_MS - 1, false));
        assert!(ops.subscription_due(id, expiry + RETRY_MS, false));
        assert!(
            ops.subscription_attempt(id, &p, "identity", expiry + RETRY_MS, false)
                .unwrap()
        );
        ops.finish_subscription(
            id,
            &p,
            "identity",
            expiry + RETRY_MS,
            Ok(reading(expiry - RETRY_MS)),
        )
        .unwrap();
        assert_eq!(ops.subscription(id).unwrap()["expiresAt"], expiry);
        assert_eq!(ops.subscription(id).unwrap()["status"], "stale");
        assert!(!ops.subscription_due(id, expiry + RETRY_MS + 60_000, false));
        assert!(ops.subscription_due(id, expiry + 2 * RETRY_MS, false));
    }

    #[test]
    fn subscription_identity_changes_discard_inflight_results_and_non_chatgpt_routes() {
        let (_dir, ops, id, mut p) = fixture();
        let now = crate::now_ms();
        ops.subscription_attempt(id, &p, "identity", now, true)
            .unwrap();
        ops.observe_subscription(id, &p, "different", Some("pro"))
            .unwrap();
        assert!(
            ops.finish_subscription(id, &p, "identity", now, Ok(reading(now + RETRY_MS)))
                .is_err()
        );
        assert!(ops.subscription(id).unwrap()["expiresAt"].is_null());
        ops.clear_subscription(id).unwrap();
        assert_eq!(ops.subscription(id).unwrap()["status"], "unknown");
        p.auth = "native-api-key".into();
        ops.set_inference_route(id, ops.snapshot()["revision"].as_u64().unwrap(), Some(p))
            .unwrap();
        assert!(!ops.subscription_due(id, now, true));
        assert_eq!(ops.subscription(id).unwrap()["status"], "unsupported");
        assert!(
            !ops.inner
                .lock()
                .unwrap()
                .runtime
                .subscriptions
                .contains_key(&id)
        );
    }

    #[test]
    fn subscription_passive_plan_wins_over_old_jwt_and_ignores_unknown_or_generic_values() {
        let (_dir, ops, id, p) = fixture();
        let raw = json!({"rateLimits":{"planType":"pro_lite","primary":{"usedPercent":10,"windowDurationMins":300,"resetsAt":2000000000}}});
        ops.publish_quota_reading(
            id,
            &p,
            "identity".into(),
            super::super::quota::normalize_reading(&raw, false),
        )
        .unwrap();
        ops.observe_subscription(id, &p, "identity", Some("plus"))
            .unwrap();
        for plan in [
            None,
            Some("unknown"),
            Some("pro"),
            Some(""),
            Some("bad\nplan"),
        ] {
            observe(
                &mut ops.inner.lock().unwrap(),
                id,
                &p,
                "identity",
                plan,
                true,
            )
            .unwrap();
        }
        assert_eq!(ops.subscription(id).unwrap()["planType"], "pro_lite");
        observe(
            &mut ops.inner.lock().unwrap(),
            id,
            &p,
            "identity",
            Some("free"),
            true,
        )
        .unwrap();
        assert_eq!(ops.subscription(id).unwrap()["planType"], "free");
    }

    #[test]
    fn initial_subscription_failures_retry_automatically_with_bounded_backoff() {
        let (dir, ops, id, p) = fixture();
        let mut now = crate::now_ms();
        for delay in [60_000, 5 * 60_000, 15 * 60_000, RETRY_MS] {
            assert!(ops.subscription_due(id, now, false));
            assert!(
                ops.subscription_attempt(id, &p, "identity", now, false)
                    .unwrap()
            );
            ops.finish_subscription(
                id,
                &p,
                "identity",
                now,
                Err(Error::new("SUBSCRIPTION_TIMEOUT", "fixture")),
            )
            .unwrap();
            assert_eq!(ops.subscription(id).unwrap()["nextAttemptAt"], now + delay);
            assert!(!ops.subscription_due(id, now + delay - 1, false));
            assert!(ops.subscription_due(id, now + delay, false));
            now += delay;
        }
        drop(ops);
        let ops = Operations::open(dir.path()).unwrap();
        ops.observe_subscription(id, &p, "identity", Some("plus"))
            .unwrap();
        assert!(
            ops.subscription_attempt(id, &p, "identity", now, false)
                .unwrap()
        );
        let expiry = now + 30 * RETRY_MS;
        ops.finish_subscription(id, &p, "identity", now, Ok(reading(expiry)))
            .unwrap();
        assert_eq!(ops.subscription(id).unwrap()["expiresAt"], expiry);
        assert!(!ops.subscription_due(id, expiry - 1, false));
        assert!(ops.subscription_due(id, expiry, false));
        assert!(
            !ops.subscription(id)
                .unwrap()
                .to_string()
                .contains("initialFailures")
        );
    }

    #[test]
    fn initial_setup_failures_retry_after_authentication_without_bypassing_disabled_state() {
        let (_dir, ops, id, p) = fixture();
        let now = crate::now_ms();
        ops.subscription_setup_failed(id, &p, now, "RPC_TIMEOUT")
            .unwrap();
        assert_eq!(
            ops.subscription(id).unwrap()["nextAttemptAt"],
            now + MANUAL_INTERVAL_MS
        );
        ops.observe_subscription(id, &p, "identity", Some("plus"))
            .unwrap();
        assert!(!ops.subscription_due(id, now + MANUAL_INTERVAL_MS - 1, false));
        assert!(ops.subscription_due(id, now + MANUAL_INTERVAL_MS, false));
        ops.set_account_activation(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            false,
            None,
        )
        .unwrap();
        assert!(!ops.subscription_due(id, now + MANUAL_INTERVAL_MS, false));
        assert!(ops.subscription_due(id, now + MANUAL_INTERVAL_MS, true));
    }

    #[test]
    fn subscription_disabled_accounts_do_not_refresh_and_errors_wait_one_day() {
        let (_dir, ops, id, p) = fixture();
        let now = crate::now_ms();
        ops.subscription_setup_failed(id, &p, now, "OAUTH_NOT_SIGNED_IN")
            .unwrap();
        assert!(!ops.subscription_due(id, now + RETRY_MS - 1, false));
        assert!(ops.subscription_due(id, now + RETRY_MS, false));
        ops.set_account_activation(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            false,
            None,
        )
        .unwrap();
        assert!(!ops.subscription_due(id, now + RETRY_MS, false));
        assert!(ops.subscription_due(id, now + RETRY_MS, true));
    }
}
