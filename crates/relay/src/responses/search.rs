//! Per-thread search controls; never mutate the account's native configuration.
use super::invalid;
use crate::error::Result;
use serde_json::{Value, json};

pub(super) const OPTIONS: &[&str] = &[
    "search_context_size",
    "external_web_access",
    "filters",
    "user_location",
];

pub(super) fn validate(tool: &Value) -> Result<()> {
    if !tool["search_context_size"].is_null()
        && !matches!(
            tool["search_context_size"].as_str(),
            Some("low" | "medium" | "high")
        )
    {
        return Err(invalid(
            "web_search.search_context_size 仅支持 low/medium/high。",
        ));
    }
    if !tool["external_web_access"].is_null() && !tool["external_web_access"].is_boolean() {
        return Err(invalid("web_search.external_web_access 必须是布尔值。"));
    }
    let domains = &tool["filters"]["allowed_domains"];
    if !domains.is_null()
        && !domains.as_array().is_some_and(|items| {
            items.len() <= 100
                && items.iter().all(|item| {
                    item.as_str().is_some_and(|s| {
                        !s.is_empty()
                            && s.len() <= 253
                            && !s.chars().any(|c| c.is_whitespace() || "/:@?#".contains(c))
                    })
                })
        })
    {
        return Err(invalid(
            "web_search.filters.allowed_domains 必须是最多 100 个不含协议或路径的域名。",
        ));
    }
    let location = &tool["user_location"];
    if !location.is_null() {
        if location["type"] != "approximate" {
            return Err(invalid(
                "web_search.user_location.type 必须是 approximate。",
            ));
        }
        for field in ["country", "region", "city", "timezone"] {
            if !location[field].is_null()
                && !location[field].as_str().is_some_and(|s| s.len() <= 256)
            {
                return Err(invalid(
                    "web_search.user_location 字段须为不超过 256 字节的字符串。",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn apply(tool: &Value, params: &mut Value) {
    let mut location = tool["user_location"].clone();
    if let Some(object) = location.as_object_mut() {
        object.remove("type");
        object.retain(|_, value| !value.is_null());
    } else {
        location = json!({});
    }
    // Explicit defaults prevent local account search preferences leaking into
    // independent API requests. Account location alignment still runs afterwards.
    params["config"]["web_search"] = json!(if tool["external_web_access"] == false {
        "cached"
    } else {
        "live"
    });
    params["config"]["tools.web_search.context_size"] =
        json!(tool["search_context_size"].as_str().unwrap_or("medium"));
    params["config"]["tools.web_search.allowed_domains"] = tool["filters"]["allowed_domains"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into();
    params["config"]["tools.web_search.location"] = location;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::responses::{Request, reuse::Session};

    fn tool() -> Value {
        json!({"type":"web_search","search_context_size":"high","external_web_access":false,
            "filters":{"allowed_domains":["example.com"]},"user_location":{"type":"approximate","country":"US","city":"Boston","timezone":"America/New_York"}})
    }

    #[test]
    fn options_survive_history_and_map_to_native_search_configuration() {
        for embedded in [false, true] {
            let body = if embedded {
                json!({"model":"m","input":[{"type":"additional_tools","role":"developer","tools":[tool()]},{"role":"user","content":"Search"}]})
            } else {
                json!({"model":"m","input":"Search","tools":[tool()]})
            };
            let r = Request::parse(body).unwrap();
            let r = r.with_history(&r.input).unwrap();
            assert_eq!(r.tools, vec![tool()]);
            assert!(r.dynamic_tools().is_empty());
            let mut params = json!({"config":{}});
            r.apply_thread(&mut params);
            assert_eq!(params["config"]["web_search"], "cached");
            assert_eq!(params["config"]["tools.web_search.context_size"], "high");
            assert_eq!(
                params["config"]["tools.web_search.allowed_domains"],
                json!(["example.com"])
            );
            assert_eq!(
                params["config"]["tools.web_search.location"]["city"],
                "Boston"
            );
            assert!(
                params["config"]["tools.web_search.location"]
                    .get("type")
                    .is_none()
            );
            assert!(
                r.parameter_handling
                    .iter()
                    .any(|note| note["action"] == "adapted"
                        && note["path"].as_str().unwrap().ends_with(".filters"))
            );
        }
        let mut params = json!({"config":{}});
        apply(&json!({"type":"web_search"}), &mut params);
        assert_eq!(params["config"]["web_search"], "live");
        assert_eq!(params["config"]["tools.web_search.context_size"], "medium");
        assert_eq!(
            params["config"]["tools.web_search.allowed_domains"],
            json!([])
        );
        assert_eq!(params["config"]["tools.web_search.location"], json!({}));
        let disabled =
            Request::parse(json!({"model":"m","input":"x","tools":[tool()],"tool_choice":"none"}))
                .unwrap();
        let mut params = json!({"config":{"web_search":"disabled"}});
        disabled.apply_thread(&mut params);
        assert_eq!(params["config"]["web_search"], "disabled");
    }

    #[test]
    fn malformed_controls_fail_instead_of_silently_enabling_live_access() {
        for (key, value) in [
            ("external_web_access", json!("false")),
            ("search_context_size", json!("huge")),
            ("filters", json!([])),
            (
                "filters",
                json!({"allowed_domains":["https://example.com"]}),
            ),
            ("filters", json!({"allowed_domains":[42]})),
            ("user_location", json!({"type":"exact"})),
            ("user_location", json!({"type":"approximate","city":42})),
        ] {
            let mut tool = tool();
            tool[key] = value;
            assert!(
                Request::parse(json!({"model":"m","input":"search","tools":[tool]})).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn changing_search_controls_cannot_reuse_an_old_thread() {
        if !crate::native_cache::enabled() {
            return;
        }
        let access = crate::operations::ClientAccess {
            fast_mode: false,
            key_id: uuid::Uuid::new_v4(),
            group_id: uuid::Uuid::new_v4(),
            models: vec![],
        };
        let first = Request::parse(json!({"model":"m","input":"search","tools":[tool()]})).unwrap();
        let answer = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"result"}]});
        let session = Session::new(
            "thread".into(),
            "test".into(),
            &access,
            &first,
            &first.input,
            std::slice::from_ref(&answer),
        )
        .unwrap();
        let mut history = first.input.clone();
        history.extend([answer, json!({"role":"user","content":"next"})]);
        for (field, value) in [
            ("external_web_access", json!(true)),
            ("filters", json!({"allowed_domains":["other.test"]})),
            ("search_context_size", json!("low")),
            (
                "user_location",
                json!({"type":"approximate","city":"Tokyo"}),
            ),
        ] {
            let mut changed = tool();
            changed[field] = value;
            let next =
                Request::parse(json!({"model":"m","input":history,"tools":[changed]})).unwrap();
            assert!(
                session
                    .continuation(&access, &next, &next.input)
                    .unwrap()
                    .is_none(),
                "{field}"
            );
        }
        let same = Request::parse(json!({"model":"m","input":history,"tools":[tool()]})).unwrap();
        assert!(
            session
                .continuation(&access, &same, &same.input)
                .unwrap()
                .is_some()
        );
    }
}
