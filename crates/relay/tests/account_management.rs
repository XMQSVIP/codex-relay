use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{
    admin::{AdminState, router},
    operations::{AccountInput, DEFAULT_GROUP},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn setup() -> (tempfile::TempDir, Arc<AdminState>, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state.clone(), dir.path().join("web"));
    (dir, state, app)
}
fn rev(state: &AdminState) -> u64 {
    state.operations.snapshot()["revision"].as_u64().unwrap()
}
async fn call(app: &axum::Router, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("host", "127.0.0.1:4317")
        .header("origin", "http://127.0.0.1:4317")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 5 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())),
    )
}
async fn account(state: &AdminState, name: &str) -> Uuid {
    let p = state.profiles.create(name).await.unwrap();
    state
        .operations
        .register_accounts(&[(p.id, p.name)])
        .unwrap();
    p.id
}
fn credential() -> Value {
    let claims = json!({"exp":4102444800u64,"sub":"fixture-user","https://api.openai.com/auth":{"chatgpt_account_id":"fixture-workspace","chatgpt_user_id":"fixture-user"}});
    json!({"access_token":format!("e30.{}.signature",URL_SAFE_NO_PAD.encode(claims.to_string()))})
}

#[tokio::test]
async fn account_multi_group_policy_batch_and_export() {
    let (_dir, state, app) = setup().await;
    let id = account(&state, "shared").await;
    let (_, group) = call(
        &app,
        "/api/groups",
        Some(json!({"revision":rev(&state),"name":"second","enabled":true})),
    )
    .await;
    let groups = json!([DEFAULT_GROUP, group["id"]]);
    let path = format!("/api/accounts/{id}/policy");
    let (status, saved) = call(
        &app,
        &path,
        Some(json!({"revision":rev(&state),"groupIds":groups,"enabled":true,"maxConcurrency":3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["accounts"][0]["groupIds"], groups);
    for (body, expected) in [
        (
            json!({"revision":rev(&state),"groupIds":[],"enabled":true,"maxConcurrency":3}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"revision":rev(&state),"groupIds":groups,"groupId":DEFAULT_GROUP,"enabled":true,"maxConcurrency":3}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_eq!(call(&app, &path, Some(body)).await.0, expected);
    }
    let (_, result) = call(&app, "/api/accounts/batch", Some(json!({"revision":rev(&state),"action":"configure","accountIds":[id],"maxConcurrency":4}))).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(
        state.operations.snapshot()["accounts"][0]["groupIds"],
        groups
    );
    let (status, exported) = call(
        &app,
        "/api/accounts/export",
        Some(json!({"accountIds":[id]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exported["items"][0]["groupIds"], groups);
    assert!(exported["items"][0].get("groupName").is_none());
    assert_eq!(
        exported["items"][0]["groupNames"].as_array().unwrap().len(),
        2
    );
    let (_, imported) = call(&app, "/api/accounts/batch", Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"imported","groupIds":groups}]}))).await;
    assert_eq!(imported["status"], "completed", "{imported}");
    assert_eq!(
        state.operations.snapshot()["accounts"][1]["groupIds"],
        groups
    );
    let (_, result) = call(&app, "/api/accounts/batch", Some(json!({"revision":rev(&state),"action":"configure","accountIds":[id],"groupIds":[group["id"]]}))).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(
        state.operations.snapshot()["accounts"][0]["groupIds"],
        json!([group["id"]])
    );
    let (status, legacy) = call(&app, &path, Some(json!({"revision":rev(&state),"groupId":DEFAULT_GROUP,"enabled":true,"maxConcurrency":4}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{legacy}");
    assert_eq!(
        state.operations.snapshot()["accounts"][0]["groupIds"],
        json!([group["id"]])
    );
    for body in [
        json!({"revision":rev(&state),"action":"configure","accountIds":[id],"groupId":DEFAULT_GROUP}),
        json!({"revision":rev(&state),"action":"import","items":[{"name":"old","groupId":DEFAULT_GROUP}]}),
    ] {
        let before = state.operations.snapshot();
        assert_eq!(
            call(&app, "/api/accounts/batch", Some(body)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(state.operations.snapshot(), before);
    }
}

#[tokio::test]
async fn concurrency_api_switches_modes_and_import_preserves_explicit_capacity() {
    let (_dir, state, app) = setup().await;
    for name in ["first", "second"] {
        let (status, _) = call(&app, "/api/accounts", Some(json!({"name":name}))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    assert_eq!(state.operations.snapshot()["globalLimit"], 10);
    let endpoint = "/api/settings/concurrency";
    let before = state.operations.snapshot();
    assert_eq!(
        call(
            &app,
            endpoint,
            Some(json!({"revision":rev(&state),"globalLimit":3}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(state.operations.snapshot(), before);
    let (status, manual) = call(
        &app,
        endpoint,
        Some(json!({"revision":rev(&state),"globalLimitAuto":false,"globalLimit":3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(manual["globalLimitAuto"], false);
    assert_eq!(manual["globalLimit"], 3);
    let (status, task) = call(&app, "/api/accounts/batch", Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"default"},{"name":"custom","maxConcurrency":2}]}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        task["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["status"] == "succeeded"),
        "{task}"
    );
    let snapshot = state.operations.snapshot();
    assert_eq!(snapshot["accounts"][2]["maxConcurrency"], 5);
    assert_eq!(snapshot["accounts"][3]["maxConcurrency"], 2);
    assert_eq!(snapshot["accountCapacity"], 17);
    assert_eq!(snapshot["globalLimit"], 3);
    let stale = rev(&state);
    let (status, automatic) = call(
        &app,
        endpoint,
        Some(json!({"revision":stale,"globalLimitAuto":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(automatic["globalLimitAuto"], true);
    assert_eq!(automatic["globalLimit"], 17);
    assert_eq!(automatic["manualGlobalLimit"], 3);
    assert_eq!(
        call(
            &app,
            endpoint,
            Some(json!({"revision":stale,"globalLimitAuto":false,"globalLimit":2}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for body in [
        json!({"revision":rev(&state),"globalLimitAuto":false}),
        json!({"revision":rev(&state),"globalLimitAuto":true,"globalLimit":10}),
        json!({"revision":rev(&state),"globalLimitAuto":false,"globalLimit":0}),
        json!({"revision":rev(&state),"globalLimitAuto":false,"globalLimit":257}),
    ] {
        assert_eq!(
            call(&app, endpoint, Some(body)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(state.operations.snapshot()["globalLimit"], 17);
}
#[tokio::test]
async fn batch_configures_only_changed_fields_and_rejects_stale_revision() {
    let (_dir, state, app) = setup().await;
    let id = account(&state, "first").await;
    state
        .operations
        .set_account(
            id,
            AccountInput {
                revision: rev(&state),
                group_id: DEFAULT_GROUP,
                enabled: false,
                max_concurrency: 2,
            },
        )
        .unwrap();
    let (status,task)=call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"configure","accountIds":[id],"maxConcurrency":8}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(task["status"], "completed");
    let p = &state.operations.snapshot()["accounts"][0];
    assert_eq!(p["enabled"], false);
    assert_eq!(p["maxConcurrency"], 8);
    assert_eq!(
        call(
            &app,
            "/api/accounts/batch",
            Some(json!({"revision":0,"action":"delete","accountIds":[id]}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn batch_delete_skips_busy_account_and_keeps_other_results() {
    let (_dir, state, app) = setup().await;
    let a = account(&state, "busy").await;
    let b = account(&state, "idle").await;
    let lease = state
        .operations
        .begin(Some(a), None, "management", "fixture", true)
        .unwrap();
    let (_, task) = call(
        &app,
        "/api/accounts/batch",
        Some(json!({"revision":rev(&state),"action":"delete","accountIds":[a,b]})),
    )
    .await;
    let result = task;
    assert_eq!(result["items"][0]["code"], "BUSY");
    assert_eq!(result["items"][1]["status"], "succeeded");
    assert_eq!(state.profiles.metadata().await.len(), 1);
    drop(lease);
}
#[tokio::test]
async fn credential_import_export_is_roundtrippable_and_results_contain_no_secret() {
    let (dir, state, app) = setup().await;
    let document = credential();
    let (_,task)=call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"first","document":document}]}))).await;
    let result = task;
    assert_eq!(result["status"], "completed", "{result}");
    let id = &result["items"][0]["accountId"];
    assert_eq!(state.operations.snapshot()["accounts"][0]["enabled"], false);
    let (status, export) = call(
        &app,
        "/api/accounts/export",
        Some(json!({"accountIds":[id]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        export["items"][0]["document"]["tokens"]["access_token"],
        document["access_token"]
    );
    let (_,task)=call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"duplicate","document":export["items"][0]["document"]}]}))).await;
    assert_eq!(task["items"][0]["code"], "DUPLICATE_ACCOUNT");
    let db = rusqlite::Connection::open(dir.path().join("accounts/operations/operations.sqlite3"))
        .unwrap();
    let tables: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='account_tasks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
    assert!(!result.to_string().contains("access_token"));
    let (_, state2, app2) = setup().await;
    let (_,task)=call(&app2,"/api/accounts/batch",Some(json!({"revision":rev(&state2),"action":"import","items":[{"name":"restored","document":export["items"][0]["document"]}]}))).await;
    assert_eq!(task["status"], "completed");
}
#[tokio::test]
async fn api_backup_import_rejects_invalid_secrets_and_endpoints_without_creating_accounts() {
    let (_dir, state, app) = setup().await;
    for upstream in [
        json!({"baseUrl":"https://example.test/v1","transport":"http-sse"}),
        json!({"baseUrl":"https://example.test/v1","transport":"http-sse","apiKey":""}),
        json!({"baseUrl":"https://example.test/v1","transport":"http-sse","apiKey":"must-not-import\n"}),
        json!({"baseUrl":"https://user:secret@example.test/v1","transport":"http-sse","apiKey":"must-not-import"}),
        json!({"baseUrl":"file:///private","transport":"http-sse","apiKey":"must-not-import"}),
    ] {
        let (status, value) = call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"restored","source":"api-key","upstream":upstream,"groupIds":[DEFAULT_GROUP],"maxConcurrency":3}]}))).await;
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ) || value["items"][0]["status"] == "failed",
            "{status} {value}"
        );
        assert!(state.profiles.metadata().await.is_empty());
        assert!(!value.to_string().contains("must-not-import"));
    }
}

#[tokio::test]
async fn config_only_export_never_reads_personal_credentials_and_metrics_is_read_only() {
    let (_dir, state, app) = setup().await;
    let p = state.profiles.create_native_config("local").await.unwrap();
    state
        .operations
        .register_accounts(&[(p.id, p.name)])
        .unwrap();
    let (_, export) = call(
        &app,
        "/api/accounts/export",
        Some(json!({"accountIds":[p.id]})),
    )
    .await;
    assert_eq!(export["items"][0]["requiresReconnect"], true);
    assert!(export["items"][0]["document"].is_null());
    let before = rev(&state);
    assert_eq!(
        call(&app, "/api/accounts/metrics?from=0&to=1000", None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, &format!("/api/accounts/{}/quota", p.id), None)
            .await
            .1["status"],
        "unknown"
    );
    assert_eq!(rev(&state), before);
}

#[tokio::test]
#[ignore = "requires the local native Codex installation and signed-in account; reads quota only"]
async fn native_quota_read_does_not_send_a_model_request() {
    std::env::set_current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap();
    let (_dir, state, app) = setup().await;
    let p = state
        .profiles
        .create_native_config("quota smoke")
        .await
        .unwrap();
    state
        .operations
        .register_accounts(&[(p.id, p.name)])
        .unwrap();
    let provider = state.profiles.inspect_native_config(p.id).await.unwrap();
    let expected = if provider.auth.starts_with(codex_relay::oauth::AUTH) {
        "supported"
    } else {
        "unsupported"
    };
    state
        .operations
        .set_inference_route(p.id, rev(&state), Some(provider))
        .unwrap();
    let (status, value) = call(
        &app,
        &format!("/api/accounts/{}/quota/refresh", p.id),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "quota adapter returned {}",
        value["error"]["code"]
    );
    assert_eq!(
        value["status"], expected,
        "native quota capability: {}",
        value["status"]
    );
    eprintln!(
        "native quota capability: {} (no model request)",
        value["status"]
    );
    let stats = state.operations.statistics(Default::default()).unwrap();
    assert_eq!(stats["summary"]["inferenceRequests"], 0);
    state.profiles.shutdown().await;
}

#[tokio::test]
async fn batch_body_limit_is_independent_of_small_management_requests() {
    let (_dir, state, app) = setup().await;
    let (_,task)=call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"invalid credential","document":{"padding":"x".repeat(100_000)}}]}))).await;
    assert_eq!(task["status"], "partial");
    assert_eq!(task["items"][0]["status"], "failed");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/accounts/batch")
                .header("host", "127.0.0.1:4317")
                .header("origin", "http://127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(4 * 1024 * 1024 + 1)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn already_managed_refresh_token_is_skipped_before_exchange() {
    let (_dir, state, app) = setup().await;
    let id = account(&state, "managed").await;
    let mut document = credential();
    document["id_token"] = document["access_token"].clone();
    document["refresh_token"] = "fixture-refresh-token-no-network".into();
    state
        .profiles
        .import_credentials(id, document)
        .await
        .unwrap();
    let (_,task)=call(&app,"/api/accounts/batch",Some(json!({"revision":rev(&state),"action":"import","items":[{"name":"duplicate","document":{"refresh_token":"fixture-refresh-token-no-network"}}]}))).await;
    assert_eq!(task["items"][0]["code"], "DUPLICATE_ACCOUNT");
    assert_eq!(state.profiles.metadata().await.len(), 1);
}

#[tokio::test]
async fn task_endpoints_are_removed_and_old_storage_is_rejected() {
    let (dir, state, app) = setup().await;
    account(&state, "preserved").await;
    for (method, path, expected) in [
        (
            "GET",
            "/api/accounts/batch".to_string(),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "POST",
            format!("/api/accounts/batch/{}/stop", Uuid::new_v4()),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:4317")
                    .header("origin", "http://127.0.0.1:4317")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let root = dir.path().join("accounts/operations");
    let db = rusqlite::Connection::open(root.join("operations.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE account_tasks(id TEXT PRIMARY KEY, created_at INTEGER, document TEXT); INSERT INTO account_tasks VALUES('old', 1, '{}');").unwrap();
    drop(app);
    drop(state);
    let error = codex_relay::operations::Operations::open(&root)
        .err()
        .unwrap();
    assert_eq!(error.code, "DATABASE_SCHEMA");
    let tables: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='account_tasks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1);
    assert_eq!(
        db.query_row("SELECT id FROM account_tasks", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "old"
    );
}

#[tokio::test]
async fn reset_rejects_invalid_input_and_unsupported_provider_without_inference() {
    let (_dir, state, app) = setup().await;
    let id = account(&state, "unsupported reset").await;
    state
        .operations
        .set_inference_route(
            id,
            rev(&state),
            Some(codex_relay::provider::Provider {
                execution: Default::default(),
                id: "third-party".into(),
                model: "fixture".into(),
                base_url: "http://127.0.0.1:1".into(),
                auth: "apikey".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let path = format!("/api/accounts/{id}/quota/reset");
    let (status, _) = call(
        &app,
        &path,
        Some(json!({"idempotencyKey":Uuid::nil(),"context":"a".repeat(64)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, value) = call(
        &app,
        &path,
        Some(json!({"idempotencyKey":Uuid::new_v4(),"context":"a".repeat(64)})),
    )
    .await;
    assert_eq!(value["error"]["code"], "RESET_UNSUPPORTED");
    let (_, automation) = call(
        &app,
        &format!("/api/accounts/{id}/quota/reset/automation"),
        None,
    )
    .await;
    assert!(automation["pendingRequest"].is_null());
    let (_, metrics) = call(
        &app,
        &format!("/api/accounts/metrics?from=0&to={}", codex_relay::now_ms()),
        None,
    )
    .await;
    assert!(
        metrics["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["requestCount"] == 0)
    );
    assert_eq!(state.profiles.metadata().await.len(), 1);
}
