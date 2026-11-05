//! Reuse exact continuations owned by the same API key; admission checks current groups.
use super::Request;
use crate::{error::Result, operations::ClientAccess};
use serde_json::{Value, json};
use uuid::Uuid;

mod context;
pub(super) use context::Context;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Session {
    pub thread: String,
    pub directory: std::path::PathBuf,
    owner: Uuid,
    settings: [u8; 32],
    thread_settings: [u8; 32],
    pending_settings: [u8; 32],
    model: String,
    effort: Option<String>,
    summary: Option<String>,
    service_tier: Option<String>,
    pub(super) default_effort: Option<String>,
    pub(super) default_summary: Option<String>,
    pub(super) default_service_tier: Option<Option<String>>,
    prefix: [u8; 32],
    count: usize,
    // The client transcript is an append-only verification ledger, not a copy
    // of native's current prompt. This checkpoint pairs that ledger with the
    // owned thread after confirmed native compaction; it also fences snapshots.
    pub(super) native_checkpoint: Option<[u8; 32]>,
    pub calls: Vec<super::continuation::Call>,
    reusable: bool,
}
pub(crate) struct Reuse<'a> {
    pub access: &'a ClientAccess,
    pub session: &'a mut Option<Session>,
    pub continuation: Option<&'a mut super::continuation::Continuation>,
    pub default_summary: Option<String>,
}

pub(crate) fn default_summary(config: &Value) -> Option<String> {
    match config["model_reasoning_summary"].as_str() {
        Some(value @ ("auto" | "concise" | "detailed" | "none")) => Some(value.into()),
        None if config["model_reasoning_summary"].is_null() => Some("auto".into()),
        _ => None,
    }
}

