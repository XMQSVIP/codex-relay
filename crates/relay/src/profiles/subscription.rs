//! Optional display metadata: no model calls or persisted credentials.
use super::Profiles;
use crate::{
    backend::Backend,
    error::{Error, Result},
    oauth,
    operations::{Operations, subscription::Reading},
    provider::Provider,
};
use serde_json::Value;
use uuid::Uuid;

impl Profiles {
    pub(super) fn subscription_view(&self, id: Uuid) -> Option<Value> {
        self.proxy_settings.as_ref()?.subscription(id).ok()
    }
    pub(super) fn invalidate_subscription(&self, id: Uuid) {
        if let Some(ops) = &self.proxy_settings {
            let _ = ops.clear_subscription(id);
        }
    }
    pub(super) fn observe_subscription(
        &self,
        id: Uuid,
        provider: &Provider,
        credential: Option<&oauth::Credential>,
    ) {
        if let Some(ops) = &self.proxy_settings {
            if let Some(c) = credential {
                let _ = ops.observe_subscription(
                    id,
                    provider,
                    &oauth::fingerprint(&c.identity),
                    c.plan_type(),
                );
            } else if !provider.is_chatgpt_account() {
                let _ = ops.clear_subscription(id);
            }
        }
    }
    pub(crate) async fn refresh_subscription(
        &self,
        id: Uuid,
        ops: &Operations,
        expected: &Provider,
        manual: bool,
    ) -> Result<()> {
        let isolated = self.is_oauth(id).await?;
        let installation = crate::installation::read_with_refresh().await?;
        let directory = self.inspection_directory()?;
        let home = self.home(id);
        let proxy = self.account_proxy(id);
        let backend = Backend::start_direct_with_proxy(
            &installation,
            directory.path(),
            if isolated { Some(&home) } else { None },
            false,
            &proxy,
        )
        .await?;
        let result = async {
            let (provider, credential) = self
                .read_route(id, &backend, directory.path(), Some(&expected.model))
                .await?;
            if &provider != expected {
                self.invalidate_subscription(id);
                return Err(Error::new(
                    "ROUTE_CHANGED",
                    "本机认证或提供方已变化，请重新检测接入。",
                ));
            }
            let credential = credential.ok_or_else(|| {
                Error::new(
                    "SUBSCRIPTION_UNSUPPORTED",
                    "此账户不支持 ChatGPT 订阅查询。",
                )
            })?;
            let identity = oauth::fingerprint(&credential.identity);
            let attempt = crate::now_ms();
            if !ops.subscription_attempt(id, expected, &identity, attempt, manual)? {
                return Ok(());
            }
            let result = async {
                let mut builder = proxy
                    .resolved()
                    .await?
                    .client_builder()?
                    .retry(reqwest::retry::never());
                // Match the native client's explicitly configured certificate authorities.
                for name in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
                    if let Some(path) = std::env::var_os(name).filter(|s| !s.is_empty()) {
                        if tokio::fs::metadata(&path).await?.len() > 1024 * 1024 {
                            return Err(invalid());
                        }
                        let pem = tokio::fs::read(path).await?;
                        for cert in
                            reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| invalid())?
                        {
                            builder = builder.add_root_certificate(cert);
                        }
                    }
                }
                let client = builder.build().map_err(|_| invalid())?;
                fetch_subscription(
                    &client,
                    &provider.base_url,
                    credential.access_token(),
                    &credential.identity.account_id,
                )
                .await
            }
            .await;
            let (current, current_credential) = self
                .read_route(id, &backend, directory.path(), Some(&expected.model))
                .await?;
            if current != provider
                || current_credential
                    .as_ref()
                    .map(|c| oauth::fingerprint(&c.identity))
                    .as_deref()
                    != Some(identity.as_str())
            {
                self.invalidate_subscription(id);
                return Err(Error::new("ROUTE_CHANGED", "订阅查询期间账户身份已变化。"));
            }
            ops.finish_subscription(id, &provider, &identity, attempt, result)
        }
        .await;
        let stopped = backend.stop().await;
        result.and(stopped)
    }
}

fn invalid() -> Error {
    Error::new("SUBSCRIPTION_UNAVAILABLE", "暂时无法确认订阅到期时间。")
}
fn date(value: &Value) -> Option<u64> {
    let date = chrono::DateTime::parse_from_rfc3339(value.as_str()?.trim()).ok()?;
    u64::try_from(date.timestamp_millis())
        .ok()
        .filter(|t| *t > 0)
}
fn parse(value: &Value) -> Result<Reading> {
    let expires_at = date(&value["active_until"]).ok_or_else(invalid)?;
    let starts_at = date(&value["active_start"]);
    if starts_at.is_some_and(|t| t > expires_at) {
        return Err(invalid());
    }
    Ok(Reading {
        expires_at,
        starts_at,
        will_renew: value["will_renew"].as_bool(),
    })
}

