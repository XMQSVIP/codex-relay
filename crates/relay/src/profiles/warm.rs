//! 有界的账户内执行器与多线程续聊缓存；在途读租约保护进程生命周期。
use super::Slot;
use crate::{backend::Backend, error::Result, responses::reuse::Session};
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
// Legacy and exclusive-tool hosts retain their cumulative limits.
pub(crate) const MAX_THREADS: usize = 32;
pub(crate) const MAX_CONTEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn digest(value: &impl serde::Serialize) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(value)?).into())
}

pub(crate) struct Worker {
    pub key: [u8; 32],
    pub config: Option<[u8; 32]>,
    pub route: Option<[u8; 32]>,
    pub directory: tempfile::TempDir,
    pub session: Option<Session>,
    pub sessions: Vec<Session>,
    pub(super) tool_session: Option<super::native_tools::Session>,
    pub threads: usize,
    pub context_bytes: usize,
    pub resources: super::resources::Ledger,
    pub shared: bool,
    pub managed_contexts: bool,
    pub standby: bool,
    pub retired: bool,
    pub retained: std::collections::HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>,
    idle_since: Instant,
    reaper_started: bool,
}

impl Worker {
    pub fn new(key: [u8; 32], directory: tempfile::TempDir) -> Self {
        Self {
            key,
            config: None,
            route: None,
            directory,
            session: None,
            sessions: Vec::new(),
            tool_session: None,
            threads: 0,
            context_bytes: 0,
            resources: Default::default(),
            shared: false,
            managed_contexts: false,
            standby: false,
            retired: false,
            retained: Default::default(),
            idle_since: Instant::now(),
            reaper_started: false,
        }
    }
    pub fn matches(&self, key: &[u8; 32]) -> bool {
        self.key == *key
            && !self.retired
            && (self.standby
                || !self.retained.is_empty()
                || self.idle_since.elapsed() < IDLE_TIMEOUT)
    }
    pub fn accepts(&self, key: &[u8; 32], bytes: usize) -> bool {
        if crate::native_cache::enabled() && self.managed_contexts {
            return self.matches(key) && self.resources.room(None, bytes);
        }
        self.shared
            && self.matches(key)
            && self.threads < MAX_THREADS
            && self.context_bytes.saturating_add(bytes) <= MAX_CONTEXT_BYTES
    }
    pub fn accepts_continuation(&self, key: &[u8; 32], bytes: usize) -> bool {
        // At the thread limit the generation is already scheduled to drain.
        // Do not select its cached thread and wait behind the retirement writer.
        self.accepts(key, bytes)
    }
    pub(super) fn accepts_plan(&self, key: &[u8; 32], thread: Option<&str>, bytes: usize) -> bool {
        self.managed_contexts && self.matches(key) && self.resources.room(thread, bytes)
    }
    pub fn idle(&mut self) -> bool {
        self.idle_since = Instant::now();
        !std::mem::replace(&mut self.reaper_started, true)
    }
    pub fn close(mut self) -> Result<()> {
        self.session.take();
        self.sessions.clear();
        self.directory.close()?;
        Ok(())
    }
}

pub(super) fn retire(slot: &Slot, backend: &Arc<Backend>) {
    if let Some(state) = backend.response_worker.lock().unwrap().as_mut() {
        state.retired = true;
    }
    recycle_after_activity(slot, backend);
}

// Capacity exhaustion stops new reservations through accepts(), but already
// reserved requests may finish preparation and execute in this generation.
pub(super) fn recycle_after_activity(slot: &Slot, backend: &Arc<Backend>) {
    if let Some(state) = backend.response_worker.lock().unwrap().as_ref() {
        for cancel in state.retained.values() {
            cancel.cancel();
        }
    }
    let slot = slot.clone();
    let backend = backend.clone();
    tokio::spawn(async move {
        // Do not hold the selection lock while siblings drain: a new request
        // may need another generation/configuration in a different pool slot.
        let idle = backend.response_activity.write().await;
        drop(idle);
        let mut current = slot.lock().await;
        if current.as_ref().is_some_and(|b| Arc::ptr_eq(b, &backend)) {
            match backend.stop().await {
                Ok(()) => *current = None,
                Err(error) => eprintln!(
                    "{}",
                    serde_json::json!({"event":"retired_worker_cleanup_failed","code":error.code})
                ),
            }
        }
    });
}

/// Owned native lifetime protection also survives a request future being dropped.
pub(crate) struct Activity {
    pub backend: Arc<Backend>,
    pub slot: Slot,
    pub _guard: tokio::sync::OwnedRwLockReadGuard<()>,
}
impl Activity {
    pub fn retire(&self) {
        retire(&self.slot, &self.backend);
    }
}

