//! Bounded ownership of native turns between client requests.
use super::Profiles;
use crate::{
    error::{Error, Result},
    operations::{ClientAccess, Operations},
    provider::Provider,
    responses::{Request, continuation::Waiting, reuse::Session},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Store {
    entries: Mutex<HashMap<Uuid, Arc<Entry>>>,
}
#[derive(Default)]
pub(crate) struct Observation(Mutex<Vec<Arc<Entry>>>);
impl std::fmt::Debug for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeContinuationObservation")
    }
}
impl Observation {
    pub(crate) fn check(&self) -> Result<()> {
        if self
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry.claimed.load(Ordering::Acquire))
        {
            Err(busy())
        } else {
            Ok(())
        }
    }
}
pub(crate) struct Entry {
    id: Uuid,
    account: Uuid,
    pub session: Session,
    pub pid: u32,
    key: [u8; 32],
    created: Instant,
    expires: Instant,
    cancel: CancellationToken,
    claimed: AtomicBool,
    published: AtomicBool,
    waiting: Mutex<Option<Waiting>>,
    backend: std::sync::Weak<crate::backend::Backend>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
pub(crate) struct Handoff(Mutex<Option<Ticket>>);
impl std::fmt::Debug for Handoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeHandoff")
    }
}
struct Ticket {
    entry: Arc<Entry>,
    completed: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.completed {
            self.entry.cancel.cancel();
        }
    }
}
impl Handoff {
    pub fn publish(&self) {
        if let Some(t) = self.0.lock().unwrap().as_ref() {
            t.entry.published.store(true, Ordering::Release);
        }
    }
    pub fn finish(&self, success: bool) {
        if let Some(mut ticket) = self.0.lock().unwrap().take() {
            ticket.completed = success;
        }
    }
}

pub(crate) struct Claim {
    store: Arc<Store>,
    entry: Arc<Entry>,
}
impl Drop for Claim {
    fn drop(&mut self) {
        self.store.remove(&self.entry);
    }
}
impl Store {
    pub(super) fn cancel_oldest(&self, account: Uuid) {
        let entries = self.entries.lock().unwrap();
        if let Some(old) = entries
            .values()
            .filter(|e| {
                e.account == account
                    && !e.claimed.load(Ordering::Acquire)
                    && !e.cancel.is_cancelled()
            })
            .min_by_key(|e| e.created)
        {
            old.cancel.cancel();
        }
    }

