use super::tests::{request, selected, setup};
use super::*;

fn routed(digest: u8) -> RoutingRequest {
    RoutingRequest {
        soft_affinity: Some([digest; 32]),
        ..request()
    }
}

#[tokio::test]
async fn first_admission_binds_before_completion_and_spillover_preserves_home() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let r = routed(1);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    let first = ops.acquire_inference(&access, r).await.unwrap();
    let home = first.account_id.unwrap();
    let (revision, expiry) = {
        let inner = ops.inner.lock().unwrap();
        let e = &inner.runtime.scheduling.affinities[&key];
        (e.revision, e.expires)
    };
    let spill = tokio::time::timeout(
        Duration::from_secs(2),
        ops.acquire_inference(&access, routed(1)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(spill.account_id, Some(home));
    {
        let inner = ops.inner.lock().unwrap();
        let e = &inner.runtime.scheduling.affinities[&key];
        assert_eq!((e.account, e.revision), (home, revision));
        assert!(e.expires > expiry);
    }
    drop(first);
    let returned = ops.acquire_inference(&access, routed(1)).await.unwrap();
    assert_eq!(returned.account_id, Some(home));
    drop((returned, spill));
}

#[tokio::test]
async fn children_inherit_once_and_never_create_or_renew_parent_bindings() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let parent = ops.acquire_inference(&access, routed(1)).await.unwrap();
    let home = parent.account_id;
    let parent_key = AffinityKey::new(&access, "m", [1; 32]);
    let parent_expiry =
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&parent_key].expires;
    let child = RoutingRequest {
        parent_affinity: Some([1; 32]),
        ..routed(2)
    };
    // Busy parent: create the child's own home immediately on another account.
    let lease = ops.acquire_inference(&access, child.clone()).await.unwrap();
    let child_home = lease.account_id;
    assert_ne!(home, child_home);
    drop((parent, lease));
    let lease = ops
        .acquire_inference(
            &access,
            RoutingRequest {
                id: Uuid::new_v4(),
                ..child
            },
        )
        .await
        .unwrap();
    assert_eq!(lease.account_id, child_home);
    drop(lease);
    // A different, unbound child inherits an available parent regardless of cursor.
    ops.inner
        .lock()
        .unwrap()
        .cursor
        .insert(access.group_id, home.unwrap());
    let inherited = ops
        .acquire_inference(
            &access,
            RoutingRequest {
                parent_affinity: Some([1; 32]),
                ..routed(3)
            },
        )
        .await
        .unwrap();
    assert_eq!(inherited.account_id, home);
    drop(inherited);
    let unknown = ops
        .acquire_inference(
            &access,
            RoutingRequest {
                parent_affinity: Some([9; 32]),
                ..routed(4)
            },
        )
        .await
        .unwrap();
    drop(unknown);
    let inner = ops.inner.lock().unwrap();
    assert_eq!(
        inner.runtime.scheduling.affinities[&parent_key].expires,
        parent_expiry
    );
    assert!(
        !inner
            .runtime
            .scheduling
            .affinities
            .contains_key(&AffinityKey::new(&access, "m", [9; 32]))
    );
}

#[test]
fn expiration_aba_and_late_completion_are_version_fenced() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let r = routed(1);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    let start = Instant::now();
    let provider = ops.configured_provider(a).unwrap();
    let mut inner = ops.inner.lock().unwrap();
    commit_affinity(&mut inner, &access, &r, a, start);
    assert_eq!(
        inner.runtime.scheduling.affinities[&key].expires,
        start + Duration::from_secs(24 * 60 * 60)
    );
    let old = inner.runtime.scheduling.affinities[&key].revision;
    commit_affinity(&mut inner, &access, &r, b, start + NATIVE_AFFINITY_TTL);
    let middle = inner.runtime.scheduling.affinities[&key].revision;
    assert_ne!(old, middle);
    commit_affinity(&mut inner, &access, &r, a, start + NATIVE_AFFINITY_TTL * 2);
    let newest = inner.runtime.scheduling.affinities[&key].revision;
    assert_ne!(old, newest);
    inner
        .runtime
        .scheduling
        .remove_affinity(&key, old, &runtime::fingerprint(&provider));
    assert_eq!(inner.runtime.scheduling.affinities[&key].revision, newest);
    drop(inner);
    ops.remember_soft_affinity(&access, "m", [1; 32], b, &provider);
    let mut inner = ops.inner.lock().unwrap();
    assert_eq!(inner.runtime.scheduling.affinities[&key].revision, newest);
    inner.runtime.scheduling.affinities.remove(&key);
    drop(inner);
    ops.remember_soft_affinity(&access, "m", [1; 32], a, &provider);
    assert!(
        ops.inner
            .lock()
            .unwrap()
            .runtime
            .scheduling
            .affinities
            .is_empty()
    );
}

