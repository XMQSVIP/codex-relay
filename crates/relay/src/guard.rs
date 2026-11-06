//! Local HTTP guard, route ownership and upstream delivery.
//! Event validation and SSE framing live in private modules; public imports stay here.
mod events;
mod request;
pub use events::{TextEvents, TextSse};
pub use request::text_request;

use crate::{
    error::{Error, Result},
    platform,
    provider::{Provider, header_name, lan_url},
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

fn blocked() -> Error {
    Error::new(
        "GUARD_EVENT_BLOCKED",
        "上游返回工具、未知事件或无效文本响应。",
    )
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(blocked)
}
fn identifier(value: &Value, key: &str) -> Result<String> {
    let id = string(value, key)?;
    if id.is_empty() || id.len() > 256 {
        return Err(blocked());
    }
    Ok(id.into())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    #[serde(default)]
    pub response_model: Option<String>,
    pub state: String,
    pub requests: u32,
    pub upstream_requests: u32,
    #[serde(default)]
    pub upstream_status: Option<u16>,
    pub forwarded_events: u32,
    pub first_text_forwarded_at: Option<u64>,
    pub upstream_ended_at: Option<u64>,
    pub failure_code: Option<String>,
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            state: "unused".into(),
            response_model: None,
            requests: 0,
            upstream_requests: 0,
            upstream_status: None,
            forwarded_events: 0,
            first_text_forwarded_at: None,
            upstream_ended_at: None,
            failure_code: None,
        }
    }
}
struct Entry {
    model: String,
    claimed: AtomicBool,
    stats: Arc<Mutex<Stats>>,
    client: Option<ClientExchange>,
}
#[derive(Clone)]
struct ClientExchange {
    request: Value,
    sender: mpsc::Sender<Value>,
    metrics: Arc<Mutex<ClientMetrics>>,
    completed: CancellationToken,
}
#[derive(Clone, Default)]
pub struct ClientMetrics {
    pub item_ids: Vec<String>,
    pub delta_count: u32,
    pub text_bytes: usize,
    pub first_delta_at: Option<u64>,
    pub usage: Option<crate::streaming::Usage>,
}
#[derive(Clone)]
pub struct Route {
    pub base_url: String,
    pub stats: Arc<Mutex<Stats>>,
    id: String,
}
impl Route {
    pub fn snapshot(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }
}
struct GuardState {
    upstream: String,
    headers: Vec<String>,
    pid: Arc<AtomicU32>,
    executable: PathBuf,
    port: u16,
    routes: Mutex<HashMap<String, Arc<Entry>>>,
    cancel: CancellationToken,
    slots: Arc<Semaphore>,
}
pub struct Guard {
    state: Arc<GuardState>,
    task: JoinHandle<()>,
    client: Option<ClientExchange>,
}
impl Guard {
    pub fn with_client(mut self, request: Option<&Value>, sender: mpsc::Sender<Value>) -> Self {
        self.client = request.map(|request| ClientExchange {
            request: request.clone(),
            sender,
            metrics: Arc::new(Mutex::new(ClientMetrics::default())),
            completed: CancellationToken::new(),
        });
        self
    }
    pub fn client_mode(&self) -> bool {
        self.client.is_some()
    }
    pub fn client_metrics(&self) -> Option<ClientMetrics> {
        self.client
            .as_ref()
            .map(|c| c.metrics.lock().unwrap().clone())
    }
    pub async fn client_completed(&self) {
        if let Some(client) = &self.client {
            client.completed.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
    pub fn is_client_completed(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|c| c.completed.is_cancelled())
    }
    pub async fn lan(provider: &Provider, pid: Arc<AtomicU32>, exe: PathBuf) -> Result<Self> {
        let url = lan_url(&provider.base_url)?;
        Self::start(url.to_string(), provider.required_headers.clone(), pid, exe).await
    }
    pub async fn fixture(
        upstream: String,
        headers: Vec<String>,
        pid: Arc<AtomicU32>,
        exe: PathBuf,
    ) -> Result<Self> {
        let url = url::Url::parse(&upstream)
            .map_err(|_| Error::new("GUARD_FIXTURE_ENDPOINT", "测试端点无效。"))?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none()
            || url.path() != "/v1"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::new(
                "GUARD_FIXTURE_ENDPOINT",
                "测试端点只能使用本机 /v1。",
            ));
        }
        Self::start(upstream, headers, pid, exe).await
    }
    async fn start(
        upstream: String,
        headers: Vec<String>,
        pid: Arc<AtomicU32>,
        executable: PathBuf,
    ) -> Result<Self> {
        let headers = headers
            .into_iter()
            .map(|s| header_name(&s))
            .collect::<Result<Vec<_>>>()?;
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(GuardState {
            upstream,
            headers,
            pid,
            executable,
            port,
            routes: Mutex::new(HashMap::new()),
            cancel: CancellationToken::new(),
            slots: Arc::new(Semaphore::new(8)),
        });
        let app = Router::new()
            .route("/{route}/v1/responses", post(handle))
            .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
            .with_state(state.clone());
        let cancel = state.cancel.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(cancel.cancelled_owned())
            .await;
        });
        Ok(Self {
            state,
            task,
            client: None,
        })
    }
    pub fn route(&self, model: &str) -> Result<Route> {
        let mut routes = self.state.routes.lock().unwrap();
        if routes.len() >= 128 || self.state.cancel.is_cancelled() {
            return Err(Error::new("GUARD_ROUTE_LIMIT", "过滤路由不可用。"));
        }
        let id = crate::secret();
        let stats = Arc::new(Mutex::new(Stats::default()));
        routes.insert(
            id.clone(),
            Arc::new(Entry {
                model: model.into(),
                claimed: AtomicBool::new(false),
                stats: stats.clone(),
                client: self.client.clone(),
            }),
        );
        Ok(Route {
            base_url: format!("http://127.0.0.1:{}/{id}/v1", self.state.port),
            stats,
            id,
        })
    }
    pub fn release(&self, route: &Route) {
        if route.snapshot().state != "active" {
            self.state.routes.lock().unwrap().remove(&route.id);
        }
    }
    pub async fn close(&self) {
        self.state.cancel.cancel();
        self.task.abort();
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.state.cancel.cancel();
        self.task.abort();
    }
}

