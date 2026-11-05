use super::*;
use crate::{
    error::Error,
    execution::ExecutionOutcome,
    operations::{AccountInput, ApiKeyCreate, ClientAccess, DEFAULT_GROUP, Lease, RoutingRequest},
    provider::Execution,
};
use serde_json::{Value, json};
use std::sync::Arc;

struct Fixture {
    _root: tempfile::TempDir,
    ops: Arc<Operations>,
    access: ClientAccess,
    a: Uuid,
    b: Uuid,
}
impl Fixture {
    fn new(execution: Execution, auth: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let ops = Operations::open(root.path()).unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        ops.register_accounts(&[(a, "a".into()), (b, "b".into())])
            .unwrap();
        let provider = Provider {
            execution,
            id: "openai".into(),
            model: "fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            auth: auth.into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        for id in [a, b] {
            ops.set_inference_route(id, revision(&ops), Some(provider.clone()))
                .unwrap();
            ops.set_account(
                id,
                AccountInput {
                    revision: revision(&ops),
                    group_id: DEFAULT_GROUP,
                    enabled: true,
                    max_concurrency: 3,
                },
            )
            .unwrap();
        }
        let key = ops
            .create_api_key(ApiKeyCreate {
                fast_mode: false,
                name: "rate-limit".into(),
                group_id: DEFAULT_GROUP,
                models: vec![],
                expires_in_days: None,
                max_concurrency: 0,
            })
            .unwrap();
        let access = ops.authenticate_api_key(&key.secret).unwrap();
        Self {
            _root: root,
            ops,
            access,
            a,
            b,
        }
    }
    async fn acquire(&self, account: Uuid) -> Lease {
        self.ops
            .acquire_inference_on(&self.access, request(), Some(account))
            .await
            .unwrap()
    }
    fn expire(&self) {
        self.ops
            .inner
            .lock()
            .unwrap()
            .runtime
            .rate_limits
            .get_mut(&self.a)
            .unwrap()
            .until = Instant::now() - Duration::from_secs(1);
    }
    fn health(&self) -> Value {
        let inner = self.ops.inner.lock().unwrap();
        runtime::health_view(
            &inner,
            inner
                .settings
                .accounts
                .iter()
                .find(|a| a.id == self.a)
                .unwrap(),
        )
    }
}
fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn request() -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: "fixture".into(),
        ..Default::default()
    }
}
fn limited(lease: Lease) {
    lease
        .finish_execution(
            &ExecutionOutcome::failed(crate::execution::native_failure(&json!({
                "codexErrorInfo":"rateLimitExceeded"
            }))),
            None,
        )
        .unwrap();
}

#[tokio::test]
async fn rate_limit_cools_only_the_login_account_and_routes_new_work_to_its_sibling() {
    let f = Fixture::new(Execution::Native, "native-chatgpt:fixture");
    limited(f.acquire(f.a).await);
    let health = f.health();
    assert_eq!(health["state"], "cooling");
    assert_eq!(health["reason"], "UPSTREAM_RATE_LIMITED");
    assert_eq!(health["rateLimitFailures"], 1);
    assert_eq!(health["sharedUpstream"], false);
    let remaining = health["retryAt"].as_u64().unwrap() - crate::now_ms();
    assert!((28_000..=30_000).contains(&remaining));
    let next = f.ops.acquire_inference(&f.access, request()).await.unwrap();
    assert_eq!(
        next.account_id,
        Some(f.b),
        "same provider URL must not cool other accounts"
    );
    next.complete(Ok(())).unwrap();
    assert_eq!(f.health()["state"], "cooling");
    assert_eq!(f.ops.snapshot()["active"], 0);
}

#[tokio::test]
async fn api_accounts_and_native_api_key_profiles_do_not_receive_this_policy() {
    for (execution, auth) in [
        (Execution::ApiKey, "api-key"),
        (Execution::ApiKey, "chatgpt"),
        (Execution::Native, "native-api-key"),
        (Execution::Native, "env:SYNTHETIC_KEY"),
        (Execution::Native, "native-chatgpt-invalid"),
    ] {
        let f = Fixture::new(execution, auth);
        limited(f.acquire(f.a).await);
        assert_eq!(f.health()["state"], "available", "{execution:?} {auth}");
        let one = f.acquire(f.a).await;
        let two = f.acquire(f.a).await;
        one.complete(Ok(())).unwrap();
        two.complete(Ok(())).unwrap();
        assert!(f.ops.inner.lock().unwrap().runtime.rate_limits.is_empty());
    }
}

