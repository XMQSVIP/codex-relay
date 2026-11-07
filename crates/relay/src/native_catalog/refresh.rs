use super::{REFRESH_AGE_SECONDS, age, catalog, read_cache};
use crate::{
    app_server_proxy::ProxyConfig,
    backend::Backend,
    error::{Error, Result},
    installation,
    oauth::{self, Credential},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
];
// A longer refresh interval must not admit stale download results.
const MAX_REFRESHED_CACHE_AGE_SECONDS: i64 = 240;

#[derive(Default)]
struct RetryState {
    failures: usize,
    next_attempt: Option<Instant>,
}

impl RetryState {
    fn completed(&mut self, succeeded: bool, now: Instant) -> Option<Duration> {
        if succeeded {
            *self = Self::default();
            return None;
        }
        self.failures = self.failures.saturating_add(1).min(RETRY_DELAYS.len());
        let delay = RETRY_DELAYS[self.failures - 1];
        self.next_attempt = Some(now + delay);
        Some(delay)
    }
}

struct Attempt {
    retry: Arc<Mutex<RetryState>>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// Owned by Profiles so stop, credential changes, deletion and shutdown also
/// stop discovery. At most one refresh per account, and two across the bridge.
pub(crate) struct Refreshes {
    attempts: Mutex<HashMap<Uuid, Attempt>>,
    capacity: Arc<Semaphore>,
}

impl Default for Refreshes {
    fn default() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(2)),
        }
    }
}

impl Refreshes {
    pub async fn schedule(
        &self,
        id: Uuid,
        home: &Path,
        workspace: &Path,
        credential: Credential,
        proxy: ProxyConfig,
    ) {
        let now = chrono::Utc::now().timestamp();
        if read_cache(home)
            .await
            .and_then(|c| age(&c, now))
            .is_some_and(|age| (-300..REFRESH_AGE_SECONDS).contains(&age))
        {
            return;
        }
        let Some(scope) = Scope::read(home).await else {
            return;
        };
        if scope.config.contains("model_catalog_json") {
            return;
        }
        let mut attempts = self.attempts.lock().unwrap();
        let retry = match attempts.get(&id) {
            Some(attempt) => {
                if !attempt.task.is_finished()
                    || attempt
                        .retry
                        .lock()
                        .unwrap()
                        .next_attempt
                        .is_some_and(|next| Instant::now() < next)
                {
                    return;
                }
                attempt.retry.clone()
            }
            None => Arc::new(Mutex::new(RetryState::default())),
        };
        let home = home.to_path_buf();
        let workspace = workspace.to_path_buf();
        let capacity = self.capacity.clone();
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let task_retry = retry.clone();
        let task = tokio::spawn(async move {
            let permit = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                permit = capacity.acquire_owned() => match permit { Ok(p) => p, Err(_) => return },
            };
            let result = refresh(&home, &workspace, scope, credential, proxy, &shutdown).await;
            drop(permit);
            if shutdown.is_cancelled() {
                return;
            }
            // Backoff starts after the attempt finishes, including download and
            // cleanup. It gates future requests; no timer starts another fetch.
            let retry_delay = task_retry
                .lock()
                .unwrap()
                .completed(result.is_ok(), Instant::now());
            eprintln!(
                "{}",
                json!({"event":"native_catalog_refresh","accountId":id,
                "updated":result.is_ok(),"errorCode":result.err().map(|e|e.code),
                "retryAfterSeconds":retry_delay.map(|delay| delay.as_secs())})
            );
        });
        attempts.insert(
            id,
            Attempt {
                retry,
                cancel,
                task,
            },
        );
    }

    pub async fn stop(&self, id: Uuid) {
        let attempt = self.attempts.lock().unwrap().remove(&id);
        if let Some(attempt) = attempt {
            attempt.cancel.cancel();
            let _ = attempt.task.await;
        }
    }

    pub async fn shutdown(&self) {
        let attempts = std::mem::take(&mut *self.attempts.lock().unwrap());
        for attempt in attempts.values() {
            attempt.cancel.cancel();
        }
        for (_, attempt) in attempts {
            let _ = attempt.task.await;
        }
    }
}

impl Drop for Refreshes {
    fn drop(&mut self) {
        for (_, attempt) in self.attempts.get_mut().unwrap().drain() {
            attempt.cancel.cancel();
            attempt.task.abort();
        }
    }
}

#[derive(PartialEq)]
struct Scope {
    config: String,
    binding: Vec<u8>,
}

impl Scope {
    async fn read(home: &Path) -> Option<Self> {
        for name in ["config.toml", "bridge-auth.json"] {
            if tokio::fs::metadata(home.join(name)).await.ok()?.len() > 256 * 1024 {
                return None;
            }
        }
        Some(Self {
            config: tokio::fs::read_to_string(home.join("config.toml"))
                .await
                .ok()?,
            binding: tokio::fs::read(home.join("bridge-auth.json")).await.ok()?,
        })
    }

    async fn publish(self, home: &Path, cache: &Value, cancel: &CancellationToken) -> Result<()> {
        // Credential/config mutations stop and join this task before changing
        // the account. Also reject manual config edits or a removed account.
        if cancel.is_cancelled() || Scope::read(home).await.as_ref() != Some(&self) {
            return Err(Error::new(
                "CATALOG_SCOPE_CHANGED",
                "模型目录所属账户或配置已变化。",
            ));
        }
        // Atomic replacement preserves the native timestamp, identity and ETag.
        // This helper only writes a private JSON file; it performs no token exchange.
        oauth::save(&home.join("models_cache.json"), cache).await
    }
}

