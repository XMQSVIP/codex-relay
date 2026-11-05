use codex_relay::{
    admin::AdminState,
    error::Error,
    guard::Stats,
    inference::{self, TextRequest},
    operations::{AccountInput, ApiKeyCreate, ApiKeyUpdate, DEFAULT_GROUP, GroupInput, Operations},
    provider::Provider,
    streaming::{StreamSummary, Usage},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

fn provider() -> Provider {
    Provider {
        execution: Default::default(),
        id: "lan_test".into(),
        model: "model-a".into(),
        base_url: "http://192.168.1.10/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}

#[test]
fn non_default_models_keep_key_scope_and_provider_change_checks() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "multi model".into())])
        .unwrap();
    ops.set_inference_route(id, revision(&ops), Some(provider()))
        .unwrap();
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "test".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["model-b".into()],
            expires_in_days: None,
            max_concurrency: 1,
        })
        .unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let (selected, configured) = ops.inference_route(DEFAULT_GROUP, "model-b").unwrap();
    assert_eq!(selected, id);
    assert_eq!(configured.model, "model-a"); // Configuration is never rewritten per request.
    assert!(
        ops.check_inference_route(id, &access, &configured, "model-b")
            .is_ok()
    );
    assert!(
        ops.check_inference_route(id, &access, &configured, "model-c")
            .is_err()
    );
    assert_eq!(ops.inference_models(Some(&access)), ["model-b"]);
    assert!(ops.inference_route(DEFAULT_GROUP, "bad\nmodel").is_err());
    let mut changed = provider();
    changed.base_url = "http://192.168.1.11/v1".into();
    ops.set_inference_route(id, revision(&ops), Some(changed))
        .unwrap();
    assert_eq!(
        ops.check_inference_route(id, &access, &configured, "model-b")
            .unwrap_err()
            .code,
        "ROUTE_CHANGED"
    );
}

