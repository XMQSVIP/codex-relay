//! Current user's Windows Internet Settings, shared by native children and probes.
//! No registry writes, global environment changes, PAC downloads or WPAD discovery.
use super::{ProxyConfig, ProxyMode, ResolvedProxy};
use crate::error::{Error, Result};
use std::collections::BTreeMap;
use tokio::process::Command;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, GetLastError, GlobalFree},
    Networking::WinHttp::{
        WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WinHttpGetIEProxyConfigForCurrentUser,
    },
};

#[derive(Default)]
struct Settings {
    proxy: String,
    bypass: String,
    pac: bool,
}

struct OwnedConfig(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG);
impl Drop for OwnedConfig {
    fn drop(&mut self) {
        for pointer in [
            self.0.lpszProxy,
            self.0.lpszProxyBypass,
            self.0.lpszAutoConfigUrl,
        ] {
            if !pointer.is_null() {
                // SAFETY: these strings are allocated by WinHTTP and released exactly once.
                unsafe {
                    GlobalFree(pointer.cast());
                }
            }
        }
    }
}

fn read_settings() -> Result<Settings> {
    let mut raw = OwnedConfig(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default());
    // SAFETY: a valid zero-initialized output struct; WinHTTP owns allocation of its strings.
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut raw.0) } == 0 {
        // No configured current-user connection is an ordinary direct connection.
        if unsafe { GetLastError() } == ERROR_FILE_NOT_FOUND {
            return Ok(Settings::default());
        }
        return Err(Error::new(
            "WINDOWS_PROXY",
            "无法读取 Windows 系统代理，请检查系统设置或为账户指定代理。",
        ));
    }
    // SAFETY: on success all non-null fields are null-terminated UTF-16 strings
    // allocated by WinHTTP, alive until the OwnedConfig guard drops.
    unsafe {
        Ok(Settings {
            proxy: wide_string(raw.0.lpszProxy)?,
            bypass: wide_string(raw.0.lpszProxyBypass)?,
            pac: !wide_string(raw.0.lpszAutoConfigUrl)?.is_empty(),
        })
    }
}

unsafe fn wide_string(pointer: *const u16) -> Result<String> {
    if pointer.is_null() {
        return Ok(String::new());
    }
    let mut len = 0;
    while len < 32768 {
        // SAFETY: caller guarantees a null-terminated OS allocation.
        if unsafe { *pointer.add(len) } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, len) })
                .map_err(|_| invalid());
        }
        len += 1;
    }
    Err(invalid())
}

fn invalid() -> Error {
    Error::new(
        "WINDOWS_PROXY",
        "Windows 系统代理格式不受支持，请为账户指定 HTTP/HTTPS 代理地址。",
    )
}

fn environment(command: &Command) -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(k, _)| super::is_proxy_key(&k.to_string_lossy()))
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_ascii_uppercase(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    for (key, value) in command.as_std().get_envs() {
        let key = key.to_string_lossy().to_ascii_uppercase();
        if super::is_proxy_key(&key) {
            if let Some(value) = value {
                env.insert(key, value.to_string_lossy().into_owned());
            } else {
                env.remove(&key);
            }
        }
    }
    env
}
fn explicit(env: &BTreeMap<String, String>) -> bool {
    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
        .iter()
        .any(|key| env.contains_key(*key))
        || env
            .get("NO_PROXY")
            .is_some_and(|value| value.split(',').any(|item| item.trim() == "*"))
}

pub(super) async fn apply_system_proxy(command: &mut Command) -> Result<()> {
    let env = environment(command);
    if explicit(&env) {
        return Ok(());
    }
    let settings = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        tokio::task::spawn_blocking(read_settings),
    )
    .await
    .map_err(|_| Error::new("WINDOWS_PROXY", "读取 Windows 系统代理超时。"))?
    .map_err(|_| Error::new("WINDOWS_PROXY", "读取 Windows 系统代理失败。"))??;
    apply_settings(command, &env, settings)
}

fn apply_settings(
    command: &mut Command,
    env: &BTreeMap<String, String>,
    settings: Settings,
) -> Result<()> {
    if explicit(env) {
        return Ok(());
    }
    let proxy = manual_proxy(&settings)?;
    if proxy.http.is_empty() && proxy.https.is_empty() {
        return Ok(());
    }
    if !proxy.http.is_empty() {
        command.env("HTTP_PROXY", proxy.http);
    }
    if !proxy.https.is_empty() {
        command.env("HTTPS_PROXY", proxy.https);
    }
    // Preserve explicit bypass policy, even when the endpoints came from Windows.
    if !env.contains_key("NO_PROXY") {
        command.env("NO_PROXY", proxy.bypass);
    }
    Ok(())
}

