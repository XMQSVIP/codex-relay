use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::QueueQuery,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: &axum::Router, method: &str, path: &str, token: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn cancel_endpoint_authenticates_and_unblocks_original_http_request() {
    let dir = tempfile::tempdir().unwrap();
    let token = "q".repeat(64);
    let state = AdminState::new(4317, dir.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let profile = state.profiles.create("queue test").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    let app = router(state.clone(), dir.path().join("web"));
    let held = state
        .operations
        .begin(Some(profile.id), None, "native_check", "held", true)
        .unwrap();
    let waiting_app = app.clone();
    let waiting_token = token.clone();
    let waiting = tokio::spawn(async move {
        call(
            &waiting_app,
            "POST",
            &format!("/api/accounts/{}/check", profile.id),
            &waiting_token,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.operations.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (status, queue) = call(&app, "GET", "/api/queue", &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue["total"], 1);
    let id = queue["records"][0]["id"].as_str().unwrap();
    let path = format!("/api/requests/{id}/cancel");
    assert_eq!(
        call(&app, "POST", &path, "wrong").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        state.operations.queue(QueueQuery::default()).unwrap()["total"],
        1
    );
    assert_eq!(call(&app, "POST", &path, &token).await.0, StatusCode::OK);
    let (status, body) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "REQUEST_CANCELLED");
    assert_eq!(
        call(&app, "POST", &path, &token).await.1["alreadyCancelled"],
        true
    );
    assert_eq!(state.profiles.list().await[0].state, "stopped");
    assert_eq!(
        call(&app, "GET", "/api/queue?page=0", &token).await.0,
        StatusCode::BAD_REQUEST
    );
    drop(held);
}

#[tokio::test]
async fn cleanup_succeeds_and_reports_missing_journal_when_storage_is_unhealthy() {
    let dir = tempfile::tempdir().unwrap();
    let token = "m".repeat(64);
    let root = dir.path().join("accounts");
    let state = AdminState::new(4317, root.clone(), token.clone())
        .await
        .unwrap();
    let profile = state.profiles.create("cleanup test").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    let lease = state
        .operations
        .begin(Some(profile.id), None, "management", "test", true)
        .unwrap();
    let db = rusqlite::Connection::open(root.join("operations/operations.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_update BEFORE UPDATE ON requests BEGIN SELECT RAISE(FAIL,'fault'); END;").unwrap();
    assert!(lease.complete(Ok(())).is_err());
    assert_eq!(state.operations.snapshot()["storageHealthy"], false);
    let app = router(state.clone(), dir.path().join("web"));
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/accounts/{}/stop", profile.id),
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["recorded"], false);
    assert!(body["warning"].is_string());
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/accounts/{}/start", profile.id),
            &token
        )
        .await
        .1["error"]["code"],
        "OPERATIONS_STORAGE"
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/accounts/{}/stop", uuid::Uuid::new_v4()),
            &token
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(state.profiles.list().await[0].state, "stopped");
    assert_ne!(body, json!({"ok":false}));
}
