use super::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn namespace_supplements_are_injected_once_per_native_thread() {
    if !crate::native_cache::enabled() {
        return;
    }
    for embedded in [false, true] {
        for disabled in [false, true] {
            let (client, peer) = tokio::io::duplex(65536);
            let (read, write) = tokio::io::split(client);
            let rpc = Rpc::from_io(read, write);
            let server = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(peer);
                let mut lines = BufReader::new(read).lines();
                let mut frames = vec![];
                while let Some(line) = lines.next_line().await.unwrap() {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let mut events = vec![json!({"id":frame["id"],"result":{}})];
                    match frame["method"].as_str().unwrap() {
                        "thread/start" => {
                            events[0]["result"] = json!({"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"})
                        }
                        "thread/inject_items" => {}
                        "turn/start" => {
                            let turn = format!("turn-{}", frames.len());
                            events[0]["result"] = json!({"turn":{"id":turn}});
                            events.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":turn,"item":{"type":"message","id":format!("msg_{turn}"),"role":"assistant","content":[{"type":"output_text","text":"done"}]}}}));
                            events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":turn,"status":"completed"}}}));
                        }
                        other => panic!("unexpected operation {other}"),
                    }
                    frames.push(frame);
                    for event in events {
                        write
                            .write_all(format!("{event}\n").as_bytes())
                            .await
                            .unwrap();
                    }
                }
                frames
            });
            let tools = json!([{"type":"namespace","name":"notes","description":format!("{}FULL_TAIL", "🌍".repeat(1024)),
                "tools":[{"type":"function","name":"read_file"}]}]);
            let mut input = vec![json!({"role":"user","content":"first"})];
            if embedded {
                input.insert(
                    0,
                    json!({"type":"additional_tools","role":"developer","tools":tools}),
                );
            }
            let mut request = Request::parse(
                json!({"model":"m","tools":if embedded { json!([]) } else {tools},
                "tool_choice":if disabled {"none"} else {"auto"},"input":input}),
            )
            .unwrap();
            let provider = Provider {
                execution: Default::default(),
                id: "p".into(),
                model: "m".into(),
                base_url: "http://127.0.0.1/v1".into(),
                auth: "api-key".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            };
            let directory = tempfile::tempdir().unwrap();
            let owner = crate::operations::ClientAccess {
                fast_mode: false,
                key_id: uuid::Uuid::new_v4(),
                group_id: uuid::Uuid::new_v4(),
                models: vec![],
            };
            let mut session = None;
            for _ in 0..2 {
                let (tx, mut rx) = mpsc::channel(32);
                let summary = tokio::time::timeout(
                    Duration::from_secs(3),
                    run_profiled(
                        &rpc,
                        &provider,
                        directory.path(),
                        &request,
                        &request.input,
                        tx,
                        CancellationToken::new(),
                        None,
                        Some(reuse::Reuse {
                            access: &owner,
                            session: &mut session,
                            continuation: None,
                            default_summary: None,
                        }),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(summary.outcome, "completed", "{summary:?}");
                assert!(session.is_some());
                while let Some(event) = rx.recv().await {
                    if event["type"] == "response.completed" {
                        request
                            .input
                            .extend(event["response"]["output"].as_array().unwrap().clone());
                    }
                }
                request.input.push(json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}));
            }
            rpc.close();
            let frames = server.await.unwrap();
            assert_eq!(
                frames
                    .iter()
                    .filter(|f| f["method"] == "thread/start")
                    .count(),
                1
            );
            let injected: Vec<_> = frames
                .iter()
                .filter(|f| f["method"] == "thread/inject_items")
                .collect();
            assert_eq!(injected.len(), 2);
            assert_eq!(
                injected[0].to_string().matches("FULL_TAIL").count(),
                usize::from(!disabled)
            );
            assert!(!injected[1].to_string().contains("FULL_TAIL"));
            assert_eq!(injected[1]["params"]["items"].as_array().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn fork_thread_start_failure_preserves_safe_diagnostics_without_submitting_a_turn() {
    let (client, peer) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let mut methods = vec![];
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            methods.push(frame["method"].as_str().unwrap().to_owned());
            assert_eq!(frame["method"], "thread/start");
            assert_eq!(
                frame["params"]["dynamicTools"][0]["tools"][0]["name"],
                "echo"
            );
            let reply = json!({"id":frame["id"],"error":{
                "code":-32600,
                "message":"dynamic tool input schema is not supported for secret: private prompt",
                "data":{"token":"secret"}
            }});
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
        methods
    });
    let request = Request::parse(json!({
        "model":"m",
        "client_metadata":{"thread_id":"branch","forked_from_thread_id":"parent"},
        "input":[
            {"type":"additional_tools","role":"developer","tools":[{
                "type":"namespace","name":"functions","description":"Inherited tools",
                "tools":[{"type":"function","name":"echo","parameters":{"type":"object"}}]
            }]},
            {"role":"user","content":"Continue in the branch"}
        ]
    }))
    .unwrap();
    let provider = Provider {
        execution: Default::default(),
        id: "p".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1/v1".into(),
        auth: "api-key".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    let directory = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        run(
            &rpc,
            &provider,
            directory.path(),
            &request,
            &request.input,
            tx,
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    rpc.close();
    assert_eq!(server.await.unwrap(), ["thread/start"]);
    let event = super::super::websocket::error_event(error);
    assert_eq!(event["code"], "RPC_REMOTE");
    assert_eq!(event["error"]["type"], "server_error");
    assert_eq!(event["message"], event["error"]["message"]);
    let message = event["message"].as_str().unwrap();
    for expected in ["thread/start", "-32600", "inputSchema"] {
        assert!(message.contains(expected), "{message}");
    }
    assert!(!event.to_string().contains("secret"));
    assert!(!event.to_string().contains("private"));
}

#[tokio::test]
async fn cancellation_respects_the_native_turn_submission_boundary() {
    for (stage, disconnect) in ["history", "clock", "submitted"]
        .into_iter()
        .flat_map(|stage| [false, true].map(|disconnect| (stage, disconnect)))
        .flat_map(|case| vec![case; if case.0 == "submitted" { 32 } else { 1 }])
    {
        let (client, peer) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        let rpc = Rpc::from_io(read, write);
        if stage == "clock" {
            rpc.set_location(Some(crate::account_location::Location {
                timezone: "UTC".into(),
                ..Default::default()
            }));
        }
        let cancel = CancellationToken::new();
        let cancelled = cancel.clone();
        let starts = Arc::new(AtomicUsize::new(0));
        let seen = starts.clone();
        let (tx, rx) = mpsc::channel(16);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            let mut rx = Some(rx);
            let mut injections = 0;
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                let mut events = vec![json!({"id":frame["id"],"result":{}})];
                match frame["method"].as_str().unwrap() {
                    "thread/start" => events[0]["result"] = json!({"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"}),
                    "thread/inject_items" => {
                        injections += 1;
                        if (stage == "history" && injections == 1) || (stage == "clock" && injections == 2) {
                            if disconnect { rx.take(); } else { cancelled.cancel(); }
                        }
                    },
                    "turn/start" => {
                        if stage == "submitted" {
                            if disconnect { rx.take(); } else { cancelled.cancel(); }
                        }
                        seen.fetch_add(1, Ordering::SeqCst);
                        events[0]["result"] = json!({"turn":{"id":"turn"}});
                        events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}));
                    },
                    "turn/interrupt" => events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}})),
                    other => panic!("unexpected operation {other}"),
                }
                for event in events {
                    write
                        .write_all(format!("{event}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let request = Request::parse(json!({"model":"m","input":"test"})).unwrap();
        let provider = Provider {
            execution: Default::default(),
            id: "p".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1/v1".into(),
            auth: "api-key".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let directory = tempfile::tempdir().unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                &rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                tx,
                cancel,
            ),
        )
        .await
        .unwrap();
        rpc.close();
        server.await.unwrap();
        assert_eq!(
            starts.load(Ordering::SeqCst),
            usize::from(stage == "submitted"),
            "cancellation must preserve the submission boundary: {stage}"
        );
        let expected = if disconnect {
            "STREAM_CONSUMER_CLOSED"
        } else {
            "REQUEST_CANCELLED"
        };
        if stage == "submitted" {
            let summary = result.unwrap();
            assert_eq!(
                summary.outcome, "cancelled",
                "{stage}, disconnect={disconnect}"
            );
            assert_eq!(summary.error_code.as_deref(), Some(expected));
            assert!(summary.cleanup_confirmed);
        } else {
            assert_eq!(result.unwrap_err().code, expected);
        }
    }
}

