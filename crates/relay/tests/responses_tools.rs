use codex_relay::{
    operations::{ClientAccess, DEFAULT_GROUP, Feature},
    responses::{Request, Store},
};
use serde_json::{Value, json};

fn function(name: &str) -> Value {
    json!({"type":"function","name":name,"parameters":{"type":"object"}})
}
fn namespace(tools: Value) -> Value {
    json!({"type":"namespace","name":"client","description":"Client tools","tools":tools})
}
fn additional(tools: Value) -> Value {
    json!({"type":"additional_tools","role":"developer","tools":tools})
}

#[test]
fn additional_tools_keep_position_schemas_and_register_callbacks() {
    let schema = json!({"type":"object","properties":{"future_option":{"type":"string"}},"required":["future_option"]});
    let request = Request::parse(json!({"model":"m","input":[
        {"role":"user","content":"before"},
        {"type":"additional_tools","id":"tools_1","role":"developer","tools":[
            {"type":"function","name":"echo","parameters":schema,"strict":true,"future_option":true}
        ]},
        {"role":"user","content":"after"}
    ]}))
    .unwrap();
    assert_eq!(request.input[1]["id"], "tools_1");
    assert_eq!(request.input[1]["tools"][0]["parameters"], schema);
    assert_eq!(request.input[1]["tools"][0]["strict"], false);
    assert!(request.input[1]["tools"][0].get("future_option").is_none());
    assert_eq!(request.input[2]["content"][0]["text"], "after");
    assert!(request.has_tool("echo"));
    assert_eq!(request.dynamic_tools()[0]["inputSchema"], schema);
}

#[test]
fn incremental_namespaces_merge_and_latest_definition_wins() {
    let update = json!({"type":"function","name":"echo","parameters":{"type":"object","properties":{"updated":{"type":"boolean"}}}});
    let request = Request::parse(json!({"model":"m","tools":[function("root")],"input":[
        additional(json!([namespace(json!([function("echo"),function("keep")]))])),
        additional(json!([namespace(json!([update,function("new")]))])),
        additional(json!([function("root")])),
        {"role":"user","content":"call"}
    ]}))
    .unwrap();
    assert_eq!(request.tools.len(), 2);
    assert_eq!(request.tools[1]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        request.tools[1]["tools"][0]["parameters"],
        update["parameters"]
    );
    for name in ["echo", "keep", "new"] {
        assert!(request.has_scoped_tool(Some("client"), name));
    }
    assert!(
        request
            .required_features(&request.input)
            .unwrap()
            .contains(&Feature::NamespacedTools)
    );
    assert_eq!(
        request.input[0]["tools"][0]["tools"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn stored_history_restores_embedded_tools_without_inheriting_top_level_tools() {
    let store = Store::default();
    let access = ClientAccess {
        fast_mode: false,
        key_id: uuid::Uuid::new_v4(),
        group_id: DEFAULT_GROUP,
        models: vec![],
    };
    let first = Request::parse(
        json!({"model":"m","tools":[function("one_request_only")],"input":[
            additional(json!([namespace(json!([function("echo")]))])),
            {"role":"user","content":"call"}
        ]}),
    )
    .unwrap();
    store.save(&access, first.input.clone(), &json!({"id":"resp_tools","model":"m","output":[
        {"type":"function_call","name":"echo","namespace":"client","call_id":"call_1","arguments":"{}"}
    ]})).unwrap();
    let next = Request::parse(
        json!({"model":"m","previous_response_id":"resp_tools","input":[
            {"type":"function_call_output","call_id":"call_1","output":"real result"}
        ]}),
    )
    .unwrap();
    let mut history = store.history(&access, "resp_tools").unwrap();
    history.extend(next.input.clone());
    let resolved = next.with_history(&history).unwrap();
    assert!(resolved.has_scoped_tool(Some("client"), "echo"));
    assert!(!resolved.has_tool("one_request_only"));
    assert!(
        resolved
            .required_features(&history)
            .unwrap()
            .contains(&Feature::NamespacedTools)
    );
    assert_eq!(
        resolved.with_history(&history).unwrap().tools,
        resolved.tools
    );
}

#[test]
fn custom_tools_use_native_string_arguments_and_keep_public_definitions() {
    let custom = json!({"type":"custom","name":"exec","description":"Run client code",
        "format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}});
    let request = Request::parse(json!({"model":"m","input":[additional(json!([
        namespace(json!([custom]))
    ])),{"role":"user","content":"run"}]}))
    .unwrap();
    assert!(request.is_custom_tool(Some("client"), "exec"));
    assert!(request.has_scoped_tool(Some("client"), "exec"));
    assert_eq!(request.input[0]["tools"][0]["tools"][0], custom);
    let native = request.dynamic_tools();
    assert_eq!(native[0]["tools"][0]["type"], "function");
    assert_eq!(
        native[0]["tools"][0]["inputSchema"]["properties"]["input"]["type"],
        "string"
    );
    assert!(
        native[0]["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("start: /.+/")
    );
    let disabled =
        Request::parse(json!({"model":"m","tool_choice":"none","input":request.input})).unwrap();
    assert!(disabled.dynamic_tools().is_empty());
    assert!(!disabled.has_scoped_tool(Some("client"), "exec"));
}

#[test]
fn malformed_declarations_and_combined_tool_overflow_fail() {
    for item in [
        json!({"type":"additional_tools","role":"user","tools":[]}),
        json!({"type":"additional_tools","role":"developer"}),
        additional(json!({})),
        additional(json!([function("same"), function("same")])),
        additional(json!([{"type":"custom","name":"x","description":42}])),
        additional(
            json!([{"type":"custom","name":"x","format":{"type":"grammar","syntax":"bad","definition":"x"}}]),
        ),
        additional(json!([{"type":"file_search"}])),
        additional(
            json!([{"type":"namespace","name":"n","description":"n","tools":[{"type":"namespace","name":"nested","tools":[]}]}]),
        ),
    ] {
        assert!(
            Request::parse(json!({"model":"m","input":[item]})).is_err(),
            "{item}"
        );
    }
    let tools: Vec<_> = (0..codex_relay::media::TOOL_COUNT)
        .map(|i| function(&format!("tool_{i}")))
        .collect();
    assert!(
        Request::parse(json!({"model":"m","input":[
            additional(json!(tools)),additional(json!([function("overflow")]))
        ]}))
        .is_err()
    );
}
#[test]
fn deferred_definitions_reach_native_rpc_without_changing_schema_or_order() {
    let request=codex_relay::responses::Request::parse(serde_json::json!({"model":"m","input":"hello","tools":[
        {"type":"function","name":"later","parameters":{"type":"object"},"defer_loading":true},
        {"type":"namespace","name":"mcp__test","description":"MCP","tools":[{"type":"custom","name":"execute","defer_loading":true}]}
    ]})).unwrap();
    let native = request.dynamic_tools();
    assert_eq!(native[0]["tools"][0]["deferLoading"], true);
    assert_eq!(
        native[0]["tools"][0]["inputSchema"],
        serde_json::json!({"type":"object"})
    );
    assert_eq!(native[1]["tools"][0]["deferLoading"], true);
    assert_eq!(request.tools[1]["tools"][0]["defer_loading"], true);
}
