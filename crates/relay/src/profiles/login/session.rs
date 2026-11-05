//! In-memory native login sessions. Secrets never enter the public status view.
use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) const TTL_MS: u64 = 10 * 60 * 1000;
const RETAIN_MS: u64 = 30 * 60 * 1000;
pub(super) type Sessions = Arc<Mutex<HashMap<Uuid, Session>>>;

#[derive(Default)]
pub(crate) struct Logins {
    pub(super) sessions: Sessions,
    pub(super) starts: tokio::sync::Mutex<()>,
}
impl Drop for Logins {
    fn drop(&mut self) {
        for session in self.sessions.lock().unwrap().values_mut() {
            session.finish("cancelled", None);
        }
    }
}

pub(super) struct Session {
    pub backend: Option<std::sync::Weak<crate::backend::Backend>>,
    pub view: Value,
    pub rpc: Option<Rpc>,
    pub callback: Option<super::callback::Callback>,
    pub cancel: CancellationToken,
    pub submitted: bool,
}
impl Session {
    pub fn active(&self) -> bool {
        matches!(self.view["status"].as_str(), Some("waiting" | "completing"))
    }
    pub fn matches(&self, login_id: &str) -> bool {
        self.view["loginId"].as_str() == Some(login_id)
    }
    pub fn finish(&mut self, status: &str, error: Option<Error>) {
        if !self.active() {
            return;
        }
        self.view["status"] = status.into();
        self.view["finishedAt"] = crate::now_ms().into();
        self.view["error"] = error.map_or(Value::Null, |e| json!(e));
        for field in ["authUrl", "verificationUrl", "userCode"] {
            self.view.as_object_mut().unwrap().remove(field);
        }
        self.view["manualCallbackAvailable"] = false.into();
        self.callback = None;
        self.rpc = None;
        self.backend = None;
        self.cancel.cancel();
    }
}
pub(super) fn prune(sessions: &mut HashMap<Uuid, Session>) {
    sessions.retain(|_, s| {
        s.active()
            || s.view["finishedAt"]
                .as_u64()
                .is_some_and(|at| crate::now_ms().saturating_sub(at) < RETAIN_MS)
    });
}
pub(super) fn expired() -> Error {
    Error::new("LOGIN_EXPIRED", "登录会话已失效，请重新发起。")
}
pub(super) fn finish(
    sessions: &Sessions,
    id: Uuid,
    login_id: &str,
    status: &str,
    error: Option<Error>,
) {
    if let Some(session) = sessions
        .lock()
        .unwrap()
        .get_mut(&id)
        .filter(|s| s.matches(login_id))
    {
        session.finish(status, error);
    }
}
pub(super) fn view(sessions: &Sessions, id: Uuid, login_id: Option<&str>) -> Result<Value> {
    let mut all = sessions.lock().unwrap();
    prune(&mut all);
    match all.get(&id) {
        Some(s) if login_id.is_none_or(|expected| s.matches(expected)) => Ok(s.view.clone()),
        _ if login_id.is_some() => Err(expired()),
        _ => Ok(Value::Null),
    }
}
