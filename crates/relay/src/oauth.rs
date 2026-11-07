//! Native-managed OAuth files and native desktop credential reuse.
//! RT-only imports exchange once; ongoing refresh and model transport stay native.
pub(crate) mod cache;
mod import;
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation::Installation,
    provider::Provider,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub use import::prepare_import;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub const AUTH: &str = "native-chatgpt";
const LIMIT: usize = 256 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub account_id: String,
    pub subject: Option<String>,
    pub expires_at: Option<u64>,
}
#[derive(Clone)]
pub struct Credential {
    pub access_token: String,
    pub id_token: Option<String>,
    pub refresh_token: Option<String>,
    pub identity: Identity,
    last_refresh: Option<String>,
    oauth_provenance: Option<OAuthProvenance>,
    login_owner_id: Option<String>,
    plan_type: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthProvenance {
    issuer: String,
    client_id: String,
}
fn invalid() -> Error {
    Error::new(
        "OAUTH_CREDENTIAL_INVALID",
        "需要有效的 Codex OAuth Access Token，或包含 access_token、id_token、refresh_token 的登录文件。",
    )
}
fn claims(token: &str) -> Result<Value> {
    if token.len() > 32768 || token.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid());
    }
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).map_err(|_| invalid())?)
        .map_err(|_| invalid())
}
pub fn parse(document: &Value) -> Result<Credential> {
    if serde_json::to_vec(document)?.len() > LIMIT {
        return Err(invalid());
    }
    let data = if document["tokens"].is_object() {
        &document["tokens"]
    } else {
        document
    };
    let field = |snake: &str, camel: &str| -> Result<Option<String>> {
        let a = data.get(snake).filter(|v| !v.is_null());
        let b = data.get(camel).filter(|v| !v.is_null());
        if a.is_some() && b.is_some() && a != b {
            return Err(invalid());
        }
        a.or(b)
            .map(|v| {
                v.as_str()
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 32768 && !s.chars().any(char::is_whitespace)
                    })
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            })
            .transpose()
    };
    let access_token = field("access_token", "accessToken")?.ok_or_else(invalid)?;
    let access = claims(&access_token)?;
    let id_token = field("id_token", "idToken")?;
    let id = id_token
        .as_deref()
        .map(claims)
        .transpose()?
        .unwrap_or(Value::Null);
    let auth = "https://api.openai.com/auth";
    let account = access[auth]["chatgpt_account_id"]
        .as_str()
        .or_else(|| id[auth]["chatgpt_account_id"].as_str())
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .ok_or_else(invalid)?;
    for candidate in [
        data.get("account_id"),
        data.get("accountId"),
        data.get("chatgptAccountId"),
        id[auth].get("chatgpt_account_id"),
    ] {
        if candidate.is_some_and(|v| !v.is_null() && v.as_str() != Some(account)) {
            return Err(Error::new(
                "OAUTH_IDENTITY_MISMATCH",
                "凭据的工作区身份不一致。",
            ));
        }
    }
    for (a, b) in [
        (
            &access[auth]["chatgpt_user_id"],
            &id[auth]["chatgpt_user_id"],
        ),
        (&access["sub"], &id["sub"]),
    ] {
        if !a.is_null() && !b.is_null() && a != b {
            return Err(Error::new(
                "OAUTH_IDENTITY_MISMATCH",
                "Access Token 与 ID Token 的用户身份不一致。",
            ));
        }
    }
    let subject = access[auth]["chatgpt_user_id"]
        .as_str()
        .or_else(|| access["sub"].as_str())
        .or_else(|| id["sub"].as_str())
        .map(str::to_owned);
    let expires_at = access["exp"].as_u64();
    let refresh_token = field("refresh_token", "refreshToken")?;
    if refresh_token.is_some() && id_token.is_none() {
        return Err(Error::new(
            "OAUTH_IMPORT_INCOMPLETE",
            "可续期凭据需要 ID Token；请导入完整 auth.json，或选择单独 Refresh Token 导入以补全凭据。",
        ));
    }
    if refresh_token.is_none() && expires_at.is_some_and(|e| e <= crate::now_ms() / 1000) {
        return Err(Error::new(
            "OAUTH_TOKEN_EXPIRED",
            "Access Token 已过期，请重新登录或导入。",
        ));
    }
    // Preserve native refresh authority, but never accept an arbitrary token endpoint.
    let oauth_provenance = data
        .get("oauth_provenance")
        .filter(|v| !v.is_null())
        .map(|value| serde_json::from_value::<OAuthProvenance>(value.clone()))
        .transpose()
        .map_err(|_| invalid())?;
    if oauth_provenance.as_ref().is_some_and(|p| {
        p.issuer.trim_end_matches('/') != "https://auth.openai.com"
            || p.client_id.is_empty()
            || p.client_id.len() > 256
            || !p
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    }) {
        return Err(Error::new(
            "OAUTH_PROVIDER",
            "导入凭据的 OAuth 签发方或客户端格式不受支持。",
        ));
    }
    let login_owner_id = data
        .get("login_owner_id")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or_else(invalid)
        })
        .transpose()?;
    Ok(Credential {
        // Display metadata only; never use a token's plan claim for authorization.
        plan_type: [&access, &id].into_iter().find_map(|claims| {
            claims[auth]["chatgpt_plan_type"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
                .map(str::to_owned)
        }),
        access_token,
        id_token,
        refresh_token,
        identity: Identity {
            account_id: account.into(),
            subject,
            expires_at,
        },
        last_refresh: document["last_refresh"]
            .as_str()
            .filter(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
            .map(str::to_owned),
        oauth_provenance,
        login_owner_id,
    })
}
impl Credential {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }
    pub fn plan_type(&self) -> Option<&str> {
        self.plan_type.as_deref()
    }
    pub fn login_params(&self) -> Value {
        json!({"type":"chatgptAuthTokens","accessToken":self.access_token,"chatgptAccountId":self.identity.account_id})
    }
    pub fn native_document(&self) -> Value {
        let mut document = json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,"last_refresh":self.last_refresh.clone().unwrap_or_else(||chrono::Utc::now().to_rfc3339()),"tokens":{"access_token":self.access_token,"id_token":self.id_token,"refresh_token":self.refresh_token,"account_id":self.identity.account_id}});
        if let Some(provenance) = &self.oauth_provenance {
            document["tokens"]["oauth_provenance"] = json!(provenance);
        }
        if let Some(owner) = &self.login_owner_id {
            document["tokens"]["login_owner_id"] = json!(owner);
        }
        document
    }
}
pub fn fingerprint(identity: &Identity) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!(
        "{}\0{}",
        identity.account_id,
        identity.subject.as_deref().unwrap_or("")
    )))
}
pub fn check_identity(expected: &Identity, actual: &Identity) -> Result<()> {
    if expected.account_id != actual.account_id
        || expected
            .subject
            .as_ref()
            .is_some_and(|s| actual.subject.as_ref() != Some(s))
    {
        return Err(Error::new(
            "OAUTH_IDENTITY_CHANGED",
            "凭据已切换账户，请重新验证路由；不会自动切换到另一个桌面账户。",
        ));
    }
    Ok(())
}
pub async fn save(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temporary.write_all(&serde_json::to_vec(value)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|_| Error::new("OAUTH_STORAGE", "无法保存账户凭据。"))?;
    Ok(())
}
pub async fn load(path: &Path) -> Result<Value> {
    let meta = tokio::fs::metadata(path).await.map_err(|_| {
        Error::new(
            "OAUTH_NOT_SIGNED_IN",
            "未找到该实例的原生登录凭据，请登录或导入。",
        )
    })?;
    if meta.len() > LIMIT as u64 {
        return Err(invalid());
    }
    Ok(serde_json::from_slice(&tokio::fs::read(path).await?)?)
}
pub async fn desktop_credential(installation: &Installation, cwd: &Path) -> Result<Credential> {
    desktop_credential_with_proxy(installation, cwd, &Default::default()).await
}
pub async fn desktop_credential_with_proxy(
    installation: &Installation,
    cwd: &Path,
    proxy: &crate::app_server_proxy::ProxyConfig,
) -> Result<Credential> {
    let backend = Backend::start_auth_reader_with_proxy(installation, cwd, proxy).await?;
    let result = async {
        let value = backend
            .rpc
            .request(
                "getAuthStatus",
                json!({"includeToken":true,"refreshToken":false}),
            )
            .await?;
        if ![Some("chatgpt"), Some("chatgptAuthTokens")].contains(&value["authMethod"].as_str()) {
            return Err(Error::new(
                "DESKTOP_OAUTH_UNAVAILABLE",
                "未读取到本机 ChatGPT 登录，此入口仅支持 ChatGPT 账户。若本机 Codex 使用 API Key 或第三方提供方，请到“账户与连接 → 添加账户”选择“使用本机 Codex”，检测后连接并启用；无需重新登录。若要连接 ChatGPT 账户，请先在 Codex 登录，或使用独立登录/凭据导入。",
            ));
        }
        if value["authToken"].as_str().is_none_or(str::is_empty) {
            return Err(Error::new(
                "DESKTOP_OAUTH_TOKEN_UNAVAILABLE",
                "原生 getAuthStatus 确认已登录 ChatGPT，但 includeToken=true 未返回可复用令牌。此安装无法使用桌面绑定，请使用独立登录或凭据导入。",
            ));
        }
        parse(&json!({"accessToken":value["authToken"]}))
    }
    .await;
    let stopped = backend.stop().await;
    let result = result?;
    stopped?;
    Ok(result)
}
pub async fn native_credential(backend: &Backend) -> Result<Credential> {
    let status = backend
        .rpc
        .request(
            "getAuthStatus",
            json!({"includeToken":true,"refreshToken":false}),
        )
        .await?;
    if ![Some("chatgpt"), Some("chatgptAuthTokens")].contains(&status["authMethod"].as_str()) {
        return Err(Error::new(
            "OAUTH_NOT_SIGNED_IN",
            "该实例尚未完成原生 ChatGPT 登录。",
        ));
    }
    parse(&json!({"accessToken":status["authToken"]}))
}

