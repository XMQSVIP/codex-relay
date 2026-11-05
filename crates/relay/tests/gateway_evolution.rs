use codex_relay::{
    error::Error,
    execution::ExecutionOutcome,
    operations::{ApiKeyCreate, DEFAULT_GROUP, Operations, RoutingRequest, StatsQuery},
    provider::Provider,
    streaming::{StreamSummary, Usage},
};
use serde_json::json;
use uuid::Uuid;
fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn provider() -> Provider {
    Provider {
        execution: Default::default(),
        id: "lan".into(),
        model: "m".into(),
        base_url: "http://192.168.1.2/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}
fn request() -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: "m".into(),
        streaming: true,
        reasoning_effort: None,
        user_agent: None,
        features: vec![],
        ..Default::default()
    }
}
fn summary() -> StreamSummary {
    StreamSummary {
        response_model: None,
        response_service_tier: None,
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        item_ids: vec![],
        outcome: "completed".into(),
        native_status: Some("completed".into()),
        error_code: None,
        error_message: None,
        delta_count: 1,
        text_bytes: 2,
        first_delta_at: Some(codex_relay::now_ms()),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            cached_input_tokens: None,
            reasoning_output_tokens: None,
            cache_write_input_tokens: None,
        }),
        interrupt_acknowledged: false,
        cleanup_confirmed: true,
        guard: Default::default(),
    }
}
fn key(ops: &Operations, name: &str) -> codex_relay::operations::ClientAccess {
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: name.into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap();
    ops.authenticate_api_key(&key.secret).unwrap()
}
#[test]
fn execution_preserves_model_failure_detail_and_cleanup_precedence() {
    let mut failure = summary();
    failure.outcome = "failed".into();
    failure.error_code = Some("NATIVE_TOOL_SCOPE".into());
    failure.error_message =
        Some("模型调用了未注册的客户端函数。（namespace=client, name=missing）".into());
    // Detailed delivery diagnostics are not serialized into usage/history metadata.
    assert!(
        serde_json::to_value(&failure)
            .unwrap()
            .get("errorMessage")
            .is_none()
    );
    let expected = failure.error_message.clone().unwrap();
    let outcome = ExecutionOutcome {
        model: Ok(failure),
        cleanup_error: Some(Error::new("CLEANUP_TEST", "cleanup failed")),
    };
    let error = outcome.error().unwrap();
    assert_eq!(error.code, "NATIVE_TOOL_SCOPE");
    assert_eq!(error.message, expected);
    let completed = ExecutionOutcome {
        model: Ok(summary()),
        cleanup_error: Some(Error::new("CLEANUP_TEST", "cleanup failed")),
    };
    assert_eq!(completed.error().unwrap().code, "CLEANUP_TEST");
}
#[tokio::test]
async fn dynamic_route_claim_uses_whichever_instance_becomes_available() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    ops.register_accounts(&[(a, "a".into()), (b, "b".into())])
        .unwrap();
    ops.set_inference_route(a, revision(&ops), Some(provider()))
        .unwrap();
    ops.set_inference_route(b, revision(&ops), Some(provider()))
        .unwrap();
    for id in [a, b] {
        ops.set_account(
            id,
            codex_relay::operations::AccountInput {
                revision: revision(&ops),
                group_id: DEFAULT_GROUP,
                enabled: true,
                max_concurrency: 1,
            },
        )
        .unwrap();
    }
    let access = key(&ops, "client");
    let first = ops.acquire_client(a, &access).await.unwrap();
    let second = ops.acquire_client(b, &access).await.unwrap();
    let cloned = ops.clone();
    let client = access.clone();
    let r = request();
    let id = r.id;
    let waiting = tokio::spawn(async move { cloned.acquire_inference(&client, r).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while ops.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the request must queue while both single-slot accounts are occupied");
    second.complete(Ok(())).unwrap();
    let lease = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, Some(b));
    assert_eq!(lease.request_id(), id);
    assert_eq!(lease.provider.as_ref().unwrap().id, "lan");
    first.complete(Ok(())).unwrap();
}
#[tokio::test]
async fn response_storage_failure_retains_model_usage_and_cleanup_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "a".into())]).unwrap();
    ops.set_inference_route(id, revision(&ops), Some(provider()))
        .unwrap();
    let access = key(&ops, "one");
    let other = key(&ops, "two");
    // Cache eviction is covered by storage tests. Inject a storage failure here
    // to verify that accounting retains the already completed model outcome.
    let storage_error = Error::new("RESPONSE_STORAGE_FULL", "storage budget exhausted");
    let lease = ops.acquire_inference(&access, request()).await.unwrap();
    lease
        .finish_execution(
            &ExecutionOutcome {
                model: Ok(summary()),
                cleanup_error: Some(Error::new("CLEANUP_TEST", "cleanup")),
            },
            Some(&storage_error),
        )
        .unwrap();
    let stats = ops
        .statistics(StatsQuery {
            key_id: Some(access.key_id),
            model: Some("m".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(stats["summary"]["failed"], 1);
    assert_eq!(stats["summary"]["inputTokens"], 10);
    let record = &stats["records"][0];
    assert_eq!(record["modelOutcome"], "completed");
    assert_eq!(record["failureStage"], "response_storage");
    assert_eq!(record["cleanupErrorCode"], "CLEANUP_TEST");
    assert_eq!(ops.client_usage(&access).unwrap()["summary"]["requests"], 1);
    assert_eq!(ops.client_usage(&other).unwrap()["summary"]["requests"], 0);
    assert_eq!(ops.snapshot()["active"], 0);
}
#[tokio::test]
async fn health_isolates_failed_instance_and_local_rejections_do_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    ops.register_accounts(&[(a, "a".into()), (b, "b".into())])
        .unwrap();
    for id in [a, b] {
        ops.set_inference_route(id, revision(&ops), Some(provider()))
            .unwrap();
    }
    ops.set_account(
        a,
        codex_relay::operations::AccountInput {
            revision: revision(&ops),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: 3,
        },
    )
    .unwrap();
    ops.set_account(
        b,
        codex_relay::operations::AccountInput {
            revision: revision(&ops),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: 1,
        },
    )
    .unwrap();
    for _ in 0..3 {
        ops.record_native_health(a, &Err(Error::new("RPC_CLOSED", "closed")));
        ops.record_native_health(b, &Err(Error::new("QUEUE_FULL", "full")));
    }
    let lease = ops
        .acquire_inference(&key(&ops, "one"), request())
        .await
        .unwrap();
    assert_eq!(lease.account_id, Some(b));
    lease.complete(Ok(())).unwrap();
    assert_eq!(
        ops.capabilities()["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == b.to_string())
            .unwrap()["health"]["consecutiveFailures"],
        0
    );
    // Keep the healthy instance occupied: after cooldown, exactly one request
    // may probe the failed instance; the next caller must remain queued.
    let access = key(&ops, "recovery");
    let healthy = ops.acquire_client(b, &access).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30_050)).await;
    let recovery = ops.acquire_inference(&access, request()).await.unwrap();
    assert_eq!(recovery.account_id, Some(a));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            ops.acquire_inference(&access, request())
        )
        .await
        .is_err()
    );
    assert_eq!(ops.snapshot()["queued"], 0);
    ops.record_native_health(a, &Ok(summary()));
    recovery.complete(Ok(())).unwrap();
    healthy.complete(Ok(())).unwrap();
    let view = ops.capabilities();
    let recovered = view["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == a.to_string())
        .unwrap();
    assert_eq!(recovered["health"]["state"], "available");
    assert_eq!(recovered["health"]["consecutiveFailures"], 0);
}
#[test]
fn fresh_database_does_not_create_audit_storage() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 5).unwrap();
    key(&ops, "test");
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'audit%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn retired_audit_layout_is_rejected_without_deleting_data() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.set_global(revision(&ops), 5).unwrap();
    drop(ops);
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE audit(id INTEGER PRIMARY KEY,action TEXT NOT NULL); INSERT INTO audit(action) VALUES('preserved');").unwrap();
    assert_eq!(
        Operations::open(dir.path()).err().unwrap().code,
        "DATABASE_SCHEMA"
    );
    assert_eq!(
        db.query_row("SELECT action FROM audit", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "preserved"
    );
}

