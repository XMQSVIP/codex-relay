use codex_relay::{
    error::Error,
    operations::{AccountInput, DEFAULT_GROUP, GroupInput, Operations, StatsQuery},
};
use serde_json::{Value, json};
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}

#[test]
fn native_affinity_setting_validates_revisions_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(ops.snapshot()["nativeAffinityTtlHours"], 24);
    let original = revision(&ops);
    for minutes in [0, 721, u32::MAX] {
        assert_eq!(
            ops.set_native_affinity_ttl(original, minutes)
                .unwrap_err()
                .code,
            "INVALID_INPUT"
        );
        assert_eq!(revision(&ops), original);
    }
    ops.set_native_affinity_ttl(original, 30).unwrap();
    assert_eq!(
        ops.set_native_affinity_ttl(original, 720).unwrap_err().code,
        "CONFIG_CONFLICT"
    );
    assert_eq!(ops.snapshot()["nativeAffinityTtlHours"], 30);
    drop(ops);
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(ops.snapshot()["nativeAffinityTtlHours"], 30);
    for minutes in [1, 720, 24] {
        ops.set_native_affinity_ttl(revision(&ops), minutes)
            .unwrap();
        assert_eq!(ops.snapshot()["nativeAffinityTtlHours"], minutes);
    }
}
fn query() -> StatsQuery {
    serde_json::from_value(json!({})).unwrap()
}

#[test]
fn key_name_filter_matches_literal_snapshot_before_pagination() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    for i in 0..25 {
        db.execute(
            "INSERT INTO requests(id,started_at,kind,action,status,key_id,key_name,input_tokens,output_tokens) VALUES (?1,?2,'inference','responses','succeeded',?3,?4,10,2)",
            rusqlite::params![Uuid::new_v4().to_string(), codex_relay::now_ms() as i64, Uuid::new_v4().to_string(), if i < 22 { "历史密钥%_名称" } else { "其他密钥" }],
        ).unwrap();
    }
    let result = ops
        .statistics(serde_json::from_value(json!({"keyName":"%_", "page":2})).unwrap())
        .unwrap();
    assert_eq!(result["summary"]["total"], 22);
    assert_eq!(result["summary"]["inputTokens"], 220);
    assert_eq!(result["records"].as_array().unwrap().len(), 2);
    assert_eq!(result["keyUsage"].as_array().unwrap().len(), 22);
    let empty = ops
        .statistics(serde_json::from_value(json!({"keyName":"不存在"})).unwrap())
        .unwrap();
    assert_eq!(empty["summary"]["total"], 0);
}
fn group(ops: &Operations, name: &str) -> Uuid {
    ops.save_group(
        None,
        GroupInput {
            revision: revision(ops),
            name: name.into(),
            enabled: true,
        },
    )
    .unwrap()
}
fn assign(ops: &Operations, id: Uuid, g: Uuid, limit: u32, enabled: bool) {
    ops.set_account(
        id,
        AccountInput {
            revision: revision(ops),
            group_id: g,
            max_concurrency: limit,
            enabled,
        },
    )
    .unwrap();
}

#[test]
fn atomic_three_level_admission_round_robin_and_cancel_release() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into()), (c, "C".into())])
        .unwrap();
    let g = group(&ops, "pool");
    assign(&ops, a, g, 1, true);
    assign(&ops, b, g, 1, true);
    ops.set_global(revision(&ops), 2).unwrap();
    let first = ops
        .begin(None, Some(g), "native_check", "check", true)
        .unwrap();
    assert_eq!(first.account_id, Some(a));
    let second = ops
        .begin(None, Some(g), "native_check", "check", true)
        .unwrap();
    assert_eq!(second.account_id, Some(b));
    assert!(
        ops.begin(Some(c), None, "native_check", "check", true)
            .is_err()
    );
    assert!(
        ops.begin(Some(a), None, "native_check", "check", true)
            .is_err()
    );
    // Lowering the limit lets existing work drain; it does not kill it or reset counters.
    ops.set_global(revision(&ops), 1).unwrap();
    drop(first);
    assert_eq!(ops.snapshot()["active"], 1);
    assert!(
        ops.begin(None, Some(g), "native_check", "check", true)
            .is_err()
    );
    second.complete(Ok(())).unwrap();
    let third = ops
        .begin(None, Some(g), "native_check", "check", true)
        .unwrap();
    assert_eq!(third.account_id, Some(a));
    drop(third);
    assert_eq!(ops.snapshot()["active"], 0);
    let s = ops.statistics(query()).unwrap();
    assert_eq!(s["summary"]["succeeded"], 1);
    assert_eq!(s["summary"]["cancelled"], 2);
    assert_eq!(s["summary"]["rejected"], 3);
}

