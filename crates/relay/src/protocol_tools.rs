//! Shared dynamic tool validation. Namespaces are native protocol values.
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::collections::HashSet;
fn invalid() -> Error {
    Error::new(
        "INVALID_INPUT",
        "工具定义无效；支持 function、namespace，以及经过路由验收的 web_search/image_generation。",
    )
}
fn name(v: &Value, limit: usize) -> Result<&str> {
    v.as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= limit
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .ok_or_else(invalid)
}
fn keys(v: &Value, allowed: &[&str]) -> Result<()> {
    if !v
        .as_object()
        .is_some_and(|m| m.keys().all(|k| allowed.contains(&k.as_str())))
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn parse(value: &Value) -> Result<Vec<Value>> {
    if value.is_null() {
        return Ok(vec![]);
    }
    let tools = value.as_array().ok_or_else(invalid)?;
    let mut names = HashSet::new();
    let mut count = 0;
    let mut result = vec![];
    for tool in tools {
        match tool["type"].as_str() {
            Some("function") => {
                let t = function(tool)?;
                if !names.insert(t["name"].as_str().unwrap().to_owned()) {
                    return Err(invalid());
                }
                result.push(t);
                count += 1;
            }
            Some("namespace") => {
                keys(tool, &["type", "name", "description", "tools"])?;
                let ns = name(&tool["name"], 64)?;
                if !names.insert(ns.to_owned()) {
                    return Err(invalid());
                }
                let desc = tool["description"].as_str().ok_or_else(invalid)?;
                let mut nested = vec![];
                let mut members = HashSet::new();
                for item in tool["tools"]
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(invalid)?
                {
                    let t = function(item)?;
                    if !members.insert(t["name"].as_str().unwrap().to_owned()) {
                        return Err(invalid());
                    }
                    nested.push(t);
                    count += 1;
                }
                result
                    .push(json!({"type":"namespace","name":ns,"description":desc,"tools":nested}));
            }
            Some("web_search" | "image_generation") => {
                keys(tool, &["type"])?;
                let kind = tool["type"].as_str().unwrap();
                if !names.insert(format!("@{kind}")) {
                    return Err(invalid());
                }
                result.push(tool.clone());
                count += 1;
            }
            _ => return Err(invalid()),
        }
    }
    if count > crate::media::TOOL_COUNT {
        return Err(Error::new(
            "PAYLOAD_TOO_LARGE",
            "合并工具目录超过 1024 个工具。",
        ));
    }
    Ok(result)
}
fn function(tool: &Value) -> Result<Value> {
    keys(
        tool,
        &[
            "type",
            "name",
            "description",
            "parameters",
            "strict",
            "defer_loading",
        ],
    )?;
    if tool["type"] != "function" {
        return Err(invalid());
    }
    let n = name(&tool["name"], 128)?;
    if !tool["strict"].is_null() && tool["strict"] != false {
        return Err(Error::new(
            "INVALID_INPUT",
            "原生动态工具不暴露严格模式，请设置 strict=false。",
        ));
    }
    let description = if tool["description"].is_null() {
        ""
    } else {
        tool["description"].as_str().ok_or_else(invalid)?
    };
    let schema = if tool["parameters"].is_null() {
        json!({"type":"object","properties":{}})
    } else {
        tool["parameters"].clone()
    };
    if !schema.is_object() {
        return Err(invalid());
    }
    let mut result = json!({"type":"function","name":n,"description":description,"parameters":schema,"strict":false});
    if !tool["defer_loading"].is_null() {
        if !tool["defer_loading"].is_boolean() {
            return Err(invalid());
        }
        result["defer_loading"] = tool["defer_loading"].clone();
    }
    Ok(result)
}
pub fn allows(tools: &[Value], namespace: Option<&str>, name: &str) -> bool {
    tools.iter().any(|t| match namespace {
        Some(ns) => {
            t["type"] == "namespace"
                && t["name"] == ns
                && t["tools"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|f| f["name"] == name))
        }
        None => t["type"] == "function" && t["name"] == name,
    })
}
pub fn native(tools: &[Value]) -> Vec<Value> {
    tools.iter().filter_map(|t|match t["type"].as_str(){
        Some("function")=>{let mut value=json!({"type":"function","name":t["name"],"description":t["description"],"inputSchema":t["parameters"]});if let Some(defer)=t["defer_loading"].as_bool(){value["deferLoading"]=json!(defer);}Some(value)},
        Some("namespace")=>Some(json!({"type":"namespace","name":t["name"],"description":t["description"],"tools":native(t["tools"].as_array().unwrap())})),_=>None,
    }).collect()
}
pub fn from_native(tools: &[Value]) -> Result<Vec<Value>> {
    let converted:Result<Vec<_>>=tools.iter().map(|t|{
        if t["type"]=="namespace"{keys(t,&["type","name","description","tools"])?;Ok(json!({"type":"namespace","name":t["name"],"description":t["description"],"tools":from_native(t["tools"].as_array().ok_or_else(invalid)?)?}))}
        else {keys(t,&["type","name","description","inputSchema","deferLoading"])?;Ok(json!({"type":t["type"],"name":t["name"],"description":t["description"],"parameters":t["inputSchema"],"defer_loading":t["deferLoading"]}))}
    }).collect();
    parse(&json!(converted?))
}
