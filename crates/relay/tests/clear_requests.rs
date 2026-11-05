use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{Operations, StatsQuery},
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn query() -> StatsQuery {
    serde_json::from_value(json!({})).unwrap()
}

#[tokio::test]
async fn clearing_requires_admin_and_confirmation_and_removes_all_history_and_usage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let token = "a".repeat(64);
    let state = AdminState::new(4317, root.clone(), token.clone())
        .await
        .unwrap();
    let profile = state.profiles.create("keep account").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (status, key) = call(
        &app,
        &token,
        "POST",
        "/api/api-keys",
        json!({"name":"keep key","groupId":codex_relay::operations::DEFAULT_GROUP}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let settings = state.operations.snapshot();
    let db = rusqlite::Connection::open(root.join("operations/operations.sqlite3")).unwrap();
    let now = codex_relay::now_ms() as i64;
    for (id, when, group, status) in [
        ("current", now, "one", "succeeded"),
        ("other", now - 7_200_000, "two", "failed"),
        ("old", now - 31 * 86_400_000, "three", "cancelled"),
    ] {
        db.execute("INSERT INTO requests(id,started_at,finished_at,kind,action,group_id,status,input_tokens,output_tokens,key_id) VALUES(?1,?2,?2,'inference','responses',?3,?4,21,8,?5)", rusqlite::params![id,when,group,status,key["key"]["id"].as_str().unwrap()]).unwrap();
    }
    for credential in ["wrong", key["secret"].as_str().unwrap()] {
        assert_eq!(
            call(
                &app,
                credential,
                "DELETE",
                "/api/requests",
                json!({"confirmed":true})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &app,
            &token,
            "DELETE",
            "/api/requests",
            json!({"confirmed":false})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    let (status, cleared) = call(
        &app,
        &token,
        "DELETE",
        "/api/requests",
        json!({"confirmed":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cleared["deleted"], 3,
        "Includes other groups and records outside the displayed time range"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let stats = state.operations.statistics(query()).unwrap();
    assert_eq!(stats["summary"]["total"], 0);
    assert_eq!(stats["summary"]["usageKnownRequests"], 0);
    assert!(stats["summary"]["inputTokens"].is_null());
    assert!(stats["summary"]["outputTokens"].is_null());
    assert!(stats["records"].as_array().unwrap().is_empty());
    assert!(
        stats["trend"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["count"] == 0)
    );
    assert_eq!(state.operations.snapshot(), settings);
    assert_eq!(state.profiles.list().await.len(), 1);
    state
        .operations
        .authenticate_api_key(key["secret"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        call(
            &app,
            &token,
            "DELETE",
            "/api/requests",
            json!({"confirmed":true})
        )
        .await
        .1["deleted"],
        0
    );
    state
        .operations
        .begin(None, None, "diagnostic", "new", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(
        state.operations.statistics(query()).unwrap()["summary"]["total"],
        1
    );
}

#[tokio::test]
async fn clearing_does_not_remove_running_or_queued_journals_or_cancel_work() {
    let temp = tempfile::tempdir().unwrap();
    let token = "b".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    state
        .operations
        .set_global(state.operations.snapshot()["revision"].as_u64().unwrap(), 1)
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let held = state
        .operations
        .begin(None, None, "diagnostic", "held", true)
        .unwrap();
    let ops = state.operations.clone();
    let waiting =
        tokio::spawn(async move { ops.acquire(None, None, "diagnostic", "waiting").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.operations.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (status, body) = call(
        &app,
        &token,
        "DELETE",
        "/api/requests",
        json!({"confirmed":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "BUSY");
    let stats = state.operations.statistics(query()).unwrap();
    assert_eq!(stats["summary"]["running"], 1);
    assert_eq!(stats["summary"]["queued"], 1);
    held.complete(Ok(())).unwrap();
    let admitted = waiting.await.unwrap().unwrap();
    assert_eq!(
        state.operations.clear_request_history().unwrap_err().code,
        "BUSY"
    );
    admitted.complete(Ok(())).unwrap();
    assert_eq!(state.operations.clear_request_history().unwrap(), 2);
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert_eq!(state.operations.snapshot()["queued"], 0);
}

#[test]
fn failed_clear_rolls_back_the_entire_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO requests(id,started_at,kind,action,status) VALUES('first',1,'diagnostic','test','succeeded'),('second',2,'diagnostic','test','failed'); CREATE TRIGGER fail_clear BEFORE DELETE ON requests WHEN OLD.id='second' BEGIN SELECT RAISE(FAIL,'synthetic fault'); END;").unwrap();
    assert_eq!(
        ops.clear_request_history().unwrap_err().code,
        "OPERATIONS_STORAGE"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    db.execute_batch("DROP TRIGGER fail_clear").unwrap();
    assert_eq!(ops.clear_request_history().unwrap(), 2);
    drop(ops);
    let reopened = Operations::open(temp.path()).unwrap();
    assert_eq!(reopened.statistics(query()).unwrap()["summary"]["total"], 0);
}
