use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[cfg(windows)]
mod windows;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    #[serde(default)]
    pub url: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyMode {
    #[default]
    Auto,
    Direct,
    Custom,
}

impl ProxyConfig {
    pub(crate) async fn automatic_view() -> serde_json::Value {
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Self::default().resolved(),
        )
        .await
        {
            Ok(Ok(proxy)) => proxy.public_view(),
            _ => serde_json::json!({"state":"error","http":null,"https":null}),
        }
    }
    /// Resolve the exact environment the native child would receive, including
    /// Windows/macOS system proxy fallback. The detector must not use reqwest defaults.
    pub(crate) async fn resolved(&self) -> Result<ResolvedProxy> {
        let mut command = Command::new("unused-location-probe");
        self.apply(&mut command).await?;
        let mut environment: std::collections::BTreeMap<String, String> = std::env::vars()
            .filter(|(key, _)| is_proxy_key(key))
            .map(|(key, value)| (proxy_key(&key), value))
            .collect();
        for (key, value) in command.as_std().get_envs() {
            let key = proxy_key(&key.to_string_lossy());
            if is_proxy_key(&key) {
                if let Some(value) = value {
                    environment.insert(key, value.to_string_lossy().into_owned());
                } else {
                    environment.remove(&key);
                }
            }
        }
        let get = |name: &str| {
            environment
                .get(name)
                .or_else(|| environment.get(&name.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default()
        };
        Ok(ResolvedProxy {
            http: get("HTTP_PROXY"),
            https: get("HTTPS_PROXY"),
            all: get("ALL_PROXY"),
            bypass: get("NO_PROXY"),
        })
    }
    pub fn validated(mut self) -> Result<Self> {
        if self.mode != ProxyMode::Custom {
            self.url.clear();
            return Ok(self);
        }
        let invalid = || {
            Error::new(
                "INVALID_INPUT",
                "请输入有效的 HTTP/HTTPS 代理地址，不包含账号密码、路径或查询参数。",
            )
        };
        let raw = self.url.trim();
        if raw.len() > 2048 || raw.chars().any(char::is_control) {
            return Err(invalid());
        }
        let url = url::Url::parse(raw).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port() == Some(0)
        {
            return Err(invalid());
        }
        self.url = url.as_str().trim_end_matches('/').to_owned();
        Ok(self)
    }

    pub async fn apply(&self, command: &mut Command) -> Result<()> {
        if self.mode == ProxyMode::Auto {
            #[cfg(windows)]
            windows::apply_system_proxy(command).await?;
            #[cfg(target_os = "macos")]
            crate::platform::macos::apply_system_proxy(command).await?;
            return Ok(());
        }
        // Override only the child environment, including mixed-case inherited aliases.
        let mut names: Vec<_> = std::env::vars_os().map(|(key, _)| key).collect();
        names.extend(command.as_std().get_envs().map(|(key, _)| key.to_owned()));
        for name in names {
            if matches!(
                name.to_string_lossy().to_ascii_uppercase().as_str(),
                "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
            ) {
                command.env_remove(name);
            }
        }
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            command.env_remove(name);
        }
        if self.mode == ProxyMode::Custom {
            command
                .env("HTTP_PROXY", &self.url)
                .env("HTTPS_PROXY", &self.url);
            command.env("NO_PROXY", "127.0.0.1,localhost,::1");
        } else {
            command.env("NO_PROXY", "*");
        }
        Ok(())
    }
}

fn is_proxy_key(key: &str) -> bool {
    matches!(
        key.to_ascii_uppercase().as_str(),
        "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
    )
}

fn proxy_key(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.into()
    }
}

