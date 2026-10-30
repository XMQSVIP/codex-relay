use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::AccountInput,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn revision(state: &AdminState) -> u64 {
    state.operations.snapshot()["revision"].as_u64().unwrap()
}
async fn remove(
    state: Arc<AdminState>,
    id: uuid::Uuid,
    revision: u64,
    origin: &str,
) -> (StatusCode, Value) {
    let response = router(state, std::path::PathBuf::from("missing-web"))
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/accounts/{id}"))
                .header("host", "127.0.0.1:4317")
                .header("origin", origin)
                .header("content-type", "application/json")
                .body(Body::from(json!({"revision":revision}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    (status, body)
}

#[tokio::test]
async fn delete_is_authorized_revision_checked_and_preserves_other_accounts_and_history() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
        .await
        .unwrap();
    let a = state
        .profiles
        .create_native_config("same name")
        .await
        .unwrap();
    let b = state.profiles.create("same name").await.unwrap();
    state
        .operations
        .register_accounts(&state.profiles.metadata().await)
        .unwrap();
    let lease = state
        .operations
        .begin(Some(a.id), None, "management", "stop", false)
        .unwrap();
    assert_eq!(
        remove(
            state.clone(),
            a.id,
            revision(&state),
            "https://evil.invalid"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        remove(
            state.clone(),
            a.id,
            revision(&state),
            "http://127.0.0.1:4317"
        )
        .await
        .1["error"]["code"],
        "BUSY"
    );
    lease.complete(Ok(())).unwrap();
    assert_eq!(
        remove(state.clone(), a.id, 0, "http://127.0.0.1:4317")
            .await
            .1["error"]["code"],
        "CONFIG_CONFLICT"
    );
    assert!(root.join(a.id.to_string()).exists());
    assert_eq!(
        remove(
            state.clone(),
            a.id,
            revision(&state),
            "http://127.0.0.1:4317"
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!root.join(a.id.to_string()).exists());
    assert!(
        root.join(b.id.to_string())
            .join("home/config.toml")
            .exists()
    );
    assert_eq!(state.profiles.list().await.len(), 1);
    assert_eq!(
        state.operations.snapshot()["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let query = serde_json::from_value(json!({"accountId":a.id})).unwrap();
    assert_eq!(
        state.operations.statistics(query).unwrap()["summary"]["succeeded"],
        1
    );
    drop(state);
    let reopened = AdminState::new_with_network(4317, root, None, vec![])
        .await
        .unwrap();
    assert_eq!(reopened.profiles.list().await.len(), 1);
    assert_eq!(
        reopened.operations.snapshot()["accounts"][0]["id"],
        b.id.to_string()
    );
}

#[tokio::test]
async fn interrupted_deletion_blocks_admission_and_recovers_each_phase_after_restart() {
    for remove_profile_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("accounts");
        let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
            .await
            .unwrap();
        let a = state.profiles.create("isolated").await.unwrap();
        state
            .operations
            .register_accounts(&state.profiles.metadata().await)
            .unwrap();
        state
            .operations
            .begin_account_delete(a.id, revision(&state))
            .unwrap();
        assert!(
            state
                .operations
                .begin(Some(a.id), None, "management", "start", true)
                .is_err()
        );
        assert!(
            state
                .operations
                .begin(Some(a.id), None, "management", "stop", false)
                .is_err()
        );
        assert!(
            state
                .operations
                .set_account(
                    a.id,
                    AccountInput {
                        revision: revision(&state),
                        group_id: uuid::Uuid::nil(),
                        enabled: true,
                        max_concurrency: 1
                    }
                )
                .is_err()
        );
        assert!(
            state
                .operations
                .set_inference_route(a.id, revision(&state), None)
                .is_err()
        );
        if remove_profile_first {
            state.profiles.delete(a.id).await.unwrap();
        }
        drop(state);
        let reopened = AdminState::new_with_network(4317, root.clone(), None, vec![])
            .await
            .unwrap();
        assert!(reopened.profiles.list().await.is_empty());
        assert!(
            reopened.operations.snapshot()["accounts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(!root.join(a.id.to_string()).exists());
    }
}

#[tokio::test]
async fn queued_account_cannot_be_deleted_even_after_its_active_lease_finishes() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, temp.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let a = state.profiles.create("queue test").await.unwrap();
    state
        .operations
        .register_accounts(&state.profiles.metadata().await)
        .unwrap();
    let lease = state
        .operations
        .begin(Some(a.id), None, "management", "start", true)
        .unwrap();
    let pending = state
        .operations
        .acquire(Some(a.id), None, "management", "start");
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut pending)
            .await
            .is_err()
    );
    lease.complete(Ok(())).unwrap();
    assert_eq!(
        state
            .operations
            .begin_account_delete(a.id, revision(&state))
            .unwrap_err()
            .code,
        "BUSY"
    );
    assert_eq!(
        state.operations.snapshot()["accounts"][0]["deleting"],
        false
    );
}
