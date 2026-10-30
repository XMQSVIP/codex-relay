//! Responses declarations stay in history; the current catalog registers native callbacks.
//! app-server dynamic tools accept JSON functions, so freeform tools use an input string.
use super::invalid;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub(super) const SEARCH_NAME: &str = "__relay_tool_search";
const DEFERRED_NAMESPACE: &str = "relay_deferred";

/// Diagnose identities without echoing tool arguments, code, or arbitrary model text.
pub(super) fn scope_error(reason: &str, namespace: Option<&str>, name: &str) -> Error {
    fn identifier(value: &str, limit: usize) -> &str {
        if !value.is_empty()
            && value.len() <= limit
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            value
        } else {
            "<invalid>"
        }
    }
    let namespace = namespace.map_or("<none>", |ns| identifier(ns, 64));
    let name = identifier(name, 128);
    Error::new(
        "NATIVE_TOOL_SCOPE",
        &format!("{reason}（namespace={namespace}, name={name}）"),
    )
}

#[derive(Clone)]
struct Identity {
    namespace: Option<String>,
    name: String,
    native_namespace: Option<String>,
    native_name: String,
}
fn identities(tools: &[Value]) -> Vec<Identity> {
    let mut result = vec![];
    for tool in tools {
        let (namespace, members) = if tool["type"] == "namespace" {
            (
                tool["name"].as_str(),
                tool["tools"].as_array().unwrap().as_slice(),
            )
        } else {
            (None, std::slice::from_ref(tool))
        };
        for member in members {
            let name = if member["type"] == "tool_search" {
                SEARCH_NAME
            } else if let Some(name) = member["name"].as_str() {
                name
            } else {
                continue;
            };
            result.push(Identity {
                namespace: namespace.map(str::to_owned),
                name: name.into(),
                native_namespace: namespace.map(|ns| native_name(ns, true)).or_else(|| {
                    (member["defer_loading"] == true).then(|| DEFERRED_NAMESPACE.into())
                }),
                native_name: native_name(name, false),
            });
        }
    }
    result
}

// app-server reserves these namespaces and the mcp prefix. Escape our own prefix
// too, so a caller cannot collide with an escaped name. Names are stable across turns.
fn native_name(name: &str, namespace: bool) -> String {
    let reserved = name == "mcp"
        || name.starts_with("mcp__")
        || name.starts_with("relay_")
        || (namespace
            && [
                "api_tool",
                "browser",
                "computer",
                "container",
                "file_search",
                "functions",
                "image_gen",
                "multi_tool_use",
                "python",
                "python_user_visible",
                "submodel_delegator",
                "terminal",
                "tool_search",
                "web",
                "clock",
                "collaboration",
            ]
            .contains(&name));
    if reserved {
        format!(
            "relay_{}",
            &hex::encode(Sha256::digest(name.as_bytes()))[..48]
        )
    } else {
        name.to_owned()
    }
}

fn native_declarations(tools: &[Value]) -> Vec<Value> {
    let mut result = vec![];
    merge(&mut result, declarations(tools, true));
    result
}
fn declarations(tools: &[Value], top_level: bool) -> Vec<Value> {
    functions(tools)
        .into_iter()
        .map(|mut tool| {
            if let Some(name) = tool["name"].as_str() {
                tool["name"] = json!(native_name(name, tool["type"] == "namespace"));
            }
            if tool["type"] == "namespace" {
                tool["tools"] = declarations(tool["tools"].as_array().unwrap(), false).into();
            } else if top_level && tool["defer_loading"]==true {
                // One namespace entry per declaration preserves declaration order;
                // merged registration below combines members of this namespace.
                return json!({"type":"namespace","name":DEFERRED_NAMESPACE,"description":"Deferred caller tools","tools":[tool]});
            }
            tool
        })
        .collect()
}

