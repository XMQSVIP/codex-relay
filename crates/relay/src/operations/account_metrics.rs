//! Account-page aggregation: all inference rows, independent of history paging.
use super::{Operations, storage};
use crate::error::{Error, Result};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountMetricsQuery {
    pub from: i64,
    pub to: i64,
    pub account_id: Option<Uuid>,
}

fn aggregate(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let succeeded: i64 = row.get(2)?;
    let failed: i64 = row.get(3)?;
    Ok(
        json!({"id":row.get::<_,Option<String>>(0)?,"requestCount":row.get::<_,i64>(1)?,
        "succeeded":succeeded,"failed":failed,
        "successRate":if succeeded+failed>0 {Some(succeeded as f64/(succeeded+failed) as f64)}else{None},
        "inputTokens":row.get::<_,Option<i64>>(4)?,"outputTokens":row.get::<_,Option<i64>>(5)?,
        "cachedInputTokens":row.get::<_,Option<i64>>(6)?,"totalTokens":row.get::<_,Option<i64>>(7)?,
        "usageKnownRequests":row.get::<_,i64>(8)?}),
    )
}

impl Operations {
    pub(crate) fn account_recently_used(&self, id: Uuid, since: u64) -> Result<bool> {
        let inner = self.inner.lock().unwrap();
        // Maintenance must not keep an idle account active. Use completion time
        // so a long inference still counts as recent after it finishes.
        inner.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM requests WHERE account_id=?1 AND kind='inference' AND (status IN ('running','queued') OR COALESCE(finished_at,started_at)>?2))",
            params![id.to_string(), since.min(i64::MAX as u64) as i64],
            |row| row.get(0),
        ).map_err(storage)
    }

    pub fn account_metrics(&self, query: AccountMetricsQuery) -> Result<Value> {
        if query.from < 0 || query.to < query.from || query.to.checked_sub(query.from).is_none() {
            return Err(Error::new("INVALID_INPUT", "统计日期范围无效。"));
        }
        let reader = self.history_db.lock().unwrap();
        let db = reader.unchecked_transaction().map_err(storage)?;
        let account = query.account_id.map(|id| id.to_string());
        let fields = "COUNT(*),SUM(status='succeeded'),SUM(status='failed'),SUM(input_tokens),SUM(output_tokens),SUM(cached_input_tokens),SUM(input_tokens+output_tokens),COUNT(input_tokens+output_tokens)";
        let filter = "kind='inference' AND account_id IS NOT NULL AND started_at>=?1 AND started_at<=?2 AND (?3 IS NULL OR account_id=?3)";
        let mut stmt = db.prepare(&format!("SELECT account_id,{fields} FROM requests WHERE {filter} GROUP BY account_id ORDER BY account_id")).map_err(storage)?;
        let rows = stmt
            .query_map(params![query.from, query.to, account], aggregate)
            .map_err(storage)?;
        let mut accounts = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage)?;
        // Last use belongs to retained history, not just today's range.
        let mut last = db.prepare("SELECT account_id,MAX(started_at) FROM requests WHERE kind='inference' AND account_id IS NOT NULL AND (?1 IS NULL OR account_id=?1) GROUP BY account_id").map_err(storage)?;
        let last_rows = last
            .query_map(params![account], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(storage)?;
        for row in last_rows {
            let (id, at) = row.map_err(storage)?;
            if let Some(value) = accounts.iter_mut().find(|a| a["id"] == id) {
                value["lastUsedAt"] = at.into();
            } else {
                accounts.push(json!({"id":id,"requestCount":0,"succeeded":0,"failed":0,"successRate":null,"inputTokens":null,"outputTokens":null,"cachedInputTokens":null,"totalTokens":null,"usageKnownRequests":0,"lastUsedAt":at}));
            }
        }
        let models = if account.is_some() {
            let mut stmt = db.prepare(&format!("SELECT model,{fields} FROM requests WHERE {filter} GROUP BY model ORDER BY SUM(input_tokens+output_tokens) DESC,model")).map_err(storage)?;
            stmt.query_map(params![query.from, query.to, account], aggregate)
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?
        } else {
            vec![]
        };
        Ok(
            json!({"from":query.from,"to":query.to,"updatedAt":crate::now_ms(),"accounts":accounts,"models":models}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_activity_ignores_maintenance_and_uses_inference_completion() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let account = Uuid::new_v4();
        assert!(!ops.account_recently_used(account, 1000).unwrap());
        let insert = |id: &str, kind: &str, start: i64, finish: Option<i64>, status: &str| {
            ops.inner.lock().unwrap().db.execute(
                "INSERT INTO requests(id,account_id,kind,action,started_at,finished_at,status) VALUES(?1,?2,?3,'test',?4,?5,?6)",
                params![id, account.to_string(), kind, start, finish, status],
            ).unwrap();
        };
        insert("refresh", "management", 2000, Some(2001), "succeeded");
        insert("old", "inference", 100, Some(1000), "succeeded");
        assert!(!ops.account_recently_used(account, 1000).unwrap());
        insert("long", "inference", 200, Some(2000), "succeeded");
        assert!(ops.account_recently_used(account, 1000).unwrap());
        assert!(!ops.account_recently_used(Uuid::new_v4(), 1000).unwrap());
        assert!(!ops.account_recently_used(account, 2000).unwrap());
        insert("running", "inference", 300, None, "running");
        assert!(ops.account_recently_used(account, 2000).unwrap());
    }
}