    pub fn find(
        &self,
        account: Uuid,
        access: &ClientAccess,
        request: &Request,
        history: &[Value],
        key: &[u8; 32],
    ) -> Result<Option<Arc<Entry>>> {
        let entries: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.account == account && &e.key == key)
            .cloned()
            .collect();
        for entry in entries {
            if !entry.cancel.is_cancelled()
                && entry.expires > Instant::now()
                && entry
                    .session
                    .continuation(access, request, history)?
                    .is_some()
            {
                if entry.claimed.load(Ordering::Acquire) {
                    return Err(busy());
                }
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }
    pub fn claim(self: &Arc<Self>, entry: &Arc<Entry>) -> Result<Option<(Waiting, Claim)>> {
        let entries = self.entries.lock().unwrap();
        if entry.claimed.load(Ordering::Acquire) {
            return Err(busy());
        }
        if entry.cancel.is_cancelled()
            || entry.expires <= Instant::now()
            || !entries.contains_key(&entry.id)
        {
            return Ok(None);
        }
        if !entry.published.load(Ordering::Acquire) || entry.claimed.load(Ordering::Acquire) {
            return Err(Error::new(
                "RESPONSE_CONTINUATION_BUSY",
                "这组工具结果的原生续接正在处理中。",
            ));
        }
        let mut state = entry.waiting.lock().unwrap();
        if state.as_mut().is_none_or(|waiting| !waiting.unchanged()) {
            entry.cancel.cancel();
            return Ok(None);
        }
        entry.claimed.store(true, Ordering::Release);
        let waiting = state.take();
        drop(state);
        if let Some(backend) = entry.backend.upgrade()
            && let Some(worker) = backend.response_worker.lock().unwrap().as_mut()
        {
            worker.retained.remove(&entry.id);
        }
        entry.changed.notify_waiters();
        Ok(waiting.map(|waiting| {
            (
                waiting,
                Claim {
                    store: self.clone(),
                    entry: entry.clone(),
                },
            )
        }))
    }
    fn remove(&self, entry: &Entry) {
        self.entries.lock().unwrap().remove(&entry.id);
        if let Some(backend) = entry.backend.upgrade()
            && let Some(worker) = backend.response_worker.lock().unwrap().as_mut()
        {
            worker.retained.remove(&entry.id);
        }
        entry.changed.notify_waiters();
    }
}

fn busy() -> Error {
    Error::new(
        "RESPONSE_CONTINUATION_BUSY",
        "这组工具结果的原生续接正在处理中。",
    )
}

fn make_room(entries: &HashMap<Uuid, Arc<Entry>>, account: Uuid) {
    let pending = |entry: &&Arc<Entry>| {
        !entry.claimed.load(Ordering::Acquire) && !entry.cancel.is_cancelled()
    };
    let account_count = entries
        .values()
        .filter(pending)
        .filter(|e| e.account == account)
        .count();
    let total = entries.values().filter(pending).count();
    if (account_count >= 32 || total >= 1024)
        && let Some(old) = entries
            .values()
            .filter(pending)
            .filter(|e| account_count < 32 || e.account == account)
            .min_by_key(|e| e.created)
    {
        old.cancel.cancel();
    }
}

impl Profiles {
    /// Observe competing claims while ordinary admission waits. Dropping the
    /// admission future removes its queue entry through the existing guard.
    pub(crate) async fn continuation_admission<T>(
        &self,
        access: &ClientAccess,
        request: &Request,
        history: &[Value],
        admission: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        if !crate::native_cache::enabled() {
            return admission.await;
        }
        // Keep the observed candidates alive through admission. A winning request
        // may finish and remove its entry before a queued competitor is polled.
        // Its claimed bit still prevents that competitor from replaying history.
        let entries: Vec<_> = self
            .continuations
            .entries
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let mut observed = Vec::new();
        for entry in entries {
            if !entry.cancel.is_cancelled()
                && entry
                    .session
                    .continuation(access, request, history)?
                    .is_some()
            {
                observed.push(entry);
            }
        }
        *request.native_observation.0.lock().unwrap() = observed.clone();
        let check = || {
            if observed
                .iter()
                .any(|entry| entry.claimed.load(Ordering::Acquire))
            {
                Err(busy())
            } else {
                Ok(())
            }
        };
        tokio::pin!(admission);
        let mut checks = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                biased;
                _=checks.tick()=>{check()?;self.check_continuation_busy(access,request,history)?;},
                result=&mut admission=>{check()?;return result;},
            }
        }
    }

    pub(crate) fn check_continuation_busy(
        &self,
        access: &ClientAccess,
        request: &Request,
        history: &[Value],
    ) -> Result<()> {
        if !crate::native_cache::enabled() {
            return Ok(());
        }
        request.native_observation.check()?;
        let entries: Vec<_> = self
            .continuations
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.claimed.load(Ordering::Acquire) && !e.cancel.is_cancelled())
            .cloned()
            .collect();
        for entry in entries {
            if entry
                .session
                .continuation(access, request, history)?
                .is_some()
            {
                return Err(busy());
            }
        }
        Ok(())
    }
}

