use super::*;
use crate::{
    operations::ClientAccess,
    provider::{Execution, Provider},
    responses::{reuse::Reuse, run_profiled},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn access() -> ClientAccess {
    ClientAccess {
        fast_mode: false,
        key_id: Uuid::new_v4(),
        group_id: Uuid::new_v4(),
        models: vec![],
    }
}
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

#[tokio::test]
async fn tool_results_resume_the_same_turn_and_usage_is_per_response() {
    tool_roundtrip(false, None, false).await;
}

#[tokio::test]
async fn tool_results_resume_across_native_compactions() {
    tool_roundtrip(true, None, false).await;
}

#[tokio::test]
async fn live_settings_change_before_tool_results_and_do_not_restart_the_turn() {
    tool_roundtrip(false, Some("applied"), false).await;
}

#[tokio::test]
async fn rejected_or_unavailable_settings_never_answer_tools_or_replay() {
    for status in ["rejected", "targetUnavailable", "unknown"] {
        tool_roundtrip(false, Some(status), false).await;
        tool_roundtrip(false, Some(status), true).await;
    }
}

#[tokio::test]
async fn history_settings_update_resumes_the_waiting_turn() {
    tool_roundtrip(false, Some("applied"), true).await;
    tool_roundtrip(true, Some("applied"), true).await;
}

async fn tool_roundtrip(compact: bool, update: Option<&'static str>, history_update: bool) {
    if !crate::native_cache::enabled() {
        return;
    }
    let (client, peer) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    let (done, finished) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let mut starts = 0;
        let mut answers = 0;
        let mut injected = 0;
        let mut updated = false;
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            let mut out = Vec::new();
            match frame["method"].as_str() {
                Some("thread/start")=>out.push(json!({"id":frame["id"],"result":{"thread":{"id":"thread"},"model":"m","modelProvider":"p","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"}})),
                Some("thread/inject_items")=>{injected+=1;out.push(json!({"id":frame["id"],"result":{}}));},
                Some("turn/start")=>{
                    starts+=1;
                    out.push(json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}}));
                    if compact { compaction_events(&mut out, "before-tools"); }
                    out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"reasoning","id":"rs_fixture","summary":[],"encrypted_content":"synthetic-reasoning"}}}));
                    out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call","id":"fc","call_id":"call","name":"echo","arguments":"{}"}}}));
                    out.push(json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":100,"outputTokens":10,"totalTokens":110,"cachedInputTokens":50}}}));
                    // Exercise callbacks arriving after the model response boundary.
                    out.push(json!({"id":"callback","method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":"call","tool":"echo","arguments":{}}}));
                },
                None=>{
                    assert_eq!(updated, update.is_some(), "live settings must be ready before any tool reply");
                    answers+=1;
                    assert_eq!(frame["id"],"callback");
                    assert_eq!(frame["result"],json!({"success":true,"contentItems":[{"type":"inputText","text":"actual tool result"}]}));
                    out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"function_call_output","call_id":"call","output":"actual tool result"}}}));
                    if compact { compaction_events(&mut out, "after-tools"); }
                    out.push(json!({"method":"rawResponseItem/completed","params":{"threadId":"thread","turnId":"turn","item":{"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}}));
                    out.push(json!({"method":"rawResponse/completed","params":{"threadId":"thread","turnId":"turn","usage":{"inputTokens":120,"outputTokens":5,"totalTokens":125,"cachedInputTokens":100}}}));
                    out.push(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}));
                },
                Some("turn/settings/update")=>{
                    assert!(update.is_some() && !updated && answers == 0);
                    assert_eq!(frame["params"], json!({"threadId":"thread","turnId":"turn","effort":"high","summary":"detailed","serviceTier":"priority"}));
                    updated = true;
                    out.push(if update == Some("rejected") {
                        json!({"id":frame["id"],"error":{"code":-32600,"message":"Synthetic update rejection"}})
                    } else {
                        json!({"id":frame["id"],"result":{"status":update.unwrap()}})
                    });
                },
                method=>panic!("unexpected native operation {method:?}"),
            }
            for value in out {
                write
                    .write_all(format!("{value}\n").as_bytes())
                    .await
                    .unwrap();
            }
            if answers == 1 || (updated && update != Some("applied")) {
                let _ = finished.await;
                break;
            }
        }
        (starts, answers, injected)
    });
    let tools =
        json!([{"type":"function","name":"echo","parameters":{"type":"object","properties":{}}}]);
    let request = Request::parse(json!({"model":"m","input":"first","tools":tools})).unwrap();
    let owner = access();
    let directory = tempfile::tempdir().unwrap();
    let mut session = None;
    let mut continuation = Continuation::default();
    let (tx, mut rx) = mpsc::channel(128);
    let summary = run_profiled(
        &rpc,
        &provider(),
        directory.path(),
        &request,
        &request.input,
        tx,
        CancellationToken::new(),
        None,
        Some(Reuse {
            default_summary: None,
            access: &owner,
            session: &mut session,
            continuation: Some(&mut continuation),
        }),
    )
    .await
    .unwrap();
    assert!(!summary.cleanup_confirmed);
    assert_eq!(summary.native_status.as_deref(), Some("waiting_for_tool"));
    assert_eq!(summary.usage.unwrap().cached_input_tokens, Some(50));
    let before = session.as_ref().unwrap().native_checkpoint;
    assert_eq!(before.is_some(), compact);
    let mut first = None;
    while let Some(event) = rx.recv().await {
        if event["type"] == "response.completed" {
            first = Some(event["response"].clone());
        }
    }
    let first = first.unwrap();
    let mut history = request.input.clone();
    history.extend(first["output"].as_array().unwrap().clone());
    // The official client serializes absent reasoning content as null on replay.
    history
        .iter_mut()
        .find(|item| item["type"] == "reasoning")
        .unwrap()["content"] = Value::Null;
    history.push(
        json!({"type":"function_call_output","call_id":"call","output":"actual tool result"}),
    );
    let mut next_body = json!({"model":"m","input":history,"tools":tools});
    if update.is_some() {
        next_body["reasoning"] = json!({"effort":"high","summary":"detailed"});
        next_body["service_tier"] = json!("priority");
        if history_update {
            next_body["reasoning"]["effort"] = json!("low");
            next_body["reasoning"]["context"] = json!("all_turns");
            next_body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"configuration_update","reasoning":{"effort":"high"}}));
        }
    }
    let next = Request::parse(next_body).unwrap();
    continuation.incoming = continuation.waiting.take();
    let (tx, mut rx) = mpsc::channel(128);
    let result = run_profiled(
        &rpc,
        &provider(),
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
    )
    .await;
    if update.is_some_and(|status| status != "applied") {
        let error = result.unwrap_err();
        assert_eq!(
            error.code,
            if update == Some("rejected") {
                "RPC_REMOTE"
            } else {
                "NATIVE_TURN_UNAVAILABLE"
            }
        );
        assert!(session.is_none());
        assert!(continuation.incoming.is_none() && continuation.waiting.is_none());
        done.send(()).unwrap();
        assert_eq!(server.await.unwrap(), (1, 0, 1));
        rpc.close();
        return;
    }
    let summary = result.unwrap();
    assert!(summary.cleanup_confirmed);
    let usage = summary.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.cached_input_tokens),
        (120, Some(100))
    );
    let mut second = None;
    while let Some(event) = rx.recv().await {
        if event["type"] == "response.completed" {
            second = Some(event["response"].clone());
        }
    }
    assert_ne!(first["id"], second.unwrap()["id"]);
    let session = session.unwrap();
    assert!(session.calls.is_empty());
    assert_eq!(session.native_checkpoint != before, compact);
    done.send(()).unwrap();
    assert_eq!(server.await.unwrap(), (1, 1, 1));
    rpc.close();
}

