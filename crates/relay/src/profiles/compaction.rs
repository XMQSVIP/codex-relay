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
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
    {
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
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "压缩请求已取消。")),
                result = tokio::time::timeout(std::time::Duration::from_secs(5), outgoing.send(event)) => {
                    result.map_err(|_| Error::new("STREAM_BACKPRESSURE", "下游读取过慢。"))?
                        .map_err(|_| Error::new("STREAM_CONSUMER_CLOSED", "下游已断开。"))?;
                }
            }
            if done {
                return Ok(());
            }
        }
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
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
