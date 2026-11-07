use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn login_session_rate_limit_settles_without_waiting_for_client_close_but_api_session_stays_open()
 {
    for auth in ["chatgpt", "native-api-key"] {
        let (client, server) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(client);
        let rpc = Rpc::from_io(reader, writer);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "thread/start" => {
                        json!({"thread":{"id":"thread"},"model":"fixture","modelProvider":"openai","sandbox":{"type":"readOnly"},"approvalPolicy":"untrusted"})
                    }
                    "turn/start" => json!({"turn":{"id":"turn"}}),
                    method => panic!("unexpected RPC: {method}"),
                };
                writer
                    .write_all(
                        format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                    )
                    .await
                    .unwrap();
                if request["method"] == "turn/start" {
                    writer.write_all(format!("{}\n", json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"failed","error":{"codexErrorInfo":"rateLimitExceeded"}}}})).as_bytes()).await.unwrap();
                }
            }
        });
        let provider = Provider {
            execution: crate::provider::Execution::Native,
            id: "openai".into(),
            model: "fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            auth: auth.into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let (input, mut incoming) = mpsc::channel(4);
        let (outgoing, mut output) = mpsc::channel(8);
        let session_rpc = rpc.clone();
        let mut session = tokio::spawn(async move {
            run(
                &session_rpc,
                &provider,
                Path::new("."),
                json!({"id":1,"method":"thread/start","params":{"model":"fixture"}}),
                &mut incoming,
                outgoing,
                CancellationToken::new(),
            )
            .await
        });
        input.send(json!({"id":2,"method":"turn/start","params":{"input":[{"type":"text","text":"fixture"}]}})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = output.recv().await {
                if event["method"] == "turn/completed" {
                    return;
                }
            }
            panic!("native completion was not forwarded");
        })
        .await
        .unwrap();
        if auth == "native-api-key" {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut session)
                    .await
                    .is_err()
            );
            input
                .send(json!({"id":3,"method":"session/close"}))
                .await
                .unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), session)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.error_code.as_deref(), Some("UPSTREAM_RATE_LIMITED"));
        assert_eq!(result.outcome, "failed");
        rpc.close();
        server.abort();
    }
}
