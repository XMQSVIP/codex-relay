//! Drop unsupported options at the HTTP boundary, never input content or schemas.
use super::invalid;
use crate::error::Result;
use serde_json::{Value, json};

pub(super) fn client_metadata(v: &Value) -> Option<Value> {
    let raw = v["client_metadata"]["x-codex-turn-metadata"].as_str()?;
    let mut value: Value = serde_json::from_str(raw).ok()?;
    value.as_object_mut()?.retain(|key, value| {
        value.is_string()
            && !["session_id", "thread_id", "turn_id", "window_id"].contains(&key.as_str())
    });
    Some(value)
}

/// Report field paths and behavior only; never retain caller option values.
pub(super) fn diagnose(v: &Value) -> Vec<Value> {
    let mut result = vec![];
    let mut note = |path: String, action: &str, reason: &str| {
        result.push(json!({"path":path,"action":action,"reason":reason}))
    };
    if v["store"] == true {
        note(
            "store".into(),
            "ignored",
            "中转站不保存聊天正文，响应始终返回 store=false",
        );
    }
    for key in v.as_object().into_iter().flat_map(|m| m.keys()) {
        if crate::native_cache::enabled()
            && [
                "session_id",
                "conversation_id",
                "thread_id",
                "parent_thread_id",
                "forked_from_thread_id",
                "forked_from_id",
                "x-codex-turn-metadata",
                "turn_metadata",
                "turnMetadata",
            ]
            .contains(&key.as_str())
        {
            note(
                key.clone(),
                "adapted",
                "仅提取本地账户亲和与线程隔离身份，不覆盖原生会话身份",
            );
            continue;
        }
        if key == "prompt_cache_key" && crate::native_cache::enabled() {
            note(
                key.clone(),
                "adapted",
                "仅作为中转站账户亲和提示，不控制原生或上游缓存键",
            );
            continue;
        }
        if ![
            "model",
            "input",
            "stream",
            "store",
            "previous_response_id",
            "instructions",
            "tools",
            "tool_choice",
            "reasoning",
            "text",
            "metadata",
            "service_tier",
            "type",
            "stream_id",
            "event_id",
            "generate",
            "access_programs",
            "client_metadata",
        ]
        .contains(&key.as_str())
        {
            note(key.clone(), "ignored", "原生 RPC 没有此参数的可靠映射");
        }
    }
    if !v["tool_choice"].is_null() && v["tool_choice"] != "auto" && v["tool_choice"] != "none" {
        note(
            "tool_choice".into(),
            "adapted",
            "使用 auto；不能保证强制工具选择",
        );
    }
    if v["text"]["format"]["type"] == "json_object" {
        note(
            "text.format".into(),
            "adapted",
            "提示词与最终 JSON 校验，不是约束解码",
        );
    }
    if v["reasoning"]["context"] == "all_turns" {
        note(
            "reasoning.context".into(),
            "adapted",
            "采用完整历史中最后一条 configuration_update 的推理强度；无更新时使用顶层 effort",
        );
    }
    if v["text"]["format"]["strict"] == false {
        note(
            "text.format.strict".into(),
            "ignored",
            "原生 outputSchema 不提供 strict=false 开关",
        );
    }
    if !v["client_metadata"].is_null() {
        note(
            "client_metadata".into(),
            "adapted",
            if crate::native_cache::enabled() {
                "提取本地会话身份；仅映射 x-codex-turn-metadata 中非保留的字符串字段到原生 turn"
            } else {
                "仅映射 x-codex-turn-metadata 中非保留的字符串字段"
            },
        );
    }
    if !v["access_programs"].is_null() {
        note(
            "access_programs.cyber".into(),
            if matches!(
                v["access_programs"]["cyber"].as_str(),
                Some("standard" | "daybreak_blue" | "daybreak_red")
            ) {
                "native"
            } else {
                "ignored"
            },
            "映射到原生 cyberAccessProgram；原生只对 OpenAI 提供方发送，API Key 另受原生功能开关约束",
        );
    }
    fn tool_notes(tools: &Value, path: &str, out: &mut Vec<Value>) {
        for (i, tool) in tools.as_array().into_iter().flatten().enumerate() {
            let path = format!("{path}[{i}]");
            let mut note = |suffix: &str, reason: &str| {
                out.push(
                    json!({"path":format!("{path}{suffix}"),"action":"adapted","reason":reason}),
                )
            };
            if tool["type"] == "function" && tool["strict"] == true {
                note(".strict", "原生动态函数使用非严格模式");
            }
            if tool["type"] == "custom" {
                note("", "字符串参数函数适配；grammar 不提供约束解码");
            }
            if tool["type"] == "tool_search" {
                note("", "客户端函数适配");
            }
            if tool["type"] == "web_search" || tool["type"] == "image_generation" {
                for key in tool
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys())
                    .filter(|k| k.as_str() != "type")
                {
                    if tool["type"] == "web_search"
                        && super::search::OPTIONS.contains(&key.as_str())
                    {
                        out.push(json!({"path":format!("{path}.{key}"),"action":"adapted","reason":"映射到隔离线程的原生搜索配置；账户位置策略优先"}));
                    } else {
                        out.push(json!({"path":format!("{path}.{key}"),"action":"ignored","reason":"原生工具配置没有逐请求可靠映射"}));
                    }
                }
            }
            if tool["type"] == "namespace" {
                tool_notes(&tool["tools"], &format!("{path}.tools"), out);
            }
        }
    }
    tool_notes(&v["tools"], "tools", &mut result);
    for (i, item) in v["input"].as_array().into_iter().flatten().enumerate() {
        if item["type"] == "additional_tools" || item["type"] == "tool_search_output" {
            tool_notes(&item["tools"], &format!("input[{i}].tools"), &mut result);
        }
    }
    for entry in &mut result {
        if let Some(path) = entry["path"].as_str() {
            entry["path"] = json!(path.chars().take(256).collect::<String>());
        }
    }
    if result.len() > 128 {
        result.truncate(128);
        result.push(json!({"path":"*","action":"adapted","reason":"更多重复参数说明已省略；完整能力边界见能力诊断"}));
    }
    result
}

