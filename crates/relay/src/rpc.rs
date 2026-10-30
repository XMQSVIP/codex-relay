use crate::error::{Error, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) type Attestation = dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
    + Send
    + Sync;
pub(super) type AuthRefresh = dyn Fn(Option<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>>
    + Send
    + Sync;

pub(crate) struct QuotaEvent {
    pub message: Value,
    claimed: AtomicBool,
}
impl QuotaEvent {
    pub fn claim(&self) -> bool {
        !self.claimed.swap(true, std::sync::atomic::Ordering::AcqRel)
    }
}

pub(super) struct Inner {
    pub(super) location: Mutex<Option<crate::account_location::Location>>,
    pub(super) pending: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    pub(super) outgoing: mpsc::Sender<Value>,
    pub(super) events: broadcast::Sender<Value>,
    quota_events: broadcast::Sender<Arc<QuotaEvent>>,
    pub(super) thread_events: Mutex<HashMap<String, broadcast::Sender<Value>>>,
    pub(super) cancel: CancellationToken,
    pub(super) client_tools: AtomicBool,
    pub(super) tool_calls: Mutex<HashMap<String, String>>,
    pub(super) auth_refresh: Mutex<Option<Arc<AuthRefresh>>>,
    pub(super) auth_refresh_active: AtomicBool,
    pub(super) attestation: Mutex<Option<Arc<Attestation>>>,
    pub(super) attestation_active: AtomicBool,
}

#[derive(Clone)]
pub struct Rpc {
    pub(super) inner: Arc<Inner>,
}

pub(super) struct PendingRequest {
    pub(super) inner: Arc<Inner>,
    pub(super) id: String,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.inner.pending.lock().unwrap().remove(&self.id);
    }
}

impl Rpc {
    pub(super) fn new() -> (Self, mpsc::Receiver<Value>) {
        let (outgoing, receiver) = mpsc::channel(64);
        let (events, _) = broadcast::channel(512);
        (
            Self {
                inner: Arc::new(Inner {
                    location: Mutex::new(None),
                    pending: Mutex::new(HashMap::new()),
                    outgoing,
                    events,
                    quota_events: broadcast::channel(32).0,
                    thread_events: Mutex::new(HashMap::new()),
                    cancel: CancellationToken::new(),
                    client_tools: AtomicBool::new(false),
                    tool_calls: Mutex::new(HashMap::new()),
                    auth_refresh: Mutex::new(None),
                    auth_refresh_active: AtomicBool::new(false),
                    attestation: Mutex::new(None),
                    attestation_active: AtomicBool::new(false),
                }),
            },
            receiver,
        )
    }

    pub fn set_location(&self, location: Option<crate::account_location::Location>) {
        *self.inner.location.lock().unwrap() = location;
    }

    /// Top-level hosted search is not a dynamic callback. Advertise its account
    /// location explicitly when the caller already requested the search tool.
    pub(crate) fn location_search_item(&self, tools: &[Value], disabled: bool) -> Option<Value> {
        if disabled || !tools.iter().any(|tool| tool["type"] == "web_search") {
            return None;
        }
        let location = self.inner.location.lock().unwrap().clone()?;
        let mut value = location.search_location();
        value["type"] = serde_json::json!("approximate");
        Some(
            serde_json::json!({"type":"additional_tools","role":"developer","tools":[{"type":"web_search","user_location":value}]}),
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.inner.events.subscribe()
    }

    pub(crate) fn subscribe_quota(&self) -> broadcast::Receiver<Arc<QuotaEvent>> {
        self.inner.quota_events.subscribe()
    }

    pub(crate) fn subscribe_thread(&self, thread: &str) -> broadcast::Receiver<Value> {
        self.inner
            .thread_events
            .lock()
            .unwrap()
            .entry(thread.to_owned())
            .or_insert_with(|| broadcast::channel(512).0)
            .subscribe()
    }

    pub(super) fn publish(&self, message: Value) -> bool {
        let mut received = false;
        if message["method"] == "account/rateLimits/updated" {
            received |= self
                .inner
                .quota_events
                .send(Arc::new(QuotaEvent {
                    message: message.clone(),
                    claimed: AtomicBool::new(false),
                }))
                .is_ok();
        }
        let channels = self.inner.thread_events.lock().unwrap();
        if let Some(thread) = message["params"]["threadId"].as_str() {
            if let Some(channel) = channels.get(thread) {
                received = channel.send(message.clone()).is_ok();
            }
        } else if message["method"] == "bridge/callbackRejected" {
            for channel in channels.values() {
                received |= channel.send(message.clone()).is_ok();
            }
        }
        received | self.inner.events.send(message).is_ok()
    }

    pub(crate) async fn closed(&self) {
        self.inner.cancel.cancelled().await;
    }

    pub fn is_closed(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }

    pub fn close(&self) {
        self.inner.cancel.cancel();
        for (_, sender) in self.inner.pending.lock().unwrap().drain() {
            let _ = sender.send(Err(Error::new(
                "RPC_CLOSED",
                "原生后端已断开；请求未重发。",
            )));
        }
    }
}

mod attestation;
mod auth_refresh;
mod dispatch;
mod host_callbacks;
mod matching;
mod reclamation;
mod tool_callbacks;
mod transport;

#[cfg(test)]
mod tests;
