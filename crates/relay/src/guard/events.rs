//! Validates event identity and output scope before forwarding upstream content.
use super::{blocked, identifier, string};
use crate::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

mod sse;
pub use sse::TextSse;

fn index(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .filter(|i| *i <= 4096)
        .ok_or_else(blocked)
}
fn part(value: &Value) -> Result<Value> {
    match string(value, "type")? {
        "output_text" => {
            Ok(json!({"type":"output_text","text":string(value,"text")?,"annotations":[]}))
        }
        "refusal" => Ok(json!({"type":"refusal","refusal":string(value,"refusal")?})),
        _ => Err(blocked()),
    }
}
fn message(value: &Value) -> Result<Value> {
    if value["type"] != "message" || value["role"] != "assistant" {
        return Err(blocked());
    }
    let content: Vec<Value> = value["content"]
        .as_array()
        .ok_or_else(blocked)?
        .iter()
        .map(part)
        .collect::<Result<_>>()?;
    let mut result =
        json!({"type":"message","id":identifier(value,"id")?,"role":"assistant","content":content});
    if let Some(status) = value.get("status") {
        if !["in_progress", "completed", "incomplete"].contains(&status.as_str().unwrap_or("")) {
            return Err(blocked());
        }
        result["status"] = status.clone();
    }
    if let Some(phase) = value.get("phase") {
        if !phase.is_null()
            && !["commentary", "partial_answer", "final_answer"]
                .contains(&phase.as_str().unwrap_or(""))
        {
            return Err(blocked());
        }
        result["phase"] = phase.clone();
    }
    Ok(result)
}

