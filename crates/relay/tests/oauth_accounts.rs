use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{oauth, profiles::Profiles};
use serde_json::{Value, json};
fn jwt(account: &str, expires: u64) -> String {
    let claims = json!({"sub":format!("user-{account}"),"email":"synthetic@example.invalid","exp":expires,"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_user_id":format!("user-{account}"),"chatgpt_plan_type":"plus"}});
    format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    )
}
fn document(account: &str) -> Value {
    let token = jwt(account, codex_relay::now_ms() / 1000 + 3600);
    json!({"tokens":{"access_token":token,"id_token":token,"refresh_token":"SYNTHETIC_REFRESH_ONLY","account_id":account}})
}
#[test]
fn subscription_claims_prefer_access_token_and_fall_back_to_id_token() {
    let mut value = document("subscription");
    let access = value["tokens"]["access_token"].as_str().unwrap();
    let mut payload: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(access.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    for (claim, expected) in [
        (json!("prolite"), "prolite"),
        (json!("  pro  "), "pro"),
        (Value::Null, "plus"),
        (json!(""), "plus"),
        (json!(42), "plus"),
        (json!("bad\nplan"), "plus"),
    ] {
        payload["https://api.openai.com/auth"]["chatgpt_plan_type"] = claim;
        value["tokens"]["access_token"] = json!(format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(payload.to_string())
        ));
        assert_eq!(oauth::parse(&value).unwrap().plan_type(), Some(expected));
    }
    value["tokens"]["id_token"] = value["tokens"]["access_token"].clone();
    assert_eq!(oauth::parse(&value).unwrap().plan_type(), None);
}

