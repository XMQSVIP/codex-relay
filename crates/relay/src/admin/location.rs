use super::AdminState;
use crate::{
    account_location::{Config, Source},
    error::{Error, Result},
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    revision: u64,
    config: Config,
    #[serde(default, rename = "previewId")]
    preview_id: Option<Uuid>,
    #[serde(default)]
    retry: bool,
}

pub(super) async fn options() -> Json<Value> {
    Json(
        json!({"timezones":chrono_tz::TZ_VARIANTS.iter().map(|tz| tz.name()).collect::<Vec<_>>(),"countries":crate::account_location::COUNTRIES.split_whitespace().collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests;
pub(super) async fn save(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> Result<Json<Value>> {
    let _setup = state.account_setup.lock().await;
    state.profiles.require_no_login(id)?;
    let (before_config, proxy, _) = state.operations.location_config(id)?;
    if input.retry
        && (input.preview_id.is_some()
            || input.config != before_config
            || !state.operations.proxy_failed(id))
    {
        return Err(Error::new(
            "INVALID_INPUT",
            "只能重试应用当前保存且切换失败的设置。",
        ));
    }
    if input.config.enabled
        && input.config.source == Source::Proxy
        && !input.retry
        && input.preview_id.is_none()
    {
        return Err(Error::new(
            "LOCATION_PREVIEW_REQUIRED",
            "请先检测时区，确认预览结果后再保存。",
        ));
    }
    let preview = if let Some(preview_id) = input.preview_id {
        if !input.config.enabled || input.config.source != Source::Proxy {
            return Err(Error::new(
                "INVALID_INPUT",
                "只有跟随账号代理时才能保存检测预览。",
            ));
        }
        Some(
            state
                .operations
                .locations
                .preview_for_save(id, preview_id, proxy)
                .await?,
        )
    } else {
        None
    };
    let changed = preview.as_ref().is_some_and(|p| {
        state.operations.locations.view(id, &before_config)["effective"] != json!(p.location)
    });
    let generation =
        state
            .operations
            .set_account_location(id, input.revision, input.config, changed)?;
    if let Some(preview) = preview {
        state.operations.locations.accept_preview(id, preview);
    }
    if let Some(generation) = generation {
        let owner = state.clone();
        state.tasks.spawn(async move {
            super::proxy::apply(owner, id, generation).await;
        });
    }
    Ok(Json(state.operations_snapshot()))
}
pub(super) async fn detect(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<super::RevisionInput>,
) -> Result<Json<Value>> {
    let (proxy, token) = {
        let _setup = state.account_setup.lock().await;
        if input.revision != state.operations.snapshot()["revision"].as_u64().unwrap() {
            return Err(Error::new("CONFIG_CONFLICT", "配置已更新，请刷新后重试。"));
        }
        let (_, proxy, token) = state.operations.location_config(id)?;
        if id == crate::operations::DESKTOP_ACCOUNT {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面共享调试连接不支持位置检测。",
            ));
        }
        if state.operations.proxy_blocked(id) {
            return Err(Error::new("BUSY", "账户配置正在切换，请稍后重试。"));
        }
        (proxy, token)
    };
    let preview = tokio::select! {
        _ = state.shutdown.cancelled() => return Err(Error::new("SERVICE_DRAINING", "服务正在停止。")),
        result = state.operations.locations.preview(id, token, proxy.clone()) => result?,
    };
    let _setup = state.account_setup.lock().await;
    let (_, current_proxy, current_token) = state.operations.location_config(id)?;
    if current_proxy != proxy
        || current_token != token
        || input.revision != state.operations.snapshot()["revision"].as_u64().unwrap()
    {
        return Err(Error::new(
            "CONFIG_CONFLICT",
            "配置已更新，请刷新后重新检测。",
        ));
    }
    Ok(Json(json!({"preview":preview,"revision":input.revision})))
}