impl Profiles {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn retain_tools(
        &self,
        account: Uuid,
        access: ClientAccess,
        provider: Provider,
        model: String,
        key: [u8; 32],
        session: Session,
        waiting: Waiting,
        handoff: &Handoff,
    ) -> Result<()> {
        let backend = waiting
            .guard
            .owner
            .as_ref()
            .expect("shared native lifetime")
            .backend
            .clone();
        let expires = Instant::now() + Duration::from_secs(300);
        let cancel = CancellationToken::new();
        let registration = self.register_retained(account, cancel.clone())?;
        let entry = Arc::new(Entry {
            id: Uuid::new_v4(),
            account,
            session,
            pid: backend.pid,
            key,
            created: Instant::now(),
            expires,
            cancel,
            claimed: AtomicBool::new(false),
            published: AtomicBool::new(false),
            waiting: Mutex::new(Some(waiting)),
            backend: Arc::downgrade(&backend),
            changed: Default::default(),
        });
        {
            let mut entries = self.continuations.entries.lock().unwrap();
            make_room(&entries, account);
            if let Some(worker) = backend.response_worker.lock().unwrap().as_mut() {
                if worker.retired {
                    entry.cancel.cancel();
                }
                worker.retained.insert(entry.id, entry.cancel.clone());
            } else {
                entry.cancel.cancel();
            }
            entries.insert(entry.id, entry.clone());
        }
        *handoff.0.lock().unwrap() = Some(Ticket {
            entry: entry.clone(),
            completed: false,
        });
        let store = self.continuations.clone();
        let ops: Option<Arc<Operations>> = self.proxy_settings.clone();
        tokio::spawn(async move {
            let _registration = registration;
            let mut checks = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    _=entry.cancel.cancelled()=>break,
                    _=entry.changed.notified()=>{},
                    _=checks.tick()=>{},
                }
                if entry.claimed.load(Ordering::Acquire) {
                    return;
                }
                if entry.backend.upgrade().is_some_and(|backend| {
                    backend
                        .response_worker
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|worker| worker.resources.expire_waiter(&entry.session.thread))
                }) {
                    break;
                }
                if Instant::now() >= entry.expires
                    || ops.as_ref().is_some_and(|ops| {
                        ops.check_retained_inference_route(
                            account,
                            access.key_id,
                            &provider,
                            &model,
                        )
                        .is_err()
                    })
                {
                    break;
                }
                let mut state = entry.waiting.lock().unwrap();
                if let Some(waiting) = state.as_mut() {
                    if !waiting.unchanged() {
                        break;
                    }
                }
            }
            // Claim and expiry serialize through the book lock. A claimed request owns
            // the turn guard and is cancelled by its own account registration.
            let waiting = {
                let _entries = store.entries.lock().unwrap();
                if entry.claimed.load(Ordering::Acquire) {
                    return;
                }
                entry.cancel.cancel();
                entry.waiting.lock().unwrap().take()
            };
            store.remove(&entry);
            drop(waiting); // TurnGuard confirms interruption while retaining the process lease.
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn entry() -> Arc<Entry> {
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let request = Request::parse(json!({"model":"m","input":"first"})).unwrap();
        Arc::new(Entry {
            id: Uuid::new_v4(),
            account: Uuid::new_v4(),
            session: Session::new(
                "t".into(),
                "unused".into(),
                &access,
                &request,
                &request.input,
                &[],
            )
            .unwrap(),
            pid: 1,
            key: [0; 32],
            created: Instant::now(),
            expires: Instant::now() + Duration::from_secs(300),
            cancel: CancellationToken::new(),
            claimed: AtomicBool::new(false),
            published: AtomicBool::new(false),
            waiting: Mutex::new(None),
            backend: Default::default(),
            changed: Default::default(),
        })
    }
    #[test]
    fn uncommitted_delivery_is_revoked_and_success_detaches_cancellation() {
        let failed = entry();
        let handoff = Handoff(Mutex::new(Some(Ticket {
            entry: failed.clone(),
            completed: false,
        })));
        handoff.publish();
        handoff.finish(false);
        assert!(failed.cancel.is_cancelled());
        let succeeded = entry();
        let handoff = Handoff(Mutex::new(Some(Ticket {
            entry: succeeded.clone(),
            completed: false,
        })));
        handoff.publish();
        handoff.finish(true);
        drop(handoff);
        assert!(!succeeded.cancel.is_cancelled());
        let dropped = entry();
        drop(Handoff(Mutex::new(Some(Ticket {
            entry: dropped.clone(),
            completed: false,
        }))));
        assert!(dropped.cancel.is_cancelled());
    }
    #[test]
    fn unpublished_or_claimed_continuations_reject_a_second_consumer() {
        let store = Arc::new(Store::default());
        let entry = entry();
        store
            .entries
            .lock()
            .unwrap()
            .insert(entry.id, entry.clone());
        assert_eq!(
            store.claim(&entry).err().unwrap().code,
            "RESPONSE_CONTINUATION_BUSY"
        );
        entry.published.store(true, Ordering::Release);
        entry.claimed.store(true, Ordering::Release);
        assert_eq!(
            store.claim(&entry).err().unwrap().code,
            "RESPONSE_CONTINUATION_BUSY"
        );
        entry.cancel.cancel();
        entry.claimed.store(false, Ordering::Release);
        assert!(store.claim(&entry).unwrap().is_none());
        assert!(entry.expires > Instant::now());
    }

