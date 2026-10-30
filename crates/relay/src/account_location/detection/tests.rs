use super::*;
use crate::app_server_proxy::ProxyMode;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn detects_deduplicates_caches_and_rejects_stale_results() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app=axum::Router::new().route("/", axum::routing::get(move || {let c=c.clone(); async move {
        c.fetch_add(1,Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        axum::Json(json!({"success":true,"country_code":"JP","region":"Tokyo","city":"Tokyo","timezone":{"id":"Asia/Tokyo"}}))
    }}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut manager = Manager::open(dir.path()).unwrap();
    Arc::get_mut(&mut manager).unwrap().trace_endpoint = None;
    Arc::get_mut(&mut manager).unwrap().endpoints =
        vec![(Provider::IpWhois, format!("http://127.0.0.1:{port}/"))];
    let config = Config {
        enabled: true,
        ..Default::default()
    };
    let proxy = ProxyConfig {
        mode: ProxyMode::Direct,
        url: String::new(),
    };
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let (first, second) = tokio::join!(
        manager.resolve(a, 0, config.clone(), proxy.clone(), false),
        manager.resolve(b, 0, config.clone(), proxy.clone(), false)
    );
    assert_eq!(first, second);
    assert_eq!(first.unwrap().timezone, "Asia/Tokyo");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    manager
        .resolve(a, 0, config.clone(), proxy.clone(), false)
        .await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    manager.invalidate(a, true);
    assert!(
        manager
            .resolve(a, 0, config.clone(), proxy.clone(), false)
            .await
            .is_none()
    );
    assert!(manager.view(a, &config)["effective"].is_null());
    let stopped = Config::default();
    manager.resolve(a, 1, stopped, proxy, false).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(manager);
    let reopened = Manager::open(dir.path()).unwrap();
    assert_eq!(
        reopened.view(b, &config)["effective"]["timezone"],
        "Asia/Tokyo"
    );
    assert!(reopened.view(a, &config)["effective"].is_null());
    server.abort();
}
#[test]
fn malformed_or_unsuccessful_detection_cannot_supply_location() {
    for data in [
        b"null".as_slice(),
        br#"{"success":false}"#,
        br#"{"success":true,"timezone":{"id":"bad"}}"#,
    ] {
        assert!(parse(Provider::IpWhois, data).is_err());
    }
}

#[tokio::test]
async fn custom_proxy_is_used_and_failures_keep_only_matching_cache() {
    use std::sync::atomic::AtomicBool;
    let failing = Arc::new(AtomicBool::new(false));
    let fail = failing.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app=axum::Router::new().fallback(move |uri: axum::http::Uri| {let fail=fail.clone();let c=c.clone();async move {
        assert_eq!(uri.host(),Some("geo.example.invalid"),"absolute proxy request, not a direct localhost lookup");
        c.fetch_add(1,Ordering::SeqCst);
        if fail.load(Ordering::SeqCst) { axum::Json(json!({"success":false})) }
        else { axum::Json(json!({"success":true,"country_code":"US","city":"New York","timezone":{"id":"America/New_York"}})) }
    }});
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut manager = Manager::open(dir.path()).unwrap();
    Arc::get_mut(&mut manager).unwrap().trace_endpoint = None;
    Arc::get_mut(&mut manager).unwrap().endpoints =
        vec![(Provider::IpWhois, "http://geo.example.invalid/".into())];
    let id = Uuid::new_v4();
    let config = Config {
        enabled: true,
        ..Default::default()
    };
    let proxy = ProxyConfig {
        mode: ProxyMode::Custom,
        url: format!("http://127.0.0.1:{port}"),
    };
    let first = manager
        .resolve(id, 0, config.clone(), proxy.clone(), true)
        .await
        .unwrap();
    failing.store(true, Ordering::SeqCst);
    // Expire only the shared short deduplication window; preserve this account's last success.
    manager.probes.lock().unwrap().clear();
    assert_eq!(
        manager
            .resolve(id, 0, config.clone(), proxy.clone(), true)
            .await
            .unwrap(),
        first
    );
    assert_eq!(manager.view(id, &config)["state"], "cached");
    manager.resolve(id, 0, config.clone(), proxy, false).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "automatic failure retries are throttled"
    );
    manager.invalidate(id, true);
    assert!(manager.view(id, &config)["effective"].is_null());
    server.abort();
}

#[tokio::test]
async fn cancellation_and_configuration_change_cannot_leave_busy_or_publish_old_results() {
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let start = started.clone();
    let unblock = gate.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(move || {
            let start = start.clone();
            let unblock = unblock.clone();
            async move {
                start.notify_one();
                unblock.notified().await;
                axum::Json(json!({"success":true,"timezone":{"id":"Asia/Tokyo"}}))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut manager = Manager::open(dir.path()).unwrap();
    Arc::get_mut(&mut manager).unwrap().trace_endpoint = None;
    Arc::get_mut(&mut manager).unwrap().endpoints =
        vec![(Provider::IpWhois, format!("http://127.0.0.1:{port}/"))];
    let id = Uuid::new_v4();
    let config = Config {
        enabled: true,
        ..Default::default()
    };
    let cfg = config.clone();
    let owner = manager.clone();
    let task = tokio::spawn(async move {
        owner
            .resolve(
                id,
                0,
                cfg,
                ProxyConfig {
                    mode: ProxyMode::Direct,
                    url: String::new(),
                },
                false,
            )
            .await
    });
    started.notified().await;
    task.abort();
    manager.invalidate(id, true);
    gate.notify_one();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(manager.view(id, &config)["state"], "pending");
    assert!(manager.view(id, &config)["effective"].is_null());
    server.abort();
}

#[test]
fn ipinfo_requires_a_valid_timezone_and_accepts_real_chinese_exits() {
    let location = parse(
        Provider::Ipinfo,
        br#"{"country":"cn","region":"Shanghai","city":"Shanghai","timezone":"Asia/Shanghai"}"#,
    )
    .unwrap();
    assert_eq!(location.country, "CN");
    assert_eq!(location.timezone, "Asia/Shanghai");
    for data in [
        b"null".as_slice(),
        br#"{"country":"US"}"#,
        br#"{"timezone":"bad"}"#,
        br#"{"error":{"title":"Rate limit"},"timezone":"Etc/UTC"}"#,
        br#"{"bogon":true,"timezone":"Etc/UTC"}"#,
        br#"{"country":"ZZ","timezone":"Etc/UTC"}"#,
    ] {
        assert!(parse(Provider::Ipinfo, data).is_err());
    }
}

#[tokio::test]
async fn preferred_service_and_fallback_both_follow_the_account_proxy() {
    use axum::{http::StatusCode, response::IntoResponse};
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
        let captured = captured.clone();
        async move {
            assert_eq!(uri.host(), Some("geo.example.invalid"));
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("cookie"));
            captured.lock().unwrap().push(uri.path().to_owned());
            match uri.path() {
                "/preferred" => axum::Json(json!({"country":"JP","timezone":"Asia/Tokyo"})).into_response(),
                "/limited" => StatusCode::TOO_MANY_REQUESTS.into_response(),
                "/invalid" => axum::Json(json!({"country":"JP"})).into_response(),
                "/oversized" => "x".repeat(64 * 1024 + 1).into_response(),
                "/slow" => {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    axum::Json(json!({"country":"JP","timezone":"Asia/Tokyo"})).into_response()
                }
                "/fallback" => axum::Json(json!({"success":true,"country_code":"US","timezone":{"id":"America/New_York"}})).into_response(),
                _ => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let proxy = ProxyConfig {
        mode: ProxyMode::Custom,
        url: format!("http://127.0.0.1:{port}"),
    };
    for primary in ["preferred", "limited", "invalid", "oversized", "slow"] {
        requests.lock().unwrap().clear();
        let dir = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(dir.path()).unwrap();
        Arc::get_mut(&mut manager).unwrap().trace_endpoint = None;
        Arc::get_mut(&mut manager).unwrap().endpoints = vec![
            (
                Provider::Ipinfo,
                format!("http://geo.example.invalid/{primary}"),
            ),
            (
                Provider::IpWhois,
                "http://geo.example.invalid/fallback".into(),
            ),
        ];
        let location = manager
            .resolve(
                Uuid::new_v4(),
                0,
                Config {
                    enabled: true,
                    ..Default::default()
                },
                proxy.clone(),
                true,
            )
            .await
            .unwrap();
        let actual = requests.lock().unwrap().clone();
        if primary == "preferred" {
            assert_eq!(location.timezone, "Asia/Tokyo");
            assert_eq!(actual, vec!["/preferred"]);
        } else {
            assert_eq!(location.timezone, "America/New_York");
            assert_eq!(actual, vec![format!("/{primary}"), "/fallback".into()]);
        }
    }
    server.abort();
}

#[test]
fn old_ipwhois_only_cache_is_not_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    {
        let manager = Manager::open(dir.path()).unwrap();
        let mut state = manager.state.lock().unwrap();
        state.entries.insert(
            id,
            Entry {
                key: "old-proxy-fingerprint".into(),
                location: Some(Location {
                    timezone: "Asia/Shanghai".into(),
                    ..Default::default()
                }),
                detected_at: crate::now_ms(),
                ..Default::default()
            },
        );
        persist(&state, id);
    }
    let reopened = Manager::open(dir.path()).unwrap();
    let status = reopened.view(
        id,
        &Config {
            enabled: true,
            ..Default::default()
        },
    );
    assert_eq!(status["state"], "pending");
    assert!(status["effective"].is_null());
}

impl Manager {
    pub(crate) async fn preview_fixture(
        &self,
        id: Uuid,
        proxy: ProxyConfig,
        location: Location,
    ) -> Preview {
        let preview = Preview {
            id: Uuid::new_v4(),
            location,
            detected_at: crate::now_ms(),
            key: format!(
                "{CACHE_PREFIX}{}",
                proxy.resolved().await.unwrap().fingerprint()
            ),
            generation: self.token(id),
        };
        self.state
            .lock()
            .unwrap()
            .previews
            .insert(id, preview.clone());
        preview
    }
}

#[test]
fn trace_requires_one_valid_ip_instead_of_country_or_html() {
    assert_eq!(
        parse_trace(b"ip=8.8.8.8\nloc=US\n").unwrap().to_string(),
        "8.8.8.8"
    );
    assert_eq!(
        parse_trace(b"ip=2606:4700:4700::1111\n")
            .unwrap()
            .to_string(),
        "2606:4700:4700::1111"
    );
    for bytes in [
        b"<html>login</html>".as_slice(),
        b"loc=US",
        b"ip=localhost",
        b"ip=127.0.0.1",
        b"ip=8.8.8.8\nip=1.1.1.1",
    ] {
        assert!(parse_trace(bytes).is_err());
    }
}

#[tokio::test]
async fn restricted_site_exit_is_used_for_both_lookups_without_credentials() {
    use axum::{http::StatusCode, response::IntoResponse};
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
        let captured = captured.clone();
        async move {
            assert_eq!(uri.host(), Some("probe.example.invalid"));
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("cookie"));
            captured.lock().unwrap().push(uri.path().to_owned());
            match uri.path() {
                "/trace" => "ip=8.8.8.8\nloc=US\n".into_response(),
                "/trace-failed" => StatusCode::FORBIDDEN.into_response(),
                "/primary/8.8.8.8" => axum::Json(json!({"ip":"8.8.8.8","country":"US","timezone":"America/New_York"})).into_response(),
                "/wrong-ip/8.8.8.8" => axum::Json(json!({"ip":"1.1.1.1","country":"CN","timezone":"Asia/Shanghai"})).into_response(),
                "/fallback/8.8.8.8" => axum::Json(json!({"ip":"8.8.8.8","success":true,"country_code":"US","timezone":{"id":"America/New_York"}})).into_response(),
                "/fallback/" => axum::Json(json!({"success":true,"country_code":"CN","timezone":{"id":"Asia/Shanghai"}})).into_response(),
                _ => StatusCode::TOO_MANY_REQUESTS.into_response(),
            }
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let proxy = ProxyConfig {
        mode: ProxyMode::Custom,
        url: format!("http://127.0.0.1:{port}"),
    };
    for (trace, primary, expected, paths) in [
        (
            "trace",
            "primary",
            "America/New_York",
            vec!["/trace", "/primary/8.8.8.8"],
        ),
        (
            "trace",
            "limited",
            "America/New_York",
            vec!["/trace", "/limited/8.8.8.8", "/fallback/8.8.8.8"],
        ),
        (
            "trace",
            "wrong-ip",
            "America/New_York",
            vec!["/trace", "/wrong-ip/8.8.8.8", "/fallback/8.8.8.8"],
        ),
        (
            "trace-failed",
            "primary",
            "Asia/Shanghai",
            vec!["/trace-failed", "/fallback/"],
        ),
    ] {
        requests.lock().unwrap().clear();
        let dir = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(dir.path()).unwrap();
        let owner = Arc::get_mut(&mut manager).unwrap();
        owner.trace_endpoint = Some(format!("http://probe.example.invalid/{trace}"));
        owner.endpoints = vec![
            (
                Provider::Ipinfo,
                format!("http://probe.example.invalid/{primary}/{{ip}}"),
            ),
            (
                Provider::IpWhois,
                "http://probe.example.invalid/fallback/{ip}".into(),
            ),
        ];
        let id = Uuid::new_v4();
        let config = Config {
            enabled: true,
            ..Default::default()
        };
        let before = manager.view(id, &config);
        let preview = manager.preview(id, 0, proxy.clone()).await.unwrap();
        assert_eq!(preview.location.timezone, expected);
        assert_eq!(
            manager.view(id, &config),
            before,
            "preview must not change effective state"
        );
        assert_eq!(*requests.lock().unwrap(), paths);
        assert_eq!(
            Manager::open(dir.path()).unwrap().view(id, &config),
            before,
            "preview is not persisted"
        );
        let accepted = manager
            .preview_for_save(id, preview.id, proxy.clone())
            .await
            .unwrap();
        manager.accept_preview(id, accepted);
        assert_eq!(manager.view(id, &config)["effective"]["timezone"], expected);
        assert!(
            manager
                .preview_for_save(id, preview.id, proxy.clone())
                .await
                .is_err()
        );
    }
    server.abort();
}

#[tokio::test]
async fn previews_reject_other_accounts_expiry_and_changed_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::open(dir.path()).unwrap();
    let id = Uuid::new_v4();
    let proxy = ProxyConfig {
        mode: ProxyMode::Direct,
        url: String::new(),
    };
    let location = Location {
        timezone: "Asia/Tokyo".into(),
        ..Default::default()
    };
    let preview = manager
        .preview_fixture(id, proxy.clone(), location.clone())
        .await;
    assert!(
        manager
            .preview_for_save(Uuid::new_v4(), preview.id, proxy.clone())
            .await
            .is_err()
    );
    let changed = ProxyConfig {
        mode: ProxyMode::Custom,
        url: "http://127.0.0.1:4321".into(),
    };
    assert!(
        manager
            .preview_for_save(id, preview.id, changed)
            .await
            .is_err()
    );
    manager
        .state
        .lock()
        .unwrap()
        .previews
        .get_mut(&id)
        .unwrap()
        .detected_at -= PREVIEW_TTL;
    assert!(
        manager
            .preview_for_save(id, preview.id, proxy.clone())
            .await
            .is_err()
    );
    let preview = manager.preview_fixture(id, proxy.clone(), location).await;
    manager.invalidate(id, false);
    assert!(
        manager
            .preview_for_save(id, preview.id, proxy)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn confirming_preview_prevents_an_older_probe_from_overwriting_it() {
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let start = started.clone();
    let unblock = gate.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(move || {
            let start = start.clone();
            let unblock = unblock.clone();
            async move {
                start.notify_one();
                unblock.notified().await;
                axum::Json(json!({"success":true,"timezone":{"id":"America/New_York"}}))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut manager = Manager::open(dir.path()).unwrap();
    let owner = Arc::get_mut(&mut manager).unwrap();
    owner.trace_endpoint = None;
    owner.endpoints = vec![(Provider::IpWhois, format!("http://127.0.0.1:{port}/"))];
    let id = Uuid::new_v4();
    let config = Config {
        enabled: true,
        ..Default::default()
    };
    let proxy = ProxyConfig {
        mode: ProxyMode::Direct,
        url: String::new(),
    };
    let owner = manager.clone();
    let cfg = config.clone();
    let route = proxy.clone();
    let task = tokio::spawn(async move { owner.resolve(id, 0, cfg, route, true).await });
    started.notified().await;
    let preview = manager
        .preview_fixture(
            id,
            proxy,
            Location {
                timezone: "Asia/Tokyo".into(),
                ..Default::default()
            },
        )
        .await;
    manager.accept_preview(id, preview);
    gate.notify_one();
    assert!(task.await.unwrap().is_none());
    assert_eq!(
        manager.view(id, &config)["effective"]["timezone"],
        "Asia/Tokyo"
    );
    server.abort();
}
