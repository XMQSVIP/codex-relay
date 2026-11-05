use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use codex_relay::{
    admin::{AdminState, router},
    app_server_proxy::{ProxyConfig, ProxyMode},
    operations::Operations,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
async fn setup() -> (tempfile::TempDir, Arc<AdminState>, axum::Router, Uuid) {
    let dir = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, dir.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let profile = state.profiles.create("proxy A").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    let app = router(state.clone(), dir.path().join("web"));
    (dir, state, app, profile.id)
}
async fn post(
    app: &axum::Router,
    path: &str,
    auth: &str,
    origin: &str,
    body: Value,
) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("origin", origin)
                .header("authorization", format!("Bearer {auth}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 100_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn save(
    app: &axum::Router,
    state: &AdminState,
    id: Uuid,
    mode: &str,
    url: &str,
) -> (u16, Value) {
    post(
        app,
        &format!("/api/accounts/{id}/proxy"),
        &"a".repeat(64),
        "http://127.0.0.1:4317",
        json!({"revision":revision(&state.operations), "config":{"mode":mode,"url":url}}),
    )
    .await
}
async fn applied(state: &AdminState, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = state.operations.snapshot();
            let account = snapshot["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["id"] == id.to_string())
                .unwrap();
            if account["proxyStatus"]["state"] == "applied" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn account_proxy_api_is_admin_only_validated_revision_checked_and_persistent() {
    let (dir, state, app, id) = setup().await;
    let rev = revision(&state.operations);
    let path = format!("/api/accounts/{id}/proxy");
    let token = "a".repeat(64);
    for (auth, origin, url, expected) in [
        ("bad", "http://127.0.0.1:4317", "http://127.0.0.1:7897", 403),
        (
            token.as_str(),
            "https://evil.invalid",
            "http://127.0.0.1:7897",
            403,
        ),
        (
            token.as_str(),
            "http://127.0.0.1:4317",
            "file:///tmp/proxy",
            400,
        ),
        (
            token.as_str(),
            "http://127.0.0.1:4317",
            "http://127.0.0.1:7897",
            200,
        ),
        (
            token.as_str(),
            "http://127.0.0.1:4317",
            "http://127.0.0.1:9999",
            409,
        ),
    ] {
        let (status, result) = post(
            &app,
            &path,
            auth,
            origin,
            json!({"revision":rev,"config":{"mode":"custom","url":url}}),
        )
        .await;
        assert_eq!(status, expected, "{result}");
    }
    applied(&state, id).await;
    assert_eq!(
        state.operations.account_proxy(id).url,
        "http://127.0.0.1:7897"
    );
    assert_eq!(
        state.profiles.list().await[0].pid,
        None,
        "a stopped account must stay stopped"
    );
    assert_eq!(save(&app, &state, Uuid::new_v4(), "auto", "").await.0, 404);
    assert_eq!(
        save(
            &app,
            &state,
            codex_relay::operations::DESKTOP_ACCOUNT,
            "auto",
            ""
        )
        .await
        .0,
        400
    );
    let legacy = post(
        &app,
        "/api/settings/app-server-proxy",
        &token,
        "http://127.0.0.1:4317",
        json!({"revision":rev,"config":{"mode":"auto"}}),
    )
    .await;
    assert!(matches!(legacy.0, 404 | 405));
    drop(app);
    drop(state);
    let ops = Operations::open(&dir.path().join("accounts/operations")).unwrap();
    assert_eq!(ops.account_proxy(id).url, "http://127.0.0.1:7897");
}

#[tokio::test]
async fn saving_while_busy_applies_immediately_without_waiting_for_the_active_lease() {
    let (_dir, state, app, id) = setup().await;
    let other = state.profiles.create("proxy B").await.unwrap();
    state
        .operations
        .register_accounts(&[(other.id, other.name)])
        .unwrap();
    let lease = state
        .operations
        .begin(Some(id), None, "inference", "test", true)
        .unwrap();
    assert_eq!(
        save(&app, &state, id, "custom", "http://127.0.0.1:7897")
            .await
            .0,
        200
    );
    applied(&state, id).await;
    assert_eq!(
        state.operations.snapshot()["accounts"][0]["active"],
        1,
        "proxy apply must not wait for this lease"
    );
    assert_eq!(
        state.operations.account_proxy(id).url,
        "http://127.0.0.1:7897"
    );
    assert_eq!(
        save(&app, &state, id, "custom", "http://127.0.0.1:7898")
            .await
            .0,
        200
    );
    applied(&state, id).await;
    assert_eq!(save(&app, &state, other.id, "direct", "").await.0, 200);
    applied(&state, other.id).await;
    let other_lease = state
        .operations
        .begin(Some(other.id), None, "inference", "test", true)
        .unwrap();
    assert!(state.operations.account_proxy(other.id).mode == ProxyMode::Direct);
    lease.complete(Ok(())).unwrap();
    other_lease.complete(Ok(())).unwrap();
    assert_eq!(
        state.operations.account_proxy(id).url,
        "http://127.0.0.1:7898"
    );
    assert_eq!(state.profiles.list().await[0].pid, None);
}

#[test]
fn proxy_validation_rejects_credentials_and_non_proxy_urls() {
    for url in [
        "",
        "127.0.0.1:7897",
        "socks5://127.0.0.1:1080",
        "http://user:secret@host:80",
        "http://host/path",
        "http://host?x=1",
        "http://host#x",
        "http://host:0",
        "http://ho\nst:80",
    ] {
        assert!(
            ProxyConfig {
                mode: ProxyMode::Custom,
                url: url.into()
            }
            .validated()
            .is_err()
        );
    }
    for url in [
        "http://127.0.0.1:7897",
        "https://proxy.example:8443",
        "http://[::1]:7897",
    ] {
        assert!(
            ProxyConfig {
                mode: ProxyMode::Custom,
                url: url.into()
            }
            .validated()
            .is_ok()
        );
    }
    let config = ProxyConfig {
        mode: ProxyMode::Direct,
        url: "old proxy".into(),
    }
    .validated()
    .unwrap();
    assert!(config.url.is_empty());
}

#[tokio::test]
async fn explicit_proxy_overrides_child_aliases_and_bypass_without_mutating_parent() {
    let parent = std::env::var_os("HTTP_PROXY");
    for mode in [ProxyMode::Custom, ProxyMode::Direct] {
        let mut command = tokio::process::Command::new("unused");
        for key in ["http_proxy", "HTTPS_PROXY", "aLl_PrOxY", "no_proxy"] {
            command.env(key, "inherited.example");
        }
        ProxyConfig {
            mode,
            url: "http://127.0.0.1:7897".into(),
        }
        .apply(&mut command)
        .await
        .unwrap();
        let values: std::collections::HashMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                // Windows merges aliases and can retain the first spelling.
                // Unix keeps differently cased variables as separate entries.
                let key = key.to_string_lossy();
                let key = if cfg!(windows) {
                    key.to_ascii_uppercase()
                } else {
                    key.into_owned()
                };
                (key, value.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        assert_eq!(values["ALL_PROXY"], None);
        #[cfg(not(windows))]
        for key in [
            "http_proxy",
            "https_proxy",
            "aLl_PrOxY",
            "all_proxy",
            "no_proxy",
        ] {
            assert_eq!(values[key], None);
        }
        if mode == ProxyMode::Custom {
            assert_eq!(
                values["HTTP_PROXY"].as_deref(),
                Some("http://127.0.0.1:7897")
            );
            assert_eq!(
                values["HTTPS_PROXY"].as_deref(),
                Some("http://127.0.0.1:7897")
            );
            assert_eq!(
                values["NO_PROXY"].as_deref(),
                Some("127.0.0.1,localhost,::1")
            );
        } else {
            assert_eq!(values["HTTP_PROXY"], None);
            assert_eq!(values["HTTPS_PROXY"], None);
            assert_eq!(values["NO_PROXY"].as_deref(), Some("*"));
        }
    }
    assert_eq!(std::env::var_os("HTTP_PROXY"), parent);
}
