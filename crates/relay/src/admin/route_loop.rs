use super::AdminState;
use crate::error::{Error, Result};
use std::{net::IpAddr, time::Duration};

impl AdminState {
    /// Check advertised names and local listeners, including DNS aliases. No
    /// credential or model request is sent while inspecting the endpoint.
    pub(crate) async fn check_route_loop(&self, base: &str) -> Result<()> {
        let upstream =
            url::Url::parse(base).map_err(|_| Error::new("PROVIDER_CONFIG", "模型地址无效。"))?;
        let Some(host) = upstream.host_str() else {
            return Ok(());
        };
        let Some(port) = upstream.port_or_known_default() else {
            return Ok(());
        };
        let endpoints = std::iter::once(self.origin.as_str())
            .chain(self.network_endpoints.iter().map(|e| e.base_url.as_str()));
        for endpoint in endpoints.filter_map(|s| url::Url::parse(s).ok()) {
            if endpoint.host_str() == Some(host) && endpoint.port_or_known_default() == Some(port) {
                return Err(loop_error());
            }
        }
        let local_port = url::Url::parse(&self.origin)
            .ok()
            .and_then(|u| u.port_or_known_default());
        if local_port != Some(port)
            && !self
                .network_endpoints
                .iter()
                .any(|e| e.listen.port() == port)
        {
            return Ok(());
        }
        let addresses = if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            vec![ip]
        } else if host.eq_ignore_ascii_case("localhost") {
            vec![IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]
        } else {
            match tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::lookup_host((host, port)),
            )
            .await
            {
                Ok(Ok(values)) => values.map(|s| s.ip()).collect(),
                _ => vec![],
            }
        };
        let local = if_addrs::get_if_addrs().unwrap_or_default();
        if addresses
            .iter()
            .any(|ip| ip.is_loopback() || local.iter().any(|i| i.ip() == *ip))
        {
            return Err(loop_error());
        }
        Ok(())
    }
}
fn loop_error() -> Error {
    Error::new(
        "RELAY_LOOP",
        "模型地址指向本中转站，会形成 app-server 中转回环；请将服务端账户指向真实模型提供方。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_aliases_and_advertised_endpoint_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let state = AdminState::new(4317, root.path().join("accounts"), "a".repeat(64))
            .await
            .unwrap();
        for url in [
            "http://127.0.0.1:4317/v1",
            "http://localhost:4317/v1",
            "http://[::1]:4317/v1",
        ] {
            assert_eq!(
                state.check_route_loop(url).await.unwrap_err().code,
                "RELAY_LOOP"
            );
        }
        state
            .check_route_loop("http://127.0.0.1:4318/v1")
            .await
            .unwrap();
        state
            .check_route_loop("https://api.openai.com/v1")
            .await
            .unwrap();
    }
}
