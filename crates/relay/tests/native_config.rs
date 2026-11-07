use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{oauth, provider};
use serde_json::{Value, json};

fn token(account: &str) -> String {
    let claims = json!({"sub":"synthetic-user","email":"synthetic@example.invalid","exp":codex_relay::now_ms()/1000+3600,"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_plan_type":"plus"}});
    format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

#[test]
fn native_defaults_detect_chatgpt_and_official_api_key() {
    let config = json!({"config":{"model":"test-model"}});
    let bearer = token("synthetic-account");
    let p = provider::from_config(&config, &json!({"authMethod":"chatgpt","authToken":bearer}))
        .unwrap();
    assert_eq!(p.id, "openai");
    assert_eq!(p.base_url, "https://chatgpt.com/backend-api");
    assert!(p.auth.starts_with(oauth::AUTH));
    assert!(!serde_json::to_string(&p).unwrap().contains(&bearer));
    let p = provider::from_config(
        &config,
        &json!({"authMethod":"apikey","authToken":"SYNTHETIC_KEY"}),
    )
    .unwrap();
    assert_eq!(p.base_url, "https://api.openai.com/v1");
    assert_eq!(p.auth, "native-api-key");
    assert!(!serde_json::to_string(&p).unwrap().contains("SYNTHETIC_KEY"));
    assert_eq!(
        provider::from_config(&config, &Value::Null)
            .unwrap_err()
            .code,
        "PROVIDER_AUTH_MISSING"
    );
}

#[test]
fn native_custom_providers_accept_public_local_and_custom_paths_without_leaking_auth() {
    for endpoint in [
        "https://relay.example.com/gateway/v1",
        "http://127.0.0.1:1234/v1",
        "http://[::1]:1234/api",
        "http://192.168.1.2/v1",
    ] {
        let config = json!({"config":{"model":"test-model","model_provider":"relay","model_providers":{"relay":{"base_url":endpoint,"env_key":"SYNTHETIC_RELAY_KEY","query_params":{"api-key":"secret-query"},"http_headers":{"ChatGPT-Account-Id":"secret-header"}}}}});
        let p = provider::from_config(&config, &Value::Null).unwrap();
        assert_eq!(p.base_url, endpoint);
        assert_eq!(p.auth, "native-bearer");
        assert!(!serde_json::to_string(&p).unwrap().contains("secret"));
    }
    let config = json!({"config":{"model":"test","model_provider":"relay","model_providers":{"relay":{"base_url":"https://relay.example/v1?api-key=secret-url","auth":{"command":"secret-command","args":["secret-arg"]}}}}});
    let p = provider::from_config(&config, &Value::Null).unwrap();
    assert_eq!(p.base_url, "https://relay.example/v1");
    assert_eq!(p.auth, "native-provider-auth");
    assert!(!serde_json::to_string(&p).unwrap().contains("secret"));
}