pub(super) fn reap_when_idle(slot: &Slot, backend: &Arc<Backend>) {
    let slot = Arc::downgrade(slot);
    let backend = Arc::downgrade(backend);
    tokio::spawn(async move {
        let mut remaining = IDLE_TIMEOUT;
        loop {
            tokio::time::sleep(remaining).await;
            let (Some(slot), Some(backend)) = (slot.upgrade(), backend.upgrade()) else {
                return;
            };
            // The request owns this lock for its entire execution; never reap an active turn.
            let mut current = slot.lock().await;
            if !current.as_ref().is_some_and(|b| Arc::ptr_eq(b, &backend)) {
                return;
            }
            remaining = {
                let state = backend.response_worker.lock().unwrap();
                let Some(state) = state.as_ref() else {
                    return;
                };
                // Keep a healthy designated host. Resource limits and explicit
                // retirement still drain it; age alone does not invalidate it.
                if state.standby && !state.retired {
                    remaining = IDLE_TIMEOUT;
                    continue;
                }
                IDLE_TIMEOUT.saturating_sub(state.idle_since.elapsed())
            };
            if remaining.is_zero() {
                if backend.response_activity.try_write().is_err() {
                    // A running request is not idle, even if no turn has
                    // finished since process creation. Recheck after it drains.
                    remaining = Duration::from_secs(1);
                    continue;
                }
                if let Err(error) = backend.stop().await {
                    eprintln!(
                        "{}",
                        serde_json::json!({"event":"idle_worker_cleanup_failed","code":error.code})
                    );
                    // Retain ownership and retry; a failed stop must not make a live
                    // process disappear from the pool or from service shutdown.
                    if let Some(state) = backend.response_worker.lock().unwrap().as_mut() {
                        state.retired = true;
                    }
                    remaining = Duration::from_secs(5);
                    continue;
                }
                *current = None;
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_hosts_bound_native_contexts_and_refuse_retired_generations() {
        let mut worker = Worker::new([1; 32], tempfile::tempdir().unwrap());
        worker.shared = true;
        worker.managed_contexts = true;
        assert!(worker.accepts(&[1; 32], 10));
        worker.threads = MAX_THREADS;
        if crate::native_cache::enabled() {
            for _ in 0..worker.resources.limits.threads {
                worker.resources.reserve(None, 0, None).unwrap();
            }
        }
        assert!(!worker.accepts(&[1; 32], 10));
        worker.threads = 1;
        worker.resources = Default::default();
        worker.context_bytes = MAX_CONTEXT_BYTES - 1;
        if crate::native_cache::enabled() {
            worker
                .resources
                .reserve(None, worker.resources.limits.bytes - 1, None)
                .unwrap();
        }
        assert!(!worker.accepts(&[1; 32], 2));
        worker.context_bytes = 0;
        worker.resources = Default::default();
        worker.retired = true;
        assert!(!worker.accepts(&[1; 32], 1));
    }
    #[test]
    fn standby_survives_idle_but_still_obeys_generation_and_context_limits() {
        let mut worker = Worker::new([1; 32], tempfile::tempdir().unwrap());
        worker.shared = true;
        worker.managed_contexts = true;
        worker.standby = true;
        worker.idle_since = Instant::now() - IDLE_TIMEOUT * 2;
        assert!(worker.accepts(&[1; 32], 1));
        worker.standby = false;
        assert!(!worker.accepts(&[1; 32], 1));
        worker.standby = true;
        worker.threads = MAX_THREADS;
        if crate::native_cache::enabled() {
            for _ in 0..worker.resources.limits.threads {
                worker.resources.reserve(None, 0, None).unwrap();
            }
        }
        assert!(!worker.accepts(&[1; 32], 1));
        worker.threads = 0;
        worker.resources = Default::default();
        worker.idle_since = Instant::now()
            .checked_sub(Duration::from_secs(7 * 24 * 60 * 60))
            .unwrap_or(worker.idle_since);
        assert!(
            worker.accepts(&[1; 32], 1),
            "a healthy standby has no fixed maximum age"
        );
    }

    #[test]
    fn exclusive_tool_hosts_keep_legacy_capacity_limits() {
        let mut worker = Worker::new([1; 32], tempfile::tempdir().unwrap());
        worker.shared = true;
        worker.threads = MAX_THREADS;
        assert!(!worker.accepts(&[1; 32], 1));
        worker.threads = 0;
        worker.context_bytes = MAX_CONTEXT_BYTES;
        assert!(!worker.accepts(&[1; 32], 1));
        assert!(!worker.accepts_plan(&[1; 32], None, 0));
    }

    #[test]
    fn worker_idle_age_bounds_extra_hosts_and_cleanup_releases_directory() {
        let mut worker = Worker::new([1; 32], tempfile::tempdir().unwrap());
        assert!(worker.matches(&[1; 32]));
        assert!(!worker.matches(&[2; 32]));
        assert!(worker.idle());
        assert!(!worker.idle(), "only one reaper per worker");
        worker.idle_since = Instant::now() - IDLE_TIMEOUT;
        assert!(!worker.matches(&[1; 32]));
        worker.idle();
        assert!(worker.matches(&[1; 32]));
        let path = worker.directory.path().to_path_buf();
        worker.close().unwrap();
        assert!(!path.exists());
    }
}
