use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use codex_relay::{backend::Backend, installation::Installation, provider::Provider, responses};
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
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_NATIVE_ONLY");
    let value: Value = serde_json::from_slice(&body).unwrap();
    let n = s.count.fetch_add(1, Ordering::SeqCst);
    s.results.lock().unwrap().push(value.clone());
    let item = if n == 15 {
        json!({"type":"function_call","id":"fc_feedback","call_id":"feedback_call","name":"content_feedback","arguments":"{}"})
    } else if n == 11 {
        use sha2::{Digest, Sha256};
        let alias = |name: &str| {
            format!(
                "relay_{}",
                &hex::encode(Sha256::digest(name.as_bytes()))[..48]
            )
        };
        json!({"type":"function_call","id":"fc_mcp","call_id":"mcp_call","namespace":alias("mcp__catalog"),"name":alias("mcp__read"),"arguments":"{}"})
    } else if n == 9 {
        json!({"type":"function_call","id":"fc_search","call_id":"find_tools","name":"__relay_tool_search","arguments":"{\"query\":\"echo\"}"})
    } else if n == 7 || n == 13 {
        let namespace = value["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "additional_tools")
            .flat_map(|item| item["tools"].as_array().unwrap())
            .find(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("relay_"))
            })
            .expect("reserved namespace must be escaped for native registration")["name"]
            .clone();
        if n == 13 {
            json!({"type":"custom_tool_call","id":"native_custom","call_id":"native_image_call","namespace":namespace,"name":"exec","input":"const image = await tools.image_gen__imagegen({prompt: \"blue whale\"});\ngeneratedImage(image);\n"})
        } else {
            json!({"type":"function_call","id":"fc_custom","call_id":"custom_call","namespace":namespace,"name":"exec","arguments":json!({"input":"text(\"client only\");\n"}).to_string()})
        }
    } else if n == 6 {
        json!({"type":"message","id":"json_message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"ok\":true}","annotations":[]}]})
    } else if n == 4 {
        json!({"type":"web_search_call","id":"search_native","status":"completed","action":{"type":"search","query":"Rust language"}})
    } else if n == 5 {
        json!({"type":"image_generation_call","id":"image_native","status":"completed","result":"iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC"})
    } else if n == 2 {
        json!({"type":"function_call","id":"fc_ns","call_id":"namespace_call","namespace":"client","name":"client_echo","arguments":"{\"text\":\"image\"}"})
    } else if n == 0 {
        json!({"type":"function_call","id":"fc_native","call_id":"native_call","namespace":"functions","name":"client_echo","arguments":"{\"text\":\"hello\"}"})
    } else {
        json!({"type":"message","id":format!("m_{n}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Native direct completed","annotations":[]}]})
    };
    let data=[json!({"type":"response.output_item.done","output_index":0,"item":item}),json!({"type":"response.completed","response":{"id":format!("r_{n}"),"status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}})].iter().map(|v|format!("data: {v}\n\n")).collect::<String>();
    ([("content-type", "text/event-stream")], data).into_response()
}
#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; native process connects directly to synthetic localhost model, no real model usage"]
async fn responses_preserve_native_transport_and_stateless_tool_history() {
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
    let fixture = Arc::new(Fixture {
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
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(home.join("config.toml"))
            .unwrap(),
        "[model_providers.synthetic.capabilities]\nremote_compaction=\"v2\""
    )
    .unwrap();
    let with_catalog = std::env::var_os("BRIDGE_NATIVE_CATALOG")
        .map(|path| {
            let mut cache: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            // Only the synthetic fixture's metadata age changes; never rewrite the
            // supplied native cache or load any account credential from it.
            // Reproduce the catalog age that used to trigger a five-second
            // discovery timeout on every turn. The original source is untouched.
            cache["fetched_at"] =
                json!((chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339());
            std::fs::write(
                home.join("models_cache.json"),
                serde_json::to_vec(&cache).unwrap(),
            )
            .unwrap();
        })
        .is_some();
    let backend = Backend::start_response_with_proxy(
        &installation,
        &cwd,
        Some(&home),
        false,
        &Default::default(),
        "gpt-6.1-sol",
    )
    .await
    .unwrap();
    // 导出的原生目录同样是进程级快照，仍须允许账号后台刷新发现元数据。
    assert!(backend.uses_cached_catalog());
    fixture.pid.store(backend.pid, Ordering::SeqCst);
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
    let tools = json!([{"type":"function","name":"client_echo","description":"Client echo","parameters":{"type":"object","properties":{"text":{"type":"string"}}},"strict":false}]);
    let request=responses::Request::parse(json!({"model":"gpt-6.1-sol","input":[{"role":"system","content":"SYSTEM_TEST"},{"type":"additional_tools","role":"developer","tools":tools},{"role":"user","content":"Call client echo"}],"store":false,"max_output_tokens":1,"reasoning":{"effort":"high"}})).unwrap();
    async fn one(
        backend: &Backend,
        provider: &Provider,
        cwd: &std::path::Path,
        request: &responses::Request,
    ) -> Value {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let work = responses::run(
            &backend.rpc,
            provider,
            cwd,
            request,
            &request.input,
            tx,
            CancellationToken::new(),
        );
        let collect = async {
            let mut events = vec![];
            while let Some(e) = rx.recv().await {
                events.push(e);
            }
            events
        };
        let (result, events) = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::join!(work, collect)
        })
        .await
        .unwrap();
        let summary = result
            .unwrap_or_else(|error| panic!("{error:?}; synthetic tools: {:?}", request.tools));
        assert_eq!(summary.outcome, "completed", "summary: {:?}", summary);
        assert_eq!(summary.guard.state, "not-used-native-direct");
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e["sequence_number"], i);
        }
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events.last().unwrap()["type"], "response.completed");
        events.last().unwrap()["response"].clone()
    }
    let first = one(&backend, &provider, &cwd, &request).await;
    assert_eq!(first["output"][0]["type"], "function_call");
    assert_eq!(first["output"][0]["call_id"], "native_call");
    assert_eq!(first["output"][0]["arguments"], "{\"text\":\"hello\"}");
    assert_eq!(first["usage"]["total_tokens"], 12);
    assert_eq!(first["max_output_tokens"], Value::Null);
    assert_eq!(first["usage"]["output_tokens"], 2);
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        1,
        "must not invent a result and continue the model"
    );
    let mut history = request.input.clone();
    assert!(first["output"][0].get("namespace").is_none());
    history.extend(first["output"].as_array().unwrap().clone());
    history.push(json!({"type":"function_call_output","call_id":"native_call","output":"CLIENT_EXECUTED_RESULT"}));
    let next = responses::Request::parse(
        json!({"model":"gpt-6.1-sol","input":history,"store":false,"max_output_tokens":1,"text":{"verbosity":"high"},"service_tier":"priority",
            "temperature":0.5,"top_p":0.8,"background":true,"parallel_tool_calls":false,
            "truncation":"auto","tool_choice":"required","future_parameter":"ignored"}),
    )
    .unwrap();
    let second = one(&backend, &provider, &cwd, &next).await;
    assert_eq!(
        second["output"][0]["content"][0]["text"],
        "Native direct completed"
    );
    assert_eq!(second["usage"]["total_tokens"], 12);
    assert_eq!(second["max_output_tokens"], Value::Null);
    assert_eq!(second["temperature"], Value::Null);
    assert_eq!(second["top_p"], Value::Null);
    assert_eq!(second["background"], false);
    assert_eq!(second["truncation"], "disabled");
    assert_eq!(second["tool_choice"], "auto");
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC";
    let namespaced = json!([{"type":"namespace","name":"client","description":"Client namespace","tools":tools}]);
    let image_request=responses::Request::parse(json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":[{"type":"input_text","text":"Describe image and call client_echo"},{"type":"input_image","image_url":image}]}],"tools":namespaced,"store":false})).unwrap();
    let third = one(&backend, &provider, &cwd, &image_request).await;
    assert_eq!(third["output"][0]["namespace"], "client");
    let mut history = image_request.input.clone();
    history.extend(third["output"].as_array().unwrap().clone());
    history.push(json!({"type":"function_call_output","call_id":"namespace_call","output":[{"type":"input_text","text":"CLIENT_NAMESPACE_RESULT"},{"type":"input_image","image_url":image}]}));
    let fourth_request = responses::Request::parse(
        json!({"model":"gpt-6.1-sol","input":history,"tools":namespaced,"store":false}),
    )
    .unwrap();
    let fourth = one(&backend, &provider, &cwd, &fourth_request).await;
    assert_eq!(fourth["output"][0]["type"], "message");
    let search=responses::Request::parse(json!({"model":"gpt-6.1-sol","input":"Search Rust","tools":[{"type":"web_search"}],"store":false})).unwrap();
    let searched = one(&backend, &provider, &cwd, &search).await;
    assert_eq!(searched["output"][0]["type"], "web_search_call");
    let image=responses::Request::parse(json!({"model":"gpt-6.1-sol","input":"Generate blue square","tools":[{"type":"image_generation"}],"store":false})).unwrap();
    let generated = one(&backend, &provider, &cwd, &image).await;
    assert_eq!(generated["output"][0]["type"], "image_generation_call");
    let json_request = responses::Request::parse(json!({
        "model":"gpt-6.1-sol","input":"Return an object","instructions":"KEEP_CALLER_INSTRUCTIONS",
        "text":{"format":{"type":"json_object"}},"store":false
    }))
    .unwrap();
    let json_result = one(&backend, &provider, &cwd, &json_request).await;
    assert_eq!(json_result["text"]["format"]["type"], "json_object");
    assert_eq!(
        json_result["output"][0]["content"][0]["text"],
        "{\"ok\":true}"
    );
    assert_eq!(json_result["instructions"], "KEEP_CALLER_INSTRUCTIONS");
    let custom_request = responses::Request::parse(json!({"model":"gpt-6.1-sol","input":[
        {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","description":"Client tools","tools":[{"type":"custom","name":"exec","description":"Run code on the client","format":{"type":"text"}}]}]},
        {"role":"user","content":"Call the client exec tool"}
    ],"store":false})).unwrap();
    let custom = one(&backend, &provider, &cwd, &custom_request).await;
    assert_eq!(custom["output"][0]["type"], "custom_tool_call");
    assert_eq!(custom["output"][0]["namespace"], "functions");
    assert_eq!(custom["output"][0]["input"], "text(\"client only\");\n");
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        8,
        "must yield to the client without fabricating tool output"
    );
    let mut history = custom_request.input.clone();
    history.extend(custom["output"].as_array().unwrap().clone());
    history.push(json!({"type":"custom_tool_call_output","call_id":"custom_call","output":"CUSTOM_CLIENT_RESULT"}));
    let custom_next =
        responses::Request::parse(json!({"model":"gpt-6.1-sol","input":history,"store":false}))
            .unwrap();
    let custom_result = one(&backend, &provider, &cwd, &custom_next).await;
    assert_eq!(custom_result["output"][0]["type"], "message");
    let compact = responses::Request::parse(json!({"model":"gpt-6.1-sol","store":false,"input":[{"role":"user","content":"Keep the exact compaction fixture"},{"type":"compaction_trigger"}]})).unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let before = fixture.count.load(Ordering::SeqCst);
    let error = responses::run(
        &backend.rpc,
        &provider,
        &cwd,
        &compact,
        &compact.input,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "INVALID_INPUT");
    assert_eq!(fixture.count.load(Ordering::SeqCst), before);
    let search_tools = json!([{"type":"tool_search","execution":"client"}]);
    let search = responses::Request::parse(
        json!({"model":"gpt-6.1-sol","store":false,"input":"Find echo","tools":search_tools}),
    )
    .unwrap();
    let found = one(&backend, &provider, &cwd, &search).await;
    assert_eq!(found["output"][0]["type"], "tool_search_call");
    assert_eq!(found["output"][0]["arguments"]["query"], "echo");
    let mut history = search.input.clone();
    history.extend(found["output"].as_array().unwrap().clone());
    history.push(json!({"type":"tool_search_output","execution":"client","call_id":"find_tools","tools":tools}));
    let search_next = responses::Request::parse(
        json!({"model":"gpt-6.1-sol","store":false,"tools":search_tools,"input":history}),
    )
    .unwrap();
    assert_eq!(
        one(&backend, &provider, &cwd, &search_next).await["status"],
        "completed"
    );
    let deferred_tools = json!([{"type":"namespace","name":"mcp__catalog","description":"Caller MCP","tools":[{"type":"function","name":"mcp__read","defer_loading":true}]}]);
    let deferred=responses::Request::parse(json!({"model":"gpt-6.1-sol","store":false,"input":"Call the client MCP function","tools":deferred_tools})).unwrap();
    let mcp = one(&backend, &provider, &cwd, &deferred).await;
    assert_eq!(mcp["output"][0]["namespace"], "mcp__catalog");
    assert_eq!(mcp["output"][0]["name"], "mcp__read");
    let mut history = deferred.input.clone();
    history.extend(mcp["output"].as_array().unwrap().clone());
    history.push(
        json!({"type":"function_call_output","call_id":"mcp_call","output":"MCP_CLIENT_RESULT"}),
    );
    let next = responses::Request::parse(
        json!({"model":"gpt-6.1-sol","store":false,"input":history,"tools":deferred_tools}),
    )
    .unwrap();
    assert_eq!(
        one(&backend, &provider, &cwd, &next).await["status"],
        "completed"
    );
    let native_custom = one(&backend, &provider, &cwd, &custom_request).await;
    let native_call = &native_custom["output"][0];
    assert_eq!(native_call["type"], "custom_tool_call");
    assert_eq!(native_call["namespace"], "functions");
    assert_eq!(native_call["call_id"], "native_image_call");
    assert_eq!(
        native_call["input"],
        "const image = await tools.image_gen__imagegen({prompt: \"blue whale\"});\ngeneratedImage(image);\n"
    );
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        14,
        "must not continue with a fabricated native tool failure"
    );
    let mut history = custom_request.input.clone();
    history.extend(native_custom["output"].as_array().unwrap().clone());
    history.push(json!({"type":"custom_tool_call_output","call_id":"native_image_call","output":[{"type":"input_text","text":"CLIENT_GENERATED_IMAGE"},{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC"}]}));
    let next =
        responses::Request::parse(json!({"model":"gpt-6.1-sol","store":false,"input":history}))
            .unwrap();
    assert_eq!(
        one(&backend, &provider, &cwd, &next).await["status"],
        "completed"
    );
    // 知营回归：检查真正发给原生模型的目录，再验证无参数业务工具及结果续接。
    let feedback = responses::Request::parse(json!({"model":"gpt-6.1-sol","input":"Read saved feedback",
        "tools":[{"type":"function","name":"content_feedback","description":"Saved feedback",
        "parameters":{"type":"object","properties":{},"additionalProperties":false},"strict":false}]})).unwrap();
    let feedback_output = one(&backend, &provider, &cwd, &feedback).await;
    assert_eq!(feedback_output["output"][0]["name"], "content_feedback");
    let next = responses::Request::parse(json!({"model":"gpt-6.1-sol","tools":feedback.tools,
        "input":[feedback_output["output"][0],{"type":"function_call_output","call_id":"feedback_call","output":"SAVED_FEEDBACK"}]})).unwrap();
    assert_eq!(
        one(&backend, &provider, &cwd, &next).await["status"],
        "completed"
    );
    let disabled = responses::Request::parse(json!({"model":"gpt-6.1-sol","input":"No tools", "tools":feedback.tools,"tool_choice":"none"})).unwrap();
    assert_eq!(
        one(&backend, &provider, &cwd, &disabled).await["status"],
        "completed"
    );
    backend.stop().await.unwrap();
    if with_catalog {
        let version = installation
            .app_server_version
            .as_deref()
            .unwrap()
            .strip_prefix("codex-cli ")
            .unwrap()
            .split('-')
            .next()
            .unwrap();
        std::fs::write(
            home.join("models_cache.json"),
            json!({
                "client_version":version,"fetched_at":chrono::Utc::now().to_rfc3339(),
                "models":[{"slug":"gpt-6.1-sol"}]
            })
            .to_string(),
        )
        .unwrap();
        let rejected = Backend::start_response_with_proxy(
            &installation,
            &cwd,
            Some(&home),
            false,
            &Default::default(),
            "gpt-6.1-sol",
        )
        .await;
        assert!(
            rejected.is_err(),
            "invalid tool policy must never fall back to host tools"
        );
    }
    server.abort();
    assert_eq!(fixture.count.load(Ordering::SeqCst), 18);
    assert_eq!(fixture.verified.load(Ordering::SeqCst), 18);
    let seen = fixture.results.lock().unwrap();
    fn tool_names(tools: &[Value]) -> Vec<String> {
        tools
            .iter()
            .flat_map(|tool| {
                if tool["type"] == "namespace" {
                    tool_names(tool["tools"].as_array().unwrap())
                } else {
                    vec![tool["name"].as_str().unwrap_or("<unnamed>").to_owned()]
                }
            })
            .collect()
    }
    for (index, expected) in [
        (0, vec!["client_echo"]),
        (15, vec!["content_feedback"]),
        (17, vec![]),
    ] {
        let mut names = tool_names(
            seen[index]["tools"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
        for item in seen[index]["input"].as_array().unwrap() {
            if item["type"] == "additional_tools" {
                names.extend(tool_names(item["tools"].as_array().unwrap()));
            }
        }
        names.sort();
        names.dedup();
        assert_eq!(
            names, expected,
            "upstream tool declarations must match the caller at request {index}"
        );
    }
    // The ingress must also accept the actual wire shape produced by this installed
    // Codex version, not only our hand-written client fixtures.
    for captured in [&seen[0], &seen[1], &seen[7], &seen[8], &seen[13], &seen[14]] {
        responses::Request::parse(captured.clone()).unwrap_or_else(|error| {
            panic!("captured native Responses request was rejected: {error:?}")
        });
    }
    let initial = seen[0]["input"].as_array().unwrap();
    let before = initial
        .iter()
        .position(|v| v["content"][0]["text"] == "SYSTEM_TEST")
        .unwrap();
    let after = initial
        .iter()
        .position(|v| v["content"][0]["text"] == "Call client echo")
        .unwrap();
    assert!(
        initial[before + 1..after]
            .iter()
            .any(|v| v["type"] == "additional_tools" && v["tools"][0]["name"] == "client_echo"),
        "embedded declarations must stay between their original messages"
    );
    assert!(
        seen[8]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "function_call_output"
                && v["call_id"] == "custom_call"
                && v["output"] == "CUSTOM_CLIENT_RESULT")
    );
    assert!(
        seen[14]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "function_call_output"
                && v["call_id"] == "native_image_call"
                && v["output"][0]["text"] == "CLIENT_GENERATED_IMAGE"
                && v["output"][1]["type"] == "input_image")
    );
    assert_eq!(seen[0]["reasoning"]["effort"], "high");
    assert_eq!(seen[1]["text"]["verbosity"], "high");
    assert_eq!(seen[1]["service_tier"], "priority");
    for ignored in ["temperature", "top_p", "future_parameter"] {
        assert!(seen[1].get(ignored).is_none());
    }
    assert_eq!(seen[1]["tool_choice"], "auto");
    assert!(
        seen[6]
            .to_string()
            .contains("exactly one valid JSON object")
    );
    assert!(seen[6].to_string().contains("KEEP_CALLER_INSTRUCTIONS"));
    assert!(
        seen[3]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "function_call_output"
                && v["output"].is_array()
                && v["output"][1]["type"] == "input_image")
    );
    assert!(
        seen.iter()
            .all(|request| request.get("max_output_tokens").is_none())
    );
    assert!(
        seen[0].to_string().contains("client_echo"),
        "registered dynamic tool must be visible in the native model request"
    );
    assert!(
        seen[2]["input"].to_string().contains("input_image"),
        "valid inline image must reach native model payload"
    );
    // 命名空间业务工具仍须保留；前面的目录断言另行保证不会泄露原生 exec。
    assert!(seen[2].to_string().contains("client_echo"));
    assert!(
        seen[3]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["namespace"] == "client" && t["call_id"] == "namespace_call")
    );
    let input = seen[1]["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|v| v["type"] == "function_call" && v["call_id"] == "native_call")
    );
    assert!(input.iter().any(|v|v["type"]=="function_call_output"&&v["output"]=="CLIENT_EXECUTED_RESULT"));
    assert!(
        input
            .iter()
            .any(|v| v["role"] == "developer" && v["content"][0]["text"] == "SYSTEM_TEST")
    );
}