fn endpoint(value: &str) -> Result<String> {
    let url = if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    };
    ProxyConfig {
        mode: ProxyMode::Custom,
        url,
    }
    .validated()
    .map(|p| p.url)
    .map_err(|_| invalid())
}

fn manual_proxy(settings: &Settings) -> Result<ResolvedProxy> {
    let server = settings.proxy.trim();
    if server.is_empty() {
        if settings.pac {
            return Err(Error::new(
                "WINDOWS_PROXY_PAC",
                "Windows 使用 PAC 自动配置脚本；请开启系统手动代理或为账户指定代理地址。",
            ));
        }
        // Automatic detection alone is not an actual configured proxy endpoint.
        return Ok(ResolvedProxy::default());
    }
    let mut result = ResolvedProxy::default();
    let mut seen = std::collections::HashSet::new();
    for entry in server
        .split(|c: char| c == ';' || c.is_ascii_whitespace())
        .filter(|v| !v.is_empty())
    {
        if let Some((protocol, address)) = entry.split_once('=') {
            let protocol = protocol.to_ascii_lowercase();
            if !seen.insert(protocol.clone()) {
                return Err(invalid());
            }
            match protocol.as_str() {
                "http" => result.http = endpoint(address)?,
                // Windows' https= selects destination protocol, not proxy TLS.
                "https" => result.https = endpoint(address)?,
                "ftp" | "socks" => {}
                _ => return Err(invalid()),
            }
        } else {
            if !seen.is_empty() {
                return Err(invalid());
            }
            seen.insert("http".into());
            seen.insert("https".into());
            result.http = endpoint(entry)?;
            result.https = result.http.clone();
        }
    }
    if result.http.is_empty() && result.https.is_empty() {
        return Err(invalid());
    }
    result.bypass = bypass(&settings.bypass)?;
    Ok(result)
}

