use codex_relay::{
    admin::AdminState,
    network::{AccessPolicy, client_router},
    operations::{ApiKeyCreate, DEFAULT_GROUP},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
async fn setup() -> (
    tempfile::TempDir,
    Arc<AdminState>,
    String,
    String,
    tokio::task::JoinHandle<()>,
) {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "test".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["a".into()],
            expires_in_days: None,
            max_concurrency: 1,
        })
        .unwrap()
        .secret;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let policy = AccessPolicy::new(&[format!("http://{address}/v1")], &[]).unwrap();
    let app = client_router(state.clone(), policy);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        temp,
        state,
        key,
        format!("ws://{address}/v1/native"),
        server,
    )
}
#[tokio::test]
async fn native_websocket_requires_key_and_rejects_model_scope_before_dispatch() {
    let (_temp, state, key, url, server) = setup().await;
    let error = connect_async(&url).await.err().unwrap();
    assert!(matches!(error,tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status()==401));
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket
        .send(Message::Text(
            json!({"id":1,"method":"thread/start","params":{"model":"forbidden"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v["params"]["code"], "MODEL_NOT_ALLOWED");
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.profiles.list().await.is_empty());
    server.abort();
}
#[tokio::test]
async fn queued_native_socket_disconnect_releases_waiter_without_starting_backend() {
    let (_temp, state, key, url, server) = setup().await;
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
                id: "p".into(),
                model: "a".into(),
                base_url: "http://192.168.1.10/v1".into(),
                auth: "none".into(),
                required_headers: vec![],
                forward_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    let lease = state
        .operations
        .acquire_client(id, &state.operations.authenticate_api_key(&key).unwrap())
        .await
        .unwrap();
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket
        .send(Message::Text(
            json!({"id":1,"method":"thread/start","params":{"model":"a"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.operations.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(socket);
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.operations.snapshot()["queued"] != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    lease.complete(Ok(())).unwrap();
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.profiles.list().await.is_empty());
    server.abort();
}
