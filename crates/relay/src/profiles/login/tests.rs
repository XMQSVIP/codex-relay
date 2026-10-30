use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn synthetic_token() -> String {
    use base64::Engine;
    let claims = json!({"exp":crate::now_ms()/1000+3600,"sub":"user-test","https://api.openai.com/auth":{"chatgpt_account_id":"test"}});
    format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}
fn scripted_rpc(token: String) -> Rpc {
    use tokio::io::AsyncBufReadExt;
    let (client, server) = tokio::io::duplex(16384);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = tokio::io::BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str() {
                Some("account/read") => {
                    json!({"account":{"type":"chatgpt","email":"test@example.invalid"}})
                }
                Some("getAuthStatus") => json!({"authMethod":"chatgpt","authToken":token}),
                _ => json!({"status":"cancelled"}),
            };
            let line = json!({"id":request["id"],"result":result}).to_string() + "\n";
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    rpc
}
#[tokio::test]
async fn buffered_completion_requires_valid_saved_credentials() {
    for valid in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let token = synthetic_token();
        if valid {
            oauth::save(&temp.path().join("auth.json"), &json!({"tokens":{"access_token":token,"id_token":token,"refresh_token":"synthetic"}})).await.unwrap();
        }
        let rpc = scripted_rpc(token);
        let login_id = Uuid::new_v4().to_string();
        let id = Uuid::new_v4();
        let sessions: Sessions = Default::default();
        let s = prepare_session(&response(&login_id), "chatgpt", rpc.clone()).unwrap();
        let cancel = s.cancel.clone();
        sessions.lock().unwrap().insert(id, s);
        let (send, receive) = tokio::sync::broadcast::channel(8);
        // Notifications buffered before the start response is processed are retained.
        send.send(json!({"method":"account/login/completed","params":{"loginId":"other","success":false}})).unwrap();
        send.send(json!({"method":"account/login/completed","params":{"loginId":login_id,"success":true}})).unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            monitor(
                sessions.clone(),
                id,
                login_id.clone(),
                rpc.clone(),
                temp.path().into(),
                receive,
                cancel,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            session::view(&sessions, id, Some(&login_id)).unwrap()["status"],
            if valid { "succeeded" } else { "failed" }
        );
        rpc.close();
    }
}
#[tokio::test]
async fn old_credentials_without_matching_event_never_succeed() {
    let temp = tempfile::tempdir().unwrap();
    let token = synthetic_token();
    oauth::save(
        &temp.path().join("auth.json"),
        &json!({"tokens":{"access_token":token,"id_token":token,"refresh_token":"synthetic"}}),
    )
    .await
    .unwrap();
    let rpc = scripted_rpc(token);
    let id = Uuid::new_v4();
    let login_id = Uuid::new_v4().to_string();
    let sessions: Sessions = Default::default();
    let mut s = prepare_session(&response(&login_id), "chatgpt", rpc.clone()).unwrap();
    s.view["expiresAt"] = (crate::now_ms() + 30).into();
    let cancel = s.cancel.clone();
    sessions.lock().unwrap().insert(id, s);
    let (send, receive) = tokio::sync::broadcast::channel(8);
    send.send(
        json!({"method":"account/login/completed","params":{"loginId":"old","success":true}}),
    )
    .unwrap();
    monitor(
        sessions.clone(),
        id,
        login_id.clone(),
        rpc.clone(),
        temp.path().into(),
        receive,
        cancel,
    )
    .await;
    let view = session::view(&sessions, id, Some(&login_id)).unwrap();
    assert_eq!(view["status"], "expired");
    assert!(view.get("authUrl").is_none());
    rpc.close();
}
#[tokio::test]
async fn disconnected_process_finishes_instead_of_waiting_forever() {
    let temp = tempfile::tempdir().unwrap();
    let rpc = scripted_rpc(synthetic_token());
    let id = Uuid::new_v4();
    let login_id = Uuid::new_v4().to_string();
    let sessions: Sessions = Default::default();
    let s = prepare_session(&response(&login_id), "chatgpt", rpc.clone()).unwrap();
    let cancel = s.cancel.clone();
    sessions.lock().unwrap().insert(id, s);
    let events = rpc.subscribe();
    rpc.close();
    monitor(
        sessions.clone(),
        id,
        login_id.clone(),
        rpc,
        temp.path().into(),
        events,
        cancel,
    )
    .await;
    assert_eq!(
        session::view(&sessions, id, Some(&login_id)).unwrap()["error"]["code"],
        "LOGIN_DISCONNECTED"
    );
}

fn response(login_id: &str) -> Value {
    json!({"type":"chatgpt","loginId":login_id,"authUrl":"https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&state=secret"})
}

