use codex_relay::{
    operations::{
        AccountInput, ApiKeyCreate, ClientAccess, DEFAULT_GROUP, Operations, RoutingRequest,
    },
    provider::Provider,
};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn limit(ops: &Operations, id: Uuid, count: u32) {
    ops.set_account(
        id,
        AccountInput {
            revision: revision(ops),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: count,
        },
    )
    .unwrap();
}
fn key(ops: &Operations, name: &str, limit: u32) -> ClientAccess {
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: name.into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: limit,
        })
        .unwrap();
    ops.authenticate_api_key(&key.secret).unwrap()
}
fn request() -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: "m".into(),
        streaming: false,
        reasoning_effort: None,
        user_agent: None,
        features: vec![],
        ..Default::default()
    }
}
fn setup() -> (tempfile::TempDir, Arc<Operations>, Uuid) {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "shared".into())]).unwrap();
    limit(&ops, id, 2);
    ops.set_inference_route(
        id,
        revision(&ops),
        Some(Provider {
            execution: Default::default(),
            id: "fixture".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            auth: "none".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        }),
    )
    .unwrap();
    assert_eq!(ops.snapshot()["accounts"][0]["maxConcurrency"], 2);
    (temp, ops, id)
}
async fn queued(ops: &Operations) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn two_users_share_capacity_and_a_third_request_waits_until_release() {
    let (_temp, ops, id) = setup();
    let a = key(&ops, "A", 0);
    let b = key(&ops, "B", 0);
    let first = ops.acquire_inference(&a, request()).await.unwrap();
    let second = ops.acquire_inference(&b, request()).await.unwrap();
    assert_eq!(first.account_id, Some(id));
    assert_eq!(second.account_id, Some(id));
    assert_eq!(ops.snapshot()["active"], 2);
    let cloned = ops.clone();
    let waiting = tokio::spawn(async move { cloned.acquire_inference(&a, request()).await });
    queued(&ops).await;
    assert!(!waiting.is_finished());
    first.complete(Ok(())).unwrap();
    let third = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ops.snapshot()["active"], 2);
    drop((second, third));
    assert_eq!(ops.snapshot()["active"], 0);
}

