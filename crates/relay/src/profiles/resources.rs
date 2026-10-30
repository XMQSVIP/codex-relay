//! Shared native context accounting. No transcript bodies are retained here.
use crate::{
    error::{Error, Result},
    native_cache::Limits,
};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum State {
    Reserved,
    Running,
    Idle,
    Waiting,
    Closing,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Token {
    id: Uuid,
    version: u64,
}
struct Entry {
    token: Token,
    thread: Option<String>,
    state: State,
    bytes: usize,
    identity: Option<[u8; 32]>,
    success_version: u64,
    used: u64,
    cleanup_started: bool,
}
pub(crate) struct Ledger {
    entries: HashMap<Uuid, Entry>,
    pub(super) limits: Limits,
    version: u64,
    clock: u64,
    directories: usize,
}
impl Default for Ledger {
    fn default() -> Self {
        Self::new(
            crate::native_cache::limits().expect("native cache configuration validated at startup"),
        )
    }
}
impl Ledger {
    fn new(limits: Limits) -> Self {
        Self {
            entries: HashMap::new(),
            limits,
            version: 0,
            clock: 0,
            directories: 0,
        }
    }
    pub(super) fn bytes(&self) -> usize {
        self.entries
            .values()
            .fold(0usize, |sum, e| sum.saturating_add(e.bytes))
    }
    fn by_thread(&self, thread: &str) -> Option<&Entry> {
        self.entries
            .values()
            .find(|e| e.thread.as_deref() == Some(thread))
    }
    pub(super) fn room(&self, thread: Option<&str>, bytes: usize) -> bool {
        if thread.is_some_and(|t| {
            self.by_thread(t)
                .is_none_or(|e| !matches!(e.state, State::Idle | State::Waiting))
        }) {
            return false;
        }
        !self.retire()
            && (thread.and_then(|t| self.by_thread(t)).is_some()
                || self.entries.len() < self.limits.threads)
            // One oversized conversation may complete in an otherwise empty
            // host. It cannot admit siblings and is discarded after completion.
            && (self.bytes().saturating_add(bytes) <= self.limits.bytes
                || self.entries.is_empty()
                || (self.entries.len() == 1 && thread.and_then(|t| self.by_thread(t)).is_some()))
    }
    pub(super) fn retire(&self) -> bool {
        self.directories >= 256
    }
    pub(super) fn reserve(
        &mut self,
        thread: Option<&str>,
        bytes: usize,
        identity: Option<[u8; 32]>,
    ) -> Result<Token> {
        if !self.room(thread, bytes) {
            return Err(full());
        }
        self.version += 1;
        let version = self.version;
        if let Some(thread) = thread {
            let entry = self
                .entries
                .values_mut()
                .find(|e| e.thread.as_deref() == Some(thread))
                .ok_or_else(full)?;
            if !matches!(entry.state, State::Idle | State::Waiting) {
                return Err(full());
            }
            entry.token.version = version;
            entry.state = State::Reserved;
            entry.bytes = entry.bytes.saturating_add(bytes);
            return Ok(entry.token);
        }
        let token = Token {
            id: Uuid::new_v4(),
            version,
        };
        self.entries.insert(
            token.id,
            Entry {
                token,
                thread: None,
                state: State::Reserved,
                bytes,
                identity,
                success_version: 0,
                used: 0,
                cleanup_started: false,
            },
        );
        Ok(token)
    }
    fn entry_mut(&mut self, token: Token) -> Option<&mut Entry> {
        self.entries.get_mut(&token.id).filter(|e| e.token == token)
    }
    pub(super) fn bind(&mut self, token: Token, thread: &str) -> Result<()> {
        let entry = self.entry_mut(token).ok_or_else(full)?;
        if entry.state != State::Reserved || entry.thread.as_deref().is_some_and(|t| t != thread) {
            return Err(full());
        }
        entry.thread = Some(thread.into());
        entry.state = State::Running;
        Ok(())
    }
    pub(super) fn output(&mut self, token: Token, bytes: usize) {
        if let Some(e) = self.entry_mut(token) {
            e.bytes = e.bytes.saturating_add(bytes);
        }
    }
    pub(super) fn finish(
        &mut self,
        token: Token,
        success: bool,
        waiting: bool,
        keep: bool,
    ) -> bool {
        self.clock += 1;
        let clock = self.clock;
        let limit = self.limits.bytes;
        let Some(e) = self.entry_mut(token) else {
            return false;
        };
        if e.state == State::Closing {
            return false;
        }
        if success {
            e.used = clock;
            e.success_version = token.version;
        }
        let keep = keep && e.bytes <= limit;
        e.state = if waiting {
            State::Waiting
        } else if keep {
            State::Idle
        } else {
            State::Closing
        };
        keep
    }
    pub(super) fn oversized(&self, thread: &str) -> bool {
        self.by_thread(thread)
            .is_some_and(|e| e.bytes > self.limits.bytes)
    }
    pub(super) fn expire_waiter(&self, thread: &str) -> bool {
        self.oversized(thread)
            || (self.bytes() > self.limits.bytes
                && !self.entries.values().any(|e| e.state == State::Closing)
                && self
                    .entries
                    .values()
                    .filter(|e| e.state == State::Waiting)
                    .min_by_key(|e| e.used)
                    .is_some_and(|e| e.thread.as_deref() == Some(thread)))
    }
    pub(super) fn closing(&mut self, thread: &str) -> Option<Token> {
        let entry = self
            .entries
            .values_mut()
            .find(|e| e.thread.as_deref() == Some(thread))?;
        if entry.cleanup_started {
            return None;
        }
        entry.cleanup_started = true;
        entry.state = State::Closing;
        Some(entry.token)
    }
    pub(super) fn release(&mut self, token: Token) -> bool {
        if self
            .entries
            .get(&token.id)
            .is_some_and(|e| e.token == token && e.state == State::Closing)
        {
            let entry = self.entries.remove(&token.id).unwrap();
            self.directories += usize::from(entry.thread.is_some());
            true
        } else {
            false
        }
    }
    pub(super) fn candidates(&self, protect: Option<&str>) -> Vec<String> {
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|e| e.state == State::Idle && e.thread.as_deref() != protect)
            .collect();
        entries.sort_by_key(|e| {
            let old = e.identity.is_some_and(|identity| {
                self.entries.values().any(|other| {
                    other.identity == Some(identity) && other.success_version > e.success_version
                })
            });
            (!old, e.used, e.token.version)
        });
        entries
            .into_iter()
            .filter_map(|e| e.thread.clone())
            .collect()
    }
}
pub(super) struct Offer {
    pub thread: Option<String>,
    pub bytes: usize,
}

