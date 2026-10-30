//! Separate, explicitly configured client listeners. Never mounts management routes.
use crate::{
    admin::AdminState,
    error::{Error, Result},
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use axum_server::tls_rustls::RustlsConfig;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkConfig {
    pub listeners: Vec<ClientListener>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientListener {
    pub listen: SocketAddr,
    pub base_urls: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    pub tls: Option<TlsFiles>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TlsFiles {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub listen: SocketAddr,
    pub base_url: String,
    pub transport: String,
}

/// The management listener binds IPv4; advertise private IPv4 interface addresses.
pub(crate) fn lan_base_urls(origin: &str) -> Vec<String> {
    let Ok(origin) = url::Url::parse(origin) else {
        return Vec::new();
    };
    let Some(port) = origin.port_or_known_default() else {
        return Vec::new();
    };
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() => Some(ip),
            _ => None,
        })
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    addresses
        .into_iter()
        .map(|ip| format!("http://{ip}:{port}/v1"))
        .collect()
}

#[derive(Clone)]
pub struct AccessPolicy {
    hosts: Vec<String>,
    origins: Vec<String>,
    management_listener: bool,
}
fn config_error() -> Error {
    Error::new(
        "NETWORK_CONFIG",
        "网络配置无效：填写独立监听地址、完整的 http(s)://主机[:端口]/v1 地址，以及明确的浏览器来源。",
    )
}
fn parse_url(value: &str, path: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| config_error())?;
    if value.len() > 2048
        || value.trim() != value
        || value.chars().any(char::is_control)
        || !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != path
        || value.contains(['\\', '*'])
        || value.contains('@')
        || matches!(url.host(),Some(url::Host::Ipv4(ip)) if ip.is_unspecified() || ip.is_multicast())
        || matches!(url.host(),Some(url::Host::Ipv6(ip)) if ip.is_unspecified() || ip.is_multicast())
    {
        return Err(config_error());
    }
    Ok(url)
}
impl AccessPolicy {
    pub fn new(base_urls: &[String], allowed_origins: &[String]) -> Result<Self> {
        if base_urls.is_empty() || base_urls.len() > 16 || allowed_origins.len() > 32 {
            return Err(config_error());
        }
        let mut hosts = Vec::new();
        for value in base_urls {
            let url = parse_url(value.trim_end_matches('/'), "/v1")?;
            hosts.push(
                url[url::Position::BeforeHost..url::Position::AfterPort].to_ascii_lowercase(),
            );
            if url.port().is_none() {
                hosts.push(format!(
                    "{}:{}",
                    url.host_str().unwrap(),
                    url.port_or_known_default().unwrap()
                ));
            }
        }
        let mut origins = Vec::new();
        for value in allowed_origins {
            origins.push(parse_url(value, "/")?.origin().ascii_serialization());
        }
        Ok(Self {
            hosts,
            origins,
            management_listener: false,
        })
    }
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        headers.get_all(header::HOST).iter().count() == 1
            && headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| self.hosts.iter().any(|h| h.eq_ignore_ascii_case(v)))
            && headers.get_all(header::ORIGIN).iter().count() <= 1
            && headers.get(header::ORIGIN).is_none_or(|v| {
                v.to_str()
                    .ok()
                    .is_some_and(|v| self.origins.iter().any(|origin| origin == v))
            })
    }
    pub fn local(port: u16) -> Self {
        let urls = [
            format!("http://127.0.0.1:{port}/v1"),
            format!("http://localhost:{port}/v1"),
            format!("http://[::1]:{port}/v1"),
        ];
        let origins = urls
            .iter()
            .map(|v| v.trim_end_matches("/v1").to_owned())
            .collect::<Vec<_>>();
        let mut policy = Self::new(&urls, &origins).expect("local origin");
        policy.management_listener = true;
        policy
    }
    pub(crate) fn allows_with_public(&self, headers: &HeaderMap, public: bool) -> bool {
        if self.management_listener && public {
            same_origin(headers)
        } else {
            self.allows(headers)
        }
    }
}

