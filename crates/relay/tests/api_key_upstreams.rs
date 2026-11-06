use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        Request, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, DEFAULT_GROUP, StatsQuery},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;

#[derive(Default)]
struct Upstream {
    calls: Mutex<Vec<(String, String, Bytes)>>,
    retrievals: Mutex<Vec<Retrieval>>,
    response_tier: Mutex<Value>,
    response_content_type: Mutex<Option<String>>,
    response_override: Mutex<Option<String>>,
    sse_layout: Mutex<Option<(String, String)>>,
    sse_keep_open: AtomicBool,
    sse_status: Mutex<Option<String>>,
    ws_handshake: Mutex<Option<Arc<HandshakeGate>>>,
    sockets: AtomicUsize,
    ids: AtomicUsize,
    ws_status: AtomicUsize,
}
#[derive(Default)]
struct HandshakeGate {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
struct Retrieval {
    method: Method,
    target: String,
    headers: HeaderMap,
}
fn with_response_tier(state: &Upstream, mut response: Value) -> Value {
    let tier = state.response_tier.lock().unwrap().clone();
    if !tier.is_null() {
        response["service_tier"] = tier;
    }
    response
}
fn automatic_compaction_output(request: &Value, id: &str) -> Value {
    if request["input"] == "plain-history-fixture" {
        json!([{"type":"message","id":format!("msg_{id}"),"role":"assistant","content":[{"type":"output_text","text":"plain answer"}]}])
    } else if request["context_management"].is_array() {
        // This fixture tests transport preservation, not a provider's tokenizer
        // or decision to compact. Unknown fields must survive as well.
        json!([{"type":"compaction","id":format!("cmp_{id}"),
            "encrypted_content":format!("synthetic-opaque-{id}"),"vendor_compaction":{"kept":true}}])
    } else {
        json!([])
    }
}
async fn http(State(state): State<Arc<Upstream>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let stream_retrieval = request.method() == Method::GET
        && request
            .uri()
            .query()
            .is_some_and(|query| query.split('&').any(|pair| pair == "stream=true"))
        && request
            .headers()
            .get("accept")
            .is_some_and(|v| v == "text/event-stream");
    if matches!(*request.method(), Method::GET | Method::DELETE)
        && path.starts_with("/prefix/v1/responses/")
    {
        state.retrievals.lock().unwrap().push(Retrieval {
            method: request.method().clone(),
            target: request.uri().path_and_query().unwrap().to_string(),
            headers: request.headers().clone(),
        });
    }
    let auth = request
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(request.headers().get("cookie").is_none());
    if path.ends_with("/alpha/search") {
        assert_eq!(request.headers().get_all("content-type").iter().count(), 1);
    }
    let bytes = to_bytes(request.into_body(), 32 * 1024 * 1024)
        .await
        .unwrap();
    state
        .calls
        .lock()
        .unwrap()
        .push((path.clone(), auth, bytes.clone()));
    if path.ends_with("/models") {
        if path.contains("/no-models/") {
            return StatusCode::NOT_FOUND.into_response();
        }
        return Json(json!({"data":[{"id":"test-model"}]})).into_response();
    }
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some(code) = value["model"]
        .as_str()
        .and_then(|s| s.strip_prefix("error-"))
        .and_then(|s| s.parse::<u16>().ok())
    {
        return (
            StatusCode::from_u16(code).unwrap(),
            [("retry-after", "7")],
            Json(json!({"error":{"code":"upstream_exact","message":"original error"}})),
        )
            .into_response();
    }
    if path.ends_with("/alpha/search") {
        if value["commands"]["test_mode"] == "malformed" {
            return (StatusCode::OK, "not json").into_response();
        }
        if value["commands"]["test_mode"] == "slow" {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        return Json(json!({"output":"search result","encrypted_output":"OPAQUE_SEARCH","results":[{"ref_id":"turn0search0","url":"https://example.com","future_field":true}],"unknown":{"kept":true}})).into_response();
    }
    if let Some(body) = state.response_override.lock().unwrap().clone() {
        return ([("content-type", "application/json")], body).into_response();
    }
    let id = format!("resp_{}", state.ids.fetch_add(1, Ordering::SeqCst));
    let mut response = with_response_tier(
        &state,
        json!({"id":id,"object":"response","status":"completed","model":"test-model","output":automatic_compaction_output(&value,&id),"usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6},"upstream_extension":{"kept":true}}),
    );
    if let Some(status) = state.sse_status.lock().unwrap().as_ref() {
        response["status"] = json!(status);
    }
    if value["input"] == "latency-fixture" {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    if value["stream"] == true || stream_retrieval {
        let compact = response["output"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                format!(
                    "data: {}\n\n",
                    json!({"type":"response.output_item.done","output_index":0,"item":item})
                )
            })
            .collect::<String>();
        let content_type = state
            .response_content_type
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "text/event-stream".into());
        let mut body = format!(
            "event: vendor.extension\ndata: {{\"type\":\"vendor.extension\",\"extra\":7}}\n\n{compact}data: {{\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}}\n\ndata: {}\n\n",
            json!({"type":format!("response.{}", response["status"].as_str().unwrap()),"response":response})
        );
        if let Some((line, blank)) = state.sse_layout.lock().unwrap().as_ref() {
            body = format!(
                "\u{feff}{}",
                body.split("\n\n")
                    .filter(|frame| !frame.is_empty())
                    .map(|frame| {
                        format!(
                            "{}{line}: comment{blank}{blank}",
                            frame.split('\n').collect::<Vec<_>>().join(line)
                        )
                    })
                    .collect::<String>()
            );
        }
        let body = if state.sse_keep_open.load(Ordering::SeqCst) {
            use futures_util::StreamExt;
            Body::from_stream(
                futures_util::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(Bytes::from(body))
                })
                .chain(futures_util::stream::pending()),
            )
        } else {
            Body::from(body)
        };
        return (
            [
                ("content-type", content_type.as_str()),
                ("x-upstream-extension", "yes"),
            ],
            body,
        )
            .into_response();
    }
    Json(if path.contains("/images/"){json!({"data":[{"b64_json":"aGVsbG8="}],"vendor":true})}else if path.ends_with("/compact"){with_response_tier(&state,json!({"object":"response.compaction","output":[{"type":"compaction","encrypted_content":"opaque"}]}))}else{response}).into_response()
}
async fn websocket(State(state): State<Arc<Upstream>>, ws: WebSocketUpgrade) -> Response {
    let gate = state.ws_handshake.lock().unwrap().clone();
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.resume.notified().await;
    }
    let status = state.ws_status.load(Ordering::SeqCst);
    if status != 0 {
        return StatusCode::from_u16(status as u16).unwrap().into_response();
    }
    state.sockets.fetch_add(1, Ordering::SeqCst);
    ws.on_upgrade(move|mut socket|async move {
        while let Some(Ok(Message::Text(text)))=socket.recv().await {
            let value:Value=serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"],"response.create");assert!(value.get("stream").is_none());
            state.calls.lock().unwrap().push(("ws".into(),"Bearer upstream-test-secret".into(),Bytes::from(text.to_string())));
            if value["model"]=="disconnect" {break;}
            let id=format!("ws_{}",state.ids.fetch_add(1,Ordering::SeqCst));
            let response=with_response_tier(&state,json!({"id":id,"status":"completed","model":"test-model","output":automatic_compaction_output(&value,&id),"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"vendor":7}));
            if socket.send(Message::Text(json!({"type":"response.created","response":response}).to_string().into())).await.is_err(){break;}
            for item in response["output"].as_array().unwrap() {
                if socket.send(Message::Text(json!({"type":"response.output_item.done","output_index":0,"item":item}).to_string().into())).await.is_err(){return;}
            }
            if socket.send(Message::Text(json!({"type":"response.completed","response":response}).to_string().into())).await.is_err(){break;}
        }
    })
}
struct Fixture {
    _root: tempfile::TempDir,
    upstream: Arc<Upstream>,
    task: tokio::task::JoinHandle<()>,
    state: Arc<AdminState>,
    app: Router,
    base: String,
    account: String,
    key: String,
}

#[tokio::test]
async fn standalone_search_preserves_protocol_credentials_and_records_every_outcome() {
    // ws-first 也必须对独立 Search 使用 HTTP，不能把 commands 改造成 Responses。
    let f = Fixture::new("ws-first").await;
    for path in ["/v1/alpha/search", "/v1/unknown-search-path"] {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", "127.0.0.1:4317")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().contains_key("x-request-id"));
    }
    let body = json!({"id":"search-session","model":"test-model","commands":{"search_query":[{"q":"PRIVATE_QUERY"}]},
        "input":[{"role":"user","content":"PRIVATE_CONTEXT"}],"settings":{"external_web_access":true},"reasoning":{"effort":"high"},"future_field":42});
    let response = f
        .request(
            "POST",
            "/v1/alpha/search",
            body.to_string(),
            "application/json; charset=utf-8",
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let output: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(output["encrypted_output"], "OPAQUE_SEARCH");
    assert_eq!(output["results"][0]["ref_id"], "turn0search0");
    assert_eq!(output["unknown"]["kept"], true);
    {
        let calls = f.upstream.calls.lock().unwrap();
        let (path, auth, bytes) = calls.last().unwrap();
        assert_eq!(path, "/prefix/v1/alpha/search");
        assert_eq!(auth, "Bearer upstream-test-secret");
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), body);
    }
    let records = f.state.operations.statistics(Default::default()).unwrap();
    let row = records["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["action"], "web.search");
    assert_eq!(row["status"], "succeeded");
    assert!(!records.to_string().contains("PRIVATE_QUERY"));
    let continued = json!({"id":"search-session","model":"test-model","commands":{"open":[{"ref_id":"turn0search0"}]},"input":[{"encrypted_content":"OPAQUE_SEARCH"}]});
    assert_eq!(
        f.request(
            "POST",
            "/v1/alpha/search",
            continued.to_string(),
            "application/json"
        )
        .await
        .status(),
        StatusCode::OK
    );
    let changed = json!({"id":"search-session","model":"another-model","commands":{}});
    assert_eq!(
        f.request(
            "POST",
            "/v1/alpha/search",
            changed.to_string(),
            "application/json"
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    for (path, method, input, status, code) in [
        (
            "/v1/alpha/search",
            "POST",
            json!({"input":"PRIVATE_QUERY"}),
            400,
            "INVALID_INPUT",
        ),
        (
            "/v1/alpha/search",
            "GET",
            json!({}),
            405,
            "METHOD_NOT_ALLOWED",
        ),
        (
            "/v1/unsupported",
            "POST",
            json!({"secret":"PRIVATE_QUERY"}),
            404,
            "ENDPOINT_NOT_SUPPORTED",
        ),
        (
            "/v1/alpha/search",
            "POST",
            json!({"id":"error","model":"error-429","commands":{}}),
            429,
            "upstream_exact",
        ),
        (
            "/v1/alpha/search",
            "POST",
            json!({"id":"bad-response","model":"test-model","commands":{"test_mode":"malformed"}}),
            502,
            "UPSTREAM_INVALID_RESPONSE",
        ),
    ] {
        let response = f
            .request(method, path, input.to_string(), "application/json")
            .await;
        assert_eq!(response.status().as_u16(), status);
        let id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["code"], code);
        let saved = f.state.operations.request_content(id).unwrap();
        assert_eq!(saved["response"]["statusCode"], status);
        assert!(!saved.to_string().contains("PRIVATE_QUERY"));
    }
    assert_eq!(f.upstream.sockets.load(Ordering::SeqCst), 0);
    // 同一个搜索 ID 在其他 Key 下是独立会话，不继承原 Key 的模型或引用绑定。
    let other_key = f
        .state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"other search caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/alpha/search")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {other_key}"))
                .header("content-type", "application/json")
                .body(Body::from(changed.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn standalone_search_disconnect_cancels_without_replay_and_releases_capacity() {
    let f = Fixture::new("http-sse").await;
    let app = f.app.clone();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/alpha/search")
        .header("host", "127.0.0.1:4317")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"id":"cancel-search","model":"test-model","commands":{"test_mode":"slow"}})
                .to_string(),
        ))
        .unwrap();
    let running = tokio::spawn(async move { app.oneshot(request).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !f
            .upstream
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.0.ends_with("/alpha/search"))
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    running.abort();
    let _ = running.await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let rows = f.state.operations.statistics(Default::default()).unwrap();
            if rows["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["action"] == "web.search" && r["status"] == "cancelled")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.upstream
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.0.ends_with("/alpha/search"))
            .count(),
        1
    );
    assert_eq!(
        f.request(
            "POST",
            "/v1/alpha/search",
            json!({"id":"after-cancel","model":"test-model","commands":{}}).to_string(),
            "application/json"
        )
        .await
        .status(),
        StatusCode::OK
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(transport: &str) -> Self {
        let upstream = Arc::new(Upstream::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/prefix/v1", listener.local_addr().unwrap());
        let upstream_app = Router::new()
            .route("/prefix/v1/responses", post(http).get(websocket))
            .fallback(http)
            .with_state(upstream.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, root.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let app = router(state.clone(), root.path().join("web"));
        let key = state
            .operations
            .create_api_key(
                serde_json::from_value::<ApiKeyCreate>(
                    json!({"name":"caller","groupId":DEFAULT_GROUP}),
                )
                .unwrap(),
            )
            .unwrap()
            .secret;
        let mut fixture = Self {
            _root: root,
            upstream,
            task,
            state,
            app,
            base,
            account: String::new(),
            key,
        };
        let (status,body)=fixture.admin("/api/accounts",json!({"name":"direct","source":"api-key","upstream":{"baseUrl":format!("{}/",fixture.base),"apiKey":"upstream-test-secret","transport":transport}})).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        fixture.account = body["id"].as_str().unwrap().into();
        assert_eq!(
            fixture.state.operations.snapshot()["accounts"][0]["enabled"],
            false
        );
        assert!(fixture.upstream.calls.lock().unwrap().is_empty());
        fixture.activate().await;
        fixture
    }
    fn revision(&self) -> u64 {
        self.state.operations.snapshot()["revision"]
            .as_u64()
            .unwrap()
    }
    async fn admin(&self, path: &str, value: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", "127.0.0.1:4317")
                    .header("origin", "http://127.0.0.1:4317")
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&body)})),
        )
    }
    async fn activate(&self) {
        let (status, body) = self
            .admin(
                &format!("/api/accounts/{}/activation", self.account),
                json!({"revision":self.revision(),"enabled":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: impl Into<Body>,
        content_type: &str,
    ) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", self.key))
                    .header("content-type", content_type)
                    .header("cookie", "local-secret")
                    .body(body.into())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    async fn json(&self, path: &str, value: Value) -> (StatusCode, Value) {
        let r = self
            .request("POST", path, value.to_string(), "application/json")
            .await;
        let s = r.status();
        let bytes = to_bytes(r.into_body(), 64 * 1024 * 1024).await.unwrap();
        (s, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn context_management_and_compaction_preserve_upstream_semantics() {
    for transport in ["http-sse", "ws-first"] {
        for stream in [false, true] {
            let f = Fixture::new(transport).await;
            let policy = json!([{"type":"compaction","compact_threshold":200000}]);
            let request = json!({"model":"test-model","input":"hello","stream":stream,
                "store":false,"context_management":policy});
            let response = f
                .request(
                    "POST",
                    "/v1/responses",
                    request.to_string(),
                    "application/json",
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let response: Value = if stream {
                let text = std::str::from_utf8(&bytes).unwrap();
                let events = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let item = events
                    .iter()
                    .find(|e| e["type"] == "response.output_item.done")
                    .unwrap();
                assert_eq!(item["item"]["type"], "compaction");
                let terminal = events
                    .iter()
                    .find(|e| e["type"] == "response.completed")
                    .unwrap();
                assert_eq!(item["item"], terminal["response"]["output"][0]);
                terminal["response"].clone()
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            let item = &response["output"][0];
            assert_eq!(item["type"], "compaction");
            assert_eq!(item["vendor_compaction"]["kept"], true);
            assert_eq!(
                item["encrypted_content"],
                format!("synthetic-opaque-{}", response["id"].as_str().unwrap())
            );
            let captured: Value =
                serde_json::from_slice(&f.upstream.calls.lock().unwrap().last().unwrap().2)
                    .unwrap();
            assert_eq!(captured["context_management"], policy);
            assert_eq!(captured["store"], false);
            // A stateless caller can drop the pre-compaction history and replay
            // the opaque output. The relay must retain its account binding.
            let input = json!([item,{"role":"user","content":"continue"}]);
            let (status, _) = f
                .json(
                    "/v1/responses",
                    json!({"model":"test-model","input":input,"context_management":policy}),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            let captured: Value =
                serde_json::from_slice(&f.upstream.calls.lock().unwrap().last().unwrap().2)
                    .unwrap();
            assert_eq!(captured["input"], input);
            assert_eq!(captured["context_management"], policy);
        }
    }
}

#[tokio::test]
async fn direct_upstream_preserves_subagent_metadata_in_request_records() {
    let f = Fixture::new("http-sse").await;
    for kind in [Some("worker"), None] {
        let mut request = json!({"model":"test-model","input":"hello"});
        if let Some(kind) = kind {
            request["client_metadata"] = json!({"x-openai-subagent":kind});
        }
        let response = f
            .request(
                "POST",
                "/v1/responses",
                request.to_string(),
                "application/json",
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let records = f.state.operations.statistics(Default::default()).unwrap();
        let row = records["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        assert_eq!(row["subagentKind"], json!(kind));
        assert_eq!(row["endpoint"], "/v1/responses");
        assert_eq!(row["clientTransport"], "http");
        assert_eq!(row["upstreamTransport"], "http");
    }
}

#[tokio::test]
async fn direct_upstream_routes_and_models_work_through_a_secondary_group() {
    let mut f = Fixture::new("http-sse").await;
    let (status, group) = f
        .admin(
            "/api/groups",
            json!({"revision":f.revision(),"name":"secondary","enabled":true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = f.admin(&format!("/api/accounts/{}/policy", f.account), json!({"revision":f.revision(),"groupIds":[DEFAULT_GROUP,group["id"]],"enabled":true,"maxConcurrency":2})).await;
    assert_eq!(status, StatusCode::OK);
    f.key = f
        .state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"secondary caller","groupId":group["id"]}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let (status, _) = f
        .admin(
            &format!("/api/groups/{DEFAULT_GROUP}"),
            json!({"revision":f.revision(),"name":"default","enabled":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let models = f
        .request("GET", "/v1/models", Body::empty(), "application/json")
        .await;
    assert_eq!(models.status(), StatusCode::OK);
    let models: Value =
        serde_json::from_slice(&to_bytes(models.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(models["data"][0]["id"], "test-model");
    let (status, body) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"hello","tools":[{"type":"vendor_tool"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let query =
        serde_json::from_value::<StatsQuery>(json!({"groupId":group["id"],"kind":"inference"}))
            .unwrap();
    assert_eq!(
        f.state.operations.statistics(query).unwrap()["summary"]["succeeded"],
        1
    );
}

#[tokio::test]
async fn imports_are_disabled_and_http_preserves_parameters_errors_and_identity() {
    let f = Fixture::new("http-sse").await;
    let payload = json!({"model":"test-model","input":"hello","store":true,"temperature":0.7,"vendor_extension":{"nested":123},"tools":[{"type":"vendor_tool"}]});
    let (status, response) = f.json("/v1/responses", payload.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["upstream_extension"]["kept"], true);
    {
        let calls = f.upstream.calls.lock().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&calls.last().unwrap().2).unwrap(),
            payload
        );
        assert!(
            calls
                .iter()
                .all(|(_, auth, _)| auth == "Bearer upstream-test-secret")
        );
    }
    let view = serde_json::to_value(f.state.profiles.list().await).unwrap();
    assert!(!view.to_string().contains("upstream-test-secret"));
    assert!(view[0]["pid"].is_null());
    assert_eq!(view[0]["upstream"]["hasApiKey"], true);
    let models = f
        .request("GET", "/v1/models", Body::empty(), "application/json")
        .await;
    let models: Value =
        serde_json::from_slice(&to_bytes(models.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(models["data"][0]["id"], "test-model");
    for code in [401, 429, 500] {
        let r = f
            .request(
                "POST",
                "/v1/responses",
                json!({"model":format!("error-{code}"),"input":"x"}).to_string(),
                "application/json",
            )
            .await;
        assert_eq!(r.status().as_u16(), code);
        assert_eq!(r.headers()["retry-after"], "7");
        let v: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(v["error"]["code"], "upstream_exact");
    }
}
#[tokio::test]
async fn sse_compact_and_image_multipart_are_forwarded() {
    let f = Fixture::new("http-sse").await;
    let r = f
        .request(
            "POST",
            "/v1/responses",
            json!({"model":"test-model","input":"x","stream":true}).to_string(),
            "application/json",
        )
        .await;
    assert_eq!(r.headers()["x-upstream-extension"], "yes");
    let bytes = to_bytes(r.into_body(), 65536).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.starts_with("event: vendor.extension\n"));
    assert!(text.contains("upstream_extension"));
    let (s, v) = f
        .json(
            "/v1/responses/compact",
            json!({"model":"test-model","input":[],"custom":1}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["object"], "response.compaction");
    let (s, v) = f
        .json(
            "/v1/images/generations",
            json!({"model":"test-model","prompt":"x","vendor":2}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["vendor"], true);
    let multipart = "--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ntest-model\r\n--boundary\r\nContent-Disposition: form-data; name=\"image\"; filename=\"x.png\"\r\nContent-Type: image/png\r\n\r\nFAKE_IMAGE\r\n--boundary--\r\n";
    let r = f
        .request(
            "POST",
            "/v1/images/edits",
            multipart,
            "multipart/form-data; boundary=boundary",
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let _ = to_bytes(r.into_body(), 4096).await.unwrap();
    assert_eq!(
        f.upstream.calls.lock().unwrap().last().unwrap().2,
        multipart
    );
    let stats = f
        .state
        .operations
        .statistics(StatsQuery::default())
        .unwrap();
    assert_eq!(stats["summary"]["total"], 5); // activation discovery + four inference requests
}
#[tokio::test]
async fn credential_update_is_private_revisioned_and_invalidates_previous_response() {
    let f = Fixture::new("http-sse").await;
    let (_, v) = f
        .json("/v1/responses", json!({"model":"test-model","input":"x"}))
        .await;
    let id = v["id"].as_str().unwrap();
    let update = json!({"revision":f.revision(),"baseUrl":f.base,"transport":"http-sse","apiKey":"rotated-secret"});
    let (s, v) = f
        .admin(
            &format!("/api/accounts/{}/upstream", f.account),
            update.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.get("apiKey").is_none());
    let (s, _) = f
        .admin(&format!("/api/accounts/{}/upstream", f.account), update)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"next","previous_response_id":id}),
        )
        .await;
    assert!(!s.is_success());
    let (s, _) = f
        .json("/v1/responses", json!({"model":"test-model","input":"new"}))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        f.upstream.calls.lock().unwrap().last().unwrap().1,
        "Bearer rotated-secret"
    );
}
#[tokio::test]
async fn websocket_preference_reuses_connection_and_only_falls_back_on_unsupported_handshake() {
    let f = Fixture::new("ws-first").await;
    let (s, v) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"x","store":false}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["vendor"], 7);
    let (s, v) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"next","previous_response_id":v["id"]}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["vendor"], 7);
    assert_eq!(f.upstream.sockets.load(Ordering::SeqCst), 1);
    f.upstream.ws_status.store(404, Ordering::SeqCst);
    let (s, v) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"fallback"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["upstream_extension"]["kept"], true);
    f.upstream.ws_status.store(401, Ordering::SeqCst);
    let count = f.upstream.calls.lock().unwrap().len();
    let r = f
        .request(
            "POST",
            "/v1/responses",
            json!({"model":"test-model","input":"auth"}).to_string(),
            "application/json",
        )
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let _ = to_bytes(r.into_body(), 4096).await.unwrap();
    assert_eq!(f.upstream.calls.lock().unwrap().len(), count);
}

#[tokio::test]
async fn response_ownership_retrieval_deletion_and_reload() {
    let f = Fixture::new("http-sse").await;
    let (_, v) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"first","store":true}),
        )
        .await;
    let id = v["id"].as_str().unwrap();
    let key = f
        .state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"other caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model":"test-model","input":"foreign","previous_response_id":id})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let r = f
        .request(
            "GET",
            &format!("/v1/responses/{id}"),
            Body::empty(),
            "application/json",
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let _ = to_bytes(r.into_body(), 4096).await.unwrap();
    assert!(
        f.upstream
            .calls
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .0
            .ends_with(id)
    );
    let r = f
        .request(
            "DELETE",
            &format!("/v1/responses/{id}"),
            Body::empty(),
            "application/json",
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let _ = to_bytes(r.into_body(), 4096).await.unwrap();
    let r = f
        .request(
            "GET",
            &format!("/v1/responses/{id}"),
            Body::empty(),
            "application/json",
        )
        .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let profiles = codex_relay::profiles::Profiles::open(f._root.path().join("accounts"))
        .await
        .unwrap();
    let view = serde_json::to_value(profiles.list().await).unwrap();
    assert_eq!(view[0]["source"], "api-key");
    assert_eq!(view[0]["upstream"]["baseUrl"], f.base);
    assert!(view[0]["pid"].is_null());
}

#[tokio::test]
async fn response_retrieval_preserves_queries_and_safe_headers_without_changing_ownership() {
    let f = Fixture::new("http-sse").await;
    let (status, response) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"first","store":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = response["id"].as_str().unwrap();
    let query = "stream=true&starting_after=7&include%5B%5D=reasoning.encrypted_content&vendor=a%2Fb+z&vendor=c";
    let foreign_key = f
        .state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"other retrieval caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let foreign = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/responses/{id}?{query}"))
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {foreign_key}"))
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert!(f.upstream.retrievals.lock().unwrap().is_empty());

    for (index, (method, query, accept)) in [
        ("GET", query, "text/event-stream"),
        ("DELETE", "vendor=delete%2Foption", "application/json"),
    ]
    .into_iter()
    .enumerate()
    {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/v1/responses/{id}?{query}"))
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", f.key))
                    .header("accept", accept)
                    .header("x-vendor-retrieval", "preserved")
                    .header("cookie", "local-secret")
                    .header("x-api-key", "client-secret")
                    .header("connection", "x-private-hop")
                    .header("x-private-hop", "local-only")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if method == "GET" {
            assert_eq!(response.headers()["content-type"], "text/event-stream");
        }
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        if method == "GET" {
            assert!(String::from_utf8_lossy(&body).contains("response.completed"));
        }
        let captured = f.upstream.retrievals.lock().unwrap();
        assert_eq!(captured.len(), index + 1);
        let last = captured.last().unwrap();
        assert_eq!(last.method.as_str(), method);
        assert_eq!(last.target, format!("/prefix/v1/responses/{id}?{query}"));
        assert_eq!(last.headers["accept"], accept);
        assert_eq!(last.headers["x-vendor-retrieval"], "preserved");
        assert_eq!(last.headers["authorization"], "Bearer upstream-test-secret");
        for name in ["cookie", "x-api-key", "connection", "x-private-hop"] {
            assert!(!last.headers.contains_key(name), "{name}");
        }
    }
    let deleted = f
        .request(
            "GET",
            &format!("/v1/responses/{id}?{query}"),
            Body::empty(),
            "application/json",
        )
        .await;
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
    assert_eq!(f.upstream.retrievals.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn revocation_during_handshake_prevents_websocket_submission_and_http_fallback() {
    use codex_relay::operations::ApiKeyUpdate;
    for status in [0, 404] {
        let f = Fixture::new("ws-first").await;
        let gate = Arc::new(HandshakeGate::default());
        *f.upstream.ws_handshake.lock().unwrap() = Some(gate.clone());
        f.upstream.ws_status.store(status, Ordering::SeqCst);
        let access = f.state.operations.authenticate_api_key(&f.key).unwrap();
        let pending = f.request(
            "POST",
            "/v1/responses",
            json!({"model":"test-model","input":"x"}).to_string(),
            "application/json",
        );
        tokio::pin!(pending);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::select! {
                _ = gate.entered.notified() => {},
                _ = &mut pending => panic!("request must wait for the handshake"),
            }
        })
        .await
        .unwrap();
        f.state
            .operations
            .update_api_key(
                access.key_id,
                ApiKeyUpdate {
                    revision: 1,
                    state: Some("revoked".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        gate.resume.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), pending)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "handshake={status}"
        );
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INVALID_API_KEY");
        assert_eq!(f.state.operations.snapshot()["active"], 0);
        assert!(
            !f.upstream
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _, _)| path == "ws" || path.ends_with("/responses")),
            "revoked requests must not reach either model transport"
        );
    }
}

#[tokio::test]
async fn disconnect_after_sse_terminal_preserves_model_result_and_usage() {
    use futures_util::StreamExt;
    for (status, outcome, error) in [
        ("completed", "completed", None),
        ("failed", "failed", Some("UPSTREAM_REQUEST_FAILED")),
        ("incomplete", "failed", Some("UPSTREAM_INCOMPLETE")),
        ("in_progress", "cancelled", None),
    ] {
        let f = Fixture::new("http-sse").await;
        f.upstream.sse_keep_open.store(true, Ordering::SeqCst);
        *f.upstream.sse_status.lock().unwrap() = Some(status.into());
        let response = f
            .request(
                "POST",
                "/v1/responses",
                json!({"model":"test-model","input":"x","stream":true}).to_string(),
                "application/json",
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut wire = Vec::new();
            loop {
                wire.extend_from_slice(&body.next().await.unwrap().unwrap());
                if wire.ends_with(b"\n\n")
                    && String::from_utf8_lossy(&wire).contains(&format!("response.{status}"))
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        drop(body);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let records = f.state.operations.statistics(Default::default()).unwrap();
                let record = &records["records"][0];
                if record["deliveryStatus"] == "disconnected" {
                    assert_eq!(
                        record["modelOutcome"], outcome,
                        "terminal={status}: {record}"
                    );
                    if let Some(error) = error {
                        assert_eq!(record["errorCode"], error);
                    }
                    if status == "in_progress" {
                        assert!(record["inputTokens"].is_null());
                        assert!(record["outputTokens"].is_null());
                    } else {
                        assert_eq!(record["inputTokens"], 4);
                        assert_eq!(record["outputTokens"], 2);
                    }
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.state.operations.snapshot()["active"], 0);
        assert_eq!(
            f.upstream
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, _)| path.ends_with("/responses"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn websocket_disconnect_is_never_replayed_and_releases_capacity() {
    let f = Fixture::new("ws-first").await;
    let r = f
        .request(
            "POST",
            "/v1/responses",
            json!({"model":"disconnect","input":[
                {"type":"compaction","encrypted_content":"orphaned-history"},
                {"role":"user","content":"one attempt"}
            ],"stream":true})
            .to_string(),
            "application/json",
        )
        .await;
    assert!(to_bytes(r.into_body(), 4096).await.is_err());
    let records = f.state.operations.statistics(Default::default()).unwrap();
    assert_eq!(records["records"][0]["clientTransport"], "sse");
    assert_eq!(records["records"][0]["upstreamTransport"], "ws");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while f.state.operations.snapshot()["active"] != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.state.responses.snapshot()["count"], 0);
    let calls = f.upstream.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(path, _, _)| path == "ws").count(), 1);
    assert!(
        !calls
            .iter()
            .any(|(path, _, _)| path.ends_with("/responses"))
    );
}

#[tokio::test]
async fn downstream_websocket_bridges_both_upstream_transports() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        *f.upstream.response_content_type.lock().unwrap() =
            Some("Text/Event-Stream; charset=utf-8".into());
        *f.upstream.sse_layout.lock().unwrap() = Some(("\r".into(), "\r".into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/responses", listener.local_addr().unwrap());
        let app = f.app.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("host", "127.0.0.1:4317".parse().unwrap());
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", f.key).parse().unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let policy = json!([{"type":"compaction","compact_threshold":200000}]);
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"response.create","stream_id":"main","model":"test-model","input":"hello","vendor":true,"context_management":policy}).to_string().into())).await.unwrap();
        let mut compact_item = None;
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(Ok(message)) = socket.next().await {
                if let Ok(text) = message.to_text() {
                    let value: Value = serde_json::from_str(text).unwrap();
                    assert_ne!(value["type"], "error", "{value}");
                    if value["type"] == "response.output_item.done" {
                        compact_item = Some(value["item"].clone());
                    }
                    if value["type"] == "response.completed" {
                        return value;
                    }
                }
            }
            panic!("socket ended");
        })
        .await
        .unwrap();
        assert_eq!(response["stream_id"], "main");
        let records = f.state.operations.statistics(Default::default()).unwrap();
        let row = &records["records"][0];
        assert_eq!(row["endpoint"], "/v1/responses");
        assert_eq!(row["clientTransport"], "ws");
        assert_eq!(
            row["upstreamTransport"],
            if transport == "ws-first" { "ws" } else { "sse" }
        );
        let compact_item = compact_item.unwrap();
        assert_eq!(compact_item["type"], "compaction");
        assert_eq!(compact_item["vendor_compaction"]["kept"], true);
        assert_eq!(response["response"]["output"][0], compact_item);
        let captured: Value =
            serde_json::from_slice(&f.upstream.calls.lock().unwrap().last().unwrap().2).unwrap();
        assert_eq!(captured["context_management"], policy);
        socket.close(None).await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn downstream_websocket_preserves_json_response_outcomes_and_rejects_invalid_bodies() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = Fixture::new("http-sse").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/responses", listener.local_addr().unwrap());
    let app = f.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("host", "127.0.0.1:4317".parse().unwrap());
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.key).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let cases = [
        (json!({"id":"json_failed","status":"failed","output":[],"error":{"code":"vendor_failure","message":"failed"},"usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}}).to_string(), "response.failed", Some("UPSTREAM_REQUEST_FAILED")),
        (json!({"id":"json_incomplete","status":"incomplete","output":[],"incomplete_details":{"reason":"max_output_tokens"}}).to_string(), "response.incomplete", Some("UPSTREAM_INCOMPLETE")),
        (json!({"error":{"code":"vendor_error","message":"rejected"}}).to_string(), "error", Some("UPSTREAM_REQUEST_FAILED")),
        ("not json".into(), "error", Some("UPSTREAM_INVALID_RESPONSE")),
        ("[]".into(), "error", Some("UPSTREAM_INVALID_RESPONSE")),
        (json!({"id":"missing_status","output":[]}).to_string(), "error", Some("UPSTREAM_INVALID_RESPONSE")),
        (json!({"id":"json_cancelled","status":"cancelled","output":[]}).to_string(), "error", Some("REQUEST_CANCELLED")),
        (json!({"id":"json_completed","status":"completed","output":[]}).to_string(), "response.completed", None),
        (json!({"id":"json_queued","status":"queued","output":[]}).to_string(), "response.queued", None),
        (json!({"id":"json_progress","status":"in_progress","output":[]}).to_string(), "response.in_progress", None),
    ];
    for (index, (body, expected_type, error_code)) in cases.iter().enumerate() {
        *f.upstream.response_override.lock().unwrap() = Some(body.clone());
        let lane = format!("case-{index}");
        let model = format!("json-case-{index}");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"response.create","stream_id":lane,"model":model,"input":"fixture","background":true}).to_string().into()
        )).await.unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(Ok(message)) = socket.next().await {
                if let Ok(text) = message.to_text() {
                    return serde_json::from_str::<Value>(text).unwrap();
                }
            }
            panic!("socket ended");
        })
        .await
        .unwrap();
        assert_eq!(event["stream_id"], lane, "{event}");
        assert_eq!(event["type"], *expected_type, "{body}: {event}");
        if expected_type.starts_with("response.") {
            assert_eq!(
                event["response"],
                serde_json::from_str::<Value>(body).unwrap()
            );
        } else if *error_code == Some("UPSTREAM_REQUEST_FAILED") {
            assert_eq!(event["error"]["code"], "vendor_error");
        } else {
            assert_eq!(event["error"]["code"].as_str(), *error_code);
        }
        let stats = f.state.operations.statistics(Default::default()).unwrap();
        let row = stats["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["model"] == model)
            .unwrap();
        assert_eq!(
            row["status"],
            if *error_code == Some("REQUEST_CANCELLED") {
                "cancelled"
            } else if error_code.is_some() {
                "failed"
            } else {
                "succeeded"
            },
            "{row}"
        );
        assert_eq!(row["errorCode"].as_str(), *error_code, "{row}");
    }
    assert_eq!(
        f.upstream
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _, _)| path.ends_with("/responses"))
            .count(),
        cases.len()
    );
    assert_eq!(f.state.operations.snapshot()["active"], 0);
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn direct_json_media_types_and_encodings_match_native_ingress_validation() {
    use std::io::Write;
    let f = Fixture::new("http-sse").await;
    let value = json!({"model":"test-model","input":"fixture"});
    let original = value.to_string();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(original.as_bytes()).unwrap();
    let compressed = gzip.finish().unwrap();
    for content_type in [
        "Application/JSON",
        "Application/Vnd.Vendor+JSON; charset=UTF-8",
    ] {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", f.key))
                    .header("content-type", content_type)
                    .header("content-encoding", "GZIP")
                    .body(Body::from(compressed.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{content_type}");
        to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&f.upstream.calls.lock().unwrap().last().unwrap().2)
                .unwrap(),
            value
        );
    }
    let calls = f.upstream.calls.lock().unwrap().len();
    for content_type in ["text/vendor+json", "application/jsonp"] {
        let response = f
            .request("POST", "/v1/responses", original.clone(), content_type)
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{content_type}"
        );
    }
    for encodings in [
        vec!["identity", "identity"],
        vec!["gzip", "identity"],
        vec!["gzip, identity"],
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1:4317")
            .header("authorization", format!("Bearer {}", f.key))
            .header("content-type", "application/json");
        for encoding in encodings {
            request = request.header("content-encoding", encoding);
        }
        let response = f
            .app
            .clone()
            .oneshot(request.body(Body::from(original.clone())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {}", f.key))
                .header("content-type", "application/json")
                .header(
                    "content-encoding",
                    axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
                )
                .body(Body::from(original))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        f.upstream.calls.lock().unwrap().len(),
        calls,
        "invalid encodings must be rejected before dispatch"
    );
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn malformed_successful_http_response_is_forwarded_but_recorded_as_failed() {
    let f = Fixture::new("http-sse").await;
    let original = "{\"id\":\"truncated";
    *f.upstream.response_override.lock().unwrap() = Some(original.into());
    let response = f
        .request(
            "POST",
            "/v1/responses",
            json!({"model":"test-model","input":"fixture"}).to_string(),
            "application/json",
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        original
    );
    let stats = f.state.operations.statistics(Default::default()).unwrap();
    let row = stats["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(row["status"], "failed");
    assert_eq!(row["errorCode"], "UPSTREAM_INVALID_RESPONSE");
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn base_url_validation_rejects_loops_and_credentials_without_creating_accounts() {
    let f = Fixture::new("http-sse").await;
    for base in [
        "http://127.0.0.1:4317/v1",
        "https://user:password@example.test/v1",
        "https://example.test/v1?api_key=x",
        "file:///tmp/key",
    ] {
        let(s,_)=f.admin("/api/accounts",json!({"name":"invalid","source":"api-key","upstream":{"baseUrl":base,"apiKey":"secret"}})).await;
        assert!(!s.is_success());
    }
    assert_eq!(f.state.profiles.list().await.len(), 1);
    let (s, _) = f
        .admin(
            &format!("/api/accounts/{}/upstream", f.account),
            json!({"revision":f.revision(),"baseUrl":f.base,"apiKey":""}),
        )
        .await;
    assert!(!s.is_success());
    let (s, _) = f
        .admin(&format!("/api/accounts/{}/start", f.account), json!({}))
        .await;
    assert!(!s.is_success());
}

#[tokio::test]
async fn model_discovery_is_optional_and_readiness_uses_routes() {
    let f = Fixture::new("http-sse").await;
    let (s, _) = f
        .admin(
            &format!("/api/accounts/{}/activation", f.account),
            json!({"revision":f.revision(),"enabled":false}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let(s,v)=f.admin("/api/accounts",json!({"name":"no model list","source":"api-key","upstream":{"baseUrl":format!("{}/no-models",f.base),"apiKey":"upstream-test-secret"}})).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = f
        .admin(
            &format!("/api/accounts/{}/activation", v["id"].as_str().unwrap()),
            json!({"revision":f.revision(),"enabled":true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(f.state.operations.has_inference_routes());
    assert!(f.state.operations.inference_models(None).is_empty());
    let (s, v) = f
        .json(
            "/v1/responses",
            json!({"model":"custom-unlisted-model","input":"hello"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["id"].is_string());
    assert!(
        f.upstream
            .calls
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .0
            .contains("/no-models/responses")
    );
}

#[tokio::test]
async fn mixed_group_admits_once_and_native_sessions_exclude_direct_accounts() {
    let f = Fixture::new("http-sse").await;
    let native = f.state.profiles.create("native fixture").await.unwrap();
    f.state
        .operations
        .register_accounts(&[(native.id, native.name)])
        .unwrap();
    let provider=serde_json::from_value(json!({"id":"native","model":"test-model","baseUrl":f.base,"auth":"native-api-key","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
    f.state
        .operations
        .set_inference_route(native.id, f.revision(), Some(provider))
        .unwrap();
    assert_eq!(
        f.state
            .operations
            .native_inference_route(DEFAULT_GROUP, "test-model")
            .unwrap()
            .0,
        native.id
    );
    let occupied = f
        .state
        .operations
        .begin(
            Some(native.id),
            None,
            "diagnostic",
            "test reservation",
            true,
        )
        .unwrap();
    let (s, v) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"hello","vendor_parameter":true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(f.state.operations.snapshot()["active"], 1);
    let id = f.account.parse().unwrap();
    let busy = f
        .state
        .operations
        .begin(Some(id), None, "diagnostic", "test reservation", true)
        .unwrap();
    let (s, _) = f
        .admin(
            &format!("/api/accounts/{}/upstream", f.account),
            json!({"revision":f.revision(),"baseUrl":f.base,"apiKey":"new-secret"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    drop(busy);
    drop(occupied);
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn history_effort_is_recorded_without_rewriting_direct_upstream_payload() {
    let f = Fixture::new("http-sse").await;
    for (context, expected) in [(json!("all_turns"), "high"), (Value::Null, "low")] {
        let body = json!({"model":"test-model","reasoning":{"effort":"low","context":context},"input":[
            {"role":"user","content":"synthetic prompt"},
            {"type":"configuration_update","reasoning":{"effort":"medium"}},
            {"type":"configuration_update","reasoning":{"effort":"high"}}
        ]});
        let (status, response) = f.json("/v1/responses", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let data = f.state.operations.statistics(Default::default()).unwrap();
        assert_eq!(data["records"][0]["reasoningEffort"], expected);
        let calls = f.upstream.calls.lock().unwrap();
        let wire = &calls
            .iter()
            .rev()
            .find(|call| call.0.ends_with("/responses"))
            .unwrap()
            .2;
        let forwarded: Value = serde_json::from_slice(wire).unwrap();
        assert_eq!(forwarded["reasoning"], body["reasoning"]);
        assert_eq!(forwarded["input"], body["input"]);
    }
}

#[tokio::test]
async fn complete_text_history_can_use_another_account_when_its_original_is_disabled() {
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        let (status, first) = f
            .json(
                "/v1/responses",
                json!({"model":"test-model","input":"plain-history-fixture"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, second) = f.admin("/api/accounts", json!({"name":"available","source":"api-key","upstream":{"baseUrl":f.base,"apiKey":"upstream-test-secret","transport":transport}})).await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        let (status, value) = f
            .admin(
                &format!(
                    "/api/accounts/{}/activation",
                    second["id"].as_str().unwrap()
                ),
                json!({"revision":f.revision(),"enabled":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let (status, value) = f
            .admin(
                &format!("/api/accounts/{}/activation", f.account),
                json!({"revision":f.revision(),"enabled":false}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let mut history = vec![json!({"role":"user","content":"plain-history-fixture"})];
        history.extend(first["output"].as_array().unwrap().clone());
        history.push(json!({"role":"user","content":"continue"}));
        let (status, result) = f
            .json(
                "/v1/responses",
                json!({"model":"test-model","input":history}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(f.state.operations.snapshot()["active"], 0);
    }
}

#[tokio::test]
async fn saving_unchanged_upstream_preserves_bindings_connections_and_revision_even_while_busy() {
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        let (status, compacted) = f
            .json(
                "/v1/responses/compact",
                json!({"model":"test-model","input":[]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, first) = f
            .json(
                "/v1/responses",
                json!({"model":"test-model","input":compacted["output"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let revision = f.revision();
        let route = f.state.operations.snapshot()["accounts"][0]["route"].clone();
        let busy = f
            .state
            .operations
            .begin(
                Some(f.account.parse().unwrap()),
                None,
                "diagnostic",
                "fixture",
                true,
            )
            .unwrap();
        let (status, value) = f.admin(&format!("/api/accounts/{}/upstream",f.account),json!({"revision":revision,"baseUrl":format!(" {}/ ",f.base),"transport":transport})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(f.revision(), revision);
        assert_eq!(f.state.operations.snapshot()["accounts"][0]["route"], route);
        drop(busy);
        let (status, continued) = f
            .json(
                "/v1/responses",
                json!({"model":"test-model","previous_response_id":first["id"],"input":"next"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{continued}");
        if transport == "ws-first" {
            assert_eq!(f.upstream.sockets.load(Ordering::SeqCst), 1);
        }
        let (status, continued) = f
            .json(
                "/v1/responses",
                json!({"model":"test-model","input":compacted["output"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{continued}");
    }
}

#[tokio::test]
async fn api_key_backup_restores_configuration_and_secret_without_manual_key_entry() {
    let mut f = Fixture::new("ws-first").await;
    let (status, backup) = f
        .admin("/api/accounts/export", json!({"accountIds":[f.account]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{backup}");
    let item = &backup["items"][0];
    assert_eq!(
        item["upstream"],
        json!({"baseUrl":f.base,"apiKey":"upstream-test-secret","transport":"ws-first"})
    );
    assert!(item["document"].is_null());
    assert_eq!(item["requiresReconnect"], false);
    let (status, deleted) = f
        .admin(
            "/api/accounts/batch",
            json!({"revision":f.revision(),"action":"delete","accountIds":[f.account]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let calls = f.upstream.calls.lock().unwrap().len();
    let (status, restored) = f.admin("/api/accounts/batch",json!({"revision":f.revision(),"action":"import","items":[{"note":"","autoName":false,"name":item["name"],"source":"api-key","groupIds":item["groupIds"],"maxConcurrency":item["maxConcurrency"],"upstream":item["upstream"]}]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["status"], "completed");
    assert!(!restored.to_string().contains("upstream-test-secret"));
    assert_eq!(f.upstream.calls.lock().unwrap().len(), calls);
    f.account = restored["items"][0]["accountId"].as_str().unwrap().into();
    let view = f.state.profiles.list().await;
    let account = serde_json::to_value(
        view.iter()
            .find(|a| a.profile.id.to_string() == f.account)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(account["upstream"]["hasApiKey"], true);
    assert_eq!(account["upstream"]["baseUrl"], f.base);
    assert_eq!(account["upstream"]["transport"], "ws-first");
    assert!(!account.to_string().contains("upstream-test-secret"));
    assert_eq!(
        f.state.operations.snapshot()["accounts"][0]["enabled"],
        false
    );
    f.activate().await;
    let (status, response) = f
        .json(
            "/v1/responses",
            json!({"model":"test-model","input":"restored","background":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        f.upstream.calls.lock().unwrap().last().unwrap().1,
        "Bearer upstream-test-secret"
    );
}

#[tokio::test]
async fn encrypted_history_is_routed_after_account_recreation_and_bound_on_success() {
    for transport in ["http-sse", "ws-first"] {
        for stream in [false, true] {
            let mut f = Fixture::new(transport).await;
            let (status, compacted) = f
                .json(
                    "/v1/responses/compact",
                    json!({"model":"test-model","input":[]}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{compacted}");
            let old_account = f.account.clone();
            let (status, deleted) = f
                .admin(
                    "/api/accounts/batch",
                    json!({
                        "revision":f.revision(),"action":"delete","accountIds":[old_account]
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{deleted}");
            assert_eq!(deleted["status"], "completed");
            assert_eq!(f.state.responses.snapshot()["count"], 0);
            let (status, restored) = f
                .admin(
                    "/api/accounts",
                    json!({
                        "name":"restored","source":"api-key","upstream":{
                            "baseUrl":f.base,"apiKey":"upstream-test-secret","transport":transport
                        }
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{restored}");
            f.account = restored["id"].as_str().unwrap().into();
            assert_ne!(f.account, old_account);
            f.activate().await;
            let mut history = compacted["output"].as_array().unwrap().clone();
            history.push(json!({"role":"user","content":"continue after restore"}));
            let input = json!({"model":"test-model","input":history,"stream":stream});
            let calls_before = f.upstream.calls.lock().unwrap().len();
            let result = f
                .request(
                    "POST",
                    "/v1/responses",
                    input.to_string(),
                    "application/json",
                )
                .await;
            assert_eq!(result.status(), StatusCode::OK);
            let bytes = to_bytes(result.into_body(), 1024 * 1024).await.unwrap();
            if stream {
                assert!(
                    String::from_utf8(bytes.to_vec())
                        .unwrap()
                        .contains("response.completed")
                );
            } else {
                let response: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(response["status"], "completed");
            }
            {
                let calls = f.upstream.calls.lock().unwrap();
                assert_eq!(calls.len(), calls_before + 1);
                let forwarded: Value = serde_json::from_slice(&calls.last().unwrap().2).unwrap();
                assert_eq!(forwarded["input"], input["input"]);
            }
            assert_eq!(f.state.operations.snapshot()["active"], 0);
            // The empty output supplies no new route hint: the accepted opaque
            // input itself must now be bound, including for SSE/WS responses.
            assert!(f.state.responses.snapshot()["count"].as_u64().unwrap() > 0);
            let (status, changed) = f
                .admin(
                    &format!("/api/accounts/{}/upstream", f.account),
                    json!({
                        "revision":f.revision(),"baseUrl":f.base,"apiKey":"changed-secret"
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{changed}");
            let calls_before = f.upstream.calls.lock().unwrap().len();
            let (status, error) = f.json("/v1/responses", input).await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], "RESPONSE_CHAIN_MISMATCH");
            assert_eq!(f.upstream.calls.lock().unwrap().len(), calls_before);
        }
    }
}

#[tokio::test]
async fn rejected_unbound_history_is_not_bound_or_replayed() {
    let f = Fixture::new("http-sse").await;
    let (status, error) = f.json("/v1/responses", json!({
        "model":"error-429","input":[{"type":"compaction","encrypted_content":"orphaned-history"}]
    })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{error}");
    assert_eq!(error["error"]["code"], "upstream_exact");
    let calls = f.upstream.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.0.ends_with("/responses"))
            .count(),
        1,
        "upstream paths: {:?}",
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(f.state.responses.snapshot()["count"], 0);
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn encrypted_history_retains_account_binding_until_configuration_changes() {
    let f = Fixture::new("http-sse").await;
    let (s, compacted) = f
        .json(
            "/v1/responses/compact",
            json!({"model":"test-model","input":[]}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let continuation = json!({"model":"test-model","input":compacted["output"]});
    let (s, _) = f.json("/v1/responses", continuation.clone()).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = f
        .admin(
            &format!("/api/accounts/{}/upstream", f.account),
            json!({"revision":f.revision(),"baseUrl":f.base,"apiKey":"changed-secret"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let calls = f.upstream.calls.lock().unwrap().len();
    let (s, _) = f.json("/v1/responses", continuation).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(f.upstream.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn actual_service_tier_is_reported_independently_of_forced_fast_mode() {
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        let id = f
            .state
            .operations
            .authenticate_api_key(&f.key)
            .unwrap()
            .key_id;
        let (status, body) = f
            .admin(
                &format!("/api/api-keys/{id}"),
                json!({"revision":1,"fastMode":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for (reported, expected) in [
            (json!("priority"), json!("priority")),
            (json!("default"), json!("default")),
            (json!("flex"), json!("flex")),
            (json!("fast"), json!("fast")),
            (Value::Null, Value::Null),
            (json!(true), Value::Null),
            (json!(""), Value::Null),
            (json!("x".repeat(65)), Value::Null),
        ] {
            *f.upstream.response_tier.lock().unwrap() = reported.clone();
            for (path, streaming) in [
                ("/v1/responses", false),
                ("/v1/responses", true),
                ("/v1/responses/compact", false),
            ] {
                let response = f.request("POST", path,
                    json!({"model":"test-model","input":"hello","service_tier":"default","stream":streaming}).to_string(),
                    "application/json",
                ).await;
                assert_eq!(response.status(), StatusCode::OK);
                let request_id = response.headers()["x-request-id"]
                    .to_str()
                    .unwrap()
                    .to_owned();
                let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
                let returned: Value = if streaming {
                    String::from_utf8(bytes.to_vec())
                        .unwrap()
                        .lines()
                        .filter_map(|line| line.strip_prefix("data: "))
                        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .find(|event| event["type"] == "response.completed")
                        .unwrap()["response"]
                        .clone()
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                assert_eq!(returned["service_tier"], reported);
                let records = f.state.operations.statistics(Default::default()).unwrap();
                let row = records["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["id"] == request_id)
                    .unwrap();
                assert_eq!(row["serviceTier"], "priority");
                assert_eq!(
                    row["responseServiceTier"], expected,
                    "{transport} {path} streaming={streaming}"
                );
            }
        }
    }
}

#[tokio::test]
async fn fast_key_overrides_tiers_on_http_and_websocket_upstreams() {
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        let id = f
            .state
            .operations
            .authenticate_api_key(&f.key)
            .unwrap()
            .key_id;
        for (index, enabled) in [true, false].into_iter().enumerate() {
            let (status, body) = f
                .admin(
                    &format!("/api/api-keys/{id}"),
                    json!({"revision":index + 1,"fastMode":enabled}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            for path in ["/v1/responses", "/v1/responses/compact"] {
                for tier in [None, Some("auto"), Some("default"), Some("flex")] {
                    let mut request = json!({"model":"test-model","input":"hello","metadata":{"service_tier":"nested"}});
                    if let Some(tier) = tier {
                        request["service_tier"] = json!(tier);
                    }
                    let (status, body) = f.json(path, request).await;
                    assert_eq!(status, StatusCode::OK, "{body}");
                    let captured: Value =
                        serde_json::from_slice(&f.upstream.calls.lock().unwrap().last().unwrap().2)
                            .unwrap();
                    assert_eq!(
                        captured["service_tier"],
                        if enabled {
                            json!("priority")
                        } else {
                            json!(tier)
                        }
                    );
                    assert_eq!(captured["metadata"]["service_tier"], "nested");
                    let records = f.state.operations.statistics(Default::default()).unwrap();
                    assert_eq!(
                        records["records"][0]["serviceTier"],
                        captured["service_tier"]
                    );
                }
            }
        }
    }
}
#[tokio::test]
async fn downstream_websocket_uses_latest_fast_key_setting_per_turn() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = Fixture::new("ws-first").await;
    let id = f
        .state
        .operations
        .authenticate_api_key(&f.key)
        .unwrap()
        .key_id;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/responses", listener.local_addr().unwrap());
    let app = f.app.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("host", "127.0.0.1:4317".parse().unwrap());
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.key).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    for (index, enabled) in [true, false].into_iter().enumerate() {
        let actual_tier = if enabled { "default" } else { "priority" };
        *f.upstream.response_tier.lock().unwrap() = json!(actual_tier);
        let (status, body) = f
            .admin(
                &format!("/api/api-keys/{id}"),
                json!({"revision":index+1,"fastMode":enabled}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"response.create","model":"test-model","input":"hello","service_tier":"default"}).to_string().into())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(Ok(message)) = socket.next().await {
                if let Ok(text) = message.to_text() {
                    let event: Value = serde_json::from_str(text).unwrap();
                    assert_ne!(event["type"], "error", "{event}");
                    if event["type"] == "response.completed" {
                        assert_eq!(event["response"]["service_tier"], actual_tier);
                        return;
                    }
                }
            }
            panic!("socket ended");
        })
        .await
        .unwrap();
        let captured: Value =
            serde_json::from_slice(&f.upstream.calls.lock().unwrap().last().unwrap().2).unwrap();
        assert_eq!(
            captured["service_tier"],
            if enabled { "priority" } else { "default" }
        );
        let records = f.state.operations.statistics(Default::default()).unwrap();
        assert_eq!(
            records["records"][0]["serviceTier"],
            captured["service_tier"]
        );
        assert_eq!(records["records"][0]["responseServiceTier"], actual_tier);
    }
    socket.close(None).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn streaming_first_response_uses_a_timestamp_instead_of_an_elapsed_duration() {
    let f = Fixture::new("http-sse").await;
    let response = f
        .request(
            "POST",
            "/v1/responses",
            json!({
                "model":"test-model","input":"latency-fixture","stream":true
            })
            .to_string(),
            "application/json",
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let data = f.state.operations.statistics(Default::default()).unwrap();
    let record = data["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    let first = record["firstDeltaMs"].as_u64().unwrap();
    assert!(
        first >= 25,
        "upstream delay must not collapse to zero: {record}"
    );
    assert!(first <= record["totalMs"].as_u64().unwrap());
}

#[tokio::test]
async fn event_stream_content_type_variants_preserve_usage_and_response_ownership() {
    let f = Fixture::new("http-sse").await;
    for content_type in [
        "Text/Event-Stream",
        "TEXT/EVENT-STREAM; charset=utf-8",
        "text/event-stream ; charset=utf-8",
    ] {
        *f.upstream.response_content_type.lock().unwrap() = Some(content_type.into());
        let response = f
            .request(
                "POST",
                "/v1/responses",
                json!({"model":"test-model","input":"plain-history-fixture","stream":true})
                    .to_string(),
                "application/json",
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let completed = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .find(|event| event["type"] == "response.completed")
            .unwrap();
        let data = f.state.operations.statistics(Default::default()).unwrap();
        let row = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == request_id)
            .unwrap();
        assert_eq!(row["upstreamTransport"], "sse", "{content_type}");
        assert_eq!(row["inputTokens"], 4, "{content_type}: {row}");
        assert_eq!(row["outputTokens"], 2, "{content_type}: {row}");
        assert!(row["firstDeltaMs"].is_u64(), "{content_type}: {row}");
        let retrieved = f
            .request(
                "GET",
                &format!(
                    "/v1/responses/{}",
                    completed["response"]["id"].as_str().unwrap()
                ),
                Body::empty(),
                "application/json",
            )
            .await;
        assert_eq!(
            retrieved.status(),
            StatusCode::OK,
            "{content_type}: response binding was lost"
        );
        to_bytes(retrieved.into_body(), 1024 * 1024).await.unwrap();
    }
}

#[tokio::test]
async fn event_stream_layouts_preserve_wire_bytes_usage_and_ownership() {
    let f = Fixture::new("http-sse").await;
    for (line, blank) in [("\n", "\n"), ("\r", "\r"), ("\n", "\r\n"), ("\r\n", "\r")] {
        *f.upstream.sse_layout.lock().unwrap() = Some((line.into(), blank.into()));
        let response = f
            .request(
                "POST",
                "/v1/responses",
                json!({"model":"test-model","input":"plain-history-fixture","stream":true})
                    .to_string(),
                "application/json",
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let wire = std::str::from_utf8(&bytes).unwrap();
        assert!(wire.starts_with('\u{feff}'));
        assert!(wire.contains(&format!(": comment{blank}{blank}")));
        let data = f.state.operations.statistics(Default::default()).unwrap();
        let row = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == request_id)
            .unwrap();
        assert_eq!(row["status"], "succeeded", "{row}");
        assert_eq!(row["inputTokens"], 4);
        assert_eq!(row["outputTokens"], 2);
        assert!(row["firstDeltaMs"].is_u64());
        let completed: Value = wire
            .split(['\r', '\n'])
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .find(|event| event["type"] == "response.completed")
            .unwrap();
        let retrieved = f
            .request(
                "GET",
                &format!(
                    "/v1/responses/{}",
                    completed["response"]["id"].as_str().unwrap()
                ),
                Body::empty(),
                "application/json",
            )
            .await;
        assert_eq!(retrieved.status(), StatusCode::OK);
        to_bytes(retrieved.into_body(), 1024 * 1024).await.unwrap();
    }
}

#[tokio::test]
async fn all_direct_ingress_paths_reject_ambiguous_encodings_before_dispatch() {
    let f = Fixture::new("http-sse").await;
    let before = f.upstream.calls.lock().unwrap().len();
    for path in [
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/alpha/search",
        "/v1/images/generations",
        "/v1/images/edits",
    ] {
        for encodings in [
            vec![b"identity".as_slice(), b"gzip"],
            vec![b"\xff".as_slice()],
            vec![b"identity,gzip".as_slice()],
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {}", f.key))
                .header("content-type", "application/json");
            for encoding in encodings {
                request = request.header(
                    "content-encoding",
                    axum::http::HeaderValue::from_bytes(encoding).unwrap(),
                );
            }
            let response = f.app.clone().oneshot(request.body(Body::from(json!({"model":"test-model","id":"s","input":"x","commands":{},"prompt":"x"}).to_string())).unwrap()).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{path}"
            );
            let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["error"]["code"],
                "UNSUPPORTED_MEDIA_TYPE"
            );
        }
    }
    let multipart = "--fixture\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ntest-model\r\n--fixture--\r\n";
    for encoding in ["gzip", "br", "identity,gzip"] {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/images/edits")
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", f.key))
                    .header("content-type", "multipart/form-data; boundary=fixture")
                    .header("content-encoding", encoding)
                    .body(Body::from(multipart))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    assert_eq!(f.upstream.calls.lock().unwrap().len(), before);
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn schema_examples_are_not_capability_requests() {
    let f = Fixture::new("http-sse").await;
    let (status, settings) = f
        .admin(
            &format!("/api/accounts/{}/features", f.account),
            json!({"revision":f.revision(),"disabledFeatures":["webSearch"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    let base = json!({"model":"test-model","input":"return a typed object","text":{"format":{"type":"json_schema","name":"example","schema":{"type":"object","properties":{"type":{"type":"string"}},"default":{"type":"ordinary-data"}}}}});
    let (status, response) = f.json("/v1/responses", base.clone()).await;
    assert_eq!(status, StatusCode::OK, "baseline: {response}");
    let mut body = base;
    body["text"]["format"]["schema"]["default"]["type"] = json!("web_search");
    let (status, response) = f.json("/v1/responses", body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "schema example must remain data: {response}"
    );
    for body in [
        json!({"model":"test-model","input":"search","tools":[{"type":"web_search"}]}),
        json!({"model":"test-model","input":[{"type":"additional_tools","tools":[{"type":"web_search"}]}]}),
    ] {
        let (status, response) = f.json("/v1/responses", body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response["error"]["code"], "CAPABILITY_UNAVAILABLE");
    }
}
#[tokio::test]
async fn mixed_group_native_prewarm_has_no_cancelled_inference() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = Fixture::new("http-sse").await;
    let native = f.state.profiles.create("native fixture").await.unwrap();
    f.state
        .operations
        .register_accounts(&[(native.id, native.name)])
        .unwrap();
    let provider=serde_json::from_value(json!({"id":"native","model":"test-model","baseUrl":f.base,"auth":"native-api-key","forwardHeaders":[],"requiredHeaders":[],"mcpServers":[]})).unwrap();
    f.state
        .operations
        .set_inference_route(native.id, f.revision(), Some(provider))
        .unwrap();
    let (status, value) = f
        .admin(
            &format!("/api/accounts/{}/activation", f.account),
            json!({"revision":f.revision(),"enabled":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let occupied = f
        .state
        .operations
        .begin(
            Some(native.id),
            None,
            "diagnostic",
            "test reservation",
            true,
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/responses", listener.local_addr().unwrap());
    let app = f.app.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("host", "127.0.0.1:4317".parse().unwrap());
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.key).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"response.create","model":"test-model","generate":false,"input":"prewarm only","reasoning":{"effort":"high"}}).to_string().into())).await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Some(Ok(message)) = socket.next().await {
            let event: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            if event["type"] == "response.completed" || event["type"] == "error" {
                return event;
            }
        }
        panic!("closed before completion")
    })
    .await
    .unwrap();
    socket.close(None).await.unwrap();
    task.abort();
    assert_eq!(response["type"], "response.completed", "{response}");
    let data = f.state.operations.statistics(Default::default()).unwrap();
    let rows: Vec<_> = data["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "inference")
        .collect();
    assert!(
        rows.is_empty(),
        "successful prewarm produced inference rows: {rows:?}"
    );
    assert_eq!(f.state.operations.snapshot()["active"], 1);
    assert_eq!(f.state.operations.snapshot()["queued"], 0);
    drop(occupied);
    assert_eq!(f.state.operations.snapshot()["active"], 0);
}

#[tokio::test]
async fn direct_prewarm_requires_websocket_and_preserves_generate_false() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    for transport in ["http-sse", "ws-first"] {
        let f = Fixture::new(transport).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/responses", listener.local_addr().unwrap());
        let app = f.app.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("host", "127.0.0.1:4317".parse().unwrap());
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", f.key).parse().unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"response.create","model":"test-model","generate":false,"input":"prewarm only"}).to_string().into())).await.unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(Ok(message)) = socket.next().await {
                let event: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if event["type"] == "response.completed" || event["type"] == "error" {
                    return event;
                }
            }
            panic!("closed before completion")
        })
        .await
        .unwrap();
        socket.close(None).await.unwrap();
        task.abort();
        let calls = f.upstream.calls.lock().unwrap();
        let generated: Vec<_> = calls
            .iter()
            .filter(|call| call.0.ends_with("responses") || call.0 == "ws")
            .collect();
        if transport == "http-sse" {
            assert_eq!(response["type"], "error", "{response}");
            assert!(generated.is_empty());
        } else {
            assert_eq!(response["type"], "response.completed", "{response}");
            assert_eq!(generated.len(), 1, "{calls:?}");
            let frame: Value = serde_json::from_slice(&generated[0].2).unwrap();
            assert_eq!(frame["generate"], false);
        }
    }
}
