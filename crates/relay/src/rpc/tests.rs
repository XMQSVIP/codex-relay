use super::*;
use crate::installation::NativeClientInfo;
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};

struct ReplyBatch {
    rpc: Rpc,
    outgoing: mpsc::Receiver<Value>,
    access: crate::operations::ClientAccess,
    directory: tempfile::TempDir,
    session: Option<crate::responses::reuse::Session>,
    continuation: crate::responses::continuation::Continuation,
    request: crate::responses::Request,
    provider: crate::provider::Provider,
}

async fn reply_batch() -> ReplyBatch {
    use crate::responses::{Request, continuation, reuse::Session};

    let (rpc, outgoing) = Rpc::new();
    rpc.enable_client_tools();
    let mut events = rpc.subscribe_thread("thread");
    let tools = json!([{"type":"function","name":"echo"}]);
    let first = Request::parse(json!({"model":"m","input":"first","tools":tools})).unwrap();
    let calls: Vec<_> = ["first", "second"].into_iter().map(|id| {
        json!({"type":"function_call","id":format!("fc_{id}"),"call_id":id,"name":"echo","arguments":"{}","status":"completed"})
    }).collect();
    let access = crate::operations::ClientAccess {
        fast_mode: false,
        key_id: uuid::Uuid::new_v4(),
        group_id: uuid::Uuid::new_v4(),
        models: vec![],
    };
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::new(
        "thread".into(),
        directory.path().into(),
        &access,
        &first,
        &first.input,
        &calls,
    )
    .unwrap();
    for id in ["first", "second"] {
        rpc.receive(&json!({"id":format!("callback_{id}"),"method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":id,"tool":"echo","arguments":{}}}).to_string()).unwrap();
        session
            .calls
            .push(continuation::callback(&first, &events.recv().await.unwrap()).unwrap());
    }
    let continuation = continuation::Continuation {
        incoming: Some(continuation::Waiting::new(continuation::TurnGuard::new(
            rpc.clone(),
            "thread".into(),
            "turn".into(),
            events,
            None,
        ))),
        ..Default::default()
    };
    let mut history = first.input;
    history.extend(calls);
    for id in ["first", "second"] {
        history.push(json!({"type":"function_call_output","call_id":id,"output":"result"}));
    }
    let request = Request::parse(json!({"model":"m","input":history,"tools":tools})).unwrap();
    let provider = crate::provider::Provider {
        execution: Default::default(),
        id: "p".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1/v1".into(),
        auth: "api-key".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    // Exactly one result can be submitted before the batch blocks.
    for _ in 1..rpc.inner.outgoing.max_capacity() {
        rpc.inner
            .outgoing
            .try_send(json!({"fixture":true}))
            .unwrap();
    }
    ReplyBatch {
        rpc,
        outgoing,
        access,
        directory,
        session: Some(session),
        continuation,
        request,
        provider,
    }
}

#[tokio::test]
async fn later_tool_replies_recheck_lifetime_after_waiting_for_capacity() {
    if !crate::native_cache::enabled() {
        return;
    }
    for case in [
        "terminal",
        "closed",
        "compaction",
        "compacted",
        "lagged",
        "cancel",
        "disconnected",
        "rpc_closed",
    ] {
        let ReplyBatch {
            rpc,
            mut outgoing,
            access,
            directory,
            mut session,
            mut continuation,
            request,
            provider,
        } = reply_batch().await;
        let (tx, rx) = mpsc::channel(16);
        let mut rx = Some(rx);
        let cancel = CancellationToken::new();
        let work = crate::responses::run_profiled(
            &rpc,
            &provider,
            directory.path(),
            &request,
            &request.input,
            tx,
            cancel.clone(),
            None,
            Some(crate::responses::reuse::Reuse {
                default_summary: None,
                access: &access,
                session: &mut session,
                continuation: Some(&mut continuation),
            }),
        );
        tokio::pin!(work);
        assert!(futures_util::poll!(&mut work).is_pending());
        assert_eq!(rpc.inner.outgoing.capacity(), 0);
        assert_eq!(rpc.inner.tool_calls.lock().unwrap().len(), 1);
        let invalidation = match case {
            "terminal" => Some(
                json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
            ),
            "closed" => Some(json!({"method":"thread/closed","params":{"threadId":"thread"}})),
            "compaction" => Some(
                json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"id":"compact","type":"contextCompaction"}}}),
            ),
            "compacted" => Some(
                json!({"method":"thread/compacted","params":{"threadId":"thread","turnId":"turn"}}),
            ),
            "lagged" => {
                for _ in 0..513 {
                    rpc.receive(
                        &json!({"method":"thread/status/changed","params":{"threadId":"thread"}})
                            .to_string(),
                    )
                    .unwrap();
                }
                None
            }
            "cancel" => {
                cancel.cancel();
                None
            }
            "disconnected" => {
                drop(rx.take());
                None
            }
            "rpc_closed" => {
                rpc.close();
                None
            }
            _ => unreachable!(),
        };
        if let Some(event) = invalidation {
            rpc.receive(&event.to_string()).unwrap();
        }
        assert_eq!(outgoing.recv().await.unwrap()["fixture"], true);
        let error = work.await.unwrap_err();
        assert_eq!(
            error.code,
            match case {
                "cancel" => "REQUEST_CANCELLED",
                "disconnected" => "STREAM_CONSUMER_CLOSED",
                "rpc_closed" => "RPC_CLOSED",
                _ => "NATIVE_TURN_UNAVAILABLE",
            },
            "{case}"
        );
        let mut submitted = Vec::new();
        while let Ok(frame) = outgoing.try_recv() {
            if frame.get("result").is_some() {
                submitted.push(frame["id"].clone());
            }
        }
        assert_eq!(submitted, vec![json!("callback_first")], "{case}");
        assert!(
            rpc.inner
                .tool_calls
                .lock()
                .unwrap()
                .contains_key("\"callback_second\""),
            "{case}: unsent callback remains owned for cleanup"
        );
        rpc.close();
    }
}