fn bypass(value: &str) -> Result<String> {
    let mut entries = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    for item in value
        .split(|c: char| c == ';' || c == ',' || c.is_ascii_whitespace())
        .filter(|v| !v.is_empty())
    {
        if item.eq_ignore_ascii_case("<local>") {
            // NO_PROXY has no 'all single-label hosts' syntax. Preserve the machine
            // name and loopback aliases; do not broaden this to bypass all traffic.
            if let Ok(name) = std::env::var("COMPUTERNAME") {
                if name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !name.is_empty()
                {
                    entries.push(name);
                }
            }
        } else if item == "*" {
            return Ok("*".into());
        } else if let Some(domain) = item.strip_prefix("*.").filter(|v| !v.contains('*')) {
            entries.push(format!(".{domain}"));
        } else if item.contains('*') {
            let parts: Vec<_> = item.split('.').collect();
            let prefix: Vec<_> = parts.iter().take_while(|p| **p != "*").collect();
            if parts.len() > 4
                || prefix.is_empty()
                || prefix.len() > 3
                || !parts[prefix.len()..].iter().all(|p| *p == "*")
            {
                return Err(invalid());
            }
            let mut octets = [0u8; 4];
            for (index, part) in prefix.iter().enumerate() {
                octets[index] = part.parse().map_err(|_| invalid())?;
            }
            entries.push(format!(
                "{}/{}",
                std::net::Ipv4Addr::from(octets),
                prefix.len() * 8
            ));
        } else if !item.contains(['<', '>', '/', ':'])
            || item.parse::<std::net::IpAddr>().is_ok()
            || valid_cidr(item)
        {
            entries.push(item.trim_matches(['[', ']']).to_owned());
        } else {
            return Err(invalid());
        }
    }
    entries.sort();
    entries.dedup();
    Ok(entries.join(","))
}
fn valid_cidr(value: &str) -> bool {
    value.split_once('/').is_some_and(|(ip, prefix)| {
        match (ip.parse::<std::net::IpAddr>(), prefix.parse::<u8>()) {
            (Ok(std::net::IpAddr::V4(_)), Ok(bits)) => bits <= 32,
            (Ok(std::net::IpAddr::V6(_)), Ok(bits)) => bits <= 128,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(proxy: &str, bypass: &str) -> Settings {
        Settings {
            proxy: proxy.into(),
            bypass: bypass.into(),
            pac: false,
        }
    }
    #[test]
    fn shared_and_per_protocol_windows_addresses() {
        let p = manual_proxy(&settings("127.0.0.1:7890", "")).unwrap();
        assert_eq!(p.http, "http://127.0.0.1:7890");
        assert_eq!(p.http, p.https);
        let p = manual_proxy(&settings(
            "http=proxy.example:8080;https=[::1]:7891;ftp=unused:21",
            "",
        ))
        .unwrap();
        assert_eq!(p.http, "http://proxy.example:8080");
        assert_eq!(p.https, "http://[::1]:7891");
        assert_eq!(p.public_view()["https"], "http://[::1]:7891");
        let p = manual_proxy(&settings("https=127.0.0.1:7890", "")).unwrap();
        assert!(p.http.is_empty());
    }
    #[test]
    fn disabled_and_invalid_settings_do_not_silently_fall_back() {
        assert!(manual_proxy(&Settings::default()).unwrap().http.is_empty());
        assert_eq!(
            manual_proxy(&Settings {
                pac: true,
                ..Default::default()
            })
            .err()
            .unwrap()
            .code,
            "WINDOWS_PROXY_PAC"
        );
        for raw in [
            "http=",
            "http=a:1;http=b:2",
            "socks=localhost:1080",
            "127.0.0.1:0",
            "http://user:secret@host:80",
            "http://proxy/path",
            "proxy:80 other:81",
        ] {
            assert!(manual_proxy(&settings(raw, "")).is_err(), "{raw}");
        }
    }
    #[test]
    fn bypass_translation_preserves_common_proxy_client_rules() {
        let result =
            bypass("localhost;127.*;10.*;172.16.*;192.168.*;*.example.test;<local>;::1").unwrap();
        for expected in [
            "127.0.0.0/8",
            "10.0.0.0/8",
            "172.16.0.0/16",
            "192.168.0.0/16",
            ".example.test",
            "localhost",
            "::1",
        ] {
            assert!(result.split(',').any(|item| item == expected), "{result}");
        }
        assert_eq!(bypass("*").unwrap(), "*");
        assert!(bypass("172.*.5.*").is_err());
    }
    #[test]
    fn explicit_environment_and_child_overrides_win() {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            let env = BTreeMap::from([(key.to_owned(), "http://explicit:88".into())]);
            let mut command = Command::new("unused");
            apply_settings(&mut command, &env, settings("localhost:7890", "")).unwrap();
            assert_eq!(command.as_std().get_envs().count(), 0);
        }
        let mut command = Command::new("unused");
        apply_settings(
            &mut command,
            &BTreeMap::from([("NO_PROXY".into(), "example.test".into())]),
            settings("localhost:7890", ""),
        )
        .unwrap();
        assert!(!command.as_std().get_envs().any(|(k, _)| k == "NO_PROXY"));
        let mut command = Command::new("unused");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"] {
            command.env_remove(key);
        }
        command.env("https_proxy", "http://child:88");
        let env = environment(&command);
        assert!(explicit(&env));
        assert_eq!(env["HTTPS_PROXY"], "http://child:88");
        assert!(explicit(&BTreeMap::from([(
            "NO_PROXY".into(),
            "localhost, *".into()
        )])));
    }
    #[tokio::test]
    async fn translated_system_proxy_reaches_http_client_and_child_environment() {
        use axum::{Router, extract::Request, routing::any};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(any(|request: Request| async move {
                    assert_eq!(request.uri().host(), Some("probe.example.invalid"));
                    "system-proxy-reached"
                })),
            )
            .await
            .unwrap()
        });
        let p = manual_proxy(&settings(&address.to_string(), "")).unwrap();
        assert_eq!(
            p.client()
                .unwrap()
                .get("http://probe.example.invalid/location")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "system-proxy-reached"
        );
        let mut child = Command::new("unused");
        apply_settings(
            &mut child,
            &BTreeMap::new(),
            settings(&address.to_string(), ""),
        )
        .unwrap();
        assert!(
            child
                .as_std()
                .get_envs()
                .any(|(key, value)| key == "HTTPS_PROXY" && value.unwrap() == p.https.as_str())
        );
        server.abort();
    }
    #[test]
    fn current_windows_settings_can_be_read_without_mutation() {
        let settings = read_settings().unwrap();
        // Inspect only sanitized endpoints; never print PAC URLs or raw settings.
        match manual_proxy(&settings) {
            Ok(proxy) => println!("Windows automatic proxy: {}", proxy.public_view()),
            Err(error) => println!("Windows automatic proxy: {}", error.code),
        }
    }
}
