use crate::{
    backend::Backend,
    error::{Error, Result},
    guard::Guard,
    installation, platform, probe, provider,
};
use axum::{
    Router,
    extract::{
        ConnectInfo, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_tungstenite::tungstenite::{Message as NativeMessage, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

struct ProxyState {
    desktop_pid: Arc<AtomicU32>,
    executable: PathBuf,
    port: u16,
    endpoint: String,
    token: String,
    connections: AtomicUsize,
    initialized: AtomicUsize,
    cancel: CancellationToken,
    slots: Arc<tokio::sync::Semaphore>,
}
struct PeerGuard {
    state: Arc<ProxyState>,
    initialized: bool,
}
impl Drop for PeerGuard {
    fn drop(&mut self) {
        self.state.connections.fetch_sub(1, Ordering::SeqCst);
        if self.initialized {
            self.state.initialized.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
async fn upgrade(
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if peer.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || headers.get("host").and_then(|v| v.to_str().ok())
            != Some(format!("127.0.0.1:{}", state.port).as_str())
        || headers.get("origin").is_some_and(|v| v != "app://-")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(permit) = state.slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !platform::verify_peer(
        peer.port(),
        state.port,
        state.desktop_pid.load(Ordering::SeqCst),
        &state.executable,
    )
    .await
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(crate::rpc::MAX_BYTES)
        .max_frame_size(crate::rpc::MAX_BYTES)
        .on_upgrade(move |peer| async move {
            let _permit = permit;
            proxy_peer(state, peer).await
        })
}
async fn proxy_peer(state: Arc<ProxyState>, mut peer: WebSocket) {
    state.connections.fetch_add(1, Ordering::SeqCst);
    let mut guard = PeerGuard {
        state: state.clone(),
        initialized: false,
    };
    let Ok(mut request) = state.endpoint.as_str().into_client_request() else {
        return;
    };
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", state.token).parse().unwrap(),
    );
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(crate::rpc::MAX_BYTES))
        .max_frame_size(Some(crate::rpc::MAX_BYTES));
    let Ok(Ok((mut backend, _))) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    else {
        return;
    };
    let mut initialize_id = None;
    loop {
        tokio::select! {
            _=state.cancel.cancelled()=>break,
            message=peer.recv()=>match message{
                Some(Ok(Message::Text(text)))=>{
                    let Ok(value)=serde_json::from_str::<Value>(&text)else{break};
                    if value["method"]=="initialize" {initialize_id=value.get("id").cloned();}
                    if tokio::time::timeout(Duration::from_secs(10),backend.send(NativeMessage::Text(text.to_string().into()))).await.map_or(true,|r|r.is_err()){break;}
                },Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},_=>break,
            },
            message=backend.next()=>match message{
                Some(Ok(NativeMessage::Text(text)))=>{
                    let Ok(value)=serde_json::from_str::<Value>(&text)else{break};
                    if initialize_id.as_ref().is_some_and(|id|value.get("id")==Some(id))&&value.get("result").is_some()&&value.get("error").is_none()&&!guard.initialized{guard.initialized=true;state.initialized.fetch_add(1,Ordering::SeqCst);initialize_id=None;}
                    if tokio::time::timeout(Duration::from_secs(10),peer.send(Message::Text(text.to_string().into()))).await.map_or(true,|r|r.is_err()){break;}
                },Some(Ok(NativeMessage::Ping(_)|NativeMessage::Pong(_)))=>{},_=>break,
            },
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), backend.close(None)).await;
}

pub async fn status() -> Value {
    match tokio::fs::read(".cache/shared-runtime.json").await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .unwrap_or(json!({"state":"invalid","inferenceEnabled":false})),
        Err(_) => json!({"state":"not-started","inferenceEnabled":false}),
    }
}
pub async fn doctor() -> Result<Value> {
    let snapshot = platform::discover().await?;
    let desktops = platform::desktop_owners(&snapshot);
    let saved = status().await;
    let connected = saved["state"] == "desktop-connected"
        && snapshot.processes.iter().any(|p| {
            Some(p.pid as u64) == saved["backendPid"].as_u64()
                && Some(p.parent_pid as u64) == saved["supervisorPid"].as_u64()
                && p.is_app_server
        })
        && desktops
            .iter()
            .any(|p| Some(p.pid as u64) == saved["desktopPid"].as_u64())
        && snapshot.listeners.iter().any(|l| {
            Some(l.pid as u64) == saved["backendPid"].as_u64()
                && Some(l.port as u64) == saved["backendPort"].as_u64()
                && l.address == "127.0.0.1"
        });
    let installation = installation::read().await.ok();
    let mut native = Value::Null;
    if let Ok(pipe) = std::env::var("CODEX_APP_TOOLS_PIPE_PATH") {
        if let Ok(pid) = platform::pipe_owner(&pipe).await {
            if desktops.iter().any(|p| p.pid == pid) {
                native = json!({"ownerPid":pid,"verified":true});
            }
        }
    }
    Ok(
        json!({"purpose":"rust-read-only-discovery","checkedAt":crate::now_ms(),"desktops":desktops,"native":native,"installation":installation,"shared":saved,"sharedDesktopVerified":connected,"inferenceEnabled":false,"gate":probe::Report::new("doctor").gate}),
    )
}

struct RuntimeLock {
    path: PathBuf,
    remove: bool,
}
impl Drop for RuntimeLock {
    fn drop(&mut self) {
        if self.remove {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub struct Controller {
    running: tokio::sync::Mutex<Option<(CancellationToken, AbortOnDropHandle<()>)>>,
    state: tokio::sync::watch::Sender<Value>,
    api: Arc<std::sync::Mutex<Option<Arc<crate::shared_api::SharedApi>>>>,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            running: tokio::sync::Mutex::new(None),
            state: tokio::sync::watch::channel(json!({"state":"stopped","managed":true})).0,
            api: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}
impl Controller {
    pub async fn check_api(&self) -> Result<Value> {
        let api = self
            .api
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::new("SHARED_NOT_CONNECTED", "桌面共享 API 尚未就绪。"))?;
        api.check().await
    }
    pub fn status(&self) -> Value {
        self.state.borrow().clone()
    }
    pub async fn start(&self, shutdown: &CancellationToken) -> Result<Value> {
        self.start_with(shutdown, |stop, state| async move {
            launch_controlled(4500, 4501, false, stop, Some(state), None).await
        })
        .await
    }
    pub async fn start_api(
        &self,
        shutdown: &CancellationToken,
        operations: Arc<crate::operations::Operations>,
    ) -> Result<Value> {
        let api = Arc::new(crate::shared_api::SharedApi::new(operations));
        let holder = self.api.clone();
        self.start_with(shutdown, move |stop, state| async move {
            *holder.lock().unwrap() = Some(api.clone());
            let result =
                launch_controlled(4500, 4501, false, stop, Some(state), Some(api.clone())).await;
            api.detach();
            holder.lock().unwrap().take();
            result
        })
        .await
    }
    pub async fn stream(
        &self,
        job: &crate::inference::RequestJob,
        sender: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> Result<crate::streaming::StreamSummary> {
        let api = self
            .api
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::new("SHARED_NOT_CONNECTED", "桌面共享 API 尚未就绪。"))?;
        if self.status()["state"] != "desktop-connected" {
            return Err(Error::new("SHARED_NOT_CONNECTED", "桌面共享连接已断开。"));
        }
        api.stream(job, sender, cancel).await
    }
    async fn start_with<F, Fut>(&self, shutdown: &CancellationToken, run: F) -> Result<Value>
    where
        F: FnOnce(CancellationToken, tokio::sync::watch::Sender<Value>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let mut running = self.running.lock().await;
        if shutdown.is_cancelled() {
            return Err(Error::new("SERVICE_SHUTDOWN", "服务正在退出。"));
        }
        if running
            .as_ref()
            .is_some_and(|(_, task)| !task.is_finished())
        {
            return Err(Error::new("BUSY", "共享连接正在启动或运行。"));
        }
        let cancel = shutdown.child_token();
        let stop = cancel.clone();
        let state = self.state.clone();
        let api_holder = self.api.clone();
        state.send_replace(json!({"state":"starting","managed":true}));
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async { run(stop, state.clone()).await })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(Error::new(
                        "SHARED_RUNTIME_FAILED",
                        "共享任务异常退出，请重新启动。",
                    ))
                });
            if let Some(api) = api_holder.lock().unwrap().take() {
                api.detach();
            }
            state.send_replace(match result {
                Ok(()) => json!({"state":"stopped","managed":true}),
                Err(error) => json!({"state":"failed","managed":true,"error":error}),
            });
        }));
        *running = Some((cancel, task));
        Ok(self.status())
    }
    pub async fn stop(&self) -> Value {
        let mut running = self.running.lock().await;
        if let Some((cancel, task)) = running.as_mut() {
            if !task.is_finished() {
                self.state
                    .send_replace(json!({"state":"stopping","managed":true}));
                cancel.cancel();
                let _ = task.await;
            }
        }
        running.take();
        self.status()
    }
}