fn digest(value: &impl serde::Serialize) -> Result<[u8; 32]> {
    crate::profiles::warm::digest(value)
}
pub(super) fn provider_digest(provider: &crate::provider::Provider) -> Result<[u8; 32]> {
    digest(provider)
}
fn history_digest(history: &[Value]) -> Result<[u8; 32]> {
    let mut history = super::request::normalize_input(&json!(history))?;
    if crate::native_cache::enabled() {
        for item in &mut history {
            // Codex may replay omitted reasoning content as null, or omit an
            // empty array. These all contain no plaintext reasoning. Preserve
            // nonempty content, summaries and encrypted content exactly.
            if item["type"] == "reasoning"
                && (item["content"].is_null()
                    || item["content"].as_array().is_some_and(Vec::is_empty))
            {
                item.as_object_mut().unwrap().remove("content");
            }
            let field = "internal_chat_message_metadata_passthrough";
            if let Some(metadata) = item.get_mut(field) {
                if let Some(fields) = metadata.as_object_mut() {
                    // Codex clears message provenance when replaying to a custom
                    // provider. It is not model context. Keep execution metadata
                    // (cell_id/executed_tool_calls/tool_calls_complete) and all
                    // unknown fields, so changed tool results still miss reuse.
                    for key in [
                        "turn_id",
                        "create_time",
                        "content_item_kinds",
                        "content_item_metadata",
                    ] {
                        fields.remove(key);
                    }
                }
                if metadata.is_null() || metadata.as_object().is_some_and(|m| m.is_empty()) {
                    item.as_object_mut().unwrap().remove(field);
                }
            }
        }
    }
    digest(&history)
}
fn settings_value(request: &Request) -> Value {
    let mut settings = json!({"model":request.model,"instructions":request.instructions,
        "tools":request.tools,"toolChoice":request.tool_choice,"reasoning":request.reasoning,
        "text":request.text,"serviceTier":request.service_tier,"cyberAccessProgram":request.cyber_access_program,"clientMetadata":request.client_metadata,"clientSession":request.client_session});
    if crate::native_cache::enabled() {
        // Per-response tracing, including root_turn_id, changes on every desktop
        // turn. apply_turn still forwards it; it is not a stable context setting.
        settings.as_object_mut().unwrap().remove("clientMetadata");
        // Match the parameters actually applied to the native thread/turn.
        // Null effort/summary/verbosity inherit configuration, just like omission.
        // Do not normalize nested schemas, tool definitions or explicit defaults.
        settings["reasoning"]
            .as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
        if settings["text"]["verbosity"].is_null() {
            settings["text"]
                .as_object_mut()
                .unwrap()
                .remove("verbosity");
        }
        settings["instructions"] = json!(request.instructions.as_deref().unwrap_or(""));
        if request.service_tier.as_deref() == Some("auto") {
            settings["serviceTier"] = Value::Null;
        }
    }
    settings
}
pub(super) fn settings(request: &Request) -> Result<[u8; 32]> {
    digest(&settings_value(request))
}
pub(super) fn thread_settings(request: &Request) -> Result<[u8; 32]> {
    let mut settings = settings_value(request);
    if crate::native_cache::enabled() {
        settings.as_object_mut().unwrap().remove("model");
        settings["reasoning"]
            .as_object_mut()
            .unwrap()
            .remove("effort");
        settings["reasoning"]
            .as_object_mut()
            .unwrap()
            .remove("summary");
        settings.as_object_mut().unwrap().remove("serviceTier");
        // outputSchema is scoped to turn/start. Plain text and JSON Schema do
        // not alter thread instructions; json_object uses a thread instruction.
        if !request.json_object() {
            settings["text"].as_object_mut().unwrap().remove("format");
        }
    }
    digest(&settings)
}
fn pending_settings(request: &Request) -> Result<[u8; 32]> {
    let mut settings = settings_value(request);
    if crate::native_cache::enabled() {
        for field in ["effort", "summary"] {
            settings["reasoning"].as_object_mut().unwrap().remove(field);
        }
        settings.as_object_mut().unwrap().remove("serviceTier");
    }
    digest(&settings)
}
fn requested_tier(request: &Request) -> Option<&str> {
    request
        .service_tier
        .as_deref()
        .filter(|tier| *tier != "auto")
}
pub(crate) fn text_history(history: &[Value]) -> bool {
    history.iter().all(|item| match item["type"].as_str() {
        Some("message") => item["content"].as_array().is_some_and(|parts| {
            parts
                .iter()
                .all(|part| matches!(part["type"].as_str(), Some("input_text" | "output_text")))
        }),
        Some("reasoning") => true,
        _ => false,
    })
}
fn appended_user_turn(history: &[Value]) -> bool {
    history
        .iter()
        .any(|item| item["type"] == "message" && item["role"] == "user")
        && history.iter().all(|item| {
            (item["type"] == "message"
                && (item["role"] == "user"
                    || (crate::native_cache::enabled()
                        && matches!(item["role"].as_str(), Some("system" | "developer")))))
                || (crate::native_cache::enabled() && item["type"] == "configuration_update")
        })
}
pub(crate) fn matching_sessions(
    access: &ClientAccess,
    request: &Request,
    history: &[Value],
    sessions: Vec<Session>,
) -> Result<std::collections::HashSet<Session>> {
    let mut matches = std::collections::HashSet::new();
    if sessions.is_empty() {
        return Ok(matches);
    }
    if !crate::native_cache::enabled() && (!request.tools.is_empty() || !text_history(history)) {
        return Ok(matches);
    }
    let settings = settings(request)?;
    let thread_settings = thread_settings(request)?;
    let mut prefixes = std::collections::HashMap::new();
    for session in sessions {
        if session.owner != access.key_id
            || !session.matches_settings(request, settings, thread_settings)
            || history.len() <= session.count
        {
            continue;
        }
        if session.calls.is_empty() {
            if !appended_user_turn(&history[session.count..]) {
                continue;
            }
        } else if super::continuation::replies(&session, history, session.count).is_none() {
            continue;
        }
        let prefix = match prefixes.get(&session.count) {
            Some(value) => *value,
            None => {
                let value = history_digest(&history[..session.count])?;
                prefixes.insert(session.count, value);
                value
            }
        };
        if prefix == session.prefix {
            matches.insert(session);
        }
    }
    Ok(matches)
}

impl Session {
    pub(super) fn model_changed(&self, request: &Request) -> bool {
        self.model != request.model
    }
    fn matches_settings(
        &self,
        request: &Request,
        settings: [u8; 32],
        thread_settings: [u8; 32],
    ) -> bool {
        if crate::native_cache::enabled() && (!request.reuse_allowed || !self.reusable) {
            return false;
        }
        if !crate::native_cache::enabled() {
            return self.settings == settings;
        }
        if !self.calls.is_empty() {
            return self.settings == settings
                || (pending_settings(request).is_ok_and(|value| self.pending_settings == value)
                    && self.can_restore_defaults(request)
                    && (requested_tier(request) == self.service_tier.as_deref()
                        || requested_tier(request).is_some()
                        || self.default_service_tier.is_some()));
        }
        self.thread_settings == thread_settings
            // Model defaults may differ. An explicit effort allows a safe turn
            // override; otherwise a new thread resolves the target's defaults.
            && (!self.model_changed(request) || request.reasoning["effort"].is_string())
            // Missing effort means configured defaults, not a previous turn's
            // override. If native did not report that default, rebuild safely.
            && self.can_restore_defaults(request)
    }

