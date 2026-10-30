use super::*;

fn access() -> ClientAccess {
    ClientAccess {
        fast_mode: false,
        key_id: Uuid::new_v4(),
        group_id: Uuid::new_v4(),
        models: vec![],
    }
}
fn answer() -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]})
}

#[test]
fn model_switch_requires_explicit_effort_and_a_completed_owned_turn() {
    if !crate::native_cache::enabled() {
        return;
    }
    let owner = access();
    let first =
        Request::parse(json!({"model":"first","input":"hello","reasoning":{"effort":"high"}}))
            .unwrap();
    let mut session = Session::new(
        "thread".into(),
        "directory".into(),
        &owner,
        &first,
        &first.input,
        &[answer()],
    )
    .unwrap();
    session.default_effort = Some("medium".into());
    let mut history = first.input.clone();
    history.extend([answer(), json!({"role":"user","content":"next"})]);
    let changed =
        Request::parse(json!({"model":"second","input":history,"reasoning":{"effort":"low"}}))
            .unwrap();
    assert_eq!(
        session
            .continuation(&owner, &changed, &changed.input)
            .unwrap(),
        Some(2)
    );
    assert!(session.model_changed(&changed));
    let defaults = Request::parse(json!({"model":"second","input":history})).unwrap();
    assert!(
        session
            .continuation(&owner, &defaults, &defaults.input)
            .unwrap()
            .is_none()
    );
    assert!(
        session
            .continuation(&access(), &changed, &changed.input)
            .unwrap()
            .is_none()
    );
    session.calls.push(super::super::continuation::Call {
        id: "call".into(),
        rpc_id: "rpc".into(),
        name: "tool".into(),
        namespace: None,
        custom: false,
        arguments: [0; 32],
    });
    assert!(!session.matches_settings(
        &changed,
        settings(&changed).unwrap(),
        thread_settings(&changed).unwrap()
    ));
}

#[test]
fn all_turns_uses_latest_configuration_for_admission_and_native_turn() {
    let owner = access();
    let store = super::super::Store::default();
    let default_request = Request::parse(json!({"model":"m","input":"hello"})).unwrap();
    assert_eq!(
        default_request
            .with_history(&default_request.input)
            .unwrap()
            .reasoning,
        json!({})
    );
    let history = json!([
        {"role":"user","content":"synthetic prompt"},
        {"type":"configuration_update","reasoning":{"effort":"low"}},
        {"type":"configuration_update","reasoning":{"effort":"medium"}},
        {"type":"configuration_update","reasoning":{"effort":"high"}}
    ]);
    for (reasoning, expected) in [
        (
            json!({"effort":"low","context":"all_turns","summary":"detailed"}),
            "high",
        ),
        (json!({"effort":"low"}), "low"),
        (json!({"effort":"low","context":"unknown"}), "low"),
        (json!({}), "high"),
    ] {
        let body = json!({"model":"m","input":history,"reasoning":reasoning});
        let parsed = Request::parse(body.clone()).unwrap();
        let resolved = parsed.with_history(&parsed.input).unwrap();
        let mut params = json!({});
        resolved.apply_turn(&mut params);
        assert_eq!(params["effort"], expected);
        assert_eq!(params["summary"], reasoning["summary"]);
        assert!(params.get("context").is_none());
        assert_eq!(
            super::super::admission_reasoning_effort(&store, &owner, &body).as_deref(),
            Some(expected)
        );
        assert_eq!(resolved.input, parsed.input);
    }
    let body =
        json!({"model":"m","input":"next","reasoning":{"effort":"low","context":"all_turns"}});
    let parsed = Request::parse(body.clone()).unwrap();
    assert_eq!(
        parsed.with_history(&parsed.input).unwrap().reasoning["effort"],
        "low"
    );
    assert_eq!(
        super::super::admission_reasoning_effort(&store, &owner, &body).as_deref(),
        Some("low")
    );

    store
        .save(
            &owner,
            history.as_array().unwrap().clone(),
            &json!({"id":"prior","model":"m","output":[]}),
        )
        .unwrap();
    let mut next = body;
    next["previous_response_id"] = json!("prior");
    let parsed = Request::parse(next.clone()).unwrap();
    let mut combined = store.continuation(&owner, "prior").unwrap().history;
    combined.extend(parsed.input.clone());
    assert_eq!(
        parsed.with_history(&combined).unwrap().reasoning["effort"],
        "high"
    );
    assert_eq!(
        super::super::admission_reasoning_effort(&store, &owner, &next).as_deref(),
        Some("high")
    );
    assert_eq!(
        super::super::admission_reasoning_effort(&store, &access(), &next).as_deref(),
        Some("low")
    );
    next["input"] = json!([{"type":"configuration_update","reasoning":{"effort":"medium"}}]);
    assert_eq!(
        super::super::admission_reasoning_effort(&store, &owner, &next).as_deref(),
        Some("medium")
    );
    next["input"][0]["reasoning"]["effort"] = json!("invalid");
    assert!(Request::parse(next).is_err());
}

