use super::Rpc;
use crate::error::{Error, Result};
use serde_json::Value;

impl Rpc {
    pub(super) fn receive(&self, text: &str) -> Result<()> {
        if text.len() > super::MAX_BYTES {
            return Err(Error::new("RPC_SIZE", "原生消息超限。"));
        }
        let message: Value = serde_json::from_str(text)?;
        if !message.is_object() {
            return Err(Error::new("RPC_PROTOCOL", "原生消息无效。"));
        }
        if self.handle_host_callback(&message)? {
            return Ok(());
        }
        if message["method"].is_string() {
            if let Some(id) = message.get("id") {
                if !id.is_string() && !id.is_number() {
                    return Err(Error::new("RPC_PROTOCOL", "原生消息标识无效。"));
                }
                if self.handle_auth_refresh(id, &message)? {
                    return Ok(());
                }
                if self.handle_tool_callback(id, &message)? {
                    return Ok(());
                }
                self.reject_client_callback(
                    id,
                    message["method"].as_str().unwrap(),
                    &message["params"]["threadId"],
                )?;
            } else {
                // Notifications are forwarded as downstream events only when
                // they do not carry a request id. Callback requests are handled above.
                self.publish(message);
            }
            return Ok(());
        }
        self.handle_response(&message)
    }

    fn handle_response(&self, message: &Value) -> Result<()> {
        let id = message
            .get("id")
            .filter(|v| v.is_string() || v.is_number())
            .ok_or_else(|| Error::new("RPC_PROTOCOL", "原生响应缺少标识。"))?;
        if message.get("result").is_some() == message.get("error").is_some() {
            return Err(Error::new("RPC_PROTOCOL", "原生响应格式无效。"));
        }
        let key = id
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| id.to_string());
        if let Some(sender) = self.inner.pending.lock().unwrap().remove(&key) {
            let result = if message.get("error").is_some() {
                Err(if message["error"]["code"] == -32601 {
                    Error::new("RPC_METHOD_UNSUPPORTED", "原生后端不支持此接口。")
                } else {
                    Error::new("RPC_REMOTE", "原生后端拒绝了请求。")
                })
            } else {
                Ok(message["result"].clone())
            };
            let _ = sender.send(result);
        }
        Ok(())
    }
}