fn request_host(headers: &HeaderMap) -> Option<url::Url> {
    if headers.get_all(header::HOST).iter().count() != 1 {
        return None;
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.trim() != host || host.contains(['/', '\\', '@', '?', '#']) {
        return None;
    }
    parse_url(&format!("http://{host}/"), "/").ok()
}
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = request_host(headers) else {
        return false;
    };
    if headers.get_all(header::ORIGIN).iter().count() > 1 {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin.to_str().ok().and_then(|v| parse_url(v, "/").ok()) else {
        return false;
    };
    let authority = headers
        .get(header::HOST)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<axum::http::uri::Authority>();
    host.host() == origin.host()
        && authority.is_ok_and(|authority| {
            authority
                .port_u16()
                .unwrap_or(if origin.scheme() == "https" { 443 } else { 80 })
                == origin.port_or_known_default().unwrap()
        })
}
/// Applies to management, static assets, and every client transport. Forwarding
/// headers never change the peer or Host used by this boundary.
pub(crate) async fn public_access(
    State(state): State<Arc<AdminState>>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    if let Some(authority) = request.uri().authority() {
        let authority = authority.as_str().to_owned();
        if let Some(host) = request.headers().get(header::HOST) {
            if !host
                .to_str()
                .is_ok_and(|host| host.eq_ignore_ascii_case(&authority))
            {
                return Err(Error::new("ORIGIN_REJECTED", "请求地址与 Host 不一致。"));
            }
        } else if request.version() == axum::http::Version::HTTP_2 {
            request.headers_mut().insert(
                header::HOST,
                HeaderValue::from_str(&authority)
                    .map_err(|_| Error::new("ORIGIN_REJECTED", "请求地址无效。"))?,
            );
        }
    }
    if !state.access.public() {
        let external_peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|peer| !peer.0.ip().is_loopback());
        let external_host = request_host(request.headers()).is_some_and(|url| match url.host() {
            Some(url::Host::Ipv4(ip)) => !ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => !ip.is_loopback(),
            Some(url::Host::Domain(name)) => name != "localhost",
            None => true,
        });
        if external_peer || external_host {
            return Err(Error::new(
                "PUBLIC_ACCESS_DISABLED",
                "外部访问已关闭，仅允许本机访问。",
            ));
        }
    }
    Ok(next.run(request).await)
}

impl NetworkConfig {
    pub async fn read(path: &Path) -> Result<Self> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| Error::new("NETWORK_CONFIG", "无法读取网络配置文件。"))?;
        if bytes.len() > 64 * 1024 {
            return Err(config_error());
        }
        let mut config: Self = serde_json::from_slice(&bytes).map_err(|_| config_error())?;
        let root = path.parent().unwrap_or(Path::new("."));
        for listener in &mut config.listeners {
            if let Some(tls) = &mut listener.tls {
                if tls.certificate.is_relative() {
                    tls.certificate = root.join(&tls.certificate);
                }
                if tls.private_key.is_relative() {
                    tls.private_key = root.join(&tls.private_key);
                }
            }
        }
        Ok(config)
    }
    pub fn validate(&self, admin_port: u16) -> Result<Vec<Endpoint>> {
        if self.listeners.len() > 8 {
            return Err(config_error());
        }
        let mut endpoints = Vec::new();
        let mut addresses = Vec::new();
        for listener in &self.listeners {
            AccessPolicy::new(&listener.base_urls, &listener.allowed_origins)?;
            if listener.listen.port() == 0
                || listener.listen.port() == admin_port
                || listener.listen.ip().is_multicast()
                || addresses.contains(&listener.listen)
            {
                return Err(config_error());
            }
            addresses.push(listener.listen);
            for value in &listener.base_urls {
                let url = parse_url(value.trim_end_matches('/'), "/v1")?;
                if listener.tls.is_some() && url.scheme() != "https" {
                    return Err(config_error());
                }
                if listener.tls.is_none()
                    && url.scheme() == "https"
                    && !listener.listen.ip().is_loopback()
                {
                    return Err(Error::new(
                        "NETWORK_CONFIG",
                        "由反向代理提供 HTTPS 时，桥的 HTTP 监听请绑定回环地址；直接 HTTPS 请配置证书。",
                    ));
                }
                let base_url = url.to_string();
                if endpoints.iter().any(|e: &Endpoint| e.base_url == base_url) {
                    return Err(config_error());
                }
                endpoints.push(Endpoint {
                    listen: listener.listen,
                    base_url,
                    transport: if listener.tls.is_some() {
                        "https"
                    } else if url.scheme() == "https" {
                        "reverse-proxy"
                    } else {
                        "http"
                    }
                    .into(),
                });
            }
        }
        Ok(endpoints)
    }
}