fn compaction_events(out: &mut Vec<Value>, id: &str) {
    for method in ["item/started", "item/completed"] {
        out.push(
            json!({"method":method,"params":{"threadId":"thread","turnId":"turn",
            "item":{"type":"contextCompaction","id":id}}}),
        );
    }
}

#[test]
fn reply_batches_validate_all_calls_and_preserve_error_text() {
    let owner = access();
    let request = Request::parse(json!({"model":"m","input":"hello"})).unwrap();
    let mut session = Session::new(
        "t".into(),
        "unused".into(),
        &owner,
        &request,
        &request.input,
        &[],
    )
    .unwrap();
    session.calls = vec![
        Call {
            id: "a".into(),
            rpc_id: "1".into(),
            name: "f".into(),
            namespace: None,
            custom: false,
            arguments: [0; 32],
        },
        Call {
            id: "b".into(),
            rpc_id: "2".into(),
            name: "g".into(),
            namespace: Some("ns".into()),
            custom: true,
            arguments: [0; 32],
        },
    ];
    let a = json!({"type":"function_call_output","call_id":"a","output":"Error: user tool failed"});
    let b = json!({"type":"custom_tool_call_output","call_id":"b","output":[{"type":"input_text","text":"ok"},{"type":"input_image","image_url":"data:image/png;base64,AA=="}]});
    let values = replies(&session, &[b.clone(), a.clone()], 0).unwrap();
    assert_eq!(
        values[0].1["contentItems"][0]["text"],
        "Error: user tool failed"
    );
    assert_eq!(values[1].1["contentItems"][1]["type"], "inputImage");
    for invalid in [
        vec![a.clone()],
        vec![a.clone(), a.clone()],
        vec![
            a.clone(),
            b.clone(),
            json!({"type":"message","role":"user"}),
        ],
    ] {
        assert!(replies(&session, &invalid, 0).is_none());
    }
    let control = json!({"type":"configuration_update","reasoning":{"effort":"high"}});
    for mixed in [
        vec![control.clone(), a.clone(), b.clone()],
        vec![a.clone(), control.clone(), b.clone()],
        vec![a.clone(), b.clone(), control.clone()],
    ] {
        assert_eq!(
            replies(&session, &mixed, 0).is_some(),
            crate::native_cache::enabled()
        );
    }
    for bad in [
        json!({"type":"configuration_update","reasoning":{"effort":"invalid"}}),
        json!({"type":"configuration_update","reasoning":{"effort":"high"},"call_id":"a"}),
    ] {
        assert!(replies(&session, &[a.clone(), b.clone(), bad], 0).is_none());
    }
    assert!(replies(&session, &[a.clone(), a.clone(), control], 0).is_none());
    let mut detailed = b;
    detailed["output"][1]["detail"] = json!("high");
    assert!(replies(&session, &[a, detailed], 0).is_none());
}