#[test]
fn completed_turns_accept_schema_changes_and_appended_instructions() {
    if !crate::native_cache::enabled() {
        return;
    }
    let owner = access();
    let original = Request::parse(json!({"model":"m","input":"first"})).unwrap();
    let session = Session::new(
        "t".into(),
        "d".into(),
        &owner,
        &original,
        &original.input,
        &[answer()],
    )
    .unwrap();
    let mut history = original.input.clone();
    history.extend([
        answer(),
        json!({"role":"developer","content":"Use the new schema."}),
        json!({"role":"system","content":"Additional client instruction."}),
        json!({"role":"user","content":"next"}),
    ]);
    for format in [
        json!({"type":"text"}),
        json!({"type":"json_schema","name":"first","strict":true,
        "schema":{"type":"object","properties":{"x":{"type":"string"}},"required":["x"],"additionalProperties":false}}),
    ] {
        let next =
            Request::parse(json!({"model":"m","input":history,"text":{"format":format}})).unwrap();
        assert_eq!(
            session.continuation(&owner, &next, &next.input).unwrap(),
            Some(2)
        );
        let mut params = json!({});
        next.apply_turn(&mut params);
        assert_eq!(
            params["outputSchema"],
            format.get("schema").cloned().unwrap_or(Value::Null)
        );
        let mut completed = Session::new(
            "t".into(),
            "d".into(),
            &owner,
            &next,
            &next.input,
            &[answer()],
        )
        .unwrap();
        let mut plain_history = next.input.clone();
        plain_history.extend([answer(), json!({"role":"user","content":"plain next"})]);
        let plain = Request::parse(json!({"model":"m","input":plain_history})).unwrap();
        assert!(
            completed
                .continuation(&owner, &plain, &plain.input)
                .unwrap()
                .is_some()
        );
        // A pending callback still requires exact per-turn settings.
        completed.calls.push(super::super::continuation::Call {
            id: "c".into(),
            rpc_id: "1".into(),
            name: "echo".into(),
            namespace: None,
            custom: false,
            arguments: [0; 32],
        });
        assert!(
            !completed.matches_settings(
                &plain,
                settings(&plain).unwrap(),
                thread_settings(&plain).unwrap()
            ) || format["type"] == "text"
        );
    }
    let json_mode = Request::parse(
        json!({"model":"m","input":history,"text":{"format":{"type":"json_object"}}}),
    )
    .unwrap();
    assert!(
        session
            .continuation(&owner, &json_mode, &json_mode.input)
            .unwrap()
            .is_none()
    );
    let mut edited = original.input.clone();
    edited[0]["content"][0]["text"] = json!("edited");
    edited.extend([answer(), json!({"role":"user","content":"next"})]);
    let edited = Request::parse(json!({"model":"m","input":edited})).unwrap();
    assert!(
        session
            .continuation(&owner, &edited, &edited.input)
            .unwrap()
            .is_none()
    );
}

