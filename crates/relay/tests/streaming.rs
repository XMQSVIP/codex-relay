use codex_relay::streaming::{TextState, encode_sse};
use serde_json::{Value, json};

#[test]
fn upstream_errors_preserve_http_diagnosis_without_overriding_cancellation_or_protocol_checks() {
    use codex_relay::{guard::Stats, streaming::classify_failure};
    for (status, code) in [
        (503, "UPSTREAM_UNAVAILABLE"),
        (401, "UPSTREAM_AUTH_FAILED"),
        (403, "UPSTREAM_AUTH_FAILED"),
        (429, "UPSTREAM_RATE_LIMITED"),
        (504, "UPSTREAM_TIMEOUT"),
        (400, "UPSTREAM_REQUEST_REJECTED"),
    ] {
        let stats = Stats {
            upstream_status: Some(status),
            failure_code: Some("GUARD_UPSTREAM".into()),
            ..Stats::default()
        };
        let error = classify_failure("STREAM_NATIVE_FAILED", &stats);
        assert_eq!(error.code, code);
        assert!(error.message.contains(&format!("HTTP {status}")));
        assert_eq!(classify_failure(&error.code, &stats).message, error.message);
        for original in [
            "STREAM_CANCELLED",
            "STREAM_OUTPUT_LIMIT",
            "STREAM_MODEL_CHANGED",
        ] {
            assert_eq!(classify_failure(original, &stats).code, original);
        }
    }
    let stats = Stats {
        failure_code: Some("GUARD_TIMEOUT".into()),
        ..Stats::default()
    };
    assert_eq!(
        classify_failure("STREAM_NATIVE_FAILED", &stats).code,
        "UPSTREAM_TIMEOUT"
    );
    let stats = Stats {
        failure_code: Some("GUARD_UPSTREAM".into()),
        ..Stats::default()
    };
    assert_eq!(
        classify_failure("STREAM_NATIVE_FAILED", &stats).code,
        "UPSTREAM_CONNECTION_FAILED"
    );
    let stats = Stats {
        upstream_status: Some(200),
        ..stats
    };
    assert_eq!(
        classify_failure("STREAM_NATIVE_FAILED", &stats).code,
        "UPSTREAM_INVALID_RESPONSE"
    );
    let stats = Stats {
        failure_code: Some("secret token or native body".into()),
        ..Stats::default()
    };
    assert!(
        !classify_failure("STREAM_NATIVE_FAILED", &stats)
            .message
            .contains("secret")
    );
}