#[derive(Default)]
pub struct TextEvents {
    items: HashMap<String, u64>,
    response_id: Option<String>,
    terminal: bool,
    poisoned: bool,
    client_tools: Option<HashMap<String, String>>,
    tool_items: HashMap<String, (u64, Value)>,
}
impl TextEvents {
    fn output_item(&self, value: &Value) -> Result<Value> {
        if let Some(names) = &self.client_tools {
            let kind = value["type"].as_str().ok_or_else(blocked)?;
            if kind == "reasoning" {
                identifier(value, "id")?;
                if !value["summary"].is_array() {
                    return Err(blocked());
                }
                return Ok(value.clone());
            }
            if ["function_call", "custom_tool_call"].contains(&kind) {
                let expected = if kind == "function_call" {
                    "function"
                } else {
                    "custom"
                };
                if names.get(string(value, "name")?).map(String::as_str) != Some(expected) {
                    return Err(blocked());
                }
                identifier(value, "id")?;
                identifier(value, "call_id")?;
                string(
                    value,
                    if expected == "function" {
                        "arguments"
                    } else {
                        "input"
                    },
                )?;
                return Ok(value.clone());
            }
        }
        message(value)
    }
    pub fn accept(&mut self, value: Value, event_name: Option<&str>) -> Result<Option<Value>> {
        if self.poisoned {
            return Err(Error::new("GUARD_CLOSED", "过滤器已关闭。"));
        }
        let result = self.accept_inner(value, event_name);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn accept_inner(&mut self, value: Value, event_name: Option<&str>) -> Result<Option<Value>> {
        let kind = string(&value, "type")?;
        if event_name.is_some_and(|name| name != kind) {
            return Err(blocked());
        }
        let mut out = json!({"type":kind});
        if let Some(number) = value.get("sequence_number") {
            if !number.is_u64() {
                return Err(blocked());
            }
            out["sequence_number"] = number.clone();
        }
        // Native SSE also carries transport metadata and rate-limit updates.
        // Single-turn text output needs neither; discard the whole frame so
        // opaque turn-state and arbitrary extra fields never reach consumers.
        if [
            "codex.response.metadata",
            "response.metadata",
            "codex.rate_limits",
            "responsesapi.websocket_timing",
        ]
        .contains(&kind)
        {
            return Ok(None);
        }
        if self.terminal {
            return Err(blocked());
        }
        if [
            "response.reasoning_text.delta",
            "response.reasoning_text.done",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_part.done",
        ]
        .contains(&kind)
        {
            return Ok(None);
        }
        match kind {
            "response.created" | "response.in_progress" | "response.completed" => {
                let r = &value["response"];
                let id = identifier(r, "id")?;
                if self.response_id.as_ref().is_some_and(|old| old != &id) {
                    return Err(blocked());
                }
                self.response_id = Some(id.clone());
                let mut output = Vec::new();
                if let Some(items) = r.get("output") {
                    for item in items.as_array().ok_or_else(blocked)? {
                        if item["type"] != "reasoning" || self.client_tools.is_some() {
                            output.push(self.output_item(item)?);
                        }
                    }
                }
                let mut response = json!({"id":id,"output":output});
                for key in ["object", "model", "created_at", "status"] {
                    if let Some(v) = r.get(key) {
                        let valid = match key {
                            "object" => v == "response",
                            "model" => v.is_string(),
                            "created_at" => v.is_number(),
                            "status" => ["queued", "in_progress", "completed"]
                                .contains(&v.as_str().unwrap_or("")),
                            _ => false,
                        };
                        if !valid {
                            return Err(blocked());
                        }
                        response[key] = v.clone();
                    }
                }
                if let Some(usage) = r.get("usage") {
                    if usage.is_null() {
                        response["usage"] = Value::Null;
                    } else {
                        let mut clean = Map::new();
                        for key in ["input_tokens", "output_tokens", "total_tokens"] {
                            clean.insert(
                                key.into(),
                                usage[key].as_u64().ok_or_else(blocked)?.into(),
                            );
                        }
                        for (group, key) in [
                            ("input_tokens_details", "cached_tokens"),
                            ("output_tokens_details", "reasoning_tokens"),
                        ] {
                            if let Some(details) = usage.get(group) {
                                if !details.is_object() {
                                    return Err(blocked());
                                }
                                let mut fields = Map::new();
                                if let Some(v) = details.get(key) {
                                    fields
                                        .insert(key.into(), v.as_u64().ok_or_else(blocked)?.into());
                                }
                                clean.insert(group.into(), fields.into());
                            }
                        }
                        response["usage"] = clean.into();
                    }
                }
                if kind == "response.completed" {
                    self.terminal = true;
                }
                out["response"] = response;
            }
            "response.output_item.added" | "response.output_item.done" => {
                if value["item"]["type"] == "reasoning" && self.client_tools.is_none() {
                    return Ok(None);
                }
                let item = self.output_item(&value["item"])?;
                let id = identifier(&item, "id")?;
                let output_index = if value.get("output_index").is_some() {
                    index(&value, "output_index")?
                } else {
                    0
                };
                if self.items.get(&id).is_some_and(|i| *i != output_index) {
                    return Err(blocked());
                }
                if ["function_call", "custom_tool_call"]
                    .contains(&item["type"].as_str().unwrap_or(""))
                {
                    if let Some((old_index, old)) = self.tool_items.get(&id) {
                        if *old_index != output_index
                            || old["call_id"] != item["call_id"]
                            || old["name"] != item["name"]
                            || old["type"] != item["type"]
                        {
                            return Err(blocked());
                        }
                    }
                    self.tool_items
                        .insert(id.clone(), (output_index, item.clone()));
                }
                self.items.insert(id, output_index);
                out["item"] = item;
                out["output_index"] = output_index.into();
            }
            "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done" => {
                let id = identifier(&value, "item_id")?;
                let idx = index(&value, "output_index")?;
                let (old_index, item) = self.tool_items.get(&id).ok_or_else(blocked)?;
                let function = kind.starts_with("response.function_call_arguments.");
                if *old_index != idx || (item["type"] == "function_call") != function {
                    return Err(blocked());
                }
                let field = if kind.ends_with(".delta") {
                    "delta"
                } else if function {
                    "arguments"
                } else {
                    "input"
                };
                out["item_id"] = id.into();
                out["output_index"] = idx.into();
                out[field] = string(&value, field)?.into();
            }
            "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done" => {
                let id = identifier(&value, "item_id")?;
                let output_index = index(&value, "output_index")?;
                let content_index = index(&value, "content_index")?;
                if self.items.get(&id) != Some(&output_index) {
                    return Err(blocked());
                }
                out["item_id"] = id.into();
                out["output_index"] = output_index.into();
                out["content_index"] = content_index.into();
                if kind.starts_with("response.content_part.") {
                    out["part"] = part(&value["part"])?;
                } else {
                    let field = if kind.ends_with(".delta") {
                        "delta"
                    } else if kind.starts_with("response.refusal.") {
                        "refusal"
                    } else {
                        "text"
                    };
                    out[field] = string(&value, field)?.into();
                }
            }
            _ => return Err(blocked()),
        }
        // The client channel never enters the native agent. Validate its
        // protocol, but preserve upstream fields rather than reconstructing it.
        Ok(Some(if self.client_tools.is_some() {
            value
        } else {
            out
        }))
    }
    fn finish(&self) -> Result<()> {
        if self.poisoned || !self.terminal {
            Err(Error::new("GUARD_TRUNCATED", "上游文本流未完整结束。"))
        } else {
            Ok(())
        }
    }
}
