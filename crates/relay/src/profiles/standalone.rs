//! 独立工具的共用原生认证、账户槽位与清理边界；不负责各接口的请求语义。
use super::Profiles;
use crate::{
    backend::Backend,
    error::{Error, Result},
    inference::RequestJob,
    operations::Operations,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl Profiles {
    pub(super) async fn standalone_request<T, F, Fut>(
        &self,
        job: &RequestJob,
        ops: &Operations,
        path: &str,
        cancel: CancellationToken,
        send: F,
    ) -> Result<T>
    where
        F: FnOnce(reqwest::Client, url::Url, reqwest::header::HeaderMap) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let (_slot, mut worker) = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            slot = self.shared_response_slot(job.account, ops.account_concurrency(job.account)?, &[0xff;32], 0, |_| Ok(false)) => slot?,
        };
        if let Some(old) = worker.take() {
            old.stop().await?;
        }
        let directory = tempfile::Builder::new()
            .prefix("standalone-")
            .tempdir_in(self.root.join(job.account.to_string()).join("workspace"))?;
        let installation = crate::installation::read_with_refresh().await?;
        let oauth = self.is_oauth(job.account).await?;
        let home = self.home(job.account);
        let proxy = self.account_proxy(job.account);
        let backend = Arc::new(
            Backend::start_direct_with_proxy(
                &installation,
                directory.path(),
                if oauth { Some(&home) } else { None },
                false,
                &proxy,
            )
            .await?,
        );
        self.track_client(job.account, &backend);
        *worker = Some(backend.clone());
        let result = {
            let work = async {
                let (provider, credential) = self
                    .read_route(
                        job.account,
                        &backend,
                        directory.path(),
                        if oauth {
                            Some(&job.provider.model)
                        } else {
                            None
                        },
                    )
                    .await?;
                ops.check_inference_route(job.account, &job.access, &provider, &job.model)?;
                let config = backend
                    .rpc
                    .request(
                        "config/read",
                        json!({"cwd":directory.path(),"includeLayers":false}),
                    )
                    .await?;
                let selected = &config["config"]["model_providers"][&provider.id];
                let openai_auth =
                    provider.id == "openai" || selected["requires_openai_auth"] == true;
                let status = if credential.is_none() && openai_auth {
                    backend
                        .rpc
                        .request(
                            "getAuthStatus",
                            json!({"includeToken":true,"refreshToken":false}),
                        )
                        .await?
                } else {
                    Value::Null
                };
                let (url, headers) = transport(
                    &config["config"],
                    &provider,
                    &status,
                    credential.as_ref(),
                    path,
                    &backend.client_info.name,
                    backend.user_agent.as_deref(),
                )?;
                let client = proxy.resolved().await?.client()?;
                send(client, url, headers).await
            };
            tokio::pin!(work);
            let mut checks = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break Err(cancelled()),
                    result = &mut work => break result,
                    _ = checks.tick() => if let Err(error) = ops.check_inference_route(job.account, &job.access, &job.provider, &job.model) { break Err(error); }
                }
            }
        };
        // The HTTP future is dropped before releasing capacity or stopping auth.
        if let Err(error) = backend.stop().await {
            // 未确认原生进程退出时保留工作目录及 worker，供账户清理重试。
            let _ = directory.keep();
            return Err(error);
        }
        *worker = None;
        let cleaned = directory.close().map_err(Error::from);
        match result {
            Err(error) => Err(error),
            Ok(response) => {
                cleaned?;
                Ok(response)
            }
        }
    }
}
fn cancelled() -> Error {
    Error::new("REQUEST_CANCELLED", "独立工具请求已取消。")
}
fn invalid() -> Error {
    Error::new(
        "PROVIDER_CONFIG",
        "独立工具接口无法使用当前提供方认证配置。",
    )
}

