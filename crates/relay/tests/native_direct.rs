use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use codex_relay::{
    backend::Backend, installation::Installation, native_session, provider::Provider,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Fixture {
    namespace: bool,
    authorization: String,
    oauth: bool,
    pid: AtomicU32,
    exe: PathBuf,
    port: u16,
    count: AtomicUsize,
    verified: AtomicUsize,
    results: Mutex<Vec<Value>>,
}
async fn model(
    State(s): State<Arc<Fixture>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !codex_relay::platform::verify_peer(
        peer.port(),
        s.port,
        s.pid.load(Ordering::SeqCst),
        &s.exe,
    )
    .await
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    s.verified.fetch_add(1, Ordering::SeqCst);
    assert!(headers["user-agent"].to_str().unwrap().contains("codex"));
    assert_eq!(headers["authorization"], s.authorization);
    if s.oauth {
        assert_eq!(headers["chatgpt-account-id"], "synthetic-oauth");
    }
    let value: Value = serde_json::from_slice(&body).unwrap();
    let advertised: Vec<_> = value["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "additional_tools")
        .flat_map(|item| item["tools"].as_array().unwrap())
        .collect();
    let namespace = if s.namespace { "client" } else { "functions" };
    assert!(
        advertised.iter().any(|ns| ns["name"] == namespace
            && ns["tools"].as_array().is_some_and(|tools| tools
                .iter()
                .any(|t| t["type"] == "function" && t["name"] == "client_echo"))),
        "client function must be advertised directly, not only inside native code-mode instructions"
    );
    let n = s.count.fetch_add(1, Ordering::SeqCst);
    s.results.lock().unwrap().push(value);
    let mut item = if n == 0 {
        json!({"type":"function_call","id":"fc_native","call_id":"native_call","name":"client_echo","arguments":"{\"text\":\"hello\"}"})
    } else {
        json!({"type":"message","id":format!("m_{n}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Native direct completed","annotations":[]}]})
    };
    if n == 0 {
        item["namespace"] = json!(namespace);
    }
    let data=[json!({"type":"response.output_item.done","output_index":0,"item":item}),json!({"type":"response.completed","response":{"id":format!("r_{n}"),"status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}})].iter().map(|v|format!("data: {v}\n\n")).collect::<String>();
    ([("content-type", "text/event-stream")], data).into_response()
}
#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; native process connects directly to synthetic localhost model, no real model usage"]
async fn native_owns_model_connection_and_client_executes_dynamic_tool() {
    check_native(false, None).await;
}
#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; native namespace callback roundtrip"]
async fn native_namespaces_preserve_callback_identity() {
    check_native(true, None).await;
}
#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; synthetic native-managed and external OAuth model/tool/multi-turn checks"]
async fn oauth_credentials_drive_native_model_connections_and_tools() {
    check_native(false, Some("managed")).await;
    check_native(true, Some("external")).await;
}
async fn check_native(namespace: bool, oauth_mode: Option<&str>) {
    let installation: Installation = serde_json::from_slice(
        &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
    )
    .unwrap();
    codex_relay::installation::validate(&installation)
        .await
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let jwt=format!("e30.{}.signature",URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"sub":"synthetic-user","email":"test@example.invalid","exp":codex_relay::now_ms()/1000+3600,"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-oauth","chatgpt_plan_type":"plus"}})).unwrap()));
    let fixture = Arc::new(Fixture {
        namespace,
        authorization: format!(
            "Bearer {}",
            if oauth_mode.is_some() {
                jwt.as_str()
            } else {
                "SYNTHETIC_NATIVE_ONLY"
            }
        ),
        oauth: oauth_mode.is_some(),
        pid: AtomicU32::new(0),
        exe: installation.app_server_executable.clone(),
        port,
        count: AtomicUsize::new(0),
        verified: AtomicUsize::new(0),
        results: Mutex::new(vec![]),
    });
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(model))
                .with_state(fixture.clone())
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    );
    std::fs::write(home.join("config.toml"),format!("model=\"gpt-6.1-sol\"\nmodel_provider=\"synthetic\"\n[features]\nenable_request_compression=false\n[model_providers.synthetic]\nname=\"Synthetic\"\nbase_url=\"http://127.0.0.1:{port}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_NATIVE_ONLY\"\n")).unwrap();
    if let Some(mode) = oauth_mode {
        let configured = std::fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .replace("requires_openai_auth=false", "requires_openai_auth=true")
            .replace("experimental_bearer_token=\"SYNTHETIC_NATIVE_ONLY\"\n", "");
        std::fs::write(
            home.join("config.toml"),
            format!("chatgpt_base_url=\"http://127.0.0.1:{port}/v1\"\n{configured}"),
        )
        .unwrap();
        if mode == "managed" {
            let credential = codex_relay::oauth::parse(
                &json!({"accessToken":jwt,"idToken":jwt,"refreshToken":"synthetic-rt"}),
            )
            .unwrap();
            codex_relay::oauth::save(&home.join("auth.json"), &credential.native_document())
                .await
                .unwrap();
        }
    }
    let backend = Backend::start_direct(&installation, &cwd, Some(&home))
        .await
        .unwrap();
    fixture.pid.store(backend.pid, Ordering::SeqCst);
    if oauth_mode == Some("external") {
        backend.rpc.request("account/login/start",json!({"type":"chatgptAuthTokens","accessToken":jwt,"chatgptAccountId":"synthetic-oauth"})).await.unwrap();
    }
    let provider = Provider {
        execution: Default::default(),
        id: "synthetic".into(),
        model: "gpt-6.1-sol".into(),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        auth: "native-bearer".into(),
        required_headers: vec!["authorization".into()],
        forward_headers: vec![],
        mcp_servers: vec![],
    };
    let mut start = json!({"id":1,"method":"thread/start","params":{"model":"gpt-6.1-sol","dynamicTools":[{"type":"function","name":"client_echo","description":"Client echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}});
    if namespace {
        let tools = start["params"]["dynamicTools"].clone();
        start["params"]["dynamicTools"] = json!([{"type":"namespace","name":"client","description":"client namespace","tools":tools}]);
    }
    let (tx, mut incoming) = tokio::sync::mpsc::channel(16);
    let (out, mut rx) = tokio::sync::mpsc::channel(16);
    let work = native_session::run(
        &backend.rpc,
        &provider,
        &cwd,
        start,
        &mut incoming,
        out,
        CancellationToken::new(),
    );
    let client = async {
        let mut calls = 0;
        let mut turns = 0;
        while let Some(e) = rx.recv().await {
            if e["id"] == 1 && e.get("result").is_some() {
                tx.send(json!({"id":2,"method":"turn/start","params":{"input":[{"type":"text","text":"Use client echo","text_elements":[]}]}})).await.unwrap();
            }
            if e["method"] == "item/tool/call" {
                assert_eq!(e["params"]["arguments"]["text"], "hello");
                if namespace {
                    assert_eq!(e["params"]["namespace"], "client");
                }
                calls += 1;
                tx.send(json!({"id":e["id"],"result":{"success":true,"contentItems":[{"type":"inputText","text":"CLIENT_EXECUTED_RESULT"}]}})).await.unwrap();
            }
            if e["method"] == "turn/completed" {
                assert_eq!(e["params"]["turn"]["status"], "completed");
                turns += 1;
                if turns == 1 {
                    tx.send(json!({"id":3,"method":"turn/start","params":{"input":[{"type":"text","text":"Continue","text_elements":[]}]}})).await.unwrap();
                } else {
                    tx.send(json!({"id":4,"method":"session/close"}))
                        .await
                        .unwrap();
                }
            }
        }
        (calls, turns)
    };
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::join!(work, client)
    })
    .await;
    backend.stop().await.unwrap();
    server.abort();
    let (summary, (calls, turns)) = result.unwrap();
    let summary = summary.unwrap();
    assert_eq!(summary.outcome, "completed");
    assert_eq!((calls, turns), (1, 2));
    assert_eq!(fixture.count.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.verified.load(Ordering::SeqCst), 3);
    assert!(
        fixture.results.lock().unwrap()[1]["input"]
            .to_string()
            .contains("CLIENT_EXECUTED_RESULT")
    );
    assert_eq!(summary.guard.state, "not-used-native-direct");
}