#[test]
fn group_pause_and_membership_control_routing() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into())])
        .unwrap();
    let g = group(&ops, "pool");
    assign(&ops, a, g, 2, true);
    assign(&ops, b, g, 2, true);
    let lease = ops
        .begin(Some(a), None, "native_check", "check", true)
        .unwrap();
    ops.begin(Some(b), None, "native_check", "check", true)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert!(
        ops.set_account(
            a,
            AccountInput {
                revision: revision(&ops),
                group_id: DEFAULT_GROUP,
                enabled: true,
                max_concurrency: 1
            }
        )
        .is_err()
    );
    ops.save_group(
        Some(g),
        GroupInput {
            revision: revision(&ops),
            name: "pool".into(),
            enabled: false,
        },
    )
    .unwrap();
    assert!(
        ops.begin(Some(b), None, "native_check", "check", true)
            .is_err()
    );
    ops.begin(Some(a), None, "management", "stop", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(ops.snapshot()["active"], 1);
    drop(lease);
    assert!(ops.delete_group(g, revision(&ops)).is_err());
    assign(&ops, a, DEFAULT_GROUP, 1, true);
    assign(&ops, b, DEFAULT_GROUP, 1, true);
    ops.delete_group(g, revision(&ops)).unwrap();
    assert!(ops.delete_group(DEFAULT_GROUP, revision(&ops)).is_err());
}

#[test]
fn persistence_restart_recovery_revision_filtering_and_single_owner() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    assert!(Operations::open(dir.path()).is_err());
    let a = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into())]).unwrap();
    let rev = revision(&ops);
    ops.set_global(rev, 7).unwrap();
    assert!(ops.set_global(rev, 8).is_err());
    ops.begin(Some(a), None, "native_check", "check", true)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    let _ = ops
        .begin(None, None, "diagnostic", "test", true)
        .unwrap()
        .complete::<()>(Err(Error::new("TEST_FAILED", "secret must not be stored")));
    let s = ops
        .statistics(serde_json::from_value(json!({"kind":"native_check","accountId":a})).unwrap())
        .unwrap();
    assert_eq!(s["summary"]["total"], 1);
    assert!(s["summary"]["inputTokens"].is_null());
    assert!(
        !ops.statistics(query())
            .unwrap()
            .to_string()
            .contains("secret")
    );
    drop(ops);
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute("INSERT INTO requests(id,started_at,kind,action,status) VALUES('crash',?1,'diagnostic','test','running')",[codex_relay::now_ms() as i64]).unwrap();
    db.execute("INSERT INTO requests(id,started_at,kind,action,status) VALUES('queue-crash',?1,'diagnostic','test','queued')",[codex_relay::now_ms() as i64]).unwrap();
    drop(db);
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(ops.snapshot()["globalLimit"], 7);
    assert_eq!(ops.snapshot()["active"], 0);
    let s = ops.statistics(query()).unwrap();
    assert_eq!(s["summary"]["cancelled"], 2);
    assert_eq!(s["summary"]["running"], 0);
    assert_eq!(s["summary"]["queued"], 0);
    assert_eq!(ops.snapshot()["queueLimit"], 64);
    let total: u64 = s["trend"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["count"].as_u64().unwrap())
        .sum();
    assert_eq!(Value::from(total), s["summary"]["total"]);
}

#[test]
fn simultaneous_requests_never_exceed_limit() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 3).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    std::thread::scope(|scope| {
        let mut threads = vec![];
        for _ in 0..12 {
            let ops = ops.clone();
            let barrier = barrier.clone();
            threads.push(scope.spawn(move || {
                let lease = ops.begin(None, None, "diagnostic", "race", true).ok();
                barrier.wait();
                lease
            }));
        }
        let leases = threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(leases.len(), 3);
        assert_eq!(ops.snapshot()["active"], 3);
        drop(leases);
    });
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(ops.statistics(query()).unwrap()["summary"]["rejected"], 9);
}

#[tokio::test]
async fn cancelling_an_admitted_future_releases_capacity_and_records_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 1).unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let owner = ops.clone();
    let task = tokio::spawn(async move {
        let _lease = owner
            .begin(None, None, "diagnostic", "cancel", true)
            .unwrap();
        let _ = started.send(());
        std::future::pending::<()>().await;
    });
    ready.await.unwrap();
    assert_eq!(ops.snapshot()["active"], 1);
    task.abort();
    let _ = task.await;
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(ops.statistics(query()).unwrap()["summary"]["cancelled"], 1);
    for _ in 0..55 {
        ops.begin(None, None, "diagnostic", "page", true)
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
    let page = ops
        .statistics(serde_json::from_value(json!({"page":2,"status":"succeeded"})).unwrap())
        .unwrap();
    assert_eq!(page["records"].as_array().unwrap().len(), 20);
    assert_eq!(page["pageSize"], 20);
    assert_eq!(page["summary"]["total"], 55);
    let first = ops
        .statistics(serde_json::from_value(json!({"status":"succeeded"})).unwrap())
        .unwrap();
    assert_eq!(first["records"].as_array().unwrap().len(), 20);
    assert!(first["records"].as_array().unwrap().iter().all(|a| {
        page["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| a["id"] != b["id"])
    }));
    let larger = ops
        .statistics(
            serde_json::from_value(json!({"page":2,"pageSize":50,"status":"succeeded"})).unwrap(),
        )
        .unwrap();
    assert_eq!(larger["records"].as_array().unwrap().len(), 5);
    let last = ops
        .statistics(serde_json::from_value(json!({"page":999,"status":"succeeded"})).unwrap())
        .unwrap();
    assert_eq!(last["page"], 3);
    assert_eq!(last["records"].as_array().unwrap().len(), 15);
    assert!(
        ops.statistics(serde_json::from_value(json!({"pageSize":0})).unwrap())
            .is_err()
    );
}
