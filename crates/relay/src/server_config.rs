use codex_relay::error::{Error, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfig {
    #[serde(default = "default_port")]
    port: u16,
}

fn default_port() -> u16 {
    4317
}

pub fn resolve_port(path: &Path, override_port: Option<u16>) -> Result<u16> {
    let configured = match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str::<ServerConfig>(text.trim_start_matches('\u{feff}'))
                .map_err(|error| {
                    Error::new(
                        "INVALID_SERVER_CONFIG",
                        &format!("{}: {error}", path.display()),
                    )
                })?
                .port
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_port(),
        Err(error) => {
            return Err(Error::new(
                "INVALID_SERVER_CONFIG",
                &format!("{}: {error}", path.display()),
            ));
        }
    };
    if configured < 1024 {
        return Err(Error::new(
            "INVALID_SERVER_CONFIG",
            "配置端口须为 1024–65535。",
        ));
    }
    let port = override_port.unwrap_or(configured);
    if port < 1024 {
        return Err(Error::new("INVALID_PORT", "端口须为 1024–65535。"));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_config_and_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.config.json");
        assert_eq!(resolve_port(&path, None).unwrap(), 4317);
        std::fs::write(&path, "\u{feff}{\"port\":8080}").unwrap();
        assert_eq!(resolve_port(&path, None).unwrap(), 8080);
        assert_eq!(resolve_port(&path, Some(9000)).unwrap(), 9000);
        assert!(resolve_port(&path, Some(0)).is_err());
    }

    #[test]
    fn rejects_invalid_config_instead_of_silently_using_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.config.json");
        for text in [
            "{",
            "{\"port\":0}",
            "{\"port\":65536}",
            "{\"port\":\"8080\"}",
            "{\"ports\":8080}",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(resolve_port(&path, None).is_err(), "{text}");
        }
    }
}
