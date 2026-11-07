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
            match &result {
                Ok(_) => "succeeded",
                Err(error) if crate::execution::cancelled(&error.code) => "cancelled",
                Err(_) => "failed",
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

#[cfg(test)]
mod tests {
    use crate::{error::Error, operations::Operations};

    #[test]
    fn explicit_completion_preserves_cancellation_status_and_releases_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        ops.set_global(ops.snapshot()["revision"].as_u64().unwrap(), 1)
            .unwrap();
        for code in [
            "REQUEST_CANCELLED",
            "REQUEST_INTERRUPTED",
            "STREAM_CANCELLED",
            "STREAM_CONSUMER_CLOSED",
            "SERVICE_SHUTDOWN",
            "UPSTREAM_TIMEOUT",
        ] {
            let lease = ops.begin(None, None, "diagnostic", "cancel", true).unwrap();
            let id = lease.id;
            assert_eq!(
                lease
                    .complete::<()>(Err(Error::new(code, "fixture")))
                    .unwrap_err()
                    .code,
                code
            );
            assert_eq!(ops.snapshot()["active"], 0);
            let inner = ops.inner.lock().unwrap();
            let (status, error): (String, String) = inner
                .db
                .query_row(
                    "SELECT status, error_code FROM requests WHERE id=?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                status,
                if code == "UPSTREAM_TIMEOUT" {
                    "failed"
                } else {
                    "cancelled"
                }
            );
            assert_eq!(error, code);
        }
    }
}
