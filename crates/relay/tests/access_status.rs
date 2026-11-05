use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, DEFAULT_GROUP},
    provider::Provider,
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn provider() -> Provider {
    Provider {
        execution: Default::default(),
        id: "openai".into(),
        model: "native-default".into(),
        base_url: "https://chatgpt.com/backend-api".into(),
        auth: "native-chatgpt:synthetic".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}
async fn post(app: axum::Router, path: String, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn diagnostics_never_enable_disable_or_rewrite_account_routes() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let account = state.profiles.create("no-credentials").await.unwrap();
    state
        .operations
        .register_accounts(&[(account.id, account.name)])
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    for route in [None, Some(provider())] {
        let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
        state
            .operations
            .set_inference_route(account.id, revision, route.clone())
            .unwrap();
        let before = state.operations.snapshot();
        let (status, _) = post(
            app.clone(),
            format!("/api/accounts/{}/connection-test", account.id),
            json!({"model":"another-model"}),
        )
        .await;
        assert!(
            !status.is_success(),
            "An account without credentials cannot pass a model test"
        );
        assert_ne!(status, StatusCode::NOT_FOUND);
        assert_eq!(state.operations.configured_provider(account.id), route);
        let after = state.operations.snapshot();
        assert_eq!(after["revision"], before["revision"]);
        assert_eq!(
            after["accounts"][0]["health"],
            before["accounts"][0]["health"]
        );
        assert_eq!(after["active"], 0);
    }
}

#[tokio::test]
async fn enabling_has_no_model_parameter_and_test_input_is_validated_early() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let account = state.profiles.create("validation").await.unwrap();
    state
        .operations
        .register_accounts(&[(account.id, account.name)])
        .unwrap();
    let revision = state.operations.snapshot()["revision"].clone();
    let app = router(state.clone(), temp.path().join("web"));
    let (status, _) = post(
        app.clone(),
        format!("/api/accounts/{}/route", account.id),
        json!({"revision":revision,"enabled":true,"model":"must-not-bind"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for model in [String::new(), "bad\nmodel".into(), "a".repeat(129)] {
        let (status, body) = post(
            app.clone(),
            format!("/api/accounts/{}/connection-test", account.id),
            json!({"model":model}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_INPUT");
    }
    assert_eq!(state.operations.snapshot()["revision"], revision);
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.operations.configured_provider(account.id).is_none());
}

#[tokio::test]
async fn unrestricted_client_models_do_not_require_per_model_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let account = state.profiles.create("client-selected").await.unwrap();
    let ops = &state.operations;
    ops.register_accounts(&[(account.id, account.name)])
        .unwrap();
    let revision = ops.snapshot()["revision"].as_u64().unwrap();
    ops.set_inference_route(account.id, revision, Some(provider()))
        .unwrap();
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "unrestricted".into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 1,
        })
        .unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    for model in ["native-default", "client-model-a", "unlisted-model-b"] {
        let (selected, configured) = ops.inference_route(DEFAULT_GROUP, model).unwrap();
        assert_eq!(selected, account.id);
        assert!(
            ops.check_inference_route(selected, &access, &configured, model)
                .is_ok()
        );
        assert_eq!(configured, provider());
    }
    assert_eq!(ops.configured_provider(account.id), Some(provider()));
}
