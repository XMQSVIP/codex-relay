use codex_relay::operations::{ApiKeyCreate, ApiKeyUpdate, DEFAULT_GROUP, GroupInput, Operations};
use rusqlite::params;
use uuid::Uuid;

fn input(group_id: Uuid) -> ApiKeyCreate {
    ApiKeyCreate {
        fast_mode: false,
        name: "客户端 A".into(),
        group_id,
        models: vec!["model-a".into(), "model-a".into(), "model-b".into()],
        expires_in_days: Some(30),
        max_concurrency: 0,
    }
}

#[test]
fn fast_mode_is_default_off_revisioned_persistent_and_portable() {
    use serde_json::json;
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let key = ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    assert!(!key.key.fast_mode);
    assert!(!ops.authenticate_api_key(&key.secret).unwrap().fast_mode);
    let updated = ops
        .update_api_key(
            key.key.id,
            serde_json::from_value(json!({"revision":1,"fastMode":true})).unwrap(),
        )
        .unwrap();
    assert!(updated.fast_mode);
    assert_eq!(updated.revision, 2);
    assert_eq!(
        ops.update_api_key(
            key.key.id,
            serde_json::from_value(json!({"revision":1,"fastMode":false})).unwrap()
        )
        .unwrap_err()
        .code,
        "CONFIG_CONFLICT"
    );
    let renamed = ops
        .update_api_key(
            key.key.id,
            serde_json::from_value(json!({"revision":2,"name":"renamed"})).unwrap(),
        )
        .unwrap();
    assert!(renamed.fast_mode);
    let backup = serde_json::to_value(ops.export_api_keys().unwrap()).unwrap();
    assert_eq!(backup["keys"][0]["fastMode"], true);
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert!(ops.authenticate_api_key(&key.secret).unwrap().fast_mode);
    assert_eq!(
        ops.list_api_keys().unwrap()[0].secret.as_deref(),
        Some(key.secret.as_str())
    );
    let destination = tempfile::tempdir().unwrap();
    let imported = Operations::open(destination.path()).unwrap();
    imported
        .import_api_keys(serde_json::from_value(json!({"backup":backup})).unwrap())
        .unwrap();
    assert!(
        imported
            .authenticate_api_key(&key.secret)
            .unwrap()
            .fast_mode
    );
    let off = ops
        .update_api_key(
            key.key.id,
            serde_json::from_value(json!({"revision":renamed.revision,"fastMode":false})).unwrap(),
        )
        .unwrap();
    assert!(!off.fast_mode);
    assert!(!ops.authenticate_api_key(&key.secret).unwrap().fast_mode);
    let mut legacy = serde_json::to_value(ops.export_api_keys().unwrap()).unwrap();
    legacy["keys"][0]
        .as_object_mut()
        .unwrap()
        .remove("fastMode");
    assert!(
        serde_json::from_value::<codex_relay::operations::ApiKeyImport>(json!({"backup":legacy}))
            .is_err()
    );
}
fn update(revision: u64, state: &str) -> ApiKeyUpdate {
    ApiKeyUpdate {
        revision,
        state: Some(state.into()),
        ..Default::default()
    }
}
#[test]
fn full_secrets_are_retrievable_across_restart_and_auth_still_uses_hashes() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let created = ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    assert_eq!(created.key.models, ["model-a", "model-b"]);
    assert!(
        ops.authenticate_api_key(&created.secret)
            .unwrap()
            .allows("model-a")
    );
    assert!(
        !ops.authenticate_api_key(&created.secret)
            .unwrap()
            .allows("MODEL-A")
    );
    assert_eq!(
        ops.authenticate_api_key(&"a".repeat(64)).unwrap_err().code,
        "INVALID_API_KEY"
    );
    let listing = serde_json::to_string(&ops.list_api_keys().unwrap()).unwrap();
    assert!(listing.contains(&created.secret));
    assert!(!listing.contains("secret_hash"));
    assert!(
        ops.list_api_keys().unwrap()[0]
            .last_authenticated_at
            .is_some()
    );
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(
        ops.authenticate_api_key(&created.secret).unwrap().key_id,
        created.key.id
    );
    assert_eq!(
        ops.list_api_keys().unwrap()[0].secret.as_deref(),
        Some(created.secret.as_str())
    );
}
#[test]
fn pause_revocation_expiry_and_stale_edits_take_effect_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let key = ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    let paused = ops.update_api_key(key.key.id, update(1, "paused")).unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap_err().code,
        "INVALID_API_KEY"
    );
    assert_eq!(
        ops.update_api_key(key.key.id, update(1, "active"))
            .unwrap_err()
            .code,
        "CONFIG_CONFLICT"
    );
    let resumed = ops
        .update_api_key(key.key.id, update(paused.revision, "active"))
        .unwrap();
    assert!(ops.authenticate_api_key(&key.secret).is_ok());
    let revoked = ops
        .update_api_key(key.key.id, update(resumed.revision, "revoked"))
        .unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap_err().code,
        "INVALID_API_KEY"
    );
    assert!(
        ops.update_api_key(key.key.id, update(revoked.revision, "active"))
            .is_err()
    );
    assert_eq!(
        ops.update_api_key(key.key.id, update(1, "revoked"))
            .unwrap()
            .revision,
        revoked.revision
    );
    let expired = ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    rusqlite::Connection::open(temp.path().join("operations.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE api_keys SET expires_at=0 WHERE id=?1",
            [expired.key.id.to_string()],
        )
        .unwrap();
    assert_eq!(
        ops.authenticate_api_key(&expired.secret).unwrap_err().code,
        "INVALID_API_KEY"
    );
    assert!(
        ops.list_api_keys()
            .unwrap()
            .iter()
            .any(|k| k.id == expired.key.id && k.state == "expired")
    );
    assert!(
        ops.update_api_key(expired.key.id, update(1, "active"))
            .is_err()
    );
}
#[test]
fn group_permissions_prevent_deletion_until_bound_keys_are_retired() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let group = ops
        .save_group(
            None,
            GroupInput {
                revision: 1,
                name: "测试组".into(),
                enabled: true,
            },
        )
        .unwrap();
    let key = ops.create_api_key(input(group)).unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap().group_id,
        group
    );
    assert_eq!(ops.delete_group(group, 2).unwrap_err().code, "BUSY");
    ops.update_api_key(key.key.id, update(1, "paused")).unwrap();
    assert_eq!(ops.delete_group(group, 2).unwrap_err().code, "BUSY");
    ops.update_api_key(key.key.id, update(2, "active")).unwrap();
    ops.save_group(
        Some(group),
        GroupInput {
            revision: 2,
            name: "测试组".into(),
            enabled: false,
        },
    )
    .unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap_err().code,
        "API_KEY_GROUP_DISABLED"
    );
    assert!(ops.create_api_key(input(group)).is_err());
    ops.update_api_key(key.key.id, update(3, "revoked"))
        .unwrap();
    ops.delete_group(group, 3).unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap_err().code,
        "INVALID_API_KEY"
    );
}
#[test]
fn malformed_scopes_are_rejected_and_storage_failure_does_not_authenticate() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    for models in [
        vec!["*".into()],
        vec!["a\nb".into()],
        vec!["$(cmd)".into()],
        vec!["a".repeat(129)],
    ] {
        let mut bad = input(DEFAULT_GROUP);
        bad.models = models;
        assert!(ops.create_api_key(bad).is_err());
    }
    let key = ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    db.execute("CREATE TRIGGER fail_auth BEFORE UPDATE ON api_keys BEGIN SELECT RAISE(FAIL,'storage unavailable'); END", []).unwrap();
    assert_eq!(
        ops.authenticate_api_key(&key.secret).unwrap_err().code,
        "OPERATIONS_STORAGE"
    );
    assert_eq!(ops.snapshot()["storageHealthy"], false);
    db.execute("DROP TRIGGER fail_auth", []).unwrap();
    db.execute(
        "UPDATE api_keys SET expires_at=?1 WHERE id=?2",
        params![0, key.key.id.to_string()],
    )
    .unwrap();
    assert!(ops.authenticate_api_key(&key.secret).is_err());
}

