//! Bounded native history injection; explicit compaction is handled by authenticated HTTP.
use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::{Value, json};

pub(super) async fn inject(rpc: &Rpc, thread: &str, items: Vec<Value>) -> Result<()> {
    let mut batch = vec![];
    // Native requests use UUID string IDs. Include the exact JSON envelope,
    // escaped thread ID and array separators when choosing each batch.
    let envelope = serde_json::to_vec(&json!({"id":uuid::Uuid::nil().to_string(),"method":"thread/inject_items","params":{"threadId":thread,"items":[]}}))?.len();
    let mut size = envelope;
    for item in items {
        let bytes = serde_json::to_vec(&item)?.len();
        if bytes + envelope > crate::rpc::MAX_BYTES {
            return Err(Error::new(
                "PAYLOAD_TOO_LARGE",
                &format!(
                    "单条历史及 RPC 封装为 {} 字节，超过原生 8 MiB 帧上限。",
                    bytes + envelope
                ),
            ));
        }
        if !batch.is_empty() && size + bytes + 1 > crate::rpc::MAX_BYTES {
            rpc.request(
                "thread/inject_items",
                json!({"threadId":thread,"items":batch}),
            )
            .await?;
            batch.clear();
            size = envelope;
        }
        size += bytes + usize::from(!batch.is_empty());
        batch.push(item);
    }
    if !batch.is_empty() {
        rpc.request(
            "thread/inject_items",
            json!({"threadId":thread,"items":batch}),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn explicit_compaction_never_creates_a_thread_or_writes_history() {
        for trigger in [false, true] {
            let input = if trigger {
                json!([{"role":"user","content":"PRIVATE_FIXTURE"},{"type":"compaction_trigger"}])
            } else {
                json!("PRIVATE_FIXTURE")
            };
            let mut request =
                crate::responses::Request::parse(json!({"model":"m","input":input})).unwrap();
            request.compact = true;
            let (client, peer) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(client);
            let rpc = Rpc::from_io(read, write);
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
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let error = crate::responses::run(
                &rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "INVALID_INPUT");
            assert_eq!(
                crate::client_api::client_error_payload(&error).0,
                axum::http::StatusCode::BAD_REQUEST
            );
            assert!(rx.recv().await.is_none());
            assert!(
                std::fs::read_dir(directory.path())
                    .unwrap()
                    .next()
                    .is_none()
            );
            let mut lines = BufReader::new(peer).lines();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                    .await
                    .is_err()
            );
            rpc.close();
        }
    }

    #[tokio::test]
    async fn large_history_is_batched_in_order_and_oversize_items_are_rejected() {
        let (client, peer) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client);
        let rpc = Rpc::from_io(read, write);
        let receiver = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            let mut batches = Vec::new();
            while batches.len() < 2 {
                let line = lines.next_line().await.unwrap().unwrap();
                assert!(line.len() <= crate::rpc::MAX_BYTES);
                let frame: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(frame["method"], "thread/inject_items");
                batches.push(
                    frame["params"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item["id"].as_str().unwrap().to_owned())
                        .collect::<Vec<_>>(),
                );
                write
                    .write_all(format!("{}\n", json!({"id":frame["id"],"result":{}})).as_bytes())
                    .await
                    .unwrap();
            }
            batches
        });
        let items = (0..3).map(|n|json!({"id":format!("item-{n}"),"type":"message","role":"user","content":[{"type":"input_text","text":"x".repeat(3*1024*1024)}]})).collect();
        inject(&rpc, "escaped-\"thread", items).await.unwrap();
        assert_eq!(
            receiver.await.unwrap(),
            vec![vec!["item-0", "item-1"], vec!["item-2"]]
        );
        let error = inject(
            &rpc,
            "t",
            vec![json!({"text":"x".repeat(crate::rpc::MAX_BYTES)})],
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "PAYLOAD_TOO_LARGE");
        rpc.close();
    }
}