#[test]
fn namespace_and_inline_media_preserve_structure_and_reject_host_paths() {
    let tools = json!([{"type":"namespace","name":"client","description":"client tools","tools":[{"type":"function","name":"echo","parameters":{"type":"object"}}]}]);
    let parsed = codex_relay::protocol_tools::parse(&tools).unwrap();
    let native = codex_relay::protocol_tools::native(&parsed);
    assert_eq!(native[0]["tools"][0]["name"], "echo");
    assert!(codex_relay::protocol_tools::allows(
        &parsed,
        Some("client"),
        "echo"
    ));
    assert!(!codex_relay::protocol_tools::allows(&parsed, None, "echo"));
    assert_eq!(
        codex_relay::protocol_tools::from_native(&native).unwrap(),
        parsed
    );
    assert!(codex_relay::media::inline_image("file:///C:/secret.png").is_err());
    assert!(codex_relay::media::inline_image("data:image/png;base64,iVBORw0KGgo=").is_err());
    assert!(codex_relay::media::inline_image("https://example.com/a.png").is_err());
    let request=codex_relay::responses::Request::parse(json!({"model":"m","input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC"}]}],"tools":tools})).unwrap();
    assert_eq!(request.required_features(&request.input).unwrap().len(), 2);
}

#[test]
fn catalogs_remain_diagnostic_without_gating_automatic_access() {
    use codex_relay::operations::{Feature, NativeCapabilities};
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let id = Uuid::new_v4();
    let p = provider();
    ops.register_accounts(&[(id, "a".into())]).unwrap();
    ops.set_inference_route(id, revision(&ops), Some(p.clone()))
        .unwrap();
    let caps = NativeCapabilities {
        installation: "build-a".into(),
        provider: codex_relay::operations::runtime::fingerprint(&p),
        checked_at: codex_relay::now_ms(),
        models: vec![json!({"id":"m","inputModalities":["text","image"]})],
        web_search: Some(true),
        ..Default::default()
    };
    ops.publish_capabilities(id, caps.clone()).unwrap();
    ops.set_disabled_features(
        id,
        revision(&ops),
        vec![Feature::WebSearch, Feature::ImageGeneration],
    )
    .unwrap();
    ops.record_feature_verification(id, &p, "build-a", Feature::ImageInput, None)
        .unwrap();
    ops.set_disabled_features(
        id,
        revision(&ops),
        vec![Feature::WebSearch, Feature::ImageGeneration],
    )
    .unwrap();
    let mut expired = caps.clone();
    expired.checked_at = codex_relay::now_ms().saturating_sub(3_600_001);
    ops.publish_capabilities(id, expired).unwrap();
    assert_eq!(
        ops.capabilities()["accounts"][0]["capabilities"]["stale"],
        true
    );
    let snapshot = ops.snapshot();
    assert_eq!(
        snapshot["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id.to_string())
            .unwrap()["capabilities"]["stale"],
        true
    );
    ops.set_disabled_features(
        id,
        revision(&ops),
        vec![Feature::WebSearch, Feature::ImageGeneration],
    )
    .unwrap();
    ops.publish_capabilities(id, caps.clone()).unwrap();
    let mut failure = caps;
    failure.models.clear();
    failure.stale = true;
    failure.error_code = Some("RPC_REMOTE".into());
    ops.publish_capabilities(id, failure).unwrap();
    let view = ops.capabilities();
    assert_eq!(view["accounts"][0]["capabilities"]["models"][0]["id"], "m");
    assert_eq!(view["accounts"][0]["capabilities"]["stale"], true);
    drop(ops);
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(
        ops.capabilities()["accounts"][0]["capabilities"]["stale"],
        true
    );
}
#[tokio::test]
async fn ingress_ids_usage_and_admin_visibility_are_scoped() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let state =
        codex_relay::admin::AdminState::new(4317, dir.path().join("accounts"), "a".repeat(64))
            .await
            .unwrap();
    let secret = state
        .operations
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "client".into(),
            group_id: DEFAULT_GROUP,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap()
        .secret;
    let app = codex_relay::admin::router(state, dir.path().join("web"));
    for (path, token, expected) in [
        ("/v1/usage", "bad", 401),
        ("/v1/usage", secret.as_str(), 200),
        ("/api/audit", secret.as_str(), 404),
        ("/api/audit", &"a".repeat(64), 404),
        ("/api/capabilities", secret.as_str(), 403),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        if path.starts_with("/v1") {
            assert!(Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).is_ok());
        }
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains(&secret));
    }
}
