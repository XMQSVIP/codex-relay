use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, ApiKeyImport, ApiKeyUpdate, DEFAULT_GROUP, Operations},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn create(ops: &Operations, name: &str) -> codex_relay::operations::ApiKeyCreated {
    ops.create_api_key(ApiKeyCreate {
        fast_mode: false,
        name: name.into(),
        group_id: DEFAULT_GROUP,
        models: vec!["test-model".into()],
        expires_in_days: None,
        max_concurrency: 3,
    })
    .unwrap()
}
fn backup(ops: &Operations) -> Value {
    serde_json::to_value(ops.export_api_keys().unwrap()).unwrap()
}
fn import(value: Value) -> ApiKeyImport {
    serde_json::from_value(json!({"backup":value})).unwrap()
}

#[test]
fn backup_roundtrip_preserves_credentials_scopes_pauses_and_expiry_across_restart() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let ops = Operations::open(source.path()).unwrap();
    let active = create(&ops, "active");
    let paused = create(&ops, "paused");
    ops.update_api_key(
        paused.key.id,
        ApiKeyUpdate {
            revision: 1,
            state: Some("paused".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let expired = create(&ops, "expired");
    Connection::open(source.path().join("operations.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE api_keys SET expires_at=1 WHERE id=?1",
            [expired.key.id.to_string()],
        )
        .unwrap();
    let exported = backup(&ops);
    let dest = Operations::open(target.path()).unwrap();
    assert_eq!(
        dest.import_api_keys(import(exported.clone()))
            .unwrap()
            .imported,
        3
    );
    let active_id = dest.authenticate_api_key(&active.secret).unwrap().key_id;
    assert_ne!(active_id, active.key.id);
    assert!(
        dest.authenticate_api_key(&active.secret)
            .unwrap()
            .allows("test-model")
    );
    assert!(
        !dest
            .authenticate_api_key(&active.secret)
            .unwrap()
            .allows("other")
    );
    assert!(dest.authenticate_api_key(&paused.secret).is_err());
    assert!(dest.authenticate_api_key(&expired.secret).is_err());
    let keys = dest.list_api_keys().unwrap();
    assert!(keys.iter().all(|k| k.max_concurrency == 3));
    assert_eq!(
        keys.iter().find(|k| k.name == "active").unwrap().created_at,
        active.key.created_at
    );
    assert_eq!(
        keys.iter()
            .find(|k| k.name == "expired")
            .unwrap()
            .expires_at,
        Some(1)
    );
    // Retrying a backup cannot undo a subsequent revocation.
    dest.update_api_key(
        active_id,
        ApiKeyUpdate {
            revision: 1,
            state: Some("revoked".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let repeated = dest.import_api_keys(import(exported)).unwrap();
    assert_eq!((repeated.imported, repeated.skipped), (0, 3));
    assert!(dest.authenticate_api_key(&active.secret).is_err());
    drop(dest);
    let dest = Operations::open(target.path()).unwrap();
    assert!(dest.authenticate_api_key(&paused.secret).is_err());
    assert!(dest.authenticate_api_key(&expired.secret).is_err());
    assert_eq!(backup(&dest)["keys"].as_array().unwrap().len(), 2);
}

#[test]
fn imports_roll_back_missing_groups_and_invalid_rows_and_can_remap_groups() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let ops = Operations::open(source.path()).unwrap();
    create(&ops, "one");
    create(&ops, "two");
    let dest = Operations::open(target.path()).unwrap();
    let mut exported = backup(&ops);
    exported["keys"][1]["groupId"] = json!(Uuid::new_v4());
    assert!(dest.import_api_keys(import(exported.clone())).is_err());
    assert!(
        dest.list_api_keys().unwrap().is_empty(),
        "The valid first row must roll back too"
    );
    let imported = dest
        .import_api_keys(
            serde_json::from_value(json!({"backup":exported,"groupId":DEFAULT_GROUP})).unwrap(),
        )
        .unwrap();
    assert_eq!(imported.imported, 2);
    assert!(
        dest.list_api_keys()
            .unwrap()
            .iter()
            .all(|key| key.group_id == DEFAULT_GROUP)
    );
    let original = backup(&ops);
    for (field, value) in [
        ("secret", json!("bad-secret")),
        ("name", json!("\n")),
        ("maxConcurrency", json!(257)),
        ("models", json!(["invalid model"])),
        ("state", json!("revoked")),
        ("expiresAt", json!(u64::MAX)),
    ] {
        let mut invalid = original.clone();
        invalid["keys"][1][field] = value;
        let failure = dest.import_api_keys(import(invalid)).unwrap_err();
        assert_eq!(failure.code, "INVALID_INPUT");
        assert!(!failure.message.contains("bad-secret"));
    }
    let empty = json!({"version":1,"keys":[]});
    assert!(dest.import_api_keys(import(empty)).is_err());
    let mut unsupported = original.clone();
    unsupported["version"] = json!(2);
    assert!(dest.import_api_keys(import(unsupported)).is_err());
    // Duplicates within a single file count as skipped and do not consume capacity.
    let other = tempfile::tempdir().unwrap();
    let other = Operations::open(other.path()).unwrap();
    let row = original["keys"][0].clone();
    let result = other
        .import_api_keys(import(json!({"version":1,"keys":[row,row]})))
        .unwrap();
    assert_eq!((result.imported, result.skipped), (1, 1));
}

fn insert(db: &Connection, id: &str, days: i64, status: &str) {
    db.execute("INSERT INTO requests(id,started_at,kind,action,status,request_content,response_content) VALUES(?1,?2,'inference','responses',?3,'{}','{}')",
        params![id, codex_relay::now_ms() as i64 - days * 86_400_000, status]).unwrap();
}

#[test]
fn import_capacity_failure_rolls_back_all_new_keys() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let ops = Operations::open(source.path()).unwrap();
    create(&ops, "one");
    create(&ops, "two");
    let dest = Operations::open(target.path()).unwrap();
    let template = backup(&ops)["keys"][0].clone();
    let keys: Vec<_> = (0..999)
        .map(|i| {
            let mut key = template.clone();
            key["secret"] = json!(format!("cr_live_{i:064x}"));
            key
        })
        .collect();
    assert_eq!(
        dest.import_api_keys(import(json!({"version":1,"keys":keys})))
            .unwrap()
            .imported,
        999
    );
    assert_eq!(
        dest.import_api_keys(import(backup(&ops))).unwrap_err().code,
        "INVALID_INPUT"
    );
    assert_eq!(dest.list_api_keys().unwrap().len(), 999);
}

#[test]
fn admission_skips_cleanup_and_maintenance_uses_the_configured_retention() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    ops.set_retention(1, 60).unwrap();
    insert(&db, "forty-days", 40, "succeeded");
    insert(&db, "seventy-days", 70, "failed");
    ops.begin(None, None, "diagnostic", "retention-check", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    let records = ids(&db);
    assert!(records.iter().any(|id| id == "forty-days"));
    assert!(records.iter().any(|id| id == "seventy-days"));
    assert_eq!(ops.prune_request_history().unwrap(), 1);
    let records = ids(&db);
    assert!(!records.iter().any(|id| id == "seventy-days"));
    assert_eq!(ops.set_retention(2, 7).unwrap(), 1);
    assert_eq!(ids(&db).len(), 1);
}

#[test]
fn maintenance_bounds_each_batch_and_keeps_active_requests() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let mut db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..2505 {
        insert(&tx, &format!("old-{index}"), 40, "succeeded");
    }
    insert(&tx, "active", 40, "running");
    insert(&tx, "waiting", 40, "queued");
    tx.commit().unwrap();
    assert_eq!(ops.prune_request_history().unwrap(), 1000);
    // Admissions and settlement can run between maintenance batches.
    ops.begin(None, None, "diagnostic", "between-batches", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    assert_eq!(ops.prune_request_history().unwrap(), 1000);
    assert_eq!(ops.prune_request_history().unwrap(), 505);
    assert_eq!(ops.prune_request_history().unwrap(), 0);
    let remaining = ids(&db);
    assert!(remaining.iter().any(|id| id == "active"));
    assert!(remaining.iter().any(|id| id == "waiting"));
    assert_eq!(remaining.len(), 3);
}
fn ids(db: &Connection) -> Vec<String> {
    db.prepare("SELECT id FROM requests ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn retention_defaults_validates_and_cleans_atomically_without_removing_active_work() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.snapshot()["retentionDays"], 30);
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    insert(&db, "old", 31, "failed");
    insert(&db, "recent", 2, "succeeded");
    insert(&db, "running", 45, "running");
    insert(&db, "queued", 45, "queued");
    for days in [0, 366, 3650, u32::MAX] {
        assert!(ops.set_retention(1, days).is_err());
    }
    assert!(ops.set_retention(0, 1).is_err());
    assert_eq!(ids(&db).len(), 4);
    assert_eq!(ops.snapshot()["revision"], 1);
    assert_eq!(ops.set_retention(1, 7).unwrap(), 1);
    assert_eq!(ids(&db), ["queued", "recent", "running"]);
    assert_eq!(ops.snapshot()["retentionDays"], 7);
    // Config and pruning must roll back together if persistence fails.
    db.execute_batch("CREATE TRIGGER fail_settings BEFORE UPDATE ON settings BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(ops.set_retention(2, 1).is_err());
    assert_eq!(ids(&db).len(), 3);
    assert_eq!(ops.snapshot()["retentionDays"], 7);
    db.execute_batch("DROP TRIGGER fail_settings;").unwrap();
    // No incoming request is needed: the maintenance entry point uses the setting.
    insert(&db, "eight-days", 8, "failed");
    assert_eq!(ops.prune_request_history().unwrap(), 1);
    insert(&db, "startup", 10, "succeeded");
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(ops.snapshot()["retentionDays"], 7);
    assert_eq!(
        ids(&db),
        ["recent"],
        "Startup settles abandoned work then prunes with the saved policy"
    );
    assert_eq!(ops.set_retention(2, 365).unwrap(), 0);
    assert_eq!(ops.set_retention(3, 1).unwrap(), 1);
    drop(ops);
    // Persisted configuration must include the current retention setting.
    let document: String = db
        .query_row("SELECT document FROM settings", [], |r| r.get(0))
        .unwrap();
    let mut document: Value = serde_json::from_str(&document).unwrap();
    document.as_object_mut().unwrap().remove("retentionDays");
    db.execute("UPDATE settings SET document=?1", [document.to_string()])
        .unwrap();
    assert_eq!(
        Operations::open(temp.path()).err().unwrap().code,
        "INVALID_JSON"
    );
}

async fn call(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn management_routes_require_admin_and_accept_large_backup_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let token = "a".repeat(64);
    let state = AdminState::new(4317, temp.path().join("accounts"), token.clone())
        .await
        .unwrap();
    let key = create(&state.operations, "one");
    let app = router(state.clone(), temp.path().join("web"));
    for credential in ["wrong", &key.secret] {
        for (method, path, body) in [
            ("GET", "/api/api-keys/export", json!(null)),
            ("POST", "/api/api-keys/import", json!({})),
            (
                "POST",
                "/api/settings/retention",
                json!({"revision":1,"retentionDays":7}),
            ),
        ] {
            assert_eq!(
                call(&app, credential, method, path, body).await.0,
                StatusCode::FORBIDDEN
            );
        }
    }
    let (status, mut exported) =
        call(&app, &token, "GET", "/api/api-keys/export", json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    exported["keys"] = json!(vec![exported["keys"][0].clone(); 300]);
    assert!(exported.to_string().len() > 65536);
    let (status, result) = call(
        &app,
        &token,
        "POST",
        "/api/api-keys/import",
        json!({"backup":exported}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result, json!({"imported":0,"skipped":300}));
    let (status, invalid) = call(
        &app,
        &token,
        "POST",
        "/api/api-keys/import",
        json!({"backup":{"version":1,"keys":[{"secret":"invalid"}]}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["error"]["code"], "INVALID_INPUT");
    let (status, settings) = call(
        &app,
        &token,
        "POST",
        "/api/settings/retention",
        json!({"revision":1,"retentionDays":90}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["retentionDays"], 90);
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            "/api/settings/retention",
            json!({"revision":1,"retentionDays":7})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
