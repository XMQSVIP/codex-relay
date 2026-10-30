//! Adaptive routing. Selection is pure; only committed admissions and outcomes
//! update counters/caches. Never perform I/O or acquire a worker lock here.
use super::{AccountPolicy, ClientAccess, Inner, Operations, RoutingRequest, quota, runtime};
use serde::Serialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

const CAPACITY: usize = 4096;
const SAMPLE_TTL: Duration = Duration::from_secs(600);
const AFFINITY_TTL: Duration = Duration::from_secs(120);
#[cfg(test)]
const NATIVE_AFFINITY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RequestClass {
    #[default]
    Text,
    Tools,
    Multimodal,
    ImageGeneration,
    ImageEdit,
    Search,
    NativeSession,
    LegacyText,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MetricKey {
    account: Uuid,
    generation: String,
    model: String,
    effort: String,
    class: RequestClass,
}
impl MetricKey {
    fn account_key(&self) -> Self {
        Self {
            model: String::new(),
            effort: String::new(),
            class: RequestClass::Text,
            ..self.clone()
        }
    }
    fn new(account: &AccountPolicy, request: &RoutingRequest) -> Option<Self> {
        Some(Self {
            account: account.id,
            generation: runtime::fingerprint(account.route.as_ref()?),
            model: request.model.clone(),
            effort: request
                .reasoning_effort
                .clone()
                .unwrap_or_else(|| "default".into()),
            class: request.class,
        })
    }
    fn current(&self, inner: &Inner) -> bool {
        inner.settings.accounts.iter().any(|a| {
            a.id == self.account
                && !a.deleting
                && a.route
                    .as_ref()
                    .is_some_and(|p| runtime::fingerprint(p) == self.generation)
        })
    }
}

#[derive(Default)]
struct Metrics {
    health: Option<(f64, Instant)>,
    latency: Option<(f64, u32, Instant)>,
    last_selected: u64,
    touched: u64,
}
impl Metrics {
    fn health(&self, now: Instant) -> f64 {
        self.health
            .filter(|(_, at)| now.duration_since(*at) < SAMPLE_TTL)
            .map_or(0.9, |(v, _)| v)
    }
    fn latency(&self, now: Instant) -> Option<f64> {
        self.latency
            .filter(|(_, n, at)| *n >= 5 && now.duration_since(*at) < SAMPLE_TTL)
            .map(|(v, _, _)| v)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AffinityKey {
    key: Uuid,
    model: String,
    digest: [u8; 32],
}
impl AffinityKey {
    fn new(access: &ClientAccess, model: &str, digest: [u8; 32]) -> Self {
        Self {
            key: access.key_id,
            model: model.into(),
            digest,
        }
    }
}
struct Affinity {
    account: Uuid,
    generation: String,
    revision: Uuid,
    ambiguous: bool,
    expires: Instant,
    touched: u64,
}
struct Flight {
    key: MetricKey,
    admitted_at: u64,
}
#[derive(Default)]
pub(super) struct State {
    metrics: HashMap<MetricKey, Metrics>,
    affinities: HashMap<AffinityKey, Affinity>,
    flights: HashMap<Uuid, Flight>,
    ordinary: HashMap<Uuid, u64>,
    tick: u64,
}
impl State {
    pub(super) fn invalidate_key(&mut self, key: Uuid) {
        self.affinities.retain(|identity, _| identity.key != key);
    }
    // Any cleanup holding an earlier snapshot must match both generations.
    fn remove_affinity(&mut self, key: &AffinityKey, revision: Uuid, generation: &str) {
        if self
            .affinities
            .get(key)
            .is_some_and(|entry| entry.revision == revision && entry.generation == generation)
        {
            self.affinities.remove(key);
        }
    }
    pub(super) fn invalidate(&mut self, before: &super::Settings, after: &super::Settings) {
        let changed: Vec<_> = before
            .accounts
            .iter()
            .filter(|old| {
                !after.accounts.iter().any(|new| {
                    new.id == old.id
                        && !new.deleting
                        && new.route == old.route
                        && new.proxy == old.proxy
                })
            })
            .map(|a| a.id)
            .collect();
        self.metrics
            .retain(|key, _| !changed.contains(&key.account));
        self.affinities
            .retain(|_, value| !changed.contains(&value.account));
        self.flights
            .retain(|_, value| !changed.contains(&value.key.account));
        self.ordinary
            .retain(|group, _| after.groups.iter().any(|g| g.id == *group));
    }
}

fn affinity_current(inner: &Inner, access: &ClientAccess, entry: &Affinity) -> bool {
    inner.settings.accounts.iter().any(|a| {
        a.id == entry.account
            && a.belongs_to(access.group_id)
            && !a.deleting
            && a.route.as_ref().is_some_and(|p| {
                p.execution != crate::provider::Execution::ApiKey
                    && runtime::fingerprint(p) == entry.generation
            })
    })
}

/// Called only after admission writes succeed, within Operations::inner.
fn commit_affinity(
    inner: &mut Inner,
    access: &ClientAccess,
    request: &RoutingRequest,
    account: Uuid,
    now: Instant,
) {
    if !crate::native_cache::enabled() {
        return;
    }
    let Some(digest) = request.soft_affinity else {
        return;
    };
    let Some(provider) = inner
        .settings
        .accounts
        .iter()
        .find(|a| a.id == account)
        .and_then(|a| a.route.as_ref())
    else {
        return;
    };
    if provider.execution == crate::provider::Execution::ApiKey {
        return;
    }
    let generation = runtime::fingerprint(provider);
    let ttl = Duration::from_secs(u64::from(inner.settings.native_affinity_ttl_hours) * 60 * 60);
    let key = AffinityKey::new(access, &request.model, digest);
    let preserve = inner
        .runtime
        .scheduling
        .affinities
        .get(&key)
        .is_some_and(|e| e.expires > now && affinity_current(inner, access, e));
    let state = &mut inner.runtime.scheduling;
    let expired: Vec<_> = state
        .affinities
        .iter()
        .filter(|(_, e)| e.expires <= now)
        .map(|(key, e)| (key.clone(), e.revision, e.generation.clone()))
        .collect();
    for (key, revision, generation) in expired {
        state.remove_affinity(&key, revision, &generation);
    }
    // This is admission recency, never lookup or response-completion recency.
    state.tick = state.tick.wrapping_add(1);
    if preserve {
        let entry = state.affinities.get_mut(&key).unwrap();
        entry.expires = now + ttl;
        entry.touched = state.tick;
    } else {
        state.affinities.insert(
            key,
            Affinity {
                account,
                generation,
                revision: Uuid::new_v4(),
                ambiguous: false,
                expires: now + ttl,
                touched: state.tick,
            },
        );
    }
    trim(&mut state.affinities, |e| e.touched);
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Components {
    load: f64,
    quota: f64,
    health: f64,
    latency: f64,
}
impl Components {
    fn score(&self) -> f64 {
        0.45 * self.load + 0.25 * self.quota + 0.20 * self.health + 0.10 * self.latency
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Decision {
    pub version: &'static str,
    pub mode: &'static str,
    pub candidate_count: usize,
    pub score: Option<f64>,
    pub components: Option<Components>,
    pub affinity: &'static str,
}
impl Decision {
    pub fn bound() -> Self {
        Self {
            version: "adaptive_v1",
            mode: "hardBinding",
            candidate_count: 1,
            score: None,
            components: None,
            affinity: "hardBinding",
        }
    }
}

fn quota_score(inner: &Inner, account: &AccountPolicy, now: u64) -> f64 {
    quota::current(inner, account)
        .filter(|q| {
            q.verified && q.status == "supported" && now.saturating_sub(q.checked_at) <= 300_000
        })
        .and_then(|q| {
            q.windows
                .iter()
                .filter(|w| w.bucket == "codex" && w.resets_at.is_none_or(|at| at > now))
                .filter_map(|w| {
                    w.used_percent
                        .filter(|n| n.is_finite())
                        .map(|used| 1.0 - used.clamp(0.0, 100.0) / 100.0)
                })
                .min_by(f64::total_cmp)
        })
        .unwrap_or(0.5)
}

/// `available` is already in round-robin order and has passed all hard filters.
pub(super) fn choose(
    inner: &Inner,
    available: &[&AccountPolicy],
    access: &ClientAccess,
    request: &RoutingRequest,
) -> Option<(AccountPolicy, Decision)> {
    let now = Instant::now();
    let rank = available
        .iter()
        .map(|a| runtime::capability_rank(inner, a, request))
        .min()?;
    let candidates: Vec<_> = available
        .iter()
        .copied()
        .filter(|a| runtime::capability_rank(inner, a, request) == rank)
        .collect();
    let state = &inner.runtime.scheduling;
    let mut latencies: Vec<_> = candidates
        .iter()
        .filter_map(|a| MetricKey::new(a, request))
        .filter_map(|k| state.metrics.get(&k).and_then(|m| m.latency(now)))
        .collect();
    latencies.sort_by(f64::total_cmp);
    let median = match latencies.len() {
        0 => None,
        n if n % 2 == 0 => Some((latencies[n / 2 - 1] + latencies[n / 2]) / 2.0),
        n => Some(latencies[n / 2]),
    };
    let scored: Vec<_> = candidates
        .iter()
        .map(|a| {
            let metrics = MetricKey::new(a, request).and_then(|key| state.metrics.get(&key));
            let account_metrics =
                MetricKey::new(a, request).and_then(|key| state.metrics.get(&key.account_key()));
            let components = Components {
                load: (1.0
                    - Operations::active_count(inner, Some(a.id), None) as f64
                        / f64::from(a.max_concurrency.max(1)))
                .clamp(0.0, 1.0),
                quota: quota_score(inner, a, crate::now_ms()),
                health: account_metrics.map_or(0.9, |m| m.health(now)),
                latency: median
                    .zip(metrics.and_then(|m| m.latency(now)))
                    .map_or(0.5, |(median, value)| median / (median + value).max(1.0)),
            };
            (
                *a,
                components,
                account_metrics.map_or(0, |m| m.last_selected),
            )
        })
        .collect();
    let best = scored
        .iter()
        .map(|(_, c, _)| c.score())
        .max_by(f64::total_cmp)?;
    let mut affinity = "notRequested";
    let mut preferred = None;
    if let Some(digest) = request.soft_affinity {
        let own = state
            .affinities
            .get(&AffinityKey::new(access, &request.model, digest));
        // Inheritance is only a first-admission hint. A valid child home, even
        // temporarily unavailable, must never be replaced by the parent's home.
        let parent = if crate::native_cache::enabled()
            && own.is_none_or(|e| e.expires <= now || !affinity_current(inner, access, e))
        {
            request
                .parent_affinity
                .and_then(|digest| {
                    state
                        .affinities
                        .get(&AffinityKey::new(access, &request.model, digest))
                })
                .filter(|e| e.expires > now && affinity_current(inner, access, e))
        } else {
            None
        };
        let entry = parent.or(own);
        affinity = match entry {
            None => "notFound",
            Some(e) if e.expires <= now => "expired",
            Some(e)
                if !inner.settings.accounts.iter().any(|a| {
                    a.id == e.account
                        && a.belongs_to(access.group_id)
                        && !a.deleting
                        && a.route
                            .as_ref()
                            .is_some_and(|p| runtime::fingerprint(p) == e.generation)
                }) =>
            {
                "routeChanged"
            }
            Some(e) if e.ambiguous => "ambiguous",
            Some(e) => match scored.iter().position(|(a, _, _)| a.id == e.account) {
                None => "unavailable",
                Some(i)
                    if scored[i]
                        .0
                        .route
                        .as_ref()
                        .is_some_and(|p| p.execution == crate::provider::Execution::ApiKey) =>
                {
                    "unavailable"
                }
                Some(i)
                    if !crate::native_cache::enabled()
                        && scored[i].1.score() + 0.08 + f64::EPSILON < best =>
                {
                    "scoreGap"
                }
                Some(i) => {
                    preferred = Some(i);
                    "hit"
                }
            },
        };
    }
    let explore = state
        .ordinary
        .get(&access.group_id)
        .copied()
        .unwrap_or(0)
        .wrapping_add(1)
        % 20
        == 0;
    let (index, mode) = if let Some(i) = preferred {
        (i, "softAffinity")
    } else if explore {
        (
            scored
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, last))| *last)
                .unwrap()
                .0,
            "exploration",
        )
    } else {
        (
            scored
                .iter()
                .position(|(_, c, _)| c.score() + 0.01 + f64::EPSILON >= best)
                .unwrap(),
            "balanced",
        )
    };
    let (account, components, _) = &scored[index];
    Some((
        (*account).clone(),
        Decision {
            version: "adaptive_v1",
            mode,
            candidate_count: scored.len(),
            score: Some(components.score()),
            components: Some(components.clone()),
            affinity,
        },
    ))
}

fn trim<K: Clone + Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>, touched: impl Fn(&V) -> u64) {
    while map.len() > CAPACITY {
        let key = map
            .iter()
            .min_by_key(|(_, value)| touched(value))
            .map(|(key, _)| key.clone())
            .unwrap();
        map.remove(&key);
    }
}

pub(super) fn admitted(
    inner: &mut Inner,
    id: Uuid,
    access: &ClientAccess,
    request: &RoutingRequest,
    account: Uuid,
    decision: &Decision,
) {
    let Some(policy) = inner.settings.accounts.iter().find(|a| a.id == account) else {
        return;
    };
    let Some(key) = MetricKey::new(policy, request) else {
        return;
    };
    let state = &mut inner.runtime.scheduling;
    state.tick = state.tick.wrapping_add(1);
    let metrics = state.metrics.entry(key.account_key()).or_default();
    metrics.last_selected = state.tick;
    metrics.touched = state.tick;
    if let Some(metrics) = state.metrics.get_mut(&key) {
        metrics.touched = state.tick;
    }
    trim(&mut state.metrics, |m| m.touched);
    state.flights.insert(
        id,
        Flight {
            key,
            admitted_at: crate::now_ms(),
        },
    );
    if matches!(decision.mode, "balanced" | "exploration") {
        let count = state.ordinary.entry(access.group_id).or_default();
        *count = count.wrapping_add(1);
    }
    if !crate::native_cache::enabled()
        && let Some(digest) = request.soft_affinity
    {
        if let Some(entry) =
            state
                .affinities
                .get_mut(&AffinityKey::new(access, &request.model, digest))
        {
            entry.touched = state.tick;
        }
    }
    commit_affinity(inner, access, request, account, Instant::now());
}

pub(super) fn finished(
    inner: &mut Inner,
    id: Uuid,
    success: bool,
    code: Option<&str>,
    first_delta: Option<u64>,
) {
    let Some(flight) = inner.runtime.scheduling.flights.remove(&id) else {
        return;
    };
    if !flight.key.current(inner) {
        return;
    }
    let failure = code.is_some_and(|code| {
        matches!(
            code,
            "RPC_CLOSED"
                | "RPC_TIMEOUT"
                | "RPC_PROTOCOL"
                | "BACKEND_START"
                | "BACKEND_SPAWN"
                | "STREAM_EVENTS_LOST"
                | "UPSTREAM_AUTH_FAILED"
                | "UPSTREAM_RATE_LIMITED"
                | "UPSTREAM_QUOTA_EXHAUSTED"
                | "UPSTREAM_REQUEST_FAILED"
                | "UPSTREAM_CAPACITY"
                | "UPSTREAM_TIMEOUT"
                | "UPSTREAM_CONNECTION_FAILED"
                | "UPSTREAM_UNAVAILABLE"
                | "UPSTREAM_INVALID_RESPONSE"
        )
    });
    if !success && !failure {
        return;
    }
    let now = Instant::now();
    let state = &mut inner.runtime.scheduling;
    state.tick = state.tick.wrapping_add(1);
    let metrics = state.metrics.entry(flight.key.account_key()).or_default();
    metrics.health = Some((0.8 * metrics.health(now) + 0.2 * f64::from(success), now));
    metrics.touched = state.tick;
    if success && let Some(first) = first_delta.filter(|at| *at >= flight.admitted_at) {
        let metrics = state.metrics.entry(flight.key).or_default();
        let value = first.saturating_sub(flight.admitted_at).max(1) as f64;
        let (old, count) = metrics
            .latency
            .filter(|(_, _, at)| now.duration_since(*at) < SAMPLE_TTL)
            .map_or((value, 0), |(v, n, _)| (v, n));
        metrics.latency = Some((0.8 * old + 0.2 * value, count.saturating_add(1), now));
        metrics.touched = state.tick;
    }
    trim(&mut state.metrics, |m| m.touched);
}

impl Operations {
    /// Digest-only success cache. Route validation is repeated under the same
    /// operations lock, so a credential or group change cannot publish stale affinity.
    pub(crate) fn remember_soft_affinity(
        &self,
        access: &ClientAccess,
        model: &str,
        digest: [u8; 32],
        account: Uuid,
        provider: &crate::provider::Provider,
    ) {
        // Enabled native affinity is committed once at admission. In particular,
        // a late response must not resurrect an expired or replaced binding.
        if crate::native_cache::enabled() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if !inner.settings.accounts.iter().any(|a| {
            a.id == account
                && !a.deleting
                && a.belongs_to(access.group_id)
                && a.route.as_ref() == Some(provider)
        }) {
            return;
        }
        let now = Instant::now();
        let generation = runtime::fingerprint(provider);
        let key = AffinityKey::new(access, model, digest);
        let state = &mut inner.runtime.scheduling;
        state.affinities.retain(|_, value| value.expires > now);
        state.tick = state.tick.wrapping_add(1);
        // Two successful completions with the same transcript are ambiguous,
        // even if both happened to use the same account.
        let ambiguous = state.affinities.contains_key(&key);
        state.affinities.insert(
            key,
            Affinity {
                account,
                generation,
                revision: Uuid::new_v4(),
                ambiguous,
                expires: now + AFFINITY_TTL,
                touched: state.tick,
            },
        );
        trim(&mut state.affinities, |a| a.touched);
    }
}

#[cfg(test)]
mod affinity_tests;
#[cfg(test)]
mod tests;