#[tokio::test]
async fn reply_batch_observer_preserves_echoes_output_usage_and_new_callbacks_for_collection() {
    if !crate::native_cache::enabled() {
        return;
    }
    for new_callback in [false, true] {
        let ReplyBatch {
            rpc,
            mut outgoing,
            access,
            directory,
            mut session,
            mut continuation,
            request,
            provider,
        } = reply_batch().await;
        let (tx, mut rx) = mpsc::channel(32);
        let work = crate::responses::run_profiled(
            &rpc,
            &provider,
            directory.path(),
            &request,
            &request.input,
            tx,
            CancellationToken::new(),
            None,
            Some(crate::responses::reuse::Reuse {
                default_summary: None,
                access: &access,
                session: &mut session,
                continuation: Some(&mut continuation),
            }),
        );
        tokio::pin!(work);
        assert!(futures_util::poll!(&mut work).is_pending());
        for event in [
            json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"other","status":"completed"}}}),
            json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call_output","call_id":"first","output":"result"}}}),
            json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"msg","delta":"kept "}}),
            json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"kept output"}]}}}),
        ] {
            rpc.receive(&event.to_string()).unwrap();
        }
        if new_callback {
            rpc.receive(&json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call","id":"fc_next","call_id":"next","name":"echo","arguments":"{}"}}}).to_string()).unwrap();
            rpc.receive(&json!({"id":"callback_next","method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":"next","tool":"echo","arguments":{}}}).to_string()).unwrap();
        }
        rpc.receive(&json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}}).to_string()).unwrap();
        assert_eq!(outgoing.recv().await.unwrap()["fixture"], true);
        if !new_callback {
            // The final reply hands the original receiver directly to collection.
            assert!(futures_util::poll!(&mut work).is_pending());
            rpc.receive(&json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}).to_string()).unwrap();
        }
        let summary = work.await.unwrap();
        assert_eq!(summary.outcome, "completed");
        assert_eq!(summary.cleanup_confirmed, !new_callback);
        assert_eq!(summary.usage.unwrap().total_tokens, 12);
        let mut terminal = Value::Null;
        while let Ok(event) = rx.try_recv() {
            if event["type"] == "response.completed" {
                terminal = event["response"].clone();
            }
        }
        assert_eq!(terminal["output"][0]["content"][0]["text"], "kept output");
        let calls: Vec<_> = terminal["output"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        assert_eq!(calls.len(), usize::from(new_callback));
        if new_callback {
            assert_eq!(calls[0]["call_id"], "next");
        }
        let mut submitted = Vec::new();
        while let Ok(frame) = outgoing.try_recv() {
            if frame.get("result").is_some() {
                submitted.push(frame["id"].clone());
            }
        }
        assert_eq!(
            submitted,
            vec![json!("callback_first"), json!("callback_second")]
        );
        rpc.close();
    }
}

