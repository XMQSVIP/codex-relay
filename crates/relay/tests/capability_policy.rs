use codex_relay::{
    operations::{
        ApiKeyCreate, ClientAccess, DEFAULT_GROUP, Feature, NativeCapabilities, Operations,
        RoutingRequest,
    },
    provider::Provider,
};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn provider() -> Provider {
    Provider {
        execution: Default::default(),
        id: "synthetic".into(),
        model: "gpt-6-astra".into(),
        base_url: "http://192.0.2.1/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    }
}
fn access(ops: &Operations, group: Uuid) -> ClientAccess {
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: "isolated policy fixture".into(),
            group_id: group,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap();
    ops.authenticate_api_key(&key.secret).unwrap()
}
fn request(model: &str, features: Vec<Feature>) -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: model.into(),
        streaming: true,
        reasoning_effort: None,
        user_agent: None,
        features,
        ..Default::default()
    }
}
fn all() -> Vec<Feature> {
    vec![
        Feature::NamespacedTools,
        Feature::ImageInput,
        Feature::WebSearch,
        Feature::ImageGeneration,
    ]
}
fn setup() -> (tempfile::TempDir, Arc<Operations>, Uuid, ClientAccess) {
    let root = tempfile::tempdir().unwrap();
    let ops = Operations::open(root.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "automatic route".into())])
        .unwrap();
    ops.set_inference_route(id, revision(&ops), Some(provider()))
        .unwrap();
    let key = access(&ops, DEFAULT_GROUP);
    (root, ops, id, key)
}
fn caps(search: bool) -> NativeCapabilities {
    NativeCapabilities {
        installation: "build-a".into(),
        provider: codex_relay::operations::runtime::fingerprint(&provider()),
        checked_at: codex_relay::now_ms(),
        web_search: Some(search),
        ..Default::default()
    }
}

#[tokio::test]
async fn automatic_capabilities_need_no_catalog_probe_enablement_or_model_binding() {
    let (_root, ops, id, key) = setup();
    for model in ["gpt-6-astra", "gpt-6-luna"] {
        let lease = ops
            .acquire_inference(&key, request(model, all()))
            .await
            .unwrap();
        assert_eq!(lease.account_id, Some(id));
        lease.complete(Ok(())).unwrap();
    }
    ops.check_capability_generation(id, "new-binary", &all())
        .unwrap();
    assert!(ops.capabilities()["accounts"][0]["capabilities"].is_null());
    assert_eq!(ops.capabilities()["automaticCapabilities"], true);
}

#[tokio::test]
async fn stale_unknown_and_restarted_catalogs_do_not_revoke_automatic_access() {
    let (root, ops, id, key) = setup();
    let mut old = caps(false);
    old.checked_at = 1;
    old.stale = true;
    ops.publish_capabilities(id, old).unwrap();
    ops.acquire_inference(&key, request("gpt-6-luna", all()))
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    ops.check_capability_generation(id, "build-b", &all())
        .unwrap();
    drop(ops);
    let ops = Operations::open(root.path()).unwrap();
    ops.acquire_inference(&key, request("gpt-6-astra", all()))
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(ops.snapshot()["accounts"][0]["disabledFeatures"], json!([]));
}

