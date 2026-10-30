use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, ClientAccess, DEFAULT_GROUP},
    responses::{self, Store},
};
use serde_json::{Value, json};
use tower::ServiceExt;
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let result = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (
        result.status(),
        serde_json::from_slice(&to_bytes(result.into_body(), 1024 * 1024).await.unwrap()).unwrap(),
    )
}
#[tokio::test]
async fn responses_auth_validation_and_storage_are_scoped_before_native_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let admin = "a".repeat(64);
    let state = AdminState::new(4317, dir.path().join("accounts"), admin.clone())
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "caller".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["allowed".into()],
            expires_in_days: None,
            max_concurrency: 1,
        })
        .unwrap()
        .secret;
    let app = router(state.clone(), dir.path().join("web"));
    let basic = json!({"model":"allowed","input":"hello"});
    assert_eq!(
        call(&app, "POST", "/v1/responses", &admin, basic.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/responses",
            &key,
            json!({"model":"forbidden","input":"hello"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for field in [
        json!({"temperature":0.7}),
        json!({"top_p":0.9}),
        json!({"tool_choice":"required"}),
        json!({"parallel_tool_calls":false}),
        json!({"background":true}),
        json!({"truncation":"auto"}),
        json!({"include":["message.output_text.logprobs"]}),
        json!({"stream_options":{"future_option":true}}),
        json!({"future_parameter":{"enabled":true}}),
    ] {
        let mut req = basic.clone();
        req.as_object_mut()
            .unwrap()
            .extend(field.as_object().unwrap().clone());
        let (status, error) = call(&app, "POST", "/v1/responses", &key, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error"]["code"], "INFERENCE_DISABLED");
    }
    // An ignored limit must not reject either response mode before routing.
    // This fixture has no route, so successful parsing reaches INFERENCE_DISABLED.
    for stream in [false, true] {
        let (status, error) = call(
            &app,
            "POST",
            "/v1/responses",
            &key,
            json!({
                "model":"allowed","stream":stream,"input":[
                    {"type":"compaction","encrypted_content":"imported-opaque-history"},
                    {"role":"user","content":"continue"}
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error"]["code"], "INFERENCE_DISABLED");
        let (status,error) = call(&app,"POST","/v1/responses",&key,json!({"model":"allowed","stream":stream,"input":[
            {"type":"additional_tools","role":"developer","tools":[{"type":"custom","name":"exec"}]},
            {"role":"user","content":"hello"}
        ]})).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error"]["code"], "INFERENCE_DISABLED");
        for limit in [Value::Null, json!(1), json!(4096)] {
            let (status, error) = call(
                &app,
                "POST",
                "/v1/responses",
                &key,
                json!({"model":"allowed","input":"hello","stream":stream,"max_output_tokens":limit}),
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error["error"]["code"], "INFERENCE_DISABLED");
        }
    }
    let (status, _) = call(
        &app,
        "POST",
        "/v1/responses",
        &key,
        json!({"model":"allowed","previous_response_id":"resp_missing","input":"hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status,_)=call(&app,"POST","/v1/responses",&key,json!({"model":"allowed","input":[{"type":"function_call_output","call_id":"unknown","output":"x"}]})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let access = state.operations.authenticate_api_key(&key).unwrap();
    assert!(
        state
            .responses
            .save(
                &access,
                vec![json!({"role":"user","content":"private"})],
                &json!({"id":"resp_test","model":"allowed","output":[]})
            )
            .is_err()
    );
    for method in ["GET", "DELETE"] {
        assert_eq!(
            call(&app, method, "/v1/responses/resp_test", &key, Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    // All valid store values reach routing, including true (ignored).
    for options in [
        json!({}),
        json!({"store":null}),
        json!({"store":false}),
        json!({"store":true}),
    ] {
        let mut request = basic.clone();
        request
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let (status, error) = call(&app, "POST", "/v1/responses", &key, request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error"]["code"], "INFERENCE_DISABLED");
    }
    assert_eq!(state.operations.snapshot()["active"], 0);
    let oversized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model":"allowed","input":"x".repeat(codex_relay::media::BODY_LIMIT)})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value =
        serde_json::from_slice(&to_bytes(oversized.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(state.profiles.list().await.is_empty());
}
#[test]
fn store_options_never_enable_body_storage() {
    for options in [
        json!({}),
        json!({"store":null}),
        json!({"store":false}),
        json!({"store":true}),
    ] {
        let mut request = json!({"model":"m","input":"private"});
        request
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let parsed = responses::Request::parse(request).unwrap();
        assert!(!parsed.store);
        assert_eq!(
            parsed
                .parameter_handling
                .iter()
                .any(|n| n["path"] == "store" && n["action"] == "ignored"),
            options["store"] == true
        );
    }
    assert!(
        responses::Request::parse(json!({"model":"m","input":"private","store":"yes"})).is_err()
    );
}

#[test]
fn response_streaming_requires_explicit_true() {
    for (options, expected) in [
        (json!({}), false),
        (json!({"stream": null}), false),
        (json!({"stream": false}), false),
        (json!({"stream": true}), true),
    ] {
        let mut request = json!({"model":"m","input":"hello"});
        request
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let parsed = responses::Request::parse(request).unwrap();
        assert_eq!(parsed.stream, expected, "options: {options}");
    }
}

#[test]
fn response_history_is_isolated_and_does_not_inherit_instructions() {
    let store = Store::default();
    let a = ClientAccess {
        fast_mode: false,
        key_id: uuid::Uuid::new_v4(),
        group_id: DEFAULT_GROUP,
        models: vec![],
    };
    let mut b = a.clone();
    b.key_id = uuid::Uuid::new_v4();
    let output = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]});
    store
        .save(
            &a,
            vec![json!({"role":"user","content":"question"})],
            &json!({"id":"resp_a","model":"m","instructions":"do not carry","output":[output]}),
        )
        .unwrap();
    assert!(store.get(&b, "resp_a").is_err());
    assert!(store.history(&b, "resp_a").is_err());
    assert!(store.delete(&b, "resp_a").is_err());
    let history = store.history(&a, "resp_a").unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("do not carry")
    );
    b = a.clone();
    b.group_id = uuid::Uuid::new_v4();
    assert!(store.get(&b, "resp_a").is_ok());
    assert_eq!(store.history(&b, "resp_a").unwrap(), history);
    b = a.clone();
    b.models = vec!["different".into()];
    assert!(store.get(&b, "resp_a").is_err());
}
#[test]
fn standard_request_preserves_roles_and_validates_unsupported_features() {
    let r=responses::Request::parse(json!({"model":"m","input":[{"role":"developer","content":"instructions"},{"role":"assistant","content":"answer"},{"role":"user","content":[{"type":"input_text","text":"next"}]}],"text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object","properties":{"ok":{"type":"boolean"}}}}},"reasoning":{"effort":"high","summary":"auto"}})).unwrap();
    assert_eq!(r.input[1]["content"][0]["type"], "output_text");
    let mut turn = json!({});
    r.apply_turn(&mut turn);
    assert_eq!(turn["effort"], "high");
    assert_eq!(turn["outputSchema"]["type"], "object");
    assert!(responses::Request::parse(json!({"model":"m","input":[{"role":"user","content":[{"type":"input_image","image_url":"http://internal"}]}]})).is_err());
    let r = responses::Request::parse(
        json!({"model":"m","input":"test","tools":[{"type":"function","name":"f","strict":true}]}),
    )
    .unwrap();
    assert_eq!(r.tools[0]["strict"], false);
}

#[test]
fn unsupported_options_are_removed_without_changing_input_or_schemas() {
    let schema = json!({"type":"object","properties":{"temperature":{"type":"number"}},"additionalProperties":false});
    let r = responses::Request::parse(json!({
        "model":"m","input":"KEEP_INPUT","instructions":"KEEP_INSTRUCTIONS",
        "temperature":0.5,"top_p":0.8,"max_output_tokens":1,"background":true,
        "parallel_tool_calls":false,"truncation":"auto","prompt":{"id":"ignored"},
        "tool_choice":{"type":"function","name":"echo"},"future_option":123,
        "reasoning":{"effort":"high","future_option":true},
        "text":{"verbosity":"low","future_option":true,"format":{
            "type":"json_schema","name":"answer","schema":schema,"future_option":true
        }},
        "tools":[{"type":"namespace","name":"client","description":"test","future_option":true,
            "tools":[{"type":"function","name":"echo","parameters":schema,"strict":true,"future_option":true}]}]
    })).unwrap();
    assert_eq!(r.tool_choice, "auto");
    assert_eq!(r.input[0]["content"][0]["text"], "KEEP_INPUT");
    assert_eq!(r.instructions.as_deref(), Some("KEEP_INSTRUCTIONS"));
    assert_eq!(r.reasoning, json!({"effort":"high"}));
    assert!(r.text.get("future_option").is_none());
    assert!(r.text["format"].get("future_option").is_none());
    assert_eq!(r.text["format"]["schema"], schema);
    assert_eq!(r.tools[0]["tools"][0]["parameters"], schema);
    assert_eq!(r.tools[0]["tools"][0]["strict"], false);
    let mut turn = json!({});
    r.apply_turn(&mut turn);
    assert_eq!(turn, json!({"effort":"high","outputSchema":schema}));
    let r = responses::Request::parse(json!({"model":"m","input":"test","tool_choice":"none",
        "tools":[{"type":"function","name":"echo","strict":true}]}))
    .unwrap();
    assert!(r.dynamic_tools().is_empty());
}

#[test]
fn compatibility_does_not_skip_required_fields_or_supported_value_validation() {
    for request in [
        json!({"input":"test","temperature":0.5}),
        json!({"model":"m","background":true}),
        json!({"model":"m","input":"test","stream":"yes"}),
        json!({"model":"m","input":"test","text":{"verbosity":"invalid"}}),
        json!({"model":"m","input":"test","reasoning":{"effort":"invalid"}}),
        json!({"model":"m","input":"test","tools":[{"type":"function","parameters":{}}]}),
        json!({"model":"m","input":"test","tools":[{"type":"custom","name":"x","format":{"type":"unknown"}}]}),
        json!({"model":"m","input":[{"role":"user","content":[{"type":"input_file","file_id":"file_x"}]}]}),
    ] {
        assert!(responses::Request::parse(request).is_err());
    }
}

#[test]
fn response_verbosity_and_service_tier_reach_native_parameters() {
    for verbosity in ["low", "medium", "high"] {
        for tier in ["auto", "default", "flex", "priority", "ultrafast"] {
            let r = responses::Request::parse(json!({
                "model":"m", "input":"hello", "text":{"verbosity":verbosity},
                "service_tier":tier
            }))
            .unwrap();
            assert_eq!(r.text["format"], json!({"type":"text"}));
            let mut thread = json!({"config":{"existing":"preserved"}});
            r.apply_thread(&mut thread);
            assert_eq!(thread["config"]["model_verbosity"], verbosity);
            assert_eq!(thread["config"]["existing"], "preserved");
            let mut turn = json!({});
            r.apply_turn(&mut turn);
            if tier == "auto" {
                assert!(turn.get("serviceTier").is_none());
                assert!(turn.get("serviceTierForTurn").is_none());
            } else {
                let field = if std::env::var("BRIDGE_NATIVE_CACHE_REUSE").as_deref() == Ok("0") {
                    "serviceTier"
                } else {
                    "serviceTierForTurn"
                };
                assert_eq!(turn[field], tier);
            }
        }
    }
    for field in [
        json!({"text":{"verbosity":"invalid"}}),
        json!({"text":{"verbosity":1}}),
        json!({"service_tier":"invalid"}),
        json!({"service_tier":true}),
        json!({"text":{"format":"text"}}),
    ] {
        let mut request = json!({"model":"m","input":"hello"});
        request
            .as_object_mut()
            .unwrap()
            .extend(field.as_object().unwrap().clone());
        assert!(responses::Request::parse(request).is_err());
    }
}

#[test]
fn json_mode_preserves_instructions_and_tool_outputs_support_content_arrays() {
    let r = responses::Request::parse(json!({"model":"m","input":"hello",
        "instructions":"caller instructions", "text":{"format":{"type":"json_object"}}
    }))
    .unwrap();
    let mut params = json!({"developerInstructions":"caller instructions","config":{}});
    r.apply_thread(&mut params);
    assert_eq!(params["baseInstructions"], "caller instructions");
    assert!(
        params["developerInstructions"]
            .as_str()
            .unwrap()
            .contains("valid JSON object")
    );
    assert_eq!(r.instructions.as_deref(), Some("caller instructions"));
    let mut turn = json!({});
    r.apply_turn(&mut turn);
    assert!(
        turn.get("outputSchema").is_none(),
        "JSON mode must not pretend an open schema is supported as strict JSON Schema"
    );
    for output in [
        json!("text"),
        json!([]),
        json!([{"type":"input_text","text":"result"}]),
    ] {
        assert!(
            responses::Request::parse(json!({"model":"m","input":[
                {"type":"function_call_output","call_id":"call","output":output}
            ]}))
            .is_ok()
        );
    }
    for output in [
        json!(42),
        json!({}),
        json!([{"type":"input_text","text":false}]),
        json!([{"type":"input_image","image_url":"http://localhost/private"}]),
        json!([{"type":"input_file","file_id":"file_123"}]),
    ] {
        assert!(
            responses::Request::parse(json!({"model":"m","input":[
                {"type":"function_call_output","call_id":"call","output":output}
            ]}))
            .is_err()
        );
    }
}
