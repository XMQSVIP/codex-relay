//! 账户内有界进程池：普通 Responses 共享进程，原生长会话与压缩独占执行槽位。
use super::{InferenceRegistry, Profiles, Slot};
use crate::{
    backend::Backend,
    error::{Error, Result},
};
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

pub(super) struct Stopping {
    registry: Arc<SyncMutex<InferenceRegistry>>,
    account: Uuid,
}
impl Drop for Stopping {
    fn drop(&mut self) {
        self.registry.lock().unwrap().stopping.remove(&self.account);
    }
}

impl Profiles {
    pub(super) async fn response_preparation(&self, account: Uuid) -> OwnedMutexGuard<()> {
        let gate = self
            .response_preparations
            .lock()
            .await
            .entry(account)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        gate.lock_owned().await
    }
    /// Process selection is serialized only for preparation. In-flight Responses
    /// share a compatible process and hold their own activity lease afterwards.
    pub(super) async fn shared_response_slot(
        &self,
        account: Uuid,
        limit: usize,
        key: &[u8; 32],
        bytes: usize,
        continuation: impl Fn(&Backend) -> Result<bool>,
    ) -> Result<(Slot, OwnedMutexGuard<Option<Arc<Backend>>>)> {
        self.shared_response_slot_planned(account, limit, key, bytes, None, continuation)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn shared_response_slot_planned(
        &self,
        account: Uuid,
        limit: usize,
        key: &[u8; 32],
        bytes: usize,
        plans: Option<&std::collections::HashMap<u32, super::resources::Offer>>,
        continuation: impl Fn(&Backend) -> Result<bool>,
    ) -> Result<(Slot, OwnedMutexGuard<Option<Arc<Backend>>>)> {
        let primary = self.slot(account).await?;
        loop {
            let mut pools = self.response_slots.lock().await;
            let pool = pools
                .entry(account)
                .or_insert_with(|| vec![primary.clone()]);
            let mut compatible = None;
            let mut idle = None;
            let mut preparing = None;
            for slot in pool.iter() {
                let Ok(worker) = slot.clone().try_lock_owned() else {
                    preparing.get_or_insert_with(|| slot.clone());
                    continue;
                };
                if let Some(backend) = worker.as_ref() {
                    let continues = backend
                        .response_worker
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|state| {
                            plans.and_then(|p| p.get(&backend.pid)).map_or_else(
                                || state.accepts_continuation(key, bytes),
                                |p| state.accepts_plan(key, p.thread.as_deref(), p.bytes),
                            )
                        });
                    if continues && continuation(backend)? {
                        return Ok((slot.clone(), worker));
                    }
                    let accepts = backend
                        .response_worker
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|state| {
                            plans.and_then(|p| p.get(&backend.pid)).map_or_else(
                                || state.accepts(key, bytes),
                                |p| state.accepts_plan(key, p.thread.as_deref(), p.bytes),
                            )
                        });
                    if accepts {
                        if continuation(backend)? {
                            return Ok((slot.clone(), worker));
                        }
                        if compatible.is_none() {
                            compatible = Some((slot.clone(), worker));
                        }
                        continue;
                    }
                    if backend.response_activity.try_write().is_err() {
                        continue;
                    }
                }
                if idle.is_none() {
                    idle = Some((slot.clone(), worker));
                }
            }
            if let Some(slot) = compatible.or(idle) {
                return Ok(slot);
            }
            if pool.len() < limit {
                let slot = Arc::new(Mutex::new(None));
                let worker = slot.clone().lock_owned().await;
                pool.push(slot.clone());
                return Ok((slot, worker));
            }
            drop(pools);
            // Retained callbacks must not occupy every generation until their TTL.
            // Cancel waiting work under resource pressure; active siblings keep their leases.
            self.continuations.cancel_oldest(account);
            // Shared preparation is serialized per account. Locked slots here
            // belong to exclusive sessions or cleanup, not a duplicate startup.
            if let Some(slot) = preparing {
                let worker = slot.lock().await;
                drop(worker);
                continue;
            }
            // Only cleanup or an already admitted sibling can occupy the last
            // slot here. The caller's cancellation also covers this wait.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    pub(super) fn begin_stop(&self, account: Uuid) -> Result<Stopping> {
        let mut registry = self.inference.lock().unwrap();
        if !registry.stopping.insert(account) {
            return Err(Error::new("BUSY", "账户实例正在停止。"));
        }
        if let Some(requests) = registry.active.get(&account) {
            for token in requests.values() {
                token.cancel();
            }
        }
        if let Some(requests) = registry.retained.get(&account) {
            for token in requests.values() {
                token.cancel();
            }
        }
        Ok(Stopping {
            registry: self.inference.clone(),
            account,
        })
    }