pub(super) fn native(tools: &[Value]) -> Vec<Value> {
    let mut catalog = vec![];
    merge(&mut catalog, native_declarations(tools));
    crate::protocol_tools::native(&catalog)
}

pub(super) fn client_identity(
    tools: &[Value],
    namespace: Option<&str>,
    name: &str,
) -> Option<(Option<String>, String)> {
    identities(tools)
        .into_iter()
        .find(|entry| {
            entry.native_name == name
                && match entry.native_namespace.as_deref() {
                    Some(ns) => namespace == Some(ns),
                    None => matches!(namespace, None | Some("functions")),
                }
        })
        .map(|entry| (entry.namespace, entry.name))
}

/// Instructions can retain caller names even though dynamic registration needs
/// aliases. Raw model output may use either spelling; callbacks still require
/// the registered native identity. Prefer native aliases to preserve their mapping.
pub(super) fn output_identity(
    tools: &[Value],
    namespace: Option<&str>,
    name: &str,
) -> Option<(Option<String>, String)> {
    client_identity(tools, namespace, name).or_else(|| {
        let map = identities(tools);
        original_identity(&map, namespace, name)
            .or_else(|| {
                // Raw model calls can omit the default `functions` namespace,
                // including desktop custom calls such as exec. Only restore
                // that declared default after exact identities have failed;
                // other namespaces and native callbacks remain strict.
                if namespace.is_none() {
                    original_identity(&map, Some("functions"), name)
                } else {
                    None
                }
            })
            .map(|entry| (entry.namespace.clone(), entry.name.clone()))
    })
}

fn original_identity<'a>(
    map: &'a [Identity],
    namespace: Option<&str>,
    name: &str,
) -> Option<&'a Identity> {
    map.iter()
        .find(|entry| entry.name == name && entry.namespace.as_deref() == namespace)
        .or_else(|| {
            // The native Responses protocol also spells bare tools as functions.name.
            (namespace == Some("functions"))
                .then(|| {
                    map.iter()
                        .find(|entry| entry.name == name && entry.namespace.is_none())
                })
                .flatten()
        })
}

pub(super) fn parse(value: &Value) -> Result<Vec<Value>> {
    let mut value = value.clone();
    super::compatibility::tools(&mut value)?;
    if value.is_null() {
        return Ok(vec![]);
    }
    let tools = value
        .as_array()
        .ok_or_else(|| invalid("tools 必须是数组。"))?;
    validate_custom(tools)?;
    let normalized = crate::protocol_tools::parse(&json!(functions(tools)))?;
    Ok(restore_custom(normalized, tools))
}