#[tokio::test]
async fn explicit_disables_survive_refresh_and_restart_and_errors_identify_the_feature() {
    let (root, ops, id, key) = setup();
    ops.set_disabled_features(id, revision(&ops), vec![Feature::ImageInput])
        .unwrap();
    let error = ops
        .acquire_inference(&key, request("gpt-6-astra", all()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
    assert!(error.message.contains("imageInput") && error.message.contains("手动"));
    assert!(!error.message.contains("namespacedTools"));
    ops.acquire_inference(&key, request("gpt-6-luna", vec![Feature::NamespacedTools]))
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    ops.publish_capabilities(id, caps(true)).unwrap();
    drop(ops);
    let ops = Operations::open(root.path()).unwrap();
    assert!(
        ops.acquire_inference(&key, request("gpt-6-astra", all()))
            .await
            .is_err()
    );
    assert!(
        ops.set_disabled_features(id, revision(&ops), vec![Feature::NamespacedTools])
            .is_err()
    );
    ops.set_disabled_features(id, revision(&ops), vec![])
        .unwrap();
    ops.acquire_inference(&key, request("gpt-6-astra", all()))
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
}

#[tokio::test]
async fn catalog_support_is_a_preference_with_a_native_fallback_not_a_hard_gate() {
    let (_root, ops, first, key) = setup();
    let second = Uuid::new_v4();
    ops.register_accounts(&[(second, "supported route".into())])
        .unwrap();
    ops.set_inference_route(second, revision(&ops), Some(provider()))
        .unwrap();
    // The supported route must be full before the next request can fall back.
    ops.set_account(
        second,
        codex_relay::operations::AccountInput {
            revision: revision(&ops),
            group_id: DEFAULT_GROUP,
            enabled: true,
            max_concurrency: 1,
        },
    )
    .unwrap();
    ops.publish_capabilities(first, caps(false)).unwrap();
    ops.publish_capabilities(second, caps(true)).unwrap();
    let supported = ops
        .acquire_inference(&key, request("gpt-6-astra", vec![Feature::WebSearch]))
        .await
        .unwrap();
    assert_eq!(supported.account_id, Some(second));
    let fallback = ops
        .acquire_inference(&key, request("gpt-6-astra", vec![Feature::WebSearch]))
        .await
        .unwrap();
    assert_eq!(fallback.account_id, Some(first));
    fallback.complete(Ok(())).unwrap();
    supported.complete(Ok(())).unwrap();
}

#[tokio::test]
async fn missing_routes_are_reported_as_setup_errors_not_unverified_capabilities() {
    let root = tempfile::tempdir().unwrap();
    let ops = Operations::open(root.path()).unwrap();
    let key = access(&ops, DEFAULT_GROUP);
    let error = ops
        .acquire_inference(&key, request("gpt-6-astra", all()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "INFERENCE_DISABLED");
}

#[tokio::test]
async fn capability_admin_api_preserves_explicit_policy_and_revision_without_probes() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let root = tempfile::tempdir().unwrap();
    let token = "a".repeat(64);
    let state =
        codex_relay::admin::AdminState::new(4317, root.path().join("accounts"), token.clone())
            .await
            .unwrap();
    let ops = state.operations.clone();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "no probe fixture".into())])
        .unwrap();
    ops.set_inference_route(id, revision(&ops), Some(provider()))
        .unwrap();
    let initial = revision(&ops);
    let app = codex_relay::admin::router(state, root.path().join("web"));
    for (body, status, disabled) in [
        (
            json!({"revision":initial,"disabledFeatures":["imageInput"]}),
            200,
            vec!["imageInput"],
        ),
        (
            json!({"revision":initial,"disabledFeatures":[]}),
            409,
            vec!["imageInput"],
        ),
        (
            json!({"revision":initial+1,"disabledFeatures":[],"features":[]}),
            422,
            vec!["imageInput"],
        ),
        (json!({"revision":initial+1}), 422, vec!["imageInput"]),
        (
            json!({"revision":initial+1,"disabledFeatures":["namespacedTools"]}),
            400,
            vec!["imageInput"],
        ),
        (
            json!({"revision":initial+1,"features":["imageInput","namespacedTools"]}),
            422,
            vec!["imageInput"],
        ),
        (
            json!({"revision":initial+1,"disabledFeatures":["webSearch","imageGeneration"]}),
            200,
            vec!["webSearch", "imageGeneration"],
        ),
        (
            json!({"revision":initial+2,"disabledFeatures":[]}),
            200,
            vec![],
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/accounts/{id}/features"))
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let actual = response.status().as_u16();
        let response_body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(
            actual,
            status,
            "{}",
            String::from_utf8_lossy(&response_body)
        );
        assert_eq!(
            ops.snapshot()["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["id"] == id.to_string())
                .unwrap()["disabledFeatures"],
            json!(disabled)
        );
    }
    assert!(ops.capabilities()["accounts"][0]["capabilities"].is_null());
}

#[tokio::test]
#[ignore = "Requires BRIDGE_CAPABILITY_DATA; clones only operations DB/WAL/SHM, never starts a native process or reads account credentials"]
async fn copied_failed_desktop_request_can_be_admitted_without_modifying_source_data() {
    use sha2::{Digest, Sha256};
    let source = std::path::PathBuf::from(std::env::var("BRIDGE_CAPABILITY_DATA").unwrap())
        .join("accounts/operations");
    let root = tempfile::tempdir().unwrap();
    let mut hashes = vec![];
    for name in [
        "operations.sqlite3",
        "operations.sqlite3-wal",
        "operations.sqlite3-shm",
    ] {
        let path = source.join(name);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        hashes.push((path, Sha256::digest(&bytes)));
        std::fs::write(root.path().join(name), bytes).unwrap();
    }
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("operations.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (model, group, status, error): (String, String, String, String) = db
        .query_row(
            "SELECT model,group_id,status,error_code FROM requests WHERE id=?1",
            ["993e0f67-ae14-4ee3-9b48-e2d9474cc2f3"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "rejected");
    assert_eq!(error, "CAPABILITY_UNAVAILABLE");
    drop(db);
    let ops = Operations::open(root.path()).unwrap();
    let key = access(&ops, Uuid::parse_str(&group).unwrap());
    for model in [&model, "gpt-6-luna"] {
        ops.acquire_inference(&key, request(model, all()))
            .await
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
    drop(ops);
    for (path, hash) in hashes {
        assert_eq!(
            Sha256::digest(std::fs::read(path).unwrap()),
            hash,
            "source data changed"
        );
    }
}
