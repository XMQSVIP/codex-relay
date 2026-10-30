//! Interactive native login with correlated, bounded sessions.
use super::Profiles;
use crate::{
    error::{Error, Result},
    oauth,
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
mod callback;
mod session;
pub(crate) use session::Logins;
use session::{Session, Sessions, TTL_MS};

impl Profiles {
    pub(crate) fn require_no_login(&self, id: Uuid) -> Result<()> {
        if self
            .logins
            .sessions
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(Session::active)
        {
            return Err(Error::new(
                "BUSY",
                "此账户正在登录，请先完成或取消本次登录。",
            ));
        }
        Ok(())
    }
    pub(crate) async fn check_login_start(&self, id: Uuid, kind: &str) -> Result<()> {
        self.require_source(id, "isolated").await?;
        if !["chatgpt", "chatgptDeviceCode"].contains(&kind) {
            return Err(Error::new("INVALID_INPUT", "不支持的登录方式。"));
        }
        self.require_no_login(id)?;
        if kind == "chatgpt" {
            if self
                .logins
                .sessions
                .lock()
                .unwrap()
                .values()
                .any(|s| s.active() && s.view["type"] == "chatgpt")
            {
                return Err(Error::new(
                    "BUSY",
                    "另一账户正在浏览器登录，请先完成或取消；也可使用设备码。",
                ));
            }
            // Native Codex may cancel an existing listener when binding fails.
            // Refuse occupied ports before invoking it; never probe its /cancel endpoint.
            let listener = std::net::TcpListener::bind(("127.0.0.1", 1455)).map_err(|_| {
                Error::new(
                    "LOGIN_PORT_BUSY",
                    "本地登录回调端口 1455 不可用，请完成其他登录后重试，或使用设备码。",
                )
            })?;
            drop(listener);
        }
        Ok(())
    }
    pub async fn login(&self, id: Uuid, kind: &str) -> Result<Value> {
        let _start = self.logins.starts.lock().await;
        self.check_login_start(id, kind).await?;
        self.stop(id).await?;
        self.reset_binding_for_login(id).await?;
        self.start(id).await?;
        let slot = self.slot(id).await?;
        let backend = slot
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::new("WORKER_STOPPED", "账户实例尚未启动。"))?;
        let events = backend.rpc.subscribe();
        let mut params = json!({"type":kind});
        if kind == "chatgpt" {
            params["codexStreamlinedLogin"] = false.into();
            params["useHostedLoginSuccessPage"] = false.into();
        }
        let result = match backend.rpc.request("account/login/start", params).await {
            Ok(value) => value,
            Err(_) => {
                let _ = backend.stop().await;
                return Err(Error::new(
                    "LOGIN_START_FAILED",
                    "无法启动原生登录，请检查安装、网络及回调端口，或使用设备码。",
                ));
            }
        };
        let parsed = prepare_session(&result, kind, backend.rpc.clone());
        let mut session = match parsed {
            Ok(session) => session,
            Err(error) => {
                // A malformed response must not leave an untracked listener alive.
                let _ = backend.stop().await;
                return Err(error);
            }
        };
        session.backend = Some(std::sync::Arc::downgrade(&backend));
        let view = session.view.clone();
        let login_id = view["loginId"].as_str().unwrap().to_owned();
        let cancel = session.cancel.clone();
        let sessions = self.logins.sessions.clone();
        {
            let mut all = sessions.lock().unwrap();
            session::prune(&mut all);
            all.insert(id, session);
        }
        let home = self.home(id);
        tokio::spawn(async move {
            monitor(
                sessions,
                id,
                login_id,
                backend.rpc.clone(),
                home,
                events,
                cancel,
            )
            .await;
        });
        Ok(view)
    }
    pub async fn login_status(&self, id: Uuid, login_id: Option<&str>) -> Result<Value> {
        self.require_source(id, "isolated").await?;
        session::view(&self.logins.sessions, id, login_id)
    }
    pub async fn submit_login_callback(
        &self,
        id: Uuid,
        login_id: &str,
        value: &str,
    ) -> Result<Value> {
        self.require_source(id, "isolated").await?;
        let (target, cancel, view) = {
            let mut sessions = self.logins.sessions.lock().unwrap();
            let s = sessions
                .get_mut(&id)
                .filter(|s| s.matches(login_id))
                .ok_or_else(session::expired)?;
            if !s.active() || crate::now_ms() >= s.view["expiresAt"].as_u64().unwrap_or(0) {
                return Err(session::expired());
            }
            if s.submitted {
                return Err(Error::new(
                    "BUSY",
                    "回调已提交，正在等待登录结果，请勿重复提交。",
                ));
            }
            let target = s
                .callback
                .as_ref()
                .ok_or_else(|| {
                    Error::new(
                        "INVALID_INPUT",
                        "本次登录不支持手动回填，请使用原授权页面或设备码。",
                    )
                })?
                .validate(value)?;
            s.submitted = true;
            s.view["status"] = "completing".into();
            (target, s.cancel.clone(), s.view.clone())
        };
        let sessions = self.logins.sessions.clone();
        let login_id = login_id.to_owned();
        tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                result = callback::forward(target) => result,
            };
            if let Err(error) = result {
                let mut all = sessions.lock().unwrap();
                if let Some(s) = all
                    .get_mut(&id)
                    .filter(|s| s.matches(&login_id) && s.active())
                {
                    s.view["warning"] = error.message.into();
                }
            }
        });
        Ok(view)
    }
    pub async fn cancel_login(&self, id: Uuid, login_id: &str) -> Result<()> {
        self.require_source(id, "isolated").await?;
        let (rpc, backend) = {
            let mut sessions = self.logins.sessions.lock().unwrap();
            let s = sessions
                .get_mut(&id)
                .filter(|s| s.matches(login_id))
                .ok_or_else(session::expired)?;
            let rpc = s.rpc.clone();
            let backend = s.backend.clone();
            s.finish("cancelled", None);
            (rpc, backend)
        };
        if let Some(rpc) = rpc {
            cancel_native(&rpc, login_id, backend.as_ref()).await;
        }
        Ok(())
    }
    pub(crate) async fn cancel_active_login(&self, id: Uuid) {
        let login_id = self
            .logins
            .sessions
            .lock()
            .unwrap()
            .get(&id)
            .filter(|s| s.active())
            .and_then(|s| s.view["loginId"].as_str())
            .map(str::to_owned);
        if let Some(login_id) = login_id {
            let _ = self.cancel_login(id, &login_id).await;
        }
    }
    pub async fn import_token(&self, id: Uuid, token: String, account_id: String) -> Result<()> {
        self.require_source(id, "isolated").await?;
        self.require_no_login(id)?;
        if token.len() < 32
            || token.len() > 32768
            || token.chars().any(char::is_whitespace)
            || account_id.is_empty()
            || account_id.len() > 256
            || account_id.chars().any(char::is_control)
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "令牌或工作区账户 ID 格式无效。",
            ));
        }
        self.start(id).await?;
        let slot = self.slot(id).await?;
        let worker = slot.lock().await;
        let backend = worker
            .as_ref()
            .ok_or_else(|| Error::new("WORKER_STOPPED", "账户实例尚未启动。"))?;
        backend.rpc.request("account/login/start", json!({"type":"chatgptAuthTokens","accessToken":token,"chatgptAccountId":account_id})).await?;
        Ok(())
    }
}
fn prepare_session(result: &Value, kind: &str, rpc: Rpc) -> Result<Session> {
    let invalid = || Error::new("LOGIN_START_FAILED", "原生登录响应不完整，请重新发起。");
    let login_id = result["loginId"]
        .as_str()
        .filter(|v| Uuid::parse_str(v).is_ok())
        .ok_or_else(invalid)?;
    if result["type"] != kind {
        return Err(invalid());
    }
    let now = crate::now_ms();
    let mut view = json!({"type":kind,"loginId":login_id,"status":"waiting","startedAt":now,"expiresAt":now+TTL_MS,"error":null});
    let key = if kind == "chatgpt" {
        "authUrl"
    } else {
        "verificationUrl"
    };
    let url = result[key].as_str().ok_or_else(invalid)?;
    callback::auth_url(url)?;
    view[key] = url.into();
    let callback = if kind == "chatgpt" {
        callback::Callback::from_auth(url).ok()
    } else {
        None
    };
    view["manualCallbackAvailable"] = callback.is_some().into();
    if kind == "chatgpt" && callback.is_none() {
        view["manualCallbackReason"] =
            "无法识别当前原生授权链接的回调参数，请在原授权页面完成或改用设备码。".into();
    }
    if kind == "chatgptDeviceCode" {
        let code = result["userCode"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or_else(invalid)?;
        view["userCode"] = code.into();
    }
    Ok(Session {
        backend: None,
        view,
        rpc: Some(rpc),
        callback,
        cancel: CancellationToken::new(),
        submitted: false,
    })
}
async fn cancel_native(
    rpc: &Rpc,
    login_id: &str,
    backend: Option<&std::sync::Weak<crate::backend::Backend>>,
) {
    let result = rpc
        .request_timeout(
            "account/login/cancel",
            json!({"loginId":login_id}),
            Duration::from_secs(3),
        )
        .await;
    if result.is_err() {
        if let Some(backend) = backend.and_then(std::sync::Weak::upgrade) {
            let _ = backend.stop().await;
        }
    }
}
async fn verify(rpc: &Rpc, home: &std::path::Path) -> Result<()> {
    let account = rpc
        .request_timeout(
            "account/read",
            json!({"refreshToken":false}),
            Duration::from_secs(3),
        )
        .await?;
    if account["account"].is_null() {
        return Err(Error::new(
            "LOGIN_VERIFY_FAILED",
            "登录完成通知已收到，但账户不可读，请重新登录。",
        ));
    }
    let credential = oauth::parse(&oauth::load(&home.join("auth.json")).await?)?;
    if credential.refresh_token.is_none() {
        return Err(Error::new(
            "LOGIN_VERIFY_FAILED",
            "登录凭据保存不完整，请重新登录。",
        ));
    }
    // Confirm the process and saved file identify the same account.
    let status = rpc
        .request_timeout(
            "getAuthStatus",
            json!({"includeToken":true,"refreshToken":false}),
            Duration::from_secs(3),
        )
        .await?;
    let live = oauth::parse(&json!({"accessToken":status["authToken"]}))?;
    oauth::check_identity(&credential.identity, &live.identity)?;
    Ok(())
}
async fn monitor(
    sessions: Sessions,
    id: Uuid,
    login_id: String,
    rpc: Rpc,
    home: std::path::PathBuf,
    mut events: tokio::sync::broadcast::Receiver<Value>,
    cancel: CancellationToken,
) {
    let backend = sessions
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|s| s.backend.clone());
    let remaining = sessions
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|s| s.view["expiresAt"].as_u64())
        .unwrap_or(0)
        .saturating_sub(crate::now_ms())
        .min(TTL_MS);
    let timeout = tokio::time::sleep(Duration::from_millis(remaining));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = &mut timeout => {
                session::finish(&sessions, id, &login_id, "expired", Some(session::expired()));
                cancel_native(&rpc, &login_id, backend.as_ref()).await;
                break;
            }
            _ = rpc.closed() => {
                session::finish(&sessions, id, &login_id, "failed", Some(Error::new("LOGIN_DISCONNECTED", "原生登录进程已退出，请重新登录。")));
                break;
            }
            event = events.recv() => {
                let message = match event {
                    Ok(value) => value,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        session::finish(&sessions, id, &login_id, "failed", Some(Error::new("LOGIN_EVENTS_LOST", "登录通知中断，无法确认本次结果，请重新登录。")));
                        cancel_native(&rpc, &login_id, backend.as_ref()).await;
                        break;
                    }
                    Err(_) => {
                        session::finish(&sessions, id, &login_id, "failed", Some(Error::new("LOGIN_DISCONNECTED", "原生登录进程已退出，请重新登录。")));
                        break;
                    },
                };
                if message["method"] != "account/login/completed" || message["params"]["loginId"] != login_id { continue; }
                let success = message["params"]["success"] == true;
                if !success {
                    let (status, error) = native_failure(message["params"]["error"].as_str().unwrap_or_default());
                    session::finish(&sessions, id, &login_id, status, Some(error));
                    break;
                }
                let result = if success {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        result = tokio::time::timeout(Duration::from_secs(10), verify(&rpc, &home)) =>
                            result.unwrap_or_else(|_| Err(Error::new("LOGIN_VERIFY_FAILED", "登录凭据校验超时，请重新登录。"))),
                    }
                } else { Err(Error::new("LOGIN_AUTH_FAILED", "原生授权未完成，请检查授权页面、网络与账户权限后重新登录。")) };
                let error = result.err().map(|e| Error::new(
                    if success { "LOGIN_VERIFY_FAILED" } else { "LOGIN_AUTH_FAILED" },
                    if success { "登录完成后凭据校验失败，请重新登录。" } else { &e.message }));
                session::finish(&sessions, id, &login_id, if error.is_none() { "succeeded" } else { "failed" }, error);
                break;
            }
        }
    }
    // Drop native handles and sensitive pending notifications immediately.
    drop(events);
    drop(rpc);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        session::prune(&mut sessions.lock().unwrap());
    });
}
#[cfg(test)]
mod tests;

fn native_failure(message: &str) -> (&'static str, Error) {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("expired") {
        ("expired", session::expired())
    } else if lower.contains("cancel") {
        (
            "cancelled",
            Error::new("LOGIN_CANCELLED", "授权已取消，可重新发起登录。"),
        )
    } else if lower.contains("token exchange") {
        (
            "failed",
            Error::new(
                "LOGIN_EXCHANGE_FAILED",
                "授权码交换失败，请检查账户代理及网络后重新登录。",
            ),
        )
    } else if lower.contains("save") || lower.contains("persist") {
        (
            "failed",
            Error::new(
                "LOGIN_SAVE_FAILED",
                "登录凭据无法保存，请检查账户目录权限及磁盘空间。",
            ),
        )
    } else {
        (
            "failed",
            Error::new(
                "LOGIN_AUTH_FAILED",
                "原生授权未完成，请检查授权页面、网络与账户权限后重新登录。",
            ),
        )
    }
}