#[test]
fn native_proxy_can_reuse_openai_credentials_and_local_providers_need_no_login() {
    let config = json!({"config":{"model":"test","model_provider":"proxy","model_providers":{"proxy":{"base_url":"https://proxy.example/custom","requires_openai_auth":true,"env_key":"IGNORED_BY_NATIVE"}}}});
    let p = provider::from_config(&config, &json!({"authMethod":"apikey"})).unwrap();
    assert_eq!(p.auth, "native-api-key");
    assert_eq!(p.base_url, "https://proxy.example/custom");
    for (id, endpoint) in [
        ("ollama", "http://localhost:11434/v1"),
        ("lmstudio", "http://localhost:1234/v1"),
    ] {
        let p = provider::from_config(
            &json!({"config":{"model":"local","model_provider":id}}),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(p.auth, "none");
        assert_eq!(p.base_url, endpoint);
    }
}

#[test]
fn native_summary_rejects_invalid_protocol_and_endpoint() {
    let mut config = json!({"config":{"model":"test","model_provider":"relay","model_providers":{"relay":{"base_url":"https://relay.example/v1"}}}});
    config["config"]["model_providers"]["relay"]["wire_api"] = json!("chat");
    assert_eq!(
        provider::from_config(&config, &Value::Null)
            .unwrap_err()
            .code,
        "PROVIDER_PROTOCOL"
    );
    config["config"]["model_providers"]["relay"]["wire_api"] = json!("responses");
    for endpoint in [
        "file:///secret",
        "https://user:secret@relay.example/v1",
        "https://relay.example/v1#secret",
    ] {
        config["config"]["model_providers"]["relay"]["base_url"] = json!(endpoint);
        assert!(provider::from_config(&config, &Value::Null).is_err());
    }
}

#[tokio::test]
async fn native_config_creation_uses_the_generic_source_and_rejects_old_input() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use codex_relay::admin::{AdminState, router};
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    for (field, expected) in [("nativeConfig", 201), ("nativeLan", 422)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/accounts")
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name":"native",field:true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        if expected == 201 {
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                    .unwrap();
            assert_eq!(body["source"], "native-config");
        }
    }
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; all credentials and model endpoints are synthetic"]
async fn native_config_reuses_each_auth_mode_through_the_client_api() {
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, Method, Uri},
        response::IntoResponse,
    };
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    struct Fixture {
        bearer: String,
        endpoint: String,
        calls: AtomicUsize,
        subscription_calls: AtomicUsize,
        request_started: std::sync::Mutex<Option<std::time::Instant>>,
        upstream_received: std::sync::Mutex<Option<std::time::Duration>>,
        release: tokio::sync::Notify,
    }
    async fn upstream(
        State(f): State<Arc<Fixture>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::response::Response {
        if uri.path().ends_with("/subscriptions") {
            f.subscription_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["authorization"], format!("Bearer {}", f.bearer));
            assert_eq!(uri.query(), Some("account_id=synthetic-native"));
            return axum::Json(json!({"active_until":"2099-01-01T00:00:00Z","will_renew":true}))
                .into_response();
        }
        if method != Method::POST {
            return axum::Json(if uri.path().ends_with("/accounts/check") {
                json!({"accounts":[{"id":"synthetic-native","workspace_backend_origin":f.endpoint,"account_routing_override":"NO_CONSTRAINT"}]})
            } else {json!({"models":[]})}).into_response();
        }
        assert_eq!(headers["authorization"], format!("Bearer {}", f.bearer));
        assert!(headers["user-agent"].to_str().unwrap().contains("codex"));
        let request: Value = serde_json::from_slice(&body).unwrap();
        if !uri.path().ends_with("/responses") {
            return axum::Json(json!({})).into_response();
        }
        if let Some(started) = *f.request_started.lock().unwrap() {
            *f.upstream_received.lock().unwrap() = Some(started.elapsed());
        }
        assert_eq!(
            request["model"],
            "gpt-6.1-sol",
            "path={} keys={:?}",
            uri.path(),
            request.as_object().map(|v| v.keys().collect::<Vec<_>>())
        );
        let call = f.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request["reasoning"]["effort"], "high");
        assert_eq!(
            request["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(
                    |item| item["role"] == "user" && item["content"][0]["text"] == "Synthetic test"
                )
                .count(),
            1,
            "caller input must reach the model exactly once"
        );
        let input = request["input"].as_array().unwrap();
        assert_eq!(
            input
                .iter()
                .filter(
                    |i| i["role"] == "user" && i["content"][0]["text"] == "Synthetic second turn"
                )
                .count(),
            usize::from(call % 2 == 1)
        );
        assert_eq!(
            input
                .iter()
                .filter(|i| i["role"] == "assistant"
                    && i["content"][0]["text"] == "native credentials work")
                .count(),
            usize::from(call % 2 == 1)
        );
        if call == 3 {
            f.release.notified().await;
            return axum::http::StatusCode::GATEWAY_TIMEOUT.into_response();
        }
        let item = json!({"type":"message","id":format!("synthetic_message_{call}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"native credentials work","annotations":[]}]});
        let events = [
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":{"id":"synthetic_response","status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
        ];
        (
            [("content-type", "text/event-stream")],
            events
                .iter()
                .map(|v| format!("data: {v}\n\n"))
                .collect::<String>(),
        )
            .into_response()
    }
    for mode in ["chatgpt", "apikey", "relay", "relay-openai-auth"] {
        if std::env::var("BRIDGE_NATIVE_TEST_MODE").is_ok_and(|selected| selected != mode) {
            continue;
        }
        let temp = tempfile::tempdir().unwrap();
        for dir in [".cache", "web", "native-home"] {
            std::fs::create_dir(temp.path().join(dir)).unwrap();
        }
        std::fs::write(temp.path().join("web/index.html"), "fixture").unwrap();
        std::fs::copy(
            std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap(),
            temp.path().join(".cache/launch-config.json"),
        )
        .unwrap();
        let bearer = if mode == "chatgpt" {
            token("synthetic-native")
        } else {
            "SYNTHETIC_NATIVE_KEY".into()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "{}://127.0.0.1:{}",
            if mode == "chatgpt" { "https" } else { "http" },
            listener.local_addr().unwrap().port()
        );
        let fixture = Arc::new(Fixture {
            bearer: bearer.clone(),
            endpoint: endpoint.clone(),
            calls: AtomicUsize::new(0),
            subscription_calls: AtomicUsize::new(0),
            request_started: std::sync::Mutex::new(None),
            upstream_received: std::sync::Mutex::new(None),
            release: tokio::sync::Notify::new(),
        });
        let router = Router::new().fallback(upstream).with_state(fixture.clone());
        let certificate = temp.path().join("fixture-ca.pem");
        let upstream_server = if mode == "chatgpt" {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
            std::fs::write(&certificate, certified.cert.pem()).unwrap();
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
                certified.cert.pem().into_bytes(),
                certified.signing_key.serialize_pem().into_bytes(),
            )
            .await
            .unwrap();
            let server = axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls).unwrap();
            tokio::spawn(async move { server.serve(router.into_make_service()).await })
        } else {
            tokio::spawn(async move { axum::serve(listener, router).await })
        };
        let home = temp.path().join("native-home");
        let mut config = "model=\"gpt-6.1-sol\"\ncli_auth_credentials_store=\"file\"\n".to_owned();
        match mode {
            "chatgpt" => {
                config.push_str(&format!("chatgpt_base_url=\"{endpoint}/backend-api\"\n"));
                let credential = oauth::parse(&json!({"accessToken":bearer,"idToken":bearer,"refreshToken":"SYNTHETIC_REFRESH_ONLY"})).unwrap();
                oauth::save(&home.join("auth.json"), &credential.native_document())
                    .await
                    .unwrap();
            }
            "apikey" => {
                config.push_str(&format!("openai_base_url=\"{endpoint}/official/v1\"\n"));
                std::fs::write(
                    home.join("auth.json"),
                    json!({"auth_mode":"apikey","OPENAI_API_KEY":bearer}).to_string(),
                )
                .unwrap();
            }
            _ => {
                let auth = if mode == "relay" {
                    "env_key=\"SYNTHETIC_RELAY_KEY\""
                } else {
                    "requires_openai_auth=true"
                };
                config.push_str(&format!("model_provider=\"relay\"\n[model_providers.relay]\nname=\"Synthetic relay\"\nbase_url=\"{endpoint}/gateway/custom/v1\"\nwire_api=\"responses\"\n{auth}\n"));
                if mode == "relay-openai-auth" {
                    std::fs::write(
                        home.join("auth.json"),
                        json!({"auth_mode":"apikey","OPENAI_API_KEY":bearer}).to_string(),
                    )
                    .unwrap();
                }
            }
        }
        config.push_str("[features]\nenable_request_compression=false\n");
        std::fs::write(home.join("config.toml"), &config).unwrap();
        let port = codex_relay::backend::unused_port().await.unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_codex-relay"));
        for (key, _) in std::env::vars_os() {
            if ["CODEX_", "OPENAI_", "CHATGPT_"]
                .iter()
                .any(|prefix| key.to_string_lossy().to_uppercase().starts_with(prefix))
            {
                command.env_remove(key);
            }
        }
        command.env_remove("SSL_CERT_FILE");
        if mode == "chatgpt" {
            command.env("SSL_CERT_FILE", &certificate);
        }
        let mut server = command
            .args(["serve", "--port", &port.to_string(), "--web-dir", "web"])
            .env("CODEX_HOME", &home)
            .env("SYNTHETIC_RELAY_KEY", &bearer)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(45))
            .build()
            .unwrap();
        let base = format!("http://127.0.0.1:{port}");
        for _ in 0..100 {
            if client.get(format!("{base}/healthz")).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let account: Value = client
            .post(format!("{base}/api/accounts"))
            .json(&json!({"name":mode,"nativeConfig":true}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(account["source"], "native-config");
        let id = account["id"].as_str().unwrap();
        let before: Value = client
            .get(format!("{base}/api/operations"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let response = client
            .post(format!("{base}/api/accounts/{id}/route"))
            .json(&json!({"revision":before["revision"],"enabled":true}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let enabled: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{mode}: {enabled}");
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            0,
            "Enabling must not send inference"
        );
        assert!(!enabled.to_string().contains(&bearer));
        if mode == "chatgpt" {
            let mut subscription = Value::Null;
            for _ in 0..80 {
                subscription = client
                    .get(format!("{base}/api/accounts/{id}/subscription"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if subscription["status"] == "current" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            assert_eq!(subscription["status"], "current", "{subscription}");
            assert_eq!(subscription["willRenew"], true);
            for _ in 0..2 {
                let value: Value = client
                    .post(format!("{base}/api/accounts/{id}/subscription/refresh"))
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                assert_eq!(value["status"], "current", "{value}");
            }
            assert_eq!(fixture.subscription_calls.load(Ordering::SeqCst), 1);
        } else {
            let subscription: Value = client
                .get(format!("{base}/api/accounts/{id}/subscription"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(subscription["status"], "unsupported");
            assert_eq!(fixture.subscription_calls.load(Ordering::SeqCst), 0);
        }

        let key: Value = client
            .post(format!("{base}/api/api-keys"))
            .json(&json!({"name":"synthetic","groupId":codex_relay::operations::DEFAULT_GROUP}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        *fixture.request_started.lock().unwrap() = Some(std::time::Instant::now());
        let response = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(key["secret"].as_str().unwrap())
            .json(&json!({"model":"gpt-6.1-sol","input":"Synthetic test","store":false,"stream":false,"reasoning":{"effort":"high"}}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let response: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{mode}: {response}");
        assert_eq!(response["status"], "completed", "{mode}: {response}");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        println!(
            "{}",
            json!({"event":"synthetic_upstream_received","authMode":mode,
            "requestToUpstreamMs":fixture.upstream_received.lock().unwrap().unwrap().as_millis(),
            "realModelUsage":false})
        );
        let overview: Value = client
            .get(format!("{base}/api/overview"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pid = overview["accounts"][0]["pid"]
            .as_u64()
            .expect("successful HTTP worker stays warm");
        assert_eq!(
            overview["accounts"][0]["planType"],
            if mode == "chatgpt" {
                json!("plus")
            } else {
                Value::Null
            },
            "native subscription must follow the configured authentication"
        );
        assert!(!overview.to_string().contains(&bearer));
        let mut history = vec![json!({"role":"user","content":"Synthetic test"})];
        history.extend(response["output"].as_array().unwrap().clone());
        history.push(json!({"role":"user","content":"Synthetic second turn"}));
        *fixture.request_started.lock().unwrap() = Some(std::time::Instant::now());
        let second: Value = client.post(format!("{base}/v1/responses"))
            .bearer_auth(key["secret"].as_str().unwrap())
            .json(&json!({"model":"gpt-6.1-sol","input":history,"store":false,"stream":false,"reasoning":{"effort":"high"}}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(second["status"], "completed", "{mode}: {second}");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        println!(
            "{}",
            json!({"event":"synthetic_upstream_received","authMode":mode,
            "turn":"continuation","requestToUpstreamMs":fixture.upstream_received.lock().unwrap().unwrap().as_millis(),"realModelUsage":false})
        );
        let overview: Value = client
            .get(format!("{base}/api/overview"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            overview["accounts"][0]["pid"], pid,
            "continuation reuses the process"
        );
        let stats: Value = client
            .get(format!("{base}/api/statistics"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let records: Vec<_> = stats["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "inference")
            .collect();
        assert_eq!(records.len(), 2);
        assert!(records[0]["nativeThreadId"].is_string());
        assert_eq!(
            records[0]["nativeThreadId"], records[1]["nativeThreadId"],
            "continuation reuses the native thread"
        );
        // A new request from a different caller must not inherit that thread's history.
        let other_key: Value = client
            .post(format!("{base}/api/api-keys"))
            .json(&json!({"name":"other caller","groupId":codex_relay::operations::DEFAULT_GROUP}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let fresh: Value = client.post(format!("{base}/v1/responses"))
            .bearer_auth(other_key["secret"].as_str().unwrap())
            .json(&json!({"model":"gpt-6.1-sol","input":"Synthetic test","store":false,"stream":false,"reasoning":{"effort":"high"}}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(fresh["status"], "completed", "{mode}: {fresh}");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
        if mode == "relay" {
            let mut history = vec![json!({"role":"user","content":"Synthetic test"})];
            history.extend(fresh["output"].as_array().unwrap().clone());
            history.push(json!({"role":"user","content":"Synthetic second turn"}));
            let stream = client.post(format!("{base}/v1/responses"))
                .bearer_auth(other_key["secret"].as_str().unwrap())
                .json(&json!({"model":"gpt-6.1-sol","input":history,"stream":true,"store":false,"reasoning":{"effort":"high"}}))
                .send().await.unwrap().error_for_status().unwrap();
            for _ in 0..100 {
                if fixture.calls.load(Ordering::SeqCst) == 4 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
            client
                .post(format!("{base}/api/accounts/{id}/stop"))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            fixture.release.notify_one();
            let body = stream.text().await.unwrap();
            assert!(
                !body.contains("response.completed"),
                "a cancelled turn cannot complete successfully"
            );
            let stats: Value = client
                .get(format!("{base}/api/statistics"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(
                stats["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["kind"] == "inference" && r["errorCode"] == "REQUEST_CANCELLED")
            );
            let overview: Value = client
                .get(format!("{base}/api/overview"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(
                overview["accounts"][0]["pid"].is_null(),
                "cancelled warm worker must be reaped"
            );
        }
        client
            .post(format!("{base}/api/accounts/{id}/stop"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let overview: Value = client
            .get(format!("{base}/api/overview"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            overview["accounts"][0]["pid"].is_null(),
            "explicit stop reaps the warm worker"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            config
        );
        client
            .post(format!("{base}/api/service/stop"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), server.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        upstream_server.abort();
    }
}
