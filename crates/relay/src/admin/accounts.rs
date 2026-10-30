//! Account creation/deletion, worker controls and credential management.

use super::{AdminState, RevisionInput};
use crate::error::{Error, Result};
use crate::operations::Lease;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NewAccount {
    name: String,
    source: Option<String>,
    upstream: Option<crate::upstream::Input>,
    #[serde(default)]
    native_config: bool,
}
pub(super) async fn create_account(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<NewAccount>,
) -> Result<(StatusCode, Json<Value>)> {
    let _setup = state.account_setup.lock().await;
    let profile = if input.source.as_deref() == Some("api-key") {
        if input.native_config {
            return Err(Error::new(
                "INVALID_INPUT",
                "API Key 与本机配置不能同时指定。",
            ));
        }
        let config = input
            .upstream
            .ok_or_else(|| Error::new("INVALID_INPUT", "缺少上游配置。"))?
            .validate(None)?;
        state.check_route_loop(&config.base_url).await?;
        state.profiles.create_upstream(&input.name, config).await?
    } else if input.source.is_some() || input.upstream.is_some() {
        return Err(Error::new("INVALID_INPUT", "账户来源或上游配置无效。"));
    } else if input.native_config {
        state.profiles.create_native_config(&input.name).await?
    } else {
        state.profiles.create(&input.name).await?
    };
    state
        .operations
        .register_accounts(&[(profile.id, profile.name.clone())])?;
    if profile.source == "api-key" {
        let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
        state
            .operations
            .set_account_activation(profile.id, revision, false, None)?;
    }
    Ok((StatusCode::CREATED, Json(serde_json::to_value(profile)?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpstreamUpdate {
    revision: u64,
    #[serde(flatten)]
    upstream: crate::upstream::Input,
}
pub(super) async fn update_upstream(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<UpstreamUpdate>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    let old_route = state.operations.activation_provider(id, input.revision)?;
    let config = state.profiles.validate_upstream(id, input.upstream).await?;
    let previous = state.profiles.upstream(id).await?;
    if config == previous {
        return Ok(Json(config.view()));
    }
    state.operations.require_account_idle(id)?;
    state.check_route_loop(&config.base_url).await?;
    // The maintenance lease prevents admission while disk and route identity change.
    let lease = state
        .operations
        .begin_account_maintenance(id, "management", "upstream_update")?;
    state.profiles.write_upstream(id, &config).await?;
    lease.complete(Ok(()))?;
    if let Err(error) = state.operations.set_inference_route(
        id,
        input.revision,
        old_route.map(|_| config.provider()),
    ) {
        state.profiles.write_upstream(id, &previous).await?;
        return Err(error);
    }
    state.upstream_connections.invalidate(id).await;
    state.responses.invalidate_account(id);
    Ok(Json(config.view()))
}
pub(super) async fn delete_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<RevisionInput>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    state.operations.begin_account_delete(id, input.revision)?;
    state.profiles.delete(id).await?;
    state.upstream_connections.invalidate(id).await;
    state.operations.finish_account_delete(id)?;
    state.responses.invalidate_account(id);
    Ok(Json(json!({"ok":true})))
}
pub(super) async fn start_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let lease = state.account_lease(id, "management", "start").await?;
    let _setup = state.account_setup.lock().await;
    lease.complete(state.profiles.start(id).await)?;
    Ok(Json(json!({"ok":true})))
}
pub(super) async fn stop_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    state.operations.cancel_proxy_restart(id);
    // Cleanup remains available even when a group is paused or its quota is full.
    let lease = maintenance_lease(&state, id, "stop")?;
    let result = state.profiles.stop(id).await;
    maintenance_result(lease, result)
}
pub(super) async fn restart_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    state.profiles.require_no_login(id)?;
    if state.operations.proxy_blocked(id) {
        return Err(Error::new("BUSY", "请在账户代理设置中重试切换。"));
    }
    let lease = maintenance_lease(&state, id, "restart")?;
    let result = async {
        state.profiles.stop(id).await?;
        state.profiles.start(id).await
    }
    .await;
    maintenance_result(lease, result)
}
fn maintenance_lease(state: &AdminState, id: uuid::Uuid, action: &str) -> Result<Option<Lease>> {
    match state
        .operations
        .begin(Some(id), None, "management", action, false)
    {
        Ok(lease) => Ok(Some(lease)),
        Err(error) if error.code == "OPERATIONS_STORAGE" => Ok(None),
        Err(error) => Err(error),
    }
}
fn maintenance_result(lease: Option<Lease>, result: Result<()>) -> Result<Json<Value>> {
    let succeeded = result.is_ok();
    let operation_error = result.as_ref().err().cloned();
    let recorded = if let Some(lease) = lease {
        match lease.complete(result) {
            Ok(()) => true,
            Err(error) if succeeded && error.code == "OPERATIONS_STORAGE" => false,
            Err(error) => return Err(operation_error.unwrap_or(error)),
        }
    } else {
        result?;
        false
    };
    Ok(Json(
        json!({"ok":true,"recorded":recorded,"warning":if recorded {None} else {Some("操作已完成，但统计存储异常，本次结果未能可靠记录。")}}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Login {
    kind: String,
}
pub(super) async fn login_account(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<Login>,
) -> Result<Json<Value>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tasks.clone().spawn(async move {
        let result = async {
            let _setup = state.account_setup.lock().await;
            require_oauth_profile(&state, id).await?;
            state.profiles.check_login_start(id, &input.kind).await?;
            state.operations.require_account_idle(id)?;
            state.operations.cancel_proxy_restart(id);
            if let Some((_, cancel)) = state.account_stops.lock().unwrap().remove(&id) {
                cancel.cancel();
            }
            state.operations.invalidate_credentials(id)?;
            let lease = state
                .operations
                .begin_account_maintenance(id, "management", "login")?;
            Ok(Json(
                lease.complete(state.profiles.login(id, &input.kind).await)?,
            ))
        }
        .await;
        let _ = tx.send(result);
    });
    rx.await
        .map_err(|_| Error::new("LOGIN_START_FAILED", "登录启动中断，请刷新状态后重试。"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct CancelLogin {
    login_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct LoginQuery {
    login_id: Option<String>,
}
pub(super) async fn login_status(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<LoginQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        state
            .profiles
            .login_status(id, query.login_id.as_deref())
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct LoginCallback {
    login_id: String,
    callback_url: String,
}
pub(super) async fn login_callback(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<LoginCallback>,
) -> Result<(StatusCode, Json<Value>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(
            state
                .profiles
                .submit_login_callback(id, &input.login_id, &input.callback_url)
                .await?,
        ),
    ))
}
pub(super) async fn cancel_login(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<CancelLogin>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    let lease = maintenance_lease(&state, id, "cancel_login")?;
    let result = state.profiles.cancel_login(id, &input.login_id).await;
    maintenance_result(lease, result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Credential {
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
    document: Option<Value>,
}
pub(super) async fn import_token(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<Credential>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    require_oauth_profile(&state, id).await?;
    state.profiles.require_no_login(id)?;
    let document = match (
        input.document,
        input.access_token,
        input.refresh_token,
        input.account_id,
    ) {
        (Some(document), None, None, None) => document,
        (None, Some(token), None, account) => json!({"accessToken":token,"accountId":account}),
        (None, None, Some(token), account) => json!({"refreshToken":token,"accountId":account}),
        _ => {
            return Err(Error::new(
                "INVALID_INPUT",
                "请选择凭据文件、Access Token 或 Refresh Token 导入。",
            ));
        }
    };
    state.operations.require_account_idle(id)?;
    let lease =
        state
            .operations
            .begin_account_maintenance(id, "management", "import_credentials")?;
    let result = async {
        // Reserve the account before consuming RT, but keep its route/files on exchange failure.
        let document = crate::oauth::prepare_import(&document, &state.operations.account_proxy(id))
            .await?
            .native_document();
        lease.invalidate_import_credentials()?;
        state.profiles.import_credentials(id, document).await
    }
    .await;
    Ok(Json(lease.complete(result)?))
}
pub(super) async fn link_desktop_credentials(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    require_oauth_profile(&state, id).await?;
    state.profiles.require_no_login(id)?;
    state.operations.invalidate_credentials(id)?;
    let lease = state
        .account_lease(id, "management", "link_desktop_credentials")
        .await?;
    Ok(Json(lease.complete(state.profiles.link_desktop(id).await)?))
}
pub(super) async fn logout_credentials(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    require_oauth_profile(&state, id).await?;
    state.operations.invalidate_credentials(id)?;
    let lease = maintenance_lease(&state, id, "logout_credentials")?;
    maintenance_result(lease, state.profiles.logout_credentials(id).await)
}
async fn require_oauth_profile(state: &AdminState, id: uuid::Uuid) -> Result<()> {
    if !state.profiles.is_oauth(id).await? {
        return Err(Error::new(
            "INVALID_INPUT",
            "此操作仅适用于独立 OAuth 账户。",
        ));
    }
    Ok(())
}