#[tokio::test]
async fn management_rejects_conflicts_and_cross_account_callbacks_and_stop_cancels() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let state = crate::admin::AdminState::new_with_network(
        4317,
        temp.path().join("accounts"),
        None,
        vec![],
    )
    .await
    .unwrap();
    let account = state.profiles.create("pending").await.unwrap();
    let other = state.profiles.create("other").await.unwrap();
    state
        .operations
        .register_accounts(&[(account.id, account.name), (other.id, other.name)])
        .unwrap();
    let login_id = Uuid::new_v4().to_string();
    let rpc = scripted_rpc(synthetic_token());
    state.profiles.logins.sessions.lock().unwrap().insert(
        account.id,
        prepare_session(&response(&login_id), "chatgpt", rpc.clone()).unwrap(),
    );
    let app = crate::admin::router(state.clone(), temp.path().join("web"));
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    for (suffix, body) in [
        ("activation", json!({"revision":revision,"enabled":true})),
        ("credentials", json!({"document":{}})),
        ("restart", json!({})),
        (
            "proxy",
            json!({"revision":revision,"config":{"mode":"direct","url":""}}),
        ),
        ("login", json!({"kind":"chatgptDeviceCode"})),
    ] {
        let reply = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/accounts/{}/{suffix}", account.id))
                    .header("host", "127.0.0.1:4317")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::CONFLICT, "{suffix}");
    }
    let reply=app.clone().oneshot(Request::builder().method("POST")
        .uri(format!("/api/accounts/{}/login/callback",other.id)).header("host","127.0.0.1:4317")
        .header("content-type","application/json").body(Body::from(json!({"loginId":login_id,"callbackUrl":"http://127.0.0.1:1455/auth/callback?code=test&state=secret"}).to_string())).unwrap()).await.unwrap();
    assert_eq!(reply.status(), StatusCode::GONE);
    let reply = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/accounts/{}/stop", account.id))
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = reply.status();
    let body = to_bytes(reply.into_body(), 8192).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        state
            .profiles
            .login_status(account.id, Some(&login_id))
            .await
            .unwrap()["status"],
        "cancelled"
    );
    assert!(state.operations.configured_provider(account.id).is_none());
    rpc.close();
}
fn rpc() -> Rpc {
    let (a, b) = tokio::io::duplex(4096);
    Rpc::from_io(a, b)
}
#[tokio::test]
async fn session_is_correlated_terminal_and_redacted() {
    let id = Uuid::new_v4();
    let login = Uuid::new_v4().to_string();
    let sessions: Sessions = Default::default();
    sessions.lock().unwrap().insert(
        id,
        prepare_session(&response(&login), "chatgpt", rpc()).unwrap(),
    );
    session::finish(&sessions, id, "wrong-login", "succeeded", None);
    assert_eq!(
        session::view(&sessions, id, Some(&login)).unwrap()["status"],
        "waiting"
    );
    assert!(session::view(&sessions, Uuid::new_v4(), Some(&login)).is_err());
    session::finish(&sessions, id, &login, "cancelled", None);
    session::finish(&sessions, id, &login, "succeeded", None);
    let view = session::view(&sessions, id, Some(&login)).unwrap();
    assert_eq!(view["status"], "cancelled");
    assert!(view.get("authUrl").is_none());
    assert!(sessions.lock().unwrap()[&id].callback.is_none());
}
#[test]
fn callback_rejects_wrong_state_duplicates_targets_and_bare_codes() {
    let spec =
        callback::Callback::from_auth(response("unused")["authUrl"].as_str().unwrap()).unwrap();
    let good = "http://127.0.0.1:1455/auth/callback?code=one&state=secret";
    assert!(spec.validate(good).is_ok());
    for bad in [
        good.replace("secret", "wrong"),
        format!("{good}&state=secret"),
        format!("{good}&code=two"),
        good.replace("127.0.0.1", "example.com"),
        good.replace("1455", "1457"),
        good.replace("/auth/callback", "/cancel"),
        format!("{good}&error=denied"),
        "one".into(),
        "x".repeat(16385),
    ] {
        assert!(spec.validate(&bad).is_err());
    }
    assert!(
        spec.validate("http://127.0.0.1:1455/auth/callback?error=denied&state=secret")
            .is_ok()
    );
}
#[test]
fn nested_authorization_and_unrecognized_callback() {
    let inner = response("unused")["authUrl"].as_str().unwrap().to_string();
    let mut outer = url::Url::parse("https://chatgpt.com/codex/desktop-auth").unwrap();
    outer.query_pairs_mut().append_pair("authorize_url", &inner);
    assert!(callback::Callback::from_auth(outer.as_str()).is_ok());
    assert!(callback::Callback::from_auth(&inner.replace("1455", "80")).is_err());
}
#[tokio::test]
async fn forwarding_follows_only_local_success_once() {
    for external in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let size = stream.read(&mut bytes).await.unwrap();
            assert!(String::from_utf8_lossy(&bytes[..size]).starts_with("GET /auth/callback?"));
            let location = if external {
                "https://example.com/success"
            } else {
                "/success"
            };
            stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            drop(stream);
            if !external {
                let (mut stream, _) = listener.accept().await.unwrap();
                let size = stream.read(&mut bytes).await.unwrap();
                assert!(String::from_utf8_lossy(&bytes[..size]).starts_with("GET /success "));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let result = callback::forward(
            url::Url::parse(&format!("http://{address}/auth/callback?code=test")).unwrap(),
        )
        .await;
        assert_eq!(result.is_err(), external);
        task.await.unwrap();
    }
}
#[tokio::test]
async fn account_conflicts_callback_replay_and_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = Profiles::open(temp.path().join("accounts")).await.unwrap();
    let account = profiles.create("login test").await.unwrap();
    let login = Uuid::new_v4().to_string();
    let mut pending = prepare_session(&response(&login), "chatgpt", rpc()).unwrap();
    pending.submitted = true;
    profiles
        .logins
        .sessions
        .lock()
        .unwrap()
        .insert(account.id, pending);
    assert_eq!(
        profiles.require_no_login(account.id).unwrap_err().code,
        "BUSY"
    );
    assert_eq!(
        profiles
            .submit_login_callback(account.id, &login, "unused")
            .await
            .unwrap_err()
            .code,
        "BUSY"
    );
    assert_eq!(
        profiles
            .cancel_login(account.id, &Uuid::new_v4().to_string())
            .await
            .unwrap_err()
            .code,
        "LOGIN_EXPIRED"
    );
    profiles.cancel_login(account.id, &login).await.unwrap();
    profiles.cancel_login(account.id, &login).await.unwrap();
    assert!(profiles.require_no_login(account.id).is_ok());
    assert_eq!(
        profiles
            .login_status(account.id, Some(&login))
            .await
            .unwrap()["status"],
        "cancelled"
    );
}