    pub(super) async fn response_slot(
        &self,
        account: Uuid,
        limit: usize,
        continuation: impl Fn(&Backend) -> Result<bool>,
    ) -> Result<(Slot, OwnedMutexGuard<Option<Arc<Backend>>>)> {
        let primary = self.slot(account).await?;
        loop {
            let mut pools = self.response_slots.lock().await;
            let pool = pools
                .entry(account)
                .or_insert_with(|| vec![primary.clone()]);
            let mut fallback = None;
            let mut empty = None;
            // Shared requests release the slot during execution. Its lock alone
            // does not permit an exclusive caller to stop or replace that host.
            for slot in pool.iter() {
                let Ok(worker) = slot.clone().try_lock_owned() else {
                    continue;
                };
                if let Some(backend) = worker.as_ref() {
                    if backend.response_activity.try_write().is_err() {
                        continue;
                    }
                    if continuation(backend)? {
                        return Ok((slot.clone(), worker));
                    }
                }
                if worker.is_none() && empty.is_none() {
                    empty = Some((slot.clone(), worker));
                } else if fallback.is_none() {
                    fallback = Some((slot.clone(), worker));
                }
            }
            if let Some(slot) = empty {
                return Ok(slot);
            }
            // Preserve other conversations while the bounded pool can grow.
            if pool.len() < limit {
                let slot = Arc::new(Mutex::new(None));
                let worker = slot.clone().try_lock_owned().unwrap();
                pool.push(slot.clone());
                return Ok((slot, worker));
            }
            if let Some(slot) = fallback {
                return Ok(slot);
            }
            drop(pools);
            // Revoke a parked turn under pressure without holding its slot.
            // Cleanup and active siblings must drain before exclusive selection.
            self.continuations.cancel_oldest(account);
            // Both activity leases and slot locks can become available. Rescan
            // all slots rather than waiting behind one long conversation; the
            // caller's cancellation covers this wait.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[cfg(unix)]
    #[tokio::test]
    async fn exclusive_selection_skips_shared_activity_and_prefers_an_empty_slot() {
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let id = profiles.create("mixed pool").await.unwrap().id;
        let shared_slot = profiles.slot(id).await.unwrap();
        let backend = Arc::new(Backend::test_backend());
        *shared_slot.lock().await = Some(backend.clone());
        let activity = backend.response_activity.clone().read_owned().await;
        let empty_slot = Arc::new(Mutex::new(None));
        profiles
            .response_slots
            .lock()
            .await
            .insert(id, vec![shared_slot.clone(), empty_slot.clone()]);
        let (selected, worker) = profiles
            .response_slot(id, 2, |_| {
                panic!("an active host cannot be an exclusive match")
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&selected, &empty_slot));
        assert!(shared_slot.try_lock().is_ok());
        drop(worker);
        drop(activity);
        profiles.stop(id).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_exclusive_pool_wait_is_cancellable_without_locking_shared_host() {
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let id = profiles.create("active host").await.unwrap().id;
        let slot = profiles.slot(id).await.unwrap();
        let backend = Arc::new(Backend::test_backend());
        *slot.lock().await = Some(backend.clone());
        let activity = backend.response_activity.clone().read_owned().await;
        let cancel = CancellationToken::new();
        {
            let waiting = async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {},
                    result = profiles.response_slot(id, 1, |_| Ok(false)) => {
                        drop(result.unwrap());
                        panic!("an active shared host was claimed exclusively");
                    }
                }
            };
            tokio::pin!(waiting);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiting)
                    .await
                    .is_err()
            );
            assert!(slot.try_lock().is_ok());
            assert!(profiles.response_slots.try_lock().is_ok());
            cancel.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap();
        }
        drop(activity);
        let (selected, worker) = profiles.response_slot(id, 1, |_| Ok(false)).await.unwrap();
        assert!(Arc::ptr_eq(&selected, &slot));
        drop(worker);
        profiles.stop(id).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exclusive_pressure_releases_slot_while_retained_turn_cleans_up() {
        use crate::{
            operations::ClientAccess,
            responses::{
                Request,
                continuation::{TurnGuard, Waiting},
                reuse::Session,
            },
            rpc::Rpc,
        };
        use serde_json::{Value, json};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let directory = tempfile::tempdir().unwrap();
        let profiles = Arc::new(
            Profiles::open(directory.path().join("accounts"))
                .await
                .unwrap(),
        );
        let id = profiles.create("parked host").await.unwrap().id;
        let slot = profiles.slot(id).await.unwrap();
        let (client, peer) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let mut backend = Backend::test_backend();
        backend.rpc = Rpc::from_io(read, write);
        let backend = Arc::new(backend);
        *backend.response_worker.lock().unwrap() = Some(super::super::warm::Worker::new(
            [0; 32],
            tempfile::tempdir_in(directory.path()).unwrap(),
        ));
        *slot.lock().await = Some(backend.clone());
        let events = backend.rpc.subscribe_thread("thread");
        let waiting = Waiting::new(TurnGuard::new(
            backend.rpc.clone(),
            "thread".into(),
            "turn".into(),
            events,
            Some(super::super::warm::Activity {
                backend: backend.clone(),
                slot: slot.clone(),
                _guard: backend.response_activity.clone().read_owned().await,
            }),
        ));
        let access = ClientAccess {
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
            fast_mode: false,
        };
        let request = Request::parse(json!({"model":"fixture","input":"test"})).unwrap();
        let session = Session::new(
            "thread".into(),
            directory.path().into(),
            &access,
            &request,
            &request.input,
            &[],
        )
        .unwrap();
        let handoff = super::super::continuations::Handoff::default();
        let provider = serde_json::from_value(json!({"id":"fixture","model":"fixture","baseUrl":"http://127.0.0.1:9/v1","auth":"none","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
        profiles
            .retain_tools(
                id,
                access,
                provider,
                "fixture".into(),
                [0; 32],
                session,
                waiting,
                &handoff,
            )
            .unwrap();
        handoff.publish();
        handoff.finish(true);

        let (interrupted_tx, interrupted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            let interrupt: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(interrupt["method"], "turn/interrupt");
            interrupted_tx.send(()).unwrap();
            release_rx.await.unwrap();
            write.write_all(format!("{}\n{}\n",
                json!({"id":interrupt["id"],"result":{}}),
                json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}}),
            ).as_bytes()).await.unwrap();
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                write
                    .write_all(format!("{}\n", json!({"id":frame["id"],"result":{}})).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let selected_profiles = profiles.clone();
        let selection = tokio::spawn(async move {
            selected_profiles
                .response_slot(id, 1, |_| Ok(false))
                .await
                .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), interrupted_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!selection.is_finished());
        assert!(
            slot.try_lock().is_ok(),
            "waiting must not obstruct native cleanup"
        );
        assert!(profiles.response_slots.try_lock().is_ok());
        release_tx.send(()).unwrap();
        let (selected, worker) = tokio::time::timeout(std::time::Duration::from_secs(5), selection)
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&selected, &slot));
        assert!(backend.response_activity.try_write().is_ok());
        drop(worker);
        profiles.stop(id).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn slots_are_independent_bounded_and_reusable_after_release() {
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let id = profiles.create("pool").await.unwrap().id;
        let (first, guard_a) = profiles.response_slot(id, 2, |_| Ok(false)).await.unwrap();
        let (second, guard_b) = profiles.response_slot(id, 2, |_| Ok(false)).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                profiles.response_slot(id, 2, |_| Ok(false))
            )
            .await
            .is_err()
        );
        drop(guard_a);
        let (again, guard_c) = profiles.response_slot(id, 2, |_| Ok(false)).await.unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        drop((guard_b, guard_c));
        assert_eq!(profiles.response_slots.lock().await[&id].len(), 2);
        profiles.stop(id).await.unwrap();
        profiles.delete(id).await.unwrap();
        assert!(!profiles.response_slots.lock().await.contains_key(&id));
    }

    #[tokio::test]
    async fn cancellation_and_registration_are_per_request_but_stop_covers_account() {
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let id = profiles.create("cancel").await.unwrap().id;
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let first = profiles.register_inference(id, a.clone()).unwrap();
        let second = profiles.register_inference(id, b.clone()).unwrap();
        let waiting_cancel = CancellationToken::new();
        let retained = profiles
            .register_retained(id, waiting_cancel.clone())
            .unwrap();
        assert_eq!(profiles.inference.lock().unwrap().active[&id].len(), 2);
        a.cancel();
        drop(first);
        assert!(!b.is_cancelled());
        assert_eq!(profiles.inference.lock().unwrap().active[&id].len(), 1);
        let stopping = profiles.begin_stop(id).unwrap();
        assert!(b.is_cancelled());
        assert!(waiting_cancel.is_cancelled());
        assert!(
            profiles
                .register_inference(id, CancellationToken::new())
                .is_err()
        );
        drop(second);
        drop(retained);
        assert!(profiles.inference.lock().unwrap().active.is_empty());
        assert!(profiles.inference.lock().unwrap().retained.is_empty());
        drop(stopping);
        drop(
            profiles
                .register_inference(id, CancellationToken::new())
                .unwrap(),
        );
        assert!(profiles.inference.lock().unwrap().active.is_empty());
    }

    #[tokio::test]
    async fn cleanup_wait_claims_any_free_slot_instead_of_waiting_for_first_dialogue() {
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let id = profiles.create("reaper").await.unwrap().id;
        let (_, first) = profiles.response_slot(id, 2, |_| Ok(false)).await.unwrap();
        let (second_slot, second) = profiles.response_slot(id, 2, |_| Ok(false)).await.unwrap();
        let waiting = profiles.response_slot(id, 2, |_| Ok(false));
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(second);
        let (slot, guard) = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&slot, &second_slot));
        drop((first, guard));
    }
}