#[tokio::test]
async fn one_key_can_run_two_conversations_and_lowering_limit_does_not_cancel_them() {
    let (_temp, ops, id) = setup();
    let a = key(&ops, "A", 0);
    let first = ops.acquire_inference(&a, request()).await.unwrap();
    let second = ops.acquire_inference(&a, request()).await.unwrap();
    limit(&ops, id, 1);
    assert_eq!(ops.snapshot()["active"], 2);
    let cloned = ops.clone();
    let waiting = tokio::spawn(async move { cloned.acquire_inference(&a, request()).await });
    queued(&ops).await;
    drop(first);
    assert!(!waiting.is_finished());
    drop(second);
    drop(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
}

#[tokio::test]
async fn key_limit_remains_effective_and_maintenance_excludes_inference() {
    let (_temp, ops, id) = setup();
    let a = key(&ops, "limited", 1);
    let b = key(&ops, "other", 0);
    let first = ops.acquire_inference(&a, request()).await.unwrap();
    let cloned = ops.clone();
    let waiting = tokio::spawn(async move { cloned.acquire_inference(&a, request()).await });
    queued(&ops).await;
    let other = ops.acquire_inference(&b, request()).await.unwrap();
    assert!(
        ops.begin(Some(id), None, "management", "import_credentials", true)
            .is_err()
    );
    waiting.abort();
    let _ = waiting.await;
    drop((first, other));
    let maintenance = ops
        .begin(Some(id), None, "management", "import_credentials", true)
        .unwrap();
    let cloned = ops.clone();
    let waiting = tokio::spawn(async move { cloned.acquire_inference(&b, request()).await });
    queued(&ops).await;
    drop(maintenance);
    drop(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
}

#[test]
fn activation_route_refresh_and_restart_preserve_parallel_capacity() {
    let (temp, ops, id) = setup();
    ops.set_account_activation(id, revision(&ops), false, None)
        .unwrap();
    ops.set_account_activation(id, revision(&ops), true, None)
        .unwrap();
    assert_eq!(ops.snapshot()["accounts"][0]["maxConcurrency"], 2);
    let provider = ops.configured_provider(id).unwrap();
    ops.set_inference_route(id, revision(&ops), Some(provider))
        .unwrap();
    drop(ops);
    let reopened = Operations::open(temp.path()).unwrap();
    assert_eq!(reopened.snapshot()["accounts"][0]["maxConcurrency"], 2);
}

#[tokio::test]
async fn automatic_capacity_admits_ten_and_adding_an_account_wakes_the_queue() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let add = |id| {
        ops.register_accounts(&[(id, "default capacity".into())])
            .unwrap();
        ops.set_inference_route(
            id,
            revision(&ops),
            Some(Provider {
                execution: Default::default(),
                id: "fixture".into(),
                model: "m".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    };
    for id in ids {
        add(id);
    }
    assert_eq!(ops.snapshot()["globalLimitAuto"], true);
    assert_eq!(ops.snapshot()["globalLimit"], 10);
    let access = key(&ops, "unlimited", 0);
    let mut leases = Vec::new();
    for _ in 0..10 {
        leases.push(
            tokio::time::timeout(
                Duration::from_secs(2),
                ops.acquire_inference(&access, request()),
            )
            .await
            .unwrap()
            .unwrap(),
        );
    }
    for id in ids {
        assert_eq!(
            leases.iter().filter(|l| l.account_id == Some(id)).count(),
            5
        );
    }
    let owner = ops.clone();
    let waiting = tokio::spawn(async move { owner.acquire_inference(&access, request()).await });
    queued(&ops).await;
    let third = Uuid::new_v4();
    add(third);
    let resumed = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(resumed.account_id, Some(third));
    assert_eq!(ops.snapshot()["globalLimit"], 15);
    assert_eq!(ops.snapshot()["active"], 11);
    ops.set_global(revision(&ops), 1).unwrap();
    assert_eq!(ops.snapshot()["active"], 11);
    drop((leases, resumed));
    assert_eq!(ops.snapshot()["active"], 0);
}

#[tokio::test]
async fn manual_capacity_stays_fixed_and_switching_to_auto_wakes_waiters() {
    let (_temp, ops, id) = setup();
    ops.set_global(revision(&ops), 1).unwrap();
    let access = key(&ops, "unlimited", 0);
    let first = ops.acquire_inference(&access, request()).await.unwrap();
    let owner = ops.clone();
    let waiting = tokio::spawn(async move { owner.acquire_inference(&access, request()).await });
    queued(&ops).await;
    ops.register_accounts(&[(Uuid::new_v4(), "new account".into())])
        .unwrap();
    limit(&ops, id, 5);
    assert_eq!(ops.snapshot()["globalLimit"], 1);
    assert!(!waiting.is_finished());
    ops.set_global_auto(revision(&ops)).unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ops.snapshot()["globalLimit"], 10);
    assert_eq!(ops.snapshot()["manualGlobalLimit"], 1);
    drop((first, second));
}

#[test]
fn automatic_capacity_tracks_edits_deletion_restart_and_rejects_legacy_settings() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.snapshot()["globalLimit"], 0);
    // Initial setup must remain possible before any accounts exist.
    ops.begin(None, None, "diagnostic", "setup", true)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into())])
        .unwrap();
    assert_eq!(ops.snapshot()["globalLimit"], 10);
    limit(&ops, a, 256);
    assert_eq!(ops.snapshot()["globalLimit"], 261);
    ops.set_global(revision(&ops), 7).unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.snapshot()["globalLimitAuto"], false);
    assert_eq!(ops.snapshot()["globalLimit"], 7);
    ops.set_global_auto(revision(&ops)).unwrap();
    ops.begin_account_delete(b, revision(&ops)).unwrap();
    assert_eq!(ops.snapshot()["globalLimit"], 256);
    ops.finish_account_delete(b).unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.snapshot()["globalLimitAuto"], true);
    assert_eq!(ops.snapshot()["globalLimit"], 256);
    assert_eq!(ops.snapshot()["manualGlobalLimit"], 7);
    assert!(ops.set_global(0, 9).is_err());
    assert!(ops.set_global_auto(0).is_err());
    assert!(ops.set_global(revision(&ops), 0).is_err());
    assert!(ops.set_global(revision(&ops), 257).is_err());
    drop(ops);
    // Missing capacity mode is rejected instead of inferring it for old data.
    let db = rusqlite::Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let document: String = db
        .query_row("SELECT document FROM settings WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&document).unwrap();
    document.as_object_mut().unwrap().remove("globalLimitAuto");
    document["accounts"][0]["maxConcurrency"] = 1.into();
    db.execute(
        "UPDATE settings SET document=?1 WHERE id=1",
        [document.to_string()],
    )
    .unwrap();
    drop(db);
    assert_eq!(
        Operations::open(temp.path()).err().unwrap().code,
        "INVALID_JSON"
    );
}