async fn handle(
    State(state): State<Arc<GuardState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let reject = |status, code| {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"error":{"code":code,"message":format!("Text guard stopped the request ({code}).")}})
                    .to_string(),
            ))
            .unwrap()
    };
    if peer.ip() != std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        || headers.get("host").and_then(|v| v.to_str().ok())
            != Some(format!("127.0.0.1:{}", state.port).as_str())
    {
        return reject(StatusCode::FORBIDDEN, "GUARD_PEER");
    }
    if headers.contains_key("origin") {
        return reject(StatusCode::FORBIDDEN, "GUARD_ORIGIN");
    }
    let entry = state.routes.lock().unwrap().get(&id).cloned();
    let Some(entry) = entry else {
        return reject(StatusCode::NOT_FOUND, "GUARD_ROUTE");
    };
    let Ok(permit) = state.slots.clone().try_acquire_owned() else {
        return reject(StatusCode::SERVICE_UNAVAILABLE, "GUARD_BUSY");
    };
    if !platform::verify_peer(
        peer.port(),
        state.port,
        state.pid.load(Ordering::SeqCst),
        &state.executable,
    )
    .await
    {
        return reject(StatusCode::FORBIDDEN, "GUARD_PEER_OWNER");
    }
    entry.stats.lock().unwrap().requests += 1;
    if entry.claimed.swap(true, Ordering::SeqCst) {
        return reject(StatusCode::CONFLICT, "GUARD_NO_REPLAY");
    }
    let prepare = (|| -> Result<(Value, reqwest::header::HeaderMap)> {
        if !headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| crate::http_media::is_type(s, "application/json"))
            || headers.contains_key("content-encoding")
        {
            return Err(Error::new("GUARD_REQUEST", "仅支持未压缩 JSON。"));
        }
        let body = text_request(serde_json::from_slice(&body)?, &entry.model)?;
        let body = if let Some(client) = &entry.client {
            crate::client_tools::upstream_request(body, &client.request)
        } else {
            body
        };
        if headers.contains_key("cookie")
            || headers.contains_key("chatgpt-account-id")
            || (headers.contains_key("authorization")
                && !state.headers.iter().any(|n| n == "authorization"))
        {
            return Err(Error::new(
                "GUARD_CREDENTIALS",
                "认证头不在当前提供方范围内。",
            ));
        }
        let mut outbound = reqwest::header::HeaderMap::new();
        for name in &state.headers {
            let value = headers
                .get(name)
                .filter(|v| !v.as_bytes().is_empty() && v.as_bytes().len() <= 16384)
                .ok_or_else(|| Error::new("GUARD_CREDENTIALS", "缺少原生认证头。"))?;
            outbound.insert(
                name.parse::<reqwest::header::HeaderName>().unwrap(),
                value.clone(),
            );
        }
        crate::native_metadata::copy_request_headers(&headers, &mut outbound)?;
        Ok((body, outbound))
    })();
    let (body, outbound) = match prepare {
        Ok(v) => v,
        Err(e) => {
            let mut stats = entry.stats.lock().unwrap();
            stats.state = "blocked".into();
            stats.failure_code = Some(e.code);
            return reject(StatusCode::BAD_GATEWAY, "GUARD_REQUEST");
        }
    };
    entry.stats.lock().unwrap().state = "active".into();
    let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(8);
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _permit = permit;
        let stats = entry.stats.clone();
        let mut ready = Some(ready_tx);
        let work = async {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .map_err(|_| Error::new("GUARD_UPSTREAM", "无法建立上游连接。"))?;
            stats.lock().unwrap().upstream_requests += 1;
            let response = client
                .post(format!("{}/responses", state.upstream))
                .headers(outbound)
                .header("accept", "text/event-stream")
                .header("accept-encoding", "identity")
                .json(&body)
                .send()
                .await
                .map_err(|_| Error::new("GUARD_UPSTREAM", "上游连接失败。"))?;
            stats.lock().unwrap().upstream_status = Some(response.status().as_u16());
            if response.status() != 200
                || !response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| crate::http_media::is_type(s, "text/event-stream"))
            {
                return Err(Error::new(
                    if [401, 403].contains(&response.status().as_u16()) {
                        "GUARD_UPSTREAM_AUTH"
                    } else {
                        "GUARD_UPSTREAM"
                    },
                    "上游未返回有效 SSE。",
                ));
            }
            if let Some(sender) = ready.take() {
                let _ = sender.send(StatusCode::OK);
            }
            let mut stream = response.bytes_stream();
            let mut filter = if let Some(client) = &entry.client {
                TextSse::for_client(&client.request)?
            } else {
                TextSse::default()
            };
            while let Some(data) = stream.next().await {
                let data = data.map_err(|_| Error::new("GUARD_UPSTREAM", "上游流断开。"))?;
                for frame in filter.push(&data)? {
                    dispatch(&entry.client, &tx, &stats, frame).await?;
                }
            }
            stats.lock().unwrap().upstream_ended_at = Some(crate::now_ms());
            for frame in filter.finish()? {
                dispatch(&entry.client, &tx, &stats, frame).await?;
            }
            stats.lock().unwrap().state = "completed".into();
            Ok::<(), Error>(())
        };
        let result = tokio::select! {
            _=state.cancel.cancelled()=>Err(Error::new("GUARD_STOPPED","过滤服务已停止。")),
            _=tx.closed()=>Err(Error::new("GUARD_CANCELLED","原生客户端已断开。")),
            result=tokio::time::timeout(Duration::from_secs(90),work)=>result.unwrap_or_else(|_|Err(Error::new("GUARD_TIMEOUT","上游响应超时。"))),
        };
        if result.is_ok() {
            if let Some(client) = &entry.client {
                // Keep the native HTTP response open until turn/interrupt is
                // acknowledged. No fabricated model frames or EOF retry loop.
                client.completed.cancel();
                tokio::select! {
                    _=tx.closed()=>{},
                    _=state.cancel.cancelled()=>{},
                    _=tokio::time::sleep(Duration::from_secs(20))=>{},
                }
            }
        }
        if let Err(error) = result {
            {
                let mut stats = stats.lock().unwrap();
                stats.state = if error.code == "GUARD_CANCELLED" {
                    "cancelled"
                } else {
                    "blocked"
                }
                .into();
                stats.failure_code = Some(error.code);
            }
            if let Some(sender) = ready.take() {
                let _ = sender.send(StatusCode::BAD_GATEWAY);
            } else {
                let _=tx.send(Ok(Bytes::from_static(b"event: error\ndata: {\"type\":\"error\",\"code\":\"bridge_guard_blocked\",\"message\":\"Text-only guard stopped the response.\"}\n\n"))).await;
            }
        }
    });
    let status = ready_rx.await.unwrap_or(StatusCode::BAD_GATEWAY);
    if status != StatusCode::OK {
        return reject(status, "GUARD_UPSTREAM");
    }
    Response::builder()
        .status(status)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .unwrap()
}
async fn forward(
    tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    stats: &Mutex<Stats>,
    frame: String,
) -> Result<()> {
    {
        let mut stats = stats.lock().unwrap();
        stats.forwarded_events += 1;
        if frame.starts_with("event: response.output_text.delta\n") {
            stats.first_text_forwarded_at.get_or_insert(crate::now_ms());
        }
    }
    tx.send(Ok(Bytes::from(frame)))
        .await
        .map_err(|_| Error::new("GUARD_CANCELLED", "原生客户端已断开。"))
}

