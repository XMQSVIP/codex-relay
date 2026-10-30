//! Request retention shares the admission lock and never removes active work.

use super::{Operations, storage};
use crate::error::{Error, Result};
use rusqlite::Connection;

const BATCH_SIZE: usize = 1000;

pub(super) fn prune(db: &Connection, days: u32) -> Result<usize> {
    let cutoff = crate::now_ms().saturating_sub(u64::from(days) * 86_400_000) as i64;
    db.execute(
        "DELETE FROM requests WHERE id IN (SELECT id FROM requests WHERE started_at < ?1 AND status NOT IN ('running','queued') ORDER BY started_at LIMIT ?2)",
        rusqlite::params![cutoff, BATCH_SIZE as i64],
    ).map_err(storage)
}

fn prune_all(db: &Connection, days: u32) -> Result<usize> {
    let cutoff = crate::now_ms().saturating_sub(u64::from(days) * 86_400_000) as i64;
    db.execute(
        "DELETE FROM requests WHERE started_at < ?1 AND status NOT IN ('running','queued')",
        [cutoff],
    )
    .map_err(storage)
}

impl Operations {
    pub(crate) async fn maintain_request_history(
        self: std::sync::Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        let mut delay = std::time::Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(delay) => {},
            }
            let ops = self.clone();
            // Await the in-flight batch on shutdown; do not detach database work.
            let result = tokio::task::spawn_blocking(move || ops.prune_request_history()).await;
            delay = if matches!(result, Ok(Ok(BATCH_SIZE))) {
                std::time::Duration::from_millis(100)
            } else {
                std::time::Duration::from_secs(60)
            };
        }
    }

    pub fn prune_request_history(&self) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        let result = prune(&inner.db, inner.settings.retention_days);
        if result.is_err() {
            inner.storage_healthy = false;
        }
        result
    }

    pub fn set_retention(&self, revision: u64, days: u32) -> Result<usize> {
        if !(1..=365).contains(&days) {
            return Err(Error::new(
                "INVALID_INPUT",
                "请求记录保存时长应为 1–365 天的整数。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在退出，请稍后重试。"));
        }
        if inner.settings.revision != revision {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "配置已被其他操作更新，请刷新后重试。",
            ));
        }
        let mut next = inner.settings.clone();
        next.retention_days = days;
        next.revision += 1;
        let tx = inner.db.transaction().map_err(storage)?;
        let deleted = prune_all(&tx, days)?;
        tx.execute(
            "UPDATE settings SET document=?1 WHERE id=1",
            [serde_json::to_string(&next)?],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        inner.settings = next;
        inner.changed.notify_waiters();
        Ok(deleted)
    }
}
