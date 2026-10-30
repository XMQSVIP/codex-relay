use super::*;
use crate::operations::{ApiKeyCreate, DEFAULT_GROUP, Feature, NativeCapabilities, StatsQuery};
use crate::provider::Provider;
use serde_json::json;
use std::sync::Arc;

pub(super) fn setup() -> (tempfile::TempDir, Arc<Operations>, ClientAccess) {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    ops.register_accounts(&[
        (Uuid::from_u128(10), "a".into()),
        (Uuid::from_u128(20), "b".into()),
    ])
    .unwrap();
    for id in [Uuid::from_u128(10), Uuid::from_u128(20)] {
        // These queue fixtures deliberately use one slot per account.
        ops.set_account(
            id,
            crate::operations::AccountInput {
                revision: ops.snapshot()["revision"].as_u64().unwrap(),
                group_id: DEFAULT_GROUP,
                enabled: true,
                max_concurrency: 1,
            },
        )
        .unwrap();
        ops.set_inference_route(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            Some(Provider {
                execution: Default::default(),
                id: "synthetic".into(),
                model: "m".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    }
    let created = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "client".into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap();
    let access = ops.authenticate_api_key(&created.secret).unwrap();
    (temp, ops, access)
}
pub(super) fn request() -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: "m".into(),
        ..Default::default()
    }
}
fn remember(
    ops: &Operations,
    access: &ClientAccess,
    model: &str,
    digest: [u8; 32],
    account: Uuid,
    provider: &Provider,
) {
    if crate::native_cache::enabled() {
        commit_affinity(
            &mut ops.inner.lock().unwrap(),
            access,
            &RoutingRequest {
                model: model.into(),
                soft_affinity: Some(digest),
                ..request()
            },
            account,
            Instant::now(),
        );
    } else {
        ops.remember_soft_affinity(access, model, digest, account, provider);
    }
}
pub(super) fn selected(
    inner: &Inner,
    access: &ClientAccess,
    request: &RoutingRequest,
) -> (Uuid, Decision) {
    let selection = Operations::select(
        inner,
        None,
        Some(DEFAULT_GROUP),
        true,
        Some(access),
        Some(request),
        false,
    )
    .unwrap();
    assert!(selection.rejection.is_none());
    (selection.account.unwrap().id, selection.scheduling.unwrap())
}
fn metric(inner: &mut Inner, account: usize, request: &RoutingRequest, latency: f64, count: u32) {
    let key = MetricKey::new(&inner.settings.accounts[account], request).unwrap();
    inner.runtime.scheduling.metrics.insert(
        key,
        Metrics {
            latency: Some((latency, count, Instant::now())),
            ..Default::default()
        },
    );
}
fn quota(inner: &mut Inner, account: usize, used: f64) {
    let account = &inner.settings.accounts[account];
    inner.runtime.quotas.insert(
        account.id,
        quota::QuotaSnapshot {
            status: "supported".into(),
            verified: true,
            checked_at: crate::now_ms(),
            generation: runtime::fingerprint(account.route.as_ref().unwrap()),
            windows: vec![quota::QuotaWindow {
                bucket: "codex".into(),
                used_percent: Some(used),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 0.000001, "{a} != {b}");
}

#[test]
fn load_quota_and_unknown_expired_data_are_normalized() {
    let (_temp, ops, access) = setup();
    let r = request();
    let mut inner = ops.inner.lock().unwrap();
    let d = selected(&inner, &access, &r).1;
    close(d.score.unwrap(), 0.805);
    inner.settings.accounts[0].max_concurrency = 4;
    inner.active.insert(
        Uuid::new_v4(),
        super::super::Active {
            account: Some(Uuid::from_u128(10)),
            group: Some(DEFAULT_GROUP),
            limited: true,
            key_id: None,
            exclusive: false,
        },
    );
    assert_eq!(selected(&inner, &access, &r).0, Uuid::from_u128(20));
    inner.active.clear();
    quota(&mut inner, 0, 90.0);
    quota(&mut inner, 1, 10.0);
    assert_eq!(selected(&inner, &access, &r).0, Uuid::from_u128(20));
    let snapshot = inner.runtime.quotas.get_mut(&Uuid::from_u128(10)).unwrap();
    snapshot.checked_at = crate::now_ms() - 300_001;
    close(
        quota_score(&inner, &inner.settings.accounts[0], crate::now_ms()),
        0.5,
    );
    let snapshot = inner.runtime.quotas.get_mut(&Uuid::from_u128(10)).unwrap();
    snapshot.checked_at = crate::now_ms();
    snapshot.windows[0].resets_at = Some(crate::now_ms() - 1);
    close(
        quota_score(&inner, &inner.settings.accounts[0], crate::now_ms()),
        0.5,
    );
    snapshot_reset(&mut inner);
    fn snapshot_reset(inner: &mut Inner) {
        let snapshot = inner.runtime.quotas.get_mut(&Uuid::from_u128(10)).unwrap();
        snapshot.windows[0].resets_at = None;
        snapshot.windows.push(quota::QuotaWindow {
            bucket: "codex".into(),
            used_percent: Some(95.0),
            ..Default::default()
        });
        close(
            quota_score(inner, &inner.settings.accounts[0], crate::now_ms()),
            0.05,
        );
    }
}

#[test]
fn latency_requires_five_fresh_comparable_samples() {
    let (_temp, ops, access) = setup();
    let r = request();
    let mut inner = ops.inner.lock().unwrap();
    metric(&mut inner, 0, &r, 9000.0, 4);
    metric(&mut inner, 1, &r, 100.0, 4);
    close(
        selected(&inner, &access, &r).1.components.unwrap().latency,
        0.5,
    );
    metric(&mut inner, 0, &r, 9000.0, 5);
    metric(&mut inner, 1, &r, 100.0, 5);
    assert_eq!(selected(&inner, &access, &r).0, Uuid::from_u128(20));
    for different in [
        RoutingRequest {
            model: "other".into(),
            ..r.clone()
        },
        RoutingRequest {
            reasoning_effort: Some("high".into()),
            ..r.clone()
        },
        RoutingRequest {
            class: RequestClass::Tools,
            ..r.clone()
        },
    ] {
        close(
            selected(&inner, &access, &different)
                .1
                .components
                .unwrap()
                .latency,
            0.5,
        );
    }
    for metric in inner.runtime.scheduling.metrics.values_mut() {
        metric.latency.as_mut().unwrap().2 = Instant::now() - SAMPLE_TTL;
    }
    close(
        selected(&inner, &access, &r).1.components.unwrap().latency,
        0.5,
    );
}

#[test]
fn feedback_excludes_client_storage_and_queue_errors_and_is_recorded_once() {
    let (_temp, ops, access) = setup();
    let mut inner = ops.inner.lock().unwrap();
    let r = request();
    let (account, decision) = selected(&inner, &access, &r);
    let key = MetricKey::new(&inner.settings.accounts[0], &r)
        .unwrap()
        .account_key();
    for code in [
        "REQUEST_CANCELLED",
        "INVALID_INPUT",
        "QUEUE_TIMEOUT",
        "OPERATIONS_STORAGE",
        "UPSTREAM_REQUEST_REJECTED",
        "IO_ERROR",
    ] {
        admitted(&mut inner, r.id, &access, &r, account, &decision);
        finished(&mut inner, r.id, false, Some(code), None);
        close(
            inner.runtime.scheduling.metrics[&key].health(Instant::now()),
            0.9,
        );
    }
    admitted(&mut inner, r.id, &access, &r, account, &decision);
    finished(&mut inner, r.id, false, Some("UPSTREAM_UNAVAILABLE"), None);
    finished(&mut inner, r.id, false, Some("UPSTREAM_UNAVAILABLE"), None);
    close(
        inner.runtime.scheduling.metrics[&key].health(Instant::now()),
        0.72,
    );
    assert_eq!(selected(&inner, &access, &r).0, Uuid::from_u128(20));
    let other = RoutingRequest {
        model: "other".into(),
        ..r.clone()
    };
    assert_eq!(selected(&inner, &access, &other).0, Uuid::from_u128(20));
    admitted(&mut inner, r.id, &access, &r, account, &decision);
    finished(&mut inner, r.id, true, None, Some(crate::now_ms() + 100));
    close(
        inner.runtime.scheduling.metrics[&key].health(Instant::now()),
        0.776,
    );
    inner
        .runtime
        .scheduling
        .metrics
        .get_mut(&key)
        .unwrap()
        .health
        .as_mut()
        .unwrap()
        .1 = Instant::now() - SAMPLE_TTL;
    close(
        inner.runtime.scheduling.metrics[&key].health(Instant::now()),
        0.9,
    );
}

#[tokio::test]
async fn round_robin_ties_exploration_and_pure_candidate_checks() {
    let (_temp, ops, access) = setup();
    for expected in [10, 20, 10, 20] {
        let lease = ops.acquire_inference(&access, request()).await.unwrap();
        assert_eq!(lease.account_id, Some(Uuid::from_u128(expected)));
        drop(lease);
    }
    let mut inner = ops.inner.lock().unwrap();
    let r = request();
    quota(&mut inner, 0, 99.0);
    quota(&mut inner, 1, 0.0);
    inner.runtime.scheduling.ordinary.insert(DEFAULT_GROUP, 19);
    let expected = Uuid::from_u128(10); // account 1 was least recently assigned
    for _ in 0..50 {
        let (id, d) = selected(&inner, &access, &r);
        assert_eq!(id, expected);
        assert_eq!(d.mode, "exploration");
    }
    assert_eq!(inner.runtime.scheduling.ordinary[&DEFAULT_GROUP], 19);
    let (id, d) = selected(&inner, &access, &r);
    admitted(&mut inner, r.id, &access, &r, id, &d);
    assert_eq!(inner.runtime.scheduling.ordinary[&DEFAULT_GROUP], 20);
    assert_eq!(selected(&inner, &access, &r).1.mode, "balanced");
}

#[tokio::test]
async fn affinity_is_scoped_expiring_and_preserves_the_available_home() {
    let (_temp, ops, access) = setup();
    let digest = [7; 32];
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let provider = ops.configured_provider(a).unwrap();
    remember(&ops, &access, "m", digest, a, &provider);
    let r = RoutingRequest {
        soft_affinity: Some(digest),
        ..request()
    };
    {
        let mut inner = ops.inner.lock().unwrap();
        inner.cursor.insert(DEFAULT_GROUP, a);
        assert_eq!(selected(&inner, &access, &r).1.mode, "softAffinity");
        assert!(inner.runtime.scheduling.ordinary.is_empty());
        let mut other = r.clone();
        other.soft_affinity = Some([8; 32]);
        assert_eq!(selected(&inner, &access, &other).1.affinity, "notFound");
        quota(&mut inner, 0, 99.0);
        quota(&mut inner, 1, 0.0);
        assert_eq!(
            selected(&inner, &access, &r).1.affinity,
            if crate::native_cache::enabled() {
                "hit"
            } else {
                "scoreGap"
            }
        );
        inner.runtime.quotas.clear();
        inner
            .runtime
            .scheduling
            .affinities
            .get_mut(&AffinityKey::new(&access, "m", digest))
            .unwrap()
            .expires = Instant::now();
        assert_eq!(selected(&inner, &access, &r).1.affinity, "expired");
    }
    remember(&ops, &access, "m", digest, a, &provider);
    let home_expires = ops.inner.lock().unwrap().runtime.scheduling.affinities
        [&AffinityKey::new(&access, "m", digest)]
        .expires;
    let held = ops.acquire_client(a, &access).await.unwrap();
    let lease = tokio::time::timeout(
        Duration::from_millis(200),
        ops.acquire_inference(&access, r.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lease.account_id, Some(b));
    drop(lease);
    drop(held);
    let created = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "other".into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap();
    let other = ops.authenticate_api_key(&created.secret).unwrap();
    assert_eq!(
        selected(&ops.inner.lock().unwrap(), &other, &r).1.affinity,
        "notFound"
    );
    remember(&ops, &access, "m", digest, b, &provider);
    if crate::native_cache::enabled() {
        let inner = ops.inner.lock().unwrap();
        assert_eq!(
            selected(&inner, &access, &r).0,
            a,
            "spillover must return to its original home"
        );
        assert!(
            inner.runtime.scheduling.affinities[&AffinityKey::new(&access, "m", digest)].expires
                > home_expires,
            "spillover admission must renew the home binding"
        );
        drop(inner);
        remember(&ops, &access, "m", digest, a, &provider);
        assert!(
            ops.inner.lock().unwrap().runtime.scheduling.affinities
                [&AffinityKey::new(&access, "m", digest)]
                .expires
                > home_expires
        );
    }
    assert_eq!(
        selected(&ops.inner.lock().unwrap(), &access, &r).1.affinity,
        if crate::native_cache::enabled() {
            "hit"
        } else {
            "ambiguous"
        }
    );
    remember(&ops, &access, "m", [9; 32], a, &provider);
    remember(&ops, &access, "m", [9; 32], a, &provider);
    let ambiguous = RoutingRequest {
        soft_affinity: Some([9; 32]),
        ..r
    };
    assert_eq!(
        selected(&ops.inner.lock().unwrap(), &access, &ambiguous)
            .1
            .affinity,
        if crate::native_cache::enabled() {
            "hit"
        } else {
            "ambiguous"
        }
    );
    ops.set_inference_route(a, ops.snapshot()["revision"].as_u64().unwrap(), None)
        .unwrap();
    let inner = ops.inner.lock().unwrap();
    assert!(
        inner
            .runtime
            .scheduling
            .metrics
            .keys()
            .all(|k| k.account != a)
    );
}

#[test]
fn capability_rank_precedes_score_and_affinity_and_unknown_is_available() {
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let provider = ops.configured_provider(a).unwrap();
    remember(&ops, &access, "m", [1; 32], a, &provider);
    let r = RoutingRequest {
        features: vec![Feature::ImageInput],
        soft_affinity: Some([1; 32]),
        ..request()
    };
    let mut inner = ops.inner.lock().unwrap();
    assert_eq!(selected(&inner, &access, &r).1.candidate_count, 2);
    quota(&mut inner, 0, 0.0);
    quota(&mut inner, 1, 99.0);
    inner.runtime.catalogs.insert(
        b,
        NativeCapabilities {
            provider: runtime::fingerprint(&provider),
            checked_at: crate::now_ms(),
            models: vec![json!({"id":"m","inputModalities":["text","image"]})],
            ..Default::default()
        },
    );
    let (id, d) = selected(&inner, &access, &r);
    assert_eq!(id, b);
    assert_eq!(d.candidate_count, 1);
    assert_eq!(d.affinity, "unavailable");
}

#[tokio::test]
async fn hard_binding_waits_for_its_account_and_preserves_queue_order_and_deadline() {
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let first = ops.acquire_client(a, &access).await.unwrap();
    let second = ops.acquire_client(b, &access).await.unwrap();
    let r = request();
    let id = r.id;
    let cloned = ops.clone();
    let client = access.clone();
    let queued =
        tokio::spawn(async move { cloned.acquire_inference_on(&client, r, Some(a)).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.inner.lock().unwrap().pending.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let deadline = ops.inner.lock().unwrap().pending[0].deadline;
    drop(second);
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    assert_eq!(ops.inner.lock().unwrap().pending[0].deadline, deadline);
    assert!(
        ops.inner
            .lock()
            .unwrap()
            .runtime
            .scheduling
            .ordinary
            .is_empty()
    );
    drop(first);
    let lease = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, Some(a));
    drop(lease);
    let stats = ops.statistics(StatsQuery::default()).unwrap();
    let record = stats["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id.to_string())
        .unwrap();
    assert_eq!(record["scheduling"]["mode"], "hardBinding");
}

#[test]
fn caches_are_bounded_and_route_changes_invalidate_records() {
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let provider = ops.configured_provider(a).unwrap();
    for n in 0..CAPACITY + 2 {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&(n as u64).to_le_bytes());
        remember(&ops, &access, "m", digest, a, &provider);
    }
    {
        let inner = ops.inner.lock().unwrap();
        assert_eq!(inner.runtime.scheduling.affinities.len(), CAPACITY);
        assert!(
            !inner
                .runtime
                .scheduling
                .affinities
                .contains_key(&AffinityKey::new(&access, "m", [0; 32]))
        );
    }
    {
        let mut inner = ops.inner.lock().unwrap();
        for n in 0..CAPACITY + 2 {
            let r = RoutingRequest {
                model: format!("m{n}"),
                ..request()
            };
            admitted(&mut inner, r.id, &access, &r, a, &Decision::bound());
            finished(&mut inner, r.id, true, None, Some(crate::now_ms() + 1));
        }
        assert_eq!(inner.runtime.scheduling.metrics.len(), CAPACITY);
    }
    let mut changed = provider;
    changed.model = "new".into();
    ops.set_inference_route(
        a,
        ops.snapshot()["revision"].as_u64().unwrap(),
        Some(changed),
    )
    .unwrap();
    let inner = ops.inner.lock().unwrap();
    assert!(inner.runtime.scheduling.metrics.is_empty());
    assert!(inner.runtime.scheduling.affinities.is_empty());
}

#[tokio::test]
async fn queued_requests_rescore_without_jumping_or_advancing_exploration() {
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let first = ops.acquire_client(a, &access).await.unwrap();
    let second = ops.acquire_client(b, &access).await.unwrap();
    let r = request();
    let older_id = r.id;
    let copy = ops.clone();
    let client = access.clone();
    let older = tokio::spawn(async move { copy.acquire_inference(&client, r).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.inner.lock().unwrap().pending.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let original_deadline = ops.inner.lock().unwrap().pending[0].deadline;
    // Both accounts become available together. Change the quota preference while
    // still holding the admission mutex, before either waiter can claim.
    {
        let mut inner = ops.inner.lock().unwrap();
        quota(&mut inner, 0, 99.0);
        quota(&mut inner, 1, 0.0);
        inner.active.clear();
        for _ in 0..30 {
            assert_eq!(Operations::first_eligible(&inner), Some(older_id));
        }
        assert!(inner.runtime.scheduling.ordinary.is_empty());
        assert_eq!(inner.pending[0].deadline, original_deadline);
    }
    let r = request();
    let copy = ops.clone();
    let client = access.clone();
    let newer = tokio::spawn(async move { copy.acquire_inference(&client, r).await });
    drop(first);
    drop(second);
    let older = tokio::time::timeout(Duration::from_secs(2), older)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(older.account_id, Some(b));
    let newer = tokio::time::timeout(Duration::from_secs(2), newer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(newer.account_id, Some(a));
    let inner = ops.inner.lock().unwrap();
    assert_eq!(inner.runtime.scheduling.ordinary[&DEFAULT_GROUP], 2);
    assert_eq!(inner.active.len(), 2);
    drop(inner);
    drop(older);
    drop(newer);
}
