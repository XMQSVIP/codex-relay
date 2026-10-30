//! Bounded failure diagnostics, loaded separately from request statistics.
use super::{Operations, storage};
use crate::error::{Error, Result};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use uuid::Uuid;

const OMITTED: &str = "[已省略原文]";
const LIMIT: usize = 64 * 1024;

fn snapshot(value: &Value, request: bool) -> Value {
    fn copy(
        value: &Value,
        request: bool,
        input: bool,
        budget: &mut usize,
        truncated: &mut bool,
    ) -> Value {
        if *budget < 64 {
            *truncated = true;
            return json!("[内容已截断]");
        }
        *budget -= 32;
        match value {
            Value::String(text) => {
                if (request && input) || text.starts_with("data:") {
                    return json!(OMITTED);
                }
                let limit = (*budget / 6).min(8192);
                let mut result: String = text.chars().take(limit).collect();
                *budget = budget.saturating_sub(result.len() * 6);
                if result.len() < text.len() {
                    result.push_str("…[已截断]");
                    *truncated = true;
                }
                json!(result)
            }
            Value::Array(values) => {
                let mut result = Vec::new();
                for value in values.iter().take(128) {
                    if *budget < 64 {
                        break;
                    }
                    result.push(copy(value, request, input, budget, truncated));
                }
                if result.len() < values.len() {
                    *truncated = true;
                    result.push(json!("[其余条目已截断]"));
                }
                Value::Array(result)
            }
            Value::Object(values) => {
                let mut result = serde_json::Map::new();
                for (key, value) in values.iter().take(128) {
                    if key.len() > 256 || *budget < key.len() * 6 + 64 {
                        *truncated = true;
                        break;
                    }
                    *budget -= key.len() * 6;
                    let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                    let secret = matches!(
                        normalized.as_str(),
                        "authorization"
                            | "apikey"
                            | "secret"
                            | "password"
                            | "cookie"
                            | "setcookie"
                            | "accesstoken"
                            | "refreshtoken"
                            | "idtoken"
                    );
                    let omitted = matches!(
                        normalized.as_str(),
                        "developerinstructions" | "baseinstructions"
                    ) || matches!(
                        key.as_str(),
                        "instructions"
                            | "encrypted_content"
                            | "encrypted_output"
                            | "encrypted_function_args"
                            | "internal_chat_message_metadata_passthrough"
                    ) || (request
                        && (matches!(key.as_str(), "metadata" | "client_metadata")
                            || matches!(
                                normalized.as_str(),
                                "sessionid"
                                    | "conversationid"
                                    | "threadid"
                                    | "parentthreadid"
                                    | "forkedfromthreadid"
                                    | "forkedfromid"
                                    | "promptcachekey"
                                    | "xcodexturnmetadata"
                                    | "turnmetadata"
                            )))
                        || (input
                            && matches!(
                                key.as_str(),
                                "arguments"
                                    | "output"
                                    | "result"
                                    | "image_url"
                                    | "file_data"
                                    | "audio"
                            ));
                    let child_input = key == "input"
                        || (input
                            && !matches!(
                                key.as_str(),
                                "type"
                                    | "role"
                                    | "status"
                                    | "id"
                                    | "call_id"
                                    | "name"
                                    | "namespace"
                                    | "detail"
                            ));
                    let copied = if secret {
                        json!("[已隐藏凭据]")
                    } else if omitted {
                        json!(OMITTED)
                    } else {
                        copy(value, request, child_input, budget, truncated)
                    };
                    result.insert(key.clone(), copied);
                }
                if result.len() < values.len() {
                    *truncated = true;
                }
                Value::Object(result)
            }
            _ => value.clone(),
        }
    }
    let mut truncated = false;
    let mut budget = LIMIT;
    let body = copy(value, request, false, &mut budget, &mut truncated);
    json!({"body":body,"truncated":truncated})
}

pub(crate) fn request_snapshot(value: &Value) -> Value {
    let mut result = snapshot(value, true);
    result["textOmitted"] = json!(true);
    result
}

