//! Public diagnostics for native JSON-RPC failures. Never forward arbitrary
//! message/data: they can echo credentials, local paths or conversation content.
use crate::error::Error;
use serde_json::Value;

pub(super) fn from_value(value: &Value) -> Error {
    let code = value["code"].as_i64();
    let reason = if code == Some(-32601) {
        "原生后端不支持此接口。"
    } else {
        value["message"]
            .as_str()
            .and_then(known_reason)
            .unwrap_or(match code {
                Some(-32700) => "原生后端无法解析 JSON 请求。",
                Some(-32600) => "原生后端判定请求无效；未识别的错误详情已隐藏。",
                Some(-32602) => "原生后端判定请求参数无效；未识别的错误详情已隐藏。",
                Some(-32603) => "原生后端内部错误；未识别的错误详情已隐藏。",
                _ => "未识别的原生错误详情已隐藏。",
            })
    };
    let native_code = code.map_or_else(|| "未提供有效数值".to_owned(), |code| code.to_string());
    Error::new(
        if code == Some(-32601) {
            "RPC_METHOD_UNSUPPORTED"
        } else {
            "RPC_REMOTE"
        },
        &format!("原生错误码 {native_code}：{reason}"),
    )
}

fn known_reason(message: &str) -> Option<&'static str> {
    // Match only native diagnostic prefixes, not arbitrary substrings. The
    // suffix (including tool names and schema values) is deliberately omitted.
    // All returned text is static, so even a forged/oversized error is bounded.
    const REASONS: &[(&str, &str)] = &[
        (
            "dynamic tool input schema is not supported for ",
            "动态工具参数 Schema 不受支持，请检查 dynamicTools 的 inputSchema。",
        ),
        (
            "dynamic tool namespace collides with a reserved Responses API namespace: ",
            "动态工具命名空间与 Responses API 保留命名空间冲突。",
        ),
        (
            "dynamic tool namespace is reserved: ",
            "动态工具命名空间使用了原生保留名称。",
        ),
        (
            "dynamic tool name is reserved: ",
            "动态工具使用了原生保留名称。",
        ),
        (
            "duplicate dynamic tool namespace: ",
            "动态工具命名空间重复。",
        ),
        (
            "duplicate dynamic tool name in namespace ",
            "同一命名空间内的动态工具名称重复。",
        ),
        ("duplicate dynamic tool name: ", "动态工具名称重复。"),
        (
            "dynamic tool namespace must contain at least one tool: ",
            "动态工具命名空间不能为空。",
        ),
        (
            "dynamic tool namespace description must be at most ",
            "动态工具命名空间描述超过原生长度限制。",
        ),
        (
            "dynamic tool namespace has leading/trailing whitespace: ",
            "动态工具命名空间名称首尾含空白。",
        ),
        (
            "dynamic tool name has leading/trailing whitespace: ",
            "动态工具名称首尾含空白。",
        ),
        (
            "deferred dynamic tool must include a namespace: ",
            "延迟加载的动态工具必须声明命名空间。",
        ),
        (
            "failed to load configuration layers: ",
            "原生后端加载分层配置失败。",
        ),
        ("failed to load configuration: ", "原生后端加载配置失败。"),
        ("failed to load config: ", "原生后端加载配置失败。"),
    ];
    REASONS
        .iter()
        .find_map(|(prefix, reason)| message.starts_with(prefix).then_some(*reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnostics_never_copy_remote_text_or_data() {
        for message in [
            "Authorization: Bearer private-token".to_owned(),
            "dynamic tool input schema is not supported for secret: private prompt".to_owned(),
            "failed to load config: /Users/private/config.toml".to_owned(),
            "私密正文\n".repeat(100_000),
        ] {
            let error = from_value(&json!({
                "code": -32600, "message": message,
                "data": {"token":"private-token","message":"private prompt"}
            }));
            assert_eq!(error.code, "RPC_REMOTE");
            assert!(error.message.contains("-32600"));
            assert!(error.message.len() < 400);
            for secret in ["private", "secret", "私密", "/Users/"] {
                assert!(!error.message.contains(secret));
            }
        }
    }

    #[test]
    fn malformed_errors_and_unknown_codes_have_a_bounded_fallback() {
        for value in [
            Value::Null,
            json!("private error"),
            json!({"code":"private-token","message":{"text":"private"}}),
            json!({"code":1.5,"message":"private"}),
        ] {
            let error = from_value(&value);
            assert_eq!(error.code, "RPC_REMOTE");
            assert!(error.message.contains("未提供有效数值"));
            assert!(!error.message.contains("private"));
        }
        let error = from_value(&json!({"code":-32099,"message":"private"}));
        assert!(error.message.contains("-32099"));
        assert!(error.message.contains("详情已隐藏"));
    }
}