async fn dispatch(
    client: &Option<ClientExchange>,
    native: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    stats: &Mutex<Stats>,
    frame: String,
) -> Result<()> {
    let data = frame
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .ok_or_else(blocked)?;
    let event: Value = serde_json::from_str(data)?;
    if matches!(
        event["type"].as_str(),
        Some("response.created" | "response.in_progress" | "response.completed")
    ) && let Some(model) = crate::streaming::reported_model(&event["response"]["model"])
    {
        stats.lock().unwrap().response_model = Some(model);
    }
    let Some(client) = client else {
        return forward(native, stats, frame).await;
    };
    {
        let mut metrics = client.metrics.lock().unwrap();
        if event["type"] == "response.output_text.delta" {
            metrics.delta_count += 1;
            metrics.text_bytes += event["delta"].as_str().unwrap_or("").len();
            metrics.first_delta_at.get_or_insert(crate::now_ms());
        }
        if event["type"] == "response.completed" {
            if event["response"]["status"] != "completed" {
                return Err(blocked());
            }
            metrics.usage = crate::streaming::Usage::from_response(&event["response"]["usage"])?;
            metrics.item_ids = event["response"]["output"]
                .as_array()
                .ok_or_else(blocked)?
                .iter()
                .map(|i| identifier(i, "id"))
                .collect::<Result<_>>()?;
        }
    }
    {
        let mut stats = stats.lock().unwrap();
        stats.forwarded_events += 1;
        if event["type"] == "response.output_text.delta" {
            stats.first_text_forwarded_at.get_or_insert(crate::now_ms());
        }
    }
    tokio::time::timeout(Duration::from_secs(5), client.sender.send(event))
        .await
        .map_err(|_| Error::new("GUARD_CANCELLED", "调用方读取超时。"))?
        .map_err(|_| Error::new("GUARD_CANCELLED", "调用方已断开。"))?;
    // No client response bytes are sent to the native agent.
    Ok(())
}

