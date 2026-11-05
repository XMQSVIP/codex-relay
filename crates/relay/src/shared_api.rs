//! API sessions attached to the owned desktop backend, never desktop threads.
use crate::{
    backend::Backend,
    error::{Error, Result},
    guard::Guard,
    inference::RequestJob,
    installation::Installation,
    operations::{DESKTOP_ACCOUNT, Operations},
    provider,
    rpc::Rpc,
    streaming::{self, StreamSummary},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct SharedApi {
    session: Mutex<Option<Session>>,
    quarantined: AtomicBool,
    pub operations: Arc<Operations>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        operations::{ApiKeyCreate, DEFAULT_GROUP},
        probe::{Fixture, fixture},
    };
    use axum::{Router, routing::post};

    #[tokio::test]
    #[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; isolated native process and loopback synthetic responses only"]
    async fn native_shared_requests_preserve_backend_and_use_requested_model() {
        let path = std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap();
        let installation: Installation =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        crate::installation::validate(&installation).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(home.join("config.toml"), "model=\"gpt-6.1-sol\"\nmodel_provider=\"native_probe\"\n[features]\nplugins=false\napps=false\nhooks=false\n[model_providers.native_probe]\nname=\"Synthetic\"\nbase_url=\"http://192.168.1.10/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_BEARER\"\nhttp_headers={\"X-OpenAI-Actor-Authorization\"=\"SYNTHETIC_ACTOR\"}\n").unwrap();
        let fixture_state = Arc::new(Fixture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/responses", post(fixture))
                    .with_state(fixture_state.clone()),
            )
            .into_future(),
        );
        let backend = Arc::new(
            Backend::start(
                &installation,
                &cwd,
                Some(&home),
                Some(crate::backend::unused_port().await.unwrap()),
                true,
            )
            .await
            .unwrap(),
        );
        let ops = Operations::open(&temp.path().join("ops")).unwrap();
        let api = SharedApi::new(ops.clone());
        *api.session.lock().unwrap() = Some(Session {
            backend: backend.clone(),
            installation,
            cwd,
            cancel: CancellationToken::new(),
            fixture: Some(upstream),
        });
        let result = async {
            let provider = api.refresh(true).await.unwrap().unwrap();
            let key = ops
                .create_api_key(ApiKeyCreate {
                    fast_mode: false,
                    name: "synthetic".into(),
                    group_id: DEFAULT_GROUP,
                    models: vec!["gpt-6-astra".into()],
                    expires_in_days: None,
                    max_concurrency: 1,
                })
                .unwrap();
            let access = ops.authenticate_api_key(&key.secret).unwrap();
            let mut threads = Vec::new();
            for scenario in [
                "text",
                "interrupt",
                "tools",
                "client_tools",
                "client_result",
                "client_interrupt",
                "text",
            ] {
                *fixture_state.scenario.lock().unwrap() = if scenario == "client_interrupt" {
                    "interrupt"
                } else {
                    scenario
                }
                .into();
                let job = RequestJob {
                    account: DESKTOP_ACCOUNT,
                    access: access.clone(),
                    provider: provider.clone(),
                    model: "gpt-6-astra".into(),
                    prompt: "Synthetic fixture only.".into(),
                    client_payload: if scenario.starts_with("client_") {
                        let mut payload = json!({"model":"gpt-6-astra","input":[{"role":"user","content":"Client-only context"}],"stream":true,"store":false,"tools":[{"type":"custom","name":"exec"},{"type":"function","name":"list_mcp_resources","parameters":{"type":"object","properties":{}}}]});
                        if scenario == "client_result" {
                            payload["input"].as_array_mut().unwrap().extend([
                                json!({"type":"custom_tool_call","id":"fc_code","call_id":"code","name":"exec","input":"text('RUST_GUARD_CANARY');"}),
                                json!({"type":"custom_tool_call_output","call_id":"code","output":"client result"}),
                            ]);
                        }
                        crate::client_tools::validate_request(&payload).unwrap();
                        Some(payload)
                    } else {
                        None
                    },
                };
                let lease = ops.acquire_client(DESKTOP_ACCOUNT, &access).await.unwrap();
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(16);
                let cancel = CancellationToken::new();
                let receive = async {
                    let mut seen = Vec::new();
                    while let Some(event) = rx.recv().await {
                        if ["interrupt", "client_interrupt"].contains(&scenario)
                            && event["type"] == "response.output_text.delta"
                        {
                            cancel.cancel();
                        }
                        seen.push(event);
                    }
                    seen
                };
                let (summary, events) = tokio::join!(api.stream(&job, tx, cancel.clone()), receive);
                lease.finish_inference(&summary).unwrap();
                let summary = summary.unwrap();
                assert_eq!(
                    summary.outcome,
                    match scenario {
                        "text" | "client_tools" | "client_result" => "completed",
                        "interrupt" | "client_interrupt" => "cancelled",
                        _ => "failed",
                    },
                    "{summary:?}"
                );
                assert!(summary.cleanup_confirmed, "{summary:?}");
                assert!(!threads.contains(&summary.thread_id));
                threads.push(summary.thread_id);
                if scenario == "text" {
                    assert_eq!(events.last().unwrap()["response"]["model"], "gpt-6-astra");
                }
                if scenario == "client_tools" {
                    let output = &events.last().unwrap()["response"]["output"];
                    assert_eq!(output[0]["type"], "custom_tool_call");
                    assert_eq!(output[0]["call_id"], "code");
                    assert_eq!(output[1]["type"], "function_call");
                    assert!(!events.iter().any(|e| e["delta"] == "Done."));
                    assert_eq!(summary.guard.upstream_requests, 1);
                }
                if ["client_tools", "client_result"].contains(&scenario) {
                    assert_eq!(summary.native_status.as_deref(), Some("interrupted"));
                    assert!(summary.interrupt_acknowledged);
                    assert!(summary.item_ids.iter().all(|id| id != "msg_bridge_receipt"));
                }
                if scenario == "client_result" {
                    assert!(events.iter().any(|e| e["delta"] == "Rust 原生验证🌍"));
                    assert_eq!(summary.usage.as_ref().unwrap().input_tokens, 10);
                    assert_eq!(summary.usage.as_ref().unwrap().output_tokens, 4);
                }
                assert!(backend.alive().await);
                assert!(
                    backend
                        .rpc
                        .request("thread/loaded/list", json!({"limit":100}))
                        .await
                        .is_ok()
                );
                assert_eq!(ops.snapshot()["active"], 0);
            }
            assert!(
                fixture_state
                    .seen
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|v| v["authenticated"] == true
                        && (v["toolsStripped"] == true || v["hasTools"] == true)
                        && v["model"] == "gpt-6-astra")
            );
            assert!(!fixture_state.exec_seen.load(Ordering::SeqCst));
            assert!(!fixture_state.resource_seen.load(Ordering::SeqCst));
        };
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(result).catch_unwind().await;
        api.detach();
        backend.stop().await.unwrap();
        server.abort();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
