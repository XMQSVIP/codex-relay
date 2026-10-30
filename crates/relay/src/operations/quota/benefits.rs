use super::{QuotaWindow, normalize};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credits {
    pub balance: Option<String>,
    pub has_credits: Option<bool>,
    pub unlimited: bool,
    pub checked_at: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredit {
    pub id: String,
    pub reset_type: Option<String>,
    pub status: Option<String>,
    pub expires_at: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredits {
    pub available_count: u64,
    pub credits: Option<Vec<ResetCredit>>,
    pub checked_at: u64,
}
#[derive(Default)]
pub struct QuotaReading {
    pub plan_type: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub credits: Option<Credits>,
    pub reset_credits: Option<ResetCredits>,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResetRequest {
    pub idempotency_key: Uuid,
    pub context: String,
    pub credit_id: Option<String>,
}
impl ResetRequest {
    pub fn validate(&self) -> Result<()> {
        if self.idempotency_key.is_nil()
            || self.context.len() != 64
            || !self.context.bytes().all(|c| c.is_ascii_hexdigit())
            || self.credit_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 512 || id.chars().any(char::is_control)
            })
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "重置请求无效，请刷新额度后重试。",
            ));
        }
        Ok(())
    }
}
pub fn reset_context(id: Uuid, identity: &str, generation: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!("{id}:{identity}:{generation}")))
}
fn short_text(v: &Value, max: usize) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
pub fn normalize_reading(raw: &Value, complete: bool) -> Result<QuotaReading> {
    let windows = normalize(raw)?;
    let now = crate::now_ms();
    let bucket = raw
        .get("rateLimitsByLimitId")
        .and_then(|v| v.get("codex"))
        .or_else(|| {
            raw.get("rateLimits")
                .filter(|v| v["limitId"].as_str().is_none_or(|id| id == "codex"))
        });
    let credits = bucket
        .and_then(|b| b.get("credits"))
        .filter(|v| v.is_object())
        .map(|v| Credits {
            balance: short_text(&v["balance"], 80).filter(|s| {
                let s = s.strip_prefix('-').unwrap_or(s);
                let mut parts = s.split('.');
                let digits = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
                parts.next().is_some_and(digits)
                    && parts.next().is_none_or(digits)
                    && parts.next().is_none()
            }),
            has_credits: v["hasCredits"].as_bool(),
            unlimited: v["unlimited"] == true,
            checked_at: now,
        });
    let reset = &raw["rateLimitResetCredits"];
    let reset_credits = reset["availableCount"]
        .as_u64()
        .map(|available_count| ResetCredits {
            available_count,
            checked_at: now,
            credits: reset["credits"].as_array().map(|items| {
                items
                    .iter()
                    .take(128)
                    .filter_map(|v| {
                        Some(ResetCredit {
                            id: short_text(&v["id"], 512)?,
                            reset_type: short_text(&v["resetType"], 64),
                            status: short_text(&v["status"], 64),
                            expires_at: v["expiresAt"].as_u64().and_then(|s| s.checked_mul(1000)),
                        })
                    })
                    .collect()
            }),
        });
    Ok(QuotaReading {
        plan_type: super::super::subscription::plan(bucket.and_then(|b| b["planType"].as_str())),
        windows,
        credits,
        reset_credits,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn balances_preserve_precision_and_counts_are_authoritative() {
        let reading = normalize_reading(&json!({"rateLimits":{"credits":{"balance":"9007199254740993.123456789","hasCredits":true,"unlimited":false}},"rateLimitResetCredits":{"availableCount":5,"credits":[{"id":"card-1","resetType":"codexRateLimits","status":"available","expiresAt":1800000000}]}}), true).unwrap();
        assert_eq!(
            reading.credits.unwrap().balance.as_deref(),
            Some("9007199254740993.123456789")
        );
        let resets = reading.reset_credits.unwrap();
        assert_eq!(resets.available_count, 5);
        assert_eq!(resets.credits.unwrap()[0].expires_at, Some(1800000000000));
    }
    #[test]
    fn absent_zero_and_count_only_remain_distinct() {
        let raw = json!({"rateLimits":{}});
        assert!(normalize_reading(&raw, true).unwrap().credits.is_none());
        assert!(
            normalize_reading(&raw, true)
                .unwrap()
                .reset_credits
                .is_none()
        );
        let reading = normalize_reading(&json!({"rateLimits":{"credits":{"balance":"0","hasCredits":false}},"rateLimitResetCredits":{"availableCount":0,"credits":[]}}), true).unwrap();
        assert_eq!(reading.credits.unwrap().balance.as_deref(), Some("0"));
        assert_eq!(reading.reset_credits.unwrap().credits.unwrap().len(), 0);
        let reading = normalize_reading(
            &json!({"rateLimits":{},"rateLimitResetCredits":{"availableCount":3,"credits":null}}),
            true,
        )
        .unwrap();
        assert!(reading.reset_credits.unwrap().credits.is_none());
    }
    #[test]
    fn invalid_fields_and_unrelated_buckets_do_not_fabricate_balances() {
        let reading = normalize_reading(
            &json!({"rateLimitsByLimitId":{"other":{"credits":{"balance":"99"}}}}),
            true,
        )
        .unwrap();
        assert!(reading.credits.is_none());
        let reading = normalize_reading(
            &json!({"rateLimits":{"credits":{"balance":"secret-invalid"}}}),
            true,
        )
        .unwrap();
        assert!(reading.credits.unwrap().balance.is_none());
        let request = ResetRequest {
            idempotency_key: Uuid::nil(),
            context: "a".repeat(64),
            credit_id: None,
        };
        assert!(request.validate().is_err());
    }
}
