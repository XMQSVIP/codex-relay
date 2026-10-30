use codex_relay::{client_tools, guard::TextSse, inference::TextRequest};
use serde_json::{Value, json};
fn request() -> Value {
    json!({"model":"model-a","input":[{"role":"user","content":"read local file"}],"tools":[{"type":"function","name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}},{"type":"custom","name":"patch"}],"store":false,"stream":true})
}
fn frame(v: &Value) -> String {
    format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap())
}
#[test]
fn tool_round_trip_preserves_context_and_never_forwards_native_tools() {
    let mut r = request();
    r["instructions"] = json!("Use client workspace");
    r["input"].as_array_mut().unwrap().extend([
        json!({"type":"reasoning","id":"reason","summary":[],"encrypted_content":"opaque"}),
        json!({"type":"function_call","id":"fc1","call_id":"call1","name":"read_file","arguments":"{\"path\":\"a.rs\"}"}),
        json!({"type":"function_call_output","call_id":"call1","output":"client file contents"}),
        json!({"type":"custom_tool_call","id":"fc2","call_id":"call2","name":"patch","input":"patch content"}),
        json!({"type":"custom_tool_call_output","call_id":"call2","output":"done"}),
    ]);
    let parsed: TextRequest = serde_json::from_value(r.clone()).unwrap();
    let payload = parsed.client_payload().unwrap().unwrap();
    assert!(!parsed.prompt().unwrap().contains("client file contents"));
    let outbound = client_tools::upstream_request(
        json!({"model":"model-a","instructions":"SERVER SYSTEM","input":[{"role":"user","content":"SERVER FILES"}],"tools":[{"name":"server_exec"}],"prompt_cache_key":"native-id"}),
        &payload,
    );
    assert_eq!(outbound["input"], r["input"]);
    assert_eq!(outbound["tools"], r["tools"]);
    assert_eq!(outbound["instructions"], r["instructions"]);
    assert_eq!(outbound["prompt_cache_key"], "native-id");
    assert!(!outbound.to_string().contains("SERVER"));
}
#[test]
fn invalid_and_server_tools_rejected_before_dispatch() {
    for tool in [
        json!({"type":"web_search"}),
        json!({"type":"mcp","server_url":"http://localhost"}),
        json!({"type":"function","name":"bad","parameters":null}),
    ] {
        let mut r = request();
        r["tools"] = json!([tool]);
        assert!(client_tools::validate_request(&r).is_err());
    }
    for input in [
        json!([]),
        json!([{"type":"function_call_output","call_id":"missing","output":"x"}]),
        json!([{"role":"user","content":[{"type":"input_image","image_url":"x"}]}]),
    ] {
        let mut r = request();
        r["input"] = input;
        assert!(client_tools::validate_request(&r).is_err());
    }
    let mut r = request();
    r["tool_choice"] = json!({"type":"function","name":"undeclared"});
    assert!(client_tools::validate_request(&r).is_err());
    let mut r = request();
    r["store"] = json!(true);
    assert!(
        serde_json::from_value::<TextRequest>(r)
            .unwrap()
            .client_payload()
            .is_err()
    );
}
#[test]
fn client_stream_preserves_parallel_tools_and_holds_completion() {
    let function =
        json!({"id":"f","type":"function_call","name":"read_file","call_id":"c1","arguments":"{}"});
    let custom =
        json!({"id":"c","type":"custom_tool_call","name":"patch","call_id":"c2","input":"修改🌍"});
    let response = json!({"id":"resp","object":"response","status":"completed","output":[function, custom],"usage":{"input_tokens":12,"output_tokens":4,"total_tokens":16}});
    let events = [
        json!({"type":"response.created","response":{"id":"resp","output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":function}),
        json!({"type":"response.function_call_arguments.delta","item_id":"f","output_index":0,"delta":"{}"}),
        json!({"type":"response.function_call_arguments.done","item_id":"f","output_index":0,"arguments":"{}"}),
        json!({"type":"response.output_item.added","output_index":1,"item":custom}),
        json!({"type":"response.custom_tool_call_input.delta","item_id":"c","output_index":1,"delta":"修改🌍"}),
        json!({"type":"response.custom_tool_call_input.done","item_id":"c","output_index":1,"input":"修改🌍"}),
        json!({"type":"response.completed","response":response}),
    ];
    let wire = events.iter().map(frame).collect::<String>();
    let mut filter = TextSse::for_client(&request()).unwrap();
    let mut out = Vec::new();
    for b in wire.as_bytes() {
        out.extend(filter.push(&[*b]).unwrap());
    }
    assert!(!out.iter().any(|s| s.contains("response.completed")));
    out.extend(filter.finish().unwrap());
    assert!(out.iter().any(|s| s.contains("修改🌍")));
    assert!(out.last().unwrap().contains("c2"));
}
#[test]
fn undeclared_tools_bad_indices_and_truncation_fail_closed() {
    let tool = json!({"id":"f","type":"function_call","call_id":"c","name":"server_exec","arguments":"{}"});
    for event in [
        json!({"type":"response.output_item.added","output_index":0,"item":tool}),
        json!({"type":"response.completed","response":{"id":"r","output":[tool]}}),
        json!({"type":"response.function_call_arguments.delta","item_id":"unknown","output_index":0,"delta":"{}"}),
        json!({"type":"response.unknown"}),
    ] {
        let mut f = TextSse::for_client(&request()).unwrap();
        assert!(f.push(frame(&event).as_bytes()).is_err());
        assert!(f.finish().is_err());
    }
    let mut f = TextSse::for_client(&request()).unwrap();
    f.push(frame(&json!({"type":"response.created","response":{"id":"r","output":[]}})).as_bytes())
        .unwrap();
    assert!(f.finish().is_err());
}

#[test]
fn validated_client_events_keep_upstream_fields_without_reconstruction() {
    let item = json!({"id":"m","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"answer","annotations":[{"type":"url_citation","url":"https://example.com","title":"source","start_index":0,"end_index":6}],"logprobs":[]}],"upstream_extension":{"version":7}});
    let events = [
        json!({"type":"response.created","sequence_number":41,"response":{"id":"r","object":"response","model":"upstream-model-id","status":"in_progress","output":[],"service_tier":"default"}}),
        json!({"type":"response.output_item.added","sequence_number":42,"output_index":0,"item":item}),
        json!({"type":"response.output_text.delta","sequence_number":43,"item_id":"m","output_index":0,"content_index":0,"delta":"answer","logprobs":[],"extension":"keep"}),
        json!({"type":"response.completed","sequence_number":44,"response":{"id":"r","object":"response","model":"upstream-model-id","status":"completed","output":[item],"service_tier":"default","usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,"input_tokens_details":{"cached_tokens":3,"upstream_counter":8}},"metadata":{"client":"sample"}}}),
    ];
    let mut filter = TextSse::for_client(&request()).unwrap();
    let mut actual = Vec::new();
    for event in &events {
        actual.extend(filter.push(frame(event).as_bytes()).unwrap());
    }
    actual.extend(filter.finish().unwrap());
    let parsed = actual
        .iter()
        .map(|f| {
            serde_json::from_str::<Value>(f.lines().find_map(|l| l.strip_prefix("data: ")).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(parsed, events);
}

#[test]
fn omitted_client_options_are_not_fabricated() {
    let native = json!({"input":[],"instructions":"server-only","tools":[{"name":"local_exec"}],"tool_choice":"required","parallel_tool_calls":true,"model":"a","client_metadata":{"native":"id"}});
    let client =
        json!({"model":"a","input":[{"role":"user","content":"hi"}],"stream":true,"store":false});
    let upstream = client_tools::upstream_request(native, &client);
    for key in [
        "instructions",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
    ] {
        assert!(upstream.get(key).is_none());
    }
    assert_eq!(upstream["client_metadata"], json!({"native":"id"}));
}