pub async fn launch(backend_port: u16, desktop_port: u16, verify_lan: bool) -> Result<()> {
    launch_controlled(
        backend_port,
        desktop_port,
        verify_lan,
        CancellationToken::new(),
        None,
        None,
    )
    .await
}
async fn launch_controlled(
    backend_port: u16,
    desktop_port: u16,
    verify_lan: bool,
    stop: CancellationToken,
    updates: Option<tokio::sync::watch::Sender<Value>>,
    api: Option<Arc<crate::shared_api::SharedApi>>,
) -> Result<()> {
    if backend_port == desktop_port || backend_port < 1024 || desktop_port < 1024 {
        return Err(Error::new("PORT_CONFLICT", "共享端口必须不同且大于 1023。"));
    }
    if stop.is_cancelled() {
        return Ok(());
    }
    let installation = installation::read_with_refresh().await?;
    let (desktop_executable, _) = installation.desktop_paths()?;
    installation::validate(&installation)
        .await
        .map_err(|error| {
            if error.code == "IO_ERROR" {
                Error::new(
                    "DESKTOP_INSTALLATION_IO",
                    "读取原生安装文件失败，请重新检测安装并检查文件访问权限。",
                )
            } else {
                error
            }
        })?;
    if !platform::desktop_owners(&platform::discover().await?).is_empty() {
        return Err(Error::new(
            "DESKTOP_RUNNING",
            "请保存工作并完全退出当前桌面，再运行共享启动器。已有进程未被停止。",
        ));
    }
    if stop.is_cancelled() {
        return Ok(());
    }
    tokio::fs::create_dir_all(".cache")
        .await
        .map_err(|e| crate::desktop_process::io_error("SHARED_CACHE", "创建共享运行目录", e))?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(".cache/shared-runtime.lock")
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::new("RUNTIME_LOCKED", "共享运行锁已存在，请核验其进程后再处理。")
            } else {
                crate::desktop_process::io_error("SHARED_LOCK", "创建共享运行锁", e)
            }
        })?;
    write!(file, "{}", std::process::id())
        .map_err(|e| crate::desktop_process::io_error("SHARED_LOCK", "写入共享运行锁", e))?;
    let mut lock = RuntimeLock {
        path: ".cache/shared-runtime.lock".into(),
        remove: true,
    };
    let backend = Arc::new(
        Backend::start(
            &installation,
            installation.app_server_executable.parent().unwrap(),
            None,
            Some(backend_port),
            false,
        )
        .await?,
    );
    let pid = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    let state = Arc::new(ProxyState {
        desktop_pid: pid.clone(),
        executable: desktop_executable.to_owned(),
        port: desktop_port,
        endpoint: backend.endpoint.clone().unwrap(),
        token: backend.token.clone().unwrap(),
        connections: AtomicUsize::new(0),
        initialized: AtomicUsize::new(0),
        cancel: cancel.clone(),
        slots: Arc::new(tokio::sync::Semaphore::new(8)),
    });
    if let Some(api) = &api {
        api.attach(backend.clone(), installation.clone(), stop.clone())
            .await?;
    }
    let result=async{
        let listener=tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,desktop_port)).await.map_err(|e| crate::desktop_process::io_error("DESKTOP_PORT", &format!("监听桌面连接端口 {desktop_port}"), e))?;
        let app=Router::new().route("/desktop",get(upgrade)).with_state(state.clone());let shutdown=cancel.clone();
        let proxy=AbortOnDropHandle::new(tokio::spawn(async move{let _=axum::serve(listener,app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown.cancelled_owned()).await;}));
        if stop.is_cancelled() { return Ok(()); }
        let mut desktop=crate::desktop_process::DesktopProcess::start(desktop_executable, desktop_port).await?;
        let desktop_pid=desktop.pid()?;pid.store(desktop_pid,Ordering::SeqCst);
        println!("{}",json!({"event":"shared-runtime-started","backendPid":backend.pid,"desktopPid":desktop_pid,"inferenceEnabled":false}));
        let mut verification:Option<AbortOnDropHandle<bool>>=None;let mut verification_state=if verify_lan{"waiting-for-desktop"}else{"not-requested"}.to_owned();let mut attempted=false;
        let mut tick=tokio::time::interval(Duration::from_secs(1));let interrupted=tokio::signal::ctrl_c();tokio::pin!(interrupted);
        let loop_result=loop{tokio::select!{
            _=&mut interrupted=>break Ok(()),
            _=stop.cancelled()=>break Ok(()),
            _=tick.tick()=>{
                if desktop.exited()?{break Ok(());}
                if !backend.alive().await{break Err(Error::new("BACKEND_EXIT","共享原生后端已退出。"));}
                let initialized=state.initialized.load(Ordering::SeqCst);
                if verify_lan&&!attempted&&initialized>0{
                    attempted=true;verification_state="running".into();let backend=backend.clone();let exe=installation.app_server_executable.clone();let connection=state.clone();
                    verification=Some(AbortOnDropHandle::new(tokio::spawn(async move{
                        let result=verify_real_lan(&backend,&exe,connection).await;
                        let (passed,value)=match result{Ok(report)=>(report.passed(),serde_json::to_value(report).unwrap()),Err(e)=>(false,json!({"error":e,"inferenceEnabled":false}))};
                        let _=tokio::fs::write(".cache/lan-verification.json",serde_json::to_vec_pretty(&value).unwrap()).await;
                        println!("{}",json!({"event":"shared-lan-check-finished","passed":passed,"report":".cache/lan-verification.json","inferenceEnabled":false}));passed
                    })));
                }
                if verification.as_ref().is_some_and(|task|task.is_finished()){verification_state=if verification.take().unwrap().await.unwrap_or(false){"passed"}else{"failed"}.into();}
                let value=json!({"format":1,"updatedAt":crate::now_ms().to_string(),"supervisorPid":std::process::id(),"backendPid":backend.pid,"desktopPid":desktop_pid,"backendPort":backend_port,"desktopPort":desktop_port,"state":if initialized>0{"desktop-connected"}else{"waiting-for-desktop"},"connections":state.connections.load(Ordering::SeqCst),"initializedConnections":initialized,"lanVerification":verification_state,"inferenceEnabled":false,"implementation":"rust"});
                if let Some(updates) = &updates { let mut live = value.clone(); live["managed"] = json!(true);
                    if let Some(api) = &api {
                        match api.refresh(initialized > 0).await {
                            Ok(provider) => { live["inferenceEnabled"] = json!(provider.is_some()); live["provider"] = json!(provider); },
                            Err(error) => { live["inferenceEnabled"] = json!(false); live["routeError"] = json!(error); }
                        }
                    }
                    updates.send_replace(live); }
                tokio::fs::write(".cache/shared-runtime.json.tmp",serde_json::to_vec_pretty(&value)?).await.map_err(|e| crate::desktop_process::io_error("SHARED_STATUS_WRITE", "写入共享状态", e))?;tokio::fs::rename(".cache/shared-runtime.json.tmp",".cache/shared-runtime.json").await.map_err(|e| crate::desktop_process::io_error("SHARED_STATUS_REPLACE", "更新共享状态", e))?;
            }
        }};
        if let Some(task)=verification{task.abort();}cancel.cancel();proxy.abort();loop_result
    }.await;
    cancel.cancel();
    if let Some(api) = &api {
        api.detach();
    }
    if let Err(error) = backend.stop().await {
        lock.remove = false;
        return Err(error);
    }
    let mut saved = status().await;
    saved["state"] = json!(if result.is_ok() { "stopped" } else { "failed" });
    saved["inferenceEnabled"] = json!(false);
    let saved_result = tokio::fs::write(
        ".cache/shared-runtime.json",
        serde_json::to_vec_pretty(&saved)?,
    )
    .await
    .map_err(|e| crate::desktop_process::io_error("SHARED_STATUS_WRITE", "保存共享退出状态", e));
    // Preserve the initial launch failure if the final status write also fails.
    result.and(saved_result)
}
async fn verify_real_lan(
    backend: &Backend,
    exe: &Path,
    state: Arc<ProxyState>,
) -> Result<probe::Report> {
    let connected = || state.initialized.load(Ordering::SeqCst) > 0 && !state.cancel.is_cancelled();
    if !connected() {
        return Err(Error::new(
            "LAN_DESKTOP_REQUIRED",
            "实际桌面尚未连接共享后端。",
        ));
    }
    let cwd = std::env::current_dir()?.join(".cache/service-workspace");
    tokio::fs::create_dir_all(&cwd).await?;
    let provider = provider::read(&backend.rpc, &cwd).await?;
    let guard = Guard::lan(&provider, Arc::new(AtomicU32::new(backend.pid)), exe.into()).await?;
    let mut report = probe::Report::new("rust-shared-desktop-real-lan-verification");
    for (name, prompt, interrupt) in [
        (
            "text",
            "请用中文分两段解释流式输出，每段约一百字。只回答文字。",
            false,
        ),
        (
            "interrupt",
            "从 1 到 1000 逐行编号，每行写一句不同的简短中文句子。只回答文字。",
            true,
        ),
    ] {
        if !connected() {
            return Err(Error::new("LAN_DESKTOP_REQUIRED", "桌面共享连接已断开。"));
        }
        println!("{}", json!({"event":"shared-lan-check","step":name}));
        let turn =
            probe::guarded_turn(&backend.rpc, &guard, &provider, &cwd, prompt, interrupt).await?;
        let stats = turn.guard.as_ref().unwrap();
        let passed = if interrupt {
            turn.status == "interrupted"
                && turn.interrupt_acknowledged
                && stats.state == "cancelled"
        } else {
            turn.status == "completed"
                && turn.exact_correlation
                && turn
                    .first_delta_at
                    .zip(stats.upstream_ended_at)
                    .is_some_and(|(a, b)| a < b)
        };
        report.checks.push(probe::Check::new(
            name,
            passed,
            "真实共享后端与选定 LAN 提供方的文本或取消检查。",
        ));
        report.results.push(serde_json::to_value(turn)?);
        if !passed {
            break;
        }
    }
    guard.close().await;
    Ok(report)
}