pub(crate) async fn install_external_credential(
    rpc: &crate::rpc::Rpc,
    credential: &Credential,
) -> Result<Identity> {
    let result = rpc
        .request("account/login/start", credential.login_params())
        .await?;
    if result["type"] != "chatgptAuthTokens" {
        return Err(Error::new(
            "OAUTH_NATIVE_LOGIN",
            "原生后端未确认外部令牌登录。",
        ));
    }
    // The caller has checked the supplied token against the account binding.
    // Re-reading getAuthStatus here can trigger remote discovery and delay every
    // request; the successful login acknowledgement confirms this exact identity.
    Ok(credential.identity.clone())
}

pub async fn provider(
    backend: &Backend,
    cwd: &Path,
    model: Option<&str>,
    identity: &Identity,
) -> Result<Provider> {
    Ok(provider_with_config(backend, cwd, model, identity).await?.0)
}

pub(crate) async fn provider_with_config(
    backend: &Backend,
    cwd: &Path,
    model: Option<&str>,
    identity: &Identity,
) -> Result<(Provider, Value)> {
    let config = backend
        .rpc
        .request("config/read", json!({"cwd":cwd,"includeLayers":false}))
        .await?;
    if config["config"]["model_provider"]
        .as_str()
        .is_some_and(|p| p != "openai")
    {
        return Err(Error::new(
            "OAUTH_PROVIDER",
            "OAuth 实例只能使用官方 openai 提供方。",
        ));
    }
    let configured = &config["config"]["model_providers"]["openai"];
    if [
        "base_url",
        "env_key",
        "experimental_bearer_token",
        "http_headers",
        "env_http_headers",
    ]
    .iter()
    .any(|key| !configured[*key].is_null())
        || config["config"]["chatgpt_base_url"]
            .as_str()
            .is_some_and(|url| url.trim_end_matches('/') != "https://chatgpt.com/backend-api")
    {
        return Err(Error::new(
            "OAUTH_PROVIDER",
            "OAuth 实例不接受自定义上游地址或身份请求头；请使用独立的原生默认配置。",
        ));
    }
    let model = if let Some(model) = model {
        model.to_owned()
    } else if let Some(model) = config["config"]["model"].as_str() {
        model.to_owned()
    } else {
        let models = backend
            .rpc
            .request("model/list", json!({"limit":100}))
            .await?;
        let list = models["data"].as_array().ok_or_else(invalid)?;
        list.iter()
            .find(|m| m["isDefault"] == true)
            .or_else(|| list.first())
            .and_then(|m| m["model"].as_str())
            .ok_or_else(|| {
                Error::new(
                    "OAUTH_MODEL",
                    "无法读取原生默认模型，请检查原生配置或刷新模型目录。",
                )
            })?
            .to_owned()
    };
    if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
    {
        return Err(Error::new("INVALID_INPUT", "模型 ID 格式无效。"));
    }
    Ok((
        Provider {
            execution: Default::default(),
            id: "openai".into(),
            model,
            base_url: "https://chatgpt.com/backend-api".into(),
            auth: format!("{AUTH}:{}", fingerprint(identity)),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: config["config"]["mcp_servers"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
        },
        config["config"].clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn external_login_requires_native_ack_without_a_status_query() {
        let token = format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(
                json!({"exp":crate::now_ms()/1000+3600,"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic"}}).to_string()
            )
        );
        let credential = parse(&json!({"accessToken":token})).unwrap();
        for kind in ["chatgptAuthTokens", "apiKey"] {
            let (client, server) = tokio::io::duplex(65536);
            let (reader, writer) = tokio::io::split(client);
            let rpc = crate::rpc::Rpc::from_io(reader, writer);
            let (reader, mut writer) = tokio::io::split(server);
            let mut lines = BufReader::new(reader).lines();
            let native = async {
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], "account/login/start");
                assert_eq!(request["params"]["chatgptAccountId"], "synthetic");
                writer
                    .write_all(
                        format!("{}\n", json!({"id":request["id"],"result":{"type":kind}}))
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(50), lines.next_line())
                        .await
                        .is_err(),
                    "external token installation must not issue getAuthStatus"
                );
            };
            let (result, ()) = tokio::join!(install_external_credential(&rpc, &credential), native);
            if kind == "chatgptAuthTokens" {
                assert_eq!(result.unwrap().account_id, "synthetic");
            } else {
                assert_eq!(result.err().unwrap().code, "OAUTH_NATIVE_LOGIN");
            }
            rpc.close();
        }
    }
}
