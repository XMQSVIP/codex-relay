use codex_relay::{
    guard::{TextEvents, TextSse, text_request},
    provider,
};
use serde_json::{Value, json};

fn item() -> Value {
    json!({"type":"message","id":"m","role":"assistant","content":[]})
}
fn events() -> Vec<Value> {
    vec![
        json!({"type":"response.created","response":{"id":"r","output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":item()}),
        json!({"type":"response.output_text.delta","item_id":"m","output_index":0,"content_index":0,"delta":"中文🌍"}),
        json!({"type":"response.completed","response":{"id":"r","status":"completed","output":[item()]}}),
    ]
}
fn frame(value: &Value) -> String {
    format!(
        "event: {}\r\ndata: {value}\r\n\r\n",
        value["type"].as_str().unwrap()
    )
}

#[test]
fn native_metadata_frames_are_discarded_without_weakening_text_filter() {
    for kind in [
        "codex.response.metadata",
        "response.metadata",
        "codex.rate_limits",
        "responsesapi.websocket_timing",
    ] {
        let metadata = json!({"type":kind,"headers":{"x-codex-turn-state":"opaque-private-state"},"item":{"type":"function_call","arguments":"never-forward"}});
        let mut filter = TextSse::default();
        assert!(filter.push(frame(&metadata).as_bytes()).unwrap().is_empty());
        let mut out = String::new();
        for event in events() {
            out.extend(filter.push(frame(&event).as_bytes()).unwrap());
        }
        // Upstream may emit transport metadata after the completion frame;
        // it must not invalidate otherwise complete text or release EOF early.
        assert!(filter.push(frame(&metadata).as_bytes()).unwrap().is_empty());
        out.extend(filter.finish().unwrap());
        assert!(!out.contains("opaque-private-state") && !out.contains("never-forward"));
        assert!(out.contains("response.completed"));
        let mut truncated = TextSse::default();
        truncated.push(frame(&metadata).as_bytes()).unwrap();
        assert!(truncated.finish().is_err());
        let mut poisoned = TextEvents::default();
        assert!(poisoned.accept(json!({"type":"response.output_item.added","item":{"id":"bad","type":"function_call"}}),None).is_err());
        assert!(poisoned.accept(metadata, None).is_err());
    }
}

#[test]
fn streaming_preserves_utf8_at_every_byte_and_holds_terminal_until_eof() {
    let data = events().iter().map(frame).collect::<String>();
    let mut filter = TextSse::default();
    let mut forwarded = Vec::new();
    for byte in data.as_bytes() {
        forwarded.extend(filter.push(&[*byte]).unwrap());
    }
    assert_eq!(forwarded.len(), 3);
    assert!(forwarded[2].contains("中文🌍"));
    assert!(!forwarded.iter().any(|s| s.contains("response.completed")));
    assert!(filter.finish().unwrap()[0].contains("response.completed"));
    assert!(filter.push(b"\n").is_err());
}

#[test]
fn rejects_tools_in_all_output_locations_and_stays_closed() {
    for kind in [
        "function_call",
        "custom_tool_call",
        "web_search_call",
        "computer_call",
    ] {
        let tool = json!({"type":kind,"id":"tool","name":"exec","arguments":"secret"});
        for event in [
            json!({"type":"response.output_item.added","item":tool}),
            json!({"type":"response.output_item.done","item":tool}),
            json!({"type":"response.completed","response":{"id":"r","output":[tool]}}),
        ] {
            let mut filter = TextEvents::default();
            assert!(filter.accept(event, None).is_err());
            assert!(filter.accept(events()[0].clone(), None).is_err());
        }
    }
}

#[test]
fn rejects_unknown_truncated_invalid_utf8_and_events_after_completion() {
    let mut filter = TextSse::default();
    assert!(
        filter
            .push(b"data: {\"type\":\"response.unknown\"}\n\n")
            .is_err()
    );
    let mut filter = TextSse::default();
    filter.push(frame(&events()[0]).as_bytes()).unwrap();
    assert!(filter.finish().is_err());
    let mut filter = TextSse::default();
    assert!(filter.push(b"data: \xff\n\n").is_err());
    let mut filter = TextSse::default();
    for value in events() {
        filter.push(frame(&value).as_bytes()).unwrap();
    }
    assert!(filter.push(frame(&events()[2]).as_bytes()).is_err());
    assert!(filter.finish().is_err());
}

#[test]
fn limits_unterminated_events() {
    assert!(
        TextSse::default()
            .push(&vec![b'a'; 1024 * 1024 + 1])
            .is_err()
    );
}

#[test]
fn drops_unrecognized_fields_annotations_and_reasoning() {
    let mut filter = TextEvents::default();
    let clean=filter.accept(json!({"type":"response.output_item.added","output_index":0,"credential":"secret","item":{"id":"m","type":"message","role":"assistant","hidden":"secret","content":[{"type":"output_text","text":"ok","annotations":[{"secret":"secret"}],"logprobs":"secret"}]}}),None).unwrap().unwrap();
    assert!(!clean.to_string().contains("secret"));
    assert!(
        filter
            .accept(
                json!({"type":"response.reasoning_text.delta","delta":"private reasoning"}),
                None
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejects_unassociated_text_and_response_id_changes() {
    assert!(
        TextEvents::default()
            .accept(events()[2].clone(), None)
            .is_err()
    );
    let mut filter = TextEvents::default();
    filter.accept(events()[0].clone(), None).unwrap();
    assert!(
        filter
            .accept(
                json!({"type":"response.completed","response":{"id":"different","output":[]}}),
                None
            )
            .is_err()
    );
    let mut filter = TextEvents::default();
    filter.accept(events()[1].clone(), None).unwrap();
    let mut delta = events()[2].clone();
    delta["output_index"] = json!(1);
    assert!(filter.accept(delta, None).is_err());
    assert!(
        TextEvents::default()
            .accept(events()[0].clone(), Some("response.completed"))
            .is_err()
    );
}

#[test]
fn removes_native_additional_tools_and_rejects_nontext_input() {
    let body = json!({"model":"test","stream":true,"input":[{"type":"additional_tools","role":"developer","tools":[{"name":"exec"}]},{"role":"user","content":[{"type":"input_text","text":"hi","attachment":"secret"}]}],"parallel_tool_calls":true,"store":true});
    let clean = text_request(body, "test").unwrap();
    assert_eq!(clean["tools"], json!([]));
    assert_eq!(clean["tool_choice"], "none");
    assert_eq!(clean["store"], false);
    assert_eq!(clean["parallel_tool_calls"], false);
    assert!(!clean.to_string().contains("secret"));
    assert_eq!(clean["input"].as_array().unwrap().len(), 1);
    for input in [
        json!([{"type":"function_call_output","call_id":"x","output":"hi"}]),
        json!([{"role":"user","content":[{"type":"input_image","image_url":"https://invalid"}]}]),
    ] {
        assert!(text_request(json!({"model":"test","stream":true,"input":input}), "test").is_err());
    }
    assert!(text_request(json!({"model":"other","stream":true,"input":[]}), "test").is_err());
}

#[test]
fn provider_summary_never_exposes_credentials() {
    let value = json!({"config":{"model":"test","model_provider":"LAN","model_providers":{"LAN":{"wire_api":"responses","base_url":"http://192.168.1.1/v1","experimental_bearer_token":"secret-bearer","http_headers":{"X-Actor":"secret-actor"}}}}});
    let summary = provider::from_config(&value, &serde_json::Value::Null).unwrap();
    let serialized = serde_json::to_string(&summary).unwrap();
    assert!(!serialized.contains("secret"));
    assert!(summary.required_headers.contains(&"authorization".into()));
    assert!(summary.required_headers.contains(&"x-actor".into()));
    for url in [
        "http://127.0.0.1/v1",
        "https://example.org/v1",
        "http://192.168.1.1/v1?key=x",
        "http://u:p@192.168.1.1/v1",
        "file:///v1",
    ] {
        assert!(provider::lan_url(url).is_err());
    }
    for name in [
        "Cookie",
        "Host",
        "ChatGPT-Account-Id",
        "Proxy-Authorization",
        "Content-Length",
        "X-Forwarded-For",
    ] {
        assert!(provider::header_name(name).is_err());
    }
}

#[test]
fn text_filter_preserves_native_message_cache_and_client_identifiers() {
    let original = json!({"model":"test","stream":true,"prompt_cache_key":"native-thread",
        "client_metadata":{"native":"untouched"},"input":[{"type":"message","id":"native-message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"hello"}]}]});
    let filtered = text_request(original.clone(), "test").unwrap();
    for field in ["prompt_cache_key", "client_metadata", "input"] {
        assert_eq!(filtered[field], original[field]);
    }
    let mut invalid = original;
    invalid["input"][0]["id"] = json!({"unexpected":"object"});
    assert!(text_request(invalid, "test").is_err());
    let mut filter = TextEvents::default();
    let created = filter.accept(events()[0].clone(), None).unwrap().unwrap();
    assert_eq!(created["response"]["id"], "r");
    let added = filter.accept(events()[1].clone(), None).unwrap().unwrap();
    assert_eq!(added["item"]["id"], "m");
}