fn boundary_error() -> Response {
    (StatusCode::FORBIDDEN,Json(json!({"error":{"message":"请求的 Host 或 Origin 未被此 API 入口允许。","type":"permission_error","param":null,"code":"ORIGIN_REJECTED"}}))).into_response()
}
async fn boundary(
    State(policy): State<AccessPolicy>,
    mut request: Request,
    next: Next,
) -> Response {
    // HTTP/2 carries the target in :authority instead of Host. Normalize only
    // protocol authority, never client-supplied forwarding headers.
    if let Some(authority) = request.uri().authority() {
        let authority = authority.as_str();
        if let Some(host) = request.headers().get(header::HOST) {
            if !host
                .to_str()
                .is_ok_and(|host| host.eq_ignore_ascii_case(authority))
            {
                return boundary_error();
            }
        } else if request.version() == axum::http::Version::HTTP_2 {
            let Ok(host) = HeaderValue::from_str(authority) else {
                return boundary_error();
            };
            request.headers_mut().insert(header::HOST, host);
        }
    }
    if !policy.allows(request.headers()) {
        return boundary_error();
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = if request.method() == Method::OPTIONS && origin.is_some() {
        let method = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|v| v.to_str().ok());
        let path = request.uri().path();
        let method_allowed = matches!(
            (path, method),
            ("/v1/models", Some("GET"))
                | ("/v1/usage", Some("GET"))
                | ("/v1/native", Some("GET"))
                | ("/v1/responses", Some("POST"))
        ) || (path
            .strip_prefix("/v1/responses/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            && matches!(method, Some("GET" | "DELETE")));
        let allowed_headers = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .is_none_or(|v| {
                v.to_str().ok().is_some_and(|v| {
                    v.split(',').all(|name| {
                        ["authorization", "content-type", "x-request-id"]
                            .contains(&name.trim().to_ascii_lowercase().as_str())
                    })
                })
            });
        if !method_allowed
            || !allowed_headers
            || request
                .headers()
                .get_all(header::ACCESS_CONTROL_REQUEST_METHOD)
                .iter()
                .count()
                != 1
            || request
                .headers()
                .get_all(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .iter()
                .count()
                > 1
        {
            return boundary_error();
        }
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type, X-Request-ID"),
        );
        response
    } else {
        next.run(request).await
    };
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("X-Request-ID, Retry-After"),
        );
    }
    response
}
async fn response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}
pub fn client_router(state: Arc<AdminState>, policy: AccessPolicy) -> Router {
    Router::new().nest("/v1",crate::client_api::router_with_policy(state.clone(),policy.clone()))
        .fallback(||async{(StatusCode::NOT_FOUND,Json(json!({"error":{"message":"接口不存在。","type":"invalid_request_error","param":null,"code":"NOT_FOUND"}})))})
        .layer(DefaultBodyLimit::max(1024*1024))
        .layer(middleware::from_fn_with_state(policy,boundary))
        .layer(middleware::from_fn_with_state(state.clone(),public_access))
        .layer(middleware::from_fn(response_headers))
        .with_state(state)
}

pub struct BoundListener {
    listener: TcpListener,
    policy: AccessPolicy,
    tls: Option<RustlsConfig>,
}
impl BoundListener {
    pub async fn bind(config: &ClientListener) -> Result<Self> {
        let policy = AccessPolicy::new(&config.base_urls, &config.allowed_origins)?;
        let tls = if let Some(files) = &config.tls {
            let _ = rustls::crypto::ring::default_provider().install_default();
            Some(
                RustlsConfig::from_pem_file(&files.certificate, &files.private_key)
                    .await
                    .map_err(|_| {
                        Error::new(
                            "NETWORK_TLS",
                            "无法加载 TLS 证书和私钥，请检查 PEM 文件、权限及证书是否匹配。",
                        )
                    })?,
            )
        } else {
            None
        };
        let listener = TcpListener::bind(config.listen).map_err(|_| {
            Error::new(
                "NETWORK_BIND",
                "客户端 API 地址无法监听，请检查 IP 和端口占用。",
            )
        })?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            policy,
            tls,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }
    pub async fn serve(
        self,
        state: Arc<AdminState>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let app = client_router(state.clone(), self.policy);
        let handle = axum_server::Handle::new();
        let stop = handle.clone();
        let serve = async move {
            if let Some(tls) = self.tls {
                axum_server::from_tcp_rustls(self.listener, tls)?
                    .handle(handle)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            } else {
                axum_server::from_tcp(self.listener)?
                    .handle(handle)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
        };
        tokio::pin!(serve);
        tokio::select! {
            result=&mut serve => result?,
            _=shutdown.cancelled()=>{
                stop.graceful_shutdown(Some(Duration::from_secs(15)));
                serve.await?;
            }
        }
        Ok(())
    }
}
