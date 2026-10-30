use super::*;
use crate::{
    provider::{Execution, Provider},
    rpc::Rpc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

fn provider() -> Provider {
    Provider {
        execution: Execution::Native,
        id: "p".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1/v1".into(),
        auth: "api-key".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}

fn compaction(method: &str, turn: &str, id: &str) -> Value {
    json!({"method":method,"params":{"threadId":"thread","turnId":turn,
        "item":{"type":"contextCompaction","id":id}}})
}

async fn roundtrip(mode: &'static str, count: usize) {
    let (client, peer) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    let cancel = CancellationToken::new();
    let native_cancel = cancel.clone();
    let (done, finished) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let (mut starts, mut turns, mut injections, mut interrupts) = (0, 0, vec![], 0);
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            let mut out = vec![];
            match frame["method"].as_str() {
                Some("thread/start") => {
                    starts += 1;
                    out.push(json!({"id":frame["id"],"result":{"thread":{"id":"thread"},
                        "model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"}}));
                }
                Some("thread/inject_items") => {
                    injections.push(frame["params"]["items"].clone());
                    out.push(json!({"id":frame["id"],"result":{}}));
                }
                Some("turn/start") => {
                    turns += 1;
                    out.push(json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}}));
                    if turns == 1 {
                        out.push(compaction("item/started", "turn", "pre"));
                        if !matches!(mode, "unfinished" | "cancelled") {
                            out.push(compaction(
                                "item/completed",
                                if mode == "wrong-turn" {
                                    "sibling"
                                } else {
                                    "turn"
                                },
                                if mode == "wrong-item" {
                                    "different"
                                } else {
                                    "pre"
                                },
                            ));
                        }
                    }
                    if mode == "cancelled" {
                        // Leave the compaction in progress until the caller cancels.
                    } else if mode == "closed" {
                        out.push(json!({"method":"thread/closed","params":{"threadId":"thread"}}));
                    } else {
                        out.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"other-turn","status":"failed"}}}));
                        out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn",
                            "item":{"type":"message","id":format!("answer-{turns}"),"role":"assistant","content":[{"type":"output_text","text":"answer"}]}}}));
                        if turns == 2 {
                            out.push(compaction("item/started", "turn", "post"));
                            out.push(compaction("item/completed", "turn", "post"));
                        }
                        out.push(json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn",
                            "usage":{"inputTokens":20,"outputTokens":2,"totalTokens":22}}}));
                        out.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}));
                    }
                }
                Some("turn/interrupt") => {
                    interrupts += 1;
                    out.push(json!({"id":frame["id"],"result":{}}));
                    out.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}}));
                }
                other => panic!("unexpected native call: {other:?}"),
            }
            for event in out {
                write
                    .write_all(format!("{event}\n").as_bytes())
                    .await
                    .unwrap();
            }
            if mode == "cancelled" && interrupts == 0 {
                native_cancel.cancel();
            }
            if turns == count && (!matches!(mode, "closed" | "cancelled") || interrupts == 1) {
                let _ = finished.await;
                return (starts, injections, interrupts);
            }
        }
        panic!("native stream closed before completion");
    });
    let owner = ClientAccess {
        fast_mode: false,
        key_id: Uuid::new_v4(),
        group_id: Uuid::new_v4(),
        models: vec![],
    };
    let directory = tempfile::tempdir().unwrap();
    let mut session = None;
    let mut history = vec![];
    let mut checkpoint = None;
    for index in 0..count {
        history.push(json!({"role":"user","content":format!("question-{index}")}));
        let mut request = Request::parse(json!({"model":"m","input":history})).unwrap();
        request.client_session = Some([1; 32]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(128);
        let summary = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            super::super::run_profiled(
                &rpc,
                &provider(),
                directory.path(),
                &request,
                &request.input,
                tx,
                cancel.clone(),
                None,
                Some(Reuse {
                    default_summary: None,
                    access: &owner,
                    session: &mut session,
                    continuation: None,
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(summary.cleanup_confirmed);
        if mode == "closed" {
            assert_eq!(summary.outcome, "failed");
            assert_eq!(summary.error_code.as_deref(), Some("NATIVE_THREAD_CLOSED"));
        } else if mode == "cancelled" {
            assert_eq!(summary.outcome, "cancelled");
            assert_eq!(summary.error_code.as_deref(), Some("REQUEST_CANCELLED"));
        } else {
            assert_eq!(summary.outcome, "completed");
            assert_eq!(summary.usage.unwrap().cached_input_tokens, None);
        }
        while let Some(event) = rx.recv().await {
            if event["type"] == "response.completed" {
                history.extend(event["response"]["output"].as_array().unwrap().clone());
            }
        }
        if mode != "confirmed" || !crate::native_cache::enabled() {
            assert!(session.is_none());
            continue;
        }
        let session = session.as_ref().unwrap();
        assert_eq!(session.native_checkpoint != checkpoint, index < 2);
        checkpoint = session.native_checkpoint;
        let mut appended = Request::parse(json!({"model":"m","input":history})).unwrap();
        appended.client_session = request.client_session;
        appended.input.push(
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}),
        );
        assert_eq!(
            session
                .continuation(&owner, &appended, &appended.input)
                .unwrap(),
            Some(history.len())
        );
        // Native compaction never authorizes a client history edit or a sibling,
        // nor a client-created replacement window that has not been verified.
        let mut edited = appended.input.clone();
        edited[0]["content"] = json!("edited");
        assert!(
            session
                .continuation(&owner, &appended, &edited)
                .unwrap()
                .is_none()
        );
        let replaced = vec![
            json!({"type":"compaction","encrypted_content":"unrelated"}),
            appended.input.last().unwrap().clone(),
        ];
        assert!(
            session
                .continuation(&owner, &appended, &replaced)
                .unwrap()
                .is_none()
        );
        for (other, rejected) in [
            (
                ClientAccess {
                    key_id: Uuid::new_v4(),
                    ..owner.clone()
                },
                true,
            ),
            (
                ClientAccess {
                    group_id: Uuid::new_v4(),
                    ..owner.clone()
                },
                false,
            ),
        ] {
            assert_eq!(
                session
                    .continuation(&other, &appended, &appended.input)
                    .unwrap()
                    .is_none(),
                rejected
            );
        }
        appended.client_session = Some([2; 32]);
        assert!(
            session
                .continuation(&owner, &appended, &appended.input)
                .unwrap()
                .is_none()
        );
    }
    done.send(()).unwrap();
    let (starts, injections, interrupts) = server.await.unwrap();
    assert_eq!(starts, 1);
    assert_eq!(
        interrupts,
        usize::from(matches!(mode, "closed" | "cancelled"))
    );
    assert_eq!(injections.len(), count);
    for (index, items) in injections.iter().enumerate() {
        assert_eq!(
            items.as_array().unwrap().len(),
            1,
            "old client history must never be reinjected after compaction"
        );
        assert_eq!(items[0]["content"][0]["text"], format!("question-{index}"));
    }
    rpc.close();
}

#[tokio::test]
async fn client_ledger_survives_pre_and_post_turn_compaction_without_replaying_history() {
    roundtrip(
        "confirmed",
        if crate::native_cache::enabled() { 3 } else { 1 },
    )
    .await;
}

#[tokio::test]
async fn unconfirmed_compaction_or_closed_thread_never_enters_the_reuse_pool() {
    for mode in [
        "unfinished",
        "wrong-turn",
        "wrong-item",
        "closed",
        "cancelled",
    ] {
        roundtrip(mode, 1).await;
    }
}
