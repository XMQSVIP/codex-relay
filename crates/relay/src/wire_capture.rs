//! Opt-in network integration test. Hooks in RPC/WS record only while this test
//! is active; this module and its hooks do not exist in production builds.
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State, ws::WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

type Trace = Arc<Mutex<Vec<Value>>>;
static ACTIVE: OnceLock<Mutex<Option<Trace>>> = OnceLock::new();

fn record(value: Value) {
    if let Some(trace) = ACTIVE.get().and_then(|a| a.lock().unwrap().clone()) {
        trace.lock().unwrap().push(value);
    }
}

pub(crate) fn record_rpc(value: &Value) {
    if matches!(
        value["method"].as_str(),
        Some("initialize" | "initialized" | "thread/start" | "turn/start")
    ) {
        record(json!({"hop":"bridge-to-app-server", "transport":"stdio",
            "observation":"successfully flushed real child-process pipe", "message":value}));
    }
}

pub(crate) fn record_native_frame(direction: &str, value: &Value) {
    record(json!({"hop":"client-to-bridge", "transport":"websocket",
        "direction":direction, "message":value}));
}

fn headers_value(headers: &HeaderMap) -> Value {
    let mut value = serde_json::Map::new();
    for name in headers.keys() {
        let hidden = matches!(
            name.as_str(),
            "authorization" | "cookie" | "set-cookie" | "sec-websocket-key" | "x-oai-attestation"
        );
        value.insert(
            name.to_string(),
            json!(
                headers
                    .get_all(name)
                    .iter()
                    .map(|v| if hidden {
                        "[redacted]".to_owned()
                    } else {
                        v.to_str().unwrap_or("[non-UTF8]").to_owned()
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    value.into()
}

fn redacted_text(text: &str) -> String {
    format!(
        "[native context redacted; bytes={}; sha256={}]",
        text.len(),
        hex::encode(Sha256::digest(text.as_bytes()))
    )
}

fn redact_context(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "text" | "description") {
                    if let Some(text) = value.as_str() {
                        if !text.starts_with("WIRE_") && text != "Wire verified" && text.len() > 128
                        {
                            *value = redacted_text(text).into();
                        }
                    }
                }
                redact_context(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_context),
        _ => {}
    }
}

fn declared_tool(body: &Value, name: &str) -> bool {
    fn has(value: &Value, name: &str) -> bool {
        value.as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["name"] == name || has(&tool["tools"], name))
        })
    }
    has(&body["tools"], name)
        || body["input"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "additional_tools" && has(&item["tools"], name))
        })
}

