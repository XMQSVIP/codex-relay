use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::DEFAULT_GROUP,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
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
    assert_eq!(response.headers()["cache-control"], "no-store");
    (
        response.status(),
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap(),
    )
}

#[tokio::test]
async fn key_editor_updates_name_group_and_limit_atomically_without_replacing_the_secret() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let token = "a".repeat(64);
    let state = AdminState::new(4317, root.clone(), token.clone())
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (status, created) = call(
        &app,
        "POST",
        "/api/api-keys",
        &token,
        json!({"name":"original","groupId":DEFAULT_GROUP,"models":["model-a"],"maxConcurrency":2}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["key"]["id"].as_str().unwrap();
    let secret = created["secret"].as_str().unwrap();
    let path = format!("/api/api-keys/{id}");
    let ops = state.operations.snapshot();
    let (status, group) = call(
        &app,
        "POST",
        "/api/groups",
        &token,
        json!({"revision":ops["revision"],"name":"new group","enabled":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let group_id = &group["id"];
    let edit = json!({"revision":1,"name":"renamed","groupId":group_id,"maxConcurrency":5});
    assert_eq!(
        call(&app, "POST", &path, secret, edit.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    for invalid in [
        json!({"revision":1,"name":"renamed","groupId":uuid::Uuid::new_v4(),"maxConcurrency":5}),
        json!({"revision":1,"name":"  ","groupId":group_id,"maxConcurrency":5}),
        json!({"revision":1,"name":"renamed","groupId":group_id,"maxConcurrency":257}),
    ] {
        assert_eq!(
            call(&app, "POST", &path, &token, invalid).await.0,
            StatusCode::BAD_REQUEST
        );
        let keys = state.operations.list_api_keys().unwrap();
        assert_eq!(keys[0].name, "original");
        assert_eq!(keys[0].group_id, DEFAULT_GROUP);
        assert_eq!(keys[0].max_concurrency, 2);
        assert_eq!(keys[0].revision, 1);
    }
    let (status, edited) = call(&app, "POST", &path, &token, edit.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["name"], "renamed");
    assert_eq!(edited["groupId"], *group_id);
    assert_eq!(edited["maxConcurrency"], 5);
    assert_eq!(edited["revision"], 2);
    assert_eq!(edited["models"], created["key"]["models"]);
    assert_eq!(edited["createdAt"], created["key"]["createdAt"]);
    assert_eq!(edited["expiresAt"], created["key"]["expiresAt"]);
    assert_eq!(
        call(&app, "POST", &path, &token, edit).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state
            .operations
            .authenticate_api_key(secret)
            .unwrap()
            .group_id
            .to_string(),
        group_id.as_str().unwrap()
    );
    assert_eq!(
        state.operations.list_api_keys().unwrap()[0]
            .secret
            .as_deref(),
        Some(secret)
    );
    drop(app);
    drop(state);
    let state = AdminState::new(4317, root, token.clone()).await.unwrap();
    let keys = state.operations.list_api_keys().unwrap();
    assert_eq!(keys[0].name, "renamed");
    assert_eq!(keys[0].max_concurrency, 5);
    assert_eq!(keys[0].secret.as_deref(), Some(secret));
    let app = router(state.clone(), temp.path().join("web"));
    assert_eq!(
        call(&app, "DELETE", &path, &token, json!({"revision":2}))
            .await
            .0,
        StatusCode::OK
    );
    assert!(state.operations.authenticate_api_key(secret).is_err());
    assert!(state.operations.list_api_keys().unwrap().is_empty());
}
#[tokio::test]
async fn desktop_debug_route_does_not_advertise_client_models_or_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (_, created) = call(
        &app,
        "POST",
        "/api/api-keys",
        &admin,
        json!({"name":"client","groupId":DEFAULT_GROUP}),
    )
    .await;
    let key = created["secret"].as_str().unwrap();
    let provider = codex_relay::provider::Provider {
        execution: Default::default(),
        id: "lan_test".into(),
        model: "test-model".into(),
        base_url: "http://192.168.1.10/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    state
        .operations
        .sync_desktop_route(Some(provider.clone()))
        .unwrap();
    let (_, catalog) = call(&app, "GET", "/v1/models", key, Value::Null).await;
    assert_eq!(catalog["data"], json!([]));
    for path in ["/healthz", "/api/api-keys", "/api/overview"] {
        let (status, body) = call(&app, "GET", path, &admin, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["inferenceEnabled"], false, "{path}");
        if path == "/api/overview" {
            assert_eq!(body["accounts"], json!([]));
        }
    }
    assert_eq!(
        state
            .operations
            .native_inference_route(DEFAULT_GROUP, "test-model")
            .unwrap_err()
            .code,
        "INFERENCE_DISABLED"
    );

    let lan = state
        .profiles
        .create_native_config("client LAN")
        .await
        .unwrap();
    state
        .operations
        .register_accounts(&[(lan.id, lan.name)])
        .unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    state
        .operations
        .set_inference_route(lan.id, revision, Some(provider))
        .unwrap();
    let (_, catalog) = call(&app, "GET", "/v1/models", key, Value::Null).await;
    assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["data"][0]["id"], "test-model");
    for path in ["/healthz", "/api/api-keys", "/api/overview"] {
        let (status, body) = call(&app, "GET", path, &admin, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["inferenceEnabled"], true, "{path}");
        if path == "/api/overview" {
            assert_eq!(body["accounts"].as_array().unwrap().len(), 1);
            assert_eq!(body["accounts"][0]["id"], lan.id.to_string());
        }
    }
    assert_eq!(
        state
            .operations
            .native_inference_route(DEFAULT_GROUP, "test-model")
            .unwrap()
            .0,
        lan.id
    );
}

#[tokio::test]
async fn omitted_scope_and_expiry_create_a_permanent_group_scoped_key() {
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (status, created) = call(
        &app,
        "POST",
        "/api/api-keys",
        &admin,
        json!({"name":"optional","groupId":DEFAULT_GROUP}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["key"]["expiresAt"], Value::Null);
    assert_eq!(created["key"]["models"], json!([]));
    let key = created["secret"].as_str().unwrap();
    let account = uuid::Uuid::new_v4();
    state
        .operations
        .register_accounts(&[(account, "test".into())])
        .unwrap();
    let rev = || state.operations.snapshot()["revision"].as_u64().unwrap();
    state
        .operations
        .set_inference_route(
            account,
            rev(),
            Some(codex_relay::provider::Provider {
                execution: Default::default(),
                id: "lan_test".into(),
                model: "new-model".into(),
                base_url: "http://192.168.1.10/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let (_, catalog) = call(&app, "GET", "/v1/models", key, Value::Null).await;
    assert_eq!(catalog["data"][0]["id"], "new-model");
    let other = state
        .operations
        .save_group(
            None,
            codex_relay::operations::GroupInput {
                revision: rev(),
                name: "other".into(),
                enabled: true,
            },
        )
        .unwrap();
    state
        .operations
        .set_account(
            account,
            codex_relay::operations::AccountInput {
                revision: rev(),
                group_id: other,
                enabled: true,
                max_concurrency: 1,
            },
        )
        .unwrap();
    let (_, catalog) = call(&app, "GET", "/v1/models", key, Value::Null).await;
    assert_eq!(catalog["data"], json!([]));
    assert!(state.profiles.list().await.is_empty());
}

#[tokio::test]
async fn management_and_client_keys_are_separate_and_enforce_scopes_before_gate() {
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let payload =
        json!({"name":"验收客户端","groupId":DEFAULT_GROUP,"models":["model-a"],"expiresInDays":7});
    assert_eq!(
        call(&app, "POST", "/api/api-keys", "", payload.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, created) = call(&app, "POST", "/api/api-keys", &admin, payload).await;
    assert_eq!(status, StatusCode::CREATED);
    let key = created["secret"].as_str().unwrap();
    assert_eq!(
        call(&app, "GET", "/api/api-keys", key, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/v1/models", &admin, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, models) = call(&app, "GET", "/v1/models", key, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models, json!({"object":"list","data":[]}));
    let (_, list) = call(&app, "GET", "/api/api-keys", &admin, Value::Null).await;
    assert_eq!(list["baseUrl"], "http://127.0.0.1:4317/v1");
    assert_eq!(list["keys"][0]["secret"], key);
    assert_eq!(
        call(&app, "GET", "/api/api-keys", key, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    let path = format!("/api/api-keys/{}", created["key"]["id"].as_str().unwrap());
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            &admin,
            json!({"revision":1,"state":"revoked"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/v1/models", key, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.profiles.list().await.is_empty());
}
#[tokio::test]
async fn duplicate_headers_query_tokens_and_foreign_origin_never_authenticate() {
    let temp = tempfile::tempdir().unwrap();
    let token = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let created = state
        .operations
        .create_api_key(codex_relay::operations::ApiKeyCreate {
            fast_mode: false,
            name: "test".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["model-a".into()],
            expires_in_days: Some(1),
            max_concurrency: 0,
        })
        .unwrap();
    let app = router(state, temp.path().join("web"));
    // These Host/Origin checks exercise an explicitly local-only deployment.
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/settings/access",
            &token,
            json!({"passwordEnabled":false,"allowPublicAccess":false}),
        )
        .await
        .0,
        StatusCode::OK,
    );
    for mode in ["duplicate", "query", "origin", "host"] {
        let mut request = Request::builder()
            .uri(if mode == "query" {
                format!("/v1/models?api_key={}", created.secret)
            } else {
                "/v1/models".into()
            })
            .header(
                "host",
                if mode == "host" {
                    "evil.invalid"
                } else {
                    "127.0.0.1:4317"
                },
            );
        if mode != "query" {
            request = request.header("authorization", format!("Bearer {}", created.secret));
        }
        if mode == "duplicate" {
            request = request.header("authorization", format!("Bearer {}", created.secret));
        }
        if mode == "origin" {
            request = request.header("origin", "https://evil.invalid");
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if ["host", "origin"].contains(&mode) {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            },
            "authentication boundary for {mode}",
        );
        if mode == "query" {
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
        }
    }
}

#[tokio::test]
async fn catalog_intersects_live_routes_with_key_scope() {
    use codex_relay::operations::{AccountInput, ApiKeyCreate};
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    state
        .operations
        .register_accounts(&[(id, "catalog test".into())])
        .unwrap();
    state
        .operations
        .set_inference_route(
            id,
            state.operations.snapshot()["revision"].as_u64().unwrap(),
            Some(codex_relay::provider::Provider {
                execution: Default::default(),
                id: "lan_test".into(),
                model: "model-a".into(),
                base_url: "http://192.168.1.10/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "catalog".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["model-a".into()],
            expires_in_days: Some(1),
            max_concurrency: 0,
        })
        .unwrap();
    let other = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "other".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["model-b".into()],
            expires_in_days: Some(1),
            max_concurrency: 0,
        })
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (status, list) = call(&app, "GET", "/v1/models", &key.secret, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"][0]["id"], "model-a");
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(&app, "GET", "/v1/models", &other.secret, Value::Null)
            .await
            .1["data"][0]["id"],
        json!("model-b")
    );
    state
        .operations
        .set_account(
            id,
            AccountInput {
                revision: state.operations.snapshot()["revision"].as_u64().unwrap(),
                group_id: DEFAULT_GROUP,
                enabled: false,
                max_concurrency: 1,
            },
        )
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/v1/models", &key.secret, Value::Null)
            .await
            .1["data"],
        json!([])
    );
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert_eq!(state.operations.snapshot()["queued"], 0);
}

#[tokio::test]
async fn key_concurrency_endpoint_requires_admin_and_rejects_stale_edits() {
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    let (status, created) = call(&app,"POST","/api/api-keys",&admin,json!({"name":"concurrency","groupId":DEFAULT_GROUP,"models":["a"],"expiresInDays":1,"maxConcurrency":2})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["key"]["maxConcurrency"], 2);
    let path = format!(
        "/api/api-keys/{}/concurrency",
        created["key"]["id"].as_str().unwrap()
    );
    let input = json!({"revision":1,"maxConcurrency":1});
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            created["secret"].as_str().unwrap(),
            input.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, changed) = call(&app, "POST", &path, &admin, input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["maxConcurrency"], 1);
    assert_eq!(changed["revision"], 2);
    assert_eq!(
        call(&app, "POST", &path, &admin, input).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            &admin,
            json!({"revision":2,"maxConcurrency":257})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, list) = call(&app, "GET", "/api/api-keys", &admin, Value::Null).await;
    assert_eq!(list["keys"][0]["maxConcurrency"], 1);
    assert!(list["keys"][0].get("requestsPerMinute").is_none());
}

#[tokio::test]
async fn key_group_and_delete_endpoints_require_admin_and_preserve_revision_checks() {
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let target = state.operations.save_group(None, serde_json::from_value(json!({"revision":state.operations.snapshot()["revision"],"name":"new","enabled":true})).unwrap()).unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let (_, created) = call(
        &app,
        "POST",
        "/api/api-keys",
        &admin,
        json!({"name":"move","groupId":DEFAULT_GROUP}),
    )
    .await;
    let key = created["secret"].as_str().unwrap();
    let path = format!("/api/api-keys/{}", created["key"]["id"].as_str().unwrap());
    let group_path = format!("{path}/group");
    let input = json!({"revision":1,"groupId":target});
    assert_eq!(
        call(&app, "POST", &group_path, key, input.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &path, key, json!({"revision":1}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, moved) = call(&app, "POST", &group_path, &admin, input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["groupId"], target.to_string());
    assert_eq!(
        call(&app, "POST", &group_path, &admin, input).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "DELETE", &path, &admin, json!({"revision":1}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "DELETE", &path, &admin, json!({"revision":2}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/v1/models", key, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/api/api-keys", &admin, Value::Null)
            .await
            .1["keys"],
        json!([])
    );
}