#[cfg(test)]
mod response_model_tests {
    use super::*;

    #[tokio::test]
    async fn captures_upstream_model_with_and_without_client_forwarding() {
        for client_mode in [false, true] {
            let stats = Mutex::new(Stats::default());
            let (native, mut native_rx) = mpsc::channel(8);
            let (sender, mut client_rx) = mpsc::channel(8);
            let client = client_mode.then(|| ClientExchange {
                request: json!({"model":"requested-model"}),
                sender,
                metrics: Arc::new(Mutex::new(ClientMetrics::default())),
                completed: CancellationToken::new(),
            });
            for (kind, model) in [
                ("response.created", json!("initial-model")),
                ("response.completed", json!("actual-model")),
                ("response.completed", Value::Null),
            ] {
                let event = json!({"type":kind,"response":{"model":model,"status":"completed","output":[]}});
                let frame = format!("event: {kind}\ndata: {event}\n\n");
                dispatch(&client, &native, &stats, frame).await.unwrap();
                if client_mode {
                    assert_eq!(client_rx.recv().await.unwrap(), event);
                } else {
                    assert!(native_rx.recv().await.unwrap().is_ok());
                }
            }
            assert_eq!(
                stats.lock().unwrap().response_model.as_deref(),
                Some("actual-model")
            );
        }
    }
}
