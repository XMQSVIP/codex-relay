use codex_relay::{
    admin::AdminState,
    network::{AccessPolicy, client_router},
    operations::{ApiKeyCreate, DEFAULT_GROUP},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

#[tokio::test]
async fn subagent_labels_are_scoped_to_each_websocket_request() {
    let root = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, root.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    use tower::ServiceExt;
    let admin = codex_relay::admin::router(state.clone(), root.path().join("web"));
    let enabled = admin
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/settings/debug")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {}", "a".repeat(64)))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"enabled":true,"revision":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), axum::http::StatusCode::OK);
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"subagent ws","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = client_router(
        state.clone(),
        AccessPolicy::new(&[format!("http://{address}/v1")], &[]).unwrap(),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for header in [None, Some("guardian")] {
        let mut request = format!("ws://{address}/v1/responses")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", key.secret).parse().unwrap(),
        );
        if let Some(kind) = header {
            request
                .headers_mut()
                .insert("x-openai-subagent", kind.parse().unwrap());
        }
        let (mut socket, _) = connect_async(request).await.unwrap();
        for (index, kind) in [Some("worker"), None, Some("review")]
            .into_iter()
            .enumerate()
        {
            let model = format!("model-{}-{index}", header.unwrap_or("plain"));
            let expected_effort = if index == 1 { "medium" } else { "high" };
            let mut frame = json!({"type":"response.create","model":model,"reasoning":{"effort":"low","context":"all_turns"},"input":[
                {"role":"user","content":"hi"},
                {"type":"configuration_update","reasoning":{"effort":expected_effort}}
            ]});
            if let Some(kind) = kind {
                frame["client_metadata"] =
                    json!({"x-codex-turn-metadata":json!({"subagent_kind":kind}).to_string()});
            }
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
            let event = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let event: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
            assert_eq!(event["type"], "error");
            let data = state.operations.statistics(Default::default()).unwrap();
            let row = data["records"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["model"] == model)
                .unwrap();
            assert_eq!(row["subagentKind"], json!(kind.or(header)));
            assert_eq!(row["reasoningEffort"], expected_effort);
            let content = state
                .operations
                .request_content(row["id"].as_str().unwrap().parse().unwrap())
                .unwrap();
            let captured: Value =
                serde_json::from_str(content["request"]["body"].as_str().unwrap()).unwrap();
            assert_eq!(captured, frame);
            assert_eq!(content["response"]["transport"], "websocket");
            assert!(
                content["response"]["body"]
                    .as_str()
                    .unwrap()
                    .contains("error")
            );
        }
        socket.close(None).await.unwrap();
    }
    server.abort();
}

#[tokio::test]
async fn prewarm_lanes_replay_errors_and_key_isolation_without_native_or_desktop() {
    let root = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, root.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "ordinary API client".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["m".into()],
            expires_in_days: None,
            max_concurrency: 2,
        })
        .unwrap()
        .secret;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = client_router(
        state.clone(),
        AccessPolicy::new(&[format!("http://{address}/v1")], &[]).unwrap(),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("ws://{address}/v1/responses");
    assert!(connect_async(&url).await.is_err());
    let mut request = url.clone().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut socket, _) = connect_async(request.clone()).await.unwrap();
    async fn receive(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }
    socket.send(Message::Text(json!({"type":"response.create","model":"m","stream_id":"warm","generate":false,"store":true,"input":"warm history","event_id":"once"}).to_string().into())).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "response.created");
    let warm = receive(&mut socket).await;
    assert_eq!(warm["stream_id"], "warm");
    assert_eq!(warm["response"]["status"], "completed");
    assert_eq!(warm["response"]["store"], false);
    let id = warm["response"]["id"].as_str().unwrap();
    assert!(
        state
            .responses
            .get(&state.operations.authenticate_api_key(&key).unwrap(), id)
            .is_err()
    );
    // A live connection retains its owned history after an authorized group move.
    let target = state
        .operations
        .save_group(
            None,
            serde_json::from_value(json!({
                "revision": state.operations.snapshot()["revision"], "name":"moved", "enabled":true
            }))
            .unwrap(),
        )
        .unwrap();
    let owner = state.operations.authenticate_api_key(&key).unwrap();
    state
        .operations
        .set_api_key_group(
            owner.key_id,
            codex_relay::operations::ApiKeyGroup {
                revision: 1,
                group_id: target,
            },
        )
        .unwrap();
    // Prewarm defaults (including model) are connection-local and no process is started.
    socket.send(Message::Text(json!({"type":"response.create","previous_response_id":id,"input":"next","generate":false}).to_string().into())).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "response.created");
    let continued = receive(&mut socket).await;
    assert_eq!(continued["type"], "response.completed");
    assert_eq!(continued["response"]["store"], false);
    socket
        .send(Message::Text(
            json!({"type":"response.create","event_id":"once","model":"m","input":"duplicate"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(receive(&mut socket).await["type"], "error");
    socket
        .send(Message::Text(
            json!({"type":"response.steer","stream_id":"idle","input":"late"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let error = receive(&mut socket).await;
    assert_eq!(error["code"], "RESPONSE_NOT_ACTIVE");
    assert_eq!(error["stream_id"], "idle");
    let (mut other, _) = connect_async(request).await.unwrap();
    other.send(Message::Text(json!({"type":"response.create","model":"m","previous_response_id":id,"input":"lost","generate":false}).to_string().into())).await.unwrap();
    assert_eq!(
        receive(&mut other).await["code"],
        "previous_response_not_found"
    );
    // A fresh connection can replay its own history without a persistent response ID.
    other.send(Message::Text(json!({"type":"response.create","model":"m","input":[{"role":"user","content":"warm history"},{"role":"user","content":"next"}],"generate":false,"store":true}).to_string().into())).await.unwrap();
    assert_eq!(receive(&mut other).await["type"], "response.created");
    assert_eq!(receive(&mut other).await["response"]["store"], false);
    other
        .send(Message::Text(
            json!({"type":"response.create","model":"forbidden","generate":false,"input":[]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(receive(&mut other).await["code"], "MODEL_NOT_ALLOWED");
    assert!(state.profiles.list().await.is_empty());
    assert_eq!(state.operations.snapshot()["active"], 0);
    socket.close(None).await.unwrap();
    other.close(None).await.unwrap();
    server.abort();
}
