//! Explicit remote compaction. Payloads remain in memory; app-server supplies auth only.
use super::Profiles;
use crate::{
    error::{Error, Result},
    execution::ExecutionOutcome,
    inference::RequestJob,
    operations::Operations,
    responses::Request,
    streaming::StreamSummary,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 64 * 1024 * 1024;

impl Profiles {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn compaction_request(
        &self,
        job: &RequestJob,
        ops: &Operations,
        request: &Request,
        history: &[Value],
        outgoing: mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> ExecutionOutcome {
        let body = payload(request, history);
        let mut observed = None;
        let observation = &mut observed;
        let result = self
            .standalone_request(
                job,
                ops,
                "responses",
                cancel.clone(),
                |client, url, mut headers| async move {
                    headers.insert(
                        reqwest::header::CONTENT_TYPE,
                        "application/json".parse().unwrap(),
                    );
                    headers.insert(
                        reqwest::header::ACCEPT,
                        "text/event-stream".parse().unwrap(),
                    );
                    // One POST only. An interrupted/unknown result must never be replayed.
                    let response = client
                        .post(url)
                        .headers(headers)
                        .json(&body)
                        .timeout(std::time::Duration::from_secs(300))
                        .send()
                        .await
                        .map_err(crate::upstream::http_error)?;
                    read(response, &outgoing, &cancel, observation).await
                },
            )
            .await;
        match (observed, result) {
            (Some(mut model), result) => {
                model.cleanup_confirmed = result.is_ok();
                ExecutionOutcome {
                    model: Ok(model),
                    cleanup_error: result.err(),
                }
            }
            (None, Err(error)) => ExecutionOutcome::failed(error),
            (None, Ok(())) => ExecutionOutcome::failed(invalid()),
        }
    }
}

fn payload(request: &Request, history: &[Value]) -> Value {
    let mut body = request.compaction_body.clone().unwrap_or_else(|| json!({}));
    // Local identities/transport controls are not upstream parameters. Keep actual
    // model options (including tool schemas) instead of RPC compatibility rewrites.
    body.as_object_mut().unwrap().retain(|key, _| {
        [
            "model",
            "instructions",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "text",
            "service_tier",
            "include",
            "max_output_tokens",
            "metadata",
            "access_programs",
        ]
        .contains(&key.as_str())
    });
    body["model"] = json!(request.model);
    if body["instructions"].is_null() {
        body["instructions"] = json!(request.instructions.as_deref().unwrap_or(""));
    }
    if body["tools"].is_null() {
        body["tools"] = json!([]);
    }
    let mut input = history.to_vec();
    for item in &mut input {
        if matches!(
            item["type"].as_str(),
            Some("custom_tool_call" | "tool_search_call")
        ) && item["id"].as_str().is_some_and(|id| id.starts_with("fc_"))
        {
            // Earlier translated calls retained the native function ID. These
            // optional item IDs are invalid for the translated wire type. Keep
            // call_id and all opaque fields so existing chats can still compact.
            item.as_object_mut().unwrap().remove("id");
        }
    }
    if input
        .last()
        .is_none_or(|item| item["type"] != "compaction_trigger")
    {
        input.push(json!({"type":"compaction_trigger"}));
    }
    body["input"] = json!(input);
    body["stream"] = json!(true);
    body["store"] = json!(false);
    body
}

fn invalid() -> Error {
    Error::new(
        "UPSTREAM_INVALID_RESPONSE",
        "上游未返回完整的远程压缩结果。",
    )
}

#[derive(Default)]
struct Collector {
    items: Vec<Value>,
}
impl Collector {
    fn accept(&mut self, event: &mut Value) -> Result<Option<StreamSummary>> {
        match event["type"].as_str() {
            Some("response.output_item.done") => {
                if self.items.len() >= 1024 {
                    return Err(invalid());
                }
                self.items.push(event["item"].clone());
            }
            Some("response.completed") => {
                let response = &mut event["response"];
                if response["output"].as_array().is_none_or(Vec::is_empty) {
                    response["output"] = json!(self.items);
                }
                let output = response["output"].as_array().ok_or_else(invalid)?;
                if !output.iter().any(|item| {
                    item["type"] == "compaction"
                        && item["encrypted_content"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                }) {
                    return Err(invalid());
                }
                if response["status"]
                    .as_str()
                    .is_some_and(|s| s != "completed")
                {
                    return Err(invalid());
                }
                // Output item values are never decoded, synthesized, or rewritten.
                return Ok(Some(crate::upstream::summary(response, None)));
            }
            Some("response.failed" | "response.incomplete") => {
                let mut summary = crate::upstream::summary(&event["response"], None);
                summary.outcome = "failed".into();
                summary.error_code = Some("UPSTREAM_REQUEST_FAILED".into());
                summary.error_message = Some("上游远程压缩未完成。".into());
                // Native/provider error text can echo history. Only our diagnostic
                // reaches the response or the failure journal.
                event["response"]["output"] = json!([]);
                event["response"]["error"] =
                    json!({"code":"UPSTREAM_REQUEST_FAILED","message":"上游远程压缩未完成。"});
                return Ok(Some(summary));
            }
            Some("error") => {
                return Err(Error::new(
                    "UPSTREAM_REQUEST_FAILED",
                    "上游远程压缩未完成。",
                ));
            }
            _ => {}
        }
        Ok(None)
    }
}

async fn read(
    response: reqwest::Response,
    outgoing: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
    observed: &mut Option<StreamSummary>,
) -> Result<()> {
    if !response.status().is_success() {
        return Err(crate::upstream::status_error(response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if crate::http_media::is_type(content_type, "application/json") {
        let mut document = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(crate::upstream::http_error)?;
            document.extend_from_slice(&chunk);
            if document.len() > LIMIT {
                return Err(invalid());
            }
        }
        let response = serde_json::from_slice::<Value>(&document).map_err(|_| invalid())?;
        let (mut event, summary) = json_compaction_event(response)?;
        *observed = Some(summary);
        event["sequence_number"] = json!(0u64);
        send_event(event, outgoing, cancel).await?;
        return Ok(());
    }
    // The ChatGPT Codex endpoint currently omits Content-Type on some valid
    // streamed compaction responses. Treat an absent header as SSE and still
    // reject an explicitly incompatible media type.
    if !content_type.is_empty() && !crate::http_media::is_type(content_type, "text/event-stream") {
        return Err(invalid());
    }
    let mut stream = response.bytes_stream();
    let mut parser = crate::upstream::dispatch::Events::default();
    let mut collector = Collector::default();
    let mut bytes = 0usize;
    let mut sequence = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(crate::upstream::http_error)?;
        bytes = bytes.saturating_add(chunk.len());
        if bytes > LIMIT {
            return Err(invalid());
        }
        for mut event in parser.push(&chunk)? {
            let summary = collector.accept(&mut event)?;
            let done = summary.is_some();
            if done {
                *observed = summary;
            }
            event["sequence_number"] = json!(sequence);
            sequence += 1;
            send_event(event, outgoing, cancel).await?;
            if done {
                return Ok(());
            }
        }
    }
    Err(invalid())
}

fn json_compaction_event(response: Value) -> Result<(Value, StreamSummary)> {
    let mut event = json!({"type":"response.completed", "response":response});
    // Some compatible upstreams return the standalone compaction object even
    // when the request asked for a streamed Responses result. Normalize only
    // the missing envelope status; the opaque output items remain untouched.
    if event["response"]["status"].is_null() {
        event["response"]["status"] = json!("completed");
    }
    let summary = Collector::default()
        .accept(&mut event)?
        .ok_or_else(invalid)?;
    Ok((event, summary))
}

async fn send_event(
    event: Value,
    outgoing: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "压缩请求已取消。")),
        result = tokio::time::timeout(std::time::Duration::from_secs(5), outgoing.send(event)) => {
            result.map_err(|_| Error::new("STREAM_BACKPRESSURE", "下游读取过慢。"))?
                .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "下游已断开。"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn response_media_type_variants_preserve_compaction_results() {
        let response = json!({
            "id":"cmp_response", "status":"completed",
            "output":[{"type":"compaction","encrypted_content":"opaque"}],
            "usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}
        });
        for (content_type, is_json) in [
            (Some("Application/JSON; charset=utf-8"), true),
            (Some("Text/Event-Stream"), false),
            (Some("TEXT/EVENT-STREAM ; charset=utf-8"), false),
            (None, false),
        ] {
            let body = if is_json {
                response.to_string()
            } else {
                format!(
                    "\u{feff}data: {}\r\r",
                    json!({"type":"response.completed","response":response})
                )
            };
            let mut builder = axum::http::Response::builder();
            if let Some(content_type) = content_type {
                builder = builder.header("content-type", content_type);
            }
            let http = reqwest::Response::from(builder.body(reqwest::Body::from(body)).unwrap());
            let (tx, mut rx) = mpsc::channel(8);
            let mut observed = None;
            read(http, &tx, &CancellationToken::new(), &mut observed)
                .await
                .unwrap();
            let event = rx.recv().await.unwrap();
            assert_eq!(event["response"], response, "{content_type:?}");
            assert_eq!(event["sequence_number"], 0);
            assert_eq!(observed.unwrap().usage.unwrap().total_tokens, 6);
        }
    }

    #[tokio::test]
    async fn incompatible_response_media_type_prefixes_are_rejected() {
        for content_type in ["text/event-streaming", "application/jsonp"] {
            let http = reqwest::Response::from(
                axum::http::Response::builder()
                    .header("content-type", content_type)
                    .body(reqwest::Body::from("{}"))
                    .unwrap(),
            );
            let (tx, _) = mpsc::channel(8);
            let mut observed = None;
            assert_eq!(
                read(http, &tx, &CancellationToken::new(), &mut observed)
                    .await
                    .unwrap_err()
                    .code,
                "UPSTREAM_INVALID_RESPONSE",
                "{content_type}"
            );
            assert!(observed.is_none());
        }
    }

    #[test]
    fn real_items_are_preserved_and_usage_unknown_stays_unknown() {
        let item = json!({"type":"compaction","id":"cmp_test","encrypted_content":"opaque", "future_field":{"v":1}});
        let mut collector = Collector::default();
        collector
            .accept(&mut json!({"type":"response.output_item.done","item":item}))
            .unwrap();
        let mut terminal = json!({"type":"response.completed","response":{"id":"r","usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}}});
        let summary = collector.accept(&mut terminal).unwrap().unwrap();
        assert_eq!(terminal["response"]["output"], json!([item]));
        assert_eq!(summary.usage.unwrap().cached_input_tokens, None);
        assert!(
            Collector::default()
                .accept(&mut json!({"type":"response.completed","response":{"output":[]}}))
                .is_err()
        );
        assert_eq!(Collector::default().accept(&mut json!({"type":"response.failed","response":{"usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}}})).unwrap().unwrap().usage.unwrap().total_tokens, 6);
    }
    #[test]
    fn json_compaction_response_is_wrapped_as_completed_event() {
        let response = json!({
            "object": "response.compaction",
            "id": "cmp_response",
            "output": [{"type":"compaction","encrypted_content":"opaque"}]
        });
        let (event, summary) = json_compaction_event(response).unwrap();
        assert_eq!(event["type"], "response.completed");
        assert_eq!(event["response"]["status"], "completed");
        assert_eq!(
            event["response"]["output"][0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(summary.outcome, "completed");
    }
    #[test]
    fn compaction_omits_only_function_ids_on_previously_translated_calls() {
        let request = Request::parse(json!({"model":"m","input":"test"})).unwrap();
        let history = vec![
            json!({"type":"custom_tool_call","id":"fc_old","call_id":"call_old",
                "name":"exec","input":"code","encrypted_content":"opaque",
                "internal_chat_message_metadata_passthrough":{"turn_id":"original"}}),
            json!({"type":"custom_tool_call_output","id":"ctco_old","call_id":"call_old","output":"result"}),
            json!({"type":"custom_tool_call","id":"ctc_valid","call_id":"call_valid","name":"exec","input":"code"}),
            json!({"type":"tool_search_call","id":"fc_search","execution":"client","call_id":"call_search","arguments":{}}),
            json!({"type":"tool_search_call","id":"tsc_valid","execution":"client","call_id":"call_search_valid","arguments":{}}),
            json!({"type":"function_call","id":"fc_valid","call_id":"call_function","name":"echo","arguments":"{}"}),
            json!({"type":"compaction","id":"cmp_valid","encrypted_content":"opaque_compaction"}),
        ];
        let original = history.clone();
        let body = payload(&request, &history);
        let mut expected = original.clone();
        for index in [0, 3] {
            expected[index].as_object_mut().unwrap().remove("id");
        }
        expected.push(json!({"type":"compaction_trigger"}));
        assert_eq!(body["input"], json!(expected));
        assert_eq!(history, original);
    }

    #[test]
    fn payload_keeps_history_and_options_without_duplicate_trigger_or_local_identity() {
        let request = Request::parse(json!({"model":"m","input":[{"role":"user","content":"secret"},{"type":"compaction_trigger"}],"thread_id":"local","prompt_cache_key":"routing", "store":true,"parallel_tool_calls":false,"tools":[{"type":"function","name":"f","parameters":{"type":"object"},"strict":true}]})).unwrap();
        let body = payload(&request, &request.input);
        assert_eq!(body["input"], json!(request.input));
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["store"], false);
        assert!(body.get("thread_id").is_none() && body.get("prompt_cache_key").is_none());
        assert_eq!(
            payload(&request, &request.input[..1])["input"],
            body["input"]
        );
    }
}
