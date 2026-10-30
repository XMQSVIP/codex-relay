use super::{Config, Transport, bindings::Binding};
use crate::{
    error::{Error, Result},
    operations::ClientAccess,
};
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, header},
};
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::Value;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    },
};
use uuid::Uuid;

type Socket = WebSocketStream<reqwest::Upgraded>;
struct Reading {
    socket: OwnedMutexGuard<Option<Socket>>,
    completed: bool,
}
impl Drop for Reading {
    fn drop(&mut self) {
        if !self.completed {
            *self.socket = None;
        }
    }
}

pub(super) type Link = Arc<Mutex<Option<Socket>>>;
type Key = (Uuid, Uuid, Uuid, String, String);
#[derive(Default)]
pub(crate) struct Connections {
    links: Mutex<HashMap<Key, (Link, Instant)>>,
    catalogs: Mutex<HashMap<Uuid, Instant>>,
}
impl Connections {
    pub(crate) async fn invalidate(&self, account: Uuid) {
        self.links.lock().await.retain(|k, _| k.0 != account);
        self.catalogs.lock().await.remove(&account);
    }
    pub(crate) async fn catalog_due(&self, account: Uuid, checked: u64) -> bool {
        if checked > 0 && crate::now_ms().saturating_sub(checked) < 300_000 {
            return false;
        }
        let mut cache = self.catalogs.lock().await;
        if cache
            .get(&account)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(300))
        {
            return false;
        }
        cache.insert(account, Instant::now());
        true
    }
    pub(super) async fn remember(
        &self,
        account: Uuid,
        access: &ClientAccess,
        provider: &str,
        id: &str,
        link: Link,
    ) {
        let mut links = self.links.lock().await;
        links.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3600));
        if links.len() >= 4096 {
            if let Some(key) = links
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                links.remove(&key);
            }
        }
        links.insert(
            (
                account,
                access.key_id,
                access.group_id,
                provider.into(),
                id.into(),
            ),
            (link, Instant::now()),
        );
    }
    async fn previous(
        &self,
        account: Uuid,
        access: &ClientAccess,
        provider: &str,
        id: &str,
    ) -> Option<Link> {
        let mut links = self.links.lock().await;
        links.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3600));
        links
            .get_mut(&(
                account,
                access.key_id,
                access.group_id,
                provider.into(),
                id.into(),
            ))
            .map(|(link, at)| {
                *at = Instant::now();
                link.clone()
            })
    }
}
pub(super) struct Transfer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub chunks: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
    pub link: Option<Link>,
}
pub(crate) fn http_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::new("UPSTREAM_TIMEOUT", "上游请求超时。")
    } else {
        Error::new("UPSTREAM_CONNECTION_FAILED", "无法连接上游服务。")
    }
}
pub(crate) fn status_error(status: StatusCode) -> Error {
    Error::new(
        match status.as_u16() {
            401 | 403 => "UPSTREAM_AUTH_FAILED",
            429 => "UPSTREAM_RATE_LIMITED",
            400..=499 => "UPSTREAM_REQUEST_REJECTED",
            _ => "UPSTREAM_REQUEST_FAILED",
        },
        &format!("上游返回 HTTP {}。", status.as_u16()),
    )
}
pub(super) fn headers(source: &HeaderMap) -> HeaderMap {
    let dynamic: Vec<_> = source
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_ascii_lowercase())
        .collect();
    source
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str();
            ![
                "host",
                "authorization",
                "x-openai-actor-authorization",
                "x-api-key",
                "api-key",
                "cookie",
                "set-cookie",
                "connection",
                "keep-alive",
                "proxy-authenticate",
                "proxy-authorization",
                "te",
                "trailer",
                "transfer-encoding",
                "upgrade",
                "content-length",
                "origin",
                "referer",
                "forwarded",
            ]
            .contains(&n)
                && !n.starts_with("sec-")
                && !n.starts_with("x-forwarded-")
                && !n.starts_with("x-codex-relay-")
                && n != "chatgpt-account-id"
                && !dynamic.iter().any(|h| h == n)
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
fn from_http(response: reqwest::Response) -> Transfer {
    Transfer {
        status: response.status(),
        headers: headers(response.headers()),
        chunks: Box::pin(response.bytes_stream().map(|r| r.map_err(http_error))),
        link: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send(
    config: &Config,
    client: &reqwest::Client,
    method: Method,
    path: &str,
    incoming: &HeaderMap,
    body: Bytes,
    value: &Value,
    binding: &Binding,
    previous: Option<&Binding>,
    access: &ClientAccess,
    connections: &Connections,
) -> Result<Transfer> {
    let mut forwarded = headers(incoming);
    forwarded.insert(header::AUTHORIZATION, config.authorization()?);
    forwarded.insert(header::ACCEPT_ENCODING, "identity".parse().unwrap());
    let ws = config.transport == Transport::WebSocket
        && path == "responses"
        && method == Method::POST
        && value["background"] != true;
    if !ws && value["generate"] == false {
        return Err(super::invalid(
            "此上游传输不支持 WebSocket 预热，请使用正常生成请求。",
        ));
    }
    if ws {
        let prior = value["previous_response_id"].as_str();
        let saved = if let Some(id) = prior {
            connections
                .previous(binding.account, access, &binding.provider.id, id)
                .await
        } else {
            None
        };
        if saved.is_none() && previous.is_some_and(|p| p.websocket) {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "上游 WebSocket 会话已断开，请提交完整历史开始新会话。",
            ));
        }
        let link = saved.unwrap_or_else(|| Arc::new(Mutex::new(None)));
        let mut socket = link.clone().lock_owned().await;
        if socket.is_none() && previous.is_some_and(|p| p.websocket) {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "上游 WebSocket 会话已断开，请提交完整历史开始新会话。",
            ));
        }
        if socket.is_none() {
            let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
            let response = client
                .get(config.url("responses")?)
                .version(reqwest::Version::HTTP_11)
                .headers(forwarded.clone())
                .header(header::CONNECTION, "Upgrade")
                .header(header::UPGRADE, "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", &key)
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .map_err(http_error)?;
            if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                let valid = response
                    .headers()
                    .get("sec-websocket-accept")
                    .and_then(|v| v.to_str().ok())
                    == Some(derive_accept_key(key.as_bytes()).as_str())
                    && response
                        .headers()
                        .get(header::UPGRADE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
                if !valid {
                    return Err(Error::new(
                        "UPSTREAM_INVALID_RESPONSE",
                        "上游 WebSocket 握手无效。",
                    ));
                }
                let io = response.upgrade().await.map_err(http_error)?;
                *socket = Some(
                    WebSocketStream::from_raw_socket(
                        io,
                        Role::Client,
                        Some(
                            WebSocketConfig::default()
                                .max_message_size(Some(crate::media::BODY_LIMIT))
                                .max_frame_size(Some(crate::media::BODY_LIMIT)),
                        ),
                    )
                    .await,
                );
            } else if ![404, 405, 426, 501].contains(&response.status().as_u16()) {
                return Ok(from_http(response));
            }
        }
        if socket.is_some() {
            let mut frame = value.clone();
            let object = frame
                .as_object_mut()
                .ok_or_else(|| super::invalid("请求必须是 JSON 对象。"))?;
            object.remove("stream");
            object.remove("background");
            object.insert("type".into(), Value::String("response.create".into()));
            if socket
                .as_mut()
                .unwrap()
                .send(Message::Text(frame.to_string().into()))
                .await
                .is_err()
            {
                *socket = None;
                return Err(Error::new(
                    "UPSTREAM_CONNECTION_FAILED",
                    "上游 WebSocket 发送失败；请求未重放。",
                ));
            }
            let stream = futures_util::stream::unfold(
                Some(Reading {
                    socket,
                    completed: false,
                }),
                |slot: Option<Reading>| async move {
                    let mut slot = slot?;
                    loop {
                        match slot.socket.as_mut()?.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let terminal =
                                    serde_json::from_str::<Value>(&text).ok().is_some_and(|v| {
                                        matches!(
                                            v["type"].as_str(),
                                            Some(
                                                "response.completed"
                                                    | "response.failed"
                                                    | "response.incomplete"
                                                    | "error"
                                            )
                                        )
                                    });
                                slot.completed = terminal;
                                let data = Bytes::from(format!("data: {text}\n\n"));
                                return Some((Ok(data), if terminal { None } else { Some(slot) }));
                            }
                            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                            _ => {
                                *slot.socket = None;
                                return Some((
                                    Err(Error::new(
                                        "UPSTREAM_CONNECTION_FAILED",
                                        "上游 WebSocket 提前断开；请求未重放。",
                                    )),
                                    None,
                                ));
                            }
                        }
                    }
                },
            );
            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
            return Ok(Transfer {
                status: StatusCode::OK,
                headers: response_headers,
                chunks: Box::pin(stream),
                link: Some(link),
            });
        }
        if value["generate"] == false {
            return Err(super::invalid(
                "上游不支持 WebSocket 预热；未发送生成请求。",
            ));
        }
    }
    let response = client
        .request(method, config.url(path)?)
        .headers(forwarded)
        .timeout(Duration::from_secs(1800))
        .body(body)
        .send()
        .await
        .map_err(http_error)?;
    Ok(from_http(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::ws::{Message as AxumMessage, WebSocketUpgrade},
        routing::get,
    };
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn https_and_websocket_use_the_same_connect_proxy_and_verify_tls() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["upstream.invalid".into()]).unwrap();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert.cert.pem().into_bytes(),
            cert.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let app=Router::new().route("/v1/responses",get(|ws:WebSocketUpgrade|async {ws.on_upgrade(|mut socket|async move {
            assert!(matches!(socket.recv().await,Some(Ok(AxumMessage::Text(_)))));
            socket.send(AxumMessage::Text(json!({"type":"response.completed","response":{"id":"tls_response","status":"completed","output":[]}}).to_string().into())).await.unwrap();
        })}).post(||async {Json(json!({"id":"http_response","output":[]}))}));
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app.into_make_service())
                .await
                .unwrap();
        });
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let tunnels = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = tunnels.clone();
        let proxy_task = tokio::spawn(async move {
            loop {
                let (mut client, _) = proxy.accept().await.unwrap();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut header = vec![];
                    loop {
                        let b = client.read_u8().await.unwrap();
                        header.push(b);
                        if header.ends_with(b"\r\n\r\n") {
                            break;
                        }
                        assert!(header.len() < 16384);
                    }
                    assert!(
                        String::from_utf8_lossy(&header)
                            .starts_with(&format!("CONNECT upstream.invalid:{} ", address.port()))
                    );
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut upstream = tokio::net::TcpStream::connect(address).await.unwrap();
                    client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        let trusted = reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::https(&proxy_url).unwrap())
            .add_root_certificate(trusted)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::nil(),
            models: vec![],
        };
        let connections = Connections::default();
        for mode in [Transport::Http, Transport::WebSocket] {
            let config = super::super::Input {
                base_url: format!("https://upstream.invalid:{}/v1", address.port()),
                api_key: Some("private".into()),
                transport: mode,
            }
            .validate(None)
            .unwrap();
            let binding = Binding {
                native_tools: false,
                account: Uuid::new_v4(),
                model: "test".into(),
                provider: config.provider(),
                websocket: false,
            };
            let value = json!({"model":"test","input":"x"});
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
            let mut result = send(
                &config,
                &client,
                Method::POST,
                "responses",
                &headers,
                value.to_string().into(),
                &value,
                &binding,
                None,
                &access,
                &connections,
            )
            .await
            .unwrap();
            assert_eq!(result.status, StatusCode::OK);
            let mut bytes = vec![];
            while let Some(chunk) = result.chunks.next().await {
                bytes.extend_from_slice(&chunk.unwrap());
            }
            assert!(
                String::from_utf8_lossy(&bytes).contains(if mode == Transport::Http {
                    "http_response"
                } else {
                    "tls_response"
                })
            );
        }
        assert!(tunnels.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        let untrusted = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::https(&proxy_url).unwrap())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert!(
            untrusted
                .post(format!(
                    "https://upstream.invalid:{}/v1/responses",
                    address.port()
                ))
                .send()
                .await
                .is_err()
        );
        server.abort();
        proxy_task.abort();
    }
}
