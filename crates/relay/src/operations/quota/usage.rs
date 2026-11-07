//! Token totals are local observations, independent of upstream quota percentages.
use super::{Inner, QuotaWindow};
use serde_json::{Value, json};
use uuid::Uuid;

pub(super) fn windows(
    inner: &Inner,
    account: Uuid,
    windows: &[QuotaWindow],
    now: u64,
) -> Vec<Value> {
    windows
        .iter()
        .map(|window| {
            let mut value = json!(window);
            value["usage"] = usage(inner, account, window, now).unwrap_or(Value::Null);
            value
        })
        .collect()
}

fn usage(inner: &Inner, account: Uuid, window: &QuotaWindow, now: u64) -> Option<Value> {
    // Unknown buckets cannot be reliably mapped to the account's inference requests.
    if window.bucket != "codex" {
        return None;
    }
    let end = window.resets_at?;
    let duration = window.duration_mins?.checked_mul(60_000)?;
    let start = end.checked_sub(duration)?;
    // Do not extrapolate an expired window: the next window may start on demand.
    if duration == 0 || start > now || end <= now || now > i64::MAX as u64 {
        return None;
    }
    inner.db.query_row(
        "SELECT COUNT(*), COUNT(input_tokens + output_tokens), SUM(input_tokens + output_tokens) FROM requests WHERE account_id=?1 AND kind='inference' AND action NOT IN ('images.generations','images.edits') AND started_at>=?2 AND started_at<=?3",
        rusqlite::params![account.to_string(), start as i64, now as i64],
        |row| {
            let count: i64 = row.get(0)?;
            let known: i64 = row.get(1)?;
            let total: Option<i64> = row.get(2)?;
            Ok(json!({"from":start,"to":now,"requestCount":count,"usageKnownRequests":known,
                "totalTokens":if count == 0 {Some(0)} else {total}}))
        },
    ).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::Operations;

    #[test]
    fn window_totals_use_period_boundaries_and_do_not_double_count_cache() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let account = Uuid::new_v4();
        let inner = ops.inner.lock().unwrap();
        for (id, at, kind, owner, input, output) in [
            (
                "before",
                59_999,
                "inference",
                account,
                Some(1000),
                Some(1000),
            ),
            (
                "boundary",
                60_000,
                "inference",
                account,
                Some(100),
                Some(20),
            ),
            ("inside", 90_000, "inference", account, Some(200), Some(30)),
            ("missing", 90_000, "inference", account, None, None),
            (
                "maintenance",
                90_000,
                "maintenance",
                account,
                Some(1000),
                Some(1000),
            ),
            (
                "other",
                90_000,
                "inference",
                Uuid::new_v4(),
                Some(1000),
                Some(1000),
            ),
            (
                "future",
                110_000,
                "inference",
                account,
                Some(1000),
                Some(1000),
            ),
        ] {
            inner.db.execute("INSERT INTO requests(id,account_id,kind,action,started_at,status,input_tokens,output_tokens,cached_input_tokens) VALUES(?1,?2,?3,'test',?4,'succeeded',?5,?6,50)",
                rusqlite::params![id, owner.to_string(), kind, at, input, output]).unwrap();
        }
        for action in ["images.generations", "images.edits"] {
            inner.db.execute("INSERT INTO requests(id,account_id,kind,action,started_at,status,input_tokens,output_tokens) VALUES(?1,?2,'inference',?1,90000,'succeeded',5000,5000)", rusqlite::params![action, account.to_string()]).unwrap();
        }
        let window = QuotaWindow {
            bucket: "codex".into(),
            duration_mins: Some(1),
            resets_at: Some(120_000),
            ..Default::default()
        };
        let result = usage(&inner, account, &window, 100_000).unwrap();
        assert_eq!(result["totalTokens"], 350);
        assert_eq!(result["requestCount"], 3);
        assert_eq!(result["usageKnownRequests"], 2);
        assert_eq!(
            usage(&inner, Uuid::new_v4(), &window, 100_000).unwrap()["totalTokens"],
            0
        );
        assert!(usage(&inner, account, &window, 120_000).is_none());
        assert!(usage(&inner, account, &window, 59_999).is_none());
        let unknown = QuotaWindow {
            bucket: "other".into(),
            ..window.clone()
        };
        assert!(usage(&inner, account, &unknown, 100_000).is_none());
        let missing_bounds = QuotaWindow {
            resets_at: None,
            ..window
        };
        assert!(usage(&inner, account, &missing_bounds, 100_000).is_none());
    }
}
