//! Per-process authentication reuse; changes and expiry invalidate the cached result.
use super::Credential;
use crate::error::Result;
use std::{
    future::Future,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct AuthCache(Mutex<Option<Cached>>);
struct Cached {
    key: [u8; 32],
    result: Result<Credential>,
    checked: Instant,
}
impl Cached {
    fn valid(&self, key: &[u8; 32], bounded_age: bool) -> bool {
        if &self.key != key {
            return false;
        }
        let age = self.checked.elapsed();
        let Ok(credential) = &self.result else {
            return age < Duration::from_secs(1);
        };
        if bounded_age && age >= Duration::from_secs(30) {
            return false;
        }
        let now = crate::now_ms() / 1000;
        match credential.identity.expires_at {
            Some(expiry) => {
                expiry > now && (expiry > now.saturating_add(60) || age < Duration::from_secs(5))
            }
            None => age < Duration::from_secs(30),
        }
    }
}
impl AuthCache {
    pub(crate) async fn get_or_refresh(
        &self,
        key: [u8; 32],
        bounded_age: bool,
        refresh: impl Future<Output = Result<Credential>>,
    ) -> Result<Credential> {
        // Only authentication refresh is single-flight, never the whole request preparation.
        let mut cached = self.0.lock().await;
        if let Some(value) = cached
            .as_ref()
            .filter(|value| value.valid(&key, bounded_age))
        {
            return value.result.clone();
        }
        *cached = None;
        let result = refresh.await;
        *cached = Some(Cached {
            key,
            result: result.clone(),
            checked: Instant::now(),
        });
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn credential(expires: u64) -> Credential {
        let token = format!("e30.{}.signature", URL_SAFE_NO_PAD.encode(
            serde_json::json!({"exp":expires,"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic"}}).to_string()));
        crate::oauth::parse(&serde_json::json!({"accessToken":token})).unwrap()
    }
    #[tokio::test]
    async fn simultaneous_requests_share_one_authentication_and_refresh_after_change() {
        let cache = Arc::new(AuthCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let cache = cache.clone();
            let calls = calls.clone();
            tasks.spawn(async move {
                cache
                    .get_or_refresh([1; 32], false, async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Ok(credential(crate::now_ms() / 1000 + 3600))
                    })
                    .await
                    .unwrap()
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap().identity.account_id, "synthetic");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cache
            .get_or_refresh([2; 32], false, async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(credential(crate::now_ms() / 1000 + 3600))
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn expired_near_expiry_and_desktop_results_are_rechecked() {
        let cache = AuthCache::default();
        for (expiry, age, bounded) in [(3600, 31, true), (30, 6, false), (1, 0, false)] {
            cache
                .get_or_refresh([1; 32], false, async {
                    Ok(credential(crate::now_ms() / 1000 + 3600))
                })
                .await
                .unwrap();
            {
                let mut state = cache.0.lock().await;
                let entry = state.as_mut().unwrap();
                entry.checked = Instant::now() - Duration::from_secs(age);
                entry.result.as_mut().unwrap().identity.expires_at = Some(if expiry == 1 {
                    1
                } else {
                    crate::now_ms() / 1000 + expiry
                });
            }
            let called = AtomicUsize::new(0);
            cache
                .get_or_refresh([1; 32], bounded, async {
                    called.fetch_add(1, Ordering::SeqCst);
                    Ok(credential(crate::now_ms() / 1000 + 3600))
                })
                .await
                .unwrap();
            assert_eq!(called.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn failed_refresh_is_shared_briefly_and_new_credentials_can_retry() {
        let cache = AuthCache::default();
        let calls = AtomicUsize::new(0);
        for _ in 0..10 {
            let result = cache
                .get_or_refresh([1; 32], false, async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(Error::new("OAUTH_NOT_SIGNED_IN", "fixture"))
                })
                .await;
            assert!(matches!(result, Err(error) if error.code == "OAUTH_NOT_SIGNED_IN"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cache
            .get_or_refresh([2; 32], false, async {
                Ok(credential(crate::now_ms() / 1000 + 3600))
            })
            .await
            .unwrap();
    }
}