fn transport(
    config: &Value,
    provider: &crate::provider::Provider,
    status: &Value,
    credential: Option<&crate::oauth::Credential>,
    path: &str,
    originator: &str,
    user_agent: Option<&str>,
) -> Result<(url::Url, reqwest::header::HeaderMap)> {
    use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
    let selected = &config["model_providers"][&provider.id];
    if ["auth", "gateway_oauth", "aws"]
        .iter()
        .any(|key| !selected[*key].is_null())
    {
        return Err(Error::new(
            "IMAGE_PROVIDER_AUTH_UNSUPPORTED",
            "此提供方的动态认证尚未接入独立工具接口；支持账号 OAuth、API Key 和静态提供方请求头。",
        ));
    }
    let chatgpt = credential.is_some()
        || matches!(
            status["authMethod"].as_str(),
            Some("chatgpt" | "chatgptAuthTokens")
        );
    let explicit = selected["base_url"].as_str().or_else(|| {
        (provider.id == "openai" && credential.is_none())
            .then(|| config["openai_base_url"].as_str())
            .flatten()
    });
    let env_base =
        (provider.id == "openai" && credential.is_none() && config["openai_base_url"].is_null())
            .then(|| std::env::var("OPENAI_BASE_URL").ok())
            .flatten();
    let base = explicit
        .or(env_base.as_deref())
        .unwrap_or(&provider.base_url);
    let mut url = url::Url::parse(base).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let mut prefix = url.path().trim_end_matches('/').to_owned();
    if chatgpt
        && provider.id == "openai"
        && explicit.is_none()
        && env_base.is_none()
        && !prefix.ends_with("/codex")
    {
        prefix.push_str("/codex");
    }
    url.set_path(&format!("{prefix}/{path}"));
    let mut headers = HeaderMap::new();
    for key in ["http_headers", "env_http_headers"] {
        if let Some(values) = selected[key].as_object() {
            for (name, value) in values {
                let name = crate::provider::header_name(name)?;
                let value = value.as_str().ok_or_else(invalid)?;
                let resolved = if key == "env_http_headers" {
                    std::env::var(value).map_err(|_| invalid())?
                } else {
                    value.to_owned()
                };
                headers.insert(
                    HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?,
                    HeaderValue::from_str(&resolved).map_err(|_| invalid())?,
                );
            }
        }
    }
    let token = if let Some(credential) = credential {
        Some(credential.access_token.clone())
    } else if let Some(key) = selected["env_key"].as_str() {
        Some(std::env::var(key).map_err(|_| invalid())?)
    } else if let Some(token) = selected["experimental_bearer_token"].as_str() {
        Some(token.to_owned())
    } else {
        status["authToken"].as_str().map(str::to_owned)
    };
    if let Some(token) = token {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid())?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    } else if selected["requires_openai_auth"] == true || provider.id == "openai" {
        return Err(Error::new(
            "PROVIDER_AUTH_MISSING",
            "独立工具接口未能读取选中账号的认证信息。",
        ));
    }
    if chatgpt {
        let identity = if let Some(credential) = credential {
            credential.identity.clone()
        } else {
            crate::oauth::parse(&json!({"accessToken":status["authToken"]}))?.identity
        };
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(&identity.account_id).map_err(|_| invalid())?,
        );
    }
    headers.insert(
        "originator",
        HeaderValue::from_str(originator).map_err(|_| invalid())?,
    );
    if let Some(agent) = user_agent {
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(agent).map_err(|_| invalid())?,
        );
    }
    Ok((url, headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider(id: &str, base: &str) -> crate::provider::Provider {
        crate::provider::Provider {
            execution: Default::default(),
            id: id.into(),
            model: "text-model".into(),
            base_url: base.into(),
            auth: "none".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        }
    }
    #[test]
    fn image_transport_uses_configured_account_credentials_and_fixed_endpoint() {
        let config = json!({"model_providers":{"local":{"base_url":"http://127.0.0.1:1234/v1?api-version=test","experimental_bearer_token":"UPSTREAM_ONLY","http_headers":{"x-route":"image"}}}});
        let (url, headers) = transport(
            &config,
            &provider("local", "http://127.0.0.1:1234/v1"),
            &Value::Null,
            None,
            "images/edits",
            "codex_desktop",
            Some("native-agent"),
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:1234/v1/images/edits?api-version=test"
        );
        assert_eq!(headers["authorization"], "Bearer UPSTREAM_ONLY");
        assert!(headers["authorization"].is_sensitive());
        assert_eq!(headers["x-route"], "image");
        assert_eq!(headers["user-agent"], "native-agent");
        let (url, _) = transport(
            &json!({}),
            &provider("openai", "https://api.openai.com/v1"),
            &json!({"authMethod":"apiKey","authToken":"API_KEY"}),
            None,
            "images/generations",
            "codex_desktop",
            None,
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://api.openai.com/v1/images/generations");
        assert!(
            transport(
                &json!({"model_providers":{"local":{"aws":{}}}}),
                &provider("local", "http://127.0.0.1:1234/v1"),
                &Value::Null,
                None,
                "images/generations",
                "codex_desktop",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn oauth_image_transport_uses_account_binding_and_codex_path() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let token = format!("e30.{}.signature", URL_SAFE_NO_PAD.encode(json!({"exp":crate::now_ms()/1000+3600,"https://api.openai.com/auth":{"chatgpt_account_id":"image-account"}}).to_string()));
        let credential = crate::oauth::parse(&json!({"accessToken":token})).unwrap();
        let (url, headers) = transport(
            &json!({}),
            &provider("openai", "https://chatgpt.com/backend-api"),
            &Value::Null,
            Some(&credential),
            "images/generations",
            "codex_desktop",
            None,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://chatgpt.com/backend-api/codex/images/generations"
        );
        assert_eq!(headers["chatgpt-account-id"], "image-account");
        assert_eq!(headers["authorization"], format!("Bearer {token}"));
        let (url, search_headers) = transport(
            &json!({}),
            &provider("openai", "https://chatgpt.com/backend-api"),
            &Value::Null,
            Some(&credential),
            "alpha/search",
            "codex_desktop",
            None,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://chatgpt.com/backend-api/codex/alpha/search"
        );
        assert_eq!(search_headers["chatgpt-account-id"], "image-account");
        assert_eq!(search_headers["authorization"], format!("Bearer {token}"));
    }
}
