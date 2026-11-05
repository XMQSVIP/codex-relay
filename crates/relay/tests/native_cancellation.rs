//! Actual app-server cancellation against a stalled local model; no real account usage.
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::{Response, StatusCode},
    routing::post,
};
use codex_relay::{
    backend::Backend, installation::Installation, native_session, provider::Provider, responses,
};
use serde_json::json;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Model {
    pid: AtomicU32,
    executable: PathBuf,
    port: u16,
    connected: tokio::sync::Notify,
}
async fn stall(
    State(model): State<Arc<Model>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response<Body> {
    if !codex_relay::platform::verify_peer(
        peer.port(),
        model.port,
        model.pid.load(Ordering::SeqCst),
        &model.executable,
    )
    .await
    {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    model.connected.notify_one();
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::pending::<
            std::result::Result<bytes::Bytes, std::convert::Infallible>,
        >()))
        .unwrap()
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; local stalled model, Responses cancellation and native turn/interrupt"]
async fn native_cancel_and_interrupt_release_processes() {
    let installation: Installation = serde_json::from_slice(
        &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let model = Arc::new(Model {
        pid: AtomicU32::new(0),
        executable: installation.app_server_executable.clone(),
        port,
        connected: tokio::sync::Notify::new(),
    });
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(stall))
                .with_state(model.clone())
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    );
    std::fs::write(home.join("config.toml"), format!("model=\"gpt-6.1-sol\"\nmodel_provider=\"synthetic\"\n[model_providers.synthetic]\nname=\"Synthetic cancellation\"\nbase_url=\"http://127.0.0.1:{port}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\n")).unwrap();
    let provider = Provider {
        execution: Default::default(),
        id: "synthetic".into(),
        model: "gpt-6.1-sol".into(),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        auth: "none".into(),
        required_headers: vec![],
        forward_headers: vec![],
        mcp_servers: vec![],
    };
    for (websocket, compact) in [(false, false), (true, false), (false, true)] {
        let backend = Backend::start_direct(&installation, temp.path(), Some(&home))
            .await
            .unwrap();
        model.pid.store(backend.pid, Ordering::SeqCst);
        let (out, mut events) = tokio::sync::mpsc::channel(32);
        let result = if websocket {
            let (input, mut incoming) = tokio::sync::mpsc::channel(8);
            let start = json!({"id":1,"method":"thread/start","params":{"model":"gpt-6.1-sol"}});
            let work = native_session::run(
                &backend.rpc,
                &provider,
                temp.path(),
                start,
                &mut incoming,
                out,
                CancellationToken::new(),
            );
            let client = async {
                let mut interrupted = false;
                while let Some(event) = events.recv().await {
                    if event["id"] == 1 && event.get("result").is_some() {
                        input.send(json!({"id":2,"method":"turn/start","params":{"input":[{"type":"text","text":"wait for cancellation","text_elements":[]}]}})).await.unwrap();
                    }
                    if event["id"] == 2 && event.get("result").is_some() {
                        model.connected.notified().await;
                        input
                            .send(json!({"id":3,"method":"turn/interrupt"}))
                            .await
                            .unwrap();
                    }
                    if event["method"] == "turn/completed" {
                        assert_eq!(event["params"]["turn"]["status"], "interrupted");
                        interrupted = true;
                        input
                            .send(json!({"id":4,"method":"session/close"}))
                            .await
                            .unwrap();
                    }
                }
                assert!(interrupted);
            };
            tokio::time::timeout(Duration::from_secs(30), async {
                let (result, ()) = tokio::join!(work, client);
                result
            })
            .await
        } else {
            let request = responses::Request::parse(
                json!({"model":"gpt-6.1-sol","input":if compact {json!([{"role":"user","content":"wait for cancellation"},{"type":"compaction_trigger"}])}else{json!("wait for cancellation")},"store":false}),
            )
            .unwrap();
            let cancel = CancellationToken::new();
            let work = responses::run(
                &backend.rpc,
                &provider,
                temp.path(),
                &request,
                &request.input,
                out,
                cancel.clone(),
            );
            let client = async {
                model.connected.notified().await;
                cancel.cancel();
                while events.recv().await.is_some() {}
            };
            tokio::time::timeout(Duration::from_secs(30), async {
                let (result, ()) = tokio::join!(work, client);
                result
            })
            .await
        };
        backend.stop().await.unwrap();
        assert!(!backend.alive().await);
        if compact {
            assert_eq!(result.unwrap().unwrap_err().code, "REQUEST_CANCELLED");
            fn session_files(path: &std::path::Path) -> usize {
                std::fs::read_dir(path)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        if e.path().is_dir() {
                            session_files(&e.path())
                        } else {
                            usize::from(e.path().extension().is_some_and(|s| s == "jsonl"))
                        }
                    })
                    .sum()
            }
            assert_eq!(
                session_files(&home.join("sessions")),
                0,
                "cancelled compaction left a persistent thread"
            );
        } else {
            let summary = result.unwrap().unwrap();
            assert_eq!(summary.outcome, "cancelled");
        }
    }
    server.abort();
}
