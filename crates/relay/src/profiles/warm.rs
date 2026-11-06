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

pub(super) fn inference_config_digest(config: &serde_json::Value) -> Result<[u8; 32]> {
    let mut effective = config.clone();
    // All inference threads/tools choose a model explicitly. Changing the
    // native default must not retire a host with a live client tool callback.
    if let Some(config) = effective.as_object_mut() {
        config.remove("model");
    }
    digest(&effective)
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

// Keep the process and its directory reachable if termination fails or this
// future is cancelled. A later account cleanup can retry the same generation.
pub(super) async fn stop_owned(worker: &mut Option<Arc<Backend>>) -> Result<()> {
    if let Some(backend) = worker.as_ref() {
        if let Some(state) = backend.response_worker.lock().unwrap().as_mut() {
            state.retired = true;
        }
        backend.stop().await?;
    }
    *worker = None;
    Ok(())
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

/// Owned process read lease, independent of scheduling capacity. TurnGuard can
/// retain it across HTTP requests and move it into asynchronous cancellation.
/// Process retirement waits for every sibling Activity before stopping the tree.
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
    fn default_model_changes_keep_inference_hosts_but_other_defaults_do_not() {
        use serde_json::json;
        let mut config =
            json!({"model":"first", "model_provider":"fixture", "model_reasoning_effort":"low"});
        let initial = inference_config_digest(&config).unwrap();
        config["model"] = json!("second");
        assert_eq!(inference_config_digest(&config).unwrap(), initial);
        config.as_object_mut().unwrap().remove("model");
        assert_eq!(inference_config_digest(&config).unwrap(), initial);
        config["model_reasoning_effort"] = json!("high");
        assert_ne!(inference_config_digest(&config).unwrap(), initial);
        config["model_reasoning_effort"] = json!("low");
        config["model_provider"] = json!("other");
        assert_ne!(inference_config_digest(&config).unwrap(), initial);
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn failed_or_cancelled_stop_retains_process_and_directory_for_retry() {
        use std::sync::atomic::Ordering;
        for injected_failure in [true, false] {
            let backend = Arc::new(Backend::test_backend());
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().to_path_buf();
            std::fs::write(path.join("owned"), "fixture").unwrap();
            *backend.response_worker.lock().unwrap() = Some(Worker::new([1; 32], directory));
            let mut worker = Some(backend.clone());
            if injected_failure {
                backend.stop_failure.store(true, Ordering::SeqCst);
                assert_eq!(
                    stop_owned(&mut worker).await.unwrap_err().code,
                    "BACKEND_STOP"
                );
            } else {
                use futures_util::FutureExt;
                let active = backend.response_activity.clone().read_owned().await;
                assert!(stop_owned(&mut worker).now_or_never().is_none());
                drop(active);
            }
            assert!(Arc::ptr_eq(worker.as_ref().unwrap(), &backend));
            assert!(backend.alive().await);
            assert!(path.join("owned").is_file());
            assert!(
                backend
                    .response_worker
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .retired
            );
            backend.stop_failure.store(false, Ordering::SeqCst);
            stop_owned(&mut worker).await.unwrap();
            assert!(worker.is_none());
            assert!(!backend.alive().await);
            assert!(!path.exists());
        }
    }

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