#[test]
fn completed_turn_changes_preserve_affinity_and_restore_known_defaults() {
    if !crate::native_cache::enabled() {
        return;
    }
    let access = access();
    let original = Request::parse(
        json!({"model":"m","input":"first","reasoning":{"effort":"low"},"service_tier":"priority"}),
    )
    .unwrap();
    let mut session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &original,
        &original.input,
        &[answer()],
    )
    .unwrap();
    let mut history = original.input.clone();
    history.extend([
        answer(),
        json!({"type":"message","role":"user","content":"next"}),
    ]);
    let context = crate::execution::RequestContext::default();
    for effort in [json!("low"), json!("high"), Value::Null] {
        for tier in [
            json!("priority"),
            json!("default"),
            json!("auto"),
            Value::Null,
        ] {
            let next = Request::parse(json!({"model":"m","input":history,"reasoning":{"effort":effort},"service_tier":tier})).unwrap();
            session.default_effort = None;
            let expected = effort.is_string().then_some(2);
            assert_eq!(
                session.continuation(&access, &next, &next.input).unwrap(),
                expected
            );
            session.default_effort = Some("medium".into());
            assert_eq!(
                session.continuation(&access, &next, &next.input).unwrap(),
                Some(2)
            );
            assert!(
                matching_sessions(&access, &next, &next.input, vec![session.clone()])
                    .unwrap()
                    .contains(&session)
            );
            assert_eq!(
                super::super::affinity::routing(&context, &original, &original.input),
                super::super::affinity::routing(&context, &next, &next.input)
            );
            let mut params = json!({});
            next.apply_turn(&mut params);
            session.apply_turn_defaults(&next, &mut params);
            assert_eq!(
                params["effort"],
                if effort.is_null() {
                    json!("medium")
                } else {
                    effort.clone()
                }
            );
            assert!(params.get("serviceTier").is_none());
            assert_eq!(
                params["serviceTierForTurn"],
                if tier == "auto" {
                    Value::Null
                } else {
                    tier.clone()
                }
            );
        }
    }
}

#[test]
fn summaries_restore_the_configured_default_and_unknown_defaults_rebuild() {
    if !crate::native_cache::enabled() {
        return;
    }
    assert_eq!(default_summary(&json!({})).as_deref(), Some("auto"));
    assert_eq!(
        default_summary(&json!({"model_reasoning_summary":"none"})).as_deref(),
        Some("none")
    );
    assert_eq!(
        default_summary(&json!({"model_reasoning_summary":"future-unknown"})),
        None
    );
    let owner = access();
    let original =
        Request::parse(json!({"model":"m","input":"first","reasoning":{"summary":"detailed"}}))
            .unwrap();
    let mut session = Session::new(
        "t".into(),
        "d".into(),
        &owner,
        &original,
        &original.input,
        &[answer()],
    )
    .unwrap();
    let mut history = original.input.clone();
    history.extend([answer(), json!({"role":"user","content":"next"})]);
    for summary in [json!("concise"), json!("none"), Value::Null] {
        let next =
            Request::parse(json!({"model":"m","input":history,"reasoning":{"summary":summary}}))
                .unwrap();
        session.default_summary = None;
        assert_eq!(
            session
                .continuation(&owner, &next, &next.input)
                .unwrap()
                .is_some(),
            !summary.is_null()
        );
        session.default_summary = Some("concise".into());
        assert!(
            session
                .continuation(&owner, &next, &next.input)
                .unwrap()
                .is_some()
        );
        let mut params = json!({});
        next.apply_turn(&mut params);
        session.apply_turn_defaults(&next, &mut params);
        assert_eq!(
            params["summary"],
            if summary.is_null() {
                json!("concise")
            } else {
                summary
            }
        );
    }
}

#[test]
fn defaults_after_live_reset_do_not_inherit_the_first_tool_requests_overrides() {
    if !crate::native_cache::enabled() {
        return;
    }
    // The last tool-result request omitted overrides, but live settings did not
    // clear the thread defaults installed by the original turn/start.
    let request = Request::parse(json!({"model":"m","input":"next"})).unwrap();
    let mut session = Session::new(
        "t".into(),
        "d".into(),
        &access(),
        &request,
        &request.input,
        &[answer()],
    )
    .unwrap();
    session.default_effort = Some("medium".into());
    session.default_summary = Some("concise".into());
    let mut params = json!({});
    session.apply_turn_defaults(&request, &mut params);
    assert_eq!(params, json!({"effort":"medium","summary":"concise"}));
}