fn validate_custom(tools: &[Value]) -> Result<()> {
    for tool in tools {
        match tool["type"].as_str() {
            Some("web_search") => super::search::validate(tool)?,
            Some("tool_search") => {
                if tool["execution"] != "client" {
                    return Err(invalid(
                        "当前原生适配支持 execution=client 的 tool_search；服务端工具搜索尚未开放。",
                    ));
                }
                if !tool["parameters"].is_null() && !tool["parameters"].is_object() {
                    return Err(invalid("tool_search.parameters 必须是对象。"));
                }
            }
            Some("namespace") => {
                if let Some(members) = tool["tools"].as_array() {
                    validate_custom(members)?;
                }
            }
            Some("custom") => {
                let format = &tool["format"];
                if format.is_null() {
                    continue;
                }
                let valid = match format["type"].as_str() {
                    Some("text") => format.as_object().is_some_and(|m| m.len() == 1),
                    Some("grammar") => {
                        format.as_object().is_some_and(|m| m.len() == 3)
                            && matches!(format["syntax"].as_str(), Some("lark" | "regex"))
                            && format["definition"].as_str().is_some_and(|s| !s.is_empty())
                    }
                    _ => false,
                };
                if !valid {
                    return Err(invalid(
                        "custom.format 必须是 text 或有效的 lark/regex grammar。",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn restore_custom(normalized: Vec<Value>, original: &[Value]) -> Vec<Value> {
    normalized
        .into_iter()
        .zip(original)
        .map(|(mut tool, original)| {
            if original["type"] == "web_search" {
                tool = original.clone();
            } else if original["type"] == "custom" || original["type"] == "tool_search" {
                tool = original.clone();
                if tool["description"].is_null() {
                    tool["description"] = json!("");
                }
            } else if tool["type"] == "namespace" {
                tool["tools"] = restore_custom(
                    tool["tools"].as_array().unwrap().clone(),
                    original["tools"].as_array().unwrap(),
                )
                .into();
            }
            tool
        })
        .collect()
}

pub(super) fn functions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            if tool["type"] == "web_search" {
                json!({"type":"web_search"})
            } else if tool["type"]=="tool_search" {
                json!({"type":"function","name":SEARCH_NAME,"description":tool["description"],"strict":false,
                    "parameters":if tool["parameters"].is_null(){json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})}else{tool["parameters"].clone()}})
            } else if tool["type"] == "namespace" {
                let mut tool = tool.clone();
                if let Some(members) = tool["tools"].as_array() {
                    tool["tools"] = functions(members).into();
                }
                tool
            } else if tool["type"] == "custom" {
                let mut description = tool["description"].as_str().unwrap_or("").to_owned();
                description.push_str(
                    "\nPass the complete freeform tool input verbatim in the input string.",
                );
                if tool["format"]["type"] == "grammar" {
                    description.push_str(&format!(
                        "\nInput grammar ({}):\n{}",
                        tool["format"]["syntax"].as_str().unwrap_or(""),
                        tool["format"]["definition"].as_str().unwrap_or("")
                    ));
                }
                // Keep invalid names/descriptions visible to the shared validator.
                let description =
                    if !tool["description"].is_null() && !tool["description"].is_string() {
                        tool["description"].clone()
                    } else {
                        json!(description)
                    };
                let mut value=json!({"type":"function","name":tool["name"],"description":description,
                "parameters":{"type":"object","properties":{"input":{"type":"string"}},
                    "required":["input"],"additionalProperties":false},"strict":false});
                if !tool["defer_loading"].is_null(){value["defer_loading"]=tool["defer_loading"].clone();}
                value
            } else {
                tool.clone()
            }
        })
        .collect()
}

/// Later declarations update only the named namespace members, not its other tools.
pub(super) fn resolve(top_level: &[Value], history: &[Value]) -> Result<Vec<Value>> {
    let mut tools = top_level.to_vec();
    for item in history {
        if item["type"] == "additional_tools" || item["type"] == "tool_search_output" {
            merge(&mut tools, parse(&item["tools"])?);
        }
    }
    // Enforce the combined count and schema constraints, not just each individual batch.
    parse(&json!(tools))
}

fn merge(tools: &mut Vec<Value>, incoming: Vec<Value>) {
    for tool in incoming {
        let key = |t: &Value| {
            t["name"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("@{}", t["type"].as_str().unwrap_or("")))
        };
        if let Some(old) = tools.iter_mut().find(|old| key(old) == key(&tool)) {
            if old["type"] == "namespace" && tool["type"] == "namespace" {
                merge(
                    old["tools"].as_array_mut().unwrap(),
                    tool["tools"].as_array().unwrap().clone(),
                );
                old["description"] = tool["description"].clone();
            } else {
                *old = tool;
            }
        } else {
            tools.push(tool);
        }
    }
}

pub(super) fn is_custom(tools: &[Value], namespace: Option<&str>, name: &str) -> bool {
    tools.iter().any(|tool| match namespace {
        Some(ns) => {
            tool["type"] == "namespace"
                && tool["name"] == ns
                && tool["tools"]
                    .as_array()
                    .is_some_and(|tools| is_custom(tools, None, name))
        }
        None => tool["type"] == "custom" && tool["name"] == name,
    })
}

#[cfg(test)]
pub(super) fn native_history(history: &[Value], disabled: bool) -> Vec<Value> {
    native_history_with_catalog(
        history,
        disabled,
        &resolve(&[], history).unwrap_or_default(),
    )
}
pub(super) fn native_history_with_catalog(
    history: &[Value],
    disabled: bool,
    tools: &[Value],
) -> Vec<Value> {
    let map = identities(tools);
    let mut calls = std::collections::HashMap::new();
    history
        .iter()
        .cloned()
        .flat_map(|mut item| {
            let original_type = item["type"].clone();
            let original_call = matches!(
                item["type"].as_str(),
                Some("function_call" | "custom_tool_call")
            );
            let identity = if original_call {
                let found = original_identity(
                    &map,
                    item["namespace"].as_str(),
                    item["name"].as_str().unwrap_or(""),
                )
                .cloned();
                if let (Some(id), Some(entry)) = (item["call_id"].as_str(), found.clone()) {
                    calls.insert(id.to_owned(), entry);
                }
                found
            } else {
                item["call_id"]
                    .as_str()
                    .and_then(|id| calls.get(id))
                    .cloned()
            };
            let mut loaded = None;
            match item["type"].as_str() {
                Some("compaction_trigger") => return vec![],
                Some("tool_search_call") if item["execution"] == "client" => {
                    item["type"] = json!("function_call");
                    item["name"] = json!(SEARCH_NAME);
                    item["arguments"] = json!(item["arguments"].to_string());
                    item.as_object_mut().unwrap().remove("execution");
                }
                Some("tool_search_output") if item["execution"] == "client" => {
                    let original = item.as_object_mut().unwrap().remove("tools").unwrap();
                    let tools = json!(if disabled {
                        vec![]
                    } else {
                        native_declarations(original.as_array().unwrap())
                    });
                    loaded =
                        Some(json!({"type":"additional_tools","role":"developer","tools":tools}));
                    item["type"] = json!("function_call_output");
                    item["output"] = json!(tools.to_string());
                    item.as_object_mut().unwrap().remove("execution");
                }
                Some("additional_tools" | "tool_search_output") => {
                    item["tools"] = if disabled {
                        json!([])
                    } else {
                        json!(native_declarations(item["tools"].as_array().unwrap()))
                    };
                }
                Some("custom_tool_call") => {
                    item["type"] = json!("function_call");
                    item["arguments"] = json!(json!({"input":item["input"]}).to_string());
                    item.as_object_mut().unwrap().remove("input");
                }
                Some("custom_tool_call_output") => item["type"] = json!("function_call_output"),
                _ => {}
            }
            if item["type"] != original_type {
                // Item IDs are typed (ctc/ctco, etc.). They cannot be replayed
                // as function IDs. These IDs are optional in injected history;
                // call_id, which pairs calls/results/notifications, stays intact.
                item.as_object_mut().unwrap().remove("id");
            }
            if item["type"] == "function_call"
                || (item["type"] == "function_call_output"
                    && (item["name"].is_string() || item["namespace"].is_string()))
            {
                if let Some(entry) = identity {
                    // A nameless final result must remain nameless: code-mode
                    // uses the presence of name to distinguish notify updates.
                    if item["type"] == "function_call" || item["name"].is_string() {
                        item["name"] = json!(entry.native_name);
                    }
                    if let Some(ns) = entry.native_namespace {
                        item["namespace"] = json!(ns);
                    } else {
                        item.as_object_mut().unwrap().remove("namespace");
                    }
                } else {
                    if let Some(ns) = item["namespace"].as_str() {
                        item["namespace"] = json!(native_name(ns, true));
                    }
                    if let Some(name) = item["name"].as_str() {
                        item["name"] = json!(native_name(name, false));
                    }
                }
            }
            let mut result = vec![item];
            result.extend(loaded);
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converted_tool_history_drops_typed_item_ids_but_preserves_call_links() {
        let tools = parse(&json!([
            {"type":"namespace","name":"functions","description":"Desktop","tools":[{"type":"custom","name":"exec"}]},
            {"type":"tool_search","execution":"client"}
        ])).unwrap();
        let history = vec![
            json!({"type":"custom_tool_call","id":"ctc_exec","call_id":"exec_call","namespace":"functions","name":"exec","input":"text('hello');"}),
            json!({"type":"custom_tool_call_output","id":"ctco_progress","call_id":"exec_call","name":"exec","output":"progress"}),
            json!({"type":"custom_tool_call_output","id":"ctco_01a17cae-5132-78f1-bf02-38b2cb758c0d","call_id":"exec_call","output":[{"type":"input_text","text":"done"}]}),
            json!({"type":"tool_search_call","id":"tsc_search","call_id":"search_call","execution":"client","arguments":{"query":"tools"}}),
            json!({"type":"tool_search_output","id":"tso_search","call_id":"search_call","execution":"client","tools":[{"type":"function","name":"echo"}]}),
            json!({"type":"function_call","id":"fc_echo","call_id":"echo_call","name":"echo","arguments":"{}"}),
            json!({"type":"function_call_output","id":"fco_echo","call_id":"echo_call","output":"ok"}),
            json!({"type":"message","id":"msg_reply","role":"assistant","content":[{"type":"output_text","text":"done"}]}),
        ];
        let original = history.clone();
        for disabled in [false, true] {
            let native = native_history_with_catalog(&history, disabled, &tools);
            for (index, kind) in [
                "function_call",
                "function_call_output",
                "function_call_output",
                "function_call",
                "function_call_output",
            ]
            .iter()
            .enumerate()
            {
                assert_eq!(native[index]["type"], *kind);
                assert!(native[index].get("id").is_none());
                assert_eq!(native[index]["call_id"], history[index]["call_id"]);
            }
            assert_eq!(native[1]["name"], "exec");
            assert_eq!(native[1]["output"], history[1]["output"]);
            assert!(native[2].get("name").is_none());
            assert_eq!(native[2]["output"], history[2]["output"]);
            assert_eq!(native[5]["type"], "additional_tools");
            for (converted, original) in native[6..].iter().zip(&history[5..]) {
                assert_eq!(converted, original);
            }
        }
        assert_eq!(history, original, "caller history must remain unchanged");
    }

    #[test]
    fn omitted_default_namespace_does_not_relax_callback_or_other_namespace_scope() {
        let tools = parse(&json!([
            {"type":"namespace","name":"functions","description":"Desktop","tools":[{"type":"custom","name":"exec"}]},
            {"type":"namespace","name":"client","description":"Other","tools":[{"type":"function","name":"echo"}]}
        ])).unwrap();
        assert_eq!(
            output_identity(&tools, None, "exec"),
            Some((Some("functions".into()), "exec".into()))
        );
        assert!(client_identity(&tools, None, "exec").is_none());
        assert!(client_identity(&tools, Some("functions"), "exec").is_none());
        assert!(output_identity(&tools, Some("client"), "exec").is_none());
        assert!(output_identity(&tools, Some("unknown"), "exec").is_none());
        assert!(output_identity(&tools, None, "echo").is_none());
        assert!(output_identity(&tools, None, "missing").is_none());

        // An explicitly registered bare tool wins over the default namespace.
        let mut collisions = tools;
        collisions.push(json!({"type":"function","name":"exec"}));
        assert_eq!(
            output_identity(&collisions, None, "exec"),
            Some((None, "exec".into()))
        );
        assert_eq!(
            output_identity(&collisions, Some("functions"), "exec"),
            Some((None, "exec".into()))
        );
        let native = native(&collisions);
        assert_eq!(
            output_identity(&collisions, native[0]["name"].as_str(), "exec"),
            Some((Some("functions".into()), "exec".into()))
        );
    }

    #[test]
    fn scope_diagnostics_include_only_bounded_identifiers() {
        let error = scope_error("unregistered", Some("functions"), "exec");
        assert_eq!(error.code, "NATIVE_TOOL_SCOPE");
        assert!(error.message.contains("namespace=functions, name=exec"));
        for value in ["secret\ncode".to_owned(), "x".repeat(129)] {
            let error = scope_error("unregistered", Some(&value), &value);
            assert!(!error.message.contains(&value));
            assert!(
                error
                    .message
                    .contains("namespace=<invalid>, name=<invalid>")
            );
        }
    }
    #[test]
    fn model_output_can_use_declared_original_function_names() {
        let tools = parse(&json!([
            {"type":"namespace","name":"functions","description":"Client system tools","tools":[{"type":"function","name":"exec_command"}]},
            {"type":"function","name":"mcp__system__info"},
            {"type":"function","name":"deferred","defer_loading":true}
        ])).unwrap();
        for (namespace, name) in [
            (Some("functions"), "exec_command"),
            (None, "mcp__system__info"),
            (None, "deferred"),
        ] {
            assert_eq!(
                output_identity(&tools, namespace, name),
                Some((namespace.map(str::to_owned), name.into()))
            );
            assert!(
                client_identity(&tools, namespace, name).is_none(),
                "RPC callbacks must still use the registered native spelling"
            );
        }
        assert!(output_identity(&tools, Some("other"), "exec_command").is_none());
        assert!(output_identity(&tools, Some("functions"), "unknown").is_none());
    }
    #[test]
    fn tool_identity_matrix_roundtrips_declarations_calls_and_results() {
        // Both public spellings of bare tools, namespace aliases, MCP names,
        // deferred registration and custom envelopes must share one identity.
        for custom in [false, true] {
            for namespace in [None, Some("functions"), Some("client"), Some("mcp__server")] {
                for name in ["echo", "mcp__echo", "relay_echo"] {
                    for deferred in [false, true] {
                        let mut member = json!({"type":if custom {"custom"} else {"function"},"name":name,"defer_loading":deferred});
                        if !custom {
                            member["parameters"] = json!({"type":"object"});
                        }
                        let tool = if let Some(ns) = namespace {
                            json!({"type":"namespace","name":ns,"description":"Matrix","tools":[member]})
                        } else {
                            member
                        };
                        let tools = parse(&json!([tool])).unwrap();
                        let identities = identities(&tools);
                        let entry = &identities[0];
                        let expected = Some((namespace.map(str::to_owned), name.to_owned()));
                        assert_eq!(output_identity(&tools, namespace, name), expected);
                        assert_eq!(
                            output_identity(
                                &tools,
                                entry.native_namespace.as_deref(),
                                &entry.native_name
                            ),
                            expected
                        );
                        for explicit_default in [false, true] {
                            let call_namespace =
                                namespace.or(explicit_default.then_some("functions"));
                            assert_eq!(output_identity(&tools, call_namespace, name), expected);
                            for result_form in 0..3 {
                                let mut call = json!({"type":if custom {"custom_tool_call"} else {"function_call"},"call_id":"c","name":name});
                                call[if custom { "input" } else { "arguments" }] =
                                    json!(if custom { "raw code" } else { "{}" });
                                if let Some(ns) = call_namespace {
                                    call["namespace"] = json!(ns);
                                }
                                let mut result = json!({"type":if custom {"custom_tool_call_output"} else {"function_call_output"},"call_id":"c","output":"client result"});
                                if result_form == 0 {
                                    result["name"] = json!(name);
                                }
                                if result_form <= 1 {
                                    result["namespace"] =
                                        json!(call_namespace.unwrap_or("functions"));
                                }
                                let native =
                                    native_history_with_catalog(&[call, result], false, &tools);
                                assert_eq!(native[0]["name"], entry.native_name);
                                assert_eq!(
                                    native[0]["namespace"].as_str(),
                                    entry.native_namespace.as_deref()
                                );
                                assert_eq!(native[1]["name"].is_string(), result_form == 0);
                                if result_form == 0 {
                                    assert_eq!(native[1]["name"], entry.native_name);
                                }
                                if result_form <= 1 {
                                    assert_eq!(
                                        native[1]["namespace"].as_str(),
                                        entry.native_namespace.as_deref()
                                    );
                                } else {
                                    assert!(native[1].get("namespace").is_none());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn searched_tools_use_the_same_identity_in_results_and_declarations() {
        let tools = parse(&json!([
            {"type":"namespace","name":"functions","description":"Code tools","tools":[{"type":"custom","name":"exec"}]},
            {"type":"function","name":"deferred","defer_loading":true}
        ]))
        .unwrap();
        let output = json!({"type":"tool_search_output","execution":"client","call_id":"search","tools":tools});
        let native = native_history_with_catalog(&[output], false, &tools);
        let returned: Value = serde_json::from_str(native[0]["output"].as_str().unwrap()).unwrap();
        assert_eq!(returned, native[1]["tools"]);
        assert_eq!(
            client_identity(&tools, returned[0]["name"].as_str(), "exec"),
            Some((Some("functions".into()), "exec".into()))
        );
        assert_eq!(
            client_identity(&tools, returned[1]["name"].as_str(), "deferred"),
            Some((None, "deferred".into()))
        );
    }
    #[test]
    fn reserved_names_are_reversible_and_cannot_collide_with_client_aliases() {
        let escaped = native_name("functions", true);
        let tools = parse(&json!([
            {"type":"function","name":"echo"},
            {"type":"namespace","name":"functions","description":"desktop","tools":[{"type":"custom","name":"echo"}]},
            {"type":"namespace","name":escaped,"description":"client alias","tools":[{"type":"function","name":"echo"}]},
            {"type":"namespace","name":"mcp__server","description":"MCP","tools":[{"type":"function","name":"mcp__echo"}]}
        ])).unwrap();
        let native = native(&tools);
        assert_ne!(native[1]["name"], native[2]["name"]);
        assert_eq!(
            client_identity(&tools, Some("functions"), "echo"),
            Some((None, "echo".into()))
        );
        assert_eq!(
            client_identity(&tools, native[1]["name"].as_str(), "echo"),
            Some((Some("functions".into()), "echo".into()))
        );
        assert_eq!(
            client_identity(&tools, native[2]["name"].as_str(), "echo"),
            Some((Some(escaped), "echo".into()))
        );
        assert_eq!(
            client_identity(
                &tools,
                native[3]["name"].as_str(),
                native[3]["tools"][0]["name"].as_str().unwrap()
            ),
            Some((Some("mcp__server".into()), "mcp__echo".into()))
        );
        assert!(client_identity(&tools, Some("other"), "echo").is_none());
    }
    #[test]
    fn native_history_preserves_freeform_bytes_and_disabled_declaration_positions() {
        let raw = "line 1\n\"quoted\" \\ 中文";
        let history = vec![
            json!({"type":"message","role":"user","content":"before"}),
            json!({"type":"additional_tools","role":"developer","tools":[{"type":"custom","name":"exec"}]}),
            json!({"type":"custom_tool_call","name":"exec","call_id":"c","input":raw}),
            json!({"type":"custom_tool_call_output","call_id":"c","output":"client result"}),
        ];
        let native = native_history(&history, false);
        assert_eq!(native.len(), history.len());
        assert_eq!(native[1]["tools"][0]["type"], "function");
        let arguments: Value =
            serde_json::from_str(native[2]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(arguments["input"], raw);
        assert_eq!(native[3]["type"], "function_call_output");
        assert_eq!(native[3]["output"], "client result");
        assert_eq!(native_history(&history, true)[1]["tools"], json!([]));
        assert_eq!(history[2]["type"], "custom_tool_call");
    }
}
