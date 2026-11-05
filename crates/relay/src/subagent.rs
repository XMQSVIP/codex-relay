//! Explicit caller-provided subagent labels, used only for request observability.
use axum::http::HeaderMap;
use serde_json::Value;

fn kind(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn turn_kind(value: &str) -> Option<String> {
    if value.len() > 16 * 1024 {
        return None;
    }
    let metadata: Value = serde_json::from_str(value).ok()?;
    kind(metadata.get("subagent_kind")?.as_str()?)
}

pub(crate) fn from_headers(headers: &HeaderMap) -> Option<String> {
    let header = |name: &str| {
        (headers.get_all(name).iter().count() == 1)
            .then(|| headers.get(name)?.to_str().ok())
            .flatten()
    };
    header("x-codex-turn-metadata")
        .and_then(turn_kind)
        .or_else(|| header("x-openai-subagent").and_then(kind))
}

/// Per-request/frame metadata takes precedence over connection-level headers.
/// Never inspect prompts, UA strings, or previous-response history for this label.
pub(crate) fn from_body(body: &Value) -> Option<String> {
    [body, &body["client_metadata"]]
        .into_iter()
        .flat_map(|object| {
            ["x-codex-turn-metadata", "turn_metadata", "turnMetadata"]
                .into_iter()
                .filter_map(move |key| object.get(key).and_then(Value::as_str))
        })
        .find_map(turn_kind)
        .or_else(|| {
            body["client_metadata"]["x-openai-subagent"]
                .as_str()
                .and_then(kind)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_sources_and_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert("x-openai-subagent", " review ".parse().unwrap());
        assert_eq!(from_headers(&headers).as_deref(), Some("review"));
        headers.insert(
            "x-codex-turn-metadata",
            r#"{"subagent_kind":"worker"}"#.parse().unwrap(),
        );
        assert_eq!(from_headers(&headers).as_deref(), Some("worker"));
        for key in ["x-codex-turn-metadata", "turn_metadata", "turnMetadata"] {
            for nested in [false, true] {
                let metadata = json!({key: r#"{"subagent_kind":"thread_spawn"}"#});
                let body = if nested {
                    json!({"client_metadata": metadata})
                } else {
                    metadata
                };
                assert_eq!(from_body(&body).as_deref(), Some("thread_spawn"));
            }
        }
        assert_eq!(
            from_body(&json!({"client_metadata":{"x-openai-subagent":"guardian"}})).as_deref(),
            Some("guardian")
        );
        assert_eq!(from_body(&json!({"client_metadata":{"x-openai-subagent":"review","x-codex-turn-metadata":r#"{"subagent_kind":"worker"}"#}})).as_deref(), Some("worker"));
    }

    #[test]
    fn absent_invalid_and_unrelated_data_do_not_mark_requests() {
        for value in [
            json!(null),
            json!(""),
            json!("  "),
            json!(7),
            json!("x".repeat(129)),
            json!("worker\nforged"),
        ] {
            assert!(from_body(&json!({"client_metadata":{"x-openai-subagent":value}})).is_none());
        }
        for metadata in [
            "not json".to_owned(),
            "[]".into(),
            r#"{"subagent_kind":false}"#.into(),
            " ".repeat(16 * 1024 + 1),
        ] {
            assert!(
                from_body(&json!({"client_metadata":{"x-codex-turn-metadata":metadata}})).is_none()
            );
        }
        assert!(from_body(&json!({"input":"subagent_kind: worker", "metadata":{"subagent_kind":"worker"},"previous_response_id":"worker"})).is_none());
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "subagent/worker".parse().unwrap());
        assert!(from_headers(&headers).is_none());
        headers.append("x-openai-subagent", "worker".parse().unwrap());
        headers.append("x-openai-subagent", "review".parse().unwrap());
        assert!(from_headers(&headers).is_none());
    }
}
