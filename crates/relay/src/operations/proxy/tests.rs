use super::*;
use crate::{
    app_server_proxy::ProxyMode,
    operations::{AccountInput, DEFAULT_GROUP},
};
use std::{sync::Arc, time::Duration};

fn setup() -> (tempfile::TempDir, Arc<Operations>, Uuid, Uuid) {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "A".into()), (b, "B".into())])
        .unwrap();
    (dir, ops, a, b)
}
fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn config(port: u16) -> ProxyConfig {
    ProxyConfig {
        mode: ProxyMode::Custom,
        url: format!("http://127.0.0.1:{port}"),
    }
}

#[tokio::test]
async fn immediate_switch_does_not_wait_for_active_work_and_only_blocks_its_account() {
    let (_dir, ops, a, b) = setup();
    let active = ops.begin(Some(a), None, "inference", "test", true).unwrap();
    let generation = ops
        .set_account_proxy(a, revision(&ops), config(7897))
        .unwrap()
        .unwrap();
    assert_eq!(
        ops.snapshot()["accounts"][0]["proxyStatus"]["state"],
        "switching"
    );
    assert_eq!(ops.begin_proxy_switch(a, generation, true), Some(true));
    assert_eq!(ops.account_proxy(a).url, config(7897).url);
    let other = ops.begin(Some(b), None, "inference", "test", true).unwrap();
    let copy = ops.clone();
    let waiting =
        tokio::spawn(async move { copy.acquire(Some(a), None, "inference", "queued").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while ops.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    active.complete(Ok(())).unwrap();
    assert!(!waiting.is_finished());
    ops.finish_proxy_switch(a, generation, Ok(()));
    let next = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    next.complete(Ok(())).unwrap();
    other.complete(Ok(())).unwrap();
    assert!(ops.account_proxy(b).mode == ProxyMode::Auto);
}

#[test]
fn latest_configuration_wins_and_failure_stays_blocked_until_retry() {
    let (_dir, ops, a, _) = setup();
    let first = ops
        .set_account_proxy(a, revision(&ops), config(7897))
        .unwrap()
        .unwrap();
    let second = ops
        .set_account_proxy(a, revision(&ops), config(7898))
        .unwrap()
        .unwrap();
    assert!(!ops.proxy_current(a, first));
    ops.finish_proxy_switch(a, first, Ok(()));
    assert!(ops.proxy_blocked(a));
    assert_eq!(ops.begin_proxy_switch(a, second, true), Some(true));
    ops.finish_proxy_switch(a, second, Err(Error::new("TEST_FAILURE", "restart failed")));
    assert_eq!(
        ops.snapshot()["accounts"][0]["proxyStatus"]["state"],
        "failed"
    );
    assert_eq!(
        ops.begin(Some(a), None, "inference", "test", true)
            .err()
            .unwrap()
            .code,
        "CONCURRENCY_LIMIT"
    );
    let retry = ops
        .set_account_proxy(a, revision(&ops), config(7898))
        .unwrap()
        .unwrap();
    assert_eq!(ops.begin_proxy_switch(a, retry, false), Some(true));
    ops.finish_proxy_switch(a, retry, Ok(()));
    assert_eq!(ops.account_proxy(a).url, config(7898).url);
}

#[test]
fn disable_delete_stop_and_shutdown_prevent_stale_restarts() {
    let (_dir, ops, a, b) = setup();
    let generation = ops
        .set_account_proxy(a, revision(&ops), config(7897))
        .unwrap()
        .unwrap();
    ops.set_account(
        a,
        AccountInput {
            revision: revision(&ops),
            group_id: DEFAULT_GROUP,
            enabled: false,
            max_concurrency: 1,
        },
    )
    .unwrap();
    ops.set_account(
        a,
        AccountInput {
            revision: revision(&ops),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: 1,
        },
    )
    .unwrap();
    assert_eq!(ops.begin_proxy_switch(a, generation, true), Some(false));
    ops.finish_proxy_switch(a, generation, Ok(()));
    let generation = ops
        .set_account_proxy(a, revision(&ops), config(7898))
        .unwrap()
        .unwrap();
    ops.begin_account_delete(a, revision(&ops)).unwrap();
    assert!(!ops.proxy_current(a, generation));
    let generation = ops
        .set_account_proxy(b, revision(&ops), config(7899))
        .unwrap()
        .unwrap();
    ops.cancel_proxy_restart(b);
    assert_eq!(ops.begin_proxy_switch(b, generation, true), Some(false));
    ops.start_shutdown().unwrap();
    assert!(!ops.proxy_current(b, generation));
}

#[test]
fn account_configs_are_independent_persistent_and_default_to_auto() {
    let (dir, ops, a, b) = setup();
    assert!(ops.account_proxy(a).mode == ProxyMode::Auto);
    let generation = ops
        .set_account_proxy(a, revision(&ops), config(7897))
        .unwrap()
        .unwrap();
    assert_eq!(ops.begin_proxy_switch(a, generation, false), Some(false));
    ops.finish_proxy_switch(a, generation, Ok(()));
    let generation = ops
        .set_account_proxy(
            b,
            revision(&ops),
            ProxyConfig {
                mode: ProxyMode::Direct,
                url: "ignored".into(),
            },
        )
        .unwrap()
        .unwrap();
    ops.finish_proxy_switch(b, generation, Ok(()));
    drop(ops);
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(ops.account_proxy(a).url, config(7897).url);
    assert!(ops.account_proxy(b).mode == ProxyMode::Direct);
    assert!(ops.account_proxy(b).url.is_empty());
    assert!(ops.snapshot().get("appServerProxy").is_none());
}

#[tokio::test]
async fn queued_requests_keep_their_timeout_on_failed_switch() {
    let (_dir, ops, a, _) = setup();
    ops.set_queue(revision(&ops), 64, 1).unwrap();
    let generation = ops
        .set_account_proxy(a, revision(&ops), config(7897))
        .unwrap()
        .unwrap();
    ops.finish_proxy_switch(
        a,
        generation,
        Err(Error::new("TEST_FAILURE", "restart failed")),
    );
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        ops.acquire(Some(a), None, "inference", "queued"),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.code, "QUEUE_TIMEOUT");
    assert_eq!(ops.snapshot()["queued"], 0);
}

#[test]
fn native_route_prefers_an_account_without_a_pending_proxy_change() {
    let (_dir, ops, a, b) = setup();
    for id in [a, b] {
        ops.set_inference_route(
            id,
            revision(&ops),
            Some(crate::provider::Provider {
                execution: Default::default(),
                id: "fixture".into(),
                model: "m".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                auth: "none".into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            }),
        )
        .unwrap();
    }
    ops.set_account_proxy(a, revision(&ops), config(7897))
        .unwrap();
    assert_eq!(ops.native_inference_route(DEFAULT_GROUP, "m").unwrap().0, b);
    ops.set_account_proxy(b, revision(&ops), config(7898))
        .unwrap();
    assert!(ops.native_inference_route(DEFAULT_GROUP, "m").is_ok());
}
