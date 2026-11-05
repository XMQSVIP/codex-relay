use super::Rpc;
use crate::error::{Error, Result};
use serde_json::{Value, json};

impl Rpc {
    pub(super) fn handle_host_callback(&self, message: &Value) -> Result<bool> {
        let Some(method) = message["method"].as_str() else {
            return Ok(false);
        };
        if !matches!(method, "attestation/generate" | "currentTime/read") {
            return Ok(false);
        }
        let id = message
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .ok_or_else(|| Error::new("RPC_PROTOCOL", "原生宿主回调缺少有效标识。"))?;
        // Host callbacks stay on this private RPC connection, including
        // malformed requests. Never forward them to downstream API events.
        if method == "attestation/generate" {
            self.attestation_request(id, &message["params"])?;
        } else {
            self.current_time_request(id, &message["params"])?;
        }
        Ok(true)
    }

    fn current_time_request(&self, id: &Value, params: &Value) -> Result<()> {
        let valid = params["threadId"]
            .as_str()
            .is_some_and(|thread| !thread.is_empty() && thread.len() <= 256);
        let reply = if valid {
            // The native protocol specifies whole Unix seconds, independent of
            // the host timezone. This callback is handled by the host itself.
            json!({"id":id,"result":{"currentTimeAt":crate::now_ms()/1000}})
        } else {
            json!({"id":id,"error":{"code":-32602,"message":"Invalid current time parameters."}})
        };
        self.inner
            .outgoing
            .try_send(reply)
            .map_err(|_| Error::new("RPC_BACKPRESSURE", "原生发送队列已满。"))
    }
}