#[cfg(test)]
mod control_tests {
    use super::*;
    async fn idle(
        stop: CancellationToken,
        updates: tokio::sync::watch::Sender<Value>,
    ) -> Result<()> {
        updates.send_replace(json!({"state":"desktop-connected","managed":true}));
        stop.cancelled().await;
        Ok(())
    }
    async fn wait_for(controller: &Controller, expected: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while controller.status()["state"] != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn controller_owns_one_runtime_and_stop_is_idempotent() {
        let controller = Controller::default();
        let shutdown = CancellationToken::new();
        controller.start_with(&shutdown, idle).await.unwrap();
        wait_for(&controller, "desktop-connected").await;
        assert_eq!(
            controller
                .start_with(&shutdown, idle)
                .await
                .unwrap_err()
                .code,
            "BUSY"
        );
        assert_eq!(controller.stop().await["state"], "stopped");
        assert_eq!(controller.stop().await["state"], "stopped");
        controller.start_with(&shutdown, idle).await.unwrap();
        shutdown.cancel();
        assert_eq!(controller.stop().await["state"], "stopped");
        assert_eq!(
            controller
                .start_with(&shutdown, idle)
                .await
                .unwrap_err()
                .code,
            "SERVICE_SHUTDOWN"
        );
    }
    #[tokio::test]
    async fn failures_are_visible_and_a_new_controller_does_not_adopt_old_runtime() {
        let controller = Controller::default();
        controller
            .start_with(&CancellationToken::new(), |_, _| async {
                Err(Error::new("DESKTOP_RUNNING", "请先退出普通桌面。"))
            })
            .await
            .unwrap();
        wait_for(&controller, "failed").await;
        assert_eq!(controller.status()["error"]["code"], "DESKTOP_RUNNING");
        assert_eq!(Controller::default().stop().await["state"], "stopped");
        controller.stop().await;
        controller
            .start_with(&CancellationToken::new(), idle)
            .await
            .unwrap();
        controller.stop().await;
    }
    #[tokio::test]
    async fn unexpected_task_failure_does_not_leave_starting_state() {
        let controller = Controller::default();
        controller
            .start_with(&CancellationToken::new(), |_, _| async {
                panic!("synthetic failure")
            })
            .await
            .unwrap();
        wait_for(&controller, "failed").await;
        assert_eq!(
            controller.status()["error"]["code"],
            "SHARED_RUNTIME_FAILED"
        );
        controller.stop().await;
    }
}
