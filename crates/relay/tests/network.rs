use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    network::{
        AccessPolicy, BoundListener, ClientListener, NetworkConfig, TlsFiles, client_router,
    },
    operations::{ApiKeyCreate, DEFAULT_GROUP},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

async fn setup() -> (tempfile::TempDir, Arc<AdminState>, String) {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let enabled = router(state.clone(), temp.path().join("web"))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/access")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {}", "a".repeat(64)))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"passwordEnabled":false,"allowPublicAccess":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    let key=state.operations.create_api_key(serde_json::from_value::<ApiKeyCreate>(json!({"name":"network","groupId":DEFAULT_GROUP,"models":["a"],"expiresInDays":1,"maxConcurrency":1})).unwrap()).unwrap();
    (temp, state, key.secret)
}
fn policy() -> AccessPolicy {
    AccessPolicy::new(
        &[
            "http://192.168.1.10:4318/v1".into(),
            "https://bridge.example.com/v1".into(),
        ],
        &["https://client.example.com".into()],
    )
    .unwrap()
}
async fn call(
    app: &Router,
    path: &str,
    method: Method,
    host: &str,
    secret: Option<&str>,
    origin: Option<&str>,
) -> axum::response::Response {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    if let Some(secret) = secret {
        req = req.header("authorization", format!("Bearer {secret}"));
    }
    if let Some(origin) = origin {
        req = req.header("origin", origin);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[test]
fn config_rejects_ambiguous_addresses_credentials_and_invalid_transport() {
    for value in [
        json!({"listeners":[{"listen":"0.0.0.0:4317","baseUrls":["http://192.168.1.10:4317/v1"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:0","baseUrls":["http://192.168.1.10/v1"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://0.0.0.0:4318/v1"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://user:secret@bridge.example.com/v1"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://bridge.example.com/v1?key=secret"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://bridge.example.com/admin"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["https://bridge.example.com/v1"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://bridge.example.com/v1"],"allowedOrigins":["*"]}]}),
        json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://bridge.example.com/v1"],"tls":{"certificate":"c.pem","privateKey":"k.pem"}}]}),
    ] {
        assert!(
            serde_json::from_value::<NetworkConfig>(value.clone())
                .unwrap()
                .validate(4317)
                .is_err(),
            "{value}"
        );
    }
    let good:NetworkConfig=serde_json::from_value(json!({"listeners":[{"listen":"0.0.0.0:4318","baseUrls":["http://192.168.1.10:4318/v1","http://bridge.example.com:4318/v1"]},{"listen":"127.0.0.1:4319","baseUrls":["https://bridge.example.com/v1"]}]})).unwrap();
    let endpoints = good.validate(4317).unwrap();
    assert_eq!(endpoints.len(), 3);
    assert_eq!(endpoints[2].transport, "reverse-proxy");
    assert!(
        serde_json::from_value::<NetworkConfig>(json!({"listeners":[],"unknown":true})).is_err()
    );
}

#[test]
fn host_and_origin_policy_rejects_duplicates_and_supports_ipv6_and_default_ports() {
    let policy = AccessPolicy::new(
        &[
            "https://bridge.example.com/v1".into(),
            "http://[::1]:4318/v1".into(),
            "https://[::1]/v1".into(),
        ],
        &["https://client.example.com".into()],
    )
    .unwrap();
    for host in [
        "bridge.example.com",
        "BRIDGE.EXAMPLE.COM:443",
        "[::1]:4318",
        "[::1]",
        "[::1]:443",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_str(host).unwrap());
        assert!(policy.allows(&headers));
        headers.insert("origin", HeaderValue::from_static("null"));
        assert!(!policy.allows(&headers));
        headers.insert(
            "origin",
            HeaderValue::from_static("https://client.example.com"),
        );
        assert!(policy.allows(&headers));
        headers.append(
            "origin",
            HeaderValue::from_static("https://client.example.com"),
        );
        assert!(!policy.allows(&headers));
        headers.remove("origin");
        headers.append("host", HeaderValue::from_str(host).unwrap());
        assert!(!policy.allows(&headers));
    }
}

#[tokio::test]
async fn external_api_never_mounts_management_and_keeps_key_authentication() {
    let (temp, state, key) = setup().await;
    let app = client_router(state.clone(), policy());
    for path in [
        "/",
        "/api/overview",
        "/api/accounts",
        "/api/api-keys",
        "/assets/index.js",
        "/healthz",
        "/data/accounts/auth.json",
    ] {
        let res = call(
            &app,
            path,
            Method::GET,
            "192.168.1.10:4318",
            Some(&"a".repeat(64)),
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(res.headers()["cache-control"], "no-store");
    }
    for secret in [
        None,
        Some("wrong"),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    ] {
        let res = call(
            &app,
            "/v1/models",
            Method::GET,
            "192.168.1.10:4318",
            secret,
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.headers()["www-authenticate"], "Bearer");
    }
    let res = call(
        &app,
        "/v1/models",
        Method::GET,
        "192.168.1.10:4318",
        Some(&key),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-accel-buffering"], "no");
    let value: Value =
        serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value, json!({"object":"list","data":[]}));
    for (host, origin) in [
        ("evil.invalid", None),
        ("192.168.1.10:4318", Some("https://evil.invalid")),
    ] {
        assert_eq!(
            call(&app, "/v1/models", Method::GET, host, Some(&key), origin)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let spoofed = Request::builder()
        .uri("/v1/models")
        .header("host", "evil.invalid")
        .header("x-forwarded-host", "bridge.example.com")
        .header("forwarded", "host=bridge.example.com;proto=https")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(spoofed).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let local = router(state, temp.path().join("web"));
    assert_eq!(
        call(
            &local,
            "/api/overview",
            Method::GET,
            "192.168.1.10:4318",
            Some(&"a".repeat(64)),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn http2_authority_uses_the_same_host_boundary() {
    let (_temp, state, key) = setup().await;
    let app = client_router(state, policy());
    for (authority, host, expected) in [
        ("bridge.example.com", None, StatusCode::OK),
        ("evil.invalid", None, StatusCode::FORBIDDEN),
        (
            "evil.invalid",
            Some("bridge.example.com"),
            StatusCode::FORBIDDEN,
        ),
        (
            "bridge.example.com",
            Some("evil.invalid"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::builder()
            .version(axum::http::Version::HTTP_2)
            .uri(format!("https://{authority}/v1/models"))
            .header("authorization", format!("Bearer {key}"));
        if let Some(host) = host {
            request = request.header("host", host);
        }
        assert_eq!(
            app.clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            expected
        );
    }
}

#[tokio::test]
async fn cors_preflight_is_explicit_and_actual_requests_still_require_a_key() {
    let (_temp, state, key) = setup().await;
    let app = client_router(state, policy());
    for (path, method) in [
        ("/v1/responses", "POST"),
        ("/v1/responses/resp_test", "GET"),
        ("/v1/responses/resp_test", "DELETE"),
    ] {
        let req = Request::builder()
            .method("OPTIONS")
            .uri(path)
            .header("host", "bridge.example.com")
            .header("origin", "https://client.example.com")
            .header("access-control-request-method", method)
            .header(
                "access-control-request-headers",
                "authorization, content-type",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
    }
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/v1/native")
        .header("host", "bridge.example.com")
        .header("origin", "https://client.example.com")
        .header("access-control-request-method", "GET")
        .header(
            "access-control-request-headers",
            "authorization, content-type",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://client.example.com"
    );
    assert!(
        !res.headers()
            .contains_key("access-control-allow-credentials")
    );
    let res = call(
        &app,
        "/v1/models",
        Method::GET,
        "bridge.example.com",
        None,
        Some("https://client.example.com"),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://client.example.com"
    );
    let res = call(
        &app,
        "/v1/models",
        Method::GET,
        "bridge.example.com",
        Some(&key),
        Some("https://client.example.com"),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["vary"], "Origin");
    for (path, method, headers) in [
        ("/api/accounts", "POST", "authorization"),
        ("/v1/models", "DELETE", "authorization"),
        ("/v1/models", "GET", "cookie"),
    ] {
        let request = Request::builder()
            .method("OPTIONS")
            .uri(path)
            .header("host", "bridge.example.com")
            .header("origin", "https://client.example.com")
            .header("access-control-request-method", method)
            .header("access-control-request-headers", headers)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn real_http_and_https_listeners_share_auth_and_stop_cleanly() {
    let (temp, state, key) = setup().await;
    let cancel = CancellationToken::new();
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let certificate = temp.path().join("cert.pem");
    let private_key = temp.path().join("key.pem");
    std::fs::write(&certificate, certified.cert.pem()).unwrap();
    std::fs::write(&private_key, certified.signing_key.serialize_pem()).unwrap();
    let root = reqwest::Certificate::from_pem(certified.cert.pem().as_bytes()).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(root)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut servers = Vec::new();
    let mut urls = Vec::new();
    for tls in [false, true] {
        let config = ClientListener {
            listen: "127.0.0.1:0".parse().unwrap(),
            base_urls: vec![format!(
                "{}://127.0.0.1/v1",
                if tls { "https" } else { "http" }
            )],
            allowed_origins: vec![],
            tls: tls.then(|| TlsFiles {
                certificate: certificate.clone(),
                private_key: private_key.clone(),
            }),
        };
        let bound = BoundListener::bind(&config).await.unwrap();
        let address = bound.local_addr().unwrap();
        urls.push(format!(
            "{}://{address}",
            if tls { "https" } else { "http" }
        ));
        servers.push(tokio::spawn(bound.serve(state.clone(), cancel.clone())));
    }
    for url in &urls {
        let res = client
            .get(format!("{url}/v1/models"))
            .header("host", "127.0.0.1")
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = client
            .get(format!("{url}/api/overview"))
            .header("host", "127.0.0.1")
            .bearer_auth("a".repeat(64))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
    // Without trusting this test certificate, HTTPS must fail verification.
    assert!(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(format!("{}/v1/models", urls[1]))
            .send()
            .await
            .is_err()
    );
    cancel.cancel();
    for server in servers {
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    for url in urls {
        assert!(client.get(format!("{url}/v1/models")).send().await.is_err());
    }
}

#[tokio::test]
async fn invalid_certificate_and_occupied_port_fail_without_processing_requests() {
    let temp = tempfile::tempdir().unwrap();
    let config = ClientListener {
        listen: "127.0.0.1:0".parse().unwrap(),
        base_urls: vec!["https://localhost/v1".into()],
        allowed_origins: vec![],
        tls: Some(TlsFiles {
            certificate: temp.path().join("missing"),
            private_key: temp.path().join("missing-key"),
        }),
    };
    assert_eq!(
        BoundListener::bind(&config).await.err().unwrap().code,
        "NETWORK_TLS"
    );
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = ClientListener {
        listen: occupied.local_addr().unwrap(),
        base_urls: vec!["http://localhost/v1".into()],
        allowed_origins: vec![],
        tls: None,
    };
    assert_eq!(
        BoundListener::bind(&config).await.err().unwrap().code,
        "NETWORK_BIND"
    );
}

#[tokio::test]
async fn unknown_endpoints_never_queue_or_dispatch_even_with_an_enabled_route() {
    let (_temp, state, key) = setup().await;
    let id = uuid::Uuid::new_v4();
    state
        .operations
        .register_accounts(&[(id, "test".into())])
        .unwrap();
    state
        .operations
        .set_inference_route(
            id,
            state.operations.snapshot()["revision"].as_u64().unwrap(),
            Some(codex_relay::provider::Provider {
                execution: Default::default(),
                id: "lan_test".into(),
                model: "a".into(),
                base_url: "http://192.168.1.10/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let held = state
        .operations
        .acquire_client(id, &state.operations.authenticate_api_key(&key).unwrap())
        .await
        .unwrap();
    let app = client_router(state.clone(), policy());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/not-an-endpoint")
        .header("host", "192.168.1.10:4318")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"model":"a","input":"queued only","stream":true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    held.complete(Ok(())).unwrap();
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert_eq!(state.operations.snapshot()["queued"], 0);
    assert!(state.profiles.list().await.is_empty());
}

#[tokio::test]
async fn configuration_file_resolves_relative_certificate_paths() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("network.json");
    std::fs::write(&path,json!({"listeners":[{"listen":"0.0.0.0:4319","baseUrls":["https://bridge.example.com:4319/v1"],"tls":{"certificate":"certs/c.pem","privateKey":"certs/k.pem"}}]}).to_string()).unwrap();
    let config = NetworkConfig::read(&path).await.unwrap();
    assert_eq!(
        config.listeners[0].tls.as_ref().unwrap().certificate,
        temp.path().join("certs/c.pem")
    );
    let endpoints = config.validate(4317).unwrap();
    let state = AdminState::new_with_network(
        4317,
        temp.path().join("accounts"),
        Some("a".repeat(64)),
        endpoints,
    )
    .await
    .unwrap();
    let app = router(state, temp.path().join("web"));
    let res = call(
        &app,
        "/api/api-keys",
        Method::GET,
        "127.0.0.1:4317",
        Some(&"a".repeat(64)),
        None,
    )
    .await;
    let value: Value =
        serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["baseUrl"], "https://bridge.example.com:4319/v1");
    assert_eq!(value["localBaseUrl"], "http://127.0.0.1:4317/v1");
    assert!(!value.to_string().contains("certs"));
}
