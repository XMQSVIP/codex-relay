use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn invalid(message: &str) -> Error {
    Error::new("NATIVE_TOOL_PROTOCOL", message)
}

#[derive(Default)]
struct Output {
    images: Vec<Value>,
    results: Vec<Value>,
    text: Vec<String>,
    calls: std::collections::HashSet<String>,
    usage: Option<Value>,
    failed: bool,
}
impl Output {
    fn consume(&mut self, event: &Value, image: bool) -> Result<()> {
        let p = &event["params"];
        match event["method"].as_str().unwrap_or("") {
            "bridge/callbackRejected" => return Err(invalid("官方工具请求了未开放的宿主能力。")),
            "thread/tokenUsage/updated" => self.usage = Some(p["tokenUsage"]["last"].clone()),
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
                    if let Some(id) = item["call_id"].as_str() {
                        self.calls.insert(id.to_owned());
                    }
                    if image && self.calls.len() > 1 {
                        return Err(invalid("模型请求了多次图片工具；已终止，不再执行或重放。"));
                    }
                }
                if !image
                    && item["type"] == "function_call_output"
                    && item["call_id"]
                        .as_str()
                        .is_some_and(|id| self.calls.contains(id))
                {
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
                    self.images
                        .push(json!({"b64_json":encoded,"revised_prompt":item["revisedPrompt"]}));
                } else if !image && item["type"] == "webSearch" {
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
            if self.text.is_empty() || self.calls.is_empty() {
                return Err(invalid("模型未完成所请求的搜索工具。"));
            }
            let usage = self
                .usage
                .as_ref()
                .filter(|v| !v.is_null())
                .map(crate::streaming::Usage::parse)
                .transpose()?
                .map(|u| u.responses());
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
        output.consume(&event, image)?;
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
    #[test]
    fn a_completed_turn_cannot_hide_a_failed_image() {
        let mut output = Output::default();
        output.consume(&json!({"method":"item/completed","params":{"item":{"type":"imageGeneration","status":"failed"}}}),true).unwrap();
        assert_eq!(
            output.finish(true).unwrap_err().code,
            "UPSTREAM_REQUEST_FAILED"
        );
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
        let value = output.finish(false).unwrap();
        assert_eq!(value["output"], "real search");
        assert!(value.get("encrypted_output").is_none());
    }
}
