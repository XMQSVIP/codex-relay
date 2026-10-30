use super::*;
use crate::account_location::Location;
use std::time::Duration;

fn custom() -> Config {
    Config {
        enabled: true,
        source: Source::Custom,
        custom: Location {
            timezone: "Asia/Tokyo".into(),
            ..Default::default()
        },
    }
}
async fn settled(state: &AdminState, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.operations.proxy_blocked(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn location_save_cancels_only_its_account_and_keeps_stopped_accounts_stopped() {
    let root = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, root.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let a = state.profiles.create("A").await.unwrap();
    let b = state.profiles.create("B").await.unwrap();
    state
        .operations
        .register_accounts(&[(a.id, a.name), (b.id, b.name)])
        .unwrap();
    let cancelled = tokio_util::sync::CancellationToken::new();
    let untouched = tokio_util::sync::CancellationToken::new();
    let _a = state
        .profiles
        .register_inference(a.id, cancelled.clone())
        .unwrap();
    let _b = state
        .profiles
        .register_inference(b.id, untouched.clone())
        .unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    let _ = save(
        State(state.clone()),
        Path(a.id),
        Json(Input {
            retry: false,
            revision,
            config: custom(),
            preview_id: None,
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), cancelled.cancelled())
        .await
        .unwrap();
    settled(&state, a.id).await;
    assert!(!untouched.is_cancelled());
    assert!(!state.profiles.has_worker(a.id).await);
    assert_eq!(
        state
            .operations
            .resolve_location(a.id, false)
            .await
            .unwrap()
            .timezone,
        "Asia/Tokyo"
    );
    assert!(
        state
            .operations
            .resolve_location(b.id, false)
            .await
            .is_none()
    );
    assert_eq!(
        save(
            State(state.clone()),
            Path(a.id),
            Json(Input {
                retry: false,
                revision,
                config: Config::default(),
                preview_id: None,
            })
        )
        .await
        .unwrap_err()
        .code,
        "CONFIG_CONFLICT"
    );
    let mut disabled = custom();
    disabled.enabled = false;
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    let _ = save(
        State(state.clone()),
        Path(a.id),
        Json(Input {
            retry: false,
            revision,
            config: disabled,
            preview_id: None,
        }),
    )
    .await
    .unwrap();
    settled(&state, a.id).await;
    assert!(
        state
            .operations
            .resolve_location(a.id, false)
            .await
            .is_none()
    );
    assert_eq!(
        state
            .operations
            .location_config(a.id)
            .unwrap()
            .0
            .custom
            .timezone,
        "Asia/Tokyo"
    );
}
#[test]
fn location_configuration_survives_restart_and_defaults_are_disabled() {
    let root = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    {
        let ops = crate::operations::Operations::open(root.path()).unwrap();
        ops.register_accounts(&[(id, "A".into())]).unwrap();
        assert!(!ops.location_config(id).unwrap().0.enabled);
        ops.set_account_location(
            id,
            ops.snapshot()["revision"].as_u64().unwrap(),
            custom(),
            false,
        )
        .unwrap();
    }
    let ops = crate::operations::Operations::open(root.path()).unwrap();
    assert_eq!(ops.location_config(id).unwrap().0, custom());
}

#[tokio::test]
async fn confirmed_preview_is_applied_only_on_save_and_conflicts_do_not_change_state() {
    let root = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, root.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let profile = state.profiles.create("preview").await.unwrap();
    state
        .operations
        .register_accounts(&[(profile.id, profile.name)])
        .unwrap();
    let (before, proxy, _) = state.operations.location_config(profile.id).unwrap();
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    let location = Location {
        timezone: "Asia/Tokyo".into(),
        country: "JP".into(),
        ..Default::default()
    };
    let preview = state
        .operations
        .locations
        .preview_fixture(profile.id, proxy, location.clone())
        .await;
    let cancelled = tokio_util::sync::CancellationToken::new();
    let _registration = state
        .profiles
        .register_inference(profile.id, cancelled.clone())
        .unwrap();
    assert!(!cancelled.is_cancelled());
    assert_eq!(
        state.operations.location_config(profile.id).unwrap().0,
        before
    );
    let config = Config {
        enabled: true,
        ..Default::default()
    };
    assert_eq!(
        save(
            State(state.clone()),
            Path(profile.id),
            Json(Input {
                retry: false,
                revision,
                config: config.clone(),
                preview_id: None,
            })
        )
        .await
        .unwrap_err()
        .code,
        "LOCATION_PREVIEW_REQUIRED"
    );
    assert_eq!(
        save(
            State(state.clone()),
            Path(profile.id),
            Json(Input {
                retry: false,
                revision: revision + 1,
                config: config.clone(),
                preview_id: Some(preview.id),
            })
        )
        .await
        .unwrap_err()
        .code,
        "CONFIG_CONFLICT"
    );
    assert!(!cancelled.is_cancelled());
    assert_eq!(
        state.operations.location_config(profile.id).unwrap().0,
        before
    );
    let _ = save(
        State(state.clone()),
        Path(profile.id),
        Json(Input {
            retry: false,
            revision,
            config: config.clone(),
            preview_id: Some(preview.id),
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), cancelled.cancelled())
        .await
        .unwrap();
    settled(&state, profile.id).await;
    assert_eq!(
        state
            .operations
            .resolve_location(profile.id, false)
            .await
            .unwrap(),
        location
    );
    assert!(!state.profiles.has_worker(profile.id).await);
}
