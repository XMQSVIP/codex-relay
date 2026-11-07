use super::*;
use crate::{
    responses::continuation::{Continuation, TurnGuard, Waiting},
    rpc::Rpc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn cancelled_uncommitted_tool_wait_is_cleaned_before_context_close() {
    let (client, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    let rpc = Rpc::from_io(read, write);
    let events = rpc.subscribe_thread("thread");
    let mut continuation = Continuation {
        waiting: Some(Waiting::new(TurnGuard::new(
            rpc.clone(),
            "thread".into(),
            "turn".into(),
            events,
            None,
        ))),
        ..Default::default()
    };
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        for method in ["turn/interrupt", "thread/unsubscribe"] {
            let frame: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(
                frame["method"], method,
                "the live turn must stop before context closure"
            );
            write
                .write_all(format!("{}\n", json!({"id":frame["id"],"result":{}})).as_bytes())
                .await
                .unwrap();
            if method == "turn/interrupt" {
                write.write_all(format!("{}\n",json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}})).as_bytes()).await.unwrap();
            }
        }
        std::future::pending::<()>().await;
    });
    let mut summary = crate::upstream::summary(
        &json!({"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}),
        None,
    );
    summary.cleanup_confirmed = false;
    summary.native_status = Some("waiting_for_tool".into());
    let usage = summary.usage.clone();
    let mut result = Ok(summary);
    let clean = cleanup_unretained_waiting(&mut continuation, &mut result).await;
    assert!(
        clean,
        "confirmed interruption must not poison the shared host"
    );
    assert!(continuation.waiting.is_none());
    let summary = result.unwrap();
    assert!(summary.cleanup_confirmed);
    assert_eq!(summary.outcome, "completed");
    assert_eq!(summary.usage, usage);
    assert_eq!(summary.native_status, None);
    rpc.request("thread/unsubscribe", json!({"threadId":"thread"}))
        .await
        .unwrap();
    server.abort();
    rpc.close();
}