#[tokio::test]
async fn concurrent_failures_share_a_window_and_late_success_cannot_clear_it() {
    let f = Fixture::new(Execution::Native, "chatgpt");
    let first = f.acquire(f.a).await;
    let second = f.acquire(f.a).await;
    let late_success = f.acquire(f.a).await;
    limited(first);
    limited(second);
    assert_eq!(f.health()["rateLimitFailures"], 1);
    f.expire();
    {
        let inner = f.ops.inner.lock().unwrap();
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == f.a)
            .unwrap();
        assert!(
            !available(&inner, account),
            "old requests must drain before a probe"
        );
    }
    late_success.complete(Ok(())).unwrap();
    assert_eq!(f.health()["state"], "halfOpen");
    let probe = f.acquire(f.a).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(40), f.acquire(f.a))
            .await
            .is_err()
    );
    assert_eq!(
        f.ops.snapshot()["queued"],
        0,
        "cancelled waiter must release its record"
    );
    limited(probe);
    assert_eq!(f.health()["rateLimitFailures"], 2);
    let remaining = f.health()["retryAt"].as_u64().unwrap() - crate::now_ms();
    assert!((58_000..=60_000).contains(&remaining));
}

#[tokio::test]
async fn recovery_backoff_is_capped_and_success_restores_parallel_capacity() {
    let f = Fixture::new(Execution::Native, "chatgpt");
    for seconds in [30, 60, 120, 240, 300, 300] {
        limited(f.acquire(f.a).await);
        let remaining = f.health()["retryAt"].as_u64().unwrap() - crate::now_ms();
        assert!((seconds * 1000 - 2000..=seconds * 1000).contains(&remaining));
        f.expire();
    }
    f.acquire(f.a).await.complete(Ok(())).unwrap();
    assert_eq!(f.health()["state"], "available");
    let one = f.acquire(f.a).await;
    let two = f.acquire(f.a).await;
    let three = f.acquire(f.a).await;
    assert_eq!(f.ops.snapshot()["active"], 3);
    one.complete(Ok(())).unwrap();
    two.complete(Ok(())).unwrap();
    limited(three);
    assert_eq!(
        f.health()["rateLimitFailures"],
        1,
        "successful recovery resets the exponent"
    );
}

#[tokio::test]
async fn queued_recovery_is_single_flight_and_dropped_probe_does_not_stick() {
    let f = Fixture::new(Execution::Native, "chatgpt");
    limited(f.acquire(f.a).await);
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let (ops, access, account) = (f.ops.clone(), f.access.clone(), f.a);
        jobs.spawn(async move {
            ops.acquire_inference_on(&access, request(), Some(account))
                .await
                .unwrap()
        });
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.ops.snapshot()["queued"] != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.expire();
    f.ops.changed.notify_waiters();
    let probe = tokio::time::timeout(Duration::from_secs(2), jobs.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.ops.snapshot()["active"], 1);
    assert_eq!(f.ops.snapshot()["queued"], 2);
    drop(probe);
    let next = tokio::time::timeout(Duration::from_secs(2), jobs.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.health()["state"], "halfOpen");
    next.complete(Ok(())).unwrap();
    jobs.join_next()
        .await
        .unwrap()
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(f.health()["state"], "available");
    assert_eq!(f.ops.snapshot()["active"], 0);
    assert_eq!(f.ops.snapshot()["queued"], 0);
}

#[tokio::test]
async fn stale_results_cannot_cool_a_changed_route_and_local_rejections_do_not_cool() {
    let f = Fixture::new(Execution::Native, "chatgpt");
    let old = f.acquire(f.a).await;
    // Simulate a late result from a replaced route without relying on maintenance timing.
    f.ops
        .inner
        .lock()
        .unwrap()
        .settings
        .accounts
        .iter_mut()
        .find(|a| a.id == f.a)
        .unwrap()
        .route
        .as_mut()
        .unwrap()
        .auth = "native-chatgpt:new-identity".into();
    limited(old);
    assert_eq!(f.health()["state"], "available");
    for code in [
        "QUEUE_FULL",
        "REQUEST_CANCELLED",
        "INVALID_INPUT",
        "UPSTREAM_QUOTA_EXHAUSTED",
    ] {
        let error = f
            .acquire(f.a)
            .await
            .complete::<()>(Err(Error::new(code, "fixture")))
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(f.health()["state"], "available");
    }
    f.acquire(f.a)
        .await
        .complete::<()>(Err(Error::new("UPSTREAM_RATE_LIMITED", "fixture")))
        .unwrap_err();
    assert_eq!(
        f.health()["state"],
        "cooling",
        "preparation errors also settle through the cooldown path"
    );
}

#[test]
fn native_rate_limit_classification_uses_only_structured_evidence() {
    for variant in [
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
        "responseTooManyFailedAttempts",
    ] {
        assert_eq!(
            crate::execution::native_failure(
                &json!({"codexErrorInfo":{variant:{"httpStatusCode":429}}})
            )
            .code,
            "UPSTREAM_RATE_LIMITED"
        );
    }
    for error in [
        json!({"message":"429 rate limit retry in 300 seconds"}),
        json!({"codexErrorInfo":{"other":{"httpStatusCode":429}}}),
        json!({"codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":500}}}),
    ] {
        assert_ne!(
            crate::execution::native_failure(&error).code,
            "UPSTREAM_RATE_LIMITED"
        );
    }
}
