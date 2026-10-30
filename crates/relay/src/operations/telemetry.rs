use super::{ClientAccess, Lease, Operations, storage};
use crate::{
    error::{Error, Result},
    execution::{ExecutionOutcome, cancelled, failure_stage},
};
use rusqlite::params;
use serde_json::{Value, json};

impl Lease {
    pub fn finish_execution(
        mut self,
        outcome: &ExecutionOutcome,
        response_error: Option<&Error>,
    ) -> Result<()> {
        let summary = outcome.model.as_ref().ok();
        if summary.is_some_and(|s| s.outcome == "completed") {
            self.owner.quota_success(self.id);
        }
        let model_error = outcome.model.as_ref().err().cloned().or_else(|| {
            summary.filter(|s| s.outcome != "completed").map(|s| {
                Error::new(
                    s.error_code.as_deref().unwrap_or("UPSTREAM_REQUEST_FAILED"),
                    s.error_message.as_deref().unwrap_or("原生请求未完成。"),
                )
            })
        });
        let error = model_error
            .as_ref()
            .or(response_error)
            .or(outcome.cleanup_error.as_ref());
        let code = error
            .map(|e| e.code.as_str())
            .or_else(|| summary.and_then(|s| s.error_code.as_deref()));
        let status = if code.is_some_and(cancelled) {
            "cancelled"
        } else if error.is_some() {
            "failed"
        } else {
            summary.map_or("failed", |s| match s.outcome.as_str() {
                "completed" => "succeeded",
                "cancelled" => "cancelled",
                _ => "failed",
            })
        };
        let mut inner = self.owner.inner.lock().unwrap();
        super::rate_limit::finished(
            &mut inner,
            self.account_id,
            self.provider.as_ref(),
            self.id,
            summary.is_some_and(|s| s.outcome == "completed"),
            model_error.as_ref().map(|e| e.code.as_str()),
        );
        super::scheduler::finished(
            &mut inner,
            self.id,
            summary.is_some_and(|s| s.outcome == "completed"),
            model_error.as_ref().map(|e| e.code.as_str()),
            summary.and_then(|s| s.first_delta_at),
        );
        let result=inner.db.execute("UPDATE requests SET status=?1,finished_at=?2,duration_ms=?3,error_code=?4,input_tokens=?5,output_tokens=?6,native_thread_id=?7,native_turn_id=?8,model_outcome=?9,failure_stage=?10,cleanup_error_code=?11,first_delta_ms=CASE WHEN ?12 IS NULL THEN NULL ELSE MAX(0,?12-started_at-COALESCE(wait_ms,0)) END,delivery_status=?13,cached_input_tokens=?15,response_model=?16,response_service_tier=?17 WHERE id=?14 AND status='running'",
            params![status,crate::now_ms() as i64,self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,code,
                summary.and_then(|s|s.usage.as_ref()).map(|u|u.input_tokens as i64),summary.and_then(|s|s.usage.as_ref()).map(|u|u.output_tokens as i64),
                summary.map(|s|s.thread_id.as_str()),summary.map(|s|s.turn_id.as_str()),summary.map(|s|s.outcome.as_str()).unwrap_or("failed"),
                if model_error.is_some(){code.map(failure_stage)}else if let Some(e)=response_error{Some(failure_stage(&e.code))}else if outcome.cleanup_error.is_some(){Some("cleanup")}else{None},
                outcome.cleanup_error.as_ref().map(|e|e.code.as_str()),summary.and_then(|s|s.first_delta_at).map(|at|at as i64),
                if response_error.is_some(){"failed"}else{"terminal_pending"},self.id.to_string(),summary.and_then(|s|s.usage.as_ref()).and_then(|u|u.cached_input_tokens).map(|n|n as i64),summary.and_then(|s|s.response_model.as_deref()),summary.and_then(|s|s.response_service_tier.as_deref())]);
        if result.is_err() {
            inner.storage_healthy = false;
        }
        inner.active.remove(&self.id);
        self.finished = true;
        inner.changed.notify_waiters();
        result.map_err(storage)?;
        drop(inner);
        if let Some(error) = error {
            let _ = self.owner.record_failure_content(
                self.id,
                None,
                &json!({"error":error}),
                "internal",
                None,
            );
        }
        Ok(())
    }
}
impl Operations {
    pub fn record_delivery(&self, id: uuid::Uuid, delivered: bool) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let result = inner.db.execute(
            "UPDATE requests SET delivery_status=?1 WHERE id=?2",
            params![
                if delivered {
                    "handed_off"
                } else {
                    "disconnected"
                },
                id.to_string()
            ],
        );
        if result.is_err() {
            inner.storage_healthy = false;
            inner.changed.notify_waiters();
        }
        result.map_err(storage)?;
        Ok(())
    }
    pub fn client_usage(&self, access: &ClientAccess) -> Result<Value> {
        let (max_concurrency, active, queued) = {
            let inner = self.inner.lock().unwrap();
            let key = super::api_keys::validate_access(&inner, access)?;
            (
                key.max_concurrency,
                inner
                    .active
                    .values()
                    .filter(|a| a.key_id == Some(access.key_id))
                    .count(),
                inner
                    .pending
                    .iter()
                    .filter(|p| p.client.as_ref().is_some_and(|c| c.key_id == access.key_id))
                    .count(),
            )
        };
        let reader = self.history_db.lock().unwrap();
        let db = reader.unchecked_transaction().map_err(storage)?;
        let from = crate::now_ms().saturating_sub(86_400_000);
        let mut stats=db.query_row("SELECT COUNT(*),SUM(input_tokens),SUM(output_tokens),COUNT(input_tokens),COALESCE(SUM(status='succeeded'),0),COALESCE(SUM(status='failed'),0) FROM requests WHERE key_id=?1 AND started_at>=?2",params![access.key_id.to_string(),from as i64],|r|Ok(json!({"requests":r.get::<_,i64>(0)?,"inputTokens":r.get::<_,Option<i64>>(1)?,"outputTokens":r.get::<_,Option<i64>>(2)?,"usageKnownRequests":r.get::<_,i64>(3)?,"succeeded":r.get::<_,i64>(4)?,"failed":r.get::<_,i64>(5)?}))).map_err(storage)?;
        let key_id = access.key_id.to_string();
        let from_ms = from as i64;
        for (name, value) in
            super::inference::usage(&db, "key_id=?1 AND started_at>=?2", &[&key_id, &from_ms])?
                .as_object()
                .unwrap()
        {
            stats[name] = value.clone();
        }
        Ok(
            json!({"keyId":access.key_id,"from":from,"to":crate::now_ms(),"summary":stats,"active":active,"queued":queued,"maxConcurrency":max_concurrency}),
        )
    }
}
