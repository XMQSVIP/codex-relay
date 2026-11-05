use codex_relay::operations::{
    AccountInput, ApiKeyConcurrency, ApiKeyCreate, ApiKeyUpdate, ClientAccess, DEFAULT_GROUP,
    Operations, QueueQuery,
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

fn input(limit: u32) -> ApiKeyCreate {
    serde_json::from_value(json!({"name":"key test","groupId":DEFAULT_GROUP,"models":["a"],"expiresInDays":1,"maxConcurrency":limit})).unwrap()
}
fn policy(revision: u64, max_concurrency: u32) -> ApiKeyConcurrency {
    ApiKeyConcurrency {
        revision,
        max_concurrency,
    }
}
fn account(ops: &Operations, name: &str, limit: u32) -> Uuid {
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, name.into())]).unwrap();
    ops.set_account(
        id,
        AccountInput {
            revision: ops.snapshot()["revision"].as_u64().unwrap(),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: limit,
        },
    )
    .unwrap();
    id
}
async fn queued(ops: &Operations, count: u64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.snapshot()["queued"] != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn spawn(
    ops: &Arc<Operations>,
    account: Uuid,
    access: &ClientAccess,
) -> tokio::task::JoinHandle<codex_relay::error::Result<codex_relay::operations::Lease>> {
    let ops = ops.clone();
    let access = access.clone();
    tokio::spawn(async move { ops.acquire_client(account, &access).await })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn key_slots_are_atomic_and_reusable_without_a_frequency_limit() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = account(&ops, "shared", 4);
    let key = ops.create_api_key(input(2)).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let ops = ops.clone();
        let access = access.clone();
        let running = running.clone();
        let peak = peak.clone();
        tasks.push(tokio::spawn(async move {
            let lease = ops.acquire_client(id, &access).await.unwrap();
            let active = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            running.fetch_sub(1, Ordering::SeqCst);
            lease.complete(Ok(())).unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= 2);
    for _ in 0..30 {
        ops.acquire_client(id, &access)
            .await
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(ops.snapshot()["queued"], 0);
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM requests WHERE key_id=?1 AND status='succeeded'",
            [key.key.id.to_string()],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        54
    );
}

#[tokio::test]
async fn one_key_shares_a_limit_across_accounts_without_blocking_another_key() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let a = account(&ops, "a", 1);
    let b = account(&ops, "b", 1);
    // Persisted group limits must not merge independent callers' quotas.
    ops.save_group(
        Some(DEFAULT_GROUP),
        serde_json::from_value(json!({
            "revision": ops.snapshot()["revision"], "name": "默认分组",
            "enabled": true
        }))
        .unwrap(),
    )
    .unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let one = ops.create_api_key(input(1)).unwrap();
    let two = ops.create_api_key(input(1)).unwrap();
    let access = ops.authenticate_api_key(&one.secret).unwrap();
    let held = ops.acquire_client(a, &access).await.unwrap();
    let pending = spawn(&ops, b, &access);
    queued(&ops, 1).await;
    let other = tokio::time::timeout(
        Duration::from_secs(2),
        ops.acquire_client(b, &ops.authenticate_api_key(&two.secret).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ops.snapshot()["active"], 2);
    other.complete(Ok(())).unwrap();
    assert!(!pending.is_finished());
    held.complete(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(ops.snapshot()["active"], 0);
}

#[tokio::test]
async fn live_edits_wake_waiters_and_lowering_does_not_interrupt_active_requests() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = account(&ops, "shared", 4);
    let key = ops.create_api_key(input(1)).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let first = ops.acquire_client(id, &access).await.unwrap();
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    ops.set_api_key_concurrency(key.key.id, policy(1, 2))
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    ops.set_api_key_concurrency(key.key.id, policy(2, 1))
        .unwrap();
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    assert_eq!(ops.snapshot()["active"], 2);
    first.complete(Ok(())).unwrap();
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());
    assert_eq!(ops.snapshot()["active"], 1);
    drop(second);
    tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(
        ops.set_api_key_concurrency(key.key.id, policy(1, 3))
            .unwrap_err()
            .code,
        "CONFIG_CONFLICT"
    );
    assert_eq!(
        ops.update_api_key(
            key.key.id,
            ApiKeyUpdate {
                revision: 1,
                state: Some("paused".into()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        "CONFIG_CONFLICT"
    );
    assert!(
        ops.set_api_key_concurrency(key.key.id, policy(3, 257))
            .is_err()
    );
    assert!(ops.create_api_key(input(257)).is_err());
}

#[tokio::test]
async fn cancelled_queued_requests_release_no_active_slot_and_revocation_wakes_waiters() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = account(&ops, "shared", 4);
    let key = ops.create_api_key(input(1)).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let held = ops.acquire_client(id, &access).await.unwrap();
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    let queue = ops.queue(QueueQuery::default()).unwrap();
    let request_id: Uuid = queue["records"][0]["id"].as_str().unwrap().parse().unwrap();
    ops.cancel_queued(request_id).unwrap();
    assert_eq!(
        pending.await.unwrap().err().unwrap().code,
        "REQUEST_CANCELLED"
    );
    assert_eq!(ops.snapshot()["active"], 1);
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    ops.update_api_key(
        key.key.id,
        ApiKeyUpdate {
            revision: 1,
            state: Some("revoked".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "INVALID_API_KEY"
    );
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(ops.snapshot()["active"], 1);
    held.complete(Ok(())).unwrap();
    assert!(
        ops.set_api_key_concurrency(key.key.id, policy(2, 2))
            .is_err()
    );
}

#[tokio::test]
async fn key_waiters_use_existing_queue_bound_timeout_and_drop_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = account(&ops, "shared", 4);
    let key = ops.create_api_key(input(1)).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    ops.set_queue(ops.snapshot()["revision"].as_u64().unwrap(), 1, 1)
        .unwrap();
    let held = ops.acquire_client(id, &access).await.unwrap();
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    assert_eq!(
        ops.acquire_client(id, &access).await.err().unwrap().code,
        "QUEUE_FULL"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "QUEUE_TIMEOUT"
    );
    let pending = spawn(&ops, id, &access);
    queued(&ops, 1).await;
    pending.abort();
    let _ = pending.await;
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(ops.snapshot()["active"], 1);
    drop(held);
    ops.acquire_client(id, &access)
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
}

#[tokio::test]
async fn missing_key_columns_are_rejected_without_upgrading_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    drop(ops);
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute_batch("ALTER TABLE api_keys DROP COLUMN max_concurrency")
        .unwrap();
    assert_eq!(
        Operations::open(dir.path()).err().unwrap().code,
        "DATABASE_SCHEMA"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('api_keys') WHERE name='max_concurrency'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn groups_can_be_created_without_concurrency_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let group = serde_json::from_value(json!({
        "revision": ops.snapshot()["revision"], "name": "授权组", "enabled": true
    }))
    .unwrap();
    ops.save_group(None, group).unwrap();
}

#[tokio::test]
async fn group_changes_and_deletion_reject_stale_waiters_but_preserve_running_statistics() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let account_id = account(&ops, "old", 1);
    let target = ops
        .save_group(
            None,
            serde_json::from_value(
                json!({"revision":ops.snapshot()["revision"],"name":"target","enabled":true}),
            )
            .unwrap(),
        )
        .unwrap();
    let key = ops.create_api_key(input(1)).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let held = ops.acquire_client(account_id, &access).await.unwrap();
    let waiting = spawn(&ops, account_id, &access);
    queued(&ops, 1).await;
    let policy = |revision, group_id| codex_relay::operations::ApiKeyGroup { revision, group_id };
    assert!(
        ops.set_api_key_group(key.key.id, policy(1, Uuid::new_v4()))
            .is_err()
    );
    let moved = ops
        .set_api_key_group(key.key.id, policy(1, target))
        .unwrap();
    assert_eq!(moved.revision, 2);
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap().group_id,
        target
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "INVALID_API_KEY"
    );
    assert!(
        ops.set_api_key_group(key.key.id, policy(1, DEFAULT_GROUP))
            .is_err()
    );
    assert!(ops.delete_api_key(key.key.id, 1).is_err());
    held.complete(Ok(())).unwrap();
    ops.set_api_key_group(key.key.id, policy(2, DEFAULT_GROUP))
        .unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let held = ops.acquire_client(account_id, &access).await.unwrap();
    let waiting = spawn(&ops, account_id, &access);
    queued(&ops, 1).await;
    ops.delete_api_key(key.key.id, 3).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "INVALID_API_KEY"
    );
    assert!(ops.authenticate_api_key(&key.secret).is_err());
    assert!(ops.list_api_keys().unwrap().is_empty());
    held.complete(Ok(())).unwrap();
    ops.delete_api_key(key.key.id, 3).unwrap();
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM requests WHERE key_id=?1 AND status='succeeded'",
            [key.key.id.to_string()],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn revoked_keys_are_hidden_and_group_edit_keeps_secret_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let target = ops
        .save_group(
            None,
            serde_json::from_value(
                json!({"revision":ops.snapshot()["revision"],"name":"target","enabled":true}),
            )
            .unwrap(),
        )
        .unwrap();
    let key = ops.create_api_key(input(2)).unwrap();
    ops.set_api_key_group(
        key.key.id,
        codex_relay::operations::ApiKeyGroup {
            revision: 1,
            group_id: target,
        },
    )
    .unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let listed = ops.list_api_keys().unwrap().remove(0);
    assert_eq!(listed.group_id, target);
    assert_eq!(listed.secret.as_deref(), Some(key.secret.as_str()));
    assert_eq!(listed.max_concurrency, 2);
    ops.update_api_key(
        key.key.id,
        ApiKeyUpdate {
            revision: 2,
            state: Some("revoked".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(ops.list_api_keys().unwrap().is_empty());
    assert!(
        ops.set_api_key_group(
            key.key.id,
            codex_relay::operations::ApiKeyGroup {
                revision: 3,
                group_id: DEFAULT_GROUP
            }
        )
        .is_err()
    );
    ops.delete_api_key(key.key.id, 3).unwrap();
}
