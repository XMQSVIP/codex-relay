use codex_relay::operations::{
    AccountInput, DEFAULT_GROUP, GroupInput, Lease, Operations, StatsQuery,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn stats(ops: &Operations) -> serde_json::Value {
    ops.statistics(serde_json::from_value::<StatsQuery>(json!({})).unwrap())
        .unwrap()
}
async fn queued(ops: &Operations, count: u64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while ops.snapshot()["queued"] != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn request(
    ops: &Arc<Operations>,
    account: Option<Uuid>,
    group: Option<Uuid>,
    action: &'static str,
) -> JoinHandle<codex_relay::error::Result<Lease>> {
    let ops = ops.clone();
    tokio::spawn(async move { ops.acquire(account, group, "native_check", action).await })
}

#[tokio::test]
async fn bounded_fifo_releases_without_duplicate_records_or_lost_wakeups() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    ops.set_queue(revision(&ops), 2, 10).unwrap();
    let first = ops
        .acquire(None, None, "diagnostic", "initial")
        .await
        .unwrap();
    let second = request(&ops, None, None, "second");
    queued(&ops, 1).await;
    let third = request(&ops, None, None, "third");
    queued(&ops, 2).await;
    let error = ops
        .acquire(None, None, "diagnostic", "overflow")
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "QUEUE_FULL");
    assert_eq!(ops.snapshot()["active"], 1);
    first.complete(Ok(())).unwrap();
    let lease = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ops.snapshot()["queued"], 1);
    assert!(!third.is_finished());
    assert_eq!(ops.snapshot()["active"], 1);
    lease.complete(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), third)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    let result = stats(&ops);
    assert_eq!(result["summary"]["total"], 4);
    assert_eq!(result["summary"]["succeeded"], 3);
    assert_eq!(result["summary"]["queueFull"], 1);
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(ops.snapshot()["active"], 0);
    assert!(
        result["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["waitMs"].is_number())
    );
}

#[tokio::test]
async fn timeout_and_cancellation_remove_waiters_without_dispatching() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    ops.set_queue(revision(&ops), 2, 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let cancel = request(&ops, None, None, "cancel");
    assert_eq!(
        ops.acquire(None, Some(DEFAULT_GROUP), "native_check", "empty")
            .await
            .err()
            .unwrap()
            .code,
        "NO_ELIGIBLE_ACCOUNT"
    );
    queued(&ops, 1).await;
    cancel.abort();
    let _ = cancel.await;
    assert_eq!(ops.snapshot()["queued"], 0);
    let timeout = request(&ops, None, None, "timeout");
    queued(&ops, 1).await;
    assert_eq!(timeout.await.unwrap().err().unwrap().code, "QUEUE_TIMEOUT");
    assert_eq!(ops.snapshot()["active"], 1);
    assert_eq!(ops.snapshot()["queued"], 0);
    held.complete(Ok(())).unwrap();
    let result = stats(&ops);
    assert_eq!(result["summary"]["timedOut"], 1);
    assert_eq!(result["summary"]["cancelled"], 1);
    let record = result["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "timed_out")
        .unwrap();
    assert!(record["waitMs"].as_u64().unwrap() >= 950);
    assert!(record["durationMs"].is_null());
}

#[tokio::test]
async fn blocked_group_does_not_block_others_and_pause_wakes_waiters() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into())])
        .unwrap();
    let group = ops
        .save_group(
            None,
            GroupInput {
                revision: revision(&ops),
                name: "B group".into(),
                enabled: true,
            },
        )
        .unwrap();
    ops.set_account(
        b,
        AccountInput {
            revision: revision(&ops),
            group_id: group,
            enabled: true,
            max_concurrency: 1,
        },
    )
    .unwrap();
    let held = ops
        .acquire(Some(a), None, "native_check", "held")
        .await
        .unwrap();
    let blocked = request(&ops, None, Some(DEFAULT_GROUP), "blocked");
    queued(&ops, 1).await;
    let available = tokio::time::timeout(
        Duration::from_secs(1),
        ops.acquire(None, Some(group), "native_check", "other-group"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(available.account_id, Some(b));
    available.complete(Ok(())).unwrap();
    ops.save_group(
        Some(DEFAULT_GROUP),
        GroupInput {
            revision: revision(&ops),
            name: "Default".into(),
            enabled: false,
        },
    )
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "ROUTE_DISABLED"
    );
    ops.begin(Some(a), None, "management", "stop", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    drop(held);
    assert_eq!(ops.snapshot()["queued"], 0);
}

#[tokio::test]
async fn live_limits_wake_queue_and_account_moves_wait_for_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into())]).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let waiting = request(&ops, Some(a), None, "waiting");
    queued(&ops, 1).await;
    let group = ops
        .save_group(
            None,
            GroupInput {
                revision: revision(&ops),
                name: "target".into(),
                enabled: true,
            },
        )
        .unwrap();
    assert_eq!(
        ops.set_account(
            a,
            AccountInput {
                revision: revision(&ops),
                group_id: group,
                enabled: true,
                max_concurrency: 1
            }
        )
        .err()
        .unwrap()
        .code,
        "BUSY"
    );
    ops.set_global(revision(&ops), 2).unwrap();
    let admitted = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ops.snapshot()["active"], 2);
    assert_eq!(admitted.account_id, Some(a));
    drop(held);
    drop(admitted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contended_queue_never_exceeds_capacity_and_drains_all_requests() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 3).unwrap();
    let mut tasks = vec![];
    for _ in 0..40 {
        let ops = ops.clone();
        tasks.push(tokio::spawn(async move {
            let lease = ops.acquire(None, None, "diagnostic", "race").await.unwrap();
            assert!(ops.snapshot()["active"].as_u64().unwrap() <= 3);
            tokio::time::sleep(Duration::from_millis(2)).await;
            lease.complete(Ok(())).unwrap();
        }));
    }
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(stats(&ops)["summary"]["succeeded"], 40);
}

