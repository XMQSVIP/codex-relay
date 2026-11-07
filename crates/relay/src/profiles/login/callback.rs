//! Only forward to the loopback listener recorded from the native authorization URL.
use crate::error::{Error, Result};
use std::{collections::HashMap, time::Duration};
use url::Url;

pub(super) struct Callback {
    target: Url,
    state: String,
}
fn invalid() -> Error {
    Error::new(
        "LOGIN_CALLBACK_INVALID",
        "回调地址无效或不属于本次登录，请粘贴授权后地址栏中的完整地址。",
    )
}
fn parameters(url: &Url) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    for (key, value) in url.query_pairs() {
        if result
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(result)
}
pub(super) fn auth_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| invalid())?;
    if value.len() > 16 * 1024
        || url.scheme() != "https"
        || ![Some("auth.openai.com"), Some("chatgpt.com")].contains(&url.host_str())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        return Err(invalid());
    }
    Ok(url)
}
impl Callback {
    pub fn from_auth(value: &str) -> Result<Self> {
        let mut url = auth_url(value)?;
        for _ in 0..2 {
            let params = parameters(&url)?;
            if let Some(inner) = params.get("authorize_url") {
                url = auth_url(inner)?;
                continue;
            }
            let target = Url::parse(params.get("redirect_uri").ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let state = params
                .get("state")
                .filter(|v| !v.is_empty() && v.len() <= 1024)
                .ok_or_else(invalid)?
                .clone();
            if target.scheme() != "http"
                || !matches!(target.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                || !matches!(target.port(), Some(1455 | 1457))
                || target.path() != "/auth/callback"
                || target.query().is_some()
                || target.fragment().is_some()
                || !target.username().is_empty()
                || target.password().is_some()
            {
                return Err(invalid());
            }
            return Ok(Self { target, state });
        }
        Err(invalid())
    }
    pub fn validate(&self, value: &str) -> Result<Url> {
        if value.len() > 16 * 1024 {
            return Err(invalid());
        }
        let url = Url::parse(value.trim()).map_err(|_| invalid())?;
        if url.origin() != self.target.origin()
            || url.path() != self.target.path()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        let params = parameters(&url)?;
        if params.contains_key("code") && params.contains_key("error") {
            return Err(invalid());
        }
        if params.get("state") != Some(&self.state) {
            return Err(invalid());
        }
        let code = params.get("code").filter(|v| !v.is_empty());
        let error = params.get("error").filter(|v| !v.is_empty());
        if code.is_some() == error.is_some() {
            return Err(invalid());
        }
        let mut target = self.target.clone();
        // Never resolve user supplied hosts, even after validation.
        if target.host_str() == Some("localhost") {
            target.set_host(Some("127.0.0.1")).map_err(|_| invalid())?;
        }
        let mut query = target.query_pairs_mut();
        for name in ["state", "code", "error"] {
            if let Some(value) = params.get(name) {
                query.append_pair(name, value);
            }
        }
        drop(query);
        Ok(target)
    }
}
pub(super) async fn forward(target: Url) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| unavailable())?;
    let response = client
        .get(target.clone())
        .send()
        .await
        .map_err(|_| unavailable())?;
    if response.status().is_redirection() {
        let next = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| target.join(v).ok())
            .ok_or_else(unavailable)?;
        if next.origin() != target.origin()
            || next.path() != "/success"
            || !next.username().is_empty()
            || next.password().is_some()
            || next.fragment().is_some()
        {
            return Err(unavailable());
        }
        let response = client.get(next).send().await.map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
    } else if !response.status().is_success() {
        return Err(unavailable());
    }
    // HTTP success is not login success: only the native notification can confirm it.
    Ok(())
}
fn unavailable() -> Error {
    Error::new(
        "LOGIN_CALLBACK_UNCONFIRMED",
        "回调转交未确认，仍在等待原生登录结果。请勿重复提交；若未完成，请取消后重新登录或使用设备码。",
    )
}
