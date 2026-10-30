use super::Rpc;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

impl Rpc {
    pub fn enable_client_tools(&self) {
        self.inner.client_tools.store(true, Ordering::SeqCst);
    }

    pub(super) fn handle_tool_callback(&self, id: &Value, message: &Value) -> Result<bool> {
        if message["method"] != "item/tool/call" || !self.inner.client_tools.load(Ordering::SeqCst)
        {
            return Ok(false);
        }
        let mut calls = self.inner.tool_calls.lock().unwrap();
        let thread = message["params"]["threadId"].as_str().unwrap_or("");
        if calls
            .values()
            .filter(|owner| owner.as_str() == thread)
            .count()
            >= 128
            || calls.contains_key(&id.to_string())
        {
            return Err(Error::new("RPC_CALLBACK_LIMIT", "工具回调重复或超限。"));
        }
        calls.insert(id.to_string(), thread.to_owned());
        drop(calls);
        if !self.publish(message.clone()) {
            return Err(Error::new("RPC_CLOSED", "无工具接收者。"));
        }
        Ok(true)
    }

    pub(super) fn reject_client_callback(
        &self,
        id: &Value,
        method: &str,
        thread: &Value,
    ) -> Result<()> {
        self.inner
            .outgoing
            .try_send(
                json!({"id":id,"error":{"code":-32601,"message":"Client callbacks are disabled."}}),
            )
            .map_err(|_| Error::new("RPC_BACKPRESSURE", "原生发送队列已满。"))?;
        self.publish(json!({"method":"bridge/callbackRejected","params":{"method":method,"threadId":thread}}));
        Ok(())
    }

    pub fn clear_tool_callbacks(&self) {
        self.inner.tool_calls.lock().unwrap().clear();
    }

    pub(crate) fn clear_thread_tool_callbacks(&self, thread: &str) {
        self.inner
            .tool_calls
            .lock()
            .unwrap()
            .retain(|_, owner| owner != thread);
    }

    pub async fn answer_tool(&self, id: Value, result: Value) -> Result<()> {
        if self
            .inner
            .tool_calls
            .lock()
            .unwrap()
            .remove(&id.to_string())
            .is_none()
        {
            return Err(Error::new("NATIVE_TOOL_ID", "工具请求不存在或已回复。"));
        }
        self.inner
            .outgoing
            .send(json!({"id":id,"result":result}))
            .await
            .map_err(|_| Error::new("RPC_CLOSED", "原生连接已关闭。"))
    }
}