/// The caller owns a native activity lease. Never release ledger capacity on
/// an acknowledgement alone, and never delete a live process's directories.
pub(crate) async fn close_context(
    backend: &std::sync::Arc<crate::backend::Backend>,
    slot: &super::Slot,
    thread: &str,
) -> bool {
    let token = {
        let mut state = backend.response_worker.lock().unwrap();
        let Some(state) = state.as_mut() else {
            return false;
        };
        state.sessions.retain(|s| s.thread != thread);
        state.resources.closing(thread)
    };
    let Some(token) = token else { return true };
    if backend.rpc.close_idle_thread(thread).await.is_err() {
        super::warm::retire(slot, backend);
        return false;
    }
    let retire = {
        let mut state = backend.response_worker.lock().unwrap();
        let Some(state) = state.as_mut() else {
            return false;
        };
        state.resources.release(token);
        state.resources.retire()
    };
    if retire {
        super::warm::retire(slot, backend);
    }
    true
}

impl super::Profiles {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resource_offers(
        &self,
        account: Uuid,
        key: &[u8; 32],
        access: &crate::operations::ClientAccess,
        request: &crate::responses::Request,
        history: &[serde_json::Value],
        matched: &HashMap<u32, std::collections::HashSet<crate::responses::reuse::Session>>,
        waiting: Option<(u32, &crate::responses::reuse::Session)>,
    ) -> Result<HashMap<u32, Offer>> {
        let slots = self
            .response_slots
            .lock()
            .await
            .get(&account)
            .cloned()
            .unwrap_or_default();
        let mut offers = HashMap::new();
        let mut candidates = Vec::new();
        for slot in slots {
            let backend = {
                let Ok(worker) = slot.try_lock() else {
                    continue;
                };
                worker.clone()
            };
            let Some(backend) = backend else { continue };
            let session = {
                let state = backend.response_worker.lock().unwrap();
                let Some(state) = state
                    .as_ref()
                    .filter(|s| s.managed_contexts && s.matches(key) && !s.resources.retire())
                else {
                    continue;
                };
                if let Some((pid, session)) = waiting
                    && pid == backend.pid
                    && state
                        .resources
                        .by_thread(&session.thread)
                        .is_some_and(|e| matches!(e.state, State::Reserved | State::Running))
                {
                    return Err(Error::new(
                        "RESPONSE_CONTINUATION_BUSY",
                        "这组工具结果的原生续接正在处理中。",
                    ));
                }
                waiting
                    .filter(|(pid, _)| *pid == backend.pid)
                    .map(|(_, s)| s.clone())
                    .or_else(|| {
                        state
                            .sessions
                            .iter()
                            .find(|s| matched.get(&backend.pid).is_some_and(|m| m.contains(*s)))
                            .cloned()
                    })
            };
            let offer = Offer {
                thread: session.as_ref().map(|s| s.thread.clone()),
                bytes: crate::responses::reservation_bytes(
                    Some(&backend.rpc),
                    access,
                    request,
                    history,
                    session.as_ref(),
                )?,
            };
            let fits = backend
                .response_worker
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|s| s.resources.room(offer.thread.as_deref(), offer.bytes));
            candidates.push((slot, backend, offer.thread.is_some(), fits));
            offers.insert(candidates.last().unwrap().1.pid, offer);
        }
        // Only reclaim from the host we intend to use. Spare compatible hosts
        // must not lose their caches just because another host needs capacity.
        if candidates
            .iter()
            .any(|(_, _, target, fits)| *target && *fits)
        {
            return Ok(offers);
        }
        let chosen = candidates
            .iter()
            .position(|(_, _, target, _)| *target)
            .or_else(|| {
                if candidates.iter().any(|(_, _, _, fits)| *fits) {
                    None
                } else {
                    candidates.iter().position(|(_, backend, _, _)| {
                        backend
                            .response_worker
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|s| !s.resources.candidates(None).is_empty())
                    })
                }
            });
        if let Some(index) = chosen {
            let (slot, backend, _, _) = candidates.swap_remove(index);
            let mut offer = offers.remove(&backend.pid).unwrap();
            loop {
                let victim = {
                    let state = backend.response_worker.lock().unwrap();
                    let Some(state) = state
                        .as_ref()
                        .filter(|s| s.matches(key) && !s.resources.retire())
                    else {
                        break;
                    };
                    if state.resources.room(offer.thread.as_deref(), offer.bytes) {
                        break;
                    }
                    state
                        .resources
                        .candidates(offer.thread.as_deref())
                        .into_iter()
                        .next()
                        .or_else(|| {
                            offer
                                .thread
                                .as_ref()
                                .filter(|t| state.resources.candidates(None).contains(t))
                                .cloned()
                        })
                };
                let Some(victim) = victim else { break };
                if !self
                    .evict_context(account, &slot, &backend, &victim)
                    .await?
                {
                    break;
                }
                if offer.thread.as_deref() == Some(&victim) {
                    offer.thread = None;
                    offer.bytes = crate::responses::reservation_bytes(
                        Some(&backend.rpc),
                        access,
                        request,
                        history,
                        None,
                    )?;
                }
            }
            offers.insert(backend.pid, offer);
        }
        Ok(offers)
    }
    async fn evict_context(
        &self,
        account: Uuid,
        slot: &super::Slot,
        backend: &std::sync::Arc<crate::backend::Backend>,
        thread: &str,
    ) -> Result<bool> {
        let Ok(guard) = backend.response_activity.clone().try_read_owned() else {
            return Ok(false);
        };
        // Detach atomically before spawning; stale matches cannot claim this thread.
        {
            let mut state = backend.response_worker.lock().unwrap();
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if !state.resources.candidates(None).iter().any(|t| t == thread) {
                return Ok(false);
            }
            state.sessions.retain(|s| s.thread != thread);
            let entry = state
                .resources
                .entries
                .values_mut()
                .find(|e| e.thread.as_deref() == Some(thread))
                .unwrap();
            entry.state = State::Closing;
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let registration = match self.register_retained(account, cancel.clone()) {
            Ok(value) => value,
            Err(error) => {
                super::warm::retire(slot, backend);
                return Err(error);
            }
        };
        let backend = backend.clone();
        let slot = slot.clone();
        let thread = thread.to_owned();
        // Owned task survives a request dropping its join handle. Stop cancels
        // this task's wait and retires the host while siblings drain normally.
        tokio::spawn(async move {
            let _registration = registration;
            let _guard = guard;
            tokio::select! {
                result=close_context(&backend,&slot,&thread)=>result,
                _=cancel.cancelled()=>{super::warm::retire(&slot,&backend);false}
            }
        })
        .await
        .map_err(|_| full())
    }
    pub(super) async fn trim_contexts(
        &self,
        account: Uuid,
        slot: &super::Slot,
        backend: &std::sync::Arc<crate::backend::Backend>,
    ) -> Result<()> {
        loop {
            let victim = {
                let state = backend.response_worker.lock().unwrap();
                state
                    .as_ref()
                    .filter(|s| s.resources.bytes() > s.resources.limits.bytes)
                    .and_then(|s| s.resources.candidates(None).into_iter().next())
            };
            let Some(victim) = victim else { return Ok(()) };
            if !self.evict_context(account, slot, backend, &victim).await? {
                return Ok(());
            }
        }
    }
}
fn full() -> Error {
    Error::new(
        "NATIVE_RESOURCE_CHANGED",
        "原生线程容量或领取状态已变化，请重新准备请求。",
    )
}