#[derive(Clone, Default, serde::Serialize)]
pub(crate) struct ResolvedProxy {
    http: String,
    https: String,
    all: String,
    bypass: String,
}
impl ResolvedProxy {
    /// Report only connection endpoints; inherited proxy URLs may contain credentials.
    fn public_view(&self) -> serde_json::Value {
        if self.bypass.split(',').any(|value| value.trim() == "*") {
            return serde_json::json!({"state":"direct","http":null,"https":null});
        }
        let endpoint = |specific: &str| -> Result<Option<String>> {
            let raw = if specific.is_empty() {
                &self.all
            } else {
                specific
            };
            if raw.is_empty() {
                return Ok(None);
            }
            proxy_address(raw)
                .map(Some)
                .ok_or_else(|| Error::new("PROXY_ADDRESS", "代理地址无效。"))
        };
        match (endpoint(&self.http), endpoint(&self.https)) {
            (Ok(http), Ok(https)) => serde_json::json!({
                "state":if http.is_some() || https.is_some() { "proxy" } else { "direct" },
                "http":http,"https":https,
            }),
            _ => serde_json::json!({"state":"error","http":null,"https":null}),
        }
    }
    pub(crate) fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            serde_json::to_vec(self).expect("proxy serialization"),
        ))
    }
    pub(crate) fn client(&self) -> Result<reqwest::Client> {
        self.client_builder()?
            .build()
            .map_err(|_| Error::new("LOCATION_PROXY", "无法创建出口位置检测连接。"))
    }
    pub(crate) fn client_builder(&self) -> Result<reqwest::ClientBuilder> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none());
        if self.bypass != "*" {
            for (url, kind) in [(&self.http, 0), (&self.https, 1), (&self.all, 2)] {
                if url.is_empty() {
                    continue;
                }
                let proxy = match kind {
                    0 => reqwest::Proxy::http(url),
                    1 => reqwest::Proxy::https(url),
                    _ => reqwest::Proxy::all(url),
                }
                .map_err(|_| {
                    crate::error::Error::new("LOCATION_PROXY", "无法使用此代理检测出口位置。")
                })?
                .no_proxy(reqwest::NoProxy::from_string(&self.bypass));
                builder = builder.proxy(proxy);
            }
        }
        Ok(builder)
    }
}

fn proxy_address(raw: &str) -> Option<String> {
    if raw.len() > 2048 || raw.chars().any(char::is_control) {
        return None;
    }
    let raw = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let url = url::Url::parse(&raw).ok()?;
    let default_port = match url.scheme() {
        "http" => 80,
        "https" => 443,
        "socks4" | "socks4a" | "socks5" | "socks5h" => 1080,
        _ => return None,
    };
    let port = url.port().unwrap_or(default_port);
    if port == 0 {
        return None;
    }
    let host = match url.host()? {
        url::Host::Ipv6(ip) => format!("[{ip}]"),
        host => host.to_string(),
    };
    Some(format!("{}://{host}:{port}", url.scheme()))
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn automatic_proxy_display_uses_protocol_precedence_and_hides_secrets() {
        let proxy = ResolvedProxy {
            http: "http://alice:secret@127.0.0.1:7890/private?token=hidden#fragment".into(),
            https: String::new(),
            all: "socks5h://[::1]:1081".into(),
            bypass: "localhost,127.0.0.1".into(),
        };
        assert_eq!(
            proxy.public_view(),
            json!({"state":"proxy","http":"http://127.0.0.1:7890","https":"socks5h://[::1]:1081"})
        );
        assert_eq!(
            proxy_address("proxy.example:8080").unwrap(),
            "http://proxy.example:8080"
        );
        assert_eq!(
            proxy_address("https://user:pass@proxy.example").unwrap(),
            "https://proxy.example:443"
        );
        assert_eq!(
            proxy_address("http://proxy.example").unwrap(),
            "http://proxy.example:80"
        );
    }

    #[test]
    fn direct_bypass_and_invalid_addresses_never_claim_a_proxy_endpoint() {
        assert_eq!(ResolvedProxy::default().public_view()["state"], "direct");
        let mut proxy = ResolvedProxy {
            https: "http://127.0.0.1:7890".into(),
            bypass: "localhost, *".into(),
            ..Default::default()
        };
        assert_eq!(
            proxy.public_view(),
            json!({"state":"direct","http":null,"https":null})
        );
        proxy.bypass.clear();
        proxy.https = "http://secret:password@".into();
        assert_eq!(
            proxy.public_view(),
            json!({"state":"error","http":null,"https":null})
        );
        assert!(proxy_address("http://localhost:0").is_none());
        assert!(proxy_address("file:///private/secret").is_none());
    }
}