#[test]
fn pending_tools_allow_only_verified_live_overrides() {
    let access = access();
    let original = Request::parse(
        json!({"model":"m","input":"first","reasoning":{"effort":"low"},"service_tier":"default"}),
    )
    .unwrap();
    let call = json!({"type":"function_call","call_id":"c","name":"echo","arguments":"{}"});
    let mut session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &original,
        &original.input,
        std::slice::from_ref(&call),
    )
    .unwrap();
    session.calls.push(super::super::continuation::Call {
        id: "c".into(),
        rpc_id: "1".into(),
        name: "echo".into(),
        namespace: None,
        custom: false,
        arguments: [0; 32],
    });
    let mut history = original.input.clone();
    history.extend([
        call,
        json!({"type":"function_call_output","call_id":"c","output":"result"}),
    ]);
    for (effort, tier, matches) in [
        ("low", "default", true),
        ("high", "default", true),
        ("low", "priority", true),
    ] {
        let matches = crate::native_cache::enabled() && matches;
        let next = Request::parse(
            json!({"model":"m","input":history,"reasoning":{"effort":effort},"service_tier":tier}),
        )
        .unwrap();
        assert_eq!(
            session.continuation(&access, &next, &next.input).unwrap(),
            matches.then_some(2)
        );
        assert_eq!(
            matching_sessions(&access, &next, &next.input, vec![session.clone()])
                .unwrap()
                .contains(&session),
            matches
        );
    }
    let reset = Request::parse(json!({"model":"m","input":history,"service_tier":"auto"})).unwrap();
    assert!(
        session
            .continuation(&access, &reset, &reset.input)
            .unwrap()
            .is_none()
    );
    session.default_effort = Some("medium".into());
    session.default_service_tier = Some(Some("priority".into()));
    assert_eq!(
        session
            .continuation(&access, &reset, &reset.input)
            .unwrap()
            .is_some(),
        crate::native_cache::enabled()
    );
    assert_eq!(
        session.pending_update(&reset).unwrap(),
        json!({"effort":"medium","serviceTier":"priority"})
    );
    let changed_schema = Request::parse(
        json!({"model":"m","input":history,"reasoning":{"effort":"low"},"service_tier":"default",
        "text":{"format":{"type":"json_schema","name":"x","schema":{"type":"object"}}}}),
    )
    .unwrap();
    assert!(
        session
            .continuation(&access, &changed_schema, &changed_schema.input)
            .unwrap()
            .is_none()
    );
}