    fn can_restore_defaults(&self, request: &Request) -> bool {
        (request.reasoning["effort"].is_string()
            || self.effort.is_none()
            || self.default_effort.is_some())
            && (request.reasoning["summary"].is_string()
                || self.summary.is_none()
                || self.default_summary.is_some())
    }

    pub(super) fn pending_update(&self, request: &Request) -> Result<Value> {
        let mut update = json!({});
        for (field, previous, default) in [
            ("effort", &self.effort, &self.default_effort),
            ("summary", &self.summary, &self.default_summary),
        ] {
            let requested = request.reasoning[field].as_str();
            if requested != previous.as_deref() {
                update[field] = json!(requested.or(default.as_deref()).ok_or_else(|| {
                    crate::error::Error::new("NATIVE_REUSE_MISMATCH", "无法恢复原生轮次默认设置。")
                })?);
            }
        }
        if requested_tier(request) != self.service_tier.as_deref() {
            update["serviceTier"] = match requested_tier(request) {
                Some(tier) => json!(tier),
                None => json!(self.default_service_tier.as_ref().ok_or_else(|| {
                    crate::error::Error::new(
                        "NATIVE_REUSE_MISMATCH",
                        "无法恢复原生服务等级默认设置。",
                    )
                })?),
            };
        }
        Ok(update)
    }

    pub(super) fn apply_turn_defaults(&self, request: &Request, params: &mut Value) {
        // A live override changes only the running turn. Even if the latest
        // tool-result request omitted an override, turn/start may still retain
        // one from the first request of that tool chain. Restore known defaults
        // on every new turn instead of inferring them from the last HTTP request.
        if crate::native_cache::enabled()
            && self.calls.is_empty()
            && request.reasoning["effort"].is_null()
            && let Some(effort) = &self.default_effort
        {
            params["effort"] = json!(effort);
        }
        if crate::native_cache::enabled()
            && self.calls.is_empty()
            && request.reasoning["summary"].is_null()
            && let Some(summary) = &self.default_summary
        {
            params["summary"] = json!(summary);
        }
    }

