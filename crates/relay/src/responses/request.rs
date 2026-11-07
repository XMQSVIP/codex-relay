use super::invalid;
use crate::error::Result;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Request {
    pub(super) original: std::sync::Arc<Value>,
    pub(super) routing_cache_key: Option<[u8; 32]>,
    pub(crate) client_session: Option<[u8; 32]>,
    pub(super) identity: super::affinity::identity::Body,
    pub(crate) reuse_allowed: bool,
    pub(crate) native_observation: std::sync::Arc<crate::profiles::continuations::Observation>,
    pub model: String,
    pub input: Vec<Value>,
    pub stream: bool,
    pub store: bool,
    pub previous: Option<String>,
    pub instructions: Option<String>,
    pub tools: Vec<Value>,
    top_level_tools: Vec<Value>,
    pub tool_choice: Value,
    pub reasoning: Value,
    reasoning_context_all_turns: bool,
    pub text: Value,
    pub metadata: Value,
    pub service_tier: Option<String>,
    pub parameter_handling: Vec<Value>,
    pub cyber_access_program: Option<String>,
    pub client_metadata: Option<Value>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub(crate) compact_endpoint: bool,
    // Transient wire options for explicit compaction; never sent to app-server or storage.
    pub(crate) compaction_body: Option<Value>,
    pub(super) control: Option<std::sync::Arc<super::control::Control>>,
}
fn keys(v: &Value, allowed: &[&str]) -> Result<()> {
    let map = v
        .as_object()
        .ok_or_else(|| invalid("参数必须是 JSON 对象。"))?;
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid(&format!("当前原生接口不支持参数：{key}")));
    }
    Ok(())
}
fn boolean(v: &Value, key: &str, default: bool) -> Result<bool> {
    if v[key].is_null() {
        Ok(default)
    } else {
        v[key]
            .as_bool()
            .ok_or_else(|| invalid(&format!("{key} 必须是布尔值。")))
    }
}
fn optional_string(v: &Value, key: &str) -> Result<Option<String>> {
    if v[key].is_null() {
        Ok(None)
    } else {
        v[key]
            .as_str()
            .map(|s| Some(s.into()))
            .ok_or_else(|| invalid(&format!("{key} 必须是字符串。")))
    }
}
fn nonempty<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| invalid(&format!("{key} 缺失或无效。")))
}
fn tool_output(value: &Value) -> Result<()> {
    if value.is_string() {
        return Ok(());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| invalid("function_call_output.output 必须是字符串或内容数组。"))?;
    for part in parts {
        match part["type"].as_str() {
            Some("input_text") => {
                keys(part, &["type", "text"])?;
                if !part["text"].is_string() {
                    return Err(invalid("工具结果 input_text.text 必须是字符串。"));
                }
            }
            Some("input_image") => {
                keys(part, &["type", "image_url", "detail"])?;
                crate::media::inline_image(
                    part["image_url"]
                        .as_str()
                        .ok_or_else(|| invalid("工具结果图片内容无效。"))?,
                )?;
                if !part["detail"].is_null()
                    && !["auto", "low", "high", "original"]
                        .contains(&part["detail"].as_str().unwrap_or(""))
                {
                    return Err(invalid("工具结果图片 detail 无效。"));
                }
            }
            _ => {
                return Err(invalid(
                    "工具结果内容数组目前支持 input_text 和内嵌 input_image。",
                ));
            }
        }
    }
    Ok(())
}

/// Validate the optional reasoning content carried by the native Responses
/// history. The official Codex protocol supports both the provider-neutral
/// `text` variant and the OpenAI `reasoning_text` variant.
pub(super) fn validate_reasoning_content(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| invalid("reasoning.content 必须是内容数组或 null。"))?;
    for part in parts {
        keys(part, &["type", "text"])?;
        if !["reasoning_text", "text"].contains(&part["type"].as_str().unwrap_or(""))
            || !part["text"].is_string()
        {
            return Err(invalid(
                "reasoning.content 目前只支持 reasoning_text 和 text。",
            ));
        }
    }
    Ok(())
}

