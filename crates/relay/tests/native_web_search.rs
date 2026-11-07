//! Real app-server, synthetic credentials and localhost model/search/images only.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{backend::Backend, installation::Installation, provider::Provider, responses};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC";

#[derive(Default)]
struct Fixture {
    case: Mutex<String>,
    models: Mutex<Vec<Value>>,
    searches: Mutex<Vec<Value>>,
    images: Mutex<Vec<Value>>,
    entered: tokio::sync::Notify,
}
async fn model(
    State(s): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_WEB_SEARCH");
    let case = s.case.lock().unwrap().clone();
    s.models.lock().unwrap().push(body.clone());
    let followup =
        body["input"].as_array().unwrap().iter().any(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "native_search"
        });
    let mut items = if followup {
        vec![
            json!({"type":"message","id":"msg_answer","role":"assistant","status":"completed","content":[{
                "type":"output_text","text":"Fixture source.","annotations":[{"type":"url_citation","url":"https://example.test/source","title":"Fixture","start_index":0,"end_index":14}]
            }]}),
        ]
    } else if case.starts_with("image") {
        vec![
            json!({"type":"function_call","id":"fc_image","call_id":"native_search","namespace":"image_gen","name":"imagegen","arguments":"{\"prompt\":\"fixture\"}"}),
        ]
    } else {
        vec![
            json!({"type":"function_call","id":"fc_web","call_id":"native_search","namespace":"web","name":"run","arguments":"{\"search_query\":[{\"q\":\"fixture\"}]}"}),
        ]
    };
    if case.ends_with("mixed") && !followup {
        items.push(json!({"type":"function_call","id":"fc_client","call_id":"client_echo","name":"echo","arguments":"{}"}));
    }
    let mut events: Vec<_> = items.iter().enumerate().map(|(index, item)| {
        json!({"type":"response.output_item.done","output_index":index,"item":item})
    }).collect();
    events.push(json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":items,"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}));
    (
        [("content-type", "text/event-stream")],
        events
            .iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect::<String>(),
    )
        .into_response()
}
async fn search(
    State(s): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_WEB_SEARCH");
    assert_eq!(body["commands"]["search_query"][0]["q"], "fixture");
    let case = s.case.lock().unwrap().clone();
    s.searches.lock().unwrap().push(body);
    s.entered.notify_one();
    if case == "failed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":"Synthetic search rejection"}})),
        )
            .into_response();
    }
    if case == "mixed" || case == "cancel" {
        // Exceed the client callback grace period to catch premature handoff.
        tokio::time::sleep(Duration::from_secs(6)).await;
    }
    Json(json!({"output":"Verified synthetic search result","encrypted_output":"OPAQUE_FIXTURE","results":[
        {"ref_id":"turn0search0","url":"https://example.test/source","title":"Fixture"}
    ]})).into_response()
}

async fn image(
    State(s): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer SYNTHETIC_WEB_SEARCH");
    assert_eq!(body["prompt"], "fixture");
    let case = s.case.lock().unwrap().clone();
    s.images.lock().unwrap().push(body);
    s.entered.notify_one();
    if case == "image_failed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":"Synthetic image rejection"}})),
        )
            .into_response();
    }
    if case == "image_mixed" || case == "image_cancel" {
        tokio::time::sleep(Duration::from_secs(6)).await;
    }
    Json(json!({"created":1,"data":[{"b64_json":PNG,"generation_id":"fixture-image"}],"usage":{"input_tokens":7,"output_tokens":11,"total_tokens":18}})).into_response()
}

async fn run(
    case: &str,
    backend: &Backend,
    provider: &Provider,
    cwd: &Path,
    request: &responses::Request,
    cancel: CancellationToken,
) -> (codex_relay::streaming::StreamSummary, Vec<Value>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    tokio::time::timeout(Duration::from_secs(60), async {
        let work = responses::run(
            &backend.rpc,
            provider,
            cwd,
            request,
            &request.input,
            tx,
            cancel,
        );
        let collect = async {
            let mut events = vec![];
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        };
        let (result, events) = tokio::join!(work, collect);
        (result.unwrap(), events)
    })
    .await
    .unwrap_or_else(|_| panic!("native tool case {case} timed out"))
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; original app-server with synthetic localhost model and search, no real usage"]
async fn native_search_and_images_complete_without_yielding_client_callbacks() {
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
    let fixture = Arc::new(Fixture::default());
    let app = Router::new()
        .route("/v1/responses", post(model))
        .route("/v1/alpha/search", post(search))
        .route("/v1/images/generations", post(image))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let claims = json!({"email":"fixture@example.test","sub":"fixture","exp":chrono::Utc::now().timestamp()+3600,
        "https://api.openai.com/auth":{"chatgpt_account_id":"fixture","chatgpt_user_id":"fixture","chatgpt_plan_type":"pro"}});
    let token = format!(
        "e30.{}.synthetic",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    std::fs::write(home.join("auth.json"), json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"access_token":token,"id_token":token,"refresh_token":"synthetic","account_id":"fixture"},
        "last_refresh":chrono::Utc::now().to_rfc3339()}).to_string()).unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
