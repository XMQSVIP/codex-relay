//! One-time RT import exchange. Never used for ongoing account refresh.
use super::{Credential, LIMIT, claims, invalid, parse};
use crate::{
    app_server_proxy::{ProxyConfig, ProxyMode},
    error::{Error, Result},
};
use serde_json::{Value, json};
use std::time::Duration;

const ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const RESPONSE_LIMIT: usize = 128 * 1024;

fn field(data: &Value, snake: &str, camel: &str) -> Result<Option<String>> {
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
}

fn exchange_error() -> Error {
    Error::new(
        "OAUTH_IMPORT_EXCHANGE",
        "Refresh Token 交换未完成，请检查网络与代理设置；请求可能已被处理，请勿连续重复提交，必要时重新登录获取凭据。",
    )
}
fn incomplete() -> Error {
    Error::new(
        "OAUTH_IMPORT_INCOMPLETE",
        "令牌交换未返回原生后端所需的有效 Access Token 和 ID Token，无法接管续期；请导入完整 auth.json 或重新登录。",
    )
}

pub async fn prepare_import(document: &Value, proxy: &ProxyConfig) -> Result<Credential> {
    prepare_with_endpoint(document, proxy, ENDPOINT).await
}

// The endpoint is private and only replaced by local mock servers in unit tests.
async fn prepare_with_endpoint(
    document: &Value,
    proxy: &ProxyConfig,
    endpoint: &str,
) -> Result<Credential> {
    if serde_json::to_vec(document)?.len() > LIMIT {
        return Err(invalid());
    }
    let data = if document["tokens"].is_object() {
        &document["tokens"]
    } else {
        document
    };
    if field(data, "access_token", "accessToken")?.is_some() {
        return parse(document);
    }
    let rt = field(data, "refresh_token", "refreshToken")?.ok_or_else(invalid)?;
    let original_id = field(data, "id_token", "idToken")?;
    if let Some(id) = &original_id {
        claims(id)?;
    }
    // Validate metadata before consuming a potentially rotating RT.
    let provenance = data.get("oauth_provenance").filter(|v| !v.is_null());
    if provenance.is_some_and(|v| {
        v["issuer"].as_str().map(|s| s.trim_end_matches('/')) != Some("https://auth.openai.com")
            || v["client_id"].as_str() != Some(CLIENT_ID)
            || v.as_object().is_none_or(|o| o.len() != 2)
    }) {
        return Err(Error::new(
            "OAUTH_PROVIDER",
            "单独 RT 导入仅支持官方 Codex OAuth 客户端；其他客户端请导入完整凭据。",
        ));
    }
    for key in [
        "account_id",
        "accountId",
        "chatgptAccountId",
        "login_owner_id",
    ] {
        if data.get(key).is_some_and(|v| {
            !v.is_null()
                && v.as_str().is_none_or(|s| {
                    s.is_empty() || s.len() > 256 || s.chars().any(char::is_control)
                })
        }) {
            return Err(invalid());
        }
    }
    let mut account = None;
    for key in ["account_id", "accountId", "chatgptAccountId"] {
        if let Some(value) = data[key].as_str() {
            if account.is_some_and(|previous| previous != value) {
                return Err(Error::new(
                    "OAUTH_IDENTITY_MISMATCH",
                    "凭据的工作区身份不一致。",
                ));
            }
            account = Some(value);
        }
    }
    let proxy = proxy.clone().validated()?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30));
    match proxy.mode {
        ProxyMode::Direct => builder = builder.no_proxy(),
        ProxyMode::Custom => {
            builder = builder
                .no_proxy()
                .proxy(reqwest::Proxy::all(&proxy.url).map_err(|_| exchange_error())?)
        }
        ProxyMode::Auto => {
            #[cfg(target_os = "macos")]
            {
                let mut command = tokio::process::Command::new("unused");
                crate::platform::macos::apply_system_proxy(&mut command).await?;
                for (key, value) in command.as_std().get_envs() {
                    if key == "HTTPS_PROXY" {
                        if let Some(value) = value {
                            builder = builder.proxy(
                                reqwest::Proxy::https(value.to_string_lossy().as_ref())
                                    .map_err(|_| exchange_error())?,
                            );
                        }
                    }
                }
            }
        }
    }
    let client = builder.build().map_err(|_| exchange_error())?;
    // Do not retry: a lost response may already have rotated the supplied RT.
    let mut response = client
        .post(endpoint)
        .json(&json!({
            "client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": rt
        }))
        .send()
        .await
        .map_err(|_| exchange_error())?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            400 | 401 | 403 => Error::new(
                "OAUTH_IMPORT_REJECTED",
                "Refresh Token 被拒绝，可能已过期、失效或不属于 Codex 客户端；请重新登录获取凭据。",
            ),
            429 => Error::new(
                "OAUTH_IMPORT_RATE_LIMITED",
                "令牌服务暂时限流，请稍后再试。",
            ),
            _ => exchange_error(),
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > RESPONSE_LIMIT as u64)
    {
        return Err(incomplete());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| exchange_error())? {
        if body.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(incomplete());
        }
        body.extend_from_slice(&chunk);
    }
    // Never return response bodies, tokens, URLs from transport errors, or parser details.
    let tokens: Value = serde_json::from_slice(&body).map_err(|_| incomplete())?;
    let at = field(&tokens, "access_token", "accessToken")
        .map_err(|_| incomplete())?
        .ok_or_else(incomplete)?;
    let id = field(&tokens, "id_token", "idToken")
        .map_err(|_| incomplete())?
        .ok_or_else(incomplete)?;
    let rt = field(&tokens, "refresh_token", "refreshToken")
        .map_err(|_| incomplete())?
        .unwrap_or(rt);
    let mut completed = data.clone();
    let object = completed.as_object_mut().ok_or_else(invalid)?;
    for key in ["accessToken", "refreshToken", "idToken"] {
        object.remove(key);
    }
    object.insert("access_token".into(), json!(at));
    object.insert("id_token".into(), json!(id));
    object.insert("refresh_token".into(), json!(rt));
    object.insert(
        "oauth_provenance".into(),
        json!({"issuer":"https://auth.openai.com", "client_id":CLIENT_ID}),
    );
    let credential = parse(&completed)?;
    if credential
        .identity
        .expires_at
        .is_none_or(|exp| exp <= crate::now_ms() / 1000)
    {
        return Err(incomplete());
    }
    if let Some(id) = original_id {
        completed["id_token"] = json!(id);
        let expected = parse(&completed)?;
        super::check_identity(&expected.identity, &credential.identity)?;
    }
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        http::{Response, StatusCode},
        routing::post,
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::{Arc, Mutex};

    fn token(account: &str, expiry: u64) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(
                json!({"sub":format!("user-{account}"), "exp":expiry,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}})
                .to_string()
            )
        )
    }
    fn response_tokens() -> Value {
        let token = token("account-a", crate::now_ms() / 1000 + 3600);
        json!({"access_token":token,"id_token":token,"refresh_token":"rotated-rt"})
    }
    fn direct() -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Direct,
            url: String::new(),
        }
    }
    struct Mock {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn mock(status: StatusCode, body: String) -> Mock {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let app = Router::new().route(
            "/token",
            post(move |Json(input): Json<Value>| {
                let body = body.clone();
                captured.lock().unwrap().push(input);
                async move {
                    Response::builder()
                        .status(status)
                        .header("location", "/token")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Mock {
            url,
            requests,
            task,
        }
    }

    #[tokio::test]
    async fn rt_only_exchange_rotates_and_persists_native_credentials() {
        for nested in [false, true] {
            let server = mock(StatusCode::OK, response_tokens().to_string()).await;
            let data = json!({"refreshToken":"input-rt", "accountId":"account-a"});
            let input = if nested { json!({"tokens":data}) } else { data };
            let credential = prepare_with_endpoint(&input, &direct(), &server.url)
                .await
                .unwrap();
            assert_eq!(
                *server.requests.lock().unwrap(),
                vec![
                    json!({"client_id":CLIENT_ID,"grant_type":"refresh_token","refresh_token":"input-rt"})
                ]
            );
            assert_eq!(credential.refresh_token.as_deref(), Some("rotated-rt"));
            assert_eq!(credential.identity.account_id, "account-a");
            let temp = tempfile::tempdir().unwrap();
            let profiles = crate::profiles::Profiles::open(temp.path().join("accounts"))
                .await
                .unwrap();
            let account = profiles.create("rt-import").await.unwrap();
            let result = profiles
                .import_credentials(account.id, credential.native_document())
                .await
                .unwrap();
            assert_eq!(result["refresh"], "native-managed");
            assert_eq!(result["inferenceVerified"], false);
            let home = temp
                .path()
                .join("accounts")
                .join(account.id.to_string())
                .join("home");
            let stored = super::super::load(&home.join("auth.json")).await.unwrap();
            assert_eq!(stored["tokens"]["refresh_token"], "rotated-rt");
            assert_eq!(stored["tokens"]["oauth_provenance"]["client_id"], CLIENT_ID);
            assert!(!home.join("bridge-token.json").exists());
            assert!(!result.to_string().contains("rotated-rt"));
        }
    }

    #[tokio::test]
    async fn omitted_rotation_preserves_rt_and_existing_imports_never_exchange() {
        let mut tokens = response_tokens();
        tokens.as_object_mut().unwrap().remove("refresh_token");
        let server = mock(StatusCode::OK, tokens.to_string()).await;
        let c = prepare_with_endpoint(&json!({"refresh_token":"input-rt"}), &direct(), &server.url)
            .await
            .unwrap();
        assert_eq!(c.refresh_token.as_deref(), Some("input-rt"));
        prepare_with_endpoint(&c.native_document(), &direct(), &server.url)
            .await
            .unwrap();
        prepare_with_endpoint(
            &json!({"accessToken":c.access_token}),
            &direct(),
            &server.url,
        )
        .await
        .unwrap();
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invalid_input_is_rejected_before_consuming_rt() {
        let server = mock(StatusCode::OK, response_tokens().to_string()).await;
        for input in [
            json!({"refreshToken":"one", "refresh_token":"two"}),
            json!({"refreshToken":"has whitespace"}),
            json!({"refreshToken":"x", "idToken":"malformed"}),
            json!({"refreshToken":"x", "accountId":42}),
            json!({"refreshToken":"x", "accountId":"a", "account_id":"b"}),
            json!({"refreshToken":"x", "oauth_provenance":{"issuer":"http://evil.invalid","client_id":CLIENT_ID}}),
            json!({"refreshToken":"x", "oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"other-client"}}),
            json!({"refreshToken":"x".repeat(LIMIT)}),
        ] {
            assert!(
                prepare_with_endpoint(&input, &direct(), &server.url)
                    .await
                    .is_err()
            );
        }
        assert!(server.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn incomplete_expired_and_mismatched_results_are_rejected() {
        let mut missing_id = response_tokens();
        missing_id.as_object_mut().unwrap().remove("id_token");
        let mut wrong_id = response_tokens();
        wrong_id["id_token"] = json!(token("account-b", crate::now_ms() / 1000 + 3600));
        let mut expired = response_tokens();
        expired["access_token"] = json!(token("account-a", 1));
        for (input, body, code) in [
            (
                json!({"refreshToken":"input-rt"}),
                missing_id,
                "OAUTH_IMPORT_INCOMPLETE",
            ),
            (
                json!({"refreshToken":"input-rt"}),
                expired,
                "OAUTH_IMPORT_INCOMPLETE",
            ),
            (
                json!({"refreshToken":"input-rt"}),
                wrong_id,
                "OAUTH_IDENTITY_MISMATCH",
            ),
            (
                json!({"refreshToken":"input-rt","accountId":"account-b"}),
                response_tokens(),
                "OAUTH_IDENTITY_MISMATCH",
            ),
            (
                json!({"refreshToken":"input-rt","idToken":token("account-b",crate::now_ms()/1000+3600)}),
                response_tokens(),
                "OAUTH_IDENTITY_MISMATCH",
            ),
        ] {
            let server = mock(StatusCode::OK, body.to_string()).await;
            let error = prepare_with_endpoint(&input, &direct(), &server.url)
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, code);
        }
    }

    #[tokio::test]
    async fn errors_are_redacted_bounded_and_never_retried_or_redirected() {
        for (status, body, code) in [
            (
                StatusCode::BAD_REQUEST,
                "input-rt SECRET_RESPONSE".into(),
                "OAUTH_IMPORT_REJECTED",
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "SECRET_RESPONSE".into(),
                "OAUTH_IMPORT_RATE_LIMITED",
            ),
            (
                StatusCode::TEMPORARY_REDIRECT,
                "SECRET_RESPONSE".into(),
                "OAUTH_IMPORT_EXCHANGE",
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "SECRET_RESPONSE".into(),
                "OAUTH_IMPORT_EXCHANGE",
            ),
            (
                StatusCode::OK,
                "SECRET_RESPONSE".into(),
                "OAUTH_IMPORT_INCOMPLETE",
            ),
            (
                StatusCode::OK,
                "x".repeat(RESPONSE_LIMIT + 1),
                "OAUTH_IMPORT_INCOMPLETE",
            ),
        ] {
            let server = mock(status, body).await;
            let error =
                prepare_with_endpoint(&json!({"refreshToken":"input-rt"}), &direct(), &server.url)
                    .await
                    .err()
                    .unwrap();
            assert_eq!(error.code, code);
            assert!(!error.to_string().contains("input-rt"));
            assert!(!error.to_string().contains("SECRET_RESPONSE"));
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn exchange_uses_configured_proxy() {
        let server = mock(StatusCode::OK, response_tokens().to_string()).await;
        let proxy = ProxyConfig {
            mode: ProxyMode::Custom,
            url: server.url.trim_end_matches("/token").into(),
        };
        let credential = prepare_with_endpoint(
            &json!({"refreshToken":"input-rt"}),
            &proxy,
            "http://oauth.invalid/token",
        )
        .await
        .unwrap();
        assert_eq!(credential.identity.account_id, "account-a");
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    #[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; synthetic RT exchange and isolated native home only"]
    async fn native_backend_reads_exchanged_credentials() {
        let server = mock(StatusCode::OK, response_tokens().to_string()).await;
        let credential =
            prepare_with_endpoint(&json!({"refreshToken":"input-rt"}), &direct(), &server.url)
                .await
                .unwrap();
        let installation: crate::installation::Installation = serde_json::from_slice(
            &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model_provider=\"openai\"\ncli_auth_credentials_store=\"file\"\nchatgpt_base_url=\"http://127.0.0.1:9\"\n").unwrap();
        super::super::save(&home.join("auth.json"), &credential.native_document())
            .await
            .unwrap();
        let backend =
            crate::backend::Backend::start_direct(&installation, temp.path(), Some(&home))
                .await
                .unwrap();
        let result = super::super::native_credential(&backend).await;
        backend.stop().await.unwrap();
        assert_eq!(result.unwrap().identity.account_id, "account-a");
    }
}