fn retain(value: &mut Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("参数必须是 JSON 对象。"))?;
    object.retain(|key, _| allowed.contains(&key.as_str()));
    Ok(())
}

pub(super) fn tools(value: &mut Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let list = value
        .as_array_mut()
        .ok_or_else(|| invalid("tools 必须是数组。"))?;
    for tool in list {
        match tool["type"].as_str() {
            Some("function") => {
                // Native dynamic functions have no strict-mode switch. Preserve
                // the caller's JSON Schema verbatim, but use native non-strict mode.
                retain(
                    tool,
                    &["type", "name", "description", "parameters", "defer_loading"],
                )?;
            }
            Some("namespace") => {
                retain(tool, &["type", "name", "description", "tools"])?;
                tools(&mut tool["tools"])?;
            }
            Some("custom") => {
                retain(
                    tool,
                    &["type", "name", "description", "format", "defer_loading"],
                )?;
            }
            Some("tool_search") => {
                retain(tool, &["type", "execution", "description", "parameters"])?
            }
            Some("web_search") => {
                retain(
                    tool,
                    &[
                        "type",
                        "search_context_size",
                        "external_web_access",
                        "filters",
                        "user_location",
                    ],
                )?;
                if !tool["filters"].is_null() {
                    retain(&mut tool["filters"], &["allowed_domains"])?;
                }
                if !tool["user_location"].is_null() {
                    retain(
                        &mut tool["user_location"],
                        &["type", "country", "region", "city", "timezone"],
                    )?;
                }
            }
            Some("image_generation") => retain(tool, &["type"])?,
            // Unsupported tool kinds and malformed definitions remain errors;
            // discarding a tool or input is not the same as ignoring an option.
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn normalize(mut value: Value) -> Result<Value> {
    retain(
        &mut value,
        &[
            "model",
            "input",
            "stream",
            "store",
            "previous_response_id",
            "instructions",
            "tools",
            "tool_choice",
            "reasoning",
            "text",
            "metadata",
            "service_tier",
        ],
    )?;
    if value["tool_choice"] != "auto" && value["tool_choice"] != "none" {
        value["tool_choice"] = json!("auto");
    }
    if !value["reasoning"].is_null() {
        retain(&mut value["reasoning"], &["effort", "summary"])?;
    }
    if !value["text"].is_null() {
        retain(&mut value["text"], &["format", "verbosity"])?;
        let format = &mut value["text"]["format"];
        if !format.is_null() {
            // Known formats keep their own validation. Unknown formats are not
            // coerced into text, since that could break consumers expecting JSON.
            match format["type"].as_str() {
                Some("text" | "json_object") => retain(format, &["type"])?,
                Some("json_schema") => {
                    retain(format, &["type", "name", "schema", "description", "strict"])?
                }
                _ => {}
            }
        }
    }
    tools(&mut value["tools"])?;
    Ok(value)
}
