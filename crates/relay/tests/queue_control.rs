use codex_relay::operations::{Operations, QueueQuery, StatsQuery};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

async fn queued(ops: &Operations, count: u64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while ops.snapshot()["queued"] != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn spawn(
    ops: &Arc<Operations>,
) -> tokio::task::JoinHandle<codex_relay::error::Result<codex_relay::operations::Lease>> {
    let ops = ops.clone();
    tokio::spawn(async move { ops.acquire(None, None, "diagnostic", "queued").await })
}
fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn queued_id(ops: &Operations) -> Uuid {
    Uuid::parse_str(
        ops.queue(QueueQuery::default()).unwrap()["records"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn administrator_cancels_only_waiting_request_and_repeated_cancel_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let waiting = spawn(&ops);
    queued(&ops, 1).await;
    let id = queued_id(&ops);
    assert_eq!(ops.cancel_queued(id).unwrap()["alreadyCancelled"], false);
    assert_eq!(ops.cancel_queued(id).unwrap()["alreadyCancelled"], true);
    assert_eq!(
        waiting.await.unwrap().err().unwrap().code,
        "REQUEST_CANCELLED"
    );
    assert_eq!(ops.snapshot()["active"], 1);
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(
        ops.cancel_queued(Uuid::new_v4()).err().unwrap().code,
        "NOT_FOUND"
    );
    let stats = ops
        .statistics(serde_json::from_value::<StatsQuery>(json!({})).unwrap())
        .unwrap();
    let running = stats["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "running")
        .unwrap();
    assert_eq!(
        ops.cancel_queued(Uuid::parse_str(running["id"].as_str().unwrap()).unwrap())
            .err()
            .unwrap()
            .code,
        "REQUEST_NOT_QUEUED"
    );
    assert_eq!(stats["summary"]["cancelled"], 1);
    let replacement = spawn(&ops);
    queued(&ops, 1).await;
    held.complete(Ok(())).unwrap();
    replacement
        .await
        .unwrap()
        .unwrap()
        .complete(Ok(()))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_racing_with_dispatch_has_one_terminal_owner() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    for _ in 0..20 {
        let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
        let waiting = spawn(&ops);
        queued(&ops, 1).await;
        let id = queued_id(&ops);
        let release = tokio::spawn(async move {
            held.complete(Ok(())).unwrap();
        });
        let result = ops.cancel_queued(id);
        release.await.unwrap();
        let acquired = waiting.await.unwrap();
        match result {
            Ok(_) => assert_eq!(acquired.err().unwrap().code, "REQUEST_CANCELLED"),
            Err(e) => {
                assert_eq!(e.code, "REQUEST_NOT_QUEUED");
                acquired.unwrap().complete(Ok(())).unwrap();
            }
        }
        assert_eq!(ops.snapshot()["active"], 0);
        assert_eq!(ops.snapshot()["queued"], 0);
    }
}

#[tokio::test]
async fn live_queue_is_ordered_paginated_and_reports_remaining_time() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let mut tasks = vec![];
    for n in 1..=53 {
        tasks.push(spawn(&ops));
        queued(&ops, n).await;
    }
    let first = ops.queue(QueueQuery::default()).unwrap();
    assert_eq!(first["total"], 53);
    assert_eq!(first["records"].as_array().unwrap().len(), 50);
    assert_eq!(first["records"][0]["position"], 1);
    assert!(first["records"][0]["remainingMs"].as_u64().unwrap() <= 30000);
    let second = ops
        .queue(QueueQuery {
            page: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(second["records"][0]["position"], 51);
    assert_eq!(second["records"].as_array().unwrap().len(), 3);
    assert!(
        ops.queue(QueueQuery {
            page: Some(0),
            ..Default::default()
        })
        .is_err()
    );
    assert_eq!(
        ops.queue(QueueQuery {
            group_id: Some(Uuid::new_v4()),
            ..Default::default()
        })
        .unwrap()["total"],
        0
    );
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    drop(held);
}

#[tokio::test]
async fn shutdown_releases_queue_rejects_new_work_and_allows_cleanup_and_active_completion() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let waiting = spawn(&ops);
    queued(&ops, 1).await;
    ops.start_shutdown().unwrap();
    assert_eq!(
        waiting.await.unwrap().err().unwrap().code,
        "SERVICE_SHUTDOWN"
    );
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(ops.snapshot()["active"], 1);
    assert_eq!(ops.snapshot()["draining"], true);
    assert_eq!(
        ops.acquire(None, None, "diagnostic", "new")
            .await
            .err()
            .unwrap()
            .code,
        "SERVICE_DRAINING"
    );
    ops.begin(None, None, "management", "stop", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    held.complete(Ok(())).unwrap();
    ops.start_shutdown().unwrap();
    let stats = ops
        .statistics(serde_json::from_value::<StatsQuery>(json!({"status":"cancelled"})).unwrap())
        .unwrap();
    assert_eq!(stats["summary"]["total"], 1);
    assert_eq!(stats["records"][0]["errorCode"], "SERVICE_SHUTDOWN");
}

#[tokio::test]
async fn shutdown_does_not_leave_waiters_blocked_when_journal_fails() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let held = ops.acquire(None, None, "diagnostic", "held").await.unwrap();
    let waiting = spawn(&ops);
    queued(&ops, 1).await;
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_update BEFORE UPDATE ON requests BEGIN SELECT RAISE(FAIL,'fault'); END;").unwrap();
    assert!(ops.start_shutdown().is_err());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "SERVICE_SHUTDOWN"
    );
    assert_eq!(ops.snapshot()["queued"], 0);
    assert_eq!(ops.snapshot()["storageHealthy"], false);
    drop(held);
}
