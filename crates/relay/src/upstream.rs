//! Direct API-key upstream configuration. Secrets are serialized only to storage and explicit backups.
use crate::{
    error::{Error, Result},
    provider::Provider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod bindings;
mod requirements;
pub(crate) use bindings::{bind_search, search_account, search_native_mode};
pub(crate) mod chat_test;
pub(crate) mod dispatch;
mod transport;
pub(crate) use transport::Connections;
pub(crate) use transport::{http_error, status_error};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[default]
    #[serde(rename = "http-sse")]
    Http,
    #[serde(rename = "ws-first")]
    WebSocket,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub transport: Transport,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    pub base_url: String,
    api_key: String,
    pub transport: Transport,
    pub generation: uuid::Uuid,
}
impl Input {
    pub(crate) fn validate(self, old: Option<&Config>) -> Result<Config> {
        let base_url = validate_base_url(&self.base_url)?;
        let key = self
            .api_key
            .or_else(|| old.map(|c| c.api_key.clone()))
            .ok_or_else(|| invalid("请提供 API 密钥。"))?;
        if key.trim().is_empty()
            || key.len() > 8192
            || key.chars().any(char::is_control)
            || reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")).is_err()
        {
            return Err(invalid("API 密钥不能为空或包含控制字符。"));
        }
        let generation = old
            .filter(|old| {
                old.base_url == base_url && old.api_key == key && old.transport == self.transport
            })
            .map_or_else(uuid::Uuid::new_v4, |old| old.generation);
        Ok(Config {
            base_url,
            api_key: key,
            transport: self.transport,
            generation,
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Backup {
    base_url: String,
    api_key: String,
    transport: Transport,
}
impl Backup {
    pub(super) fn restore(&self) -> Result<Config> {
        Input {
            base_url: self.base_url.clone(),
            api_key: Some(self.api_key.clone()),
            transport: self.transport,
        }
        .validate(None)
    }
}
fn validate_base_url(value: &str) -> Result<String> {
    let raw = value.trim();
    let url = url::Url::parse(raw).map_err(|_| invalid("请输入有效的基础 URL。"))?;
    if raw.len() > 2048
        || raw.chars().any(char::is_control)
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(invalid(
            "基础 URL 须为 HTTP/HTTPS API 前缀，不包含账号密码、查询参数或片段。",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').into())
}
impl Config {
    pub fn view(&self) -> Value {
        json!({"baseUrl":self.base_url,"transport":self.transport,"hasApiKey":!self.api_key.is_empty()})
    }
    pub(super) fn backup(&self) -> Result<Backup> {
        self.authorization()?;
        Ok(Backup {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            transport: self.transport,
        })
    }
    pub fn provider(&self) -> Provider {
        Provider {
            execution: crate::provider::Execution::ApiKey,
            id: format!("api-key-{}", self.generation),
            model: String::new(),
            base_url: self.base_url.clone(),
            auth: "api-key".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        }
    }
    pub fn url(&self, path: &str) -> Result<url::Url> {
        url::Url::parse(&format!(
            "{}/{}",
            self.base_url,
            path.trim_start_matches('/')
        ))
        .map_err(|_| invalid("上游接口地址无效。"))
    }
    pub fn authorization(&self) -> Result<reqwest::header::HeaderValue> {
        if self.api_key.is_empty() {
            return Err(invalid("请先配置上游 API 密钥。"));
        }
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|_| invalid("API 密钥无效。"))
    }
}
pub(crate) fn invalid(message: &str) -> Error {
    Error::new("INVALID_INPUT", message)
}

pub(crate) async fn refresh_models(
    state: &std::sync::Arc<crate::admin::AdminState>,
    access: &crate::operations::ClientAccess,
) {
    use futures_util::{StreamExt, stream};
    let snapshot = state.operations.snapshot();
    let accounts = snapshot["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            a["enabled"] == true
                && a["groupIds"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|id| id == &access.group_id.to_string()))
                && a["route"]["execution"] == "api-key"
        })
        .cloned()
        .collect::<Vec<_>>();
    stream::iter(accounts)
        .for_each_concurrent(4, |account| async move {
            let Some(id) = account["id"]
                .as_str()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
            else {
                return;
            };
            if !state
                .upstream_connections
                .catalog_due(
                    id,
                    account["capabilities"]["checkedAt"].as_u64().unwrap_or(0),
                )
                .await
            {
                return;
            }
            if let Some(base) = account["route"]["baseUrl"].as_str() {
                if state.check_route_loop(base).await.is_err() {
                    return;
                }
            }
            if let Ok((_, caps, _)) = state
                .profiles
                .inspect_upstream(
                    id,
                    crate::profiles::RouteCheck::Access,
                    state.shutdown.child_token(),
                )
                .await
            {
                let _ = state.operations.publish_capabilities(id, caps);
            }
        })
        .await;
}

pub(crate) async fn read_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err(Error::new(
                "UPSTREAM_INVALID_RESPONSE",
                "上游配置响应过大。",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::new("UPSTREAM_INVALID_RESPONSE", "上游未返回有效 JSON。"))
}
pub(crate) fn summary(value: &Value, first: Option<u64>) -> crate::streaming::StreamSummary {
    crate::streaming::StreamSummary {
        response_model: crate::streaming::reported_model(&value["model"]),
        response_service_tier: value["service_tier"]
            .as_str()
            .filter(|tier| {
                !tier.is_empty()
                    && tier.len() <= 64
                    && tier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .map(str::to_owned),
        thread_id: String::new(),
        turn_id: String::new(),
        item_ids: vec![],
        outcome: "completed".into(),
        native_status: None,
        error_code: None,
        error_message: None,
        delta_count: 0,
        text_bytes: 0,
        first_delta_at: first,
        usage: crate::streaming::Usage::from_response(&value["usage"])
            .ok()
            .flatten(),
        interrupt_acknowledged: false,
        cleanup_confirmed: true,
        guard: Default::default(),
    }
}
