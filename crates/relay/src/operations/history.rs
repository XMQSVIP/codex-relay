//! Request-history queries, usage aggregation and idle-only history clearing.

use super::{Operations, storage};
use crate::error::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

enum UsageGroup {
    Account,
    Key,
    Model,
}

fn grouped_usage(
    db: &rusqlite::Connection,
    filter: &str,
    bind: &[&dyn rusqlite::ToSql],
    group: UsageGroup,
) -> Result<Vec<Value>> {
    let (id, name) = match group {
        UsageGroup::Account => ("account_id", "account_name"),
        UsageGroup::Key => ("key_id", "key_name"),
        UsageGroup::Model => ("COALESCE(model,'')", "model"),
    };
    let (attribution, name_lookup) = if matches!(group, UsageGroup::Model) {
        (
            "kind='inference'".to_owned(),
            "NULLIF(entity_id,'')".to_owned(),
        )
    } else {
        (
            format!("{id} IS NOT NULL"),
            format!(
                "(SELECT {name} FROM requests WHERE {filter} AND {id}=grouped.entity_id AND {name} IS NOT NULL ORDER BY started_at DESC,id DESC LIMIT 1)"
            ),
        )
    };
    // Stable IDs keep renames together and identical names separate. Retain the
    // latest available name snapshot, including for deleted accounts and keys.
    // Aggregate first, then use account/key time indexes for name lookup instead
    // of sorting every history row through a window function.
    let mut statement = db
        .prepare(&format!(
            "WITH grouped AS (
SELECT {id} AS entity_id, COUNT(*) AS request_count, SUM(input_tokens) AS input_total,
SUM(output_tokens) AS output_total, SUM(input_tokens+output_tokens) AS token_total,
COUNT(input_tokens+output_tokens) AS known_count, SUM(status='failed') AS failed_count,
AVG(CASE WHEN kind='inference' AND first_delta_ms IS NOT NULL THEN first_delta_ms+COALESCE(wait_ms,0) END) AS first_response,
AVG(CASE WHEN kind='inference' AND finished_at IS NOT NULL THEN MAX(0,finished_at-started_at) END) AS total_ms,
SUM(CASE WHEN kind='inference' THEN cached_input_tokens END) AS cached_total,
SUM(CASE WHEN kind='inference' AND cached_input_tokens IS NOT NULL THEN input_tokens END) AS cache_input_total,
COUNT(CASE WHEN kind='inference' THEN cached_input_tokens END) AS cache_known_count,
AVG(CASE WHEN kind='inference' THEN first_delta_ms END) AS model_first_response
FROM requests WHERE {filter} AND {attribution} GROUP BY {id})
SELECT entity_id,{name_lookup}, request_count,input_total,output_total,token_total,known_count,
failed_count,first_response,total_ms,cached_total,cache_input_total,cache_known_count,model_first_response
FROM grouped ORDER BY token_total DESC,entity_id ASC"
        ))
        .map_err(storage)?;
    statement
        .query_map(bind, |row| {
            let cached = row.get::<_, Option<i64>>(10)?;
            let known_input = row.get::<_, Option<i64>>(11)?;
            let rate = cached
                .zip(known_input)
                .filter(|(_, input)| *input > 0)
                .map(|(cache, input)| cache as f64 / input as f64);
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, Option<String>>(1)?,
                "requestCount": row.get::<_, i64>(2)?,
                "inputTokens": row.get::<_, Option<i64>>(3)?,
                "outputTokens": row.get::<_, Option<i64>>(4)?,
                "totalTokens": row.get::<_, Option<i64>>(5)?,
                "usageKnownRequests": row.get::<_, i64>(6)?,
                "failedRequests": row.get::<_, i64>(7)?,
                "averageFirstResponseMs": row.get::<_, Option<f64>>(8)?,
                "averageModelFirstResponseMs": row.get::<_, Option<f64>>(13)?,
                "averageTotalMs": row.get::<_, Option<f64>>(9)?,
                "cachedInputTokens": cached,
                "cacheKnownInputTokens": known_input,
                "cacheKnownRequests": row.get::<_, i64>(12)?,
                "cacheHitRate": rate,
            }))
        })
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatsQuery {
    pub key_id: Option<Uuid>,
    pub key_name: Option<String>,
    pub model: Option<String>,
    pub error_code: Option<String>,
    pub hours: Option<u32>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub group_id: Option<Uuid>,
    pub group_name: Option<String>,
    pub account_id: Option<Uuid>,
    pub account_name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

impl Operations {
    pub fn clear_request_history(&self) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        // Admission uses this same lock: no request can lose its journal row
        // between the idle check and the committed deletion.
        if !inner.active.is_empty() || !inner.pending.is_empty() {
            return Err(Error::new(
                "BUSY",
                "还有执行中或排队的请求，请等待结束或取消后再清空记录。",
            ));
        }
        if inner.draining {
            return Err(Error::new(
                "SERVICE_DRAINING",
                "服务正在退出，请稍后再清空记录。",
            ));
        }
        let transaction = inner.db.transaction().map_err(storage)?;
        let deleted = transaction
            .execute("DELETE FROM requests", [])
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(deleted)
    }

    pub fn statistics(&self, query: StatsQuery) -> Result<Value> {
        let (from, end, span_ms) = match (query.from, query.to) {
            (Some(from), Some(to)) if query.hours.is_none() && from >= 0 && to >= from => {
                let span = to
                    .checked_sub(from)
                    .and_then(|v| v.checked_add(1))
                    .ok_or_else(|| Error::new("INVALID_INPUT", "统计日期范围无效。"))?;
                (from, to, span)
            }
            (None, None) => {
                let hours = query.hours.unwrap_or(24);
                if ![1, 24, 168, 720].contains(&hours) {
                    return Err(Error::new("INVALID_INPUT", "统计筛选条件无效。"));
                }
                let end = crate::now_ms() as i64;
                let span = i64::from(hours) * 3_600_000;
                (end.saturating_sub(span), end, span)
            }
            _ => {
                return Err(Error::new(
                    "INVALID_INPUT",
                    "请同时提供有效的统计开始和结束时间。",
                ));
            }
        };
        let hours = span_ms / 3_600_000 + i64::from(span_ms % 3_600_000 != 0);
        let page = query.page.unwrap_or(1);
        let page_size = query.page_size.unwrap_or(20);
        if page == 0
            || ![20, 50, 100].contains(&page_size)
            || query.kind.as_ref().is_some_and(|v| {
                !["management", "diagnostic", "native_check", "inference"].contains(&v.as_str())
            })
            || query.status.as_ref().is_some_and(|v| {
                ![
                    "running",
                    "queued",
                    "timed_out",
                    "succeeded",
                    "failed",
                    "rejected",
                    "cancelled",
                    "interrupted",
                ]
                .contains(&v.as_str())
            })
        {
            return Err(Error::new("INVALID_INPUT", "统计筛选条件无效。"));
        }
        let reader = self.history_db.lock().unwrap();
        // All aggregates and the page must describe the same committed snapshot.
        let db = reader.unchecked_transaction().map_err(storage)?;
        let group = query.group_id.map(|id| id.to_string());
        let account = query.account_id.map(|id| id.to_string());
        let key_id = query.key_id.map(|id| id.to_string());
        // Desktop diagnostic jobs keep their journal rows for queue tracking and
        // recovery, but are not request history. Apply this before pagination and
        // aggregation so existing rows cannot inflate totals or leave page gaps.
        // Account-scoped diagnostics (such as connection tests) remain visible.
        // Emit only active predicates so SQLite can use the account/key/group
        // time indexes. Values always remain bound parameters, including names.
        let mut filter = "NOT (kind='diagnostic' AND account_id IS NULL AND group_id IS NULL AND action IN ('shared','guard','stream','provider','inspect-provider','doctor','prepare')) AND started_at>=?1 AND started_at<=?2".to_owned();
        let mut values = vec![
            rusqlite::types::Value::Integer(from),
            rusqlite::types::Value::Integer(end),
        ];
        for (column, value, contains) in [
            ("group_id", group, false),
            ("account_id", account, false),
            ("kind", query.kind, false),
            ("status", query.status, false),
            ("key_id", key_id, false),
            ("model", query.model, false),
            ("error_code", query.error_code, false),
            ("key_name", query.key_name, true),
            ("account_name", query.account_name, true),
            ("group_name", query.group_name, true),
        ] {
            if let Some(value) = value {
                values.push(rusqlite::types::Value::Text(value));
                let parameter = values.len();
                if contains {
                    filter.push_str(&format!(" AND instr({column},?{parameter})>0"));
                } else {
                    filter.push_str(&format!(" AND {column}=?{parameter}"));
                }
            }
        }
        let bind: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let bind = bind.as_slice();
        // Counts, usage and latency share one scan and one snapshot. FILTER
        // preserves NULL for unknown usage and excludes management token data.
        let summary = db.query_row(&format!("SELECT
COUNT(*),COALESCE(SUM(status='succeeded'),0),COALESCE(SUM(status='failed'),0),
COALESCE(SUM(status='rejected'),0),COALESCE(SUM(status IN ('cancelled','interrupted')),0),
COALESCE(SUM(status='running'),0),AVG(CASE WHEN status IN ('succeeded','failed') THEN duration_ms END),
COALESCE(SUM(kind='inference'),0),COALESCE(SUM(status='queued'),0),COALESCE(SUM(status='timed_out'),0),
COALESCE(SUM(error_code='QUEUE_FULL'),0),AVG(wait_ms),
COALESCE(SUM(kind='inference' AND status='succeeded'),0),COALESCE(SUM(kind='inference' AND status='failed'),0),
SUM(input_tokens) FILTER (WHERE kind='inference'),SUM(output_tokens) FILTER (WHERE kind='inference'),
SUM(input_tokens+output_tokens) FILTER (WHERE kind='inference'),COUNT(input_tokens+output_tokens) FILTER (WHERE kind='inference'),
SUM(cached_input_tokens) FILTER (WHERE kind='inference'),
SUM(input_tokens) FILTER (WHERE kind='inference' AND cached_input_tokens IS NOT NULL),
COUNT(cached_input_tokens) FILTER (WHERE kind='inference'),
AVG(first_delta_ms+COALESCE(wait_ms,0)) FILTER (WHERE kind='inference'),
AVG(MAX(0,finished_at-started_at)) FILTER (WHERE kind='inference' AND finished_at IS NOT NULL),
AVG(first_delta_ms) FILTER (WHERE kind='inference')
FROM requests WHERE {filter}"), bind, |r| {
            let cached = r.get::<_, Option<i64>>(18)?;
            let known_input = r.get::<_, Option<i64>>(19)?;
            let rate = cached.zip(known_input).filter(|(_, input)| *input > 0)
                .map(|(cache, input)| cache as f64 / input as f64);
            Ok(json!({
                "total":r.get::<_,i64>(0)?,"succeeded":r.get::<_,i64>(1)?,"failed":r.get::<_,i64>(2)?,
                "rejected":r.get::<_,i64>(3)?,"cancelled":r.get::<_,i64>(4)?,"running":r.get::<_,i64>(5)?,
                "averageMs":r.get::<_,Option<f64>>(6)?,"inferenceRequests":r.get::<_,i64>(7)?,
                "queued":r.get::<_,i64>(8)?,"timedOut":r.get::<_,i64>(9)?,"queueFull":r.get::<_,i64>(10)?,
                "averageWaitMs":r.get::<_,Option<f64>>(11)?,"inferenceSucceeded":r.get::<_,i64>(12)?,"inferenceFailed":r.get::<_,i64>(13)?,
                "inputTokens":r.get::<_,Option<i64>>(14)?,"outputTokens":r.get::<_,Option<i64>>(15)?,
                "totalTokens":r.get::<_,Option<i64>>(16)?,"usageKnownRequests":r.get::<_,i64>(17)?,
                "cachedInputTokens":cached,"cacheKnownInputTokens":known_input,"cacheKnownRequests":r.get::<_,i64>(20)?,"cacheHitRate":rate,
                "averageFirstResponseMs":r.get::<_,Option<f64>>(21)?,"averageTotalMs":r.get::<_,Option<f64>>(22)?,
                "averageModelFirstResponseMs":r.get::<_,Option<f64>>(23)?,
            }))
        }).map_err(storage)?;
        let account_usage = grouped_usage(&db, &filter, bind, UsageGroup::Account)?;
        let key_usage = grouped_usage(&db, &filter, bind, UsageGroup::Key)?;
        let model_usage = grouped_usage(&db, &filter, bind, UsageGroup::Model)?;
        let total_pages = summary["total"]
            .as_u64()
            .unwrap_or(0)
            .div_ceil(u64::from(page_size))
            .max(1);
        let page = u64::from(page).min(total_pages);
        let mut statement=db.prepare(&format!("SELECT id,started_at,kind,action,account_id,account_name,group_id,group_name,status,duration_ms,error_code,wait_ms,key_id,model,input_tokens,output_tokens,native_thread_id,native_turn_id,model_outcome,failure_stage,cleanup_error_code,first_delta_ms,delivery_status,cached_input_tokens,key_name,CASE WHEN finished_at IS NOT NULL THEN MAX(0,finished_at-started_at) END,CASE WHEN first_delta_ms IS NOT NULL THEN first_delta_ms+COALESCE(wait_ms,0) END,reasoning_effort,user_agent,streaming,response_model,parameter_handling,scheduling,subagent_kind,service_tier,response_service_tier,endpoint,client_transport,upstream_transport FROM requests WHERE {filter} ORDER BY started_at DESC,id DESC LIMIT {page_size} OFFSET {}",(page-1)*u64::from(page_size))).map_err(storage)?;
        let records=statement.query_map(bind,|r|Ok(json!({"endpoint":r.get::<_,Option<String>>(36)?,"clientTransport":r.get::<_,Option<String>>(37)?,"upstreamTransport":r.get::<_,Option<String>>(38)?,"id":r.get::<_,String>(0)?,"startedAt":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"action":r.get::<_,String>(3)?,"accountId":r.get::<_,Option<String>>(4)?,"accountName":r.get::<_,Option<String>>(5)?,"groupId":r.get::<_,Option<String>>(6)?,"groupName":r.get::<_,Option<String>>(7)?,"status":r.get::<_,String>(8)?,"durationMs":r.get::<_,Option<i64>>(9)?,"errorCode":r.get::<_,Option<String>>(10)?,"waitMs":r.get::<_,Option<i64>>(11)?,"keyId":r.get::<_,Option<String>>(12)?,"model":r.get::<_,Option<String>>(13)?,"inputTokens":r.get::<_,Option<i64>>(14)?,"outputTokens":r.get::<_,Option<i64>>(15)?,"nativeThreadId":r.get::<_,Option<String>>(16)?,"nativeTurnId":r.get::<_,Option<String>>(17)?,"modelOutcome":r.get::<_,Option<String>>(18)?,"failureStage":r.get::<_,Option<String>>(19)?,"cleanupErrorCode":r.get::<_,Option<String>>(20)?,"firstDeltaMs":r.get::<_,Option<i64>>(21)?,"deliveryStatus":r.get::<_,Option<String>>(22)?,"cachedInputTokens":r.get::<_,Option<i64>>(23)?,"keyName":r.get::<_,Option<String>>(24)?,"totalMs":r.get::<_,Option<i64>>(25)?,"firstResponseMs":r.get::<_,Option<i64>>(26)?,"reasoningEffort":r.get::<_,Option<String>>(27)?,"userAgent":r.get::<_,Option<String>>(28)?,"streaming":r.get::<_,Option<bool>>(29)?,"responseModel":r.get::<_,Option<String>>(30)?,"subagentKind":r.get::<_,Option<String>>(33)?,"serviceTier":r.get::<_,Option<String>>(34)?,"responseServiceTier":r.get::<_,Option<String>>(35)?,"scheduling":r.get::<_,Option<String>>(32)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"parameterHandling":r.get::<_,Option<String>>(31)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or_else(||json!([]))}))).map_err(storage)?.collect::<std::result::Result<Vec<_>,_>>().map_err(storage)?;
        let bucket_ms = span_ms / 24 + i64::from(span_ms % 24 != 0);
        let mut trend = vec![
            json!({"count":0,"succeeded":0,"rejected":0,"inferenceRequests":0,"inputTokens":null,"outputTokens":null,"totalTokens":null,"usageKnownRequests":0,"cachedInputTokens":null,"cacheKnownInputTokens":null,"cacheKnownRequests":0,"cacheHitRate":null});
            24
        ];
        let mut statement=db.prepare(&format!("SELECT MIN(23,(started_at-?1)/{bucket_ms}),COUNT(*),SUM(status='succeeded'),SUM(status='rejected'),SUM(kind='inference'),SUM(CASE WHEN kind='inference' THEN input_tokens END),SUM(CASE WHEN kind='inference' THEN output_tokens END),SUM(CASE WHEN kind='inference' THEN input_tokens+output_tokens END),COUNT(CASE WHEN kind='inference' THEN input_tokens+output_tokens END),SUM(CASE WHEN kind='inference' THEN cached_input_tokens END),SUM(CASE WHEN kind='inference' AND cached_input_tokens IS NOT NULL THEN input_tokens END),COUNT(CASE WHEN kind='inference' THEN cached_input_tokens END) FROM requests WHERE {filter} GROUP BY 1")).map_err(storage)?;
        let buckets = statement
            .query_map(bind, |r| {
                let cached = r.get::<_, Option<i64>>(9)?;
                let known_input = r.get::<_, Option<i64>>(10)?;
                let rate = cached.zip(known_input).filter(|(_, input)| *input > 0)
                    .map(|(cache, input)| cache as f64 / input as f64);
                Ok((
                    r.get::<_, u32>(0)? as usize,
                    json!({"count":r.get::<_,i64>(1)?,"succeeded":r.get::<_,i64>(2)?,"rejected":r.get::<_,i64>(3)?,"inferenceRequests":r.get::<_,i64>(4)?,"inputTokens":r.get::<_,Option<i64>>(5)?,"outputTokens":r.get::<_,Option<i64>>(6)?,"totalTokens":r.get::<_,Option<i64>>(7)?,"usageKnownRequests":r.get::<_,i64>(8)?,"cachedInputTokens":cached,"cacheKnownInputTokens":known_input,"cacheKnownRequests":r.get::<_,i64>(11)?,"cacheHitRate":rate}),
                ))
            })
            .map_err(storage)?;
        for row in buckets {
            let (i, bucket) = row.map_err(storage)?;
            trend[i] = bucket;
        }
        for (i, bucket) in trend.iter_mut().enumerate() {
            bucket["startedAt"] = from
                .saturating_add((i as i64).saturating_mul(bucket_ms))
                .min(end)
                .into();
        }
        Ok(
            json!({"from":from,"to":end,"hours":hours,"page":page,"pageSize":page_size,"summary":summary,"trend":trend,"records":records,"accountUsage":account_usage,"keyUsage":key_usage,"modelUsage":model_usage}),
        )
    }
}