#[tokio::test]
async fn blocked_tool_reply_checks_validity_only_after_capacity_is_available() {
    let (rpc, mut outgoing) = Rpc::new();
    let _events = rpc.subscribe();
    rpc.enable_client_tools();
    rpc.receive(&json!({"id":"callback","method":"item/tool/call","params":{"threadId":"t","callId":"call","tool":"echo","arguments":{}}}).to_string()).unwrap();
    let capacity = rpc.inner.outgoing.max_capacity();
    for _ in 0..capacity {
        rpc.inner
            .outgoing
            .try_send(json!({"fixture":true}))
            .unwrap();
    }
    let valid = std::sync::atomic::AtomicBool::new(true);
    let checked = std::sync::atomic::AtomicUsize::new(0);
    let reply = rpc.answer_tool_checked(json!("callback"), json!({"success":true}), || {
        checked.fetch_add(1, Ordering::SeqCst);
        if valid.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::new("NATIVE_TURN_UNAVAILABLE", "fixture"))
        }
    });
    tokio::pin!(reply);
    assert!(futures_util::poll!(&mut reply).is_pending());
    assert_eq!(checked.load(Ordering::SeqCst), 0);
    assert_eq!(rpc.inner.tool_calls.lock().unwrap().len(), 1);
    valid.store(false, Ordering::SeqCst);
    outgoing.recv().await.unwrap();
    assert_eq!(reply.await.unwrap_err().code, "NATIVE_TURN_UNAVAILABLE");
    assert_eq!(checked.load(Ordering::SeqCst), 1);
    assert_eq!(
        rpc.inner.outgoing.capacity(),
        1,
        "failed validation must return the reserved queue slot"
    );
    assert_eq!(
        rpc.inner.tool_calls.lock().unwrap().len(),
        1,
        "the callback remains owned until cleanup"
    );
    while let Ok(message) = outgoing.try_recv() {
        assert_eq!(message["fixture"], true);
    }
    rpc.clear_thread_tool_callbacks("t");
    assert!(rpc.answer_tool(json!("callback"), json!({})).await.is_err());
    assert!(outgoing.try_recv().is_err());
}

#[tokio::test]
async fn responses_thread_applies_account_location_without_rewriting_fixed_instructions() {
    let (rpc, mut outgoing) = Rpc::new();
    rpc.set_location(Some(crate::account_location::Location {
        timezone: "UTC".into(),
        ..Default::default()
    }));
    let instructions = "<codex_apps_client_time_context><timezone>Asia/Shanghai</timezone><current_date>2000-01-01</current_date></codex_apps_client_time_context>";
    let params = json!({"developerInstructions":instructions});
    let (result, ()) = tokio::join!(rpc.start_responses_thread(params.clone()), async {
        let request = outgoing.recv().await.unwrap();
        assert_eq!(request["method"], "thread/start");
        assert_eq!(request["params"]["developerInstructions"], instructions);
        assert_eq!(
            request["params"]["config"]["include_environment_context_time"],
            false
        );
        assert_eq!(
            request["params"]["config"]["tools.web_search.location"]["timezone"],
            "UTC"
        );
        rpc.receive(&json!({"id":request["id"],"result":{}}).to_string())
            .unwrap();
    });
    result.unwrap();
    let (result, ()) = tokio::join!(rpc.request("thread/start", params), async {
        let request = outgoing.recv().await.unwrap();
        assert!(
            request["params"]["developerInstructions"]
                .as_str()
                .unwrap()
                .contains("<timezone>UTC</timezone>")
        );
        rpc.receive(&json!({"id":request["id"],"result":{}}).to_string())
            .unwrap();
    });
    result.unwrap();
}

