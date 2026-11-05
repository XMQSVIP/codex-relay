use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::Path;
mod native;
pub use native::from_config;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Execution {
    #[default]
    Native,
    ApiKey,
}
impl Execution {
    fn is_native(&self) -> bool {
        *self == Self::Native
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    #[serde(default, skip_serializing_if = "Execution::is_native")]
    pub execution: Execution,
    pub id: String,
    pub model: String,
    pub base_url: String,
    pub auth: String,
    pub forward_headers: Vec<String>,
    pub required_headers: Vec<String>,
    pub mcp_servers: Vec<String>,
}

impl Provider {
    pub(crate) fn is_chatgpt_account(&self) -> bool {
        self.execution == Execution::Native
            && (self.auth == "chatgpt"
                || self
                    .auth
                    .strip_prefix(crate::oauth::AUTH)
                    .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':')))
    }
}

pub(crate) async fn read_with_credential(
    rpc: &Rpc,
    cwd: &Path,
) -> Result<(Provider, Value, Option<crate::oauth::Credential>)> {
    native::read_with_credential(rpc, cwd).await
}

pub fn lan_url(value: &str) -> Result<url::Url> {
    let url =
        url::Url::parse(value).map_err(|_| Error::new("PROVIDER_ENDPOINT", "提供方地址无效。"))?;
    let private = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_private(),
        _ => false,
    };
    if !private
        || !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !["/v1", "/v1/"].contains(&url.path())
    {
        return Err(Error::new(
            "PROVIDER_ENDPOINT",
            "LAN 提供方地址须为私有 IPv4 的 HTTP/HTTPS 地址，路径为 /v1。",
        ));
    }
    let mut url = url;
    url.set_path("/v1");
    Ok(url)
}
pub fn config_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn header_name(value: &str) -> Result<String> {
    let name = value.to_lowercase();
    if name.parse::<axum::http::HeaderName>().is_err()
        || [
            "host",
            "connection",
            "keep-alive",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "origin",
            "referer",
            "cookie",
            "set-cookie",
            "chatgpt-account-id",
            "forwarded",
        ]
        .contains(&name.as_str())
        || ["proxy-", "content-", "accept", "x-forwarded-", "sec-"]
            .iter()
            .any(|s| name.starts_with(s))
    {
        return Err(Error::new(
            "PROVIDER_HEADERS",
            "提供方包含不支持的传输或浏览器认证头。",
        ));
    }
    Ok(name)
}
pub async fn read(rpc: &Rpc, cwd: &Path) -> Result<Provider> {
    native::read(rpc, cwd).await
}

pub fn text_policy(servers: &[String]) -> Result<Map<String, Value>> {
    let mut config = Map::new();
    config.insert("web_search".into(), json!("disabled"));
    config.insert("project_doc_max_bytes".into(), json!(0));
    config.insert("features.skip_host_skill_discovery".into(), json!(true));
    config.insert(
        "tools.experimental_request_user_input.enabled".into(),
        json!(false),
    );
    config.insert("tools.update_plan.enabled".into(), json!(false));
    config.insert(
        "features.multi_agent_v2".into(),
        json!({"enabled":false,"disable_native_tools":true}),
    );
    config.insert(
        "features.code_mode".into(),
        json!({"enabled":false,"excluded_tool_namespaces":["functions","clock"]}),
    );
    for feature in [
        "apps",
        "plugins",
        "hooks",
        "shell_tool",
        "multi_agent",
        "browser_use",
        "computer_use",
        "image_generation",
        "goals",
        "memories",
        "skill_search",
        "workspace_dependencies",
        "view_image",
        "sleep_tool",
        "tool_suggest",
        "browser_use_external",
        "in_app_browser",
        "code_mode_host",
        "code_mode_only",
        "current_time_reminder",
        "deferred_executor",
        "token_budget",
        "send_message_to_user_async",
        "request_permissions_tool",
        "unbounded_connection_retries",
        "enable_request_compression",
    ] {
        config.insert(format!("features.{feature}"), json!(false));
    }
    for server in servers {
        if !config_key(server) {
            return Err(Error::new(
                "MCP_SERVER_NAME",
                "MCP 配置名称不支持，拒绝生成不完整的禁用策略。",
            ));
        }
        config.insert(format!("mcp_servers.{server}.enabled"), json!(false));
    }
    Ok(config)
}
pub fn overrides(provider: &Provider, route: &str) -> Result<Map<String, Value>> {
    if !config_key(&provider.id) {
        return Err(Error::new("PROVIDER_CONFIG", "提供方名称无效。"));
    }
    let mut config = text_policy(&provider.mcp_servers)?;
    for (key, value) in [
        ("base_url", json!(route)),
        ("supports_websockets", json!(false)),
        ("request_max_retries", json!(0)),
        ("stream_max_retries", json!(0)),
    ] {
        config.insert(format!("model_providers.{}.{key}", provider.id), value);
    }
    Ok(config)
}
