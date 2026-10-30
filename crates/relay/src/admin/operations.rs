//! Scheduling settings, groups, queue controls and request statistics.

use super::{AdminState, RevisionInput};
use crate::error::{Error, Result};
use crate::operations::{AccountInput, GroupInput, QueueQuery, StatsQuery};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) async fn operations(State(state): State<Arc<AdminState>>) -> Json<Value> {
    Json(state.operations_snapshot())
}
pub(super) async fn statistics(
    State(state): State<Arc<AdminState>>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        state
            .operations
            .run_history_query(move |ops| ops.statistics(query))
            .await?,
    ))
}
pub(super) async fn account_metrics(
    State(state): State<Arc<AdminState>>,
    Query(query): Query<crate::operations::AccountMetricsQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        state
            .operations
            .run_history_query(move |ops| ops.account_metrics(query))
            .await?,
    ))
}
pub(super) async fn request_content(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(state.operations.request_content(id)?))
}

pub(super) async fn debug_settings(State(state): State<Arc<AdminState>>) -> Json<Value> {
    Json(state.operations.debug_settings())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DebugInput {
    enabled: bool,
    revision: u64,
}
pub(super) async fn set_debug(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<DebugInput>,
) -> Result<Json<Value>> {
    Ok(Json(
        state.operations.set_debug(input.enabled, input.revision)?,
    ))
}
pub(super) async fn queue(
    State(state): State<Arc<AdminState>>,
    Query(query): Query<QueueQuery>,
) -> Result<Json<Value>> {
    Ok(Json(state.operations.queue(query)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClearRequestHistory {
    confirmed: bool,
}
pub(super) async fn clear_request_history(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<ClearRequestHistory>,
) -> Result<Json<Value>> {
    if !input.confirmed {
        return Err(Error::new("INVALID_INPUT", "请先确认清空所有请求记录。"));
    }
    let deleted = state.operations.clear_request_history()?;
    Ok(Json(json!({"deleted":deleted})))
}
pub(super) async fn cancel_request(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(state.operations.cancel_queued(id)?))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ConcurrencyInput {
    revision: u64,
    global_limit: Option<u32>,
    global_limit_auto: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct QueueInput {
    revision: u64,
    queue_limit: u32,
    queue_timeout_seconds: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NativeAffinityInput {
    revision: u64,
    native_affinity_ttl_hours: u32,
}
pub(super) async fn set_native_affinity(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<NativeAffinityInput>,
) -> Result<Json<Value>> {
    state
        .operations
        .set_native_affinity_ttl(input.revision, input.native_affinity_ttl_hours)?;
    Ok(Json(state.operations_snapshot()))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RetentionInput {
    revision: u64,
    retention_days: u32,
}
pub(super) async fn set_retention(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<RetentionInput>,
) -> Result<Json<Value>> {
    state
        .operations
        .set_retention(input.revision, input.retention_days)?;
    Ok(Json(state.operations_snapshot()))
}
pub(super) async fn set_queue(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<QueueInput>,
) -> Result<Json<Value>> {
    state.operations.set_queue(
        input.revision,
        input.queue_limit,
        input.queue_timeout_seconds,
    )?;
    Ok(Json(state.operations_snapshot()))
}
pub(super) async fn set_concurrency(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<ConcurrencyInput>,
) -> Result<Json<Value>> {
    if input.global_limit_auto {
        if input.global_limit.is_some() {
            return Err(Error::new(
                "INVALID_INPUT",
                "自动模式无需设置服务总并发数值。",
            ));
        }
        state.operations.set_global_auto(input.revision)?;
    } else {
        let limit = input
            .global_limit
            .ok_or_else(|| Error::new("INVALID_INPUT", "手动模式需要设置服务总并发上限。"))?;
        state.operations.set_global(input.revision, limit)?;
    }
    Ok(Json(state.operations_snapshot()))
}
pub(super) async fn create_group(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<GroupInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let id = state.operations.save_group(None, input)?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}
pub(super) async fn update_group(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<GroupInput>,
) -> Result<Json<Value>> {
    state.operations.save_group(Some(id), input)?;
    Ok(Json(state.operations_snapshot()))
}
pub(super) async fn delete_group(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<RevisionInput>,
) -> Result<Json<Value>> {
    state.operations.delete_group(id, input.revision)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AccountPolicyInput {
    revision: u64,
    group_ids: Vec<uuid::Uuid>,
    enabled: bool,
    max_concurrency: u32,
}
pub(super) async fn account_policy(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<AccountPolicyInput>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    let groups = input.group_ids;
    state.operations.set_account_groups(
        id,
        AccountInput {
            revision: input.revision,
            group_id: groups.first().copied().unwrap_or_default(),
            enabled: input.enabled,
            max_concurrency: input.max_concurrency,
        },
        groups,
    )?;
    Ok(Json(state.operations_snapshot()))
}