/// A version-fenced handle used by the Responses adapter, never serialized.
#[derive(Clone)]
pub(crate) struct Tracker {
    pub(super) backend: std::sync::Weak<crate::backend::Backend>,
    pub(super) token: Token,
}
impl Tracker {
    pub(crate) fn reserve_input(&self, bytes: usize) -> Result<()> {
        let backend = self.backend.upgrade().ok_or_else(full)?;
        let mut worker = backend.response_worker.lock().unwrap();
        let state = worker.as_mut().ok_or_else(full)?;
        if state.resources.bytes().saturating_add(bytes) > state.resources.limits.bytes
            && state.resources.entries.len() != 1
        {
            return Err(full());
        }
        if state
            .resources
            .entry_mut(self.token)
            .is_none_or(|e| e.state != State::Running)
        {
            return Err(full());
        }
        state.resources.output(self.token, bytes);
        Ok(())
    }
    pub(crate) fn bind(&self, thread: &str) -> Result<()> {
        let backend = self.backend.upgrade().ok_or_else(full)?;
        let mut worker = backend.response_worker.lock().unwrap();
        worker
            .as_mut()
            .ok_or_else(full)?
            .resources
            .bind(self.token, thread)
    }
    pub(crate) fn output(&self, bytes: usize) {
        if let Some(backend) = self.backend.upgrade()
            && let Some(worker) = backend.response_worker.lock().unwrap().as_mut()
        {
            worker.resources.output(self.token, bytes);
        }
    }
}
pub(super) struct Reservation {
    pub tracker: Tracker,
    pub slot: super::Slot,
    pub finished: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.finished
            && let Some(backend) = self.tracker.backend.upgrade()
        {
            super::warm::retire(&self.slot, &backend);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add(l: &mut Ledger, t: &str, i: Option<[u8; 32]>, b: usize) -> Token {
        let v = l.reserve(None, b, i).unwrap();
        l.bind(v, t).unwrap();
        l.finish(v, true, false, true);
        v
    }
    #[test]
    fn counts_live_contexts_and_fences_late_cleanup() {
        let mut l = Ledger::new(Limits {
            threads: 2,
            bytes: 100,
        });
        let old = add(&mut l, "a", None, 20);
        add(&mut l, "b", None, 20);
        assert!(!l.room(None, 1));
        assert!(l.room(Some("a"), 1));
        let new = l.reserve(Some("a"), 5, None).unwrap();
        l.bind(new, "a").unwrap();
        assert_eq!(l.bytes(), 45);
        assert!(!l.release(old));
        assert_eq!(l.candidates(None), vec!["b"]);
        l.finish(new, true, true, true);
        assert_eq!(l.candidates(None), vec!["b"]);
        let closing = l.closing("b").unwrap();
        assert!(!l.room(None, 1));
        assert!(l.release(closing));
        assert!(!l.release(closing));
        assert!(l.room(None, 1));
    }
    #[test]
    fn oversized_input_runs_alone_then_releases_only_after_close() {
        let mut l = Ledger::new(Limits {
            threads: 4,
            bytes: 10,
        });
        let t = l.reserve(None, 12, None).unwrap();
        l.bind(t, "large").unwrap();
        assert!(!l.room(None, 0));
        assert!(!l.finish(t, true, false, true));
        assert_eq!(l.bytes(), 12);
        assert!(!l.room(None, 0));
        assert!(l.release(t));
        add(&mut l, "continued", None, 8);
        let continued = l.reserve(Some("continued"), 3, None).unwrap();
        l.bind(continued, "continued").unwrap();
        assert!(!l.finish(continued, true, false, true));
        assert_eq!(l.bytes(), 11);
    }
    #[test]
    fn old_versions_precede_lru_and_late_success_cannot_replace_newer() {
        let mut l = Ledger::new(Limits {
            threads: 8,
            bytes: 100,
        });
        add(&mut l, "unrelated", None, 1);
        let slow = l.reserve(None, 1, Some([1; 32])).unwrap();
        l.bind(slow, "slow").unwrap();
        add(&mut l, "new", Some([1; 32]), 1);
        l.finish(slow, true, false, true);
        assert_eq!(l.candidates(None), vec!["slow", "unrelated", "new"]);
        let reused = l.reserve(Some("slow"), 1, None).unwrap();
        l.bind(reused, "slow").unwrap();
        l.finish(reused, true, false, true);
        assert_eq!(l.candidates(None), vec!["new", "unrelated", "slow"]);
        assert_eq!(l.candidates(Some("new")), vec!["unrelated", "slow"]);
    }
    #[test]
    fn output_pressure_and_directory_limit() {
        let mut l = Ledger::new(Limits {
            threads: 1,
            bytes: 10,
        });
        for n in 0..256 {
            let t = n.to_string();
            let v = add(&mut l, &t, None, 1);
            l.output(v, 20);
            assert!(!l.finish(v, true, false, true));
            assert!(l.release(v));
        }
        assert!(l.retire());
        assert!(!l.room(None, 0));
        assert_eq!(l.bytes(), 0);
    }
    #[test]
    fn unknown_identities_failures_and_budget_reservations_do_not_refresh_lru() {
        let mut l = Ledger::new(Limits {
            threads: 4,
            bytes: 10,
        });
        add(&mut l, "a", None, 2);
        add(&mut l, "b", None, 2);
        let failed = l.reserve(Some("a"), 1, None).unwrap();
        l.bind(failed, "a").unwrap();
        l.finish(failed, false, false, true);
        assert_eq!(l.candidates(None), vec!["a", "b"]);
        let active = l.reserve(None, 4, Some([9; 32])).unwrap();
        l.bind(active, "active").unwrap();
        assert!(l.reserve(None, 2, None).is_err());
        assert_eq!(l.bytes(), 9);
        assert!(!l.candidates(None).contains(&"active".to_owned()));
        l.output(active, 4);
        l.finish(active, true, true, true);
        assert!(l.expire_waiter("active"));
        let a = l.closing("a").unwrap();
        assert!(!l.expire_waiter("active"));
        l.release(a);
    }
}
