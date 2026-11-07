use codex_relay::{
    operations::{
        AccountInput, ApiKeyCreate, ClientAccess, DEFAULT_GROUP, GroupInput, Operations,
        RoutingRequest, StatsQuery,
    },
    provider::Provider,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

fn revision(ops: &Operations) -> u64 {
    ops.snapshot()["revision"].as_u64().unwrap()
}
fn group(ops: &Operations) -> Uuid {
    ops.save_group(
        None,
        GroupInput {
            revision: revision(ops),
            name: "second".into(),
            enabled: true,
        },
    )
    .unwrap()
}
fn input(ops: &Operations) -> AccountInput {
    AccountInput {
        revision: revision(ops),
        group_id: DEFAULT_GROUP,
        enabled: true,
        max_concurrency: 2,
    }
}
fn setup() -> (tempfile::TempDir, Arc<Operations>, Uuid, Uuid) {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let id = Uuid::new_v4();
    ops.register_accounts(&[(id, "shared".into())]).unwrap();
    let second = group(&ops);
    ops.set_account_groups(id, input(&ops), vec![DEFAULT_GROUP, second])
        .unwrap();
    (dir, ops, id, second)
}
fn key(ops: &Operations, group_id: Uuid) -> ClientAccess {
    let key = ops
        .create_api_key(ApiKeyCreate {
            fast_mode: false,
            name: group_id.to_string(),
            group_id,
            models: vec![],
            expires_in_days: None,
            max_concurrency: 0,
        })
        .unwrap();
    ops.authenticate_api_key(&key.secret).unwrap()
}
fn request() -> RoutingRequest {
    RoutingRequest {
        id: Uuid::new_v4(),
        model: "m".into(),
        ..Default::default()
    }
}

#[test]
fn membership_validation_counts_persistence_and_rejects_legacy_data() {
    let (dir, ops, id, second) = setup();
    ops.set_account_groups(id, input(&ops), vec![DEFAULT_GROUP, second, second])
        .unwrap();
    let snapshot = ops.snapshot();
    assert_eq!(
        snapshot["accounts"][0]["groupIds"],
        json!([DEFAULT_GROUP, second])
    );
    assert_eq!(snapshot["accountCapacity"], 2);
    assert!(snapshot["accounts"][0].get("groupId").is_none());
    for group in snapshot["groups"].as_array().unwrap() {
        assert_eq!(group["memberCount"], 1);
    }
    assert_eq!(
        ops.delete_group(second, revision(&ops)).unwrap_err().code,
        "BUSY"
    );
    for groups in [vec![], vec![DEFAULT_GROUP, Uuid::new_v4()]] {
        assert!(ops.set_account_groups(id, input(&ops), groups).is_err());
        assert_eq!(ops.snapshot()["revision"], snapshot["revision"]);
    }
    let mut stale = input(&ops);
    stale.revision -= 1;
    assert_eq!(
        ops.set_account_groups(id, stale, vec![second])
            .unwrap_err()
            .code,
        "CONFIG_CONFLICT"
    );
    drop(ops);
    let ops = Operations::open(dir.path()).unwrap();
    assert_eq!(
        ops.snapshot()["accounts"][0]["groupIds"],
        snapshot["accounts"][0]["groupIds"]
    );
    drop(ops);
    // Old single-group documents must be rejected without rewriting stored data.
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    let document: String = db
        .query_row("SELECT document FROM settings WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut document: Value = serde_json::from_str(&document).unwrap();
    document["accounts"][0]
        .as_object_mut()
        .unwrap()
        .remove("groupIds");
    document["accounts"][0]["groupId"] = json!(second);
    db.execute(
        "UPDATE settings SET document=?1 WHERE id=1",
        [document.to_string()],
    )
    .unwrap();
    drop(db);
    assert_eq!(
        Operations::open(dir.path()).err().unwrap().code,
        "INVALID_JSON"
    );
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    let stored: String = db
        .query_row("SELECT document FROM settings WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), document);
}

#[test]
fn persisted_settings_require_current_fields_without_filling_old_documents() {
    let (dir, ops, _id, _second) = setup();
    drop(ops);
    let db = rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    let stored: String = db
        .query_row("SELECT document FROM settings WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let current: Value = serde_json::from_str(&stored).unwrap();
    for field in [
        "globalLimitAuto",
        "queueLimit",
        "queueTimeoutSeconds",
        "nativeAffinityTtlHours",
        "retentionDays",
    ] {
        let mut old = current.clone();
        old.as_object_mut().unwrap().remove(field);
        let old = old.to_string();
        db.execute("UPDATE settings SET document=?1 WHERE id=1", [&old])
            .unwrap();
        assert_eq!(
            Operations::open(dir.path()).err().unwrap().code,
            "INVALID_JSON",
            "{field}"
        );
        let unchanged: String = db
            .query_row("SELECT document FROM settings WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(unchanged, old);
    }
}

#[tokio::test]
async fn groups_share_capacity_and_keep_request_attribution_and_authorization() {
    let (_dir, ops, id, second) = setup();
    ops.set_global(revision(&ops), 10).unwrap();
    let provider = Provider {
        execution: Default::default(),
        id: "fixture".into(),
        model: "m".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        auth: "none".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    ops.set_inference_route(id, revision(&ops), Some(provider.clone()))
        .unwrap();
    let first_access = key(&ops, DEFAULT_GROUP);
    let second_access = key(&ops, second);
    assert_eq!(ops.native_inference_route(second, "m").unwrap().0, id);
    assert_eq!(ops.inference_models(Some(&second_access)), vec!["m"]);
    ops.check_inference_route(id, &second_access, &provider, "m")
        .unwrap();
    let first = ops
        .acquire_inference(&first_access, request())
        .await
        .unwrap();
    let second_lease = ops
        .acquire_inference(&second_access, request())
        .await
        .unwrap();
    assert_eq!(first.account_id, Some(id));
    assert_eq!(second_lease.account_id, Some(id));
    let snapshot = ops.snapshot();
    assert_eq!(snapshot["accounts"][0]["active"], 2);
    assert_eq!(snapshot["groups"][0]["active"], 1);
    assert_eq!(snapshot["groups"][1]["active"], 1);
    let waiting_ops = ops.clone();
    let access = second_access.clone();
    let waiting =
        tokio::spawn(async move { waiting_ops.acquire_inference(&access, request()).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while ops.snapshot()["queued"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!waiting.is_finished());
    assert_eq!(ops.snapshot()["groups"][1]["queued"], 1);
    assert_eq!(
        ops.set_account_groups(id, input(&ops), vec![second])
            .unwrap_err()
            .code,
        "BUSY"
    );
    ops.save_group(
        Some(DEFAULT_GROUP),
        GroupInput {
            revision: revision(&ops),
            name: "default".into(),
            enabled: false,
        },
    )
    .unwrap();
    first.complete(Ok(())).unwrap();
    let third = tokio::time::timeout(Duration::from_secs(3), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(third.account_id, Some(id));
    second_lease.complete(Ok(())).unwrap();
    third.complete(Ok(())).unwrap();
    assert!(ops.has_inference_routes());
    ops.acquire_client(id, &second_access)
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    // An account-specific check may use another enabled membership.
    ops.begin(Some(id), None, "diagnostic", "check", true)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    let stats: StatsQuery =
        serde_json::from_value(json!({"groupId":second,"kind":"inference"})).unwrap();
    assert_eq!(ops.statistics(stats).unwrap()["summary"]["succeeded"], 3);
    ops.save_group(
        Some(DEFAULT_GROUP),
        GroupInput {
            revision: revision(&ops),
            name: "default".into(),
            enabled: true,
        },
    )
    .unwrap();
    ops.set_account_groups(id, input(&ops), vec![second])
        .unwrap();
    assert!(ops.acquire_client(id, &first_access).await.is_err());
    assert!(
        ops.check_inference_route(id, &first_access, &provider, "m")
            .is_err()
    );
    assert!(
        ops.acquire_inference(&first_access, request())
            .await
            .is_err()
    );
    assert!(ops.inference_models(Some(&first_access)).is_empty());
    ops.acquire_inference(&second_access, request())
        .await
        .unwrap()
        .complete(Ok(()))
        .unwrap();
}
