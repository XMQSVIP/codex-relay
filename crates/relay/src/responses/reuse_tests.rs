use super::*;

fn access() -> ClientAccess {
    ClientAccess {
        fast_mode: false,
        key_id: Uuid::new_v4(),
        group_id: Uuid::new_v4(),
        models: vec![],
    }
}

fn request(root_turn: &str) -> Request {
    Request::parse(json!({"model":"m","input":"first","client_metadata":{
        "x-codex-turn-metadata":json!({"root_turn_id":root_turn,"request_kind":"turn"}).to_string()
    }}))
    .unwrap()
}

fn output() -> Value {
    json!({"type":"message","id":"msg_fixture","role":"assistant","status":"completed",
        "phase":"final_answer","content":[{"type":"output_text","text":"answer","annotations":[],"logprobs":[]}]})
}

#[test]
fn resource_reservation_counts_only_new_history_on_continuation() {
    let access = access();
    let r = Request::parse(json!({"model":"m","input":"long history ".repeat(1024)})).unwrap();
    let session = Session::new(
        "native".into(),
        "fixture".into(),
        &access,
        &r,
        &r.input,
        &[output()],
    )
    .unwrap();
    let mut history = r.input.clone();
    history.push(output());
    let next =
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]});
    history.push(next.clone());
    let request = Request::parse(json!({"model":"m","input":history})).unwrap();
    let delta =
        crate::responses::reservation_bytes(None, &access, &request, &history, Some(&session))
            .unwrap();
    assert_eq!(delta, serde_json::to_vec(&next).unwrap().len());
    assert!(
        crate::responses::reservation_bytes(None, &access, &request, &history, None).unwrap()
            > delta + 10_000
    );
}

#[test]
fn logical_threads_isolate_identical_histories_and_unsafe_sessions_cannot_be_claimed() {
    if !crate::native_cache::enabled() {
        return;
    }
    let access = access();
    let make = |metadata: Value| {
        let mut r = Request::parse(json!({"model":"m","input":"first","client_metadata":metadata}))
            .unwrap();
        crate::responses::affinity::apply(&mut r, Default::default());
        r
    };
    let root = make(json!({"thread_id":"root"}));
    let session = Session::new(
        "native-root".into(),
        "fixture".into(),
        &access,
        &root,
        &root.input,
        &[output()],
    )
    .unwrap();
    let mut history = root.input.clone();
    history.extend([
        output(),
        json!({"type":"message","role":"user","content":"next"}),
    ]);
    let next = make(json!({"thread_id":"root"}));
    assert_eq!(
        session.continuation(&access, &next, &history).unwrap(),
        Some(2)
    );
    for metadata in [
        json!({"thread_id":"child","parent_thread_id":"root"}),
        json!({"thread_id":"fork","forked_from_thread_id":"root"}),
        json!({"subagent_kind":"thread_spawn"}),
        json!({"parent_thread_id":"root"}),
    ] {
        let child = make(metadata);
        assert!(
            session
                .continuation(&access, &child, &history)
                .unwrap()
                .is_none()
        );
        assert!(
            matching_sessions(&access, &child, &history, vec![session.clone()])
                .unwrap()
                .is_empty()
        );
    }
    let unsafe_request = make(json!({"subagent_kind":"thread_spawn"}));
    let unsafe_session = Session::new(
        "unsafe".into(),
        "fixture".into(),
        &access,
        &unsafe_request,
        &root.input,
        &[output()],
    )
    .unwrap();
    let anonymous = make(json!({}));
    assert!(
        unsafe_session
            .continuation(&access, &anonymous, &history)
            .unwrap()
            .is_none()
    );
}

#[test]
fn desktop_turn_metadata_is_forwarded_without_invalidating_session() {
    let expected = crate::native_cache::enabled().then_some(2);
    let access = access();
    let previous = request("turn-1");
    let session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &previous,
        &previous.input,
        &[output()],
    )
    .unwrap();
    let mut next = request("turn-2");
    let mut history = previous.input.clone();
    history.extend([
        output(),
        json!({"type":"message","role":"user","content":"next"}),
    ]);
    assert_ne!(previous.client_metadata, next.client_metadata);
    assert_eq!(
        session.continuation(&access, &next, &history).unwrap(),
        expected
    );
    assert_eq!(
        matching_sessions(&access, &next, &history, vec![session.clone()])
            .unwrap()
            .contains(&session),
        expected.is_some()
    );
    let mut params = json!({});
    next.apply_turn(&mut params);
    assert_eq!(
        params["responsesapiClientMetadata"]["root_turn_id"],
        "turn-2"
    );
    next.client_metadata = None;
    assert_eq!(
        session.continuation(&access, &next, &history).unwrap(),
        expected
    );
    next.client_session = Some([1; 32]);
    assert_eq!(
        session.continuation(&access, &next, &history).unwrap(),
        None
    );
}

