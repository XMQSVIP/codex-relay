use super::Request;
use crate::{
    error::{Error, Result},
    streaming::{StreamSummary, Usage},
};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) struct Output {
    response_model: Option<String>,
    response: Value,
    sequence: u64,
    items: Vec<Value>,
    positions: HashMap<String, usize>,
    finished: std::collections::HashSet<String>,
    call_ids: std::collections::HashSet<String>,
    usage: Option<Usage>,
    calls: bool,
    bytes: usize,
    deltas: u32,
    first: Option<u64>,
    json_object: bool,
    incomplete: bool,
    interrupted: bool,
}

fn part(text: &str) -> Value {
    json!({"type":"output_text","text":text,"annotations":[],"logprobs":[]})
}
fn protocol() -> Error {
    Error::new("UPSTREAM_INVALID_RESPONSE", "原生输出条目无效或不完整。")
}
fn opaque_fields(raw: &Value, item: &mut Value) {
    for key in [
        "encrypted_content",
        "encrypted_function_args",
        "internal_chat_message_metadata_passthrough",
        "agent",
    ] {
        if let Some(value) = raw.get(key) {
            item[key] = value.clone();
        }
    }
}
impl Output {
    pub fn new(r: &Request) -> Self {
        Self {
            response_model: None,
            response: json!({"id":format!("resp_{}",uuid::Uuid::new_v4().simple()),"object":"response","created_at":crate::now_ms()/1000,"completed_at":null,"status":"in_progress","error":null,"incomplete_details":null,"model":r.model,"output":[],"instructions":r.instructions,"tools":r.tools,"tool_choice":r.tool_choice,"parallel_tool_calls":true,"previous_response_id":r.previous,"store":r.store,"reasoning":r.reasoning,"text":r.text,"metadata":r.metadata,"usage":null,"background":false,"max_output_tokens":null,"temperature":null,"top_p":null,"truncation":"disabled"}),
            sequence: 0,
            items: vec![],
            positions: HashMap::new(),
            finished: Default::default(),
            call_ids: Default::default(),
            usage: None,
            calls: false,
            bytes: 0,
            deltas: 0,
            first: None,
            json_object: r.json_object(),
            incomplete: false,
            interrupted: false,
        }
    }
    fn event(&mut self, mut v: Value) -> Value {
        v["sequence_number"] = self.sequence.into();
        self.sequence += 1;
        v
    }
    fn call_id<'a>(&mut self, raw: &'a Value) -> Result<&'a str> {
        let id = raw["call_id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or_else(protocol)?;
        if !self.call_ids.insert(id.to_owned()) {
            return Err(Error::new(
                "UPSTREAM_INVALID_RESPONSE",
                "原生返回了重复的工具调用 call_id。",
            ));
        }
        Ok(id)
    }
    pub fn created(&mut self) -> Vec<Value> {
        vec![
            self.event(json!({"type":"response.created","response":self.response})),
            self.event(json!({"type":"response.in_progress","response":self.response})),
        ]
    }
    fn check_budget(&self) -> Result<()> {
        if self.items.len() > crate::media::HISTORY_COUNT
            || self.bytes > crate::media::BODY_LIMIT
            || serde_json::to_vec(&self.items)?.len() > crate::media::BODY_LIMIT
        {
            return Err(protocol());
        }
        Ok(())
    }
    fn message(&mut self, id: &str, out: &mut Vec<Value>) -> Result<usize> {
        if let Some(i) = self.positions.get(id) {
            return Ok(*i);
        }
        let i = self.items.len();
        if i >= crate::media::HISTORY_COUNT {
            return Err(protocol());
        }
        self.positions.insert(id.into(), i);
        let item = json!({"id":id,"type":"message","role":"assistant","status":"in_progress","content":[]});
        out.push(
            self.event(json!({"type":"response.output_item.added","output_index":i,"item":item})),
        );
        out.push(self.event(json!({"type":"response.content_part.added","item_id":id,"output_index":i,"content_index":0,"part":part("")})));
        let mut item = item;
        item["content"] = json!([part("")]);
        self.items.push(item);
        Ok(i)
    }
    fn append(&mut self, index: usize, delta: &str, out: &mut Vec<Value>) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        self.bytes += delta.len();
        if self.bytes > crate::media::BODY_LIMIT {
            return Err(Error::new(
                "UPSTREAM_INVALID_RESPONSE",
                "响应输出超过 32 MiB。",
            ));
        }
        let text = self.items[index]["content"][0]["text"]
            .as_str()
            .ok_or_else(protocol)?
            .to_owned()
            + delta;
        self.items[index]["content"][0]["text"] = text.into();
        self.deltas += 1;
        self.first.get_or_insert(crate::now_ms());
        out.push(self.event(json!({"type":"response.output_text.delta","item_id":self.items[index]["id"],"output_index":index,"content_index":0,"delta":delta,"logprobs":[]})));
        Ok(())
    }
    fn reasoning(&mut self, id: &str, out: &mut Vec<Value>) -> Result<usize> {
        if let Some(&i) = self.positions.get(id) {
            if self.items[i]["type"] != "reasoning" {
                return Err(protocol());
            }
            return Ok(i);
        }
        let i = self.items.len();
        self.positions.insert(id.into(), i);
        let item = json!({"type":"reasoning","id":id,"summary":[]});
        out.push(
            self.event(json!({"type":"response.output_item.added","output_index":i,"item":item})),
        );
        self.items.push(item);
        self.check_budget()?;
        Ok(i)
    }
    fn reasoning_delta(
        &mut self,
        i: usize,
        n: usize,
        delta: &str,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        let id = self.items[i]["id"].clone();
        let summary = self.items[i]["summary"]
            .as_array_mut()
            .ok_or_else(protocol)?;
        if n > summary.len() || n >= crate::media::HISTORY_COUNT {
            return Err(protocol());
        }
        if n == summary.len() {
            summary.push(json!({"type":"summary_text","text":""}));
            out.push(self.event(json!({"type":"response.reasoning_summary_part.added","item_id":id,"output_index":i,"summary_index":n,"part":{"type":"summary_text","text":""}})));
        }
        if !delta.is_empty() {
            let prior = self.items[i]["summary"][n]["text"]
                .as_str()
                .ok_or_else(protocol)?;
            self.items[i]["summary"][n]["text"] = json!(format!("{prior}{delta}"));
            self.bytes += delta.len();
            self.deltas += 1;
            self.first.get_or_insert(crate::now_ms());
            out.push(self.event(json!({"type":"response.reasoning_summary_text.delta","item_id":id,"output_index":i,"summary_index":n,"delta":delta})));
        }
        self.check_budget()
    }
    pub fn consume(&mut self, e: &Value, r: &Request) -> Result<Vec<Value>> {
        let p = &e["params"];
        let mut out = vec![];
        match e["method"].as_str().unwrap_or("") {
            "item/reasoning/summaryTextDelta" => {
                let id = p["itemId"].as_str().ok_or_else(protocol)?;
                if self.finished.contains(id) {
                    return Err(protocol());
                }
                let i = self.reasoning(id, &mut out)?;
                let n = p["summaryIndex"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(protocol)?;
                self.reasoning_delta(i, n, p["delta"].as_str().ok_or_else(protocol)?, &mut out)?;
            }
            "item/agentMessage/delta" => {
                // JSON mode validates a complete native message before releasing
                // any of its text. Incomplete/unparseable output is never a
                // successful JSON response, in either HTTP or SSE mode.
                if self.json_object {
                    return Ok(out);
                }
                let id = p["itemId"].as_str().ok_or_else(protocol)?;
                if self.finished.contains(id) {
                    return Err(protocol());
                }
                let i = self.message(id, &mut out)?;
                self.append(i, p["delta"].as_str().ok_or_else(protocol)?, &mut out)?;
            }
            "rawResponseItem/completed" => {
                let raw = &p["item"];
                let kind = raw["type"].as_str().ok_or_else(protocol)?;
                self.incomplete |= raw["status"] == "incomplete";
                let id = raw["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("item_{}", uuid::Uuid::new_v4().simple()));
                if id.is_empty() || id.len() > 256 {
                    return Err(protocol());
                }
                if !self.finished.insert(id.clone()) {
                    return Err(protocol());
                }
                match kind {
                    "additional_tools" | "configuration_update" => return Ok(out),
                    "compaction" | "context_compaction" | "agent_message" | "tool_search_call"
                    | "tool_search_output" => {
                        if kind.starts_with("tool_search")
                            && (r.tool_choice == "none"
                                || !r.tools.iter().any(|t| t["type"] == "tool_search"))
                        {
                            return Err(super::tool_catalog::scope_error(
                                "原生返回了未注册的工具搜索调用。",
                                None,
                                kind,
                            ));
                        }
                        if kind == "tool_search_call" && raw["execution"] == "client" {
                            self.call_id(raw)?;
                        }
                        let i = self.items.len();
                        let mut item = raw.clone();
                        item["id"] = json!(id);
                        self.bytes += serde_json::to_vec(&item)?.len();
                        self.calls |= kind == "tool_search_call" && raw["execution"] == "client";
                        out.push(self.event(json!({"type":"response.output_item.added","output_index":i,"item":item})));
                        out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":item})));
                        self.items.push(item);
                    }
                    "message" => {
                        if raw["role"] != "assistant" {
                            return Ok(out);
                        }
                        if self.json_object && raw["phase"] == "commentary" {
                            return Ok(out);
                        }
                        let content = raw["content"].as_array().ok_or_else(protocol)?;
                        if !content
                            .iter()
                            .all(|v| v["type"] == "output_text" && v["text"].is_string())
                        {
                            return Err(protocol());
                        }
                        let text = content
                            .iter()
                            .map(|v| v["text"].as_str().unwrap())
                            .collect::<String>();
                        if self.json_object
                            && !serde_json::from_str::<Value>(&text).is_ok_and(|v| v.is_object())
                        {
                            return Err(Error::new(
                                "UPSTREAM_INVALID_RESPONSE",
                                "原生模型未返回有效 JSON 对象；json_object 校验失败。",
                            ));
                        }
                        let i = self.message(&id, &mut out)?;
                        let prior = self.items[i]["content"][0]["text"]
                            .as_str()
                            .ok_or_else(protocol)?;
                        let suffix = text.strip_prefix(prior).ok_or_else(protocol)?.to_owned();
                        self.append(i, &suffix, &mut out)?;
                        self.items[i]["status"] = json!(if raw["status"] == "incomplete" {
                            "incomplete"
                        } else {
                            "completed"
                        });
                        opaque_fields(raw, &mut self.items[i]);
                        if !raw["phase"].is_null() {
                            self.items[i]["phase"] = raw["phase"].clone();
                        }
                        let annotations = content
                            .iter()
                            .filter_map(|v| v["annotations"].as_array())
                            .flatten()
                            .cloned()
                            .collect::<Vec<_>>();
                        self.items[i]["content"][0]["annotations"] = annotations.into();
                        let part = self.items[i]["content"][0].clone();
                        out.push(self.event(json!({"type":"response.output_text.done","item_id":id,"output_index":i,"content_index":0,"text":text,"logprobs":[]})));
                        out.push(self.event(json!({"type":"response.content_part.done","item_id":id,"output_index":i,"content_index":0,"part":part})));
                        out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":self.items[i]})));
                    }
                    "function_call" | "custom_tool_call" => {
                        if r.tool_choice == "none" {
                            return Err(super::tool_catalog::scope_error(
                                "模型调用了本次请求已禁用的客户端工具。",
                                raw["namespace"].as_str(),
                                raw["name"].as_str().unwrap_or(""),
                            ));
                        }
                        let native_name = raw["name"].as_str().ok_or_else(protocol)?;
                        let native_custom = kind == "custom_tool_call";
                        let original_namespace = raw["namespace"].as_str();
                        let (namespace, name) = super::tool_catalog::output_identity(
                            &r.tools,
                            original_namespace,
                            native_name,
                        )
                        .ok_or_else(|| {
                            super::tool_catalog::scope_error(
                                "模型调用了未注册的客户端函数。",
                                original_namespace,
                                native_name,
                            )
                        })?;
                        let namespace = namespace.as_deref();
                        let name = name.as_str();
                        let custom = r.is_custom_tool(namespace, name);
                        if native_custom && !custom {
                            return Err(super::tool_catalog::scope_error(
                                "模型返回的 custom 调用不属于已注册的客户端 custom 工具。",
                                original_namespace,
                                native_name,
                            ));
                        }
                        let args = raw[if native_custom { "input" } else { "arguments" }]
                            .as_str()
                            .ok_or_else(protocol)?;
                        let call = self.call_id(raw)?;
                        if namespace.is_none()
                            && name == super::tool_catalog::SEARCH_NAME
                            && r.tools.iter().any(|t| t["type"] == "tool_search")
                        {
                            let arguments: Value =
                                serde_json::from_str(args).map_err(|_| protocol())?;
                            if !arguments.is_object() {
                                return Err(protocol());
                            }
                            let i = self.items.len();
                            let item = json!({"type":"tool_search_call","id":id,"call_id":call,"execution":"client","status":"completed","arguments":arguments});
                            self.bytes += args.len();
                            self.calls = true;
                            out.push(self.event(json!({"type":"response.output_item.added","output_index":i,"item":item})));
                            out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":item})));
                            self.items.push(item);
                            self.check_budget()?;
                            self.first.get_or_insert(crate::now_ms());
                            return Ok(out);
                        }
                        self.bytes += args.len();
                        self.calls = true;
                        let i = self.items.len();
                        // The native protocol can also emit CustomToolCall directly,
                        // even though dynamic registration uses a function envelope.
                        // Freeform input must not be parsed as JSON or executed here.
                        let payload = if native_custom {
                            args.to_owned()
                        } else if custom {
                            let value: Value =
                                serde_json::from_str(args).map_err(|_| protocol())?;
                            value["input"].as_str().ok_or_else(protocol)?.to_owned()
                        } else {
                            args.to_owned()
                        };
                        let (kind, field, delta_event, done_event) = if custom {
                            (
                                "custom_tool_call",
                                "input",
                                "response.custom_tool_call_input.delta",
                                "response.custom_tool_call_input.done",
                            )
                        } else {
                            (
                                "function_call",
                                "arguments",
                                "response.function_call_arguments.delta",
                                "response.function_call_arguments.done",
                            )
                        };
                        let mut item = json!({"type":kind,"id":id,"call_id":call,"name":name,"status":"in_progress"});
                        item[field] = json!("");
                        if let Some(namespace) = namespace {
                            item["namespace"] = namespace.into();
                        }
                        out.push(self.event(json!({"type":"response.output_item.added","output_index":i,"item":item})));
                        out.push(self.event(json!({"type":delta_event,"item_id":id,"output_index":i,"delta":payload})));
                        let mut done =
                            json!({"type":done_event,"item_id":id,"output_index":i,"name":name});
                        done[field] = json!(payload);
                        out.push(self.event(done));
                        item[field] = payload.into();
                        item["status"] = json!(if raw["status"] == "incomplete" {
                            "incomplete"
                        } else {
                            "completed"
                        });
                        opaque_fields(raw, &mut item);
                        out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":item})));
                        self.items.push(item);
                    }
                    "web_search_call" | "image_generation_call" => {
                        let tool = if kind == "web_search_call" {
                            "web_search"
                        } else {
                            "image_generation"
                        };
                        if r.tool_choice == "none" || !r.tools.iter().any(|t| t["type"] == tool) {
                            return Err(super::tool_catalog::scope_error(
                                "原生执行了未请求的服务端工具。",
                                None,
                                tool,
                            ));
                        }
                        if raw["status"] != "completed" {
                            return Err(Error::new(
                                "UPSTREAM_TOOL_FAILED",
                                "原生服务端工具未完成。",
                            ));
                        }
                        let i = self.items.len();
                        let mut item = raw.clone();
                        item["id"] = id.clone().into();
                        if tool == "image_generation" {
                            let result = raw["result"].as_str().ok_or_else(protocol)?;
                            crate::media::generated_image(result)?;
                        }
                        self.bytes += serde_json::to_vec(&item)?.len();
                        let mut initial = item.clone();
                        initial["status"] = json!("in_progress");
                        if tool == "image_generation" {
                            initial["result"] = Value::Null;
                        }
                        out.push(self.event(json!({"type":"response.output_item.added","output_index":i,"item":initial})));
                        out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":item})));
                        self.items.push(item);
                    }
                    "reasoning" => {
                        let i = self.reasoning(&id, &mut out)?;
                        let mut item = json!({"type":"reasoning","id":id,"summary":raw["summary"]});
                        if !item["summary"].is_array() {
                            return Err(protocol());
                        }
                        if !raw["content"].is_null() {
                            super::request::validate_reasoning_content(&raw["content"])
                                .map_err(|_| protocol())?;
                            item["content"] = raw["content"].clone();
                            self.bytes += serde_json::to_vec(&raw["content"])?.len();
                        }
                        if self.items[i]["summary"].as_array().unwrap().len()
                            > raw["summary"].as_array().unwrap().len()
                        {
                            return Err(protocol());
                        }
                        for (n, s) in raw["summary"].as_array().unwrap().iter().enumerate() {
                            let text = s["text"].as_str().ok_or_else(protocol)?;
                            let prior = self.items[i]["summary"][n]["text"].as_str().unwrap_or("");
                            let suffix = text.strip_prefix(prior).ok_or_else(protocol)?.to_owned();
                            self.reasoning_delta(i, n, &suffix, &mut out)?;
                            out.push(self.event(json!({"type":"response.reasoning_summary_text.done","item_id":id,"output_index":i,"summary_index":n,"text":text})));
                            out.push(self.event(json!({"type":"response.reasoning_summary_part.done","item_id":id,"output_index":i,"summary_index":n,"part":s})));
                        }
                        opaque_fields(raw, &mut item);
                        if !raw["encrypted_content"].is_null() {
                            self.bytes += raw["encrypted_content"]
                                .as_str()
                                .ok_or_else(protocol)?
                                .len();
                        }
                        out.push(self.event(json!({"type":"response.output_item.done","output_index":i,"item":item})));
                        self.items[i] = item;
                    }
                    _ => {
                        return Err(Error::new(
                            "NATIVE_OUTPUT_UNSUPPORTED",
                            &format!("原生后端生成了当前 Responses 接口不支持的条目类型：{kind}。"),
                        ));
                    }
                }
                self.check_budget()?;
                // Native tool calls can be the only output of a successful turn.
                // Count their validated output even when no text delta was emitted.
                if matches!(
                    kind,
                    "function_call"
                        | "custom_tool_call"
                        | "tool_search_call"
                        | "tool_search_output"
                        | "web_search_call"
                        | "image_generation_call"
                ) {
                    self.first.get_or_insert(crate::now_ms());
                }
            }
            "model/rerouted" => {
                if let Some(model) = crate::streaming::reported_model(&p["toModel"]) {
                    self.response_model = Some(model);
                }
            }
            "rawResponse/completed" if !p["usage"].is_null() => {
                let u = Usage::parse(&p["usage"])?;
                if let Some(total) = &mut self.usage {
                    total.input_tokens += u.input_tokens;
                    total.output_tokens += u.output_tokens;
                    total.total_tokens += u.total_tokens;
                    total.cached_input_tokens = total
                        .cached_input_tokens
                        .zip(u.cached_input_tokens)
                        .map(|(a, b)| a + b);
                    total.reasoning_output_tokens = total
                        .reasoning_output_tokens
                        .zip(u.reasoning_output_tokens)
                        .map(|(a, b)| a + b);
                } else {
                    self.usage = Some(u);
                }
            }
            _ => {}
        }
        Ok(out)
    }
    pub fn has_calls(&self) -> bool {
        self.calls
    }
    pub fn items(&self) -> &[Value] {
        &self.items
    }
    pub fn interrupted(&mut self) {
        self.incomplete = true;
        self.interrupted = true;
        self.items.retain(|item| {
            item["id"]
                .as_str()
                .is_some_and(|id| self.finished.contains(id))
                && item["status"] != "incomplete"
                && item["status"] != "in_progress"
        });
        self.response["incomplete_details"] = json!({"reason":"interrupted"});
    }
    pub fn failed(&mut self, error: &Error) -> Value {
        if self.check_budget().is_err() {
            self.items.clear();
        }
        self.response["output"] = json!(self.items);
        self.response["status"] = json!("failed");
        self.response["error"] = json!({"code":error.code,"message":error.message});
        if let Some(usage) = &self.usage {
            self.response["usage"] = usage.responses();
        }
        self.event(json!({"type":"response.failed","response":self.response}))
    }
    pub fn completed(&mut self) -> Result<Value> {
        if (!self.interrupted && self.items.is_empty())
            || self.items.iter().any(|v| v["status"] == "in_progress")
        {
            return Err(protocol());
        }
        if self.json_object && !self.interrupted {
            let messages = self.items.iter().filter(|v| v["type"] == "message").count();
            if messages > 1 || (!self.calls && messages != 1) {
                return Err(Error::new(
                    "UPSTREAM_INVALID_RESPONSE",
                    "JSON 模式必须返回一个完整对象，或返回待调用的函数。",
                ));
            }
        }
        self.response["output"] = self.items.clone().into();
        self.response["status"] = json!(if self.incomplete {
            "incomplete"
        } else {
            "completed"
        });
        self.response["completed_at"] = json!(crate::now_ms() / 1000);
        if let Some(u) = &self.usage {
            self.response["usage"] = u.responses();
        }
        Ok(self.event(json!({"type":if self.incomplete {"response.incomplete"}else{"response.completed"},"response":self.response})))
    }
    pub fn summary(&self, thread: &str, turn: &str) -> Result<StreamSummary> {
        Ok(StreamSummary {
            response_model: self.response_model.clone(),
            response_service_tier: None,
            thread_id: thread.into(),
            turn_id: turn.into(),
            item_ids: self
                .items
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_owned))
                .collect(),
            outcome: if self.incomplete {
                "incomplete"
            } else {
                "completed"
            }
            .into(),
            native_status: Some(
                if self.interrupted {
                    "interrupted"
                } else if self.incomplete {
                    "incomplete"
                } else {
                    "completed"
                }
                .into(),
            ),
            error_code: self.incomplete.then(|| {
                if self.interrupted {
                    "REQUEST_INTERRUPTED"
                } else {
                    "UPSTREAM_INCOMPLETE"
                }
                .into()
            }),
            error_message: None,
            delta_count: self.deltas,
            text_bytes: self.bytes,
            first_delta_at: self.first,
            usage: self.usage.clone(),
            interrupt_acknowledged: self.interrupted,
            cleanup_confirmed: true,
            guard: crate::guard::Stats {
                state: "not-used-native-direct".into(),
                ..Default::default()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_only_responses_record_first_output_without_text() {
        let request = Request::parse(json!({"model":"m","input":"test","tools":[
            {"type":"function","name":"echo"},
            {"type":"custom","name":"exec"},
            {"type":"tool_search","execution":"client"},
            {"type":"web_search"}
        ]}))
        .unwrap();
        for item in [
            json!({"type":"function_call","id":"fc","call_id":"call","name":"echo","arguments":"{}"}),
            json!({"type":"custom_tool_call","id":"custom","call_id":"call","name":"exec","input":""}),
            json!({"type":"function_call","id":"adapted","call_id":"call","name":"exec","arguments":"{\"input\":\"run\"}"}),
            json!({"type":"function_call","id":"search","call_id":"call","name":super::super::tool_catalog::SEARCH_NAME,"arguments":"{}"}),
            json!({"type":"tool_search_call","id":"search","call_id":"call","execution":"client","arguments":{}}),
            json!({"type":"tool_search_output","id":"search_output","tools":[]}),
            json!({"type":"web_search_call","id":"web","status":"completed"}),
        ] {
            let mut output = Output::new(&request);
            output.created();
            assert!(
                output
                    .summary("t", "turn")
                    .unwrap()
                    .first_delta_at
                    .is_none()
            );
            let before = crate::now_ms();
            let event = json!({"method":"rawResponseItem/completed","params":{"item":item}});
            let events = output.consume(&event, &request).unwrap();
            assert!(!events.is_empty());
            let summary = output.summary("t", "turn").unwrap();
            let first = summary
                .first_delta_at
                .expect("tool output must have a timestamp");
            assert!((before..=crate::now_ms()).contains(&first));
            assert_eq!(summary.delta_count, 0);
            assert_eq!(output.completed().unwrap()["type"], "response.completed");

            // A later tool must not replace an earlier text or tool timestamp.
            let mut output = Output::new(&request);
            output.first = Some(1);
            output.consume(&event, &request).unwrap();
            assert_eq!(output.summary("t", "turn").unwrap().first_delta_at, Some(1));
        }
    }

    #[test]
    fn metadata_and_rejected_tools_do_not_record_first_output() {
        let request = Request::parse(json!({"model":"m","input":"test"})).unwrap();
        for item in [
            json!({"type":"additional_tools","id":"config","tools":[]}),
            json!({"type":"reasoning","id":"reason","summary":[],"encrypted_content":"opaque"}),
        ] {
            let mut output = Output::new(&request);
            output
                .consume(
                    &json!({"method":"rawResponseItem/completed","params":{"item":item}}),
                    &request,
                )
                .unwrap();
            assert!(
                output
                    .summary("t", "turn")
                    .unwrap()
                    .first_delta_at
                    .is_none()
            );
        }
        let mut output = Output::new(&request);
        assert!(output.consume(&json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"function_call","id":"fc","call_id":"call","name":"unregistered","arguments":"{}"
        }}}), &request).is_err());
        assert!(
            output
                .summary("t", "turn")
                .unwrap()
                .first_delta_at
                .is_none()
        );
    }

    #[test]
    fn duplicate_or_invalid_call_ids_fail_before_forwarding_the_call() {
        let r = Request::parse(json!({"model":"m","input":"test","tools":[{"type":"function","name":"echo"},{"type":"tool_search","execution":"client"}]})).unwrap();
        let call = json!({"method":"rawResponseItem/completed","params":{"item":{"type":"function_call","id":"one","name":"echo","call_id":"same","arguments":"{}"}}});
        let mut output = Output::new(&r);
        output.consume(&call, &r).unwrap();
        let mut second = call.clone();
        second["params"]["item"]["id"] = json!("two");
        assert_eq!(
            output.consume(&second, &r).unwrap_err().code,
            "UPSTREAM_INVALID_RESPONSE"
        );
        assert_eq!(output.items.len(), 1);
        for id in ["".to_owned(), "x".repeat(257)] {
            second["params"]["item"]["call_id"] = json!(id);
            assert!(Output::new(&r).consume(&second, &r).is_err());
        }
        let search = json!({"method":"rawResponseItem/completed","params":{"item":{"type":"tool_search_call","id":"search","execution":"client","call_id":"same","arguments":{}}}});
        assert!(output.consume(&search, &r).is_err());
        let mut disabled = r;
        disabled.tool_choice = json!("none");
        assert_eq!(
            Output::new(&disabled)
                .consume(&search, &disabled)
                .unwrap_err()
                .code,
            "NATIVE_TOOL_SCOPE"
        );
    }
    #[test]
    fn interrupted_response_drops_partial_items_and_is_not_success() {
        let r = Request::parse(json!({"model":"m","input":"hi"})).unwrap();
        let mut output = Output::new(&r);
        output.consume(&json!({"method":"item/agentMessage/delta","params":{"itemId":"partial","delta":"unfinished"}}),&r).unwrap();
        output.interrupted();
        let event = output.completed().unwrap();
        assert_eq!(event["type"], "response.incomplete");
        assert_eq!(
            event["response"]["incomplete_details"]["reason"],
            "interrupted"
        );
        assert_eq!(event["response"]["output"], json!([]));
        assert!(event["response"]["usage"].is_null());
    }
    #[test]
    fn reasoning_deltas_are_not_repeated_and_native_fields_survive() {
        let r = Request::parse(json!({"model":"m","input":"hi"})).unwrap();
        let mut output = Output::new(&r);
        let mut events=output.consume(&json!({"method":"item/reasoning/summaryTextDelta","params":{"itemId":"reason","summaryIndex":0,"delta":"First "}}),&r).unwrap();
        let raw = json!({"type":"reasoning","id":"reason","summary":[{"type":"summary_text","text":"First second"}],"content":[{"type":"reasoning_text","text":"opaque reasoning"}],"internal_chat_message_metadata_passthrough":{"turn_id":"original"}});
        events.extend(
            output
                .consume(
                    &json!({"method":"rawResponseItem/completed","params":{"item":raw}}),
                    &r,
                )
                .unwrap(),
        );
        let deltas = events
            .iter()
            .filter(|e| e["type"] == "response.reasoning_summary_text.delta")
            .map(|e| e["delta"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(deltas, "First second");
        let response = output.completed().unwrap();
        assert_eq!(response["response"]["output"][0], raw);
    }
    #[test]
    fn opaque_output_obeys_the_same_budget_as_visible_text() {
        let request = Request::parse(json!({"model":"m","input":"hello"})).unwrap();
        let mut output = Output::new(&request);
        let error=output.consume(&json!({"method":"rawResponseItem/completed","params":{"item":{"type":"message","id":"large","role":"assistant","encrypted_content":"x".repeat(crate::media::BODY_LIMIT),"content":[{"type":"output_text","text":"small"}]}}}),&request).unwrap_err();
        assert_eq!(output.failed(&error)["response"]["output"], json!([]));
    }
    #[test]
    fn incomplete_items_and_opaque_metadata_are_never_reported_as_success() {
        let request = Request::parse(json!({"model":"m","input":"hello"})).unwrap();
        let mut output = Output::new(&request);
        output.consume(&json!({"method":"rawResponseItem/completed","params":{"item":{"type":"message","id":"partial","role":"assistant","status":"incomplete","encrypted_content":"opaque","content":[{"type":"output_text","text":"partial"}]}}}),&request).unwrap();
        let event = output.completed().unwrap();
        assert_eq!(event["type"], "response.incomplete");
        assert_eq!(event["response"]["status"], "incomplete");
        assert_eq!(
            event["response"]["output"][0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(output.summary("t", "turn").unwrap().outcome, "incomplete");
    }
    #[test]
    fn response_model_requires_an_explicit_native_report() {
        let request = json_request();
        let mut output = Output::new(&request);
        assert!(
            output
                .summary("thread", "turn")
                .unwrap()
                .response_model
                .is_none()
        );
        output
            .consume(
                &json!({"method":"model/rerouted","params":{"toModel":"gpt-6.1-sol"}}),
                &request,
            )
            .unwrap();
        assert_eq!(
            output
                .summary("thread", "turn")
                .unwrap()
                .response_model
                .as_deref(),
            Some("gpt-6.1-sol")
        );
        for invalid in [
            json!(null),
            json!(""),
            json!("x".repeat(257)),
            json!("not a model\n"),
        ] {
            output
                .consume(
                    &json!({"method":"model/rerouted","params":{"toModel":invalid}}),
                    &request,
                )
                .unwrap();
            assert_eq!(
                output
                    .summary("thread", "turn")
                    .unwrap()
                    .response_model
                    .as_deref(),
                Some("gpt-6.1-sol")
            );
        }
    }

    fn json_request() -> Request {
        Request::parse(
            json!({"model":"m","input":"return JSON","text":{"format":{"type":"json_object"}}}),
        )
        .unwrap()
    }
    fn message(text: &str, phase: &str) -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"message","id":format!("m_{phase}"),"role":"assistant","phase":phase,
            "content":[{"type":"output_text","text":text}]
        }}})
    }
    #[test]
    fn json_mode_buffers_partial_text_and_validates_before_emission() {
        let request = json_request();
        let mut output = Output::new(&request);
        let mut events = output.created();
        assert!(output.consume(&json!({"method":"item/agentMessage/delta","params":{"itemId":"m_final_answer","delta":"{\"ok\":"}}), &request).unwrap().is_empty());
        assert!(
            output
                .consume(&message("Working on it", "commentary"), &request)
                .unwrap()
                .is_empty()
        );
        events.extend(
            output
                .consume(&message("{\"ok\":true}", "final_answer"), &request)
                .unwrap(),
        );
        let done = output.completed().unwrap();
        assert_eq!(done["response"]["output"].as_array().unwrap().len(), 1);
        assert_eq!(
            done["response"]["output"][0]["content"][0]["text"],
            "{\"ok\":true}"
        );
        events.push(done);
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], i);
        }
        let deltas: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "response.output_text.delta")
            .collect();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0]["delta"], "{\"ok\":true}");
    }
    #[test]
    fn json_mode_rejects_invalid_or_non_object_output_without_text_events() {
        for text in [
            "plain text",
            "[1,2]",
            "null",
            "true",
            "```json\n{}\n```",
            "{\"ok\":",
            "{} {}",
        ] {
            let request = json_request();
            let mut output = Output::new(&request);
            let error = output
                .consume(&message(text, "final_answer"), &request)
                .unwrap_err();
            assert_eq!(error.code, "UPSTREAM_INVALID_RESPONSE");
            assert!(output.items.is_empty());
            assert!(output.completed().is_err());
        }
    }
    #[test]
    fn json_mode_keeps_real_tool_calls() {
        let request = Request::parse(json!({"model":"m","input":"return JSON",
            "text":{"format":{"type":"json_object"}},"tools":[{"type":"function","name":"echo"}]
        }))
        .unwrap();
        let mut output = Output::new(&request);
        output
            .consume(
                &json!({"method":"rawResponseItem/completed","params":{"item":{
                    "type":"function_call","id":"fc","call_id":"call","name":"echo","arguments":"{}"
                }}}),
                &request,
            )
            .unwrap();
        assert_eq!(
            output.completed().unwrap()["response"]["output"][0]["type"],
            "function_call"
        );
    }
    #[test]
    fn json_mode_does_not_complete_reasoning_only_or_multiple_objects() {
        let request = json_request();
        let mut output = Output::new(&request);
        output
            .consume(
                &json!({"method":"rawResponseItem/completed","params":{"item":{
                    "type":"reasoning","id":"r","summary":[]
                }}}),
                &request,
            )
            .unwrap();
        assert!(output.completed().is_err());
        output
            .consume(&message("{}", "final_answer"), &request)
            .unwrap();
        let mut second = message("{}", "final_answer");
        second["params"]["item"]["id"] = json!("second");
        output.consume(&second, &request).unwrap();
        assert!(output.completed().is_err());
    }
    #[test]
    fn parallel_calls_and_reasoning_keep_native_data_and_order() {
        let request=Request::parse(json!({"model":"m","input":"test","tools":[{"type":"function","name":"echo","parameters":{"type":"object"}}]})).unwrap();
        let mut output = Output::new(&request);
        let mut events = output.created();
        for item in [
            json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"summary"}],"content":[{"type":"text","text":"provider reasoning"}],"encrypted_content":"opaque-native","internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}),
            json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"echo","arguments":"{\"value\":1}"}),
            json!({"type":"function_call","id":"fc_2","call_id":"call_2","name":"echo","arguments":"{\"value\":2}"}),
        ] {
            events.extend(
                output
                    .consume(
                        &json!({"method":"rawResponseItem/completed","params":{"item":item}}),
                        &request,
                    )
                    .unwrap(),
            );
        }
        assert!(output.has_calls());
        let done = output.completed().unwrap();
        assert_eq!(
            done["response"]["output"][0]["encrypted_content"],
            "opaque-native"
        );
        assert_eq!(
            done["response"]["output"][0]["content"][0]["text"],
            "provider reasoning"
        );
        assert_eq!(
            done["response"]["output"][0]["internal_chat_message_metadata_passthrough"]["turn_id"],
            "turn-1"
        );
        assert_eq!(done["response"]["output"][2]["call_id"], "call_2");
        assert_eq!(done["response"]["usage"], Value::Null);
        events.push(done);
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e["sequence_number"], i);
        }
        let indices = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.done")
            .map(|e| e["output_index"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(indices, vec![0, 1, 2]);
    }
    #[test]
    fn mismatched_stream_text_and_unregistered_tools_fail() {
        let request = Request::parse(json!({"model":"m","input":"test"})).unwrap();
        let mut output = Output::new(&request);
        output.consume(&json!({"method":"item/agentMessage/delta","params":{"itemId":"msg_1","delta":"prefix"}}),&request).unwrap();
        assert!(output.completed().is_err());
        assert!(output.consume(&json!({"method":"rawResponseItem/completed","params":{"item":{"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"different"}]}}}),&request).is_err());
        assert!(output.consume(&json!({"method":"rawResponseItem/completed","params":{"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"unregistered","arguments":"{}"}}}),&request).is_err());
    }

    #[test]
    fn native_functions_namespace_maps_only_registered_bare_tools() {
        let request = Request::parse(
            json!({"model":"m","input":"test","tools":[{"type":"function","name":"echo"}]}),
        )
        .unwrap();
        for (namespace, name, accepted) in [
            ("functions", "echo", true),
            ("other", "echo", false),
            ("functions", "exec", false),
        ] {
            let mut output = Output::new(&request);
            let event = json!({"method":"rawResponseItem/completed","params":{"item":{"type":"function_call","id":"fc","call_id":"call","namespace":namespace,"name":name,"arguments":"{}"}}});
            assert_eq!(output.consume(&event, &request).is_ok(), accepted);
            if accepted {
                let result = output.completed().unwrap();
                assert_eq!(result["response"]["output"][0]["name"], "echo");
                assert!(result["response"]["output"][0].get("namespace").is_none());
            }
        }
    }

    #[test]
    fn omitted_functions_namespace_restores_desktop_exec_and_history() {
        let request = Request::parse(json!({"model":"m","input":"获取电脑配置","tools":[
            {"type":"namespace","name":"functions","description":"Desktop tools","tools":[
                {"type":"custom","name":"exec","format":{"type":"text"}},
                {"type":"function","name":"wait"}
            ]}
        ]}))
        .unwrap();
        let input = "text(await tools.exec_command({cmd: \"Get-ComputerInfo\"}));\n";
        for mut raw in [
            json!({"type":"custom_tool_call","id":"exec_item","call_id":"exec_call","name":"exec","input":input}),
            json!({"type":"function_call","id":"exec_item","call_id":"exec_call","name":"exec","arguments":json!({"input":input}).to_string()}),
            json!({"type":"function_call","id":"wait_item","call_id":"wait_call","name":"wait","arguments":"{\"cell_id\":\"cell\"}"}),
        ] {
            for null_namespace in [false, true] {
                if null_namespace {
                    raw["namespace"] = Value::Null;
                }
                let event = json!({"method":"rawResponseItem/completed","params":{"item":raw}});
                let mut output = Output::new(&request);
                let events = output.consume(&event, &request).unwrap();
                let response = output.completed().unwrap();
                let item = &response["response"]["output"][0];
                assert_eq!(item["namespace"], "functions");
                assert_eq!(item["name"], raw["name"]);
                assert_eq!(item["call_id"], raw["call_id"]);
                assert!(
                    events
                        .iter()
                        .any(|event| event["type"] == "response.output_item.done"
                            && event["item"] == *item)
                );
                let custom = raw["name"] == "exec";
                if custom {
                    assert_eq!(item["type"], "custom_tool_call");
                    assert_eq!(item["input"], input);
                    assert!(events.iter().any(|event| event["type"]
                        == "response.custom_tool_call_input.delta"
                        && event["delta"] == input));
                } else {
                    assert_eq!(item["arguments"], raw["arguments"]);
                }
                let continued = Request::parse(json!({"model":"m","tools":request.tools,"input":[
                    item,
                    {"type":if custom {"custom_tool_call_output"} else {"function_call_output"},"call_id":item["call_id"],"output":"CLIENT_RESULT"}
                ]})).unwrap();
                let native = super::super::tool_catalog::native_history_with_catalog(
                    &continued.input,
                    false,
                    &continued.tools,
                );
                let declarations = super::super::tool_catalog::native(&continued.tools);
                assert_eq!(native[0]["namespace"], declarations[0]["name"]);
                assert_eq!(native[1]["output"], "CLIENT_RESULT");
                assert!(native[1].get("name").is_none());
            }
        }
    }

    #[test]
    fn custom_tool_calls_restore_freeform_json_and_sse_payloads() {
        let request = Request::parse(json!({"model":"m","input":[
            {"type":"additional_tools","role":"developer","tools":[{"type":"custom","name":"exec"}]},
            {"role":"user","content":"run"}
        ]})).unwrap();
        let input = "const s = \"中文\";\ntext(s);";
        let event = json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"function_call","id":"fc","call_id":"call","namespace":"functions","name":"exec",
            "arguments":json!({"input":input}).to_string()
        }}});
        let mut output = Output::new(&request);
        let events = output.consume(&event, &request).unwrap();
        assert!(
            events.iter().any(
                |e| e["type"] == "response.custom_tool_call_input.delta" && e["delta"] == input
            )
        );
        assert!(events
            .iter()
            .any(|e| e["type"] == "response.custom_tool_call_input.done" && e["input"] == input));
        let completed = output.completed().unwrap();
        let item = &completed["response"]["output"][0];
        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["call_id"], "call");
        assert_eq!(item["input"], input);
        assert!(item.get("arguments").is_none());
        assert!(item.get("namespace").is_none());
        let mut malformed = event;
        malformed["params"]["item"]["arguments"] = json!("{}");
        assert!(Output::new(&request).consume(&malformed, &request).is_err());
    }

    #[test]
    fn native_custom_calls_yield_exact_client_image_code_and_preserve_identity() {
        let input = "const result = await tools.image_gen__imagegen({prompt: \"画蓝鲸\"});\ngeneratedImage(result);\n";
        for namespace in [
            None,
            Some("functions"),
            Some("image_gen"),
            Some("mcp__images"),
        ] {
            let mut tool = json!({"type":"custom","name":"exec","format":{"type":"text"}});
            if let Some(namespace) = namespace {
                tool = json!({"type":"namespace","name":namespace,"description":"Client image tools","tools":[tool]});
            }
            let request = Request::parse(json!({"model":"m","input":[
                {"type":"additional_tools","role":"developer","tools":[tool]},
                {"role":"user","content":"Draw an image on the client"}
            ]}))
            .unwrap();
            let declarations = super::super::tool_catalog::native(&request.tools);
            let native_namespace = if namespace.is_some() {
                declarations[0]["name"].as_str()
            } else {
                Some("functions")
            };
            let raw = json!({"type":"custom_tool_call","id":"custom_image","call_id":"image_call","name":"exec","namespace":native_namespace,"input":input,"encrypted_content":"opaque","internal_chat_message_metadata_passthrough":{"key":"value"}});
            let event = json!({"method":"rawResponseItem/completed","params":{"item":raw}});
            let mut output = Output::new(&request);
            let events = output.consume(&event, &request).unwrap();
            assert!(
                output.has_calls(),
                "must yield control at the model response boundary"
            );
            assert_eq!(
                events
                    .iter()
                    .map(|e| e["type"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec![
                    "response.output_item.added",
                    "response.custom_tool_call_input.delta",
                    "response.custom_tool_call_input.done",
                    "response.output_item.done"
                ]
            );
            assert_eq!(events[1]["delta"], input);
            assert_eq!(events[2]["input"], input);
            let response = output.completed().unwrap();
            let item = &response["response"]["output"][0];
            assert_eq!(item["type"], "custom_tool_call");
            assert_eq!(item["id"], "custom_image");
            assert_eq!(item["call_id"], "image_call");
            assert_eq!(item["namespace"].as_str(), namespace);
            assert_eq!(item["input"], input);
            assert_eq!(item["encrypted_content"], "opaque");
            let mut original = event.clone();
            original["params"]["item"]["namespace"] = json!(namespace);
            let mut direct = Output::new(&request);
            direct.consume(&original, &request).unwrap();
            assert_eq!(direct.completed().unwrap()["response"]["output"][0], *item);
            let mut history = request.input.clone();
            history.push(item.clone());
            history.push(json!({"type":"custom_tool_call_output","call_id":"image_call","output":"CLIENT_GENERATED_IMAGE"}));
            let continued = Request::parse(json!({"model":"m","input":history})).unwrap();
            assert!(
                continued
                    .input
                    .iter()
                    .any(|i| i["output"] == "CLIENT_GENERATED_IMAGE")
            );
        }
    }

    #[test]
    fn native_custom_calls_cannot_bypass_tool_scope_or_output_limits() {
        let event = json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"custom_tool_call","id":"custom","call_id":"call","name":"exec","input":"arbitrary code is client input"
        }}});
        for request in [
            json!({"model":"m","input":"draw","tools":[]}),
            json!({"model":"m","input":"draw","tools":[{"type":"function","name":"exec"}]}),
            json!({"model":"m","input":"draw","tools":[{"type":"custom","name":"exec"}],"tool_choice":"none"}),
        ] {
            let request = Request::parse(request).unwrap();
            assert_eq!(
                Output::new(&request)
                    .consume(&event, &request)
                    .unwrap_err()
                    .code,
                "NATIVE_TOOL_SCOPE"
            );
        }
        let request = Request::parse(
            json!({"model":"m","input":"draw","tools":[{"type":"custom","name":"exec"}]}),
        )
        .unwrap();
        let mut malformed = event.clone();
        malformed["params"]["item"]["input"] = json!({"not":"text"});
        assert_eq!(
            Output::new(&request)
                .consume(&malformed, &request)
                .unwrap_err()
                .code,
            "UPSTREAM_INVALID_RESPONSE"
        );
        let mut excessive = event;
        excessive["params"]["item"]["input"] = json!("x".repeat(crate::media::BODY_LIMIT + 1));
        let mut output = Output::new(&request);
        let error = output.consume(&excessive, &request).unwrap_err();
        assert_eq!(output.failed(&error)["response"]["status"], "failed");
        assert_eq!(output.failed(&error)["response"]["output"], json!([]));
    }
}
