use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, DEFAULT_GROUP, Operations, StatsQuery},
    provider::Provider,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn setup(admit: bool) -> (tempfile::TempDir, Arc<AdminState>, String, Router) {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    if admit {
        // A journal-only account fails native preparation without making a model call.
        let account = Uuid::new_v4();
        state
            .operations
            .register_accounts(&[(account, "missing profile".into())])
            .unwrap();
        let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
        state
            .operations
            .set_inference_route(
                account,
                revision,
                Some(Provider {
                    execution: Default::default(),
                    id: "test".into(),
                    model: "test-model".into(),
                    base_url: "http://127.0.0.1:9/v1".into(),
                    auth: "none".into(),
                    forward_headers: vec![],
                    required_headers: vec![],
                    mcp_servers: vec![],
                }),
            )
            .unwrap();
    }
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"test caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let app = router(state.clone(), temp.path().join("web"));
    (temp, state, key, app)
}

async fn content(app: &Router, id: &str, token: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/requests/{id}/content"))
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (
        response.status(),
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap(),
    )
}

#[tokio::test]
async fn http_and_sse_failures_save_original_parameters_and_the_delivered_response() {
    for admit in [false, true] {
        for stream in [false, true] {
            let (_temp, state, key, app) = setup(admit).await;
            let body = json!({"model":"test-model","input":[{"role":"user","content":"PRIVATE_PROMPT"}],"instructions":"PRIVATE_INSTRUCTIONS","stream":stream,"max_output_tokens":37,"reasoning":{"effort":"high"}});
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("host", "127.0.0.1:4317")
                        .header("authorization", format!("Bearer {key}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let id = response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .to_owned();
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let expected = if admit && stream {
                assert_eq!(status, StatusCode::OK);
                let text = String::from_utf8(bytes.to_vec()).unwrap();
                serde_json::from_str::<Value>(
                    text.lines()
                        .filter_map(|line| line.strip_prefix("data: "))
                        .next_back()
                        .unwrap(),
                )
                .unwrap()
            } else {
                assert!(!status.is_success());
                serde_json::from_slice::<Value>(&bytes).unwrap()
            };
            let (loaded, saved) = content(&app, &id, &"a".repeat(64)).await;
            assert_eq!(loaded, StatusCode::OK);
            assert_eq!(saved["request"]["body"]["max_output_tokens"], 37);
            assert_eq!(saved["request"]["body"]["input"][0]["role"], "user");
            assert_eq!(saved["request"]["body"]["stream"], stream);
            assert_eq!(saved["response"]["body"], expected);
            assert_eq!(saved["response"]["statusCode"], status.as_u16());
            assert_eq!(
                saved["response"]["transport"],
                if admit && stream { "sse" } else { "http" }
            );
            assert!(!saved.to_string().contains("PRIVATE_"));
            assert!(!saved.to_string().contains(&key));
            assert_eq!(content(&app, &id, &key).await.0, StatusCode::FORBIDDEN);
            let stats = state.operations.statistics(StatsQuery::default()).unwrap();
            assert!(stats["records"][0].get("request").is_none());
            assert!(stats["records"][0].get("response").is_none());
            assert!(!stats.to_string().contains("PRIVATE_"));
            assert_eq!(state.operations.clear_request_history().unwrap(), 1);
            assert_eq!(
                content(&app, &id, &"a".repeat(64)).await.0,
                StatusCode::NOT_FOUND
            );
        }
    }
}

#[test]
fn execution_errors_survive_restart_and_success_does_not_store_content() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let success = ops
        .begin(None, None, "inference", "responses", true)
        .unwrap();
    let success_id = success.request_id();
    success.complete(Ok(())).unwrap();
    let failure = ops
        .begin(None, None, "inference", "responses", true)
        .unwrap();
    let failure_id = failure.request_id();
    failure
        .finish_execution(
            &codex_relay::execution::ExecutionOutcome::failed(codex_relay::error::Error::new(
                "UPSTREAM_REQUEST_REJECTED",
                "specific failure detail",
            )),
            None,
        )
        .unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert!(ops.request_content(success_id).unwrap()["response"].is_null());
    assert_eq!(
        ops.request_content(failure_id).unwrap()["response"]["body"]["error"]["message"],
        "specific failure detail"
    );
}

#[tokio::test]
async fn websocket_failure_keeps_lane_and_original_frame() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message, client::IntoClientRequest},
    };
    for (admit, native) in [(false, false), (true, false), (false, true)] {
        let (_temp, state, key, _) = setup(admit).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let policy =
            codex_relay::network::AccessPolicy::new(&[format!("http://{address}/v1")], &[])
                .unwrap();
        let app = codex_relay::network::client_router(state.clone(), policy);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = if native { "native" } else { "responses" };
        let mut upgrade = format!("ws://{address}/v1/{endpoint}")
            .into_client_request()
            .unwrap();
        upgrade
            .headers_mut()
            .insert("authorization", format!("Bearer {key}").parse().unwrap());
        let (mut socket, _) = connect_async(upgrade).await.unwrap();
        let body = if native {
            json!({"id":1,"method":"thread/start","params":{"model":"test-model","developerInstructions":"PRIVATE_PROMPT"}})
        } else {
            json!({"type":"response.create","model":"test-model","stream_id":"lane-a","stream":false,"input":"PRIVATE_PROMPT","max_output_tokens":37})
        };
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(20), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let received: Value = serde_json::from_str(received.to_text().unwrap()).unwrap();
        let stats = state.operations.statistics(StatsQuery::default()).unwrap();
        let id = Uuid::parse_str(stats["records"][0]["id"].as_str().unwrap()).unwrap();
        let saved = state.operations.request_content(id).unwrap();
        assert_eq!(saved["response"]["body"], received);
        assert_eq!(saved["response"]["transport"], "websocket");
        assert!(saved["response"]["statusCode"].is_null());
        if native {
            assert_eq!(saved["request"]["body"]["method"], "thread/start");
        } else {
            assert_eq!(saved["request"]["body"]["stream"], false);
            assert_eq!(saved["request"]["body"]["stream_id"], "lane-a");
        }
        assert!(!saved.to_string().contains("PRIVATE_PROMPT"));
        socket.close(None).await.unwrap();
        server.abort();
    }
}
