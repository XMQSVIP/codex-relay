//! API key management and authorized endpoint discovery.

use super::{AdminState, RevisionInput};
use crate::{
    error::Result,
    operations::{ApiKeyCreate, ApiKeyCreated, ApiKeyUpdate, ApiKeyView},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) async fn api_keys(State(state): State<Arc<AdminState>>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"keys":state.operations.list_api_keys()?,"baseUrl":state.network_endpoints.first().map(|e|e.base_url.clone()).unwrap_or_else(||format!("{}/v1",state.origin)),"localBaseUrl":format!("{}/v1",state.origin),"lanBaseUrls":crate::network::lan_base_urls(&state.origin),"networkEndpoints":state.network_endpoints,"inferenceEnabled":state.operations.has_inference_routes()}),
    ))
}
pub(super) async fn create_api_key(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<ApiKeyCreate>,
) -> Result<(StatusCode, Json<ApiKeyCreated>)> {
    Ok((
        StatusCode::CREATED,
        Json(state.operations.create_api_key(input)?),
    ))
}
pub(super) async fn export_api_keys(
    State(state): State<Arc<AdminState>>,
) -> Result<Json<crate::operations::ApiKeyBackup>> {
    Ok(Json(state.operations.export_api_keys()?))
}
pub(super) async fn import_api_keys(
    State(state): State<Arc<AdminState>>,
    input: std::result::Result<
        Json<crate::operations::ApiKeyImport>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Json<crate::operations::ApiKeyImportResult>> {
    let Json(input) = input.map_err(|_| {
        crate::error::Error::new(
            "INVALID_INPUT",
            "备份字段不完整、类型错误或文件超过 8 MiB，请使用“全部导出”生成的 JSON 备份。",
        )
    })?;
    Ok(Json(state.operations.import_api_keys(input)?))
}
pub(super) async fn update_api_key(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<ApiKeyUpdate>,
) -> Result<Json<ApiKeyView>> {
    let invalidate = input.state.as_deref() == Some("revoked");
    let result = state.operations.update_api_key(id, input)?;
    if invalidate {
        state.responses.invalidate_owner(id);
    }
    Ok(Json(result))
}
pub(super) async fn set_api_key_group(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<crate::operations::ApiKeyGroup>,
) -> Result<Json<ApiKeyView>> {
    let result = state.operations.set_api_key_group(id, input)?;
    Ok(Json(result))
}
pub(super) async fn delete_api_key(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<RevisionInput>,
) -> Result<Json<Value>> {
    state.operations.delete_api_key(id, input.revision)?;
    state.responses.invalidate_owner(id);
    Ok(Json(json!({"deleted":true})))
}
pub(super) async fn set_api_key_concurrency(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<crate::operations::ApiKeyConcurrency>,
) -> Result<Json<ApiKeyView>> {
    Ok(Json(state.operations.set_api_key_concurrency(id, input)?))
}