async fn capture_ingress(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if !path.starts_with("/v1/") {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, crate::media::BODY_LIMIT).await.unwrap();
    record(json!({"hop":"client-to-bridge", "transport":"http",
        "method":parts.method.as_str(), "path":path, "version":format!("{:?}",parts.version),
        "headers":headers_value(&parts.headers), "receivedAtMs":crate::now_ms(),"bodyBytes":bytes.len(),"bodySha256":hex::encode(Sha256::digest(&bytes)),
        "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}}));
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

#[derive(Clone)]
struct Model {
    trace: Trace,
    calls: Arc<Mutex<usize>>,
    history: Arc<Mutex<HashMap<String, Value>>>,
}

impl Model {
    fn events(&self, body: &Value) -> Vec<Value> {
        let n = {
            let mut count = self.calls.lock().unwrap();
            *count += 1;
            *count
        };
        // A WS continuation inherits tools/history from the referenced response,
        // including generate=false warmups. Capture the raw delta unchanged.
        let mut resolved = body.clone();
        if let Some(previous) = body["previous_response_id"].as_str() {
            let history = self.history.lock().unwrap();
            let previous = history.get(previous).expect("unknown fixture response id");
            let mut input = previous["input"].as_array().unwrap().clone();
            input.extend(body["input"].as_array().unwrap().clone());
            resolved["input"] = input.into();
            if body.get("tools").is_none() {
                resolved["tools"] = previous["tools"].clone();
            }
        }
        let body = &resolved;
        let input = body["input"].as_array().expect("upstream input array");
        if input.iter().any(|i| {
            i["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p["text"] == "WIRE_SECOND_TEXT"))
        }) {
            for marker in ["WIRE_HTTP_JSON", "WIRE_SECOND_TEXT"] {
                assert_eq!(
                    input
                        .iter()
                        .filter(|i| i["role"] == "user"
                            && i["content"]
                                .as_array()
                                .is_some_and(|parts| parts.iter().any(|p| p["text"] == marker)))
                        .count(),
                    1,
                    "each user turn reaches the model once"
                );
            }
            assert_eq!(
                input
                    .iter()
                    .filter(
                        |i| i["role"] == "assistant" && i["content"][0]["text"] == "Wire verified"
                    )
                    .count(),
                1
            );
            assert_eq!(body["reasoning"]["effort"], "high");
        }
        let wants_tool = input.iter().any(|v| v.to_string().contains("WIRE_TOOL"));
        let output = input.iter().find(|v| v["type"] == "function_call_output");
        let tool = wants_tool && output.is_none();
        self.trace.lock().unwrap().push(json!({"hop":"model-fixture-evaluation",
            "responseId":format!("resp_wire_{n}"),"previousResponseId":body["previous_response_id"],
            "wantsTool":wants_tool,"toolDeclaredInEffectiveContext":declared_tool(body,"client_echo")}));
        if wants_tool {
            assert!(
                declared_tool(body, "client_echo"),
                "tool missing from actual upstream request"
            );
        }
        if let Some(output) = output {
            assert_eq!(output["output"], "WIRE_CLIENT_RESULT");
        }
        let warmup = body["generate"] == false;
        let item = if tool {
            json!({"type":"function_call","id":format!("fc_wire_{n}"),
                "call_id":"wire_call", "name":"client_echo", "arguments":"{\"text\":\"wire\"}"})
        } else {
            json!({"type":"message","id":format!("msg_wire_{n}"),"role":"assistant",
                "status":"completed","content":[{"type":"output_text","text":"Wire verified","annotations":[]}]})
        };
        let response = json!({"id":format!("resp_wire_{n}"),"object":"response",
            "status":"in_progress","output":[],"model":"gpt-6.1-sol"});
        let mut events = vec![json!({"type":"response.created","response":response})];
        if !tool && !warmup {
            let id = &item["id"];
            events.extend([
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":id,"role":"assistant","status":"in_progress","content":[]}}),
                json!({"type":"response.content_part.added","output_index":0,"item_id":id,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":id,"content_index":0,"delta":"Wire "}),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":id,"content_index":0,"delta":"verified"}),
            ]);
        }
        if !warmup {
            events.push(json!({"type":"response.output_item.done","output_index":0,"item":item}));
        }
        events.push(json!({"type":"response.completed","response":{"id":format!("resp_wire_{n}"),
            "object":"response","status":"completed","output":if warmup {json!([])}else{json!([item])},
            "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}));
        for (i, event) in events.iter_mut().enumerate() {
            event["sequence_number"] = i.into();
        }
        let mut cached = resolved;
        if !warmup {
            cached["input"].as_array_mut().unwrap().push(item);
        }
        self.history
            .lock()
            .unwrap()
            .insert(format!("resp_wire_{n}"), cached);
        events
    }
}

async fn model_http(State(s): State<Model>, headers: HeaderMap, bytes: Bytes) -> Response {
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_WIRE_ONLY");
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    s.trace
        .lock()
        .unwrap()
        .push(json!({"hop":"app-server-to-model","transport":"http",
        "method":"POST","path":"/v1/responses","headers":headers_value(&headers),
        "receivedAtMs":crate::now_ms(),"bodyBytes":bytes.len(),"bodySha256":hex::encode(Sha256::digest(&bytes)),"body":body}));
    let data = s
        .events(&body)
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    ([("content-type", "text/event-stream")], data).into_response()
}

async fn model_ws(State(s): State<Model>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_WIRE_ONLY");
    s.trace.lock().unwrap().push(json!({"hop":"app-server-to-model","transport":"http",
        "method":"GET","path":"/v1/responses","headers":headers_value(&headers),"upgrade":"websocket"}));
    ws.on_upgrade(move |mut socket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                axum::extract::ws::Message::Text(text) => {
                    let body: Value = serde_json::from_str(&text).unwrap();
                    s.trace
                        .lock()
                        .unwrap()
                        .push(json!({"hop":"app-server-to-model",
                        "transport":"websocket","receivedAtMs":crate::now_ms(),"bodyBytes":text.len(),"bodySha256":hex::encode(Sha256::digest(text.as_bytes())),"body":body}));
                    assert_eq!(body["type"], "response.create");
                    for event in s.events(&body) {
                        if socket
                            .send(axum::extract::ws::Message::Text(event.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                axum::extract::ws::Message::Ping(_) | axum::extract::ws::Message::Pong(_) => {}
                _ => break,
            }
        }
    })
}

async fn post_json(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    key: &str,
    body: Value,
) -> Value {
    let response = client
        .post(format!("{base}{path}"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    let value: Value =
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{path} {status}: {text}"));
    assert!(status.is_success(), "{path} {status}: {value}");
    value
}

async fn native_client(base: &str, key: &str, trace: &Trace) {
    let mut request = format!("{}/v1/native", base.replacen("http://", "ws://", 1))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    request
        .headers_mut()
        .insert("user-agent", "codex-relay-wire-client/1".parse().unwrap());
    let (mut socket, handshake) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(handshake.status(), 101);
    trace
        .lock()
        .unwrap()
        .push(json!({"hop":"bridge-to-client","transport":"http",
        "path":"/v1/native","status":101,"headers":headers_value(handshake.headers())}));
    socket.send(Message::Text(json!({"id":1,"method":"thread/start","params":{
        "model":"gpt-6.1-sol","dynamicTools":[{"type":"function","name":"client_echo",
        "description":"Synthetic caller function","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}).to_string().into())).await.unwrap();
    let mut tool_seen = false;
    let mut completed = false;
    while let Some(message) = socket.next().await {
        let message = message.unwrap();
        let Message::Text(text) = message else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        trace
            .lock()
            .unwrap()
            .push(json!({"hop":"bridge-to-client","transport":"websocket","message":event}));
        assert_ne!(event["method"], "bridge/error", "{event}");
        assert!(event.get("error").is_none(), "{event}");
        if event["id"] == 1 && event.get("result").is_some() {
            socket
                .send(Message::Text(
                    json!({"id":2,"method":"turn/start","params":{
                "input":[{"type":"text","text":"WIRE_TOOL native","text_elements":[]}]}})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        if event["method"] == "item/tool/call" {
            assert_eq!(event["params"]["tool"], "client_echo");
            assert_eq!(event["params"]["arguments"], json!({"text":"wire"}));
            tool_seen = true;
            socket
                .send(Message::Text(
                    json!({"id":event["id"],"result":{
                "success":true,"contentItems":[{"type":"inputText","text":"WIRE_CLIENT_RESULT"}]}})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        if event["method"] == "turn/completed" {
            assert_eq!(event["params"]["turn"]["status"], "completed", "{event}");
            completed = true;
            socket
                .send(Message::Text(
                    json!({"id":3,"method":"session/close"}).to_string().into(),
                ))
                .await
                .unwrap();
        }
        if event["id"] == 3 && event.get("result").is_some() {
            break;
        }
    }
    assert!(
        tool_seen && completed,
        "native WS did not finish tool round trip"
    );
    let _ = socket.close(None).await;
}

fn assert_output(response: &Value) {
    assert_eq!(response["status"], "completed", "{response}");
    assert_eq!(response["output"][0]["content"][0]["text"], "Wire verified");
    assert_eq!(response["usage"]["total_tokens"], 12);
}

fn check_case(case: &Value, configured: &crate::installation::NativeClientInfo) -> Value {
    let trace = case["trace"].as_array().unwrap();
    let rpc: Vec<_> = trace
        .iter()
        .filter(|e| e["hop"] == "bridge-to-app-server")
        .map(|e| &e["message"])
        .collect();
    let initializers: Vec<_> = rpc.iter().filter(|e| e["method"] == "initialize").collect();
    let upstream: Vec<_> = trace
        .iter()
        .filter(|e| e["hop"] == "app-server-to-model" && e["body"].is_object())
        .collect();
    let generations: Vec<_> = upstream
        .iter()
        .filter(|e| e["body"]["generate"] != false)
        .collect();
    let http: Vec<_> = trace
        .iter()
        .filter(|e| e["hop"] == "app-server-to-model" && e["headers"].is_object())
        .collect();
    let first = &generations[0]["body"];
    let mut checks = vec![];
    let mut check = |name: &str, passed: bool| checks.push(json!({"check":name,"passed":passed}));
    check(
        "initialize preserves the existing configured clientInfo",
        !initializers.is_empty()
            && initializers
                .iter()
                .all(|e| e["params"]["clientInfo"] == json!(configured)),
    );
    check(
        "initialize acknowledged before thread/turn methods",
        initializers.len() == rpc.iter().filter(|e| e["method"] == "initialized").count()
            && rpc
                .windows(2)
                .all(|p| p[0]["method"] != "initialize" || p[1]["method"] == "initialized"),
    );
    check(
        "only implemented initialization capabilities advertised",
        initializers.iter().all(|e| {
            e["params"]["capabilities"]
                == json!({"experimentalApi":true,"requestAttestation":false})
        }),
    );
    check(
        "upstream originator follows existing configuration and User-Agent is native",
        http.iter().all(|e| {
            e["headers"]["originator"][0] == configured.name
                && e["headers"]["user-agent"][0]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s != "codex-relay-wire-client/1")
        }),
    );
    check(
        "no upstream device proof injected",
        http.iter()
            .all(|e| e["headers"].get("x-oai-attestation").is_none()),
    );
    check(
        "native request model, input, stream and store semantics",
        generations.iter().all(|e| {
            e["body"]["model"] == "gpt-6.1-sol"
                && e["body"]["input"].is_array()
                && e["body"]["stream"] == true
                && e["body"]["store"] == false
        }),
    );
    check(
        "reasoning effort and text verbosity reach the network",
        first["reasoning"]["effort"] == "high" && first["text"]["verbosity"] == "low",
    );
    check(
        "caller instructions reach the network",
        upstream.iter().any(|e| {
            e["body"]["input"].as_array().unwrap().iter().any(|i| {
                i["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|p| p["text"] == "WIRE_INSTRUCTIONS"))
            })
        }),
    );
    check(
        "HTTP continuation includes real client tool output",
        generations.iter().any(|e| {
            e["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["type"] == "function_call_output" && i["output"] == "WIRE_CLIENT_RESULT")
        }),
    );
    check(
        "function declarations present in effective context including WS inheritance",
        trace
            .iter()
            .filter(|e| e["hop"] == "model-fixture-evaluation")
            .all(|e| e["wantsTool"] != true || e["toolDeclaredInEffectiveContext"] == true),
    );
    check(
        "additional_tools items have the documented developer role and tool array",
        upstream.iter().all(|e| {
            e["body"]["input"].as_array().unwrap().iter().all(|i| {
                i["type"] != "additional_tools"
                    || (i["role"] == "developer" && i["tools"].is_array())
            })
        }),
    );
    check(
        "HTTP JSON and SSE and native WS complete",
        trace.iter().any(|e| e["transport"] == "http-sse")
            && trace.iter().any(|e| {
                e["hop"] == "bridge-to-client" && e["message"]["method"] == "turn/completed"
            }),
    );
    if case["mode"] == "websocket" {
        check(
            "upstream WebSocket handshake and response.create frames observed",
            http.iter()
                .any(|e| e["method"] == "GET" && e["headers"]["upgrade"][0] == "websocket")
                && upstream.iter().all(|e| {
                    e["transport"] == "websocket" && e["body"]["type"] == "response.create"
                }),
        );
        check(
            "WS continuation carries previous_response_id",
            generations
                .iter()
                .any(|e| e["body"]["previous_response_id"].is_string()),
        );
    } else {
        check(
            "upstream HTTP POST uses application/json",
            http.iter().all(|e| {
                e["method"] == "POST" && e["headers"]["content-type"][0] == "application/json"
            }),
        );
    }
    let ws_stream = upstream
        .iter()
        .filter(|e| e["transport"] == "websocket" && e["body"].get("stream").is_some())
        .count();
    let additional_tools = upstream
        .iter()
        .filter(|e| {
            e["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["type"] == "additional_tools")
        })
        .count();
    json!({"mode":case["mode"],"checks":checks,"initializeRequests":initializers.len(),
        "upstreamCreateMessages":upstream.len(),"generationRequests":generations.len(),
        "warmupMessages":upstream.len()-generations.len(),
        "observations":{"messagesWithDocumentedAdditionalToolsInput":additional_tools},
        "nativeDifferencesFromPublicResponses":{"wsFramesContainingStream":ws_stream,
            "action":"WS stream field differs from the public WebSocket guide. The bridge does not rewrite app-server transport."}})
}

async fn run_case(mode: &str) -> Value {
    let trace: Trace = Default::default();
    *ACTIVE.get_or_init(Default::default).lock().unwrap() = Some(trace.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_port = listener.local_addr().unwrap().port();
    let model = Model {
        trace: trace.clone(),
        calls: Default::default(),
        history: Default::default(),
    };
    let model_app = Router::new()
        .route("/v1/responses", post(model_http).get(model_ws))
        .with_state(model.clone());
    let model_server = tokio::spawn(async move {
        axum::serve(listener, model_app).await.unwrap();
    });
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
model="gpt-6.1-sol"
model_provider="synthetic_wire"
model_reasoning_effort="medium"
[features]
enable_request_compression=false
[model_providers.synthetic_wire]
name="Synthetic wire fixture"
base_url="http://127.0.0.1:{model_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets={}
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_WIRE_ONLY"
"#,
            mode == "websocket"
        ),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let admin_token = "a".repeat(64);
    let state = crate::admin::AdminState::new(
        port,
        PathBuf::from(format!("accounts-{mode}")),
        admin_token.clone(),
    )
    .await
    .unwrap();
    let app = crate::admin::router(state.clone(), PathBuf::from("web"))
        .layer(middleware::from_fn(capture_ingress));
    let bridge_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .user_agent("codex-relay-wire-client/1")
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let work = async {
        let account = post_json(
            &client,
            &base,
            "/api/accounts",
            &admin_token,
            json!({"name":"Wire fixture","nativeConfig":true}),
        )
        .await;
        let revision = client
            .get(format!("{base}/api/operations"))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["revision"]
            .clone();
        post_json(
            &client,
            &base,
            &format!("/api/accounts/{}/route", account["id"].as_str().unwrap()),
            &admin_token,
            json!({"revision":revision,"enabled":true}),
        )
        .await;
        eprintln!("{mode}: native route inspected");
        let key = post_json(
            &client,
            &base,
            "/api/api-keys",
            &admin_token,
            json!({"name":"Wire caller","groupId":"00000000-0000-0000-0000-000000000000"}),
        )
        .await["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        let unauthorized = client
            .post(format!("{base}/v1/responses"))
            .json(&json!({"model":"gpt-6.1-sol","input":"WIRE_UNAUTHORIZED"}))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(unauthorized.json::<Value>().await.unwrap()["error"]["type"].is_string());
        let response = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(&key)
            .json(&json!({
            "model":"gpt-6.1-sol","input":"WIRE_HTTP_JSON","stream":false,"instructions":"WIRE_INSTRUCTIONS",
            "reasoning":{"effort":"high"},"text":{"verbosity":"low"},"store":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let headers = headers_value(response.headers());
        let plain: Value = response.json().await.unwrap();
        assert_output(&plain);
        assert_eq!(plain["store"], false);
        eprintln!("{mode}: HTTP JSON completed");
        trace.lock().unwrap().push(json!({"hop":"bridge-to-client","transport":"http","status":200,"headers":headers,"body":plain}));
        let before = trace.lock().unwrap().clone();
        let mut history = vec![json!({"role":"user","content":"WIRE_HTTP_JSON"})];
        history.extend(plain["output"].as_array().unwrap().clone());
        history.push(json!({"role":"user","content":"WIRE_SECOND_TEXT"}));
        let continuation = post_json(&client, &base, "/v1/responses", &key,
            json!({"model":"gpt-6.1-sol","input":history,"stream":false,
                "instructions":"WIRE_INSTRUCTIONS","reasoning":{"effort":"high"},"text":{"verbosity":"low"},"store":false})).await;
        assert_output(&continuation);
        let after = trace.lock().unwrap().clone();
        for method in ["initialize", "thread/start"] {
            let count = |events: &[Value]| {
                events
                    .iter()
                    .filter(|e| {
                        e["hop"] == "bridge-to-app-server" && e["message"]["method"] == method
                    })
                    .count()
            };
            assert_eq!(
                count(&before),
                count(&after),
                "text continuation must reuse {method}"
            );
        }
        if mode == "websocket" {
            let connections = |events: &[Value]| {
                events
                    .iter()
                    .filter(|e| {
                        e["hop"] == "app-server-to-model"
                            && e["headers"]["upgrade"][0] == "websocket"
                    })
                    .count()
            };
            assert_eq!(
                connections(&before),
                connections(&after),
                "continuation must reuse the native WebSocket connection"
            );
        }
        eprintln!("{mode}: text continuation reused process and thread");
        let tools = json!([{"type":"function","name":"client_echo","description":"Synthetic caller function","parameters":{"type":"object","properties":{"text":{"type":"string"}}},"strict":false}]);
        let response = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(&key)
            .header("accept", "text/event-stream")
            .json(&json!({
            "model":"gpt-6.1-sol","input":"WIRE_TOOL http","stream":true,"tools":tools}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        let headers = headers_value(response.headers());
        let text = response.text().await.unwrap();
        let events: Vec<Value> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events.first().unwrap()["type"],
            "response.created",
            "first event: {}",
            events.first().unwrap()
        );
        assert_eq!(events.last().unwrap()["type"], "response.completed");
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], i);
        }
        let first = &events.last().unwrap()["response"];
        let call = &first["output"][0];
        assert_eq!(call["type"], "function_call");
        eprintln!("{mode}: HTTP SSE tool received");
        assert_eq!(
            state.operations.snapshot()["active"],
            0,
            "waiting for a client tool must release admission"
        );
        let tool_before = trace.lock().unwrap().clone();
        trace.lock().unwrap().push(json!({"hop":"bridge-to-client","transport":"http-sse","headers":headers,"events":events}));
        let mut history = vec![json!({"role":"user","content":"WIRE_TOOL http"})];
        history.extend(first["output"].as_array().unwrap().clone());
        history.push(json!({"type":"function_call_output","call_id":call["call_id"],"output":"WIRE_CLIENT_RESULT"}));
        let second = post_json(
            &client,
            &base,
            "/v1/responses",
            &key,
            json!({"model":"gpt-6.1-sol",
            "input":history,"tools":tools,"store":false,"stream":false}),
        )
        .await;
        assert_output(&second);
        eprintln!("{mode}: HTTP tool continuation completed");
        if crate::native_cache::enabled() {
            let tool_after = trace.lock().unwrap().clone();
            for method in ["thread/start", "turn/start", "turn/interrupt"] {
                let count = |events: &[Value]| {
                    events
                        .iter()
                        .filter(|e| {
                            e["hop"] == "bridge-to-app-server" && e["message"]["method"] == method
                        })
                        .count()
                };
                assert_eq!(
                    count(&tool_before),
                    count(&tool_after),
                    "tool continuation must preserve {method}"
                );
            }
            let cache_key = |events: &[Value]| {
                events
                    .iter()
                    .rev()
                    .find_map(|e| e["body"]["prompt_cache_key"].as_str().map(str::to_owned))
            };
            assert!(cache_key(&tool_before).is_some());
            assert_eq!(
                cache_key(&tool_before),
                cache_key(&tool_after),
                "native tool cache identity must remain stable"
            );
        }
        native_client(&base, &key, &trace).await;
        eprintln!("{mode}: native WS tool round trip completed");
        for _ in 0..100 {
            if state.operations.snapshot()["active"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(state.operations.snapshot()["active"], 0);
    };
    // Cleanup runs on timeout too; the parent process owns all these workers.
    let result = tokio::time::timeout(
        Duration::from_secs(150),
        std::panic::AssertUnwindSafe(work).catch_unwind(),
    )
    .await;
    state.shutdown.cancel();
    state.profiles.shutdown().await;
    bridge_server.abort();
    model_server.abort();
    *ACTIVE.get().unwrap().lock().unwrap() = None;
    if let Err(panic) = result.expect("wire scenario timed out") {
        std::panic::resume_unwind(panic);
    }
    let trace = trace.lock().unwrap().clone();
    json!({"mode":mode,"modelRequests":*model.calls.lock().unwrap(),"trace":trace})
}

#[tokio::test]
#[ignore = "Opt-in real HTTP/WS + installed official app-server; synthetic local model only"]
async fn captures_real_http_ws_and_native_initialization() {
    if std::env::var_os("BRIDGE_WIRE_CAPTURE_CHILD").is_none() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let installation = PathBuf::from(
            std::env::var_os("BRIDGE_NATIVE_INSTALLATION")
                .expect("set BRIDGE_NATIVE_INSTALLATION to the prepared launch-config.json"),
        );
        let report = root.join(".cache/wire-protocol-capture.json");
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join(".cache")).unwrap();
        std::fs::create_dir(temporary.path().join("home")).unwrap();
        std::fs::copy(
            installation,
            temporary.path().join(".cache/launch-config.json"),
        )
        .unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "wire_capture::captures_real_http_ws_and_native_initialization",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(temporary.path())
            .env("BRIDGE_WIRE_CAPTURE_CHILD", "1")
            .env("BRIDGE_WIRE_REPORT", &report)
            .kill_on_drop(true);
        for (name, _) in std::env::vars_os() {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            if [
                "CODEX_",
                "OPENAI_",
                "CHATGPT_",
                "ANTHROPIC_",
                "AZURE_OPENAI_",
            ]
            .iter()
            .any(|p| upper.starts_with(p))
                || matches!(
                    upper.as_str(),
                    "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
                )
            {
                command.env_remove(name);
            }
        }
        command
            .env("CODEX_HOME", temporary.path().join("home"))
            .env("NO_PROXY", "127.0.0.1,localhost,::1");
        let status = command.status().await.unwrap();
        assert!(
            status.success(),
            "network capture failed; report (if written): {}",
            report.display()
        );
        eprintln!("real network capture: {}", report.display());
        return;
    }
    let installation = crate::installation::read().await.unwrap();
    crate::installation::validate(&installation).await.unwrap();
    let mut cases = vec![];
    for mode in ["http", "websocket"] {
        eprintln!("real network capture: running upstream {mode}");
        cases.push(run_case(mode).await);
    }
    let configured = installation.client_info().unwrap();
    let comparisons: Vec<_> = cases
        .iter()
        .map(|case| check_case(case, configured))
        .collect();
    for case in &mut cases {
        redact_context(case);
    }
    let report = json!({"capturedAtMs":crate::now_ms(),"network":"real loopback TCP sockets",
        "nativeExecutable":installation.app_server_executable,"nativeSha256":installation.app_server_sha256,
        "nativeVersion":installation.app_server_version,"cases":cases,"comparisons":comparisons,
        "configuredClientInfo":configured,"clientInfoPolicy":"Preserved without modification; no identity authorization or upstream acceptance claim.",
        "redaction":"Credential header values and long native-added context texts redacted. Original body byte lengths and SHA-256 are retained.",
        "captureLevel":"HTTP semantic headers and JSON bodies, decoded WebSocket messages, successful stdio writes. Header case/order, masking and TLS bytes are not captured.",
        "realModelRequests":0,"validDeviceAttestationGenerated":false,
        "limits":["Local plaintext HTTP/WS, no TLS fingerprint measurement.",
            "Official app-server binary and actual bridge routers; no live desktop UI session.",
            "Initialization captured after successful pipe writes, not inferred from a mock.",
            "Synthetic model responses do not establish OpenAI service acceptance."],
        "sources":["https://learn.chatgpt.com/docs/app-server#initialization",
            "https://developers.openai.com/api/docs/guides/websocket-mode",
            "https://developers.openai.com/api/docs/guides/tools-tool-search#add-tools-at-a-specific-point-in-the-input",
            "https://developers.openai.com/api/reference/resources/responses/methods/create"]});
    let output = PathBuf::from(std::env::var_os("BRIDGE_WIRE_REPORT").unwrap());
    std::fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    for comparison in report["comparisons"].as_array().unwrap() {
        for check in comparison["checks"].as_array().unwrap() {
            assert_eq!(
                check["passed"], true,
                "{}: {}",
                comparison["mode"], check["check"]
            );
        }
    }
}