#[test]
fn desktop_replay_may_omit_message_provenance_but_not_context() {
    let expected = crate::native_cache::enabled().then_some(2);
    let access = access();
    let request = request("turn-1");
    let mut answer = output();
    answer["internal_chat_message_metadata_passthrough"] = json!({
        "turn_id":"native-turn","create_time":123.5,
        "content_item_kinds":["output_text"],"content_item_metadata":[{"source":"model"}]
    });
    let session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &request,
        &request.input,
        &[answer],
    )
    .unwrap();
    let mut history = request.input.clone();
    history.extend([
        output(),
        json!({"type":"message","role":"user","content":"next"}),
    ]);
    assert_eq!(
        session.continuation(&access, &request, &history).unwrap(),
        expected
    );
    assert_eq!(
        matching_sessions(&access, &request, &history, vec![session.clone()])
            .unwrap()
            .contains(&session),
        expected.is_some()
    );
    for (field, value) in [
        ("content", json!([{"type":"output_text","text":"edited"}])),
        ("phase", json!("commentary")),
        ("encrypted_content", json!("changed opaque content")),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"unknown_field":"different"}),
        ),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"cell_id":"another tool execution"}),
        ),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"executed_tool_calls":[{"name":"other"}]}),
        ),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"tool_calls_complete":true}),
        ),
    ] {
        let mut changed = history.clone();
        changed[1][field] = value;
        assert_eq!(
            session.continuation(&access, &request, &changed).unwrap(),
            None,
            "{field}"
        );
    }
    let mut changed = history;
    changed[0]["content"][0]["text"] = json!("edited question");
    assert_eq!(
        session.continuation(&access, &request, &changed).unwrap(),
        None
    );
}

#[test]
fn removing_tool_execution_metadata_or_encrypted_history_still_prevents_reuse() {
    let access = access();
    let request = request("turn-1");
    for extra in [
        json!({"encrypted_content":"opaque-native-output"}),
        json!({"internal_chat_message_metadata_passthrough":{"turn_id":"native","cell_id":"cell"}}),
        json!({"internal_chat_message_metadata_passthrough":{"turn_id":"native","executed_tool_calls":[]}}),
        json!({"internal_chat_message_metadata_passthrough":{"turn_id":"native","tool_calls_complete":false}}),
    ] {
        let mut answer = output();
        answer
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let session = Session::new(
            "t".into(),
            "fixture".into(),
            &access,
            &request,
            &request.input,
            &[answer],
        )
        .unwrap();
        let mut history = request.input.clone();
        history.extend([
            output(),
            json!({"type":"message","role":"user","content":"next"}),
        ]);
        assert_eq!(
            session.continuation(&access, &request, &history).unwrap(),
            None
        );
    }
}

#[test]
fn desktop_reasoning_content_absent_null_and_empty_are_equivalent() {
    let access = access();
    let request = request("turn-1");
    let reasoning =
        json!({"type":"reasoning","id":"rs_fixture","summary":[],"encrypted_content":"opaque"});
    let forms = [None, Some(Value::Null), Some(json!([]))];
    for saved in &forms {
        let mut original = reasoning.clone();
        if let Some(content) = saved {
            original["content"] = content.clone();
        }
        let session = Session::new(
            "t".into(),
            "fixture".into(),
            &access,
            &request,
            &request.input,
            &[original, output()],
        )
        .unwrap();
        for replayed in &forms {
            let mut replay = reasoning.clone();
            if let Some(content) = replayed {
                replay["content"] = content.clone();
            }
            let mut history = request.input.clone();
            history.extend([
                replay,
                output(),
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}),
            ]);
            let expected = (crate::native_cache::enabled() || saved == replayed).then_some(3);
            assert_eq!(
                session.continuation(&access, &request, &history).unwrap(),
                expected,
                "saved={saved:?} replayed={replayed:?}"
            );
            assert_eq!(
                matching_sessions(&access, &request, &history, vec![session.clone()])
                    .unwrap()
                    .contains(&session),
                expected.is_some()
            );
        }
    }
}