model="gpt-6.1-sol"
model_provider="synthetic"
cli_auth_credentials_store="file"
chatgpt_base_url="http://127.0.0.1:{port}"
[features]
remote_models=false
secret_auth_storage=false
enable_request_compression=false
[model_providers.synthetic]
name="OpenAI"
base_url="http://127.0.0.1:{port}/v1"
wire_api="responses"
requires_openai_auth=true
supports_standalone_web_search=true
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_WEB_SEARCH"
[model_providers.synthetic.capabilities]
remote_compaction="v2"
"#
        ),
    )
    .unwrap();
    let backend = Backend::start_response_with_proxy(
        &installation,
        &cwd,
        Some(&home),
        true,
        &Default::default(),
        "gpt-6.1-sol",
    )
    .await
    .unwrap();
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
    for case in [
        "success",
        "mixed",
        "failed",
        "cancel",
        "after_cancel",
        "image",
        "image_mixed",
        "image_failed",
        "image_cancel",
        "image_after_cancel",
    ] {
        *fixture.case.lock().unwrap() = case.into();
        fixture.models.lock().unwrap().clear();
        fixture.searches.lock().unwrap().clear();
        fixture.images.lock().unwrap().clear();
        let image = case.starts_with("image");
        let mut tools = vec![json!({"type":if image {"image_generation"} else {"web_search"}})];
        if case.ends_with("mixed") {
            tools.push(json!({"type":"function","name":"echo"}));
        }
        let request = responses::Request::parse(
            json!({"model":"gpt-6.1-sol","input":"Search fixture","tools":tools}),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        // Consume previous search notifications before waiting for this request.
        while fixture.entered.notified().now_or_never().is_some() {}
        eprintln!("native tool case: {case}");
        let work = run(case, &backend, &provider, &cwd, &request, cancel.clone());
        let (summary, events) = if case.ends_with("cancel") && !case.ends_with("after_cancel") {
            tokio::join!(work, async {
                fixture.entered.notified().await;
                cancel.cancel();
            })
            .0
        } else {
            work.await
        };
        assert_eq!(
            fixture.searches.lock().unwrap().len(),
            usize::from(!image),
            "{case}"
        );
        assert_eq!(
            fixture.images.lock().unwrap().len(),
            usize::from(image),
            "{case}"
        );
        if case == "cancel" || case == "image_cancel" {
            assert_eq!(summary.outcome, "cancelled");
            assert!(summary.cleanup_confirmed);
            assert_eq!(fixture.models.lock().unwrap().len(), 1);
            continue;
        }
        if case.ends_with("failed") {
            assert_eq!(summary.outcome, "failed");
            assert_eq!(summary.error_code.as_deref(), Some("UPSTREAM_TOOL_FAILED"));
            assert!(summary.cleanup_confirmed, "{case}: {summary:?}");
            assert_eq!(events.last().unwrap()["type"], "response.failed");
            continue;
        }
        assert_eq!(summary.outcome, "completed", "{case}: {summary:?}");
        let response = &events.last().unwrap()["response"];
        assert_eq!(response["status"], "completed");
        let output = response["output"].as_array().unwrap();
        if image {
            let image = output
                .iter()
                .find(|item| item["type"] == "image_generation_call")
                .unwrap();
            assert_eq!(image["status"], "completed");
            assert_eq!(image["result"], PNG);
            assert_eq!(image["revised_prompt"], "fixture");
            assert!(!response.to_string().contains("generated_images"));
        } else {
            let search = output
                .iter()
                .find(|item| item["type"] == "web_search_call")
                .unwrap();
            assert_eq!(search["status"], "completed");
            assert_eq!(search["action"]["type"], "search");
            assert_eq!(
                search["action"]["sources"][0]["url"],
                "https://example.test/source"
            );
        }
        assert!(
            output
                .iter()
                .all(|item| item["type"] != "function_call_output")
        );
        assert!(output.iter().all(|item| item["name"] != "run"));
        assert!(output.iter().all(|item| item["name"] != "imagegen"));
        if case.ends_with("mixed") {
            assert_eq!(fixture.models.lock().unwrap().len(), 1);
            assert!(
                output
                    .iter()
                    .any(|item| item["type"] == "function_call" && item["name"] == "echo")
            );
        } else {
            assert_eq!(fixture.models.lock().unwrap().len(), 2);
            assert_eq!(response["usage"]["total_tokens"], 24);
            let message = output
                .iter()
                .find(|item| item["type"] == "message")
                .unwrap();
            assert_eq!(
                message["content"][0]["annotations"][0]["url"],
                "https://example.test/source"
            );
            assert!(
                fixture.models.lock().unwrap()[1]["input"]
                    .to_string()
                    .contains(if image {
                        PNG
                    } else {
                        "Verified synthetic search result"
                    })
            );
        }
    }
    backend.stop().await.unwrap();
    server.abort();
}
