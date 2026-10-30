//! Route access, connection checks and capability verification.

use super::AdminState;
use crate::error::{Error, Result};
use crate::profiles::RouteCheck;
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RouteInput {
    revision: u64,
    enabled: bool,
}
pub(super) async fn set_route(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<RouteInput>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    let inspected = if input.enabled {
        Some(inspect_access(&state, id).await?)
    } else {
        None
    };
    state.operations.set_inference_route(
        id,
        input.revision,
        inspected.as_ref().map(|(p, _)| p.clone()),
    )?;
    if let Some((_, caps)) = inspected {
        state.operations.publish_capabilities(id, caps)?;
        super::activation::schedule_start(&state, id);
    }
    Ok(Json(state.operations_snapshot()))
}

pub(super) async fn inspect_access(
    state: &Arc<AdminState>,
    id: uuid::Uuid,
) -> Result<(
    crate::provider::Provider,
    crate::operations::NativeCapabilities,
)> {
    let lease = state
        .account_lease(id, "native_check", "inspect_route")
        .await?;
    let cancel = state.shutdown.child_token();
    let guard = crate::inference::CancelOnDrop(cancel.clone());
    let registration = state.profiles.register_inference(id, cancel.clone())?;
    let task_state = state.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let result = task_state
            .profiles
            .inspect_route(id, RouteCheck::Access, cancel)
            .await;
        let result = lease.complete(result.map(|(provider, caps, _)| (provider, caps)));
        let _ = tx.send(result);
    });
    let result = rx
        .await
        .map_err(|_| Error::new("SERVICE_SHUTDOWN", "接入检查已结束。"))?;
    drop(guard);
    if let Ok((provider, _)) = &result {
        state.check_route_loop(&provider.base_url).await?;
    }
    result
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConnectionTestInput {
    model: Option<String>,
}
pub(super) async fn test_connection(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<ConnectionTestInput>,
) -> Result<Json<Value>> {
    if input.model.as_ref().is_some_and(|model| {
        model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
    }) {
        return Err(Error::new("INVALID_INPUT", "测试模型 ID 格式无效。"));
    }
    let setup = state.account_setup.lock().await;
    let lease = state
        .account_lease(id, "diagnostic", "connection_test")
        .await?;
    drop(setup);
    let cancel = state.shutdown.child_token();
    let guard = crate::inference::CancelOnDrop(cancel.clone());
    let registration = state.profiles.register_inference(id, cancel.clone())?;
    let task_state = state.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let result = task_state
            .profiles
            .inspect_route(id, RouteCheck::Connection { model: input.model }, cancel)
            .await;
        let result = match result {
            Ok((provider, _, Some(outcome))) => {
                let settled = lease.finish_execution(&outcome, None);
                if let Some(error) = outcome.error() {
                    Err(error)
                } else {
                    settled.map(|()| json!({"ok":true,"model":provider.model}))
                }
            }
            Ok(_) => lease.complete(Err(Error::new(
                "CONNECTION_TEST",
                "连接测试未返回模型结果。",
            ))),
            Err(error) => lease.complete(Err(error)),
        };
        // Diagnostics never publish a route or alter its model, enabled state or health.
        let _ = tx.send(result);
    });
    let result = rx
        .await
        .map_err(|_| Error::new("SERVICE_SHUTDOWN", "连接测试已结束。"))??;
    drop(guard);
    Ok(Json(result))
}
pub(super) async fn check_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let lease = state
        .operations
        .acquire(Some(id), None, "native_check", "account_check")
        .await?;
    Ok(Json(lease.complete(
        if id == crate::operations::DESKTOP_ACCOUNT {
            state.desktop_control.check_api().await
        } else {
            state.profiles.check_native_account(id).await
        },
    )?))
}
pub(super) async fn check_group(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let lease = state
        .operations
        .acquire(None, Some(id), "native_check", "group_check")
        .await?;
    let account = lease
        .account_id
        .ok_or_else(|| Error::new("NO_ELIGIBLE_ACCOUNT", "分组没有可用账户。"))?;
    Ok(Json(lease.complete(
        if account == crate::operations::DESKTOP_ACCOUNT {
            state.desktop_control.check_api().await
        } else {
            state.profiles.check_native_account(account).await
        },
    )?))
}

pub(super) async fn capabilities(State(state): State<Arc<AdminState>>) -> Json<Value> {
    let mut report = state.operations.capabilities();
    report["responsesAdapter"] = crate::responses::diagnostics::report().await;
    Json(report)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FeatureInput {
    revision: u64,
    disabled_features: Vec<crate::operations::Feature>,
}

#[cfg(test)]
mod current_contract_tests {
    use super::FeatureInput;
    use crate::operations::GroupInput;
    use serde_json::json;

    #[test]
    fn management_inputs_accept_only_the_current_policy_fields() {
        assert!(
            serde_json::from_value::<FeatureInput>(json!({"revision":1,"disabledFeatures":[]}))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<FeatureInput>(json!({"revision":1,"features":[]})).is_err()
        );
        assert!(
            serde_json::from_value::<FeatureInput>(
                json!({"revision":1,"disabledFeatures":[],"features":[]})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<GroupInput>(
                json!({"revision":1,"name":"group","enabled":true})
            )
            .is_ok()
        );
        assert!(
            serde_json::from_value::<GroupInput>(
                json!({"revision":1,"name":"group","enabled":true,"maxConcurrency":4})
            )
            .is_err()
        );
    }
}
pub(super) async fn set_features(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<FeatureInput>,
) -> Result<Json<Value>> {
    state
        .operations
        .set_disabled_features(id, input.revision, input.disabled_features)?;
    Ok(Json(state.operations_snapshot()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyFeatureInput {
    feature: crate::operations::Feature,
}
pub(super) async fn verify_feature(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<VerifyFeatureInput>,
) -> Result<Json<Value>> {
    let setup = state.account_setup.lock().await;
    let lease = state
        .account_lease(id, "diagnostic", "verify_feature")
        .await?;
    drop(setup);
    let cancel = state.shutdown.child_token();
    let _guard = crate::inference::CancelOnDrop(cancel.clone());
    let registration = state.profiles.register_inference(id, cancel.clone())?;
    let operations = state.operations.clone();
    let task = state.tasks.clone().spawn(async move {
        let _registration = registration;
        lease.complete(
            state
                .profiles
                .verify_feature(id, &state.operations, input.feature, cancel)
                .await,
        )
    });
    task.await
        .map_err(|_| Error::new("CAPABILITY_PROBE_FAILED", "原生能力验证任务中断。"))??;
    let snapshot = operations.snapshot();
    let enabled = snapshot["accounts"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["id"] == id.to_string()))
        .is_some_and(|a| {
            a["features"]
                .as_array()
                .is_some_and(|f| f.contains(&json!(input.feature)))
        });
    Ok(Json(json!({"verified":true,"enabled":enabled})))
}
