//! Client-owned tool protocol. Nothing in this module executes a tool.
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::collections::HashMap;

fn invalid() -> Error {
    Error::new(
        "INVALID_INPUT",
        "工具请求无效；支持文本消息、function/custom 工具及对应结果，需回传完整上下文。",
    )
}
pub fn name(v: &Value) -> Result<&str> {
    v.as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
        .ok_or_else(invalid)
}
pub fn tools(value: &Value) -> Result<HashMap<String, String>> {
    let mut names = HashMap::new();
    if let Some(list) = value.get("tools") {
        let list = list.as_array().ok_or_else(invalid)?;
        if list.len() > 128 {
            return Err(invalid());
        }
        for tool in list {
            let kind = tool["type"].as_str().ok_or_else(invalid)?;
            if !["function", "custom"].contains(&kind) {
                return Err(invalid());
            }
            let n = name(&tool["name"])?;
            if names.insert(n.into(), kind.into()).is_some() {
                return Err(invalid());
            }
            if kind == "function" && !tool["parameters"].is_object() {
                return Err(invalid());
            }
            if tool.get("description").is_some_and(|v| !v.is_string())
                || tool
                    .get("strict")
                    .is_some_and(|v| !v.is_null() && !v.is_boolean())
            {
                return Err(invalid());
            }
            let allowed = if kind == "function" {
                &["type", "name", "description", "parameters", "strict"][..]
            } else {
                &["type", "name", "description", "format"][..]
            };
            if tool
                .as_object()
                .ok_or_else(invalid)?
                .keys()
                .any(|k| !allowed.contains(&k.as_str()))
            {
                return Err(invalid());
            }
        }
    }
    Ok(names)
}
fn text_content(v: &Value) -> bool {
    v.is_string()
        || v.as_array().is_some_and(|a| {
            a.iter().all(|p| {
                (["input_text", "output_text"].contains(&p["type"].as_str().unwrap_or(""))
                    && p["text"].is_string())
                    || (p["type"] == "refusal" && p["refusal"].is_string())
            })
        })
}
pub fn validate_request(value: &Value) -> Result<()> {
    if serde_json::to_vec(value)?.len() > 1024 * 1024 {
        return Err(invalid());
    }
    let names = tools(value)?;
    if let Some(choice) = value.get("tool_choice") {
        if let Some(s) = choice.as_str() {
            if !["auto", "none", "required"].contains(&s) || (s == "required" && names.is_empty()) {
                return Err(invalid());
            }
        } else if names.get(name(&choice["name"])?).map(String::as_str) != choice["type"].as_str() {
            return Err(invalid());
        }
    }
    match &value["input"] {
        Value::String(s) if !s.trim().is_empty() => (),
        Value::Array(items) if !items.is_empty() => {
            let mut calls = HashMap::new();
            for item in items {
                match item["type"].as_str().unwrap_or("message") {
                    "message" => {
                        if !["system", "developer", "user", "assistant"]
                            .contains(&item["role"].as_str().unwrap_or(""))
                            || !text_content(&item["content"])
                        {
                            return Err(invalid());
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        let kind = if item["type"] == "function_call" {
                            "function"
                        } else {
                            "custom"
                        };
                        name(&item["name"])?;
                        let field = if kind == "function" {
                            "arguments"
                        } else {
                            "input"
                        };
                        if !item[field].is_string()
                            || calls
                                .insert(name(&item["call_id"])?.to_owned(), kind)
                                .is_some()
                        {
                            return Err(invalid());
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        let kind = if item["type"] == "function_call_output" {
                            "function"
                        } else {
                            "custom"
                        };
                        if calls.remove(name(&item["call_id"])?) != Some(kind)
                            || !text_content(&item["output"])
                        {
                            return Err(invalid());
                        }
                    }
                    "reasoning" => {
                        if !item["summary"].is_array() {
                            return Err(invalid());
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            if !calls.is_empty() {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
/// Replace server-agent instructions and input, retaining native transport metadata.
pub fn upstream_request(mut native: Value, client: &Value) -> Value {
    for key in [
        "input",
        "instructions",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "previous_response_id",
    ] {
        native.as_object_mut().unwrap().remove(key);
    }
    for (key, value) in client.as_object().unwrap() {
        native[key] = value.clone();
    }
    native["stream"] = json!(true);
    native["store"] = json!(false);
    native
}
