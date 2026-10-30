//! Current database initialization and interrupted-request recovery.

use super::{Inner, Operations, Settings, runtime, schema};
use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

pub(super) struct DirectoryLock(pub(super) std::fs::File);

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // Release explicitly: a concurrently spawned child may temporarily hold
        // an inherited descriptor until exec closes it on Unix.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(super) fn storage(_: rusqlite::Error) -> Error {
    Error::new("OPERATIONS_STORAGE", "运行数据读写失败。")
}

impl Operations {
    pub(crate) async fn run_history_query<T: Send + 'static>(
        self: &Arc<Self>,
        query: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        // Queue on the async side instead of occupying blocking threads with a mutex wait.
        let permit = self
            .history_queries
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::new("OPERATIONS_STORAGE", "统计读取已停止。"))?;
        let ops = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            query(&ops)
        })
        .await
        .map_err(|_| Error::new("OPERATIONS_STORAGE", "统计读取任务失败。"))?
    }

    pub fn open(root: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(root)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("operations.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| Error::new("OPERATIONS_LOCKED", "已有管理服务使用这份运行数据。"))?;
        let lock = DirectoryLock(lock);
        let db = Connection::open(root.join("operations.sqlite3")).map_err(storage)?;
        db.busy_timeout(Duration::from_millis(500))
            .map_err(storage)?;
        schema::initialize(&db)?;
        db.execute("UPDATE requests SET request_content=NULL,response_content=NULL WHERE json_extract(request_content,'$.debug')=1 OR json_extract(response_content,'$.debug')=1", []).map_err(storage)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(storage)?;
        let runtime = runtime::load(&db)?;
        let document: Option<String> = db
            .query_row("SELECT document FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        let settings = match document {
            Some(s) => serde_json::from_str(&s)?,
            None => Settings::default(),
        };
        db.execute(
            "INSERT OR IGNORE INTO settings VALUES(1,?1)",
            [serde_json::to_string(&settings)?],
        )
        .map_err(storage)?;
        // A previous process cannot still own leases because the data-directory lock is held.
        db.execute("UPDATE requests SET status='interrupted', finished_at=?1,error_code='SERVICE_RESTARTED' WHERE status IN ('running','queued')",[crate::now_ms() as i64]).map_err(storage)?;
        super::retention::prune(&db, settings.retention_days)?;
        // WAL snapshots let history queries run without holding the admission lock.
        let history_db = Connection::open_with_flags(
            root.join("operations.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(storage)?;
        history_db
            .busy_timeout(Duration::from_millis(500))
            .map_err(storage)?;
        let changed = Arc::new(Notify::new());
        Ok(Arc::new(Self {
            locations: crate::account_location::Manager::open(root)?,
            inner: Mutex::new(Inner {
                debug_epoch: 0,
                debug_enabled: false,
                runtime,
                proxy_changes: HashMap::new(),
                db,
                settings,
                active: HashMap::new(),
                cursor: HashMap::new(),
                storage_healthy: true,
                pending: VecDeque::new(),
                changed: changed.clone(),
                draining: false,
            }),
            history_db: Mutex::new(history_db),
            history_queries: Arc::new(tokio::sync::Semaphore::new(1)),
            changed,
            _lock: lock,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn directory_lock_releases_even_when_a_duplicate_descriptor_survives() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let duplicate = ops._lock.0.try_clone().unwrap();
        assert!(matches!(Operations::open(dir.path()), Err(e) if e.code == "OPERATIONS_LOCKED"));
        drop(ops);
        let reopened = Operations::open(dir.path()).unwrap();
        drop(duplicate);
        assert!(matches!(Operations::open(dir.path()), Err(e) if e.code == "OPERATIONS_LOCKED"));
        drop(reopened);
        assert!(Operations::open(dir.path()).is_ok());
    }

    #[test]
    fn statistics_and_account_metrics_do_not_take_the_admission_lock() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let guard = ops.inner.lock().unwrap();
        let reader = ops.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let result = reader.statistics(Default::default()).and_then(|_| {
                reader.account_metrics(crate::operations::AccountMetricsQuery {
                    from: 0,
                    to: crate::now_ms() as i64,
                    account_id: None,
                })
            });
            tx.send(result).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        drop(guard);
        thread.join().unwrap();
        result
            .expect("history queries waited for the admission lock")
            .unwrap();
    }

    #[test]
    fn a_history_snapshot_does_not_block_admission_or_settlement() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let reader = ops.history_db.lock().unwrap();
        let tx = reader.unchecked_transaction().unwrap();
        let count = |db: &Connection| {
            db.query_row::<i64, _, _>("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count(&tx), 0);
        ops.begin(None, None, "diagnostic", "concurrent-write", false)
            .unwrap()
            .complete(Ok(()))
            .unwrap();
        assert_eq!(count(&tx), 0, "read snapshot changed during a query");
        tx.commit().unwrap();
        assert_eq!(count(&reader), 1);
        assert!(
            reader.execute("DELETE FROM requests", []).is_err(),
            "history connection must be read-only"
        );
    }
}