    #[tokio::test]
    async fn a_claim_while_admission_is_queued_returns_busy_without_waiting_for_capacity() {
        if !crate::native_cache::enabled() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(directory.path().join("accounts"))
            .await
            .unwrap();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let request = Request::parse(json!({"model":"m","input":"first"})).unwrap();
        let mut waiting = entry();
        Arc::get_mut(&mut waiting).unwrap().session = Session::new(
            "t".into(),
            "d".into(),
            &access,
            &request,
            &request.input,
            &[],
        )
        .unwrap();
        profiles
            .continuations
            .entries
            .lock()
            .unwrap()
            .insert(waiting.id, waiting.clone());
        let mut history = request.input.clone();
        history.extend(request.input.clone());
        let (admitted, ready) = tokio::sync::oneshot::channel();
        let queued = profiles.continuation_admission(&access, &request, &history, async {
            ready.await.unwrap();
            Ok(())
        });
        let competitor = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            waiting.claimed.store(true, Ordering::Release);
            profiles.continuations.remove(&waiting);
            admitted.send(()).unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(queued, competitor)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, "RESPONSE_CONTINUATION_BUSY");
        profiles.continuations.remove(&waiting);
        // Also cover a winner completing after admission but before native slot
        // preparation. Request-scoped evidence must outlive the store entry.
        waiting.claimed.store(false, Ordering::Release);
        profiles
            .continuations
            .entries
            .lock()
            .unwrap()
            .insert(waiting.id, waiting.clone());
        profiles
            .continuation_admission(&access, &request, &history, async { Ok(()) })
            .await
            .unwrap();
        waiting.claimed.store(true, Ordering::Release);
        profiles.continuations.remove(&waiting);
        assert_eq!(
            request.native_observation.check().unwrap_err().code,
            "RESPONSE_CONTINUATION_BUSY"
        );
        assert_eq!(
            profiles.continuations.claim(&waiting).err().unwrap().code,
            "RESPONSE_CONTINUATION_BUSY"
        );
    }

    #[test]
    fn expiration_cannot_be_extended_by_an_invalid_claim() {
        let store = Arc::new(Store::default());
        let mut expired = entry();
        Arc::get_mut(&mut expired).unwrap().expires = Instant::now() - Duration::from_secs(1);
        store
            .entries
            .lock()
            .unwrap()
            .insert(expired.id, expired.clone());
        let deadline = expired.expires;
        for _ in 0..2 {
            assert!(store.claim(&expired).unwrap().is_none());
        }
        assert_eq!(expired.expires, deadline);
        assert!(!expired.claimed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn compaction_while_parked_invalidates_claim_before_the_expiry_task_runs() {
        let store = Arc::new(Store::default());
        let entry = entry();
        let (client, _peer) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(client);
        let rpc = crate::rpc::Rpc::from_io(read, write);
        let (events, receiver) = tokio::sync::broadcast::channel(16);
        *entry.waiting.lock().unwrap() = Some(Waiting {
            guard: crate::responses::continuation::TurnGuard {
                rpc: rpc.clone(),
                thread: "t".into(),
                turn: "turn".into(),
                owner: None,
            },
            events: receiver,
        });
        store
            .entries
            .lock()
            .unwrap()
            .insert(entry.id, entry.clone());
        entry.published.store(true, Ordering::Release);
        events
            .send(
                json!({"method":"item/started","params":{"threadId":"t","turnId":"turn",
            "item":{"id":"compact","type":"contextCompaction"}}}),
            )
            .unwrap();
        assert!(store.claim(&entry).unwrap().is_none());
        assert!(entry.cancel.is_cancelled());
        assert!(!entry.claimed.load(Ordering::Acquire));
        assert!(
            entry.waiting.lock().unwrap().is_some(),
            "cleanup still owns the turn guard"
        );
        rpc.close();
    }

    #[test]
    fn bounded_waiting_capacity_evicts_oldest_and_never_a_claimed_request() {
        for (limit, same_account) in [(32, true), (1024, false)] {
            let mut entries = HashMap::new();
            let active = entry();
            active.claimed.store(true, Ordering::Release);
            entries.insert(active.id, active.clone());
            let account = active.account;
            let mut oldest = None;
            for index in 0..=limit {
                let mut next = entry();
                if same_account {
                    Arc::get_mut(&mut next).unwrap().account = account;
                }
                make_room(&entries, next.account);
                if index == 0 {
                    oldest = Some(next.clone());
                }
                entries.insert(next.id, next);
            }
            assert!(oldest.unwrap().cancel.is_cancelled());
            assert!(!active.cancel.is_cancelled());
            assert_eq!(
                entries
                    .values()
                    .filter(|e| !e.claimed.load(Ordering::Acquire) && !e.cancel.is_cancelled())
                    .count(),
                limit
            );
        }
    }
}