#[test]
fn old_database_is_rejected_without_rebuilding_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TABLE requests(id TEXT PRIMARY KEY); INSERT INTO requests VALUES('keep-me');",
    )
    .unwrap();
    let error = Operations::open(dir.path()).err().unwrap();
    assert_eq!(error.code, "DATABASE_SCHEMA");
    assert_eq!(
        db.query_row("SELECT id FROM requests", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep-me"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('requests')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn shrinking_queue_preserves_waiters_and_original_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    ops.set_queue(revision(&ops), 2, 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let first = request(&ops, None, None, "first");
    queued(&ops, 1).await;
    let second = request(&ops, None, None, "second");
    queued(&ops, 2).await;
    ops.set_queue(revision(&ops), 1, 30).unwrap();
    assert_eq!(ops.snapshot()["queued"], 2);
    assert_eq!(
        ops.acquire(None, None, "diagnostic", "overflow")
            .await
            .err()
            .unwrap()
            .code,
        "QUEUE_FULL"
    );
    for task in [first, second] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .err()
                .unwrap()
                .code,
            "QUEUE_TIMEOUT"
        );
    }
    assert_eq!(ops.snapshot()["queued"], 0);
    drop(held);
}

#[tokio::test]
async fn group_waiter_selects_available_account_only_when_dequeued() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into())])
        .unwrap();
    let held_a = ops
        .acquire(Some(a), None, "native_check", "held-a")
        .await
        .unwrap();
    let held_b = ops
        .acquire(Some(b), None, "native_check", "held-b")
        .await
        .unwrap();
    let waiting = request(&ops, None, Some(DEFAULT_GROUP), "group-waiter");
    queued(&ops, 1).await;
    assert_eq!(ops.snapshot()["groups"][0]["queued"], 1);
    assert_eq!(ops.snapshot()["accounts"][0]["queued"], 0);
    held_b.complete(Ok(())).unwrap();
    let admitted = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(admitted.account_id, Some(b));
    admitted.complete(Ok(())).unwrap();
    let rows = stats(&ops);
    let record = rows["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["action"] == "group-waiter")
        .unwrap();
    assert_eq!(record["accountId"], b.to_string());
    drop(held_a);
}