#[tokio::test]
async fn queue_reads_current_binding_and_only_running_transition_renews_it() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let first = ops.acquire_client(a, &access).await.unwrap();
    let second = ops.acquire_client(b, &access).await.unwrap();
    let r = routed(1);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    let (owner, client) = (ops.clone(), access.clone());
    let pending = tokio::spawn(async move { owner.acquire_inference(&client, r).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.inner.lock().unwrap().pending.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let mut inner = ops.inner.lock().unwrap();
        assert!(inner.runtime.scheduling.affinities.is_empty());
        // A different admitted request establishes a home while this request queues.
        commit_affinity(
            &mut inner,
            &access,
            &routed(1),
            b,
            Instant::now() - Duration::from_secs(60),
        );
    }
    let expiry = ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].expires;
    for _ in 0..10 {
        let inner = ops.inner.lock().unwrap();
        Operations::first_eligible(&inner);
    }
    assert_eq!(
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].expires,
        expiry
    );
    drop(second);
    let lease = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, Some(b));
    assert!(ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].expires > expiry);
    drop((lease, first));
}

#[tokio::test]
async fn hard_binding_wins_and_direct_admissions_do_not_touch_native_affinity() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let first = ops
        .acquire_inference_on(&access, routed(1), Some(a))
        .await
        .unwrap();
    drop(first);
    let bound = ops
        .acquire_inference_on(&access, routed(1), Some(b))
        .await
        .unwrap();
    assert_eq!(bound.account_id, Some(b));
    drop(bound);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    let mut inner = ops.inner.lock().unwrap();
    assert_eq!(inner.runtime.scheduling.affinities[&key].account, a);
    let expiry = inner.runtime.scheduling.affinities[&key].expires;
    inner
        .settings
        .accounts
        .iter_mut()
        .find(|p| p.id == b)
        .unwrap()
        .route
        .as_mut()
        .unwrap()
        .execution = crate::provider::Execution::ApiKey;
    commit_affinity(&mut inner, &access, &routed(1), b, Instant::now());
    commit_affinity(&mut inner, &access, &routed(2), b, Instant::now());
    assert_eq!(inner.runtime.scheduling.affinities[&key].expires, expiry);
    assert_eq!(inner.runtime.scheduling.affinities.len(), 1);
}

#[tokio::test]
async fn admission_storage_failure_never_creates_affinity_or_capacity() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    ops.inner.lock().unwrap().db.execute_batch("CREATE TEMP TRIGGER deny_admission BEFORE INSERT ON requests BEGIN SELECT RAISE(FAIL, 'fixture'); END;").unwrap();
    assert!(ops.acquire_inference(&access, routed(1)).await.is_err());
    let inner = ops.inner.lock().unwrap();
    assert!(inner.runtime.scheduling.affinities.is_empty());
    assert!(inner.active.is_empty());
}

#[tokio::test]
async fn cancelled_waiter_does_not_renew_binding() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    commit_affinity(
        &mut ops.inner.lock().unwrap(),
        &access,
        &routed(1),
        a,
        Instant::now() - Duration::from_secs(60),
    );
    let expiry = ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].expires;
    let first = ops.acquire_client(a, &access).await.unwrap();
    let second = ops.acquire_client(b, &access).await.unwrap();
    let (owner, client) = (ops.clone(), access.clone());
    let pending = tokio::spawn(async move { owner.acquire_inference(&client, routed(1)).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.inner.lock().unwrap().pending.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    assert!(pending.await.is_err());
    assert_eq!(
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].expires,
        expiry
    );
    assert!(ops.inner.lock().unwrap().pending.is_empty());
    drop((first, second));
}

