use super::{Inner, Rpc};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

impl Rpc {
    pub fn set_auth_refresh<F>(&self, callback: F)
    where
        F: Fn(
                Option<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        *self.inner.auth_refresh.lock().unwrap() = Some(Arc::new(callback));
    }

    pub(super) fn handle_auth_refresh(&self, id: &Value, message: &Value) -> Result<bool> {
        if message["method"] != "account/chatgptAuthTokens/refresh" {
            return Ok(false);
        }
        let handler = self.inner.auth_refresh.lock().unwrap().clone();
        let Some(handler) = handler else {
            return Ok(false);
        };
        if self.inner.auth_refresh_active.swap(true, Ordering::SeqCst) {
            return Err(Error::new("RPC_CALLBACK_LIMIT", "原生认证刷新重复。"));
        }
        let owner = self.inner.clone();
        let id = id.clone();
        let outgoing = self.inner.outgoing.clone();
        let cancel = self.inner.cancel.clone();
        let previous = message["params"]["previousAccountId"]
            .as_str()
            .map(str::to_owned);
        tokio::spawn(async move {
            struct RefreshGuard(Arc<Inner>);
            impl Drop for RefreshGuard {
                fn drop(&mut self) {
                    self.0.auth_refresh_active.store(false, Ordering::SeqCst);
                }
            }
            let _guard = RefreshGuard(owner);
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                r = tokio::time::timeout(Duration::from_secs(8), handler(previous)) => r,
            };
            let reply = match result {
                Ok(Ok(result)) => json!({"id":id,"result":result}),
                _ => {
                    json!({"id":id,"error":{"code":-32001,"message":"Desktop credentials unavailable or account changed."}})
                }
            };
            tokio::select! {
                _ = cancel.cancelled() => {},
                _ = outgoing.send(reply) => {},
            }
        });
        Ok(true)
    }
}