#[tokio::test]
async fn internal_shared_route_preferences_do_not_survive_as_client_accounts() {
    use codex_relay::operations::DESKTOP_ACCOUNT;
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let ops = &state.operations;
    ops.sync_desktop_route(Some(provider())).unwrap();
    let version = revision(ops);
    ops.sync_desktop_route(Some(provider())).unwrap();
    assert_eq!(version, revision(ops));
    assert_eq!(
        ops.inference_route(DEFAULT_GROUP, "model-b").unwrap().0,
        DESKTOP_ACCOUNT
    );
    assert!(
        ops.begin_account_delete(DESKTOP_ACCOUNT, revision(ops))
            .is_err()
    );
    assert!(
        ops.set_inference_route(DESKTOP_ACCOUNT, revision(ops), None)
            .is_err()
    );
    let group = ops
        .save_group(
            None,
            GroupInput {
                revision: revision(ops),
                name: "shared".into(),
                enabled: true,
            },
        )
        .unwrap();
    ops.set_account(
        DESKTOP_ACCOUNT,
        AccountInput {
            revision: revision(ops),
            group_id: group,
            enabled: false,
            max_concurrency: 1,
        },
    )
    .unwrap();
    ops.sync_desktop_route(None).unwrap();
    ops.sync_desktop_route(Some(provider())).unwrap();
    assert!(ops.inference_route(group, "model-b").is_err());
    assert_eq!(
        ops.snapshot()["accounts"][0]["groupIds"],
        serde_json::json!([group])
    );
    drop(state);
    let reopened = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    assert!(
        reopened.operations.snapshot()["accounts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn desktop_shared_route_is_not_dispatched_for_api_requests() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    state
        .operations
        .sync_desktop_route(Some(provider()))
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "shared".into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 1,
        })
        .unwrap();
    let access = state.operations.authenticate_api_key(&key.secret).unwrap();
    let error = match inference::start(state.clone(), access, &request()).await {
        Ok(_) => panic!("API requests must use a dedicated native account"),
        Err(error) => error,
    };
    assert_eq!(error.code, "INFERENCE_DISABLED");
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.profiles.list().await.is_empty());
}
fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn request() -> TextRequest {
    serde_json::from_value(
        json!({"model":"model-a","input":"local test","stream":true,"store":false}),
    )
    .unwrap()
}
async fn setup() -> (tempfile::TempDir, Arc<AdminState>, Uuid, String) {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let account = state.profiles.create("isolated test").await.unwrap();
    state
        .operations
        .register_accounts(&[(account.id, account.name)])
        .unwrap();
    state
        .operations
        .set_inference_route(account.id, revision(&state.operations), Some(provider()))
        .unwrap();
    let key = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "test".into(),
            group_id: DEFAULT_GROUP,
            models: vec!["model-a".into()],
            expires_in_days: Some(1),
            max_concurrency: 0,
        })
        .unwrap();
    (temp, state, account.id, key.secret)
}
#[test]
fn only_supported_single_turn_text_inputs_are_accepted() {
    assert_eq!(request().prompt().unwrap(), "local test");
    for input in [
        json!(""),
        json!([]),
        json!({"role":"user"}),
        json!("界".repeat(6000)),
    ] {
        let value = serde_json::from_value::<TextRequest>(json!({"model":"model-a","input":input}))
            .unwrap();
        assert!(value.prompt().is_err());
    }
    assert!(
        serde_json::from_value::<TextRequest>(json!({"model":"a","input":"x","tools":[]}))
            .unwrap()
            .client_payload()
            .unwrap()
            .is_some()
    );
    let value =
        serde_json::from_value::<TextRequest>(json!({"model":"a","input":"x","store":true}))
            .unwrap();
    assert!(value.prompt().is_err());
}
#[tokio::test]
async fn revoke_or_disable_while_queued_prevents_dispatch_and_releases_capacity() {
    for revoke in [true, false] {
        let (_temp, state, account, key) = setup().await;
        let access = state.operations.authenticate_api_key(&key).unwrap();
        let key_id = access.key_id;
        let block = state
            .operations
            .acquire(Some(account), None, "native_check", "block")
            .await
            .unwrap();
        let queued_state = state.clone();
        let pending =
            tokio::spawn(async move { inference::start(queued_state, access, &request()).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.operations.snapshot()["queued"] != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if revoke {
            state
                .operations
                .update_api_key(
                    key_id,
                    ApiKeyUpdate {
                        revision: 1,
                        state: Some("revoked".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        } else {
            state
                .operations
                .set_inference_route(account, revision(&state.operations), None)
                .unwrap();
        }
        block.complete(Ok(())).unwrap();
        let error = match pending.await.unwrap() {
            Ok(_) => panic!("request should not dispatch"),
            Err(e) => e,
        };
        assert_eq!(
            error.code,
            if revoke {
                "INVALID_API_KEY"
            } else {
                "INFERENCE_DISABLED"
            }
        );
        assert_eq!(state.operations.snapshot()["active"], 0);
        assert_eq!(state.operations.snapshot()["queued"], 0);
        assert!(state.profiles.list().await.iter().all(|a| a.pid.is_none()));
    }
}
#[tokio::test]
async fn queued_http_task_drop_cancels_admission_without_starting_a_backend() {
    let (_temp, state, account, key) = setup().await;
    let access = state.operations.authenticate_api_key(&key).unwrap();
    let block = state
        .operations
        .acquire(Some(account), None, "native_check", "block")
        .await
        .unwrap();
    let queued_state = state.clone();
    let pending =
        tokio::spawn(async move { inference::start(queued_state, access, &request()).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.operations.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    assert_eq!(state.operations.snapshot()["queued"], 0);
    block.complete(Ok(())).unwrap();
    assert_eq!(state.operations.snapshot()["active"], 0);
}
#[tokio::test]
async fn unsupported_credential_source_emits_error_and_releases_owned_lease() {
    let (_temp, state, _account, key) = setup().await;
    let access = state.operations.authenticate_api_key(&key).unwrap();
    let (mut rx, _guard, _) = inference::start(state.clone(), access, &request())
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event["type"], "error");
    assert_eq!(event["code"], "INVALID_INPUT");
    assert!(rx.recv().await.is_none());
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(state.profiles.list().await.iter().all(|a| a.pid.is_none()));
}

#[tokio::test(flavor = "current_thread")]
async fn stopping_before_spawned_task_runs_cancels_without_starting_a_backend() {
    let (_temp, state, account, key) = setup().await;
    let access = state.operations.authenticate_api_key(&key).unwrap();
    let (mut rx, _guard, _) = inference::start(state.clone(), access, &request())
        .await
        .unwrap();
    state.profiles.stop(account).await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event["code"], "STREAM_CANCELLED");
    assert_eq!(state.operations.snapshot()["active"], 0);
    assert!(
        state
            .profiles
            .list()
            .await
            .iter()
            .all(|a| a.state == "stopped" && a.pid.is_none())
    );
}
#[tokio::test]
async fn source_is_persisted_and_native_config_cannot_change_desktop_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = codex_relay::profiles::Profiles::open(temp.path().join("accounts"))
        .await
        .unwrap();
    let p = profiles.create_native_config("local native").await.unwrap();
    assert_eq!(
        profiles.login(p.id, "chatgpt").await.unwrap_err().code,
        "INVALID_INPUT"
    );
    assert_eq!(
        profiles
            .import_token(p.id, "x".repeat(64), "workspace".into())
            .await
            .unwrap_err()
            .code,
        "INVALID_INPUT"
    );
    drop(profiles);
    let profiles = codex_relay::profiles::Profiles::open(temp.path().join("accounts"))
        .await
        .unwrap();
    assert_eq!(profiles.list().await[0].profile.source, "native-config");
}
#[test]
fn model_routes_are_scoped_and_account_movement_never_crosses_authorized_group() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let account = Uuid::new_v4();
    ops.register_accounts(&[(account, "a".into())]).unwrap();
    ops.set_inference_route(account, revision(&ops), Some(provider()))
        .unwrap();
    assert_eq!(ops.inference_models(None), ["model-a"]);
    assert!(
        ops.set_account(
            account,
            AccountInput {
                revision: revision(&ops),
                group_id: DEFAULT_GROUP,
                enabled: true,
                max_concurrency: 2
            }
        )
        .is_ok()
    );
    let other = ops
        .save_group(
            None,
            GroupInput {
                revision: revision(&ops),
                name: "other".into(),
                enabled: true,
            },
        )
        .unwrap();
    ops.set_account(
        account,
        AccountInput {
            revision: revision(&ops),
            group_id: other,
            enabled: true,
            max_concurrency: 1,
        },
    )
    .unwrap();
    assert!(ops.inference_route(DEFAULT_GROUP, "model-a").is_err());
    assert!(
        ops.begin(
            Some(account),
            Some(DEFAULT_GROUP),
            "inference",
            "responses",
            true
        )
        .is_err()
    );
    assert_eq!(ops.inference_route(other, "model-a").unwrap().0, account);
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.inference_route(other, "model-a").unwrap().1, provider());
}
#[test]
fn inference_usage_is_persisted_once_and_missing_usage_remains_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let lease = ops
        .begin(None, None, "inference", "responses", true)
        .unwrap();
    let id = lease.request_id();
    let key = Uuid::new_v4();
    lease.bind_client(key).unwrap();
    let summary = StreamSummary {
        response_model: None,
        response_service_tier: None,
        thread_id: "native-thread".into(),
        turn_id: "native-turn".into(),
        item_ids: vec![],
        outcome: "completed".into(),
        native_status: Some("completed".into()),
        error_code: None,
        error_message: None,
        delta_count: 1,
        text_bytes: 3,
        first_delta_at: Some(1),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 4,
            total_tokens: 14,
            cached_input_tokens: None,
            reasoning_output_tokens: None,
            cache_write_input_tokens: None,
        }),
        interrupt_acknowledged: false,
        cleanup_confirmed: true,
        guard: Stats::default(),
    };
    lease.finish_inference(&Ok(summary)).unwrap();
    ops.begin(None, None, "inference", "responses", true)
        .unwrap()
        .finish_inference(&Err(Error::new("STREAM_CANCELLED", "cancel")))
        .unwrap();
    let stats = ops
        .statistics(serde_json::from_value(json!({"kind":"inference"})).unwrap())
        .unwrap();
    assert_eq!(stats["summary"]["inputTokens"], 10);
    assert_eq!(stats["summary"]["outputTokens"], 4);
    assert_eq!(stats["summary"]["usageKnownRequests"], 1);
    assert_eq!(stats["summary"]["cancelled"], 1);
    let db = rusqlite::Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let recorded: (String, String) = db
        .query_row(
            "SELECT key_id,native_turn_id FROM requests WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(recorded, (key.to_string(), "native-turn".into()));
}
