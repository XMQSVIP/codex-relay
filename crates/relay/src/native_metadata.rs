use crate::error::{Error, Result};
use axum::http::HeaderMap;

// Observed on the pinned app-server's local synthetic Responses request. Copy
// values from that process only; never invent defaults or replay another account's IDs.
pub const REQUEST_HEADERS: &[&str] = &[
    "user-agent",
    "originator",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-codex-beta-features",
    "x-openai-internal-codex-responses-lite",
];

pub fn copy_request_headers(source: &HeaderMap, target: &mut HeaderMap) -> Result<()> {
    for &name in REQUEST_HEADERS {
        let Some(value) = source.get(name) else {
            continue;
        };
        let hop_by_hop = source.get_all("connection").iter().any(|connection| {
            connection.to_str().map_or(true, |s| {
                s.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case(name))
            })
        });
        let max_bytes = if name == "x-codex-turn-metadata" {
            16384
        } else {
            1024
        };
        if hop_by_hop
            || source.get_all(name).iter().count() != 1
            || value.as_bytes().is_empty()
            || value.as_bytes().len() > max_bytes
        {
            return Err(Error::new("GUARD_NATIVE_METADATA", "原生请求元数据无效。"));
        }
        target.insert(name, value.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_native_values_without_defaults_credentials_or_transport_headers() {
        let mut source = HeaderMap::new();
        for &name in REQUEST_HEADERS {
            source.insert(name, format!("native-{name}").parse().unwrap());
        }
        for name in [
            "authorization",
            "cookie",
            "chatgpt-account-id",
            "host",
            "content-length",
            "x-oai-attestation",
            "x-unrecognized",
        ] {
            source.insert(name, "not-client-metadata".parse().unwrap());
        }
        let mut target = HeaderMap::new();
        copy_request_headers(&source, &mut target).unwrap();
        assert_eq!(target.len(), REQUEST_HEADERS.len());
        for &name in REQUEST_HEADERS {
            assert_eq!(target[name], source[name]);
        }
        let mut empty = HeaderMap::new();
        copy_request_headers(&HeaderMap::new(), &mut empty).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn rejects_ambiguous_oversized_and_hop_by_hop_metadata() {
        let mut source = HeaderMap::new();
        source.append("session-id", "first".parse().unwrap());
        source.append("session-id", "second".parse().unwrap());
        assert!(copy_request_headers(&source, &mut HeaderMap::new()).is_err());
        source.insert("session-id", "x".repeat(1025).parse().unwrap());
        assert!(copy_request_headers(&source, &mut HeaderMap::new()).is_err());
        source.insert("session-id", "native".parse().unwrap());
        source.insert("connection", "keep-alive, Session-Id".parse().unwrap());
        assert!(copy_request_headers(&source, &mut HeaderMap::new()).is_err());
    }
}
