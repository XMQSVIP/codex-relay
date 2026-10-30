use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{
    admin::{AdminState, router},
    operations::{DEFAULT_GROUP, DESKTOP_ACCOUNT, Operations},
    provider::Provider,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn provider() -> Provider {
    Provider {
        execution: Default::default(),
        id: "activation-fixture".into(),
        model: "test-model".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}
async fn setup() -> (tempfile::TempDir, Arc<AdminState>, axum::Router, Uuid) {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let profile = state.profiles.create("activation test").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    state
        .operations
        .set_inference_route(profile.id, revision(&state.operations), Some(provider()))
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    (temp, state, app, profile.id)
}
async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("origin", "http://127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 100_000).await.unwrap()).unwrap();
    (status, body)
}
async fn activate(
    app: &axum::Router,
    state: &AdminState,
    id: Uuid,
    enabled: bool,
) -> (StatusCode, Value) {
    post(
        app,
        &format!("/api/accounts/{id}/activation"),
        json!({"revision":revision(&state.operations),"enabled":enabled}),
    )
    .await
}
fn stops(ops: &Operations) -> usize {
    ops.statistics(Default::default()).unwrap()["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["action"] == "stop_disabled")
        .count()
}

#[tokio::test]
async fn disabling_drains_active_work_rejects_queued_work_and_preserves_configuration() {
    let (temp, state, app, id) = setup().await;
    let auth_path = temp
        .path()
        .join("accounts")
        .join(id.to_string())
        .join("home/auth.json");
    tokio::fs::write(&auth_path, b"synthetic credential sentinel")
        .await
        .unwrap();
    let active = state
        .operations
        .begin(Some(id), None, "inference", "responses", true)
        .unwrap();
    let ops = state.operations.clone();
    let queued = tokio::spawn(async move {
        ops.acquire(Some(id), None, "native_check", "queued")
            .await
            .err()
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.operations.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let (status, result) = activate(&app, &state, id, false).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["operations"]["accounts"][0]["enabled"], false);
    assert_eq!(result["operations"]["accounts"][0]["active"], 1);
    assert_eq!(state.operations.configured_provider(id), Some(provider()));
    assert_eq!(stops(&state.operations), 0);
    assert_eq!(queued.await.unwrap().code, "ROUTE_DISABLED");
    assert_eq!(
        state
            .operations
            .inference_route(DEFAULT_GROUP, "test-model")
            .unwrap_err()
            .code,
        "INFERENCE_DISABLED"
    );
    assert_eq!(state.operations.snapshot()["active"], 1);

    active.complete(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while stops(&state.operations) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(&auth_path).await.unwrap(),
        b"synthetic credential sentinel"
    );
    let (status, result) = activate(&app, &state, id, true).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        state
            .operations
            .inference_route(DEFAULT_GROUP, "test-model")
            .unwrap()
            .0,
        id
    );
    assert_eq!(stops(&state.operations), 1);
    // Activation now schedules a worker; disable it before tearing down this fixture.
    assert_eq!(activate(&app, &state, id, false).await.0, StatusCode::OK);
}

#[tokio::test]
async fn reenabling_before_drain_finishes_cancels_the_pending_stop() {
    let (_temp, state, app, id) = setup().await;
    let active = state
        .operations
        .begin(Some(id), None, "inference", "responses", true)
        .unwrap();
    assert_eq!(activate(&app, &state, id, false).await.0, StatusCode::OK);
    assert_eq!(activate(&app, &state, id, true).await.0, StatusCode::OK);
    active.complete(Ok(())).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(stops(&state.operations), 0);
    assert_eq!(state.operations.snapshot()["accounts"][0]["enabled"], true);
    assert!(
        state
            .operations
            .inference_route(DEFAULT_GROUP, "test-model")
            .is_ok()
    );
    assert_eq!(activate(&app, &state, id, false).await.0, StatusCode::OK);
}

#[tokio::test]
async fn repeated_activation_changes_leave_only_the_latest_stop_pending() {
    let (_temp, state, app, id) = setup().await;
    let active = state
        .operations
        .begin(Some(id), None, "inference", "responses", true)
        .unwrap();
    for enabled in [false, true, false, false] {
        assert_eq!(activate(&app, &state, id, enabled).await.0, StatusCode::OK);
    }
    assert_eq!(stops(&state.operations), 0);
    active.complete(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while stops(&state.operations) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(stops(&state.operations), 1);
}

#[tokio::test]
async fn activation_checks_revisions_and_disabled_state_survives_restart() {
    let (temp, state, app, id) = setup().await;
    let version = revision(&state.operations);
    let (status, result) = post(
        &app,
        &format!("/api/accounts/{id}/activation"),
        json!({"revision":version-1,"enabled":false}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert_eq!(result["error"]["code"], "CONFIG_CONFLICT");
    assert_eq!(state.operations.snapshot()["accounts"][0]["enabled"], true);
    assert_eq!(stops(&state.operations), 0);
    assert_eq!(activate(&app, &state, id, false).await.0, StatusCode::OK);
    drop(app);
    drop(state);
    let reopened = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    assert_eq!(
        reopened.operations.snapshot()["accounts"][0]["enabled"],
        false
    );
    assert_eq!(
        reopened.operations.configured_provider(id),
        Some(provider())
    );
    assert!(reopened.operations.inference_models(None).is_empty());
    assert!(reopened.profiles.list().await[0].pid.is_none());
}

#[tokio::test]
async fn disabled_accounts_can_import_credentials_without_enabling_the_service() {
    let (_temp, state, app, id) = setup().await;
    assert_eq!(activate(&app, &state, id, false).await.0, StatusCode::OK);
    let claims = json!({"sub":"synthetic-user","exp":codex_relay::now_ms()/1000+3600,
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account","chatgpt_user_id":"synthetic-user"}});
    let token = format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    let (status, result) = post(
        &app,
        &format!("/api/accounts/{id}/credentials"),
        json!({"accessToken":token,"accountId":"synthetic-account"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(state.operations.snapshot()["accounts"][0]["enabled"], false);
    assert!(state.operations.configured_provider(id).is_none());
    assert_eq!(
        state.profiles.list().await[0].credential_source.as_deref(),
        Some("external")
    );
}

#[test]
fn activation_requires_a_route_and_rejects_deleted_or_debug_accounts() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "unconfigured".into())])
        .unwrap();
    assert_eq!(
        ops.set_account_activation(id, revision(&ops), true, None)
            .unwrap_err()
            .code,
        "INFERENCE_DISABLED"
    );
    assert_eq!(
        ops.set_account_activation(DESKTOP_ACCOUNT, revision(&ops), false, None)
            .unwrap_err()
            .code,
        "INVALID_INPUT"
    );
    ops.begin_account_delete(id, revision(&ops)).unwrap();
    assert_eq!(
        ops.set_account_activation(id, revision(&ops), true, Some(provider()))
            .unwrap_err()
            .code,
        "BUSY"
    );
}