#[tokio::test]
async fn terminal_delivery_backpressure_keeps_usage_and_releases_tool_waits() {
    for pending_tool in [false, true] {
        let (client, peer) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        let rpc = Rpc::from_io(read, write);
        let interrupts = Arc::new(AtomicUsize::new(0));
        let seen = interrupts.clone();
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                let mut events = vec![json!({"id":frame["id"],"result":{}})];
                match frame["method"].as_str().unwrap() {
                    "thread/start" => {
                        events[0]["result"] = json!({"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"})
                    }
                    "thread/inject_items" => {}
                    "turn/start" => {
                        events[0]["result"] = json!({"turn":{"id":"turn"}});
                        let item = if pending_tool {
                            json!({"type":"function_call","id":"fc","call_id":"echo","name":"echo","arguments":"{}"})
                        } else {
                            json!({"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"done"}]})
                        };
                        events.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":item}}));
                        if pending_tool {
                            events.push(json!({"method":"item/tool/call","id":"callback","params":{"threadId":"thread","turnId":"turn","callId":"echo","tool":"echo","arguments":{}}}));
                        }
                        events.push(json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}}));
                        if !pending_tool {
                            events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}));
                        }
                    }
                    "turn/interrupt" => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}}));
                    }
                    other => panic!("unexpected operation {other}"),
                }
                for event in events {
                    write
                        .write_all(format!("{event}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let request = Request::parse(
            json!({"model":"m","input":"test","tools":[{"type":"function","name":"echo"}]}),
        )
        .unwrap();
        let provider = Provider {
            execution: Default::default(),
            id: "p".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1/v1".into(),
            auth: "api-key".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let directory = tempfile::tempdir().unwrap();
        let owner = crate::operations::ClientAccess {
            fast_mode: false,
            key_id: uuid::Uuid::new_v4(),
            group_id: uuid::Uuid::new_v4(),
            models: vec![],
        };
        let mut session = None;
        let mut continuation = continuation::Continuation::default();
        // Exactly enough room for every nonterminal event. The model finishes,
        // but delivering response.completed must hit the backpressure deadline.
        let (tx, mut rx) = mpsc::channel(if pending_tool { 6 } else { 8 });
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            run_profiled(
                &rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                tx,
                CancellationToken::new(),
                None,
                Some(reuse::Reuse {
                    access: &owner,
                    session: &mut session,
                    continuation: Some(&mut continuation),
                    default_summary: None,
                }),
            ),
        )
        .await
        .unwrap();
        server.abort();
        let summary = result.expect("delivery failure must preserve the collected summary");
        assert_eq!(summary.error_code.as_deref(), Some("STREAM_BACKPRESSURE"));
        assert_eq!(summary.outcome, "failed");
        assert_eq!(summary.usage.unwrap().total_tokens, 12);
        assert!(summary.cleanup_confirmed);
        assert!(session.is_none());
        assert!(continuation.waiting.is_none());
        assert_eq!(interrupts.load(Ordering::SeqCst), usize::from(pending_tool));
        while let Some(event) = rx.recv().await {
            assert_ne!(event["type"], "response.completed");
        }
        rpc.close();
    }
}

