use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    profiles::Profiles,
};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn installation_paths_are_validated_before_diagnostic_admission() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    for body in [
        json!({"kind":"doctor","paths":{"appServerExecutable":"b"}}),
        json!({"kind":"prepare","paths":{"appServerExecutable":"b"}}),
        json!({"kind":"prepare","paths":{"desktopExecutable":"a"}}),
        json!({"kind":"prepare","paths":{"desktopExecutable":"a","appServerExecutable":"b","archive":"c","extra":true}}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/jobs")
                    .header("host", "127.0.0.1:4317")
                    .header("origin", "http://127.0.0.1:4317")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert!(state.jobs.lock().await.is_empty());
        assert_eq!(state.operations.snapshot()["active"], 0);
    }
    // A non-executable file passes path validation but fails the background job,
    // releasing its diagnostic lease without replacing the saved configuration.
    let file = temp.path().join("invalid-installation");
    std::fs::write(&file, [0; 16]).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("host", "127.0.0.1:4317")
                .header("origin", "http://127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"kind":"prepare","paths":{"appServerExecutable":file}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let jobs = state.jobs.lock().await;
            if jobs[0].state == "failed" {
                assert!(jobs[0].error.is_some());
                break;
            }
            drop(jobs);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn local_console_without_token_keeps_origin_and_client_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let disabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/access")
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"passwordEnabled":false,"allowPublicAccess":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    for (path, method, host, origin, status) in [
        (
            "/api/session",
            "GET",
            "127.0.0.1:4317",
            None,
            StatusCode::OK,
        ),
        (
            "/api/operations",
            "GET",
            "127.0.0.1:4317",
            None,
            StatusCode::OK,
        ),
        (
            "/api/operations",
            "GET",
            "127.0.0.1:4317",
            Some("http://127.0.0.1:4317"),
            StatusCode::OK,
        ),
        (
            "/api/operations",
            "GET",
            "evil.invalid",
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/operations",
            "GET",
            "127.0.0.1:4317",
            Some("https://evil.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/session",
            "GET",
            "127.0.0.1:4317",
            Some("https://evil.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/session",
            "GET",
            "evil.invalid",
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/models",
            "GET",
            "127.0.0.1:4317",
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/v1/native",
            "GET",
            "127.0.0.1:4317",
            None,
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            status,
            "{method} {path} {host} {origin:?}"
        );
        if path == "/api/session" && status == StatusCode::OK {
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(
                body,
                json!({"requiresToken": false,"passwordEnabled":false,"authenticated":true,"allowPublicAccess":false})
            );
        }
    }
    // Management writes work without a token, including when a stale token is cached.
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/groups")
        .header("host", "127.0.0.1:4317").header("origin", "http://127.0.0.1:4317")
        .header("authorization", "Bearer stale-token").header("content-type", "application/json")
        .body(Body::from(json!({"revision":state.operations.snapshot()["revision"],"name":"local-access","enabled":true}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let enabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/access")
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"passwordEnabled":false,"allowPublicAccess":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    let client = codex_relay::network::client_router(
        state,
        codex_relay::network::AccessPolicy::new(&["http://192.168.1.10:4318/v1".into()], &[])
            .unwrap(),
    );
    for (path, status) in [
        ("/api/session", StatusCode::NOT_FOUND),
        ("/api/operations", StatusCode::NOT_FOUND),
        ("/v1/models", StatusCode::UNAUTHORIZED),
    ] {
        let response = client
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "192.168.1.10:4318")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}

#[tokio::test]
async fn optional_token_mode_exposes_only_the_login_requirement_before_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let token = "d".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"requiresToken": true,"passwordEnabled":false,"authenticated":true,"allowPublicAccess":true})
    );
    for bearer in [None, Some("stale-token"), Some(token.as_str())] {
        let mut request = Request::builder()
            .uri("/api/operations")
            .header("host", "127.0.0.1:4317");
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if bearer == Some(token.as_str()) {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
}

#[tokio::test]
async fn admin_rejects_missing_token_foreign_origin_and_unknown_account() {
    let temp = tempfile::tempdir().unwrap();
    let token = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    for (bearer, origin, host) in [
        ("", "http://127.0.0.1:4317", "127.0.0.1:4317"),
        ("Bearer a", "http://127.0.0.1:4317", "127.0.0.1:4317"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/overview")
                    .header("host", host)
                    .header("origin", origin)
                    .header("authorization", bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for (origin, host) in [
        ("https://evil.invalid", "127.0.0.1:4317"),
        ("http://127.0.0.1:4317", "evil.invalid:4317"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/overview")
                    .header("host", host)
                    .header("origin", origin)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/accounts/{}/stop", uuid::Uuid::new_v4()))
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/native")
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        String::from_utf8(to_bytes(response.into_body(), 4096).await.unwrap().to_vec())
            .unwrap()
            .contains("INVALID_API_KEY")
    );
}

#[tokio::test]
async fn profiles_persist_metadata_with_isolated_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let profiles = Profiles::open(root.clone()).await.unwrap();
    assert!(profiles.create("\n").await.is_err());
    let first = profiles.create("账户 A").await.unwrap();
    let second = profiles.create("账户 B").await.unwrap();
    assert_ne!(first.id, second.id);
    for id in [first.id, second.id] {
        assert!(root.join(id.to_string()).join("home/config.toml").is_file());
        assert!(root.join(id.to_string()).join("workspace").is_dir());
    }
    let persisted = tokio::fs::read(root.join("profiles.json")).await.unwrap();
    let data: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
    for record in data.as_array().unwrap() {
        assert_eq!(record.as_object().unwrap().len(), 3);
    }
    drop(profiles);
    let profiles = Profiles::open(root).await.unwrap();
    assert_eq!(profiles.list().await.len(), 2);
    assert!(
        profiles
            .list()
            .await
            .iter()
            .all(|p| p.state == "stopped" && p.account.is_none())
    );
    assert!(
        profiles
            .import_token(first.id, "bad".into(), "id".into())
            .await
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(profiles.list().await).unwrap()[0]["account"],
        json!(null)
    );
}

#[tokio::test]
#[ignore = "Requires the locally pinned Codex installation; creates two unsigned-in native instances, never dispatches inference."]
async fn native_accounts_start_and_stop_independently() {
    std::env::set_current_dir(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let profiles = Profiles::open(temp.path().join("accounts")).await.unwrap();
    let a = profiles.create("Native test A").await.unwrap();
    let b = profiles.create("Native test B").await.unwrap();
    profiles.start(a.id).await.unwrap();
    profiles.start(b.id).await.unwrap();
    let views = profiles.list().await;
    assert!(
        views
            .iter()
            .all(|p| p.state == "signed-out" && p.account.is_none())
    );
    assert_ne!(views[0].pid, views[1].pid);
    profiles.stop(a.id).await.unwrap();
    let views = profiles.list().await;
    assert_eq!(views[0].state, "stopped");
    assert_eq!(views[1].state, "signed-out");
    profiles.shutdown().await;
}

#[tokio::test]
async fn operations_routes_persist_groups_enforce_admission_and_expose_filtered_statistics() {
    let temp = tempfile::tempdir().unwrap();
    let token = "c".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    async fn call(
        app: &axum::Router,
        token: &str,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
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
        let value =
            serde_json::from_slice(&to_bytes(response.into_body(), 100000).await.unwrap()).unwrap();
        (status, value)
    }
    assert_eq!(
        call(&app, "wrong", "GET", "/api/statistics", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, account) = call(
        &app,
        &token,
        "POST",
        "/api/accounts",
        json!({"name":"API test"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = uuid::Uuid::parse_str(account["id"].as_str().unwrap()).unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    let (status, group) = call(
        &app,
        &token,
        "POST",
        "/api/groups",
        json!({"revision":revision,"name":"API group","enabled":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let group_id = group["id"].as_str().unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/accounts/{id}/policy"),
            json!({"revision":revision,"groupIds":[group_id],"enabled":true,"maxConcurrency":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            "/api/settings/concurrency",
            json!({"revision":revision,"globalLimitAuto":false,"globalLimit":1})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let lease = state
        .operations
        .begin(Some(id), None, "native_check", "test", true)
        .unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            "/api/settings/queue",
            json!({"revision":revision,"queueLimit":1,"queueTimeoutSeconds":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    let waiting_app = app.clone();
    let waiting_token = token.clone();
    let waiting = tokio::spawn(async move {
        call(
            &waiting_app,
            &waiting_token,
            "POST",
            &format!("/api/accounts/{id}/start"),
            json!({}),
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
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/accounts/{id}/start"),
            json!({})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (status, body) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "QUEUE_TIMEOUT");
    drop(lease);
    let (_, stats) = call(
        &app,
        &token,
        "GET",
        &format!("/api/statistics?groupId={group_id}&status=rejected"),
        json!({}),
    )
    .await;
    assert_eq!(stats["summary"]["total"], 1);
    assert_eq!(stats["records"][0]["errorCode"], "QUEUE_FULL");
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/groups/{group_id}"),
            json!({"revision":revision,"name":"API group","enabled":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/accounts/{id}/check"),
            json!({})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/accounts/{id}/stop"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, &token, "GET", "/api/statistics?hours=999", json!({}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn desktop_controls_require_local_admin_and_stop_without_ownership_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, dir.path().join("accounts"), "test-token".into())
        .await
        .unwrap();
    let app = router(state, dir.path().join("web"));
    for path in ["/api/desktop/start", "/api/desktop/stop"] {
        for (token, origin) in [
            ("wrong", "http://127.0.0.1:4317"),
            ("test-token", "https://evil.invalid"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("host", "127.0.0.1:4317")
                        .header("origin", origin)
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/desktop/stop")
                .header("host", "127.0.0.1:4317")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn service_stop_requires_admin_is_idempotent_and_blocks_new_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, dir.path().join("accounts"), "stop-test".into())
        .await
        .unwrap();
    let app = router(state, dir.path().join("web"));
    for (token, origin, expected) in [
        ("wrong", "http://127.0.0.1:4317", StatusCode::FORBIDDEN),
        ("stop-test", "https://evil.invalid", StatusCode::FORBIDDEN),
        ("stop-test", "http://127.0.0.1:4317", StatusCode::ACCEPTED),
        ("stop-test", "http://127.0.0.1:4317", StatusCode::ACCEPTED),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/service/stop")
                    .header("host", "127.0.0.1:4317")
                    .header("origin", origin)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/desktop/start")
                .header("host", "127.0.0.1:4317")
                .header("authorization", "Bearer stop-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
