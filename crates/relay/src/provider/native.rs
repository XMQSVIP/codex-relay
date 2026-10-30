//! Metadata only. Native app-server owns credentials, refresh and model transport.
use super::*;

pub fn from_config(value: &Value, status: &Value) -> Result<Provider> {
    let config = &value["config"];
    let invalid = || Error::new("PROVIDER_CONFIG", "无法识别本机 Codex 的有效提供方配置。");
    let id = config["model_provider"].as_str().unwrap_or("openai");
    if !config_key(id) {
        return Err(invalid());
    }
    let model = config["model"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| {
            Error::new(
                "PROVIDER_MODEL",
                "无法读取本机默认模型，请检查 Codex 配置。",
            )
        })?;
    let provider = &config["model_providers"][id];
    if provider["wire_api"]
        .as_str()
        .is_some_and(|v| v != "responses")
    {
        return Err(Error::new(
            "PROVIDER_PROTOCOL",
            "当前提供方未使用原生 Responses 协议。",
        ));
    }
    let openai_auth = id == "openai" || provider["requires_openai_auth"] == true;
    let method = status["authMethod"].as_str();
    let chatgpt = openai_auth && matches!(method, Some("chatgpt" | "chatgptAuthTokens"));
    if openai_auth && !chatgpt && !matches!(method, Some("apikey" | "apiKey")) {
        return Err(Error::new(
            "PROVIDER_AUTH_MISSING",
            "本机 Codex 尚未登录或配置 API Key。请先在本机完成认证。",
        ));
    }
    let endpoint = provider["base_url"]
        .as_str()
        .or_else(|| match id {
            "openai" => config["openai_base_url"].as_str().or_else(|| {
                if chatgpt {
                    config["chatgpt_base_url"].as_str()
                } else {
                    None
                }
            }),
            _ => None,
        })
        .unwrap_or(match id {
            "openai" if chatgpt => "https://chatgpt.com/backend-api",
            "openai" => "https://api.openai.com/v1",
            "ollama" => "http://localhost:11434/v1",
            "lmstudio" => "http://localhost:1234/v1",
            _ => "",
        });
    let mut base = url::Url::parse(endpoint).map_err(|_| invalid())?;
    if !["http", "https"].contains(&base.scheme())
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.fragment().is_some()
    {
        return Err(Error::new(
            "PROVIDER_ENDPOINT",
            "本机提供方需要有效的 HTTP/HTTPS 地址。",
        ));
    }
    // Query values can be credentials. Keep them exclusively in native config.
    base.set_query(None);
    let mut headers = Vec::new();
    for key in ["http_headers", "env_http_headers"] {
        if let Some(map) = provider[key].as_object() {
            for name in map.keys() {
                if name.parse::<axum::http::HeaderName>().is_err() {
                    return Err(invalid());
                }
                headers.push(name.to_lowercase());
            }
        }
    }
    let auth = if chatgpt {
        let credential = crate::oauth::parse(&json!({"accessToken":status["authToken"]}))?;
        format!(
            "{}:{}:local-config",
            crate::oauth::AUTH,
            crate::oauth::fingerprint(&credential.identity)
        )
    } else if openai_auth {
        "native-api-key".into()
    } else if !provider["auth"].is_null()
        || !provider["gateway_oauth"].is_null()
        || !provider["aws"].is_null()
    {
        "native-provider-auth".into()
    } else if provider["env_key"].as_str().is_some_and(|v| !v.is_empty())
        || provider["experimental_bearer_token"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    {
        "native-bearer".into()
    } else if headers.is_empty() {
        "none".into()
    } else {
        "native-headers".into()
    };
    if openai_auth || auth == "native-bearer" {
        headers.push("authorization".into());
    }
    headers.sort();
    headers.dedup();
    Ok(Provider {
        execution: Default::default(),
        id: id.into(),
        model: model.into(),
        base_url: base.as_str().trim_end_matches('/').into(),
        auth,
        required_headers: headers.clone(),
        forward_headers: headers,
        mcp_servers: config["mcp_servers"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
    })
}

pub(super) async fn read(rpc: &Rpc, cwd: &Path) -> Result<Provider> {
    Ok(read_with_config(rpc, cwd).await?.0)
}

pub(super) async fn read_with_config(rpc: &Rpc, cwd: &Path) -> Result<(Provider, Value)> {
    let (provider, config, _) = read_with_credential(rpc, cwd).await?;
    Ok((provider, config))
}

pub(super) async fn read_with_credential(
    rpc: &Rpc,
    cwd: &Path,
) -> Result<(Provider, Value, Option<crate::oauth::Credential>)> {
    let mut config = rpc
        .request("config/read", json!({"cwd":cwd,"includeLayers":false}))
        .await?;
    let snapshot = config["config"].clone();
    let id = config["config"]["model_provider"]
        .as_str()
        .unwrap_or("openai");
    let openai_auth =
        id == "openai" || config["config"]["model_providers"][id]["requires_openai_auth"] == true;
    // Do not ask for OpenAI credentials when a custom provider owns its auth.
    let status = if openai_auth {
        rpc.request(
            "getAuthStatus",
            json!({"includeToken":true,"refreshToken":false}),
        )
        .await?
    } else {
        Value::Null
    };
    if id == "openai" && config["config"]["openai_base_url"].is_null() {
        if let Ok(base) = std::env::var("OPENAI_BASE_URL") {
            if !base.is_empty() {
                config["config"]["openai_base_url"] = base.into();
            }
        }
    }
    if config["config"]["model"].as_str().is_none_or(str::is_empty) {
        let models = rpc.request("model/list", json!({"limit":100})).await?;
        let list = models["data"]
            .as_array()
            .ok_or_else(|| Error::new("PROVIDER_MODEL", "原生模型目录不可用。"))?;
        config["config"]["model"] = list
            .iter()
            .find(|m| m["isDefault"] == true)
            .or_else(|| list.first())
            .and_then(|m| m.get("model"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    let provider = from_config(&config, &status)?;
    let credential = if provider.is_chatgpt_account() {
        Some(crate::oauth::parse(
            &json!({"accessToken":status["authToken"]}),
        )?)
    } else {
        None
    };
    Ok((provider, snapshot, credential))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn route_and_fingerprint_share_one_unmodified_config_snapshot() {
        let (client, server) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(client);
        let rpc = Rpc::from_io(reader, writer);
        let (reader, mut writer) = tokio::io::split(server);
        let config = json!({"model_provider":"fixture","model_providers":{"fixture":{"base_url":"http://127.0.0.1:9/v1","wire_api":"responses"}}});
        let expected = config.clone();
        let server = async move {
            let mut lines = BufReader::new(reader).lines();
            for (method, result) in [
                ("config/read", json!({"config":config})),
                (
                    "model/list",
                    json!({"data":[{"isDefault":true,"model":"default-model"}]}),
                ),
            ] {
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], method);
                let response = json!({"id":request["id"],"result":result});
                writer
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
                writer.flush().await.unwrap();
            }
        };
        let (result, _) = tokio::join!(read_with_config(&rpc, Path::new(".")), server);
        let (provider, snapshot) = result.unwrap();
        assert_eq!(provider.model, "default-model");
        assert_eq!(
            snapshot, expected,
            "derived defaults must not change the config fingerprint"
        );
    }
}