#[tokio::test]
async fn terminal_tool_failure_preserves_confirmed_native_cleanup() {
    terminal_failure(true).await;
    terminal_failure(false).await;
}

async fn terminal_failure(missing_tool: bool) {
    let (client, peer) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    let interrupts = Arc::new(AtomicUsize::new(0));
    let seen = interrupts.clone();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            let mut events = match frame["method"].as_str().unwrap() {
                "thread/start" => vec![
                    json!({"id":frame["id"],"result":{"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"}}),
                ],
                "thread/inject_items" => vec![json!({"id":frame["id"],"result":{}})],
                "turn/start" => vec![
                    json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}}),
                    json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call","id":"fc_search","call_id":"search","namespace":"web","name":"run","arguments":"{}"}}}),
                    json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}}),
                    json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
                ],
                "turn/interrupt" => {
                    seen.fetch_add(1, Ordering::SeqCst);
                    vec![
                        json!({"id":frame["id"],"error":{"code":-32600,"message":"Turn already completed"}}),
                    ]
                }
                other => panic!("unexpected operation {other}"),
            };
            if !missing_tool {
                events.retain(|event| event["method"] != "rawResponseItem/completed");
            }
            for event in events {
                write
                    .write_all(format!("{event}\n").as_bytes())
                    .await
                    .unwrap();
            }
        }
    });
    let request =
        Request::parse(json!({"model":"m","input":"search","tools":[{"type":"web_search"}]}))
            .unwrap();
    let provider = Provider {
        execution: Default::default(),
        id: "p".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1/v1".into(),
        auth: "api-key".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    let directory = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        run(
            &rpc,
            &provider,
            directory.path(),
            &request,
            &request.input,
            tx,
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    server.abort();
    assert_eq!(result.outcome, "failed");
    assert_eq!(
        result.error_code.as_deref(),
        Some(if missing_tool {
            "UPSTREAM_TOOL_FAILED"
        } else {
            "UPSTREAM_INVALID_RESPONSE"
        })
    );
    assert_eq!(events.last().unwrap()["type"], "response.failed");
    assert_eq!(result.usage.unwrap().total_tokens, 12);
    assert_eq!(
        interrupts.load(Ordering::SeqCst),
        0,
        "a consumed terminal event must not trigger a second interrupt"
    );
    assert!(
        result.cleanup_confirmed,
        "tool output failure must not retire a completed host"
    );
}

#[tokio::test]
async fn unusable_tool_output_is_interrupted_instead_of_parked() {
    if !crate::native_cache::enabled() {
        return;
    }
    for case in ["incomplete_call", "incomplete_reasoning", "invalid_json"] {
        let (client, peer) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        let rpc = Rpc::from_io(read, write);
        let interrupts = Arc::new(AtomicUsize::new(0));
        let seen = interrupts.clone();
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                let mut events = vec![json!({"id":frame["id"],"result":{}})];
                match frame["method"].as_str().unwrap() {
                    "thread/start" => {
                        events[0]["result"] = json!({"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"})
                    }
                    "thread/inject_items" => {}
                    "turn/start" => {
                        events[0]["result"] = json!({"turn":{"id":"turn"}});
                        if case == "incomplete_reasoning" {
                            events.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"reasoning","id":"rs","summary":[],"status":"incomplete"}}}));
                        }
                        if case == "invalid_json" {
                            for id in ["msg_a", "msg_b"] {
                                events.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"message","id":id,"role":"assistant","content":[{"type":"output_text","text":"{}"}]}}}));
                            }
                        }
                        events.extend([
                            json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call","id":"fc","call_id":"echo","name":"echo","arguments":"{}","status":if case == "incomplete_call" {"incomplete"} else {"completed"}}}}),
                            json!({"method":"item/tool/call","id":"callback","params":{"threadId":"thread","turnId":"turn","callId":"echo","tool":"echo","arguments":{}}}),
                            json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}}),
                        ]);
                    }
                    "turn/interrupt" => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        events.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}}));
                    }
                    other => panic!("unexpected operation {other}"),
                }
                for event in events {
                    write
                        .write_all(format!("{event}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let request = Request::parse(
            json!({"model":"m","input":"echo","tools":[{"type":"function","name":"echo"}],"text":{"format":{"type":if case == "invalid_json" {"json_object"} else {"text"}}}}),
        )
        .unwrap();
        let provider = Provider {
            execution: Default::default(),
            id: "p".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1/v1".into(),
            auth: "api-key".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let directory = tempfile::tempdir().unwrap();
        let owner = crate::operations::ClientAccess {
            fast_mode: false,
            key_id: uuid::Uuid::new_v4(),
            group_id: uuid::Uuid::new_v4(),
            models: vec![],
        };
        let mut session = None;
        let mut continuation = continuation::Continuation::default();
        let (tx, mut rx) = mpsc::channel(32);
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run_profiled(
                &rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                tx,
                CancellationToken::new(),
                None,
                Some(reuse::Reuse {
                    access: &owner,
                    session: &mut session,
                    continuation: Some(&mut continuation),
                    default_summary: None,
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        server.abort();
        assert_eq!(
            result.outcome,
            if case == "invalid_json" {
                "failed"
            } else {
                "incomplete"
            }
        );
        assert_eq!(
            events.last().unwrap()["type"],
            if case == "invalid_json" {
                "response.failed"
            } else {
                "response.incomplete"
            }
        );
        assert_eq!(
            events.last().unwrap()["response"]["usage"]["total_tokens"],
            12
        );
        assert!(session.is_none());
        assert!(
            continuation.waiting.is_none(),
            "incomplete output must not transfer a live turn"
        );
        assert_eq!(interrupts.load(Ordering::SeqCst), 1);
        assert!(result.cleanup_confirmed);
    }
}