#[test]
fn reasoning_replay_cannot_change_or_omit_meaningful_content() {
    let access = access();
    let request = request("turn-1");
    for content in [
        json!([{"type":"reasoning_text","text":"preserve this"}]),
        json!([{"type":"text","text":"preserve this"}]),
    ] {
        let reasoning = json!({"type":"reasoning","id":"rs_fixture","summary":[{"type":"summary_text","text":"summary"}],"content":content,"encrypted_content":"opaque"});
        let session = Session::new(
            "t".into(),
            "fixture".into(),
            &access,
            &request,
            &request.input,
            &[reasoning.clone(), output()],
        )
        .unwrap();
        let mut history = request.input.clone();
        history.extend([
            reasoning,
            output(),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}),
        ]);
        assert_eq!(
            session.continuation(&access, &request, &history).unwrap(),
            Some(3)
        );
        for (field, value) in [
            ("content", Value::Null),
            ("content", json!([])),
            (
                "content",
                json!([{"type":"reasoning_text","text":"edited"}]),
            ),
            ("summary", json!([])),
            ("encrypted_content", json!("different opaque content")),
            ("encrypted_content", Value::Null),
        ] {
            let mut changed = history.clone();
            changed[1][field] = value;
            assert_eq!(
                session.continuation(&access, &request, &changed).unwrap(),
                None,
                "{field}"
            );
        }
        let mut changed = history;
        changed[1].as_object_mut().unwrap().remove("content");
        assert_eq!(
            session.continuation(&access, &request, &changed).unwrap(),
            None
        );
    }
}

#[test]
fn equivalent_native_settings_preserve_reuse_without_mutating_request() {
    let access = access();
    let base = json!({"model":"m","input":"first"});
    let original = Request::parse(base.clone()).unwrap();
    let session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &original,
        &original.input,
        &[output()],
    )
    .unwrap();
    let mut history = original.input.clone();
    history.extend([
        output(),
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}),
    ]);
    let mut original_params = json!({});
    original.apply_thread(&mut original_params);
    original.apply_turn(&mut original_params);
    for (field, value) in [
        ("reasoning", json!({"effort":null,"summary":null})),
        ("text", json!({"verbosity":null})),
        ("instructions", json!("")),
        ("service_tier", json!("auto")),
    ] {
        let mut body = base.clone();
        body[field] = value;
        let request = Request::parse(body).unwrap();
        let mut params = json!({});
        request.apply_thread(&mut params);
        request.apply_turn(&mut params);
        assert_eq!(
            params, original_params,
            "native settings must be equivalent: {field}"
        );
        if crate::native_cache::enabled() {
            let context = crate::execution::RequestContext::default();
            assert_eq!(
                super::super::affinity::routing(&context, &original, &original.input),
                super::super::affinity::routing(&context, &request, &request.input),
                "fallback account affinity must also remain stable: {field}"
            );
        }
        let expected = crate::native_cache::enabled().then_some(2);
        assert_eq!(
            session.continuation(&access, &request, &history).unwrap(),
            expected,
            "{field}"
        );
        assert_eq!(
            matching_sessions(&access, &request, &history, vec![session.clone()])
                .unwrap()
                .contains(&session),
            expected.is_some(),
            "{field}"
        );
        let mut after = json!({});
        request.apply_thread(&mut after);
        request.apply_turn(&mut after);
        assert_eq!(
            params, after,
            "fingerprinting must not mutate RPC parameters"
        );
    }
    for (field, value) in [
        ("reasoning", json!({"effort":"low"})),
        ("reasoning", json!({"summary":"auto"})),
        ("text", json!({"verbosity":"low"})),
        ("instructions", json!("different")),
        ("service_tier", json!("default")),
        ("service_tier", json!("priority")),
        ("model", json!("different")),
    ] {
        let mutable =
            crate::native_cache::enabled() && matches!(field, "service_tier" | "reasoning");
        let mut body = base.clone();
        body[field] = value;
        let request = Request::parse(body).unwrap();
        assert_eq!(
            session.continuation(&access, &request, &history).unwrap(),
            mutable.then_some(2),
            "{field}"
        );
    }
}

#[test]
fn settings_normalization_preserves_nulls_inside_schemas() {
    for field in ["text", "tools"] {
        let schema = json!({"type":"object","properties":{"value":{"default":null}}});
        let mut body = json!({"model":"m","input":"first"});
        body[field] = if field == "text" {
            json!({"format":{"type":"json_schema","name":"result","schema":schema}})
        } else {
            json!([{"type":"function","name":"echo","parameters":schema}])
        };
        let original = Request::parse(body.clone()).unwrap();
        let property = if field == "text" {
            &mut body["text"]["format"]["schema"]["properties"]["value"]
        } else {
            &mut body["tools"][0]["parameters"]["properties"]["value"]
        };
        property.as_object_mut().unwrap().remove("default");
        let changed = Request::parse(body).unwrap();
        assert_ne!(settings(&original).unwrap(), settings(&changed).unwrap());
    }
}