impl Request {
    pub fn parse(v: Value) -> Result<Self> {
        // Desktop can keep the initial effort at the top level and append
        // configuration_update items when the user changes it mid-conversation.
        let reasoning_context_all_turns = v["reasoning"]["context"] == "all_turns";
        let compaction_body = v["input"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["type"] == "compaction_trigger")
            })
            .then(|| v.clone());
        let identity = super::affinity::identity::Body::parse(&v);
        let routing_cache_key = v["prompt_cache_key"]
            .as_str()
            .filter(|key| !key.chars().any(char::is_control))
            .map(str::trim)
            .filter(|key| !key.is_empty() && key.len() <= 512)
            .and_then(|key| crate::profiles::warm::digest(&key).ok());
        let original =
            std::sync::Arc::new(crate::operations::request_content::request_snapshot(&v));
        let parameter_handling = super::compatibility::diagnose(&v);
        let cyber_access_program = match v["access_programs"]["cyber"].as_str() {
            Some("standard") => Some("standard".into()),
            Some("daybreak_blue") => Some("daybreakBlue".into()),
            Some("daybreak_red") => Some("daybreakRed".into()),
            _ => None,
        };
        let client_metadata = super::compatibility::client_metadata(&v);
        let subagent_kind = crate::subagent::from_body(&v);
        let v = super::compatibility::normalize(v)?;
        // Accept the public option, but never persist response bodies.
        boolean(&v, "store", false)?;
        let model = nonempty(&v, "model")?.to_owned();
        let service_tier = optional_string(&v, "service_tier")?;
        if service_tier
            .as_deref()
            .is_some_and(|s| !["auto", "default", "flex", "priority", "ultrafast"].contains(&s))
        {
            return Err(invalid(
                "service_tier 仅支持 auto/default/flex/priority/ultrafast。",
            ));
        }
        let tool_choice = if v["tool_choice"].is_null() {
            json!("auto")
        } else {
            v["tool_choice"].clone()
        };
        let top_level_tools = super::tool_catalog::parse(&v["tools"])?;
        crate::media::budget(&v, crate::media::BODY_LIMIT)?;
        let input = normalize_input(&v["input"])?;
        let compact = input
            .iter()
            .any(|item| item["type"] == "compaction_trigger");
        if compact
            && (input
                .last()
                .is_none_or(|v| v["type"] != "compaction_trigger")
                || input
                    .iter()
                    .filter(|v| v["type"] == "compaction_trigger")
                    .count()
                    != 1)
        {
            return Err(invalid("compaction_trigger 必须是最后且唯一的压缩控制项。"));
        }
        let tools = super::tool_catalog::resolve(&top_level_tools, &input)?;
        let reasoning = if v["reasoning"].is_null() {
            json!({})
        } else {
            v["reasoning"].clone()
        };
        keys(&reasoning, &["effort", "summary"])?;
        if let Some(e) = optional_string(&reasoning, "effort")? {
            if ![
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&e.as_str())
            {
                return Err(invalid("reasoning.effort 无效。"));
            }
        }
        if let Some(s) = optional_string(&reasoning, "summary")? {
            if !["auto", "concise", "detailed", "none"].contains(&s.as_str()) {
                return Err(invalid("reasoning.summary 无效。"));
            }
        }
        let mut text = if v["text"].is_null() {
            json!({"format":{"type":"text"}})
        } else {
            v["text"].clone()
        };
        keys(&text, &["format", "verbosity"])?;
        if let Some(verbosity) = optional_string(&text, "verbosity")? {
            if !["low", "medium", "high"].contains(&verbosity.as_str()) {
                return Err(invalid("text.verbosity 仅支持 low/medium/high。"));
            }
        }
        if text["format"].is_null() {
            text["format"] = json!({"type":"text"});
        }
        if !text["format"].is_null() {
            let f = &text["format"];
            match f["type"].as_str() {
                Some("text" | "json_object") => keys(f, &["type"])?,
                Some("json_schema") => {
                    keys(f, &["type", "name", "schema", "description", "strict"])?;
                    nonempty(f, "name")?;
                    boolean(f, "strict", true)?;
                    if !f["schema"].is_object() {
                        return Err(invalid("text.format.schema 必须是对象。"));
                    }
                }
                _ => {
                    return Err(invalid(
                        "text.format 必须是对象，type 仅支持 text、json_object 或 json_schema。",
                    ));
                }
            }
        }
        let metadata = if v["metadata"].is_null() {
            json!({})
        } else {
            v["metadata"].clone()
        };
        if !metadata.as_object().is_some_and(|m| {
            m.len() <= 16
                && m.iter().all(|(k, v)| {
                    k.chars().count() <= 64 && v.as_str().is_some_and(|s| s.chars().count() <= 512)
                })
        }) {
            return Err(invalid(
                "metadata 最多 16 个字符串键值对，键/值长度上限为 64/512。",
            ));
        }
        Ok(Self {
            original,
            routing_cache_key,
            client_session: None,
            identity,
            reuse_allowed: true,
            native_observation: Default::default(),
            model,
            input,
            stream: boolean(&v, "stream", false)?,
            store: false,
            previous: optional_string(&v, "previous_response_id")?,
            instructions: optional_string(&v, "instructions")?,
            tools,
            top_level_tools,
            tool_choice,
            reasoning,
            reasoning_context_all_turns,
            text,
            metadata,
            service_tier,
            parameter_handling,
            cyber_access_program,
            client_metadata,
            subagent_kind,
            compact,
            compact_endpoint: false,
            compaction_body,
            control: None,
        })
    }
    pub fn has_tool(&self, name: &str) -> bool {
        self.has_scoped_tool(None, name)
    }
    pub fn has_scoped_tool(&self, namespace: Option<&str>, name: &str) -> bool {
        self.tool_choice != "none"
            && (crate::protocol_tools::allows(&self.tools, namespace, name)
                || self.is_custom_tool(namespace, name))
    }
    pub fn is_custom_tool(&self, namespace: Option<&str>, name: &str) -> bool {
        super::tool_catalog::is_custom(&self.tools, namespace, name)
    }
    pub(super) fn client_tool_identity(
        &self,
        namespace: Option<&str>,
        name: &str,
    ) -> Option<(Option<String>, String)> {
        if self.tool_choice == "none" {
            return None;
        }
        super::tool_catalog::client_identity(&self.tools, namespace, name)
    }
    pub fn with_history(&self, history: &[Value]) -> Result<Self> {
        let mut request = self.clone();
        request.tools = super::tool_catalog::resolve(&self.top_level_tools, history)?;
        let effort = effective_effort(&self.reasoning, history, self.reasoning_context_all_turns);
        if !effort.is_null() {
            request.reasoning["effort"] = effort.clone();
        }
        Ok(request)
    }
    pub fn dynamic_tools(&self) -> Vec<Value> {
        if self.tool_choice == "none" {
            vec![]
        } else {
            super::tool_catalog::native(&self.tools)
        }
    }
    pub fn required_features(&self, history: &[Value]) -> Result<Vec<crate::operations::Feature>> {
        use crate::operations::Feature;
        let mut features = vec![];
        if crate::media::budget(&json!(history), crate::media::BODY_LIMIT)? {
            features.push(Feature::ImageInput);
        }
        if self.tools.iter().any(|t| t["type"] == "namespace") {
            features.push(Feature::NamespacedTools);
        }
        for (name, feature) in [
            ("web_search", Feature::WebSearch),
            ("image_generation", Feature::ImageGeneration),
        ] {
            if self.tool_choice != "none" && self.tools.iter().any(|t| t["type"] == name) {
                features.push(feature);
            }
        }
        Ok(features)
    }
    pub fn apply_thread(&self, params: &mut Value) {
        if self.tool_choice != "none"
            && let Some(tool) = self.tools.iter().find(|tool| tool["type"] == "web_search")
        {
            super::search::apply(tool, params);
        }
        if crate::native_cache::enabled() {
            // Official live-turn settings updates require this feature. Only
            // effort, summary and tier are updated after validating a tool wait.
            params["config"]["features.step_model_switching"] = json!(true);
        }
        // Both desktop and SDK callers already supply their intended instructions.
        // Avoid layering the inner agent's default base prompt over that request.
        params["baseInstructions"] = json!(self.instructions.as_deref().unwrap_or(""));
        params["developerInstructions"] = json!("");
        if !self.text["verbosity"].is_null() {
            params["config"]["model_verbosity"] = self.text["verbosity"].clone();
        }
        if self.json_object() {
            // app-server exposes outputSchema (strict JSON Schema), but no JSON
            // mode. Do not send an unconstrained schema as strict=true: providers
            // reject it. Use instructions plus validation before emitting text.
            let original = params["developerInstructions"].as_str().unwrap_or("");
            params["developerInstructions"] = json!(format!(
                "{original}\n\nOutput format: Respond with exactly one valid JSON object. \
                 Do not wrap the JSON in Markdown or add prose outside it. \
                 Tool calls may be used when needed; the final answer must be a JSON object."
            ));
        }
    }
    pub fn json_object(&self) -> bool {
        self.text["format"]["type"] == "json_object"
    }
    pub fn apply_turn(&self, params: &mut Value) {
        if let Some(program) = &self.cyber_access_program {
            params["cyberAccessProgram"] = json!(program);
        }
        if let Some(metadata) = &self.client_metadata {
            params["responsesapiClientMetadata"] = metadata.clone();
        }
        if let Some(tier) = &self.service_tier {
            // Native "auto" inherits the configured tier; other values override it.
            if tier != "auto" {
                // Responses requests are independent: a priority request must
                // not change the default for subsequent requests on this thread.
                params[if crate::native_cache::enabled() {
                    "serviceTierForTurn"
                } else {
                    "serviceTier"
                }] = json!(tier);
            }
        }
        for (from, to) in [("effort", "effort"), ("summary", "summary")] {
            if !self.reasoning[from].is_null() {
                params[to] = self.reasoning[from].clone();
            }
        }
        if self.text["format"]["type"] == "json_schema" {
            params["outputSchema"] = self.text["format"]["schema"].clone();
        }
    }
}