#[derive(Clone)]
struct Session {
    backend: Arc<Backend>,
    installation: Installation,
    cwd: PathBuf,
    cancel: CancellationToken,
    #[cfg(test)]
    fixture: Option<String>,
}
impl SharedApi {
    pub fn new(operations: Arc<Operations>) -> Self {
        Self {
            session: Mutex::new(None),
            quarantined: AtomicBool::new(false),
            operations,
        }
    }
    pub async fn attach(
        &self,
        backend: Arc<Backend>,
        installation: Installation,
        cancel: CancellationToken,
    ) -> Result<()> {
        let cwd = std::env::current_dir()?.join(".cache/shared-api-workspace");
        tokio::fs::create_dir_all(&cwd).await?;
        *self.session.lock().unwrap() = Some(Session {
            backend,
            installation,
            cwd,
            cancel,
            #[cfg(test)]
            fixture: None,
        });
        self.quarantined.store(false, Ordering::SeqCst);
        Ok(())
    }
    pub fn detach(&self) {
        self.session.lock().unwrap().take();
        let _ = self.operations.sync_desktop_route(None);
    }
    pub async fn check(&self) -> Result<Value> {
        let session = self.session()?;
        let value = session
            .backend
            .rpc
            .request("account/read", json!({"refreshToken":false}))
            .await?;
        Ok(
            json!({"accountId":DESKTOP_ACCOUNT,"pid":session.backend.pid,"credentialsLoaded":!value["account"].is_null(),"inferenceDispatched":false}),
        )
    }
    fn session(&self) -> Result<Session> {
        if self.quarantined.load(Ordering::SeqCst) {
            return Err(Error::new(
                "SHARED_API_CLEANUP",
                "上次 API 轮次未确认清理；共享 API 已暂停，请保存桌面工作后重启共享连接。",
            ));
        }
        self.session
            .lock()
            .unwrap()
            .clone()
            .filter(|s| !s.cancel.is_cancelled())
            .ok_or_else(|| Error::new("SHARED_NOT_CONNECTED", "桌面共享连接尚未就绪。"))
    }
    pub async fn refresh(&self, connected: bool) -> Result<Option<provider::Provider>> {
        if !connected {
            self.operations.sync_desktop_route(None)?;
            return Ok(None);
        }
        let session = self.session()?;
        let result = provider::read(&session.backend.rpc, &session.cwd).await;
        self.operations
            .sync_desktop_route(result.as_ref().ok().cloned())?;
        result.map(Some)
    }
    pub async fn stream(
        &self,
        job: &RequestJob,
        sender: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> Result<StreamSummary> {
        let session = self.session()?;
        if cancel.is_cancelled() {
            return Err(Error::new("STREAM_CANCELLED", "请求已取消。"));
        }
        if !session.backend.alive().await {
            return Err(Error::new("SHARED_NOT_CONNECTED", "桌面共享后端已退出。"));
        }
        // Each API request owns a separate WS subscription. Closing it never
        // closes the desktop's RPC connection or terminates the shared process.
        let rpc = Rpc::connect(
            session
                .backend
                .endpoint
                .as_deref()
                .ok_or_else(|| Error::new("SHARED_NOT_CONNECTED", "共享端点不可用。"))?,
            session
                .backend
                .token
                .as_deref()
                .ok_or_else(|| Error::new("SHARED_NOT_CONNECTED", "共享凭据不可用。"))?,
        )
        .await?;
        let result = async {
            rpc.initialize(session.installation.client_info()?).await?;
            let mut provider = provider::read(&rpc, &session.cwd).await?;
            if provider != job.provider {
                return Err(Error::new(
                    "ROUTE_CHANGED",
                    "桌面提供方配置已变化，请刷新后重试。",
                ));
            }
            self.operations.check_inference_route(
                DESKTOP_ACCOUNT,
                &job.access,
                &provider,
                &job.model,
            )?;
            provider.model.clone_from(&job.model);
            let guard = Guard::lan(
                &provider,
                Arc::new(AtomicU32::new(session.backend.pid)),
                session.installation.app_server_executable.clone(),
            )
            .await?;
            #[cfg(test)]
            let guard = if let Some(upstream) = &session.fixture {
                guard.close().await;
                Guard::fixture(
                    upstream.clone(),
                    provider.required_headers.clone(),
                    Arc::new(AtomicU32::new(session.backend.pid)),
                    session.installation.app_server_executable.clone(),
                )
                .await?
            } else {
                guard
            };
            let guard = guard.with_client(job.client_payload.as_ref(), sender.clone());
            let work = streaming::guarded_shared_stream(
                &rpc,
                &guard,
                streaming::StreamRequest {
                    provider: &provider,
                    cwd: &session.cwd,
                    prompt: &job.prompt,
                    timeout: Duration::from_secs(120),
                },
                sender,
                cancel.clone(),
            );
            tokio::pin!(work);
            let result = tokio::select! {
                result=&mut work => result,
                _=session.cancel.cancelled() => { cancel.cancel(); work.await }
            };
            guard.close().await;
            if let Ok(summary) = &result {
                if !summary.cleanup_confirmed {
                    self.quarantined.store(true, Ordering::SeqCst);
                    self.operations.sync_desktop_route(None)?;
                }
            } else if result.as_ref().err().is_some_and(|e| {
                ["STREAM_START_UNCERTAIN", "RPC_TIMEOUT"].contains(&e.code.as_str())
            }) {
                self.quarantined.store(true, Ordering::SeqCst);
                self.operations.sync_desktop_route(None)?;
            }
            result
        }
        .await;
        rpc.close();
        result
    }
}