/// Local scheduling/database load only: no real accounts, upstream calls or billing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "synthetic 100-concurrency / 100k-history performance check"]
async fn hundred_requests_with_large_history_and_concurrent_statistics() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let accounts: Vec<_> = (0..10)
        .map(|n| (Uuid::new_v4(), format!("load-{n}")))
        .collect();
    ops.register_accounts(&accounts).unwrap();
    for (id, _) in &accounts {
        limit(&ops, *id, 10);
        ops.set_inference_route(
            *id,
            revision(&ops),
            Some(Provider {
                execution: Default::default(),
                id: "fixture".into(),
                model: "m".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    }
    let access = key(&ops, "load", 0);
    let mut db = rusqlite::Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    for (index, (account, _)) in accounts.iter().enumerate() {
        tx.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000)
            INSERT INTO requests(id,started_at,finished_at,kind,action,status,account_id,account_name,key_id,key_name,model,input_tokens,output_tokens)
            SELECT printf('history-%d-%d',?1,i),?2+i,?2+i+1000,'inference','responses','succeeded',?3,'load-account',?4,'load-key','m',1000,100 FROM n",
            rusqlite::params![index as i64, codex_relay::now_ms() as i64 - 3_600_000, account.to_string(), access.key_id.to_string()]).unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    let reader = ops.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let statistics = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let mut times = Vec::new();
        for _ in 0..5 {
            let started = std::time::Instant::now();
            let data = reader.statistics(Default::default()).unwrap();
            assert!(data["summary"]["total"].as_u64().unwrap() >= 100_000);
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times
    });
    started_rx.await.unwrap();
    let started = std::time::Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..100 {
        let ops = ops.clone();
        let access = access.clone();
        tasks.spawn(async move {
            let started = std::time::Instant::now();
            let lease = ops.acquire_inference(&access, request()).await.unwrap();
            (lease, started.elapsed().as_secs_f64() * 1000.0)
        });
    }
    let mut leases = Vec::new();
    let mut latency = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (lease, ms) = result.unwrap();
        leases.push(lease);
        latency.push(ms);
    }
    assert_eq!(ops.snapshot()["active"], 100);
    for (account, _) in &accounts {
        assert_eq!(
            leases
                .iter()
                .filter(|l| l.account_id == Some(*account))
                .count(),
            10
        );
    }
    for lease in leases {
        lease.complete(Ok(())).unwrap();
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let query_ms = statistics.join().unwrap();
    latency.sort_by(f64::total_cmp);
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(
        ops.statistics(Default::default()).unwrap()["summary"]["total"],
        100_100
    );
    println!(
        "local synthetic load: 10 accounts x 10 active, 100000 history rows; admission p95={:.2}ms p99={:.2}ms; admission+settlement={elapsed:.2}ms; statistics_ms={query_ms:?}",
        latency[94], latency[98]
    );
}
