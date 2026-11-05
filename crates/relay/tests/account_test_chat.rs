use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use codex_relay::admin::{AdminState, router};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

type Captured = Arc<Mutex<Vec<Value>>>;
async fn upstream(
    State(captured): State<Captured>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (HeaderMap, String) {
    assert_eq!(headers["authorization"], "Bearer synthetic-test-key");
    captured.lock().unwrap().push(body.clone());
    let text = body["input"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    if text == "wait" {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    let events = if text == "fail" {
        vec![
            json!({"type":"response.failed","response":{"error":{"message":"PRIVATE_FAILURE_TEXT"}}}),
        ]
    } else {
        vec![
            json!({"type":"response.output_text.delta","delta":"PRIVATE_REPLY"}),
            json!({"type":"response.completed","response":{"id":"resp_test","model":"test-model","usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}),
        ]
    };
    (
        headers,
        events.iter().map(|v| format!("data: {v}\n\n")).collect(),
    )
}
async fn call(app: &Router, path: &str, body: Value, authorized: bool) -> axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "127.0.0.1:4317")
        .header("origin", "http://127.0.0.1:4317")
        .header("content-type", "application/json");
    if authorized {
        request = request.header("authorization", format!("Bearer {}", "a".repeat(64)));
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
async fn text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn chat_is_bound_to_selected_account_and_never_journals_content() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let captured = Captured::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(upstream))
                .with_state(captured.clone()),
        )
        .into_future(),
    );
    let created=call(&app,"/api/accounts",json!({"name":"test only","source":"api-key","upstream":{"baseUrl":format!("http://{address}/v1"),"apiKey":"synthetic-test-key"}}),true).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let account: Value = serde_json::from_str(&text(created).await).unwrap();
    let id = account["id"].as_str().unwrap();
    let path = format!("/api/accounts/{id}/chat-test");
    let body = json!({"model":"test-model","reasoningEffort":"high","messages":[{"role":"user","content":"PRIVATE_PROMPT"}]});
    assert!(
        !call(&app, &path, body.clone(), false)
            .await
            .status()
            .is_success()
    );
    let malformed = call(
        &app,
        &path,
        json!({"messages":[{"role":"system","content":"bad"}]}),
        true,
    )
    .await;
    assert!(!malformed.status().is_success());
    let invalid_effort = call(&app, &path, json!({"model":"test-model","reasoningEffort":"invalid","messages":[{"role":"user","content":"hi"}]}), true).await;
    assert_eq!(invalid_effort.status(), StatusCode::BAD_REQUEST);
    assert!(captured.lock().unwrap().is_empty());
    let response = call(&app, &path, body, true).await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    let stream = text(response).await;
    assert!(stream.contains("PRIVATE_REPLY"), "{stream}");
    assert!(stream.contains("\"type\":\"done\""), "{stream}");
    assert!(stream.contains("\"totalTokens\":7"));
    let request = captured.lock().unwrap()[0].clone();
    assert_eq!(request["store"], false);
    assert_eq!(request["tools"], json!([]));
    assert_eq!(request["tool_choice"], "none");
    assert_eq!(request["reasoning"]["effort"], "high");
    assert_eq!(request["input"][0]["content"], "PRIVATE_PROMPT");
    let failure = text(
        call(
            &app,
            &path,
            json!({"model":"test-model","messages":[{"role":"user","content":"fail"}]}),
            true,
        )
        .await,
    )
    .await;
    assert!(failure.contains("\"type\":\"error\""));
    assert!(!failure.contains("PRIVATE_FAILURE_TEXT"));
    assert!(captured.lock().unwrap()[1].get("reasoning").is_none());
    let records = state.operations.statistics(Default::default()).unwrap();
    for record in records["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["action"] == "chat_test")
    {
        assert_eq!(record["accountId"], id);
        let content = state
            .operations
            .request_content(record["id"].as_str().unwrap().parse().unwrap())
            .unwrap();
        assert!(content["request"].is_null());
        assert!(content["response"].is_null());
    }
    assert!(!records.to_string().contains("PRIVATE_"));
    let response = call(
        &app,
        &path,
        json!({"model":"test-model","messages":[{"role":"user","content":"wait"}]}),
        true,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while captured.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.operations.snapshot()["active"] != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.operations.snapshot()["active"], 0);
    server.abort();
}
