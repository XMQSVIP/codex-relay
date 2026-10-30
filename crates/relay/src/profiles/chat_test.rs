use super::Profiles;
use crate::{
    backend::Backend,
    error::{Error, Result},
    execution::ExecutionOutcome,
    installation, responses,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Profiles {
    pub(crate) async fn test_chat(
        &self,
        id: Uuid,
        model: Option<String>,
        reasoning_effort: Option<String>,
        history: Vec<Value>,
        outgoing: mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> ExecutionOutcome {
        let result = self
            .test_chat_inner(id, model, reasoning_effort, history, outgoing, cancel)
            .await;
        result.unwrap_or_else(ExecutionOutcome::failed)
    }

    async fn test_chat_inner(
        &self,
        id: Uuid,
        model: Option<String>,
        reasoning_effort: Option<String>,
        history: Vec<Value>,
        outgoing: mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> Result<ExecutionOutcome> {
        if self.is_api_key(id).await? {
            let model = model
                .filter(|m| !m.is_empty())
                .ok_or_else(|| Error::new("INVALID_INPUT", "API Key 账户测试须指定模型。"))?;
            let work = async {
                let config = self.upstream(id).await?;
                let client = self.account_proxy(id).resolved().await?.client()?;
                crate::upstream::chat_test::run(
                    config,
                    client,
                    id,
                    model,
                    reasoning_effort,
                    history,
                    outgoing.clone(),
                )
                .await
            };
            return Ok(ExecutionOutcome {
                model: tokio::select! {
                    _=cancel.cancelled()=>Err(Error::new("REQUEST_CANCELLED","测试已停止。")),
                    _=outgoing.closed()=>Err(Error::new("STREAM_CONSUMER_CLOSED","测试窗口已关闭。")),
                    result=tokio::time::timeout(Duration::from_secs(300),work)=>result.unwrap_or_else(|_|Err(Error::new("UPSTREAM_TIMEOUT","测试请求超时。"))),
                },
                cleanup_error: None,
            });
        }
        let directory = self.inspection_directory()?;
        let isolated = self.is_oauth(id).await?;
        let home = self.home(id);
        let setup = async {
            let installation = installation::read_with_refresh().await?;
            Backend::start_test_chat(
                &installation,
                directory.path(),
                if isolated { Some(&home) } else { None },
                &self.account_proxy(id),
            )
            .await
        };
        let backend = Arc::new(tokio::select! {
            _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "测试已停止。")),
            result = tokio::time::timeout(Duration::from_secs(30), setup) => result.map_err(|_| Error::new("UPSTREAM_TIMEOUT", "测试实例启动超时。"))??,
        });
        self.track_client(id, &backend);
        let work = async {
            let provider = self
                .read_route_provider(id, &backend, directory.path(), model.as_deref())
                .await?;
            if let Some(ops) = &self.proxy_settings {
                backend
                    .rpc
                    .set_location(ops.resolve_location(id, false).await);
            }
            outgoing
                .send(json!({"type":"test.ready","model":provider.model}))
                .await
                .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "测试窗口已关闭。"))?;
            let mut body = json!({"model":provider.model,"input":history,
                "stream":true,"store":false,"tools":[],"tool_choice":"none"});
            if let Some(effort) = reasoning_effort {
                body["reasoning"] = json!({"effort":effort});
            }
            let request = responses::Request::parse(body)?;
            // Run the native Responses adapter directly: no response store, affinity,
            // reusable worker, or failure-content middleware receives this transcript.
            responses::run(
                &backend.rpc,
                &provider,
                directory.path(),
                &request,
                &request.input,
                outgoing.clone(),
                cancel.clone(),
            )
            .await
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(Error::new("REQUEST_CANCELLED", "测试已停止。")),
            _ = outgoing.closed() => Err(Error::new("STREAM_CONSUMER_CLOSED", "测试窗口已关闭。")),
            result = tokio::time::timeout(Duration::from_secs(300), work) => result.unwrap_or_else(|_| Err(Error::new("UPSTREAM_TIMEOUT", "测试请求超时。"))),
        };
        let stopped = backend.stop().await;
        let cleaned = directory.close().map_err(Error::from);
        Ok(ExecutionOutcome {
            model: result,
            cleanup_error: stopped.err().or_else(|| cleaned.err()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};

    #[tokio::test]
    #[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; isolated home and local synthetic model only"]
    async fn private_native_chat_leaves_no_transcript() {
        let installation: installation::Installation = serde_json::from_slice(
            &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
        )
        .unwrap();
        installation::validate(&installation).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captures = captured.clone();
        let app=Router::new().route("/v1/responses",post(move |Json(body):Json<Value>| {
            let captures=captures.clone();async move {
                captures.lock().unwrap().push(body);
                let item=json!({"type":"message","id":"msg_private","role":"assistant","status":"completed","content":[{"type":"output_text","text":"PRIVATE_NATIVE_REPLY_SENTINEL","annotations":[]}]});
                let events=[json!({"type":"response.output_item.done","output_index":0,"item":item}),json!({"type":"response.completed","response":{"id":"resp_private","status":"completed","output":[item],"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}})];
                ([("content-type","text/event-stream")],events.iter().map(|v|format!("data: {v}\n\n")).collect::<String>())
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::fs::write(home.join("config.toml"),format!("model=\"gpt-6.1-sol\"\nmodel_provider=\"synthetic\"\n[features]\nenable_request_compression=false\n[model_providers.synthetic]\nname=\"Synthetic\"\nbase_url=\"http://{address}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\nexperimental_bearer_token=\"SYNTHETIC_ONLY\"\n")).unwrap();
        let proxy = serde_json::from_value(json!({"mode":"direct","url":""})).unwrap();
        let backend = Backend::start_test_chat(&installation, &cwd, Some(&home), &proxy)
            .await
            .unwrap();
        let provider = crate::provider::Provider {
            execution: Default::default(),
            id: "synthetic".into(),
            model: "gpt-6.1-sol".into(),
            base_url: format!("http://{address}/v1"),
            auth: "native-bearer".into(),
            required_headers: vec![],
            forward_headers: vec![],
            mcp_servers: vec![],
        };
        let request=responses::Request::parse(json!({"model":"gpt-6.1-sol","store":false,"input":"PRIVATE_NATIVE_PROMPT_SENTINEL","tools":[],"tool_choice":"none"})).unwrap();
        let (tx, mut rx) = mpsc::channel(128);
        let reader = tokio::spawn(async move {
            let mut events = vec![];
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        });
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            responses::run(
                &backend.rpc,
                &provider,
                &cwd,
                &request,
                &request.input,
                tx,
                CancellationToken::new(),
            ),
        )
        .await;
        backend.stop().await.unwrap();
        server.abort();
        let events = reader.await.unwrap();
        assert_eq!(result.unwrap().unwrap().outcome, "completed");
        assert!(events.iter().any(|v| {
            v["type"] == "response.output_text.delta"
                && v["delta"]
                    .as_str()
                    .unwrap_or("")
                    .contains("PRIVATE_NATIVE_REPLY_SENTINEL")
        }));
        assert!(captured.lock().unwrap().iter().any(|v| {
            v["input"]
                .to_string()
                .contains("PRIVATE_NATIVE_PROMPT_SENTINEL")
        }));
        fn scan(path: &std::path::Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    assert!(
                        !bytes
                            .windows(b"PRIVATE_NATIVE_".len())
                            .any(|v| v == b"PRIVATE_NATIVE_"),
                        "transcript persisted: {}",
                        path.display()
                    );
                }
            }
        }
        scan(temp.path());
        assert!(!home.join("history.jsonl").exists());
    }
}