pub(super) fn effective_effort<'a>(
    reasoning: &'a Value,
    history: &'a [Value],
    all_turns: bool,
) -> &'a Value {
    if (all_turns || reasoning["effort"].is_null())
        && let Some(update) = history
            .iter()
            .rev()
            .find(|item| item["type"] == "configuration_update")
    {
        &update["reasoning"]["effort"]
    } else {
        &reasoning["effort"]
    }
}

pub(super) fn configuration_effort(value: &Value) -> Result<&str> {
    keys(value, &["type", "reasoning"])?;
    keys(&value["reasoning"], &["effort"])?;
    let effort = value["reasoning"]["effort"].as_str().unwrap_or("");
    if ![
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ]
    .contains(&effort)
    {
        return Err(invalid("configuration_update.reasoning.effort 无效。"));
    }
    Ok(effort)
}

pub(super) fn normalize_input(input: &Value) -> Result<Vec<Value>> {
    let values = if let Some(text) = input.as_str() {
        vec![json!({"role":"user","content":text})]
    } else {
        input
            .as_array()
            .cloned()
            .ok_or_else(|| invalid("input 必须是字符串或消息数组。"))?
    };
    if values.len() > crate::media::HISTORY_COUNT {
        return Err(invalid("input 最多包含 16384 个条目。"));
    }
    values.into_iter().map(|mut v|{
        // Optional native provenance is carried through as opaque JSON. It never
        // selects a local account, workspace, execution policy or credential.
        let mut provenance=serde_json::Map::new();
        if let Some(map)=v.as_object_mut() {
            for key in ["encrypted_content","encrypted_function_args","internal_chat_message_metadata_passthrough","agent"] {
                if key!="encrypted_content" || !matches!(map.get("type").and_then(Value::as_str),Some("reasoning"|"compaction"|"compaction_summary"|"context_compaction")) {
                    if let Some(value)=map.remove(key) { provenance.insert(key.into(),value); }
                }
            }
        }
        let kind=v["type"].as_str().unwrap_or("message").to_owned();
        match kind.as_str(){
            "compaction"|"compaction_summary"=>{
                keys(&v,&["type","id","encrypted_content"])?;
                if !v["encrypted_content"].as_str().is_some_and(|v|!v.is_empty()){return Err(invalid("compaction 缺少加密内容。"));}
                v["type"]=json!("compaction");
            },
            "context_compaction"=>{keys(&v,&["type","id","encrypted_content"])?;optional_string(&v,"encrypted_content")?;},
            "compaction_trigger"=>{keys(&v,&["type"])?;},
            "configuration_update"=>{
                configuration_effort(&v)?;
            },
            "agent_message"=>{
                keys(&v,&["type","id","author","recipient","content"])?;
                nonempty(&v,"author")?;nonempty(&v,"recipient")?;
                if !v["content"].as_array().is_some_and(|a|a.iter().all(|part|match part["type"].as_str(){Some("input_text")=>part["text"].is_string(),Some("encrypted_content")=>part["encrypted_content"].as_str().is_some_and(|s|!s.is_empty()),_=>false})){return Err(invalid("agent_message 内容须为 input_text 或 encrypted_content。"));}
            },
            "tool_search_call"=>{
                keys(&v,&["type","id","call_id","status","execution","arguments"])?;
                if !matches!(v["execution"].as_str(),Some("client"|"server")) || !v["arguments"].is_object(){return Err(invalid("tool_search_call 无效。"));}
                if v["execution"]=="client" { nonempty(&v,"call_id")?; }
            },
            "tool_search_output"=>{
                keys(&v,&["type","id","call_id","status","execution","tools"])?;
                if !matches!(v["execution"].as_str(),Some("client"|"server")) || !v["tools"].is_array(){return Err(invalid("tool_search_output 无效。"));}
                if v["execution"]=="client" { nonempty(&v,"call_id")?; }
                v["tools"]=super::tool_catalog::parse(&v["tools"])?.into();
                if v["status"].is_null(){v["status"]=json!("completed");}
            },
            "additional_tools"=>{
                keys(&v,&["type","id","role","tools"])?;
                if v["role"]!="developer" || !v["tools"].is_array(){return Err(invalid("additional_tools 必须包含 developer role 和 tools 数组。"));}
                optional_string(&v,"id")?;
                v["tools"]=super::tool_catalog::parse(&v["tools"])?.into();
            },
            "message"=>{
                keys(&v,&["type","id","role","content","status","phase"])?;
                let role=nonempty(&v,"role")?.to_owned();
                if !["user","assistant","system","developer"].contains(&role.as_str()){return Err(invalid("消息 role 无效。"));}
                let content=if let Some(s)=v["content"].as_str(){vec![json!({"type":if role=="assistant"{"output_text"}else{"input_text"},"text":s})]}else{v["content"].as_array().cloned().ok_or_else(||invalid("消息 content 必须是文本或内容数组。"))?};
                for p in &content{
                    if p["type"]=="input_image" {
                        if role!="user"{return Err(invalid("图片只能作为 user 输入。"));}
                        keys(p,&["type","image_url","detail"])?;
                        crate::media::inline_image(p["image_url"].as_str().ok_or_else(||invalid("图片内容无效。"))?)?;
                        if !p["detail"].is_null() && !["auto","low","high","original"].contains(&p["detail"].as_str().unwrap_or("")){return Err(invalid("图片 detail 无效。"));}
                        continue;
                    }
                    if !["input_text","output_text"].contains(&p["type"].as_str().unwrap_or("")) || !p["text"].is_string(){return Err(invalid("当前只支持文本消息内容。"));}
                    keys(p,&["type","text","annotations","logprobs"])?;
                }
                // Native history drops system messages; caller instructions use developer.
                if role=="system" {v["role"]=json!("developer");}
                v["type"]=json!("message");v["content"]=content.into();
            },
            // Native FunctionCall keeps arguments as raw text. A client may be
            // replaying malformed model arguments together with its error result.
            "function_call"=>{keys(&v,&["type","id","call_id","name","namespace","arguments","status"])?;nonempty(&v,"name")?;if !v["namespace"].is_null(){nonempty(&v,"namespace")?;}nonempty(&v,"call_id")?;if !v["arguments"].is_string(){return Err(invalid("function_call.arguments 必须是字符串。"));}},
            "custom_tool_call"=>{keys(&v,&["type","id","call_id","name","namespace","input","status"])?;nonempty(&v,"name")?;if !v["namespace"].is_null(){nonempty(&v,"namespace")?;}nonempty(&v,"call_id")?;if !v["input"].is_string(){return Err(invalid("custom_tool_call.input 必须是字符串。"));}},
            "function_call_output"|"custom_tool_call_output"=>{keys(&v,&["type","id","call_id","name","namespace","output","status"])?;nonempty(&v,"call_id")?;optional_string(&v,"name")?;optional_string(&v,"namespace")?;tool_output(&v["output"])?;v.as_object_mut().unwrap().remove("status");},
            "web_search_call"=>{keys(&v,&["type","id","status","action"])?;nonempty(&v,"id")?;if v["status"]!="completed"{return Err(invalid("搜索历史必须已完成。"));}},
            "image_generation_call"=>{keys(&v,&["type","id","status","result","revised_prompt"])?;nonempty(&v,"id")?;if v["status"]!="completed"{return Err(invalid("生图历史必须已完成。"));}crate::media::generated_image(v["result"].as_str().ok_or_else(||invalid("生图结果无效。"))?)?;},
            "reasoning"=>{keys(&v,&["type","id","summary","content","encrypted_content","status"])?;if !v["summary"].is_array() || (!v["encrypted_content"].is_null()&&!v["encrypted_content"].is_string()){return Err(invalid("reasoning 条目无效。"));}validate_reasoning_content(&v["content"])?;v.as_object_mut().unwrap().remove("status");v.as_object_mut().unwrap().entry("encrypted_content").or_insert(Value::Null);},
            _=>return Err(invalid(&format!("不支持的 input 条目类型：{kind}")))
        }
        v.as_object_mut().unwrap().extend(provenance);
        Ok(v)
    }).collect()
}
pub(super) fn validate_history(history: &[Value]) -> Result<()> {
    if history.len() > crate::media::HISTORY_COUNT {
        return Err(invalid("合并历史最多包含 16384 个条目。"));
    }
    let mut calls: HashMap<&str, (&Value, bool)> = HashMap::new();
    for v in history {
        match v["type"].as_str() {
            Some("function_call" | "custom_tool_call" | "tool_search_call")
                if v["execution"] != "server" =>
            {
                let id = nonempty(v, "call_id")?;
                if calls.insert(id, (v, false)).is_some() {
                    return Err(invalid("工具调用 call_id 重复。"));
                }
            }
            Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
                if v["execution"] != "server" =>
            {
                let (call, completed) = calls.get_mut(nonempty(v, "call_id")?).ok_or_else(|| invalid(
                    "工具结果缺少匹配类型的工具调用；请使用 previous_response_id 或回传完整历史。"))?;
                let kind = if v["type"] == "custom_tool_call_output" {
                    "custom_tool_call"
                } else if v["type"] == "tool_search_output" {
                    "tool_search_call"
                } else {
                    "function_call"
                };
                if call["type"] != kind {
                    return Err(invalid("工具结果与调用类型不匹配。"));
                }
                if v["name"].as_str().is_some_and(|name| call["name"] != name)
                    || v["namespace"]
                        .as_str()
                        .is_some_and(|ns| call["namespace"].as_str().unwrap_or("functions") != ns)
                {
                    return Err(invalid("工具结果的 name/namespace 与原调用不匹配。"));
                }
                // Code-mode notify carries the public exec name; ordinary final
                // results omit it. Notifications may also arrive after a yield.
                let supplemental = kind == "custom_tool_call"
                    && call["name"] == "exec"
                    && call["namespace"] == "functions"
                    && v["name"] == "exec";
                if !supplemental {
                    if *completed {
                        return Err(invalid("工具调用的正式结果重复。"));
                    }
                    *completed = true;
                }
            }
            _ => {}
        }
    }
    if calls.values().any(|(_, completed)| !completed) {
        return Err(invalid("请提交全部待处理工具调用的结果。"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_model_arguments_can_be_replayed_with_client_error() {
        let history = json!([
            {"type":"function_call","call_id":"bad_args","name":"echo","arguments":"{broken"},
            {"type":"function_call_output","call_id":"bad_args","output":"Tool arguments could not be parsed"}
        ]);
        let request = Request::parse(json!({"model":"m","input":history})).unwrap();
        validate_history(&request.input).unwrap();
        assert_eq!(request.input[0]["arguments"], "{broken");
        let mut invalid = history;
        invalid[0]["arguments"] = json!({});
        assert!(Request::parse(json!({"model":"m","input":invalid})).is_err());
    }
    #[test]
    fn code_mode_notifications_are_supplemental_and_identity_checked() {
        let call = json!({"type":"custom_tool_call","namespace":"functions","name":"exec","call_id":"c","input":"notify('progress')"});
        let note = json!({"type":"custom_tool_call_output","name":"exec","call_id":"c","output":"progress"});
        let done = json!({"type":"custom_tool_call_output","call_id":"c","output":"finished"});
        assert!(validate_history(&[call.clone(), note.clone()]).is_err());
        assert!(
            validate_history(&[
                call.clone(),
                note.clone(),
                note.clone(),
                done.clone(),
                note.clone()
            ])
            .is_ok()
        );
        assert!(validate_history(&[call.clone(), done.clone(), done]).is_err());
        let mut bad = note.clone();
        bad["name"] = json!("other");
        assert!(validate_history(&[call, bad]).is_err());
        assert!(validate_history(&[note]).is_err());
    }
    #[test]
    fn metadata_maps_only_public_turn_entries_and_cyber_is_explicit() {
        let request=Request::parse(json!({"model":"m","input":"hi","access_programs":{"cyber":"daybreak_blue"},"client_metadata":{"secret":"MUST_NOT_COPY","x-codex-turn-metadata":"{\"session_id\":\"spoof\",\"custom_tag\":\"ok\"}"},"max_output_tokens":5})).unwrap();
        let mut params = json!({});
        request.apply_turn(&mut params);
        assert_eq!(params["cyberAccessProgram"], "daybreakBlue");
        assert_eq!(
            params["responsesapiClientMetadata"],
            json!({"custom_tag":"ok"})
        );
        assert!(
            !serde_json::to_string(&request.parameter_handling)
                .unwrap()
                .contains("MUST_NOT_COPY")
        );
        assert!(
            request
                .parameter_handling
                .iter()
                .any(|n| n["path"] == "max_output_tokens" && n["action"] == "ignored")
        );
    }
    #[test]
    fn custom_history_requires_matching_call_type_and_all_results() {
        let call =
            json!({"type":"custom_tool_call","name":"exec","call_id":"c","input":"line\ntext"});
        let output = json!({"type":"custom_tool_call_output","call_id":"c","output":"real result"});
        assert!(validate_history(&[call.clone(), output.clone()]).is_ok());
        assert!(validate_history(std::slice::from_ref(&call)).is_err());
        assert!(
            validate_history(&[
                call.clone(),
                json!({"type":"function_call_output","call_id":"c","output":"wrong type"})
            ])
            .is_err()
        );
        assert!(validate_history(&[call.clone(), call, output]).is_err());
    }

    #[test]
    fn reasoning_history_accepts_official_content_variants() {
        let request = Request::parse(json!({
            "model": "m",
            "input": [{
                "type": "reasoning",
                "id": "r",
                "summary": [],
                "content": [
                    {"type": "reasoning_text", "text": "native reasoning"},
                    {"type": "text", "text": "provider text"}
                ]
            }]
        }))
        .unwrap();
        assert_eq!(request.input[0]["content"][0]["type"], "reasoning_text");
        assert_eq!(request.input[0]["content"][1]["text"], "provider text");
    }

    #[test]
    fn reasoning_history_rejects_malformed_content() {
        let error = match Request::parse(json!({
            "model": "m",
            "input": [{
                "type": "reasoning",
                "summary": [],
                "content": [{"type": "unknown", "text": "bad"}]
            }]
        })) {
            Ok(_) => panic!("malformed reasoning content must be rejected"),
            Err(error) => error,
        };
        assert!(error.message.contains("reasoning.content"));
    }

    #[test]
    fn tool_outputs_preserve_official_identity_fields() {
        let request = Request::parse(json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "call", "name": "exec", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call", "name": "exec", "namespace": "functions", "output": "done"}
            ]
        }))
        .unwrap();
        assert_eq!(request.input[1]["name"], "exec");
        assert_eq!(request.input[1]["namespace"], "functions");
    }
}