#[test]
fn inheritance_is_scoped_and_invalidated_by_route_generation() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let mut inner = ops.inner.lock().unwrap();
    commit_affinity(&mut inner, &access, &routed(1), a, Instant::now());
    let child = RoutingRequest {
        parent_affinity: Some([1; 32]),
        ..routed(2)
    };
    assert_eq!(selected(&inner, &access, &child).0, a);
    let mut other = access.clone();
    other.key_id = Uuid::new_v4();
    let candidates: Vec<_> = inner.settings.accounts.iter().collect();
    assert_eq!(
        choose(&inner, &candidates, &other, &child)
            .unwrap()
            .1
            .affinity,
        "notFound"
    );
    other = access.clone();
    other.group_id = Uuid::new_v4();
    assert_eq!(
        choose(&inner, &candidates, &other, &child)
            .unwrap()
            .1
            .affinity,
        "notFound"
    );
    let another = RoutingRequest {
        model: "other".into(),
        ..child.clone()
    };
    assert_eq!(
        choose(&inner, &candidates, &access, &another)
            .unwrap()
            .1
            .affinity,
        "notFound"
    );
    inner
        .settings
        .accounts
        .iter_mut()
        .find(|p| p.id == a)
        .unwrap()
        .route
        .as_mut()
        .unwrap()
        .auth = "changed".into();
    assert_eq!(selected(&inner, &access, &child).1.affinity, "notFound");
}

#[tokio::test]
async fn changing_key_or_account_groups_keeps_eligible_affinity_and_rechecks_access() {
    if !crate::native_cache::enabled() {
        return;
    }
    use crate::operations::{AccountInput, ApiKeyGroup, ApiKeyUpdate, GroupInput};
    let (_temp, ops, access) = setup();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    commit_affinity(
        &mut ops.inner.lock().unwrap(),
        &access,
        &routed(1),
        a,
        Instant::now(),
    );
    let revision = ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].revision;
    let group = ops
        .save_group(
            None,
            GroupInput {
                revision: ops.snapshot()["revision"].as_u64().unwrap(),
                name: "second".into(),
                enabled: true,
            },
        )
        .unwrap();
    for account in [a, b] {
        ops.set_account_groups(
            account,
            AccountInput {
                revision: ops.snapshot()["revision"].as_u64().unwrap(),
                group_id: access.group_id,
                enabled: true,
                max_concurrency: 1,
            },
            vec![access.group_id, group],
        )
        .unwrap();
    }
    let moved = ops
        .set_api_key_group(
            access.key_id,
            ApiKeyGroup {
                revision: 1,
                group_id: group,
            },
        )
        .unwrap();
    let moved_access = ClientAccess {
        group_id: group,
        ..access.clone()
    };
    assert!(key == AffinityKey::new(&moved_access, "m", [1; 32]));
    let lease = ops
        .acquire_inference(&moved_access, routed(1))
        .await
        .unwrap();
    assert_eq!(lease.account_id, Some(a));
    drop(lease);
    assert_eq!(
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].revision,
        revision
    );
    ops.update_api_key(
        access.key_id,
        ApiKeyUpdate {
            revision: moved.revision,
            group_id: Some(access.group_id),
            ..Default::default()
        },
    )
    .unwrap();
    ops.set_account_groups(
        a,
        AccountInput {
            revision: ops.snapshot()["revision"].as_u64().unwrap(),
            group_id: group,
            enabled: true,
            max_concurrency: 1,
        },
        vec![group],
    )
    .unwrap();
    assert_eq!(
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].revision,
        revision
    );
    let lease = ops.acquire_inference(&access, routed(1)).await.unwrap();
    assert_eq!(lease.account_id, Some(b));
    drop(lease);
    assert_ne!(
        ops.inner.lock().unwrap().runtime.scheduling.affinities[&key].revision,
        revision
    );
}

#[test]
fn configured_ttl_applies_on_next_successful_admission_without_retroactive_extension() {
    if !crate::native_cache::enabled() {
        return;
    }
    let (_temp, ops, access) = setup();
    let now = Instant::now();
    let a = Uuid::from_u128(10);
    let key = AffinityKey::new(&access, "m", [1; 32]);
    commit_affinity(&mut ops.inner.lock().unwrap(), &access, &routed(1), a, now);
    ops.set_native_affinity_ttl(ops.snapshot()["revision"].as_u64().unwrap(), 2)
        .unwrap();
    let mut inner = ops.inner.lock().unwrap();
    assert_eq!(
        inner.runtime.scheduling.affinities[&key].expires,
        now + Duration::from_secs(24 * 60 * 60)
    );
    commit_affinity(
        &mut inner,
        &access,
        &routed(1),
        a,
        now + Duration::from_secs(60),
    );
    assert_eq!(
        inner.runtime.scheduling.affinities[&key].expires,
        now + Duration::from_secs(121 * 60)
    );
}
