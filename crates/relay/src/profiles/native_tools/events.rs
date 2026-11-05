use crate::{
    error::{Error, Result},
    rpc::Rpc,
    streaming::Usage,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn invalid(message: &str) -> Error {
    Error::new("NATIVE_TOOL_PROTOCOL", message)
}

#[derive(Default)]
struct Call {
    completed: bool,
    output_received: bool,
}

#[derive(Default)]
struct Output {
    images: Vec<Value>,
    results: Vec<Value>,
    text: Vec<String>,
    calls: std::collections::HashMap<String, Call>,
    usage: Option<Usage>,
    failed: bool,
}
impl Output {
    fn consume(&mut self, event: &Value, image: bool) -> Result<()> {
        let p = &event["params"];
        match event["method"].as_str().unwrap_or("") {
            "bridge/callbackRejected" => return Err(invalid("官方工具请求了未开放的宿主能力。")),
            "rawResponse/completed" if !p["usage"].is_null() => {
                let usage = Usage::parse(&p["usage"])?;
                if let Some(total) = &mut self.usage {
                    total.accumulate(usage)?;
                } else {
                    self.usage = Some(usage);
                }
            }
            "rawResponseItem/completed" => {
                let item = &p["item"];
                if item["type"] == "function_call" {
                    let valid = if image {
                        item["namespace"] == "image_gen" && item["name"] == "imagegen"
                    } else {
                        item["namespace"] == "web" && item["name"] == "run"
                    };
                    if !valid {
                        return Err(invalid("模型选择了未开放的官方工具。"));
                    }
                    let id = item["call_id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                        .ok_or_else(|| invalid("官方工具调用缺少有效 call_id。"))?;
                    if self.calls.len() >= crate::media::HISTORY_COUNT
                        || self.calls.insert(id.to_owned(), Call::default()).is_some()
                    {
                        return Err(invalid("官方工具调用过多或 call_id 重复。"));
                    }
                    if image && self.calls.len() > 1 {
                        return Err(invalid("模型请求了多次图片工具；已终止，不再执行或重放。"));
                    }
                }
                if !image
                    && item["type"] == "function_call_output"
                    && item["call_id"]
                        .as_str()
                        .is_some_and(|id| self.calls.contains_key(id))
                {
                    let call = self
                        .calls
                        .get_mut(item["call_id"].as_str().unwrap())
                        .unwrap();
                    if call.output_received
                        || !(item["output"].is_string() || item["output"].is_array())
                    {
                        return Err(invalid("官方搜索工具结果重复或无效。"));
                    }
                    call.output_received = true;
                    if let Some(s) = item["output"].as_str() {
                        self.text.push(s.into());
                    }
                    if let Some(items) = item["output"].as_array() {
                        for item in items {
                            if item["type"] == "input_text"
                                && let Some(text) = item["text"].as_str()
                            {
                                self.text.push(text.into());
                            }
                        }
                    }
                }
            }
            "item/completed" => {
                let item = &p["item"];
                if (image && item["type"] == "imageGeneration")
                    || (!image && item["type"] == "webSearch")
                {
                    let call = item["id"]
                        .as_str()
                        .and_then(|id| self.calls.get_mut(id))
                        .ok_or_else(|| invalid("官方工具完成事件不属于本轮调用。"))?;
                    if call.completed {
                        return Err(invalid("官方工具重复完成。"));
                    }
                    call.completed = true;
                }
                if image && item["type"] == "imageGeneration" {
                    if item["status"] != "completed" {
                        self.failed = true;
                        return Ok(());
                    }
                    let encoded = item["result"]
                        .as_str()
                        .ok_or_else(|| invalid("官方工具没有返回内联图片。"))?;
                    let data = STANDARD
                        .decode(encoded)
                        .map_err(|_| invalid("官方图片编码无效。"))?;
                    if data.len() > crate::images::RESPONSE_LIMIT
                        || !data.starts_with(b"\x89PNG\r\n\x1a\n")
                    {
                        return Err(invalid("官方图片不是有效大小的 PNG。"));
                    }
                    let mut reader = image::ImageReader::with_format(
                        std::io::Cursor::new(&data),
                        image::ImageFormat::Png,
                    );
                    let mut limits = image::Limits::default();
                    limits.max_alloc = Some(64 * 1024 * 1024);
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    reader.limits(limits);
                    reader
                        .decode()
                        .map_err(|_| invalid("官方图片无法完整解码或超过尺寸限制。"))?;
                    self.images
                        .push(json!({"b64_json":encoded,"revised_prompt":item["revisedPrompt"]}));
                } else if !image && item["type"] == "webSearch" {
                    if (!item["status"].is_null() && item["status"] != "completed")
                        || !item["action"].is_object()
                    {
                        return Err(Error::new(
                            "UPSTREAM_REQUEST_FAILED",
                            "官方搜索工具未完成，未重发请求。",
                        ));
                    }
                    if let Some(results) = item["results"].as_array() {
                        self.results.extend(results.iter().cloned());
                    }
                }
            }
            _ => {}
        }
        if self.images.len() > 1 {
            return Err(invalid("官方工具返回了多个图片结果，不能视为单张请求。"));
        }
        if serde_json::to_vec(
            &json!({"images":self.images,"results":self.results,"text":self.text}),
        )?
        .len()
            > crate::images::RESPONSE_LIMIT
        {
            return Err(Error::new(
                "PAYLOAD_TOO_LARGE",
                "官方工具结果超过大小限制。",
            ));
        }
        Ok(())
    }
    fn finish(self, image: bool) -> Result<Value> {
        if self.failed {
            return Err(Error::new(
                "UPSTREAM_REQUEST_FAILED",
                "官方图片工具执行失败，未重发请求。",
            ));
        }
        if image {
            if self.images.len() != 1 {
                return Err(invalid("模型未完成所请求的图片工具。"));
            }
            Ok(
                json!({"created":crate::now_ms()/1000,"data":self.images,"usage":null,"native_usage":self.usage,"transport":"official-app-server-tool-turn"}),
            )
        } else {
            if self.text.is_empty()
                || self.calls.is_empty()
                || self
                    .calls
                    .values()
                    .any(|call| !call.completed || !call.output_received)
            {
                return Err(invalid("模型未完成所请求的搜索工具。"));
            }
            let usage = self.usage.as_ref().map(Usage::responses);
            Ok(
                json!({"output":self.text.join("\n"),"results":self.results,"usage":usage,"native_usage":self.usage,"transport":"official-app-server-tool-turn"}),
            )
        }
    }
}

pub(super) async fn collect(
    rpc: &Rpc,
    thread: &str,
    turn: &str,
    image: bool,
    events: &mut broadcast::Receiver<Value>,
    cancel: CancellationToken,
    usage: &mut Option<Usage>,
) -> Result<Value> {
    let mut output = Output::default();
    loop {
        let event = tokio::select! {
            biased;
            _=cancel.cancelled()=>return Err(Error::new("REQUEST_CANCELLED","官方工具已取消。")),
            _=rpc.closed()=>return Err(invalid("官方进程断开，未重发请求。")),
            event=events.recv()=>event.map_err(|_|invalid("官方工具事件丢失，未重发请求。"))?,
        };
        let p = &event["params"];
        if event["method"] != "bridge/callbackRejected" && p["threadId"] != thread {
            continue;
        }
        if p.get("turnId").is_some_and(|v| v != turn) {
            continue;
        }
        let consumed = output.consume(&event, image);
        // Keep confirmed model usage even when this collector fails or its
        // future is dropped by cancellation/timeout in the owning tool request.
        *usage = output.usage.clone();
        consumed?;
        if image && (output.failed || !output.images.is_empty()) {
            // The caller stops and joins this exclusive native process before
            // settling the image lease. No model follow-up is needed for delivery.
            return output.finish(true);
        }
        if event["method"] == "turn/completed" && p["turn"]["id"] == turn {
            if p["turn"]["status"] != "completed" {
                return Err(crate::execution::native_failure(&p["turn"]["error"]));
            }
            return output.finish(image);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(item: Value) -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":item}})
    }
    fn search_call(id: &str) -> Value {
        event(json!({"type":"function_call","namespace":"web","name":"run","call_id":id}))
    }
    fn search_done(id: &str) -> Value {
        json!({"method":"item/completed","params":{"item":{"type":"webSearch","id":id,"action":{"type":"search"},"results":[]}}})
    }
    fn search_output(id: &str) -> Value {
        event(json!({"type":"function_call_output","call_id":id,"output":"real search"}))
    }

    #[tokio::test]
    async fn failed_search_keeps_confirmed_usage_outside_the_result() {
        let (client, _peer) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let rpc = Rpc::from_io(read, write);
        let (tx, mut rx) = broadcast::channel(16);
        for mut event in [
            search_call("missing"),
            json!({"method":"rawResponse/completed","params":{"usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}}),
            json!({"method":"turn/completed","params":{"turn":{"id":"turn","status":"completed"}}}),
        ] {
            event["params"]["threadId"] = json!("thread");
            event["params"]["turnId"] = json!("turn");
            tx.send(event).unwrap();
        }
        let mut usage = None;
        let error = collect(
            &rpc,
            "thread",
            "turn",
            false,
            &mut rx,
            CancellationToken::new(),
            &mut usage,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "NATIVE_TOOL_PROTOCOL");
        assert_eq!(usage.unwrap().total_tokens, 12);
        rpc.close();
    }

    #[test]
    fn a_completed_turn_cannot_hide_a_failed_image() {
        let mut output = Output::default();
        output.consume(&event(json!({"type":"function_call","namespace":"image_gen","name":"imagegen","call_id":"image"})), true).unwrap();
        output.consume(&json!({"method":"item/completed","params":{"item":{"type":"imageGeneration","id":"image","status":"failed"}}}),true).unwrap();
        assert_eq!(
            output.finish(true).unwrap_err().code,
            "UPSTREAM_REQUEST_FAILED"
        );
    }

    #[test]
    fn standalone_images_require_a_decodable_png_not_just_its_signature() {
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC").unwrap();
        for (data, valid) in [
            (png.as_slice(), true),
            (&png[..8], false),
            (&png[..40], false),
        ] {
            let mut output = Output::default();
            output.consume(&event(json!({"type":"function_call","namespace":"image_gen","name":"imagegen","call_id":"image"})),true).unwrap();
            let completed = json!({"method":"item/completed","params":{"item":{"type":"imageGeneration","id":"image","status":"completed","result":STANDARD.encode(data)}}});
            assert_eq!(output.consume(&completed, true).is_ok(), valid);
            assert_eq!(output.finish(true).is_ok(), valid);
        }
    }
    #[test]
    fn search_uses_only_the_output_of_the_requested_tool() {
        let mut output = Output::default();
        for item in [
            json!({"type":"function_call","namespace":"web","name":"run","call_id":"s"}),
            json!({"type":"function_call_output","call_id":"other","output":"wrong"}),
            json!({"type":"function_call_output","call_id":"s","output":[{"type":"input_text","text":"real search"}]}),
        ] {
            output
                .consume(
                    &json!({"method":"rawResponseItem/completed","params":{"item":item}}),
                    false,
                )
                .unwrap();
        }
        output.consume(&search_done("s"), false).unwrap();
        let value = output.finish(false).unwrap();
        assert_eq!(value["output"], "real search");
        assert!(value.get("encrypted_output").is_none());
    }

    #[test]
    fn standalone_search_requires_every_call_to_finish_and_return_output() {
        for missing in ["completion", "output", "both"] {
            let mut output = Output::default();
            for event in [
                search_call("first"),
                search_done("first"),
                search_output("first"),
                search_call("second"),
            ] {
                output.consume(&event, false).unwrap();
            }
            if missing == "output" {
                output.consume(&search_done("second"), false).unwrap();
            }
            if missing == "completion" {
                output.consume(&search_output("second"), false).unwrap();
            }
            assert_eq!(
                output.finish(false).unwrap_err().code,
                "NATIVE_TOOL_PROTOCOL",
                "{missing}"
            );
        }
        for reversed in [false, true] {
            let mut output = Output::default();
            for id in ["first", "second"] {
                output.consume(&search_call(id), false).unwrap();
                let tail = if reversed {
                    [search_output(id), search_done(id)]
                } else {
                    [search_done(id), search_output(id)]
                };
                for event in tail {
                    output.consume(&event, false).unwrap();
                }
            }
            assert_eq!(
                output.finish(false).unwrap()["output"],
                "real search\nreal search"
            );
        }
    }

    #[test]
    fn standalone_tool_results_are_scoped_and_duplicates_cannot_hide_missing_calls() {
        for duplicate in [search_call("s"), search_done("s"), search_output("s")] {
            let mut output = Output::default();
            for event in [search_call("s"), search_done("s"), search_output("s")] {
                output.consume(&event, false).unwrap();
            }
            assert!(output.consume(&duplicate, false).is_err());
        }
        assert!(
            Output::default()
                .consume(&search_done("unknown"), false)
                .is_err()
        );
        for id in ["", &"x".repeat(257)] {
            assert!(Output::default().consume(&search_call(id), false).is_err());
        }
    }

    #[test]
    fn standalone_usage_sums_model_steps_without_counting_thread_snapshots() {
        let mut output = Output::default();
        for event in [search_call("s"), search_done("s"), search_output("s")] {
            output.consume(&event, false).unwrap();
        }
        for _ in 0..2 {
            output.consume(&json!({"method":"rawResponse/completed","params":{"usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12,"cachedInputTokens":4,"cacheWriteInputTokens":3,"reasoningOutputTokens":1}}}),false).unwrap();
            output.consume(&json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":10,"outputTokens":2,"totalTokens":12},"total":{"inputTokens":100,"outputTokens":20,"totalTokens":120}}}}),false).unwrap();
        }
        let value = output.finish(false).unwrap();
        assert_eq!(value["usage"]["total_tokens"], 24);
        assert_eq!(value["usage"]["input_tokens_details"]["cached_tokens"], 8);
        assert_eq!(value["native_usage"]["cacheWriteInputTokens"], 6);
        assert_eq!(
            value["usage"]["output_tokens_details"]["reasoning_tokens"],
            2
        );
    }
}
