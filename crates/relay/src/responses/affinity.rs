//! Stable local routing digests. Native thread reuse has separate, stricter validation.
use super::Request;
use serde_json::Value;
pub(crate) mod identity;

pub(crate) fn apply(request: &mut Request, headers: identity::Headers) {
    let identity = request.identity.resolve(headers);
    request.client_session = identity.own;
    request.reuse_allowed = identity.reusable;
}

pub(crate) fn parent(
    context: &crate::execution::RequestContext,
    request: &Request,
) -> Option<[u8; 32]> {
    if !crate::native_cache::enabled() || request.compact {
        return None;
    }
    let parent = request.identity.resolve(context.identity_headers).parent?;
    crate::profiles::warm::digest(&("native-session-v1", parent)).ok()
}

/// A routing hint, never authorization to reuse a native conversation.
pub(crate) fn routing(
    context: &crate::execution::RequestContext,
    request: &Request,
    history: &[Value],
) -> Option<[u8; 32]> {
    if !crate::native_cache::enabled() {
        return continuation(request, history);
    }
    if request.compact {
        return None;
    }
    let explicit = request.identity.resolve(context.identity_headers).own;
    if let Some(explicit) = explicit {
        return crate::profiles::warm::digest(&("native-session-v1", explicit)).ok();
    }
    if let Some(hint) = request.routing_cache_key {
        // A cache hint cannot masquerade as a parent's explicit identity.
        return crate::profiles::warm::digest(&("native-cache-hint-v1", hint)).ok();
    }
    let first = history
        .iter()
        .position(|item| item["type"] == "message" && item["role"] == "user")?;
    crate::profiles::warm::digest(&serde_json::json!({
        "kind":"native-prefix-v1", "settings":super::reuse::thread_settings(request).ok()?,
        "prefix":&history[..=first],
    }))
    .ok()
}

fn digest(request: &Request, history: &[Value]) -> Option<[u8; 32]> {
    if request.compact || !request.tools.is_empty() {
        return None;
    }
    let mut messages = Vec::with_capacity(history.len());
    let mut assistant = false;
    for item in history {
        if item["type"] != "message" {
            return None;
        }
        let role = item["role"].as_str()?;
        if !matches!(role, "user" | "assistant" | "system" | "developer") {
            return None;
        }
        let mut text = String::new();
        for part in item["content"].as_array()? {
            if !matches!(part["type"].as_str(), Some("input_text" | "output_text")) {
                return None;
            }
            text.push_str(part["text"].as_str()?);
        }
        assistant |= role == "assistant" && !text.is_empty();
        messages.push((role, text));
    }
    if !assistant {
        return None;
    }
    crate::profiles::warm::digest(&(super::reuse::settings(request).ok()?, messages)).ok()
}

pub(super) fn continuation(request: &Request, history: &[Value]) -> Option<[u8; 32]> {
    let end = history
        .iter()
        .rposition(|m| m["type"] != "message" || m["role"] != "user")?
        + 1;
    if end == history.len() {
        return None;
    }
    // Validate the appended messages too: images/tools cannot create text affinity.
    digest(request, history)?;
    digest(request, &history[..end])
}

pub(super) fn completed(
    request: &Request,
    history: &[Value],
    response: &Value,
) -> Option<[u8; 32]> {
    if response["status"] != "completed" {
        return None;
    }
    let output = response["output"].as_array()?;
    if output
        .iter()
        .any(|item| !matches!(item["type"].as_str(), Some("message" | "reasoning")))
    {
        return None;
    }
    // Plain-text clients send public assistant text back, without opaque reasoning.
    // Re-submitted opaque items are handled by the response-chain binding first.
    let mut all = history.to_vec();
    all.extend(
        output
            .iter()
            .filter(|item| item["type"] == "message")
            .cloned(),
    );
    digest(request, &all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn routing_identity_is_stable_for_tool_turns_and_header_order_is_explicit() {
        let context = crate::execution::RequestContext::default();
        let tools = json!([{"type":"function","name":"echo","parameters":{"type":"object","properties":{}}}]);
        let first = Request::parse(json!({"model":"m","input":"hello","tools":tools})).unwrap();
        let next=Request::parse(json!({"model":"m","input":[{"role":"user","content":"hello"},{"role":"assistant","content":"answer"},{"role":"user","content":"next"}],"tools":tools})).unwrap();
        if crate::native_cache::enabled() {
            let keyed = Request::parse(
                json!({"model":"m","input":"hello","prompt_cache_key":"explicit-key"}),
            )
            .unwrap();
            let keyed_next = Request::parse(
                json!({"model":"m","input":"different history","prompt_cache_key":"explicit-key"}),
            )
            .unwrap();
            assert_eq!(
                routing(&context, &keyed, &keyed.input),
                routing(&context, &keyed_next, &keyed_next.input)
            );
            assert_ne!(
                routing(&context, &keyed, &keyed.input),
                routing(&context, &first, &first.input)
            );
            let header_context = crate::execution::RequestContext {
                identity_headers: identity::Headers::parse(&{
                    let mut h = axum::http::HeaderMap::new();
                    h.insert("session-id", "primary".parse().unwrap());
                    h
                }),
                ..Default::default()
            };
            assert_eq!(
                routing(&header_context, &keyed, &keyed.input),
                routing(&header_context, &keyed_next, &keyed_next.input)
            );
            assert_ne!(
                routing(&header_context, &keyed, &keyed.input),
                routing(&context, &keyed, &keyed.input)
            );
            assert!(routing(&context, &first, &first.input).is_some());
            assert_eq!(
                routing(&context, &first, &first.input),
                routing(&context, &next, &next.input)
            );
            let other = Request::parse(json!({"model":"m","input":"other","tools":tools})).unwrap();
            assert_ne!(
                routing(&context, &first, &first.input),
                routing(&context, &other, &other.input)
            );
        }
    }
    #[test]
    fn complete_text_history_and_settings_must_match() {
        let first = Request::parse(json!({"model":"m","input":"first"})).unwrap();
        let response = json!({"status":"completed","output":[{"type":"reasoning","encrypted_content":"opaque"},{"type":"message","role":"assistant","id":"out","content":[{"type":"output_text","text":"answer","annotations":[]}]}]});
        let saved = completed(&first, &first.input, &response).unwrap();
        let body = json!({"model":"m","input":[{"role":"user","content":"first"},{"role":"assistant","content":"answer"},{"role":"user","content":"next"}]});
        let next = Request::parse(body.clone()).unwrap();
        assert_eq!(continuation(&next, &next.input), Some(saved));
        for (field, value) in [
            ("instructions", json!("changed")),
            ("reasoning", json!({"effort":"high"})),
            ("model", json!("other")),
        ] {
            let mut changed = body.clone();
            changed[field] = value;
            let request = Request::parse(changed).unwrap();
            assert_ne!(continuation(&request, &request.input), Some(saved));
        }
        let mut changed = next.input.clone();
        changed[0]["content"][0]["text"] = json!("different conversation");
        assert_ne!(continuation(&next, &changed), Some(saved));
        assert_eq!(continuation(&first, &first.input), None);
        changed[0] = json!({"type":"reasoning","encrypted_content":"opaque"});
        assert_eq!(continuation(&next, &changed), None);
    }
}