#[tokio::test]
async fn checked_submission_revalidates_after_context_injection_without_leaking_pending_rpc() {
    let (rpc, mut outgoing) = Rpc::new();
    rpc.set_location(Some(crate::account_location::Location {
        timezone: "UTC".into(),
        ..Default::default()
    }));
    let cancel = CancellationToken::new();
    let checked = rpc.request_checked("turn/start", json!({"threadId":"t","input":[]}), || {
        if cancel.is_cancelled() {
            Err(Error::new("REQUEST_CANCELLED", "fixture"))
        } else {
            Ok(())
        }
    });
    let (result, ()) = tokio::join!(checked, async {
        let request = outgoing.recv().await.unwrap();
        assert_eq!(request["method"], "thread/inject_items");
        cancel.cancel();
        rpc.receive(&json!({"id":request["id"],"result":{}}).to_string())
            .unwrap();
    });
    assert_eq!(result.unwrap_err().code, "REQUEST_CANCELLED");
    assert!(
        outgoing.try_recv().is_err(),
        "turn/start must not be sent after cancellation"
    );
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
    let (result, ()) = tokio::join!(rpc.request("config/read", json!({})), async {
        let request = outgoing.recv().await.unwrap();
        assert_eq!(request["method"], "config/read");
        rpc.receive(&json!({"id":request["id"],"result":{"ok":true}}).to_string())
            .unwrap();
    });
    assert_eq!(result.unwrap()["ok"], true);
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shared_threads_isolate_events_callback_cleanup_and_rejection() {
    let (rpc, mut outgoing) = Rpc::new();
    rpc.enable_client_tools();
    let mut first = rpc.subscribe_thread("first");
    let mut second = rpc.subscribe_thread("second");
    for (id, thread) in [(1, "first"), (2, "second")] {
        rpc.receive(
            &json!({"id":id,"method":"item/tool/call","params":{"threadId":thread,"tool":"echo"}})
                .to_string(),
        )
        .unwrap();
    }
    assert_eq!(first.recv().await.unwrap()["id"], 1);
    assert_eq!(second.recv().await.unwrap()["id"], 2);
    rpc.clear_thread_tool_callbacks("first");
    rpc.answer_tool(json!(2), json!({"success":true}))
        .await
        .unwrap();
    assert_eq!(outgoing.recv().await.unwrap()["id"], 2);
    assert!(rpc.answer_tool(json!(1), json!({})).await.is_err());
    // Flooding a sibling cannot overrun this thread's notification buffer.
    for _ in 0..1024 {
        rpc.receive(
            &json!({"method":"item/agentMessage/delta","params":{"threadId":"first","delta":"x"}})
                .to_string(),
        )
        .unwrap();
    }
    assert!(matches!(
        second.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    rpc.receive(
        &json!({"id":3,"method":"unknown/callback","params":{"threadId":"first"}}).to_string(),
    )
    .unwrap();
    assert!(matches!(
        second.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    rpc.close();
    tokio::time::timeout(Duration::from_millis(100), rpc.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn host_time_callback_returns_unix_seconds_without_downstream_events() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    for id in [json!(17), json!("time-request")] {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        rpc.receive(
            &json!({"id":id,"method":"currentTime/read","params":{"threadId":"synthetic-thread"}})
                .to_string(),
        )
        .unwrap();
        let reply = messages.recv().await.unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(reply["id"], id);
        let seconds = reply["result"]["currentTimeAt"].as_u64().unwrap();
        assert!((before..=after).contains(&seconds));
    }
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn malformed_host_callbacks_cannot_become_downstream_notifications() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    for params in [
        json!(null),
        json!({}),
        json!({"threadId":false}),
        json!({"threadId":""}),
    ] {
        rpc.receive(&json!({"id":1,"method":"currentTime/read","params":params}).to_string())
            .unwrap();
        assert_eq!(messages.recv().await.unwrap()["error"]["code"], -32602);
    }
    for method in ["currentTime/read", "attestation/generate"] {
        for id in [None, Some(json!(null)), Some(json!({"secret":"private"}))] {
            let mut request = json!({"method":method,"params":{"secret":"private"}});
            if let Some(id) = id {
                request["id"] = id;
            }
            assert_eq!(
                rpc.receive(&request.to_string()).unwrap_err().code,
                "RPC_PROTOCOL"
            );
        }
    }
    assert!(messages.try_recv().is_err());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn auth_refresh_is_private_and_cannot_use_a_different_account() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.set_auth_refresh(|previous| {
        Box::pin(async move {
            if previous.as_deref() != Some("account-a") {
                return Err(Error::new(
                    "OAUTH_IDENTITY_CHANGED",
                    "synthetic private error",
                ));
            }
            Ok(json!({"accessToken":"synthetic-private-token","chatgptAccountId":"account-a"}))
        })
    });
    rpc.receive(r#"{"id":"auth-1","method":"account/chatgptAuthTokens/refresh","params":{"previousAccountId":"account-a"}}"#).unwrap();
    assert_eq!(
        messages.recv().await.unwrap()["result"]["accessToken"],
        "synthetic-private-token"
    );
    assert!(events.try_recv().is_err());
    tokio::task::yield_now().await;
    rpc.receive(r#"{"id":"auth-2","method":"account/chatgptAuthTokens/refresh","params":{"previousAccountId":"account-b"}}"#).unwrap();
    let denied = messages.recv().await.unwrap();
    assert_eq!(denied["error"]["code"], -32001);
    assert!(!denied.to_string().contains("synthetic private"));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn initializes_with_native_metadata_and_notifies_only_after_acknowledgment() {
    for enabled in [false, true] {
        let (rpc, mut messages) = Rpc::new();
        if enabled {
            rpc.set_attestation_provider(|| Box::pin(async { Ok("synthetic-proof".into()) }));
        }
        let client_info = NativeClientInfo {
            name: "codex_desktop".into(),
            title: "Codex Desktop".into(),
            version: "26.1016.51333".into(),
        };
        let expected = serde_json::to_value(&client_info).unwrap();
        let task_rpc = rpc.clone();
        let task = tokio::spawn(async move { task_rpc.initialize(&client_info).await });
        let request = messages.recv().await.unwrap();
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["clientInfo"], expected);
        assert_eq!(
            request["params"]["capabilities"],
            json!({"experimentalApi":true,"requestAttestation":enabled})
        );
        assert!(messages.try_recv().is_err());
        rpc.receive(
            &json!({"id":request["id"],"result":{"userAgent":"native-response"}}).to_string(),
        )
        .unwrap();
        assert_eq!(task.await.unwrap().unwrap()["userAgent"], "native-response");
        assert_eq!(messages.recv().await.unwrap()["method"], "initialized");
        assert!(messages.try_recv().is_err());
    }
}

#[tokio::test]
async fn attestation_proof_is_private_and_preserves_callback_id() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.set_attestation_provider(|| Box::pin(async { Ok("synthetic-private-proof".into()) }));
    for id in [json!(42), json!("proof-request")] {
        rpc.receive(&json!({"id":id,"method":"attestation/generate","params":{}}).to_string())
            .unwrap();
        let response = messages.recv().await.unwrap();
        assert_eq!(
            response,
            json!({"id":id,"result":{"token":"synthetic-private-proof"}})
        );
        tokio::task::yield_now().await;
    }
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn unavailable_and_invalid_attestation_never_reaches_api_events() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    assert_eq!(messages.recv().await.unwrap()["error"]["code"], -32001);
    // A caller cannot submit a token through callback parameters.
    rpc.receive(r#"{"id":2,"method":"attestation/generate","params":{"token":"never-forward"}}"#)
        .unwrap();
    let response = messages.recv().await.unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert!(!response.to_string().contains("never-forward"));
    for invalid in ["", "secret\r\nheader", &"x".repeat(64 * 1024 + 1)] {
        let invalid = invalid.to_owned();
        rpc.set_attestation_provider(move || {
            let invalid = invalid.clone();
            Box::pin(async move { Ok(invalid) })
        });
        rpc.receive(r#"{"id":3,"method":"attestation/generate","params":{}}"#)
            .unwrap();
        let response = messages.recv().await.unwrap();
        assert_eq!(response["error"]["code"], -32001);
        assert!(!response.to_string().contains("secret"));
        tokio::task::yield_now().await;
    }
    rpc.set_attestation_provider(|| {
        Box::pin(async { Err(Error::new("PROOF", "private provider error")) })
    });
    rpc.receive(r#"{"id":4,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    assert!(
        !messages
            .recv()
            .await
            .unwrap()
            .to_string()
            .contains("private provider error")
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn attestation_is_bounded_and_cancelled_with_its_connection() {
    let (rpc, mut messages) = Rpc::new();
    rpc.set_attestation_provider(|| Box::pin(std::future::pending()));
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    rpc.receive(r#"{"id":2,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    let busy = messages.recv().await.unwrap();
    assert_eq!(busy["id"], 2);
    assert_eq!(busy["error"]["code"], -32001);
    rpc.close();
    tokio::time::timeout(Duration::from_secs(1), async {
        while rpc.inner.attestation_active.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(messages.try_recv().is_err());
}

#[tokio::test]
async fn attestation_timeout_releases_provider_slot() {
    let (rpc, mut messages) = Rpc::new();
    rpc.set_attestation_provider(|| Box::pin(std::future::pending()));
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(10), messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply["error"]["code"], -32001);
    tokio::task::yield_now().await;
    assert!(!rpc.inner.attestation_active.load(Ordering::SeqCst));
}

#[tokio::test]
async fn correlates_out_of_order_responses_and_redacts_errors() {
    let (rpc, mut messages) = Rpc::new();
    let a = rpc.clone();
    let a = tokio::spawn(async move { a.request("a", json!({})).await });
    let first = messages.recv().await.unwrap();
    let b = rpc.clone();
    let b = tokio::spawn(async move { b.request("b", json!({})).await });
    let second = messages.recv().await.unwrap();
    rpc.receive(&json!({"id":second["id"],"result":{"value":2}}).to_string())
        .unwrap();
    rpc.receive(&json!({"id":first["id"],"error":{"message":"secret"}}).to_string())
        .unwrap();
    assert_eq!(b.await.unwrap().unwrap()["value"], 2);
    assert!(!a.await.unwrap().unwrap_err().to_string().contains("secret"));
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remote_errors_keep_method_code_and_safe_reason_without_replay() {
    let (rpc, mut messages) = Rpc::new();
    for (native_code, message, expected_code, reason) in [
        (
            -32600,
            "dynamic tool input schema is not supported for secret: private prompt",
            "RPC_REMOTE",
            "inputSchema",
        ),
        (
            -32600,
            "dynamic tool namespace collides with a reserved Responses API namespace: secret",
            "RPC_REMOTE",
            "保留命名空间冲突",
        ),
        (-32602, "private prompt", "RPC_REMOTE", "请求参数无效"),
        (-32603, "private internal error", "RPC_REMOTE", "内部错误"),
        (
            -32601,
            "private unsupported method",
            "RPC_METHOD_UNSUPPORTED",
            "不支持此接口",
        ),
    ] {
        let request_rpc = rpc.clone();
        let task =
            tokio::spawn(async move { request_rpc.request("thread/start", json!({})).await });
        let sent = messages.recv().await.unwrap();
        rpc.receive(&json!({"id":sent["id"],"error":{"code":native_code,"message":message,"data":{"token":"secret"}}}).to_string()).unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code, expected_code);
        assert!(error.message.contains("thread/start"));
        assert!(error.message.contains(&native_code.to_string()));
        assert!(error.message.contains(reason), "{}", error.message);
        assert!(!error.message.contains("private"));
        assert!(!error.message.contains("secret"));
        assert!(rpc.inner.pending.lock().unwrap().is_empty());
        assert!(messages.try_recv().is_err());
        assert!(!rpc.is_closed());
    }
}

#[tokio::test]
async fn cancellation_timeout_and_disconnect_release_slots_without_replay() {
    let (rpc, mut messages) = Rpc::new();
    let task_rpc = rpc.clone();
    let task = tokio::spawn(async move { task_rpc.request("cancel", json!({})).await });
    messages.recv().await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
    assert_eq!(
        rpc.request_timeout("timeout", json!({}), Duration::from_millis(10))
            .await
            .unwrap_err()
            .code,
        "RPC_TIMEOUT"
    );
    assert_eq!(messages.recv().await.unwrap()["method"], "timeout");
    assert!(messages.try_recv().is_err());
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
    let task_rpc = rpc.clone();
    let task = tokio::spawn(async move { task_rpc.request("disconnect", json!({})).await });
    messages.recv().await.unwrap();
    rpc.close();
    assert_eq!(task.await.unwrap().unwrap_err().code, "RPC_CLOSED");
}

#[tokio::test]
async fn callbacks_are_rejected_and_protocol_failures_close_stdio() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.receive(r#"{"id":1,"method":"account/chatgptAuthTokens/refresh","params":{"secret":"never-forward"}}"#).unwrap();
    let rejected = messages.recv().await.unwrap();
    assert_eq!(rejected["error"]["code"], -32601);
    assert!(
        !events
            .recv()
            .await
            .unwrap()
            .to_string()
            .contains("never-forward")
    );
    let (client, mut server) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(client);
    let rpc = Rpc::from_io(reader, writer);
    use tokio::io::AsyncWriteExt;
    server.write_all(b"{bad json}\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !rpc.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(rpc.request("after-close", json!({})).await.is_err());
}

#[tokio::test]
async fn quota_subscribers_ignore_tokens_and_claim_each_update_only_once() {
    let (rpc, _messages) = Rpc::new();
    let mut first = rpc.subscribe_quota();
    let mut second = rpc.subscribe_quota();
    for _ in 0..600 {
        rpc.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"first","delta":"text"}}));
    }
    assert!(matches!(
        first.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    rpc.publish(
        json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"usedPercent":50}}}),
    );
    let one = first.recv().await.unwrap();
    let two = second.recv().await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&one, &two));
    assert!(one.claim());
    assert!(!two.claim());
    rpc.publish(
        json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"usedPercent":60}}}),
    );
    assert!(second.recv().await.unwrap().claim());
}
