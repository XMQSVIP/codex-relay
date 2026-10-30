//! Durable reset attempts: retry uncertain outcomes with the original key.
use super::{
    Inner, Operations,
    quota::{self, ResetRequest},
    storage,
};
use crate::error::{Error, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const LEAD_MS: u64 = 600_000;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetAutomation {
    pub enabled: bool,
    pub revision: u64,
    pub next_attempt_at: u64,
    pub last_result: Option<String>,
    pub pending_request: Option<ResetRequest>,
    #[serde(default)]
    pending_expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<String>,
    #[serde(default)]
    settled: Vec<String>,
}
fn load(inner: &Inner, id: Uuid) -> Result<ResetAutomation> {
    if !inner.settings.accounts.iter().any(|a| a.id == id) {
        return Err(Error::new("NOT_FOUND", "账户不存在。"));
    }
    let doc: Option<String> = inner
        .db
        .query_row(
            "SELECT document FROM account_reset_automation WHERE account_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    doc.map(|s| serde_json::from_str(&s).map_err(Error::from))
        .unwrap_or_else(|| {
            Ok(ResetAutomation {
                revision: 1,
                ..Default::default()
            })
        })
}
fn save(inner: &mut Inner, id: Uuid, state: &ResetAutomation) -> Result<()> {
    let result = inner
        .db
        .execute(
            "INSERT OR REPLACE INTO account_reset_automation(account_id,document) VALUES(?1,?2)",
            rusqlite::params![id.to_string(), serde_json::to_string(state)?],
        )
        .map_err(storage);
    if result.is_err() {
        inner.storage_healthy = false;
    }
    result.map(|_| ())
}
fn candidate(q: &quota::QuotaSnapshot, state: &ResetAutomation, now: u64) -> Option<ResetRequest> {
    if !q.verified
        || q.status != "supported"
        || q.identity.starts_with("account:")
        || now.saturating_sub(q.checked_at) > 30_000
    {
        return None;
    }
    let cards = q.reset_credits.as_ref()?;
    if cards.available_count == 0 || now.saturating_sub(cards.checked_at) > 30_000 {
        return None;
    }
    let card = cards
        .credits
        .as_ref()?
        .iter()
        .filter(|c| {
            !c.id.is_empty()
                && c.status.as_deref() == Some("available")
                && c.reset_type.as_deref() == Some("codexRateLimits")
                && c.expires_at
                    .is_some_and(|t| t > now && t <= now.saturating_add(LEAD_MS))
                && !(state.identity.as_deref() == Some(&q.identity)
                    && state.settled.contains(&c.id))
        })
        .min_by_key(|c| c.expires_at)?;
    Some(ResetRequest {
        idempotency_key: Uuid::new_v4(),
        context: String::new(),
        credit_id: Some(card.id.clone()),
    })
}
impl Operations {
    pub fn reset_automation(&self, id: Uuid) -> Result<ResetAutomation> {
        load(&self.inner.lock().unwrap(), id)
    }
    pub fn set_reset_automation(
        &self,
        id: Uuid,
        enabled: bool,
        revision: u64,
    ) -> Result<ResetAutomation> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = load(&inner, id)?;
        if state.revision != revision {
            return Err(Error::new("CONFIG_CONFLICT", "设置已变化，请刷新后重试。"));
        }
        state.enabled = enabled;
        state.revision += 1;
        if state.pending_request.is_none() {
            state.next_attempt_at = 0;
        }
        save(&mut inner, id, &state)?;
        Ok(state)
    }
    pub(crate) fn reset_auto_due(&self, id: Uuid, now: u64) -> bool {
        self.reset_automation(id)
            .is_ok_and(|s| s.enabled && s.next_attempt_at <= now)
    }
    pub(crate) fn plan_auto_reset(&self, id: Uuid, now: u64) -> Result<Option<ResetRequest>> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = load(&inner, id)?;
        if state.pending_expires_at.is_some_and(|t| t <= now) {
            state.pending_request = None;
            state.pending_expires_at = None;
            state.last_result = Some("expired_unknown".into());
            save(&mut inner, id, &state)?;
        }
        let account = inner.settings.accounts.iter().find(|a| a.id == id).unwrap();
        if !state.enabled || !account.enabled || account.deleting || state.next_attempt_at > now {
            return Ok(None);
        }
        let Some(q) = quota::current(&inner, account).filter(|q| {
            q.verified && q.status == "supported" && now.saturating_sub(q.checked_at) <= 30_000
        }) else {
            return Ok(None);
        };
        let context = quota::reset_context(id, &q.identity, &q.generation);
        if let Some(request) = state.pending_request {
            // Never retarget an uncertain redemption after credentials or routes change.
            return Ok((request.context == context).then_some(request));
        }
        Ok(candidate(q, &state, now).map(|mut request| {
            request.context = context;
            request
        }))
    }
    pub(crate) fn prepare_reset(&self, id: Uuid, request: &ResetRequest) -> Result<()> {
        request.validate()?;
        let mut inner = self.inner.lock().unwrap();
        let mut state = load(&inner, id)?;
        let account = inner.settings.accounts.iter().find(|a| a.id == id).unwrap();
        let q = quota::current(&inner, account)
            .filter(|q| {
                q.verified
                    && !q.identity.starts_with("account:")
                    && quota::reset_context(id, &q.identity, &q.generation) == request.context
            })
            .ok_or_else(|| Error::new("ROUTE_CHANGED", "账户身份已变化，请刷新额度后重试。"))?;
        let identity = q.identity.clone();
        let expiry = q
            .reset_credits
            .as_ref()
            .and_then(|cards| cards.credits.as_ref())
            .and_then(|cards| {
                cards
                    .iter()
                    .find(|c| request.credit_id.as_deref() == Some(&c.id))
            })
            .and_then(|card| card.expires_at);
        if let Some(pending) = &state.pending_request {
            if pending != request {
                return Err(Error::new(
                    "RESET_PENDING",
                    "上一次重置结果尚未确认，请先核验原请求。",
                ));
            }
        }
        // Accounts with the same upstream identity share one reset inventory.
        let mut stmt = inner
            .db
            .prepare(
                "SELECT account_id,document FROM account_reset_automation WHERE account_id<>?1",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map([id.to_string()], |r| r.get::<_, String>(1))
            .map_err(storage)?;
        for row in rows {
            let other: ResetAutomation = serde_json::from_str(&row.map_err(storage)?)?;
            if other.identity.as_deref() == Some(&identity) && other.pending_request.is_some() {
                return Err(Error::new(
                    "RESET_PENDING",
                    "共享此额度的账户有未确认的重置，请先核验原请求。",
                ));
            }
        }
        drop(stmt);
        if state.identity.as_deref() != Some(&identity) {
            state.settled.clear();
        }
        state.identity = Some(identity);
        if state.pending_request.is_none() {
            state.pending_expires_at = expiry;
        }
        state.pending_request = Some(request.clone());
        state.next_attempt_at = crate::now_ms() + 60_000;
        save(&mut inner, id, &state)
    }
    pub(crate) fn finish_reset(&self, id: Uuid, outcome: &str, certain: bool) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = load(&inner, id)?;
        state.last_result = Some(outcome.into());
        state.next_attempt_at = crate::now_ms() + 60_000;
        if certain {
            state.pending_expires_at = None;
            if let Some(request) = state.pending_request.take() {
                if outcome != "nothingToReset" {
                    if let Some(card) = request.credit_id {
                        state.settled.push(card);
                    }
                    // A bounded cache prevents an unchanged inventory from redeeming again.
                    if state.settled.len() > 256 {
                        state.settled.drain(..128);
                    }
                }
            }
        }
        save(&mut inner, id, &state)
    }
    pub(crate) fn defer_auto_reset(&self, id: Uuid) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = load(&inner, id)?;
        let now = crate::now_ms();
        let next = inner
            .runtime
            .quotas
            .get(&id)
            .filter(|q| q.verified && q.status == "supported")
            .and_then(|q| q.reset_credits.as_ref())
            .map(|cards| {
                cards
                    .credits
                    .iter()
                    .flatten()
                    .filter(|c| c.status.as_deref() == Some("available"))
                    .filter_map(|c| c.expires_at)
                    .filter(|t| *t > now)
                    .map(|t| t.saturating_sub(LEAD_MS))
                    .min()
                    .unwrap_or(now + 900_000)
                    .min(now + 900_000)
            })
            .unwrap_or(now + 60_000);
        state.next_attempt_at = if state.pending_request.is_some() {
            now + 60_000
        } else {
            next.max(now + 60_000)
        };
        save(&mut inner, id, &state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn setup() -> (tempfile::TempDir, std::sync::Arc<Operations>, Uuid) {
        let temp = tempfile::tempdir().unwrap();
        let ops = Operations::open(temp.path()).unwrap();
        let id = Uuid::new_v4();
        ops.register_accounts(&[(id, "test".into())]).unwrap();
        let provider = crate::provider::Provider {
            execution: Default::default(),
            id: "openai".into(),
            model: "test".into(),
            base_url: "https://example.test".into(),
            auth: "chatgpt".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let revision = ops.snapshot()["revision"].as_u64().unwrap();
        ops.set_inference_route(id, revision, Some(provider.clone()))
            .unwrap();
        let now = crate::now_ms();
        let reading = quota::normalize_reading(&json!({"rateLimits":{},"rateLimitResetCredits":{"availableCount":2,"credits":[{"id":"later","resetType":"codexRateLimits","status":"available","expiresAt":(now+3_600_000)/1000},{"id":"soon","resetType":"codexRateLimits","status":"available","expiresAt":(now+120_000)/1000}]}}), true).unwrap();
        ops.publish_quota_reading(id, &provider, "test-identity".into(), Ok(reading))
            .unwrap();
        (temp, ops, id)
    }
    #[test]
    fn manual_default_auto_requires_fresh_identified_expiring_cards() {
        let (_temp, ops, id) = setup();
        let now = crate::now_ms();
        assert!(!ops.reset_auto_due(id, now));
        assert!(ops.plan_auto_reset(id, now).unwrap().is_none());
        ops.set_reset_automation(id, true, 1).unwrap();
        assert_eq!(
            ops.plan_auto_reset(id, now)
                .unwrap()
                .unwrap()
                .credit_id
                .as_deref(),
            Some("soon")
        );
        assert!(ops.plan_auto_reset(id, now + 31_000).unwrap().is_none());
        for invalid in ["noExpiry", "noId", "used", "otherType", "expired"] {
            let mut inner = ops.inner.lock().unwrap();
            let mut q = inner.runtime.quotas[&id].clone();
            let cards = q.reset_credits.as_mut().unwrap().credits.as_mut().unwrap();
            cards.retain(|c| c.id == "soon");
            match invalid {
                "noExpiry" => cards[0].expires_at = None,
                "noId" => cards[0].id.clear(),
                "used" => cards[0].status = Some("redeemed".into()),
                "otherType" => cards[0].reset_type = Some("other".into()),
                _ => cards[0].expires_at = Some(now),
            }
            assert!(candidate(&q, &ResetAutomation::default(), now).is_none());
            inner.settings.accounts[0].enabled = false;
        }
        assert!(ops.plan_auto_reset(id, now).unwrap().is_none());
    }
    #[test]
    fn uncertain_key_survives_restart_and_mode_changes_and_prevents_a_new_redemption() {
        let (temp, ops, id) = setup();
        ops.set_reset_automation(id, true, 1).unwrap();
        let request = ops.plan_auto_reset(id, crate::now_ms()).unwrap().unwrap();
        ops.prepare_reset(id, &request).unwrap();
        ops.finish_reset(id, "RESET_UNCERTAIN", false).unwrap();
        ops.set_reset_automation(id, false, 2).unwrap();
        ops.set_reset_automation(id, true, 3).unwrap();
        let mut different = request.clone();
        different.idempotency_key = Uuid::new_v4();
        assert_eq!(
            ops.prepare_reset(id, &different).unwrap_err().code,
            "RESET_PENDING"
        );
        let mut stale = request.clone();
        stale.context = "b".repeat(64);
        assert_eq!(
            ops.prepare_reset(id, &stale).unwrap_err().code,
            "ROUTE_CHANGED"
        );
        ops.prepare_reset(id, &request).unwrap();
        drop(ops);
        let ops = Operations::open(temp.path()).unwrap();
        let state = ops.reset_automation(id).unwrap();
        assert_eq!(state.pending_request, Some(request));
        assert!(state.enabled);
        ops.finish_reset(id, "alreadyRedeemed", true).unwrap();
        assert!(ops.reset_automation(id).unwrap().pending_request.is_none());
    }
    #[test]
    fn multiple_cards_are_redeemed_one_at_a_time_in_expiry_order() {
        let (_temp, ops, id) = setup();
        let now = crate::now_ms();
        let mut inner = ops.inner.lock().unwrap();
        let q = inner.runtime.quotas.get_mut(&id).unwrap();
        q.reset_credits.as_mut().unwrap().credits.as_mut().unwrap()[0].expires_at =
            Some(now + 300_000);
        drop(inner);
        ops.set_reset_automation(id, true, 1).unwrap();
        let first = ops.plan_auto_reset(id, now).unwrap().unwrap();
        assert_eq!(first.credit_id.as_deref(), Some("soon"));
        ops.prepare_reset(id, &first).unwrap();
        ops.finish_reset(id, "reset", true).unwrap();
        assert!(ops.plan_auto_reset(id, now).unwrap().is_none());
        let mut inner = ops.inner.lock().unwrap();
        let q = inner.runtime.quotas.get_mut(&id).unwrap();
        q.checked_at = now + 61_000;
        q.reset_credits.as_mut().unwrap().checked_at = now + 61_000;
        drop(inner);
        let second = ops.plan_auto_reset(id, now + 61_000).unwrap().unwrap();
        assert_eq!(second.credit_id.as_deref(), Some("later"));
        assert_ne!(first.idempotency_key, second.idempotency_key);
    }
    #[test]
    fn reset_runs_alongside_inference_without_using_its_capacity() {
        let (_temp, ops, id) = setup();
        let inference = ops
            .begin(Some(id), None, "inference", "responses", true)
            .unwrap();
        let reset = ops
            .begin(Some(id), None, "management", "quota_reset_auto", false)
            .unwrap();
        assert_eq!(ops.snapshot()["accounts"][0]["active"], 1);
        ops.set_reset_automation(id, true, 1).unwrap();
        let request = ops.plan_auto_reset(id, crate::now_ms()).unwrap().unwrap();
        ops.prepare_reset(id, &request).unwrap();
        ops.finish_reset(id, "reset", true).unwrap();
        // Even if the upstream inventory briefly lags, don't redeem this card again.
        let inner = ops.inner.lock().unwrap();
        assert!(
            candidate(
                &inner.runtime.quotas[&id],
                &load(&inner, id).unwrap(),
                crate::now_ms()
            )
            .is_none()
        );
        drop(inner);
        reset.complete(Ok(())).unwrap();
        assert_eq!(ops.snapshot()["accounts"][0]["active"], 1);
        inference.complete(Ok(())).unwrap();
    }
}