#[test]
fn new_effort_control_can_append_but_cannot_rewrite_history_or_override_explicit_effort() {
    if !crate::native_cache::enabled() {
        return;
    }
    let access = access();
    let original =
        Request::parse(json!({"model":"m","input":"first","reasoning":{"effort":"low"}})).unwrap();
    let session = Session::new(
        "t".into(),
        "fixture".into(),
        &access,
        &original,
        &original.input,
        &[answer()],
    )
    .unwrap();
    let mut history = original.input.clone();
    history.extend([
        answer(),
        json!({"type":"configuration_update","reasoning":{"effort":"high"}}),
        json!({"type":"message","role":"user","content":"next"}),
    ]);
    for explicit in [Value::Null, json!("medium")] {
        let parsed =
            Request::parse(json!({"model":"m","input":history,"reasoning":{"effort":explicit}}))
                .unwrap();
        let next = parsed.with_history(&parsed.input).unwrap();
        assert_eq!(
            next.reasoning["effort"],
            if explicit.is_null() {
                json!("high")
            } else {
                explicit
            }
        );
        assert_eq!(
            session.continuation(&access, &next, &next.input).unwrap(),
            Some(2)
        );
        let mut changed = next.input.clone();
        changed[0]["content"][0]["text"] = json!("edited");
        assert_eq!(
            session.continuation(&access, &next, &changed).unwrap(),
            None
        );
        assert_eq!(
            session
                .continuation(&access, &next, &next.input[..3])
                .unwrap(),
            None
        );
        changed = next.input.clone();
        changed[3]["role"] = json!("developer");
        assert_eq!(
            session.continuation(&access, &next, &changed).unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn completed_native_turns_apply_overrides_and_defaults_without_restarting_thread() {
    if !crate::native_cache::enabled() {
        return;
    }
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (client, peer) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let rpc = crate::rpc::Rpc::from_io(read, write);
    let (done, finished) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let mut starts = 0;
        let mut turns = 0;
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            let mut out = vec![];
            match frame["method"].as_str() {
                Some("thread/start") => {
                    starts += 1;
                    out.push(json!({"id":frame["id"],"result":{"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted","reasoningEffort":"medium"}}));
                }
                Some("thread/inject_items") => out.push(json!({"id":frame["id"],"result":{}})),
                Some("turn/start") => {
                    let expected = [
                        ("low", json!("priority")),
                        ("high", json!("default")),
                        ("medium", Value::Null),
                        ("high", Value::Null),
                    ];
                    assert_eq!(frame["params"]["effort"], expected[turns].0);
                    assert_eq!(frame["params"]["serviceTierForTurn"], expected[turns].1);
                    assert!(frame["params"].get("serviceTier").is_none());
                    turns += 1;
                    out.push(json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}}));
                    out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":answer()}}));
                    out.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}));
                }
                other => panic!("unexpected native call: {other:?}"),
            }
            for event in out {
                write
                    .write_all(format!("{event}\n").as_bytes())
                    .await
                    .unwrap();
            }
            if turns == 4 {
                let _ = finished.await;
                return (starts, turns);
            }
        }
        panic!("native stream closed before all turns")
    });
    let access = access();
    let directory = tempfile::tempdir().unwrap();
    let provider = crate::provider::Provider {
        execution: crate::provider::Execution::Native,
        id: "p".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1/v1".into(),
        auth: "api-key".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    let mut session = None;
    let mut history = vec![];
    for (effort, tier, all_turns) in [
        (json!("low"), json!("priority"), false),
        (json!("high"), json!("default"), false),
        (Value::Null, json!("auto"), false),
        (json!("low"), json!("auto"), true),
    ] {
        history.push(json!({"type":"message","role":"user","content":"next"}));
        if all_turns {
            history.push(json!({"type":"configuration_update","reasoning":{"effort":"medium"}}));
            history.push(json!({"type":"configuration_update","reasoning":{"effort":"high"}}));
        }
        let mut body =
            json!({"model":"m","input":history,"reasoning":{"effort":effort},"service_tier":tier});
        if all_turns {
            body["reasoning"]["context"] = json!("all_turns");
        }
        let parsed = Request::parse(body).unwrap();
        let request = parsed.with_history(&parsed.input).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(128);
        let summary = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::run_profiled(
                &rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                tx,
                tokio_util::sync::CancellationToken::new(),
                None,
                Some(Reuse {
                    default_summary: Some("auto".into()),
                    access: &access,
                    session: &mut session,
                    continuation: None,
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(summary.cleanup_confirmed);
        assert_eq!(
            session.as_ref().unwrap().default_effort.as_deref(),
            Some("medium")
        );
        while let Some(event) = rx.recv().await {
            if event["type"] == "response.completed" {
                history.extend(event["response"]["output"].as_array().unwrap().clone());
            }
        }
    }
    done.send(()).unwrap();
    assert_eq!(server.await.unwrap(), (1, 4));
    rpc.close();
}

#[test]
fn configuration_update_keeps_pending_tool_continuation() {
    if !crate::native_cache::enabled() {
        return;
    }
    let owner = access();
    let original =
        Request::parse(json!({"model":"m","input":"first","reasoning":{"effort":"low"}})).unwrap();
    let call = json!({"type":"function_call","call_id":"c","name":"echo","arguments":"{}"});
    let mut session = Session::new(
        "t".into(),
        "fixture".into(),
        &owner,
        &original,
        &original.input,
        std::slice::from_ref(&call),
    )
    .unwrap();
    session.calls.push(super::super::continuation::Call {
        id: "c".into(),
        rpc_id: "1".into(),
        name: "echo".into(),
        namespace: None,
        custom: false,
        arguments: [0; 32],
    });
    let mut history = original.input.clone();
    history.extend([
        call,
        json!({"type":"function_call_output","call_id":"c","output":"result"}),
    ]);
    let baseline =
        Request::parse(json!({"model":"m","input":history,"reasoning":{"effort":"high"}})).unwrap();
    assert_eq!(
        session
            .continuation(&owner, &baseline, &baseline.input)
            .unwrap(),
        Some(2)
    );
    history.push(json!({"type":"configuration_update","reasoning":{"effort":"high"}}));
    let parsed = Request::parse(
        json!({"model":"m","input":history,"reasoning":{"effort":"low","context":"all_turns"}}),
    )
    .unwrap();
    let resolved = parsed.with_history(&parsed.input).unwrap();
    assert_eq!(resolved.reasoning["effort"], "high");
    assert_eq!(
        session.pending_update(&resolved).unwrap(),
        json!({"effort":"high"})
    );
    assert_eq!(
        session
            .continuation(&owner, &resolved, &resolved.input)
            .unwrap(),
        Some(2),
        "configuration control must not discard the waiting tool turn"
    );
}
