//! Real installed app-server, local synthetic model only; no production credentials.
use axum::{Router, body::Bytes, extract::State, response::IntoResponse, routing::post};
use codex_relay::{
    account_location::Location, backend::Backend, installation::Installation, provider::Provider,
    responses,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

async fn model(State(captures): State<Arc<Mutex<Vec<Value>>>>, body: Bytes) -> impl IntoResponse {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let compact = request["input"].as_array().is_some_and(|items| {
        items
            .last()
            .is_some_and(|v| v["type"] == "compaction_trigger")
    });
    captures.lock().unwrap().push(request);
    let item = if compact {
        json!({"type":"compaction","id":"cmp_location","encrypted_content":"SYNTHETIC_LOCATION_COMPACTION"})
    } else {
        json!({"type":"message","id":"m_location","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Captured","annotations":[]}]})
    };
    let events = [
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":"r_location","status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
    ];
    (
        [("content-type", "text/event-stream")],
        events
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>(),
    )
}
fn search_locations(value: &Value, output: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            if map.get("type").is_some_and(|v| v == "web_search") {
                output.push(value["user_location"].clone());
            }
            for value in map.values() {
                search_locations(value, output);
            }
        }
        Value::Array(list) => {
            for value in list {
                search_locations(value, output);
            }
        }
        _ => {}
    }
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; local synthetic model only"]
async fn native_location_reaches_model_search_and_each_turn() {
    let installation: Installation = serde_json::from_slice(
        &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
    )
    .unwrap();
    codex_relay::installation::validate(&installation)
        .await
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let captures = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/responses", post(model))
        .with_state(captures.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(home.join("config.toml"),format!("model=\"gpt-6.1-sol\"\nmodel_provider=\"synthetic\"\n[features]\nenable_request_compression=false\n[model_providers.synthetic]\nname=\"Synthetic\"\nbase_url=\"http://127.0.0.1:{port}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_LOCATION_ONLY\"\n[model_providers.synthetic.capabilities]\nremote_compaction=\"v2\"\n")).unwrap();
    let backend = Backend::start_direct(&installation, &cwd, Some(&home))
        .await
        .unwrap();
    let location = Location {
        timezone: "America/New_York".into(),
        country: "US".into(),
        region: "New York".into(),
        city: "New York".into(),
    };
    backend.rpc.set_location(Some(location.clone()));
    let provider = Provider {
        execution: Default::default(),
        id: "synthetic".into(),
        model: "gpt-6.1-sol".into(),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        auth: "native-bearer".into(),
        required_headers: vec![],
        forward_headers: vec![],
        mcp_servers: vec![],
    };
    let result=tokio::time::timeout(Duration::from_secs(90),async {
        let request=responses::Request::parse(json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"<environment_context><timezone>Asia/Shanghai</timezone><current_date>1999-01-01</current_date></environment_context>"},{"role":"user","content":"Search local weather"}],"instructions":"KEEP_ORIGINAL_INSTRUCTIONS","tools":[{"type":"web_search"}]})).unwrap();
        let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        let reader=tokio::spawn(async move{while rx.recv().await.is_some(){}});
        let result=responses::run(&backend.rpc,&provider,&cwd,&request,&request.input,tx,CancellationToken::new()).await;
        reader.await.unwrap();result.unwrap();
        let compact=responses::Request::parse(json!({"model":"gpt-6.1-sol","store":false,"input":[{"role":"user","content":"Keep account location during compaction"},{"type":"compaction_trigger"}]})).unwrap();
        let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        let reader=tokio::spawn(async move{while rx.recv().await.is_some(){}});
        let result=responses::run(&backend.rpc,&provider,&cwd,&compact,&compact.input,tx,CancellationToken::new()).await;
        reader.await.unwrap();result.unwrap();
        // A second turn on the SAME raw native thread gets a fresh managed context.
        let started=backend.rpc.request("thread/start",json!({"model":"gpt-6.1-sol","modelProvider":"synthetic","cwd":cwd,"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","config":{"web_search":"disabled"}})).await.unwrap();
        let thread=started["thread"]["id"].as_str().unwrap();
        for text in ["First turn","Second turn"] {
            let mut events=backend.rpc.subscribe();
            backend.rpc.request("turn/start",json!({"threadId":thread,"input":[{"type":"text","text":text}]})).await.unwrap();
            loop {let event=events.recv().await.unwrap();if event["method"]=="turn/completed" && event["params"]["threadId"]==thread{break;}}
        }
    }).await;
    backend.stop().await.unwrap();
    server.abort();
    result.unwrap();
    let captures = captures.lock().unwrap();
    assert!(captures.len() >= 3);
    std::fs::create_dir_all(".cache").unwrap();
    std::fs::write(
        ".cache/native-location-captures.json",
        serde_json::to_vec_pretty(&*captures).unwrap(),
    )
    .unwrap();
    // Native may prewarm a connection before the actual user turn.
    let first = captures
        .iter()
        .find(|body| body["input"].to_string().contains("Search local weather"))
        .expect("actual user turn");
    let text = first["input"].to_string();
    assert!(first.to_string().contains("KEEP_ORIGINAL_INSTRUCTIONS"));
    assert!(text.contains("America/New_York"));
    assert!(text.contains(&location.date(chrono::Utc::now())));
    assert!(!text.contains("Asia/Shanghai"));
    assert!(!text.contains("1999-01-01"));
    let mut locations = vec![];
    search_locations(first, &mut locations);
    assert!(
        !locations.is_empty(),
        "actual native request must contain a search declaration"
    );
    assert!(
        locations
            .iter()
            .all(|l| l["timezone"] == "America/New_York" && l["city"] == "New York"),
        "search location must survive native serialization: {locations:?}"
    );
    let compact = captures
        .iter()
        .find(|body| {
            body["input"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["type"] == "compaction_trigger")
            })
        })
        .expect("compaction request captured");
    assert!(compact["input"].to_string().contains("America/New_York"));
    let last = captures.last().unwrap()["input"].to_string();
    assert!(
        last.matches("codex_apps_client_time_context").count() >= 4,
        "each turn receives a context"
    );
}
