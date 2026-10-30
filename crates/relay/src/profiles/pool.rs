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
        let mut pools = self.response_slots.lock().await;
        let pool = pools.entry(account).or_insert_with(|| vec![primary]);
        let mut fallback = None;
        let mut empty = None;
        // 不等待被另一个请求持有的槽位；先寻找同一密钥与完整历史匹配的续聊。
        // 获取槽位锁和选择在池锁内完成，避免两个请求同时取得同一个空闲执行器。
        for slot in pool.iter() {
            let Ok(worker) = slot.clone().try_lock_owned() else {
                continue;
            };
            if worker
                .as_ref()
                .map(|backend| continuation(backend))
                .transpose()?
                .unwrap_or(false)
            {
                return Ok((slot.clone(), worker));
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
        // 留下其他对话的空闲上下文；池达到配置上限后才回收不匹配的空闲进程。
        if pool.len() < limit {
            let slot = Arc::new(Mutex::new(None));
            let worker = slot.clone().try_lock_owned().unwrap();
            pool.push(slot.clone());
            return Ok((slot, worker));
        }
        if let Some(slot) = fallback {
            return Ok(slot);
        }
        // 空闲回收器也会短暂持有槽位锁，但不占推理额度。已经获准的请求应等待任一
        // 槽位释放，不能误报账户忙，也不能固定等待另一个长对话占用的第一个槽位。
        let waiters: Vec<_> = pool
            .iter()
            .cloned()
            .map(|slot| {
                Box::pin(async move {
                    let worker = slot.clone().lock_owned().await;
                    (slot, worker)
                })
            })
            .collect();
        drop(pools);
        let (claimed, _, _) = futures_util::future::select_all(waiters).await;
        Ok(claimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

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