impl Operations {
    pub(crate) fn record_upstream_transport(&self, id: Uuid, transport: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let result = inner.db.execute(
            "UPDATE requests SET upstream_transport=?1 WHERE id=?2",
            params![transport, id.to_string()],
        );
        if result.is_err() {
            inner.storage_healthy = false;
        }
        result.map_err(storage)?;
        Ok(())
    }

    pub(crate) fn record_failure_content(
        &self,
        id: Uuid,
        request: Option<&Value>,
        body: &Value,
        transport: &str,
        status: Option<u16>,
    ) -> Result<()> {
        let mut response = snapshot(body, false);
        response["transport"] = json!(transport);
        response["statusCode"] = json!(status);
        let mut inner = self.inner.lock().unwrap();
        let result = inner.db.execute(
            "UPDATE requests SET request_content=COALESCE(?1,request_content),response_content=?2 WHERE id=?3 AND COALESCE(json_extract(request_content,'$.debug'),0)!=1 AND COALESCE(json_extract(response_content,'$.debug'),0)!=1 AND action!='chat_test' AND status IN ('failed','rejected','timed_out','cancelled','interrupted')",
            params![request.map(Value::to_string),response.to_string(),id.to_string()],
        );
        if result.is_err() {
            inner.storage_healthy = false;
        }
        result.map_err(storage)?;
        Ok(())
    }

    pub fn request_content(&self, id: Uuid) -> Result<Value> {
        let inner = self.inner.lock().unwrap();
        let content: Option<(Option<String>, Option<String>)> = inner
            .db
            .query_row(
                "SELECT request_content,response_content FROM requests WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let (request, response) =
            content.ok_or_else(|| Error::new("NOT_FOUND", "请求记录不存在或已清理。"))?;
        let parse = |value: Option<String>| -> Result<Value> {
            value
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .unwrap_or(Ok(Value::Null))
        };
        Ok(json!({"id":id,"request":parse(request)?,"response":parse(response)?}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_structure_is_retained_without_message_text_or_credentials() {
        let value = request_snapshot(
            &json!({"model":"m","input":[{"role":"user","content":[{"type":"input_text","text":"private prompt"},{"type":"input_image","image_url":"data:image/png;base64,secret"}]},{"type":"function_call","name":"run","arguments":"private arguments"}],"reasoning":{"effort":"high"},"text":{"format":{"type":"json_object"}},"api_key":"secret","instructions":"private instructions"}),
        );
        assert_eq!(value["body"]["input"][0]["role"], "user");
        assert_eq!(
            value["body"]["input"][0]["content"][0]["type"],
            "input_text"
        );
        assert_eq!(value["body"]["reasoning"]["effort"], "high");
        assert_eq!(value["body"]["text"]["format"]["type"], "json_object");
        assert!(!value.to_string().contains("private"));
        assert!(!value.to_string().contains("secret"));
    }
    #[test]
    fn oversized_content_stays_valid_json_and_reports_truncation() {
        let value = snapshot(
            &json!({"error":{"message":"错误\n".repeat(50_000)},"output":vec![json!({"text":"x".repeat(8000)});200]}),
            false,
        );
        assert_eq!(value["truncated"], true);
        assert!(value.to_string().len() < LIMIT + 1024);
        assert!(
            value["body"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("错误")
        );
    }
}
#[test]
fn caller_identity_is_omitted_from_failure_snapshots() {
    for field in [
        "session_id",
        "thread_id",
        "parent_thread_id",
        "forked_from_thread_id",
        "forked_from_id",
        "conversation_id",
        "prompt_cache_key",
        "x-codex-turn-metadata",
        "turn_metadata",
        "turnMetadata",
    ] {
        let value = request_snapshot(&json!({"model":"m", field:"private-session-identity"}));
        assert!(
            !value.to_string().contains("private-session-identity"),
            "{field}"
        );
    }
}
