use codex_relay::operations::{Operations, StatsQuery};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn desktop_diagnostics_are_hidden_before_history_pagination_and_aggregation() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let started_at = codex_relay::now_ms() as i64 - 1000;
    for action in [
        "doctor",
        "shared",
        "guard",
        "stream",
        "provider",
        "inspect-provider",
        "prepare",
    ] {
        for status in [
            "queued",
            "running",
            "succeeded",
            "failed",
            "rejected",
            "cancelled",
            "interrupted",
            "timed_out",
        ] {
            db.execute(
                "INSERT INTO requests(id,started_at,kind,action,status) VALUES(?1,?2,'diagnostic',?3,?4)",
                params![format!("{action}-{status}"), started_at, action, status],
            )
            .unwrap();
        }
    }
    for i in 0..21 {
        db.execute(
            "INSERT INTO requests(id,started_at,kind,action,status,input_tokens,output_tokens) VALUES(?1,?2,'inference','responses','succeeded',10,5)",
            params![format!("client-{i:02}"), started_at],
        )
        .unwrap();
    }
    let account = Uuid::new_v4();
    ops.register_accounts(&[(account, "account".into())])
        .unwrap();
    for (kind, action) in [
        ("diagnostic", "connection_test"),
        ("management", "start"),
        ("native_check", "account_check"),
    ] {
        ops.begin(Some(account), None, kind, action, true)
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
    // New jobs and previously stored desktop diagnostics follow the same rule.
    let job = ops.begin(None, None, "diagnostic", "doctor", true).unwrap();
    assert_eq!(ops.snapshot()["active"], 1);
    assert_eq!(
        ops.statistics(StatsQuery::default()).unwrap()["summary"]["total"],
        24
    );
    job.complete(Ok(())).unwrap();
    drop(db);
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let stats = |query: Value| {
        ops.statistics(serde_json::from_value::<StatsQuery>(query).unwrap())
            .unwrap()
    };
    let first = stats(json!({}));
    assert_eq!(first["summary"]["total"], 24);
    assert_eq!(first["summary"]["inferenceRequests"], 21);
    assert_eq!(first["summary"]["inputTokens"], 210);
    assert_eq!(first["summary"]["outputTokens"], 105);
    assert_eq!(first["records"].as_array().unwrap().len(), 20);
    assert_eq!(
        first["trend"]
            .as_array()
            .unwrap()
            .iter()
            .map(|bucket| bucket["count"].as_u64().unwrap())
            .sum::<u64>(),
        24
    );
    let second = stats(json!({"page": 2}));
    assert_eq!(second["page"], 2);
    assert_eq!(second["records"].as_array().unwrap().len(), 4);
    for record in first["records"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["records"].as_array().unwrap())
    {
        assert!(record["kind"] != "diagnostic" || record["accountId"] == account.to_string());
    }
    let diagnostic = stats(json!({"kind": "diagnostic"}));
    assert_eq!(diagnostic["summary"]["total"], 1);
    assert_eq!(diagnostic["records"][0]["action"], "connection_test");
    let failed = stats(json!({"status": "failed"}));
    assert_eq!(failed["summary"]["total"], 0);
    assert!(failed["records"].as_array().unwrap().is_empty());
}
