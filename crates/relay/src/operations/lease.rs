//! Execution settlement and capacity release, including dropped requests.

use super::{Lease, storage};
use crate::error::Result;
use rusqlite::params;

impl Lease {
    pub(super) fn settle(&mut self, status: &str, error: Option<&str>) -> Result<()> {
        if status == "succeeded" {
            self.owner.quota_success(self.id);
        }
        let mut inner = self.owner.inner.lock().unwrap();
        super::rate_limit::finished(
            &mut inner,
            self.account_id,
            self.provider.as_ref(),
            self.id,
            status == "succeeded",
            error,
        );
        super::scheduler::finished(&mut inner, self.id, status == "succeeded", error, None);
        inner.active.remove(&self.id);
        self.finished = true;
        let result=inner.db.execute("UPDATE requests SET status=?1,finished_at=?2,duration_ms=?3,error_code=?4 WHERE id=?5 AND status='running'",params![status,crate::now_ms() as i64,self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,error,self.id.to_string()]);
        if result.is_err() {
            inner.storage_healthy = false;
        }
        inner.changed.notify_waiters();
        result.map_err(storage)?;
        Ok(())
    }
    pub fn complete<T>(mut self, result: Result<T>) -> Result<T> {
        self.settle(
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
            result.as_ref().err().map(|e| e.code.as_str()),
        )?;
        if let Err(error) = &result {
            let _ = self.owner.record_failure_content(
                self.id,
                None,
                &serde_json::json!({"error":error}),
                "internal",
                None,
            );
        }
        result
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.settle("cancelled", Some("REQUEST_CANCELLED"));
        }
    }
}