#[tokio::test]
async fn subscription_is_available_without_workers_and_cleared_on_logout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let profiles = Profiles::open(root.clone()).await.unwrap();
    let managed = profiles.create("managed").await.unwrap();
    let external = profiles.create("external").await.unwrap();
    profiles
        .import_credentials(managed.id, document("managed"))
        .await
        .unwrap();
    profiles
        .import_credentials(
            external.id,
            json!({
                "accessToken": jwt("external", codex_relay::now_ms() / 1000 + 3600)
            }),
        )
        .await
        .unwrap();
    drop(profiles);
    let profiles = Profiles::open(root.clone()).await.unwrap();
    for view in profiles.list().await {
        assert_eq!(view.plan_type.as_deref(), Some("plus"));
        assert_eq!(view.state, "stopped");
        assert!(view.pid.is_none());
        assert!(
            view.account.is_none(),
            "subscription metadata must not imply live login"
        );
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("signature"));
        assert!(!serialized.contains("SYNTHETIC_REFRESH"));
    }
    // A replaced credential must not leak another workspace's subscription.
    oauth::save(
        &root.join(managed.id.to_string()).join("home/auth.json"),
        &document("different"),
    )
    .await
    .unwrap();
    let list = profiles.list().await;
    assert!(
        list.iter()
            .find(|v| v.profile.id == managed.id)
            .unwrap()
            .plan_type
            .is_none()
    );
    profiles.logout_credentials(external.id).await.unwrap();
    assert!(profiles.list().await.iter().all(|v| v.plan_type.is_none()));
}
#[test]
fn imports_normalize_native_credentials_and_reject_ambiguous_identity() {
    let credentials = oauth::parse(&document("account-a")).unwrap();
    assert_eq!(credentials.identity.account_id, "account-a");
    assert_eq!(credentials.native_document()["auth_mode"], "chatgpt");
    assert!(credentials.native_document()["last_refresh"].is_string());
    let mut native = document("account-a");
    native["last_refresh"] = json!("2026-10-23T00:00:00Z");
    native["tokens"]["oauth_provenance"] =
        json!({"issuer":"https://auth.openai.com","client_id":"synthetic_client"});
    native["tokens"]["login_owner_id"] = json!("synthetic-login");
    let normalized = oauth::parse(&native).unwrap().native_document();
    assert_eq!(normalized["last_refresh"], native["last_refresh"]);
    assert_eq!(
        normalized["tokens"]["oauth_provenance"],
        native["tokens"]["oauth_provenance"]
    );
    assert_eq!(
        normalized["tokens"]["login_owner_id"],
        native["tokens"]["login_owner_id"]
    );
    native["tokens"]["oauth_provenance"]["issuer"] = json!("http://127.0.0.1:9");
    assert_eq!(oauth::parse(&native).err().unwrap().code, "OAUTH_PROVIDER");
    let mut mixed = document("account-a");
    let other = jwt("account-b", codex_relay::now_ms() / 1000 + 3600);
    let mut claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(other.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    claims["https://api.openai.com/auth"]["chatgpt_account_id"] = json!("account-a");
    mixed["tokens"]["id_token"] = json!(format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    ));
    assert_eq!(
        oauth::parse(&mixed).err().unwrap().code,
        "OAUTH_IDENTITY_MISMATCH"
    );
    let mut bad = document("account-a");
    bad["tokens"]["account_id"] = json!("account-b");
    assert_eq!(
        oauth::parse(&bad).err().unwrap().code,
        "OAUTH_IDENTITY_MISMATCH"
    );
    assert!(oauth::parse(&json!({"refreshToken":"only-refresh"})).is_err());
    assert!(oauth::parse(&json!({"accessToken":jwt("a",1)})).is_err());
    assert!(oauth::parse(&json!({"OPENAI_API_KEY":"synthetic"})).is_err());
    assert!(
        oauth::check_identity(
            &credentials.identity,
            &oauth::parse(&document("account-b")).unwrap().identity
        )
        .is_err()
    );
}
#[tokio::test]
async fn imported_accounts_survive_restart_and_logout_only_clears_selected_account() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = Profiles::open(temp.path().join("accounts")).await.unwrap();
    let a = profiles.create("a").await.unwrap();
    let b = profiles.create("b").await.unwrap();
    profiles
        .import_credentials(a.id, document("account-a"))
        .await
        .unwrap();
    profiles
        .import_credentials(b.id, document("account-b"))
        .await
        .unwrap();
    let root = temp.path().join("accounts");
    let catalog_a = root.join(a.id.to_string()).join("home/models_cache.json");
    let catalog_b = root.join(b.id.to_string()).join("home/models_cache.json");
    std::fs::write(&catalog_a, "old-account-catalog").unwrap();
    std::fs::write(&catalog_b, "other-account-catalog").unwrap();
    profiles
        .import_credentials(a.id, document("account-a"))
        .await
        .unwrap();
    assert!(
        !catalog_a.exists(),
        "rebind must invalidate the previous catalog"
    );
    assert!(catalog_b.exists());
    std::fs::write(&catalog_a, "new-account-catalog").unwrap();
    let first = oauth::load(&root.join(a.id.to_string()).join("home/auth.json"))
        .await
        .unwrap();
    assert_eq!(
        oauth::parse(&first).unwrap().identity.account_id,
        "account-a"
    );
    assert!(
        !std::fs::read_to_string(root.join("profiles.json"))
            .unwrap()
            .contains("SYNTHETIC_REFRESH")
    );
    drop(profiles);
    let profiles = Profiles::open(root.clone()).await.unwrap();
    profiles.logout_credentials(a.id).await.unwrap();
    assert!(!catalog_a.exists(), "logout also removes model metadata");
    assert!(catalog_b.exists());
    assert!(!root.join(a.id.to_string()).join("home/auth.json").exists());
    assert!(root.join(b.id.to_string()).join("home/auth.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn account_directories_and_credentials_are_private_and_symlinks_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let profiles = Profiles::open(root.clone()).await.unwrap();
    let account = profiles.create("private").await.unwrap();
    profiles
        .import_credentials(account.id, document("private"))
        .await
        .unwrap();
    let home = root.join(account.id.to_string()).join("home");
    for directory in [&root, &home, &root.join(account.id.to_string())] {
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for name in ["auth.json", "bridge-auth.json"] {
        assert_eq!(
            std::fs::metadata(home.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let link = temp.path().join("linked");
    symlink(&home, &link).unwrap();
    assert_eq!(
        codex_relay::platform::protect_directory(&link)
            .await
            .unwrap_err()
            .code,
        "PRIVATE_DIRECTORY"
    );
}
#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; isolated synthetic credentials only, no real model request"]
async fn native_reads_imported_credentials_and_external_auth_stays_in_its_process() {
    use codex_relay::{backend::Backend, installation::Installation};
    let install: Installation = serde_json::from_slice(
        &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    for name in ["account-a", "account-b"] {
        let home = temp.path().join(name);
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "model_provider=\"openai\"\ncli_auth_credentials_store=\"file\"\nchatgpt_base_url=\"http://127.0.0.1:9\"\n",
        )
        .unwrap();
        let credential = oauth::parse(&document(name)).unwrap();
        oauth::save(&home.join("auth.json"), &credential.native_document())
            .await
            .unwrap();
        let backend = Backend::start_direct(&install, temp.path(), Some(&home))
            .await
            .unwrap();
        let result = oauth::native_credential(&backend).await;
        backend.stop().await.unwrap();
        assert_eq!(result.unwrap().identity.account_id, name);
    }
    let home = temp.path().join("external");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "chatgpt_base_url=\"http://127.0.0.1:9\"\n",
    )
    .unwrap();
    let backend = Backend::start_direct(&install, temp.path(), Some(&home))
        .await
        .unwrap();
    let credential =
        oauth::parse(&json!({"accessToken":jwt("external",codex_relay::now_ms()/1000+3600)}))
            .unwrap();
    let login = backend
        .rpc
        .request("account/login/start", credential.login_params())
        .await
        .unwrap();
    assert_eq!(login["type"], "chatgptAuthTokens");
    let read = oauth::native_credential(&backend).await;
    backend.stop().await.unwrap();
    assert_eq!(read.unwrap().identity.account_id, "external");
    assert!(!home.join("auth.json").exists());
}

#[tokio::test]
async fn admin_import_does_not_return_secrets_and_old_route_requires_reenable() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let state = codex_relay::admin::AdminState::new_with_network(
        4317,
        temp.path().join("accounts"),
        None,
        vec![],
    )
    .await
    .unwrap();
    let account = state.profiles.create("oauth").await.unwrap();
    state
        .operations
        .register_accounts(&[(account.id, "oauth".into())])
        .unwrap();
    let rev = state.operations.snapshot()["revision"].as_u64().unwrap();
    state
        .operations
        .set_inference_route(
            account.id,
            rev,
            Some(codex_relay::provider::Provider {
                execution: Default::default(),
                id: "openai".into(),
                model: "test-model".into(),
                base_url: "https://chatgpt.com/backend-api".into(),
                auth: "native-chatgpt:old".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let app = codex_relay::admin::router(state.clone(), temp.path().join("web"));
    state
        .profiles
        .import_credentials(account.id, document("old-account"))
        .await
        .unwrap();
    let home = temp
        .path()
        .join("accounts")
        .join(account.id.to_string())
        .join("home");
    let old_auth = std::fs::read(home.join("auth.json")).unwrap();
    let old_binding = std::fs::read(home.join("bridge-auth.json")).unwrap();
    // Refuse the TLS CONNECT locally: no real token service or credentials are used.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let received = socket.read(&mut buf).await.unwrap();
            assert!(received > 0);
            socket
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    let saved = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/accounts/{}/proxy", account.id))
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"revision":revision,"config":{"mode":"custom","url":proxy_url}})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while state.operations.snapshot()["accounts"][0]["proxyStatus"]["state"] != "applied" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for input in [
        json!({"refreshToken":"synthetic-import-rt"}),
        json!({"document":{"tokens":{"refresh_token":"synthetic-import-rt"}}}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/accounts/{}/credentials", account.id))
                    .header("host", "127.0.0.1:4317")
                    .header("content-type", "application/json")
                    .body(Body::from(input.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 502);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("synthetic-import-rt"));
        assert!(state.operations.configured_provider(account.id).is_some());
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), old_auth);
        assert_eq!(
            std::fs::read(home.join("bridge-auth.json")).unwrap(),
            old_binding
        );
        assert_eq!(state.operations.snapshot()["active"], 0);
    }
    proxy.await.unwrap();
    let active = state
        .operations
        .begin(Some(account.id), None, "management", "test_busy", false)
        .unwrap();
    let busy = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/accounts/{}/credentials", account.id))
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"refreshToken":"synthetic-import-rt"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(busy.status(), 409);
    active.complete(Ok(())).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/accounts/{}/credentials", account.id))
                .header("host", "127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"document":document("account-a")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("SYNTHETIC_REFRESH"));
    assert!(state.operations.configured_provider(account.id).is_none());
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/accounts/{}/credentials", account.id))
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        !temp
            .path()
            .join("accounts")
            .join(account.id.to_string())
            .join("home/auth.json")
            .exists()
    );
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; synthetic credentials only, no model inference"]
async fn native_access_enables_without_inference_and_diagnostic_failure_preserves_route() {
    use std::{process::Stdio, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".cache")).unwrap();
    std::fs::create_dir(temp.path().join("web")).unwrap();
    std::fs::write(temp.path().join("web/index.html"), "test").unwrap();
    std::fs::copy(
        std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap(),
        temp.path().join(".cache/launch-config.json"),
    )
    .unwrap();
    let port = codex_relay::backend::unused_port().await.unwrap();
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_codex-relay"))
        .args(["serve", "--port", &port.to_string(), "--web-dir", "web"])
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
        .json(&json!({"name":"synthetic-access"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = account["id"].as_str().unwrap();
    let home = temp.path().join("data/accounts").join(id).join("home");
    std::fs::write(
        home.join("config.toml"),
        "model=\"gpt-6.1-sol\"\nmodel_provider=\"openai\"\ncli_auth_credentials_store=\"file\"\n",
    )
    .unwrap();
    client.post(format!("{base}/api/accounts/{id}/credentials")).json(&json!({"document":{"accessToken":jwt("synthetic-access",codex_relay::now_ms()/1000+3600)}})).send().await.unwrap().error_for_status().unwrap();
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
    assert_eq!(
        status, 200,
        "Access must not require inference with a valid upstream token: {enabled}"
    );
    assert!(enabled["accounts"][0]["route"].is_object());
    assert!(
        enabled["accounts"][0]["health"]["lastVerifiedAt"].is_null(),
        "Enabling must not report a model inference"
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
        "Metadata worker must be reaped"
    );
    // Expiry is synthetic; the diagnostic must fail before any upstream model call.
    oauth::save(
        &home.join("bridge-token.json"),
        &json!({"accessToken":jwt("synthetic-access",1)}),
    )
    .await
    .unwrap();
    let response = client
        .post(format!("{base}/api/accounts/{id}/connection-test"))
        .json(&json!({"model":"client-selected-other-model"}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let after: Value = client
        .get(format!("{base}/api/operations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["revision"], enabled["revision"]);
    assert_eq!(
        after["accounts"][0]["route"],
        enabled["accounts"][0]["route"]
    );
    assert_eq!(after["active"], 0);
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
}