#[test]
fn optional_scope_and_permanent_key_survive_restart_and_remain_revocable() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let request: ApiKeyCreate =
        serde_json::from_value(serde_json::json!({"name":"permanent","groupId":DEFAULT_GROUP}))
            .unwrap();
    let key = ops.create_api_key(request).unwrap();
    assert!(key.key.models.is_empty());
    assert_eq!(key.key.expires_at, None);
    assert_eq!(
        serde_json::to_value(&key.key).unwrap()["expiresAt"],
        serde_json::Value::Null
    );
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    assert!(access.allows("new-model"));
    assert!(ops.inference_models(Some(&access)).is_empty());
    assert!(ops.inference_route(access.group_id, "new-model").is_err());
    let paused = ops.update_api_key(key.key.id, update(1, "paused")).unwrap();
    assert!(ops.authenticate_api_key(&key.secret).is_err());
    let resumed = ops
        .update_api_key(key.key.id, update(paused.revision, "active"))
        .unwrap();
    assert!(ops.authenticate_api_key(&key.secret).is_ok());
    ops.update_api_key(key.key.id, update(resumed.revision, "revoked"))
        .unwrap();
    assert!(ops.authenticate_api_key(&key.secret).is_err());
}

#[test]
fn hash_only_key_data_is_rejected_without_rewriting_it() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.create_api_key(input(DEFAULT_GROUP)).unwrap();
    drop(ops);
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    db.execute("UPDATE api_keys SET secret=NULL", []).unwrap();
    assert_eq!(
        Operations::open(dir.path()).err().unwrap().code,
        "DATABASE_SCHEMA"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM api_keys WHERE secret IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
