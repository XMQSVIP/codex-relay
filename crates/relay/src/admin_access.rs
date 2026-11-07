use crate::{
    admin::AdminState,
    error::{Error, Result},
};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
};
use ring::{
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const ITERATIONS: u32 = 600_000;
const TTL: Duration = Duration::from_secs(12 * 3600);
const COOKIE: &str = "cb_admin_session";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PasswordHash {
    salt: [u8; 16],
    hash: [u8; 32],
    iterations: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Stored {
    password: Option<PasswordHash>,
    // Existing files without this field retain their previous local-only behavior.
    #[serde(default)]
    allow_public_access: bool,
}
impl Default for Stored {
    fn default() -> Self {
        Self {
            password: None,
            allow_public_access: true,
        }
    }
}

struct Inner {
    settings: Stored,
    generation: u64,
    sessions: HashMap<String, Instant>,
    attempts: VecDeque<Instant>,
}
pub(crate) struct Access {
    path: PathBuf,
    inner: Mutex<Inner>,
    mutation: tokio::sync::Mutex<()>,
    hashing: tokio::sync::Semaphore,
}
fn invalid_password() -> Error {
    Error::new("ADMIN_PASSWORD_INVALID", "后台密码不正确。")
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::new("ADMIN_AUTH", "无法创建安全会话。"))?;
    Ok(bytes)
}
fn session_key(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
fn token(headers: &HeaderMap) -> Option<&str> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        for pair in header.to_str().ok()?.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name == COOKIE {
                if found.is_some()
                    || value.len() != 64
                    || !value.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return None;
                }
                found = Some(value);
            }
        }
    }
    found
}
pub(crate) fn cookie(value: Option<&str>, headers: &HeaderMap) -> HeaderValue {
    let secure = headers
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v.starts_with("https://"));
    HeaderValue::from_str(&format!(
        "{COOKIE}={}; Path=/api; HttpOnly; SameSite=Strict; Max-Age={}{}",
        value.unwrap_or(""),
        if value.is_some() { TTL.as_secs() } else { 0 },
        if secure { "; Secure" } else { "" }
    ))
    .unwrap()
}
impl Access {
    pub(crate) async fn open(path: PathBuf) -> Result<Arc<Self>> {
        let settings: Stored = match tokio::fs::read(&path).await {
            Ok(bytes) if bytes.len() <= 4096 => serde_json::from_slice(&bytes)?,
            Ok(_) => return Err(Error::new("ADMIN_AUTH_CONFIG", "后台访问设置无效。")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::default(),
            Err(e) => return Err(e.into()),
        };
        if settings
            .password
            .as_ref()
            .is_some_and(|p| p.iterations != ITERATIONS)
        {
            return Err(Error::new("ADMIN_AUTH_CONFIG", "后台密码配置无效。"));
        }
        Ok(Arc::new(Self {
            path,
            inner: Mutex::new(Inner {
                settings,
                generation: 0,
                sessions: HashMap::new(),
                attempts: VecDeque::new(),
            }),
            mutation: tokio::sync::Mutex::new(()),
            hashing: tokio::sync::Semaphore::new(2),
        }))
    }
    pub(crate) fn public(&self) -> bool {
        self.inner.lock().unwrap().settings.allow_public_access
    }
    pub(crate) fn enabled(&self) -> bool {
        self.inner.lock().unwrap().settings.password.is_some()
    }
    pub(crate) fn authenticated(&self, headers: &HeaderMap) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.settings.password.is_none()
            || token(headers)
                .and_then(|t| inner.sessions.get(&session_key(t)))
                .is_some_and(|until| *until > Instant::now())
    }
    pub(crate) fn view(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"passwordEnabled":inner.settings.password.is_some(),"allowPublicAccess":inner.settings.allow_public_access})
    }
    fn issue(inner: &mut Inner) -> Result<String> {
        let value = hex::encode(random::<32>()?);
        let now = Instant::now();
        inner.sessions.retain(|_, expiry| *expiry > now);
        if inner.sessions.len() >= 128 {
            let oldest = inner
                .sessions
                .iter()
                .min_by_key(|(_, expiry)| *expiry)
                .map(|(key, _)| key.clone())
                .unwrap();
            inner.sessions.remove(&oldest);
        }
        inner.sessions.insert(session_key(&value), now + TTL);
        Ok(value)
    }
    async fn verify(&self, hash: PasswordHash, password: String) -> Result<()> {
        let _permit = self
            .hashing
            .acquire()
            .await
            .map_err(|_| invalid_password())?;
        tokio::task::spawn_blocking(move || {
            pbkdf2::verify(
                pbkdf2::PBKDF2_HMAC_SHA256,
                NonZeroU32::new(hash.iterations).unwrap(),
                &hash.salt,
                password.as_bytes(),
                &hash.hash,
            )
            .map_err(|_| invalid_password())
        })
        .await
        .map_err(|_| invalid_password())?
    }
    pub(crate) async fn login(&self, password: String) -> Result<Option<String>> {
        let (hash, generation) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(hash) = inner.settings.password.clone() else {
                return Ok(None);
            };
            let now = Instant::now();
            inner
                .attempts
                .retain(|at| now.duration_since(*at) < Duration::from_secs(60));
            if inner.attempts.len() >= 5 {
                return Err(Error::new(
                    "ADMIN_LOGIN_RATE_LIMIT",
                    "尝试次数过多，请 1 分钟后重试。",
                ));
            }
            inner.attempts.push_back(now);
            (hash, inner.generation)
        };
        self.verify(hash, password).await?;
        let mut inner = self.inner.lock().unwrap();
        if inner.generation != generation {
            return Err(Error::new(
                "ADMIN_LOGIN_REQUIRED",
                "登录设置已变更，请重新登录。",
            ));
        }
        inner.attempts.clear();
        Self::issue(&mut inner).map(Some)
    }
    pub(crate) fn logout(&self, headers: &HeaderMap) {
        if let Some(t) = token(headers) {
            self.inner.lock().unwrap().sessions.remove(&session_key(t));
        }
    }
    pub(crate) async fn update(&self, input: SettingsInput) -> Result<Option<String>> {
        let _mutation = self.mutation.lock().await;
        let previous = self.inner.lock().unwrap().settings.password.clone();
        if let Some(hash) = previous.clone() {
            self.verify(hash, input.current_password.unwrap_or_default())
                .await?;
        }
        let password = if input.password_enabled {
            if let Some(value) = input.password.filter(|v| !v.is_empty()) {
                let _permit = self
                    .hashing
                    .acquire()
                    .await
                    .map_err(|_| invalid_password())?;
                Some(
                    tokio::task::spawn_blocking(move || {
                        let salt = random::<16>()?;
                        let mut hash = [0; 32];
                        pbkdf2::derive(
                            pbkdf2::PBKDF2_HMAC_SHA256,
                            NonZeroU32::new(ITERATIONS).unwrap(),
                            &salt,
                            value.as_bytes(),
                            &mut hash,
                        );
                        Ok::<_, Error>(PasswordHash {
                            salt,
                            hash,
                            iterations: ITERATIONS,
                        })
                    })
                    .await
                    .map_err(|_| invalid_password())??,
                )
            } else {
                Some(previous.ok_or_else(|| Error::new("INVALID_INPUT", "请设置后台登录密码。"))?)
            }
        } else {
            None
        };
        let next = Stored {
            password,
            allow_public_access: input.allow_public_access,
        };
        let bytes = serde_json::to_vec(&next)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist(&path)
                .map_err(|_| Error::new("ADMIN_AUTH_STORAGE", "后台访问设置保存失败。"))?;
            Ok::<_, Error>(())
        })
        .await
        .map_err(|_| Error::new("ADMIN_AUTH_STORAGE", "后台访问设置保存失败。"))??;
        let mut inner = self.inner.lock().unwrap();
        inner.settings = next;
        inner.generation += 1;
        inner.sessions.clear();
        inner.attempts.clear();
        if inner.settings.password.is_some() {
            Self::issue(&mut inner).map(Some)
        } else {
            Ok(None)
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SettingsInput {
    password_enabled: bool,
    allow_public_access: bool,
    password: Option<String>,
    current_password: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginInput {
    password: String,
}
pub(crate) async fn settings(State(state): State<Arc<AdminState>>) -> Json<Value> {
    Json(state.access.view())
}
pub(crate) async fn update_settings(
    State(state): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(input): Json<SettingsInput>,
) -> Result<([(axum::http::HeaderName, HeaderValue); 1], Json<Value>)> {
    let access = state.access.clone();
    // Finish the atomic settings change even if the browser disconnects while
    // password derivation or the durable write is running.
    let value = state
        .tasks
        .spawn(async move { access.update(input).await })
        .await
        .map_err(|_| Error::new("ADMIN_AUTH_STORAGE", "后台访问设置保存失败。"))??;
    Ok((
        [(header::SET_COOKIE, cookie(value.as_deref(), &headers))],
        Json(state.access.view()),
    ))
}
pub(crate) async fn login(
    State(state): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Result<([(axum::http::HeaderName, HeaderValue); 1], Json<Value>)> {
    if !state.management_allows(&headers) {
        return Err(Error::new("ORIGIN_REJECTED", "请求来源不允许。"));
    }
    let value = state.access.login(input.password).await?;
    Ok((
        [(header::SET_COOKIE, cookie(value.as_deref(), &headers))],
        Json(json!({"authenticated":true})),
    ))
}
pub(crate) async fn logout(
    State(state): State<Arc<AdminState>>,
    request: Request,
) -> ([(axum::http::HeaderName, HeaderValue); 1], Json<Value>) {
    state.access.logout(request.headers());
    (
        [(header::SET_COOKIE, cookie(None, request.headers()))],
        Json(json!({"ok":true})),
    )
}

#[cfg(test)]
mod defaults_tests {
    use super::*;

    #[tokio::test]
    async fn new_settings_allow_external_access_and_saved_choices_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-access.json");
        assert!(Access::open(path.clone()).await.unwrap().public());
        for (settings, expected) in [
            (json!({"password":null,"allowPublicAccess":false}), false),
            (json!({"password":null,"allowPublicAccess":true}), true),
            (json!({"password":null}), false),
        ] {
            tokio::fs::write(&path, settings.to_string()).await.unwrap();
            assert_eq!(Access::open(path.clone()).await.unwrap().public(), expected);
        }
    }
}
