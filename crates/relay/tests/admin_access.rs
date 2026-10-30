use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    network::{AccessPolicy, client_router},
    operations::{ApiKeyCreate, DEFAULT_GROUP},
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    host: &str,
    remote: bool,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if remote {
        request = request.extension(ConnectInfo(
            "203.0.113.4:45678".parse::<SocketAddr>().unwrap(),
        ));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}
const LOCAL: &str = "127.0.0.1:4317";
const PUBLIC: &str = "bridge.example.com:4317";

#[tokio::test]
async fn public_access_is_independent_of_password_and_gates_both_routers_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
        .await
        .unwrap();
    let web = temp.path().join("web");
    tokio::fs::create_dir(&web).await.unwrap();
    tokio::fs::write(web.join("index.html"), "test console")
        .await
        .unwrap();
    let app = router(state.clone(), web.clone());
    let client = client_router(
        state.clone(),
        AccessPolicy::new(&[format!("http://{PUBLIC}/v1")], &[]).unwrap(),
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/settings/access",
            Value::Null,
            None,
            LOCAL,
            false
        )
        .await
        .1,
        json!({"passwordEnabled":false,"allowPublicAccess":true})
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            None,
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &client,
            "GET",
            "/v1/models",
            Value::Null,
            None,
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/settings/access",
            json!({"passwordEnabled":false,"allowPublicAccess":false}),
            None,
            LOCAL,
            false,
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in [
        "/",
        "/healthz",
        "/api/session",
        "/api/operations",
        "/api/session/login",
        "/v1/models",
        "/v1/native",
    ] {
        for host in [LOCAL, PUBLIC] {
            let result = call(
                &app,
                if path.ends_with("login") {
                    "POST"
                } else {
                    "GET"
                },
                path,
                json!({"password":""}),
                None,
                host,
                true,
            )
            .await;
            assert_eq!(result.0, StatusCode::FORBIDDEN, "{path} {host}");
            assert_eq!(result.1["error"]["code"], "PUBLIC_ACCESS_DISABLED");
        }
    }
    assert_eq!(
        call(
            &client,
            "GET",
            "/v1/models",
            Value::Null,
            None,
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // A loopback reverse proxy preserving a public Host also cannot bypass the switch.
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            None,
            PUBLIC,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let enabled = call(
        &app,
        "POST",
        "/api/settings/access",
        json!({"passwordEnabled":false,"allowPublicAccess":true}),
        None,
        LOCAL,
        false,
    )
    .await;
    assert_eq!(enabled.0, StatusCode::OK);
    assert_eq!(
        enabled.1,
        json!({"passwordEnabled":false,"allowPublicAccess":true})
    );
    for path in ["/", "/healthz", "/api/session", "/api/operations"] {
        assert_eq!(
            call(&app, "GET", path, Value::Null, None, PUBLIC, true)
                .await
                .0,
            StatusCode::OK,
            "{path}"
        );
    }
    for app in [&app, &client] {
        assert_eq!(
            call(app, "GET", "/v1/models", Value::Null, None, PUBLIC, true)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"public test","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    for app in [&app, &client] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header("host", PUBLIC)
                    .header("authorization", format!("Bearer {}", key.secret))
                    .extension(ConnectInfo(
                        "203.0.113.4:45678".parse::<SocketAddr>().unwrap(),
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
    for origin in [
        "https://evil.example",
        "null",
        "https://bridge.example.com:1234",
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/session")
                    .header("host", PUBLIC)
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
    let same = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .header("host", PUBLIC)
                .header("origin", format!("http://{PUBLIC}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(same.status(), StatusCode::OK);
    // Preserve an explicitly supplied default port when comparing HTTPS Origin.
    for (host, origin, expected) in [
        (
            "bridge.example.com:80",
            "http://bridge.example.com",
            StatusCode::OK,
        ),
        (
            "bridge.example.com:80",
            "https://bridge.example.com",
            StatusCode::FORBIDDEN,
        ),
        (
            "bridge.example.com:443",
            "https://bridge.example.com",
            StatusCode::OK,
        ),
        (
            "bridge.example.com:443",
            "http://bridge.example.com",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let result = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/session")
                    .header("host", host)
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), expected, "{host} {origin}");
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/settings/access",
            json!({"passwordEnabled":false,"allowPublicAccess":false}),
            None,
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    for app in [&app, &client] {
        assert_eq!(
            call(app, "GET", "/v1/models", Value::Null, None, PUBLIC, true)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            None,
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(client);
    drop(app);
    drop(state);
    let reopened = AdminState::new_with_network(4317, root, None, vec![])
        .await
        .unwrap();
    assert_eq!(
        call(
            &router(reopened, web),
            "GET",
            "/api/settings/access",
            Value::Null,
            None,
            LOCAL,
            false
        )
        .await
        .1,
        json!({"passwordEnabled":false,"allowPublicAccess":false})
    );
}

#[tokio::test]
async fn short_and_long_passwords_can_be_set_changed_and_used_to_log_in() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    let mut cookie = None;
    let mut current_password = String::new();
    for password in ["1".to_owned(), "a".repeat(129), "密码🔑".repeat(256)] {
        let saved = call(
            &app,
            "POST",
            "/api/settings/access",
            json!({
                "passwordEnabled": true,
                "allowPublicAccess": false,
                "password": password,
                "currentPassword": current_password,
            }),
            cookie.as_deref(),
            LOCAL,
            false,
        )
        .await;
        assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
        let logged_in = call(
            &app,
            "POST",
            "/api/session/login",
            json!({"password": password}),
            None,
            LOCAL,
            false,
        )
        .await;
        assert_eq!(logged_in.0, StatusCode::OK, "{}", logged_in.1);
        cookie = Some(logged_in.2.unwrap().split(';').next().unwrap().to_owned());
        current_password = password;
    }
    let disabled = call(
        &app,
        "POST",
        "/api/settings/access",
        json!({
            "passwordEnabled": false,
            "allowPublicAccess": false,
            "currentPassword": current_password,
        }),
        cookie.as_deref(),
        LOCAL,
        false,
    )
    .await;
    assert_eq!(disabled.0, StatusCode::OK, "{}", disabled.1);
}

#[tokio::test]
async fn password_sessions_guard_management_and_survive_settings_changes_only_with_reauthentication()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let empty = call(
        &app,
        "POST",
        "/api/settings/access",
        json!({"passwordEnabled":true,"allowPublicAccess":false,"password":""}),
        None,
        LOCAL,
        false,
    )
    .await;
    assert_eq!(empty.0, StatusCode::BAD_REQUEST);
    let enabled = call(
        &app,
        "POST",
        "/api/settings/access",
        json!({"passwordEnabled":true,"allowPublicAccess":false,"password":"first test password"}),
        None,
        LOCAL,
        false,
    )
    .await;
    assert_eq!(enabled.0, StatusCode::OK);
    let cookie = enabled.2.unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/api"));
    let cookie = cookie.split(';').next().unwrap().to_owned();
    assert!(!enabled.1.to_string().contains("first test password"));
    assert_eq!(
        call(&app, "GET", "/api/session", Value::Null, None, LOCAL, false)
            .await
            .1["authenticated"],
        false
    );
    for (method, path, body) in [
        ("GET", "/api/operations", Value::Null),
        ("GET", "/api/settings/access", Value::Null),
        ("DELETE", "/api/requests", json!({"confirmed":true})),
        (
            "POST",
            "/api/settings/access",
            json!({"passwordEnabled":false,"allowPublicAccess":true}),
        ),
    ] {
        let denied = call(&app, method, path, body, None, LOCAL, false).await;
        assert_eq!(denied.0, StatusCode::UNAUTHORIZED);
        assert_eq!(denied.1["error"]["code"], "ADMIN_LOGIN_REQUIRED");
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            Some("cb_admin_session=bad"),
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            Some(&cookie),
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/session/login",
            json!({"password":"wrong"}),
            None,
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let logged = call(
        &app,
        "POST",
        "/api/session/login",
        json!({"password":"first test password"}),
        None,
        LOCAL,
        false,
    )
    .await;
    assert_eq!(logged.0, StatusCode::OK);
    let second = logged.2.unwrap().split(';').next().unwrap().to_owned();
    assert_ne!(cookie, second);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/settings/access",
            json!({"passwordEnabled":true,"allowPublicAccess":true,"currentPassword":"wrong"}),
            Some(&cookie),
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let changed=call(&app,"POST","/api/settings/access",json!({"passwordEnabled":true,"allowPublicAccess":true,"currentPassword":"first test password","password":"second test password"}),Some(&cookie),LOCAL,false).await;
    assert_eq!(changed.0, StatusCode::OK);
    let third = changed.2.unwrap().split(';').next().unwrap().to_owned();
    for stale in [&cookie, &second] {
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/operations",
                Value::Null,
                Some(stale),
                LOCAL,
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            Some(&third),
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/v1/models",
            Value::Null,
            Some(&third),
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/session/logout",
            json!({}),
            Some(&third),
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            Some(&third),
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let stored = tokio::fs::read_to_string(root.join("admin-access.json"))
        .await
        .unwrap();
    assert!(!stored.contains("test password"));
    assert!(!stored.contains(third.trim_start_matches("cb_admin_session=")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.join("admin-access.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(app);
    drop(state);
    let state = AdminState::new_with_network(4317, root, None, vec![])
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/session",
            Value::Null,
            Some(&third),
            PUBLIC,
            true
        )
        .await
        .1,
        json!({"requiresToken":false,"passwordEnabled":true,"authenticated":false,"allowPublicAccess":true})
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/session/login",
            json!({"password":"first test password"}),
            None,
            LOCAL,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let logged = call(
        &app,
        "POST",
        "/api/session/login",
        json!({"password":"second test password"}),
        None,
        LOCAL,
        false,
    )
    .await;
    let cookie = logged.2.unwrap().split(';').next().unwrap().to_owned();
    let disabled=call(&app,"POST","/api/settings/access",json!({"passwordEnabled":false,"allowPublicAccess":true,"currentPassword":"second test password"}),Some(&cookie),LOCAL,false).await;
    assert_eq!(disabled.0, StatusCode::OK);
    assert!(disabled.2.unwrap().contains("Max-Age=0"));
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/operations",
            Value::Null,
            None,
            PUBLIC,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn login_limits_failures_and_rejects_cross_origin_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/access")
                .header("host", LOCAL)
                .header("origin", "https://evil.invalid")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"passwordEnabled":false,"allowPublicAccess":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(call(&app,"POST","/api/settings/access",json!({"passwordEnabled":true,"allowPublicAccess":false,"password":"rate test password"}),None,LOCAL,false).await.0,StatusCode::OK);
    for _ in 0..5 {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/session/login",
                json!({"password":"wrong"}),
                None,
                LOCAL,
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let limited = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/login")
                .header("host", LOCAL)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"password":"rate test password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers()["retry-after"], "60");
}
