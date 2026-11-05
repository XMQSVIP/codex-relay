use super::*;
use serde_json::json;

fn digest(value: &str) -> Option<Digest> {
    crate::profiles::warm::digest(&value).ok()
}
fn headers() -> Headers {
    let mut headers = HeaderMap::new();
    headers.insert("session-id", "cache-route".parse().unwrap());
    headers.insert("thread-id", "header-thread".parse().unwrap());
    headers.insert(
        "x-codex-turn-metadata",
        r#"{"thread_id":"connection-thread","session_id":"connection-session"}"#
            .parse()
            .unwrap(),
    );
    Headers::parse(&headers)
}

#[test]
fn precedence_and_thread_identity_ignore_cache_header_changes() {
    let mut body = json!({"thread_id":"body-thread", "client_metadata": {
        "thread_id":"explicit-thread", "x-codex-turn-metadata": r#"{"thread_id":"frame-thread"}"#}});
    for expected in [
        "explicit-thread",
        "frame-thread",
        "body-thread",
        "connection-thread",
    ] {
        let id = Body::parse(&body).resolve(headers());
        assert_eq!(id.own, digest(expected));
        assert!(id.reusable);
        match expected {
            "explicit-thread" => {
                body["client_metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("thread_id");
            }
            "frame-thread" => {
                body["client_metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("x-codex-turn-metadata");
            }
            "body-thread" => {
                body.as_object_mut().unwrap().remove("thread_id");
            }
            _ => {}
        }
    }
    let body = Body::parse(&json!({"client_metadata":{"thread_id":"stable"}}));
    assert_eq!(
        body.resolve(headers()).own,
        body.resolve(Headers::default()).own
    );
    assert_eq!(
        Body::parse(&json!({"session_id":"stable"}))
            .resolve(Headers::default())
            .own,
        body.resolve(headers()).own
    );
}

#[test]
fn explicit_parent_and_fork_are_hints_and_anonymous_children_cannot_reuse() {
    assert!(
        !Body::parse(&json!({"client_metadata":{"subagent_kind":"worker"}}))
            .resolve(headers())
            .reusable
    );
    for field in [
        "parent_thread_id",
        "forked_from_thread_id",
        "forked_from_id",
    ] {
        let child = Body::parse(&json!({"client_metadata":{"thread_id":"child",field:"parent"}}))
            .resolve(headers());
        assert_eq!(child.own, digest("child"));
        assert_eq!(child.parent, digest("parent"));
        assert!(child.reusable);
    }
    for body in [
        json!({"client_metadata":{"subagent_kind":"worker"}}),
        json!({"parent_thread_id":"parent"}),
        json!({"thread_id":"parent","parent_thread_id":"parent"}),
        json!({"client_metadata":{"session_id":"parent","subagent_kind":"worker"}}),
    ] {
        let child = Body::parse(&body).resolve(Headers::default());
        assert!(!child.reusable, "{body}");
        assert!(child.parent.is_none());
    }
    let mut plain = HeaderMap::new();
    plain.insert("session-id", "parent".parse().unwrap());
    plain.insert("x-openai-subagent", "worker".parse().unwrap());
    assert!(!Body::default().resolve(Headers::parse(&plain)).reusable);
}

#[test]
fn conflicts_disable_reuse_but_lower_priority_connection_fields_do_not_override_frames() {
    let body = json!({"turn_metadata":r#"{"thread_id":"one"}"#,
        "client_metadata":{"x-codex-turn-metadata":r#"{"thread_id":"two","parent_thread_id":"parent"}"#}});
    let identity = Body::parse(&body).resolve(headers());
    assert!(!identity.reusable);
    assert!(identity.own.is_none() && identity.parent.is_none());
    let mut plain = HeaderMap::new();
    plain.append("thread-id", "one".parse().unwrap());
    plain.append("thread-id", "two".parse().unwrap());
    assert!(!Body::default().resolve(Headers::parse(&plain)).reusable);
    let frame = Body::parse(&json!({"thread_id":"frame"})).resolve(Headers::parse(&plain));
    assert!(frame.reusable);
    assert_eq!(frame.own, digest("frame"));
}

#[test]
fn only_bounded_explicit_identity_is_used() {
    assert_eq!(
        Body::parse(&json!({"session_id":"frame-session"}))
            .resolve(headers())
            .own,
        digest("frame-session")
    );
    for value in [
        json!(null),
        json!(42),
        json!(" "),
        json!("x\n"),
        json!("x".repeat(513)),
    ] {
        assert!(
            Body::parse(&json!({"thread_id":value}))
                .resolve(Headers::default())
                .own
                .is_none()
        );
    }
    let body = json!({"input":"session_id: forged", "request_id":"request", "turn_id":"turn",
        "client_metadata":{"root_turn_id":"root","agent_name":"worker","turn_metadata":" ".repeat(16385)}});
    let id = Body::parse(&body).resolve(Headers::default());
    assert!(id.own.is_none() && id.parent.is_none() && id.reusable);
    assert_eq!(
        Body::parse(&json!({"thread_id":" stable "}))
            .resolve(Headers::default())
            .own,
        digest("stable")
    );
}