async fn fetch_subscription(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    account: &str,
) -> Result<Reading> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut url = url::Url::parse(base).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || account.is_empty()
            || account.len() > 512
        {
            return Err(invalid());
        }
        let path = url.path().trim_end_matches('/');
        let path = if path.ends_with("/backend-api") {
            format!("{path}/subscriptions")
        } else {
            "/backend-api/subscriptions".into()
        };
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        url.query_pairs_mut().append_pair("account_id", account);
        let origin = url.origin().ascii_serialization();
        let mut response = client
            .get(url)
            .bearer_auth(token)
            .header("ChatGPT-Account-Id", account)
            .header("Accept", "application/json")
            .header("Origin", &origin)
            .header("Referer", format!("{origin}/"))
            .send()
            .await
            .map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        const LIMIT: usize = 64 * 1024;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len().saturating_add(chunk.len()) > LIMIT {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        parse(&serde_json::from_slice(&bytes).map_err(|_| invalid())?)
    })
    .await
    .map_err(|_| Error::new("SUBSCRIPTION_TIMEOUT", "订阅查询超时。"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        http::{HeaderMap, StatusCode, Uri},
        response::IntoResponse,
    };
    use serde_json::json;

    #[test]
    fn subscription_dates_are_not_token_expiry_or_usage_reset() {
        for value in [
            json!({"exp":2000000000}),
            json!({"active_until":"bad"}),
            json!({"active_until":2000000000}),
            json!({"active_until":"2026-01-01T00:00:00Z","active_start":"2027-01-01T00:00:00Z"}),
        ] {
            assert!(parse(&value).is_err());
        }
        let reading =
            parse(&json!({"active_until":"2027-01-01T00:00:00+08:00","will_renew":true})).unwrap();
        assert!(reading.will_renew.unwrap());
        assert_eq!(reading.expires_at, 1798732800000);
    }

    #[tokio::test]
    async fn subscription_transport_is_bounded_and_does_not_redirect_or_leak_errors() {
        async fn handler(uri: Uri, headers: HeaderMap) -> axum::response::Response {
            assert_eq!(headers["authorization"], "Bearer SYNTHETIC_SECRET");
            assert_eq!(headers["chatgpt-account-id"], "fixture");
            assert_eq!(uri.query(), Some("account_id=fixture"));
            match uri.path() {
                "/ok/backend-api/subscriptions" => {
                    axum::Json(json!({"active_until":"2027-01-01T00:00:00Z","will_renew":false}))
                        .into_response()
                }
                "/large/backend-api/subscriptions" => "x".repeat(65537).into_response(),
                "/chunked/backend-api/subscriptions" => {
                    axum::body::Body::from_stream(futures_util::stream::iter(
                        (0..3).map(|_| Ok::<_, std::convert::Infallible>(vec![b'x'; 32768])),
                    ))
                    .into_response()
                }
                "/redirect/backend-api/subscriptions" => (
                    StatusCode::FOUND,
                    [(
                        "location",
                        "/ok/backend-api/subscriptions?account_id=fixture",
                    )],
                )
                    .into_response(),
                "/slow/backend-api/subscriptions" => {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                    "{}".into_response()
                }
                _ => (StatusCode::UNAUTHORIZED, "SYNTHETIC_SECRET").into_response(),
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(handler))
                .await
                .unwrap();
        });
        let client = crate::app_server_proxy::ProxyConfig {
            mode: crate::app_server_proxy::ProxyMode::Direct,
            url: String::new(),
        }
        .resolved()
        .await
        .unwrap()
        .client()
        .unwrap();
        let reading = fetch_subscription(
            &client,
            &format!("{base}/ok/backend-api"),
            "SYNTHETIC_SECRET",
            "fixture",
        )
        .await
        .unwrap();
        assert_eq!(reading.will_renew, Some(false));
        let proxy_client = crate::app_server_proxy::ProxyConfig {
            mode: crate::app_server_proxy::ProxyMode::Custom,
            url: base.clone(),
        }
        .resolved()
        .await
        .unwrap()
        .client()
        .unwrap();
        // This hostname cannot resolve: success proves the configured proxy is used.
        assert!(
            fetch_subscription(
                &proxy_client,
                "http://subscription.invalid/ok/backend-api",
                "SYNTHETIC_SECRET",
                "fixture"
            )
            .await
            .is_ok()
        );
        for path in ["large", "chunked", "redirect", "error", "slow"] {
            let error = fetch_subscription(
                &client,
                &format!("{base}/{path}/backend-api"),
                "SYNTHETIC_SECRET",
                "fixture",
            )
            .await
            .unwrap_err();
            assert!(
                !serde_json::to_string(&error)
                    .unwrap()
                    .contains("SYNTHETIC_SECRET")
            );
        }
        server.abort();
    }
}
