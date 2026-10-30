use super::{PendingRequest, Rpc};
use crate::{
    error::{Error, Result},
    installation::NativeClientInfo,
};
use serde_json::{Value, json};
use std::time::Duration;

impl Rpc {
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, Duration::from_secs(15))
            .await
    }

    pub async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let mut params = params;
        let location = self.inner.location.lock().unwrap().clone();
        if let Some(location) = location {
            let now = chrono::Utc::now();
            match method {
                "thread/start" => location.apply_thread(&mut params, now),
                "thread/inject_items" => location.align_items(&mut params["items"], now),
                "turn/start" | "thread/compact/start" => {
                    if let Some(input) = params["input"].as_array_mut() {
                        for item in input {
                            if item["type"] == "text"
                                && let Some(text) = item["text"].as_str()
                            {
                                item["text"] = json!(location.align_text(text, "user", now));
                            }
                        }
                    }
                    self.raw_request(
                        "thread/inject_items",
                        json!({"threadId":params["threadId"],"items":[location.time_item(now)]}),
                        timeout,
                    )
                    .await?;
                }
                _ => {}
            }
        }
        self.raw_request(method, params, timeout).await
    }

    async fn raw_request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if self.is_closed() {
            return Err(Error::new("RPC_CLOSED", "原生连接已关闭。"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let message = json!({"id":id,"method":method,"params":params});
        let bytes = serde_json::to_vec(&message)?.len();
        if bytes > super::MAX_BYTES {
            return Err(Error::new(
                "RPC_SIZE",
                &format!("原生 {method} 请求序列化后为 {bytes} 字节，超过 8 MiB 帧上限。"),
            ));
        }
        {
            let mut pending = self.inner.pending.lock().unwrap();
            if pending.len() >= 64 {
                return Err(Error::new("RPC_PENDING_LIMIT", "原生请求队列已满。"));
            }
            pending.insert(id.clone(), sender);
        }
        let _pending = PendingRequest {
            inner: self.inner.clone(),
            id: id.clone(),
        };
        if self.inner.outgoing.try_send(message).is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(Error::new("RPC_BACKPRESSURE", "原生发送队列已满。"));
        }
        let result = tokio::time::timeout(timeout, receiver).await;
        self.inner.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(value)) => value.map_err(|mut error| {
                if error.code == "RPC_REMOTE" {
                    error.message = format!("原生后端拒绝了 {method} 请求。");
                }
                error
            }),
            Ok(Err(_)) => Err(Error::new("RPC_CLOSED", "原生后端断开，请求结果未知。")),
            Err(_) => Err(Error::new(
                "RPC_TIMEOUT",
                &format!("原生 {method} 请求超时，结果未知；未重发。"),
            )),
        }
    }

    pub async fn initialize(&self, client_info: &NativeClientInfo) -> Result<Value> {
        let request_attestation = self.inner.attestation.lock().unwrap().is_some();
        let response = self
            .request(
                "initialize",
                json!({"clientInfo":client_info,"capabilities":{"experimentalApi":true,"requestAttestation":request_attestation}}),
            )
            .await?;
        self.inner
            .outgoing
            .try_send(json!({"method":"initialized"}))
            .map_err(|_| Error::new("RPC_CLOSED", "原生连接已关闭。"))?;
        Ok(response)
    }
}