async fn refresh(
    home: &Path,
    workspace: &Path,
    scope: Scope,
    credential: Credential,
    proxy: ProxyConfig,
    cancel: &CancellationToken,
) -> Result<()> {
    let installation = installation::read_with_refresh().await?;
    let directory = tempfile::Builder::new()
        .prefix("catalog-refresh-")
        .tempdir_in(workspace)?;
    let isolated_home: PathBuf = directory.path().join("home");
    tokio::fs::create_dir(&isolated_home).await?;
    crate::platform::protect_directory(&isolated_home).await?;
    tokio::fs::write(isolated_home.join("config.toml"), &scope.config).await?;
    // A separate temporary home prevents racing native-managed token refresh.
    // Only the current validated access token is installed in process memory.
    let backend = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "模型目录刷新已取消。")),
        result = Backend::start_direct_with_proxy(&installation, directory.path(), Some(&isolated_home), false, &proxy) => result?,
    };
    let fetch = async {
        backend.install_external_credential(&credential).await?;
        oauth::provider(
            &backend,
            directory.path(),
            Some("catalog-refresh"),
            &credential.identity,
        )
        .await?;
        backend
            .rpc
            .request("model/list", json!({"limit":100,"includeHidden":true}))
            .await?;
        let cache = read_cache(&isolated_home)
            .await
            .ok_or_else(|| Error::new("CATALOG_NOT_REFRESHED", "原生模型目录下载未完成。"))?;
        let now = chrono::Utc::now().timestamp();
        let model = cache["models"]
            .as_array()
            .and_then(|m| m.first())
            .and_then(|m| m["slug"].as_str())
            .unwrap_or("");
        if age(&cache, now)
            .is_none_or(|age| !(-300..=MAX_REFRESHED_CACHE_AGE_SECONDS).contains(&age))
            || catalog(cache.clone(), &installation, model, now).is_none()
        {
            return Err(Error::new(
                "CATALOG_NOT_REFRESHED",
                "原生未保存有效的新模型目录。",
            ));
        }
        Ok(cache)
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::new("REQUEST_CANCELLED", "模型目录刷新已取消。")),
        result = tokio::time::timeout(Duration::from_secs(20), fetch) =>
            result.unwrap_or_else(|_| Err(Error::new("CATALOG_REFRESH_TIMEOUT", "后台模型目录刷新超时。"))),
    };
    let stopped = backend.stop().await;
    let cleaned = directory.close();
    let cache = result?;
    stopped?;
    cleaned?;
    scope.publish(home, &cache, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> Credential {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let claims = json!({"sub":"synthetic","exp":crate::now_ms()/1000+3600,
            "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic"}});
        oauth::parse(&json!({"accessToken":format!("e30.{}.signature", URL_SAFE_NO_PAD.encode(claims.to_string()))})).unwrap()
    }

    #[tokio::test]
    async fn refresh_is_single_flight_and_stop_cancels_waiting_work() {
        let home = tempfile::tempdir().unwrap();
        tokio::fs::write(home.path().join("config.toml"), "model='test'")
            .await
            .unwrap();
        tokio::fs::write(home.path().join("bridge-auth.json"), "{}")
            .await
            .unwrap();
        let refreshes = Refreshes::default();
        // Fill capacity so no child or network can be started by this test.
        let permits = refreshes
            .capacity
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        tokio::time::timeout(
            Duration::from_secs(1),
            refreshes.schedule(
                id,
                home.path(),
                home.path(),
                credential(),
                ProxyConfig::default(),
            ),
        )
        .await
        .unwrap();
        let first = refreshes.attempts.lock().unwrap()[&id].task.id();
        refreshes
            .schedule(
                id,
                home.path(),
                home.path(),
                credential(),
                ProxyConfig::default(),
            )
            .await;
        assert_eq!(refreshes.attempts.lock().unwrap()[&id].task.id(), first);
        tokio::time::timeout(Duration::from_secs(1), refreshes.stop(id))
            .await
            .unwrap();
        assert!(refreshes.attempts.lock().unwrap().is_empty());
        for _ in 0..2 {
            refreshes
                .schedule(
                    Uuid::new_v4(),
                    home.path(),
                    home.path(),
                    credential(),
                    ProxyConfig::default(),
                )
                .await;
        }
        tokio::time::timeout(Duration::from_secs(1), refreshes.shutdown())
            .await
            .unwrap();
        assert!(refreshes.attempts.lock().unwrap().is_empty());
        drop(permits);
        assert_eq!(refreshes.capacity.available_permits(), 2);
    }

    #[tokio::test]
    async fn publish_preserves_native_metadata_and_rejects_changed_scope() {
        let home = tempfile::tempdir().unwrap();
        tokio::fs::write(home.path().join("config.toml"), "model='test'")
            .await
            .unwrap();
        tokio::fs::write(home.path().join("bridge-auth.json"), "{\"generation\":1}")
            .await
            .unwrap();
        let cache = json!({"fetched_at":"2026-10-24T12:00:00Z","identity":"native-scope","etag":"native-tag","models":[]});
        Scope::read(home.path())
            .await
            .unwrap()
            .publish(home.path(), &cache, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(read_cache(home.path()).await.unwrap(), cache);
        let scope = Scope::read(home.path()).await.unwrap();
        tokio::fs::write(home.path().join("bridge-auth.json"), "{\"generation\":2}")
            .await
            .unwrap();
        assert!(
            scope
                .publish(home.path(), &json!({}), &CancellationToken::new())
                .await
                .is_err()
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            Scope::read(home.path())
                .await
                .unwrap()
                .publish(home.path(), &json!({}), &cancel)
                .await
                .is_err()
        );
        assert_eq!(read_cache(home.path()).await.unwrap(), cache);
    }
}
