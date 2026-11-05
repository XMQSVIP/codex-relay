use super::*;
use crate::{
    operations::ClientAccess,
    provider::Provider,
    responses::{reuse::Reuse, run_profiled},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn submission_observer_keeps_queued_terminal_cleanup_evidence() {
    for already_queued in [false, true] {
        let (rpc, mut outgoing) = Rpc::new();
        let (events, receiver) = broadcast::channel(8);
        let mut waiting = Waiting::new(TurnGuard::new(
            rpc,
            "thread".into(),
            "turn".into(),
            receiver,
            None,
        ));
        if !already_queued {
            assert!(waiting.submission_valid());
        }
        let terminal = json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}});
        events.send(terminal.clone()).unwrap();
        assert!(!waiting.submission_valid());
        assert!(waiting.guard.completed);
        if !already_queued {
            assert_eq!(
                waiting.guard.events.as_mut().unwrap().try_recv().unwrap(),
                terminal
            );
        }
        assert!(waiting.cleanup().await);
        assert!(
            outgoing.try_recv().is_err(),
            "confirmed completion must not send another interrupt"
        );
    }
}

#[tokio::test]
async fn queued_invalidation_during_resume_preflight_never_answers_old_callbacks() {
    if !crate::native_cache::enabled() {
        return;
    }
    for stage in ["catalog", "settings"] {
        for kind in ["terminal", "delta", "compaction"] {
            let (client, peer) = tokio::io::duplex(65536);
            let (read, write) = tokio::io::split(client);
            let rpc = Rpc::from_io(read, write);
            rpc.enable_client_tools();
            let mut events = rpc.subscribe_thread("thread");
            let answers = Arc::new(AtomicUsize::new(0));
            let seen = answers.clone();
            let server = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(peer);
                write.write_all(format!("{}\n", json!({"id":"callback","method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":"call","tool":"echo","arguments":{}}})).as_bytes()).await.unwrap();
                let mut lines = BufReader::new(read).lines();
                while let Some(line) = lines.next_line().await.unwrap() {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let mut out = Vec::new();
                    match frame["method"].as_str() {
                        Some("model/list" | "turn/settings/update") => {
                            assert_eq!(
                                frame["method"],
                                if stage == "catalog" {
                                    "model/list"
                                } else {
                                    "turn/settings/update"
                                }
                            );
                            out.push(match kind {
                                "terminal" => json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
                                "delta" => json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"msg","delta":"unexpected"}}),
                                _ => json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"id":"compact","type":"contextCompaction"}}}),
                            });
                            let result = if stage == "catalog" {
                                json!({"data":[{"id":"m","model":"m","serviceTiers":[{"id":"ultrafast"}]}],"nextCursor":null})
                            } else {
                                json!({"status":"applied"})
                            };
                            out.push(json!({"id":frame["id"],"result":result}));
                        }
                        None => {
                            seen.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(frame["id"], "callback");
                            out.extend([
                                json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"unexpected response"}]}}}),
                                json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
                            ]);
                        }
                        method => panic!("unexpected native request {method:?}"),
                    }
                    let bytes = out
                        .into_iter()
                        .map(|event| format!("{event}\n"))
                        .collect::<String>();
                    write.write_all(bytes.as_bytes()).await.unwrap();
                }
            });
            let tools = json!([{"type":"function","name":"echo"}]);
            let first = Request::parse(json!({"model":"m","input":"first","tools":tools})).unwrap();
            let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"echo","arguments":"{}","status":"completed"});
            let owner = ClientAccess {
                fast_mode: false,
                key_id: uuid::Uuid::new_v4(),
                group_id: uuid::Uuid::new_v4(),
                models: vec![],
            };
            let directory = tempfile::tempdir().unwrap();
            let mut session = Session::new(
                "thread".into(),
                directory.path().to_path_buf(),
                &owner,
                &first,
                &first.input,
                std::slice::from_ref(&call),
            )
            .unwrap();
            session
                .calls
                .push(callback(&first, &events.recv().await.unwrap()).unwrap());
            let mut session = Some(session);
            let mut continuation = Continuation {
                incoming: Some(Waiting::new(TurnGuard::new(
                    rpc.clone(),
                    "thread".into(),
                    "turn".into(),
                    events,
                    None,
                ))),
                ..Default::default()
            };
            let mut history = first.input.clone();
            history.extend([
                call,
                json!({"type":"function_call_output","call_id":"call","output":"result"}),
            ]);
            let mut body = json!({"model":"m","input":history,"tools":tools});
            if stage == "catalog" {
                body["service_tier"] = json!("ultrafast");
            } else {
                body["reasoning"] = json!({"effort":"high"});
            }
            let next = Request::parse(body).unwrap();
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
            let (tx, _rx) = mpsc::channel(16);
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                run_profiled(
                    &rpc,
                    &provider,
                    directory.path(),
                    &next,
                    &next.input,
                    tx,
                    CancellationToken::new(),
                    None,
                    Some(Reuse {
                        default_summary: None,
                        access: &owner,
                        session: &mut session,
                        continuation: Some(&mut continuation),
                    }),
                ),
            )
            .await
            .unwrap();
            rpc.close();
            server.await.unwrap();
            assert_eq!(
                answers.load(Ordering::SeqCst),
                0,
                "{stage}/{kind}: preflight must not submit stale tool results"
            );
            assert_eq!(result.unwrap_err().code, "NATIVE_TURN_UNAVAILABLE");
            assert!(session.is_none());
            assert!(continuation.incoming.is_none() && continuation.waiting.is_none());
        }
    }
}