    pub fn new(
        thread: String,
        directory: std::path::PathBuf,
        access: &ClientAccess,
        request: &Request,
        history: &[Value],
        output: &[Value],
    ) -> Result<Self> {
        let all = history.iter().chain(output).cloned().collect::<Vec<_>>();
        Ok(Self {
            thread,
            directory,
            owner: access.key_id,
            settings: settings(request)?,
            thread_settings: thread_settings(request)?,
            pending_settings: pending_settings(request)?,
            model: request.model.clone(),
            effort: request.reasoning["effort"].as_str().map(str::to_owned),
            summary: request.reasoning["summary"].as_str().map(str::to_owned),
            service_tier: requested_tier(request).map(str::to_owned),
            default_effort: None,
            default_summary: None,
            default_service_tier: None,
            prefix: history_digest(&all)?,
            count: all.len(),
            native_checkpoint: None,
            calls: vec![],
            reusable: request.reuse_allowed,
        })
    }
    pub fn continuation(
        &self,
        access: &ClientAccess,
        request: &Request,
        history: &[Value],
    ) -> Result<Option<usize>> {
        if self.owner != access.key_id
            || !self.matches_settings(request, settings(request)?, thread_settings(request)?)
            || (!crate::native_cache::enabled() && !request.tools.is_empty())
            || history.len() <= self.count
            || (!crate::native_cache::enabled() && !text_history(history))
            || history_digest(&history[..self.count])? != self.prefix
        {
            return Ok(None);
        }
        if self.calls.is_empty() {
            if !appended_user_turn(&history[self.count..]) {
                return Ok(None);
            }
        } else if super::continuation::replies(self, history, self.count).is_none() {
            return Ok(None);
        }
        Ok(Some(self.count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_tools_allow_reuse_but_different_sessions_or_catalogs_do_not() {
        if !crate::native_cache::enabled() {
            return;
        }
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let tools = json!([{"type":"function","name":"echo","parameters":{"type":"object","properties":{}}}]);
        let mut request =
            Request::parse(json!({"model":"m","input":"first","tools":tools})).unwrap();
        request.client_session = Some([1; 32]);
        let output = vec![
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}),
        ];
        let session = Session::new(
            "thread".into(),
            "directory".into(),
            &access,
            &request,
            &request.input,
            &output,
        )
        .unwrap();
        let mut history = request.input.clone();
        history.extend(output);
        history.push(
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}),
        );
        assert!(
            session
                .continuation(&access, &request, &history)
                .unwrap()
                .is_some()
        );
        request.client_session = Some([2; 32]);
        assert!(
            session
                .continuation(&access, &request, &history)
                .unwrap()
                .is_none()
        );
        request.client_session = Some([1; 32]);
        request.tools[0]["name"] = json!("changed");
        assert!(
            session
                .continuation(&access, &request, &history)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn exact_continuations_are_scoped_and_settings_never_leak() {
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let request =
            Request::parse(json!({"model":"m","input":"first","reasoning":{"effort":"high"}}))
                .unwrap();
        let output = vec![
            json!({"type":"reasoning","id":"r","status":"completed","summary":[]}),
            json!({"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"one","annotations":[]}]}),
        ];
        let session = Session::new(
            "thread".into(),
            std::path::PathBuf::from("synthetic-session"),
            &access,
            &request,
            &request.input,
            &output,
        )
        .unwrap();
        let mut history = request.input.clone();
        history.extend(output);
        history.push(json!({"role":"user","content":"second"}));
        let next =
            Request::parse(json!({"model":"m","input":history,"reasoning":{"effort":"high"}}))
                .unwrap();
        assert_eq!(
            session.continuation(&access, &next, &next.input).unwrap(),
            Some(3)
        );
        let matched =
            matching_sessions(&access, &next, &next.input, vec![session.clone()]).unwrap();
        assert!(matched.contains(&session));
        let mut newer = session.clone();
        newer.count += 1;
        assert!(
            !matched.contains(&newer),
            "a changed session must not match a stale preparation snapshot"
        );
        let moved = ClientAccess {
            group_id: Uuid::new_v4(),
            ..access.clone()
        };
        assert_eq!(
            session.continuation(&moved, &next, &next.input).unwrap(),
            Some(3)
        );
        assert!(
            matching_sessions(&moved, &next, &next.input, vec![session.clone()])
                .unwrap()
                .contains(&session)
        );
        let other = ClientAccess {
            key_id: Uuid::new_v4(),
            ..moved
        };
        assert_eq!(
            session.continuation(&other, &next, &next.input).unwrap(),
            None
        );
        assert!(
            matching_sessions(&other, &next, &next.input, vec![session.clone()])
                .unwrap()
                .is_empty()
        );
        for (field, value) in [
            ("reasoning", json!({"effort":"medium"})),
            ("instructions", json!("different")),
            ("model", json!("other")),
            ("service_tier", json!("priority")),
            ("text", json!({"verbosity":"low"})),
        ] {
            let mut body = json!({"model":"m","input":history,"reasoning":{"effort":"high"}});
            let mutable = crate::native_cache::enabled()
                && matches!(field, "reasoning" | "service_tier" | "model");
            body[field] = value;
            let changed = Request::parse(body).unwrap();
            assert_eq!(
                session
                    .continuation(&access, &changed, &changed.input)
                    .unwrap(),
                mutable.then_some(3)
            );
        }
        let mut changed = next.input.clone();
        changed[0]["content"][0]["text"] = json!("edited first turn");
        assert_eq!(
            session.continuation(&access, &next, &changed).unwrap(),
            None
        );
        assert_eq!(
            session
                .continuation(&access, &next, &next.input[..3])
                .unwrap(),
            None
        );
        changed = next.input.clone();
        changed[3]["role"] = json!("developer");
        assert_eq!(
            session.continuation(&access, &next, &changed).unwrap(),
            None
        );
    }
}

#[cfg(test)]
#[path = "reuse_tests.rs"]
mod desktop_tests;

#[cfg(test)]
#[path = "turn_settings_tests.rs"]
mod turn_settings_tests;

#[cfg(test)]
#[path = "compaction_reuse_tests.rs"]
mod compaction_tests;