fn event(method: &str, params: Value) -> Value {
    let mut params = params;
    params["threadId"] = "thread-native".into();
    params["turnId"] = "turn-native".into();
    json!({"method":method,"params":params})
}
fn message(method: &str, text: &str) -> Value {
    event(
        method,
        json!({"item":{"type":"agentMessage","id":"item-native","text":text,"phase":"final_answer"}}),
    )
}
fn delta(text: &str) -> Value {
    event(
        "item/agentMessage/delta",
        json!({"itemId":"item-native","delta":text}),
    )
}
fn terminal(status: &str) -> Value {
    event(
        "turn/completed",
        json!({"turn":{"id":"turn-native","status":status}}),
    )
}
fn state() -> TextState {
    let mut s = TextState::new("thread-native", "turn-native", "native-model", 100).unwrap();
    s.created().unwrap();
    s
}
#[test]
fn streams_unicode_and_reconciles_authoritative_final_text_without_duplication() {
    let mut s = TextState::new("thread-native", "turn-native", "native-model", 100).unwrap();
    let mut out = vec![s.created().unwrap()];
    out.extend(s.accept(&message("item/started", "")).unwrap());
    out.extend(s.accept(&delta("中文🌍\n")).unwrap());
    out.extend(
        s.accept(&message("item/completed", "中文🌍\n完成"))
            .unwrap(),
    );
    out.extend(s.accept(&terminal("completed")).unwrap());
    assert_eq!(
        out.iter()
            .filter_map(|e| if e["type"] == "response.output_text.delta" {
                e["delta"].as_str()
            } else {
                None
            })
            .collect::<String>(),
        "中文🌍\n完成"
    );
    for (i, e) in out.iter().enumerate() {
        assert_eq!(e["sequence_number"], i);
        let frame = encode_sse(e).unwrap();
        assert_eq!(frame.lines().count(), 3);
        assert_eq!(
            serde_json::from_str::<Value>(
                frame
                    .lines()
                    .nth(1)
                    .unwrap()
                    .strip_prefix("data: ")
                    .unwrap()
            )
            .unwrap(),
            *e
        );
    }
    let response = &out.last().unwrap()["response"];
    assert_eq!(response["id"], "turn-native");
    assert_eq!(response["metadata"]["native_thread_id"], "thread-native");
    assert_eq!(response["output"][0]["id"], "item-native");
    assert_eq!(response["output"][0]["phase"], "final_answer");
    assert!(response["usage"].is_null());
    assert!(s.accept(&delta("late")).is_err());
}
#[test]
fn usage_uses_latest_turn_value_without_accumulating_thread_total() {
    let mut s = state();
    for input in [10, 12, 12] {
        s.accept(&event("thread/tokenUsage/updated", json!({"tokenUsage":{"total":{"inputTokens":9999},"last":{"inputTokens":input,"outputTokens":4,"totalTokens":input+4,"cachedInputTokens":2,"cacheWriteInputTokens":0,"reasoningOutputTokens":1}}}))).unwrap();
    }
    let usage = s.response("in_progress")["usage"].clone();
    assert_eq!(usage["input_tokens"], 12);
    assert_eq!(usage["total_tokens"], 16);
    assert_eq!(usage["input_tokens_details"]["cached_tokens"], 2);
    assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 1);
    assert!(s.accept(&event("thread/tokenUsage/updated", json!({"tokenUsage":{"last":{"inputTokens":10,"outputTokens":4,"totalTokens":14,"cachedInputTokens":11}}}))).is_err());
}
#[test]
fn rejects_mismatches_tools_and_invalid_order_and_remains_closed() {
    let mut wrong_turn = delta("secret");
    wrong_turn["params"]["turnId"] = "another-turn".into();
    for bad in [
        wrong_turn,
        delta("before-item"),
        terminal("completed"),
        event(
            "item/started",
            json!({"item":{"id":"tool","type":"commandExecution"}}),
        ),
        event("item/newUnknownDelta", json!({})),
    ] {
        let mut s = state();
        assert!(s.accept(&bad).is_err());
        assert_eq!(
            s.accept(&message("item/started", "")).unwrap_err().code,
            "STREAM_CLOSED"
        );
    }
    let mut s = state();
    let mut other = delta("ignored");
    other["params"]["threadId"] = "another-thread".into();
    assert!(s.accept(&other).unwrap().is_empty());
    s.accept(&message("item/started", "prefix")).unwrap();
    assert_eq!(
        s.accept(&message("item/completed", "different"))
            .unwrap_err()
            .code,
        "STREAM_TEXT_MISMATCH"
    );
}
#[test]
fn never_turns_failed_or_interrupted_turns_into_success_or_leaks_native_errors() {
    for status in ["failed", "interrupted"] {
        let mut s = state();
        assert!(s.accept(&terminal(status)).unwrap().is_empty());
        assert_eq!(s.status.as_deref(), Some(status));
    }
    let mut s = state();
    let error = s
        .accept(&event(
            "error",
            json!({"error":{"message":"SENSITIVE_UPSTREAM_TOKEN"},"willRetry":true}),
        ))
        .unwrap_err();
    let failed = s.failed(&error.code);
    assert_eq!(failed["type"], "response.failed");
    assert!(!failed.to_string().contains("SENSITIVE"));
}
#[test]
fn bounds_text_output_and_rejects_sse_header_injection() {
    let mut s = state();
    s.accept(&message("item/started", "")).unwrap();
    assert_eq!(
        s.accept(&delta(&"a".repeat(1024 * 1024 + 1)))
            .unwrap_err()
            .code,
        "STREAM_OUTPUT_LIMIT"
    );
    for kind in ["", "response.delta\nevent: fake", "中文"] {
        assert!(encode_sse(&json!({"type":kind})).is_err());
    }
}
#[test]
fn keeps_multiple_message_phases_and_suppresses_reasoning_content() {
    let mut s = state();
    for phase in ["commentary", "partial_answer", "final_answer"] {
        for method in ["item/started", "item/completed"] {
            s.accept(&event(
                method,
                json!({"item":{"type":"agentMessage","id":phase,"phase":phase,"text":phase}}),
            ))
            .unwrap();
        }
    }
    let hidden = s
        .accept(&event(
            "item/reasoning/textDelta",
            json!({"delta":"PRIVATE_REASONING"}),
        ))
        .unwrap();
    assert!(hidden.is_empty());
    let response = s.accept(&terminal("completed")).unwrap();
    assert_eq!(
        response[0]["response"]["output"].as_array().unwrap().len(),
        3
    );
    assert!(!response[0].to_string().contains("PRIVATE_REASONING"));
}
