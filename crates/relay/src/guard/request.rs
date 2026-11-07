//! Restricts the native agent's upstream request to the admitted text-only model.
use super::identifier;
use crate::error::{Error, Result};
use serde_json::{Value, json};

pub fn text_request(mut body: Value, model: &str) -> Result<Value> {
    let err = || Error::new("GUARD_REQUEST_SHAPE", "仅支持指定模型的流式纯文本输入。");
    if body["model"] != model
        || body["stream"] != true
        || !body["previous_response_id"].is_null()
        || body
            .get("tools")
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(err());
    }
    let mut input = Vec::new();
    for item in body["input"].as_array().ok_or_else(err)? {
        if item["type"] == "additional_tools" {
            if item["role"] != "developer" || !item["tools"].is_array() {
                return Err(err());
            }
            continue;
        }
        if item.get("type").is_some_and(|v| v != "message")
            || !["system", "developer", "user", "assistant"]
                .contains(&item["role"].as_str().unwrap_or(""))
        {
            return Err(err());
        }
        let content = if item["content"].is_string() {
            item["content"].clone()
        } else {
            let mut parts = Vec::new();
            for part in item["content"].as_array().ok_or_else(err)? {
                if !["input_text", "output_text"].contains(&part["type"].as_str().unwrap_or(""))
                    || !part["text"].is_string()
                {
                    return Err(err());
                }
                parts.push(json!({"type":part["type"],"text":part["text"]}));
            }
            parts.into()
        };
        let mut message = json!({"type":"message","role":item["role"],"content":content});
        if item.get("id").is_some() {
            message["id"] = identifier(item, "id").map_err(|_| err())?.into();
        }
        if let Some(phase) = item.get("phase") {
            if !phase.is_null()
                && !["commentary", "final_answer"].contains(&phase.as_str().unwrap_or(""))
            {
                return Err(err());
            }
            message["phase"] = phase.clone();
        }
        input.push(message);
    }
    body["input"] = input.into();
    body["tools"] = json!([]);
    body["tool_choice"] = json!("none");
    body["parallel_tool_calls"] = json!(false);
    body["store"] = json!(false);
    Ok(body)
}
