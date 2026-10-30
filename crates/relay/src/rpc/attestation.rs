use super::{Inner, Rpc};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

impl Rpc {
    /// Register a host-owned proof provider before initialize. The provider must
    /// attest this integration; do not accept tokens from downstream API callers
    /// or reuse proofs belonging to a different application. No provider is
    /// installed by default, so ordinary gateway sessions do not opt in.
    pub fn set_attestation_provider<F>(&self, callback: F)
    where
        F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        *self.inner.attestation.lock().unwrap() = Some(Arc::new(callback));
    }

    pub(super) fn attestation_request(&self, id: &Value, params: &Value) -> Result<()> {
        let reply_error = |code, message| {
            self.inner
                .outgoing
                .try_send(json!({"id":id,"error":{"code":code,"message":message}}))
                .map_err(|_| Error::new("RPC_BACKPRESSURE", "原生发送队列已满。"))
        };
        if !params.is_null() && !params.as_object().is_some_and(|p| p.is_empty()) {
            return reply_error(-32602, "Invalid attestation parameters.");
        }
        let provider = self.inner.attestation.lock().unwrap().clone();
        let Some(provider) = provider else {
            return reply_error(-32001, "Client attestation unavailable.");
        };
        if self.inner.attestation_active.swap(true, Ordering::SeqCst) {
            return reply_error(-32001, "Client attestation already pending.");
        }
        let owner = self.inner.clone();
        let id = id.clone();
        tokio::spawn(async move {
            struct AttestationGuard(Arc<Inner>);
            impl Drop for AttestationGuard {
                fn drop(&mut self) {
                    self.0.attestation_active.store(false, Ordering::SeqCst);
                }
            }
            let _guard = AttestationGuard(owner.clone());
            let result = tokio::select! {
                biased;
                _ = owner.cancel.cancelled() => return,
                result = tokio::time::timeout(Duration::from_secs(8), async { provider().await }) => result,
            };
            let reply = match result {
                Ok(Ok(token))
                    if !token.is_empty()
                        && token.len() <= 64 * 1024
                        && axum::http::HeaderValue::from_str(&token).is_ok() =>
                {
                    json!({"id":id,"result":{"token":token}})
                }
                _ => {
                    json!({"id":id,"error":{"code":-32001,"message":"Client attestation unavailable."}})
                }
            };
            tokio::select! {
                biased;
                _ = owner.cancel.cancelled() => {},
                _ = owner.outgoing.send(reply) => {},
            }
        });
        Ok(())
    }
}
