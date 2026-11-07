//! Managed Linux Codex operations share diagnostic exclusion, not inference capacity.
use super::AdminState;
use crate::{
    error::{Error, Result},
    installation::managed::{Action, Run},
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    action: Action,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CancelInput {
    id: Uuid,
}

struct Busy(Arc<AdminState>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

pub(super) async fn start(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<Input>,
) -> Result<(StatusCode, Json<Value>)> {
    let id = submit(&state, input).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}

pub(super) async fn cancel(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<CancelInput>,
) -> Result<Json<Value>> {
    state.native_runtime.cancel(input.id)?;
    Ok(Json(json!({"id":input.id})))
}

async fn submit(state: &Arc<AdminState>, input: Input) -> Result<Uuid> {
    state
        .native_runtime
        .validate_action(input.action, input.version.as_deref())?;
    if state.shutdown.is_cancelled() {
        return Err(Error::new("SERVICE_DRAINING", "服务正在停止。"));
    }
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err(Error::new("BUSY", "已有原生环境操作在运行，请等待完成。"));
    }
    let busy = Busy(state.clone());
    let run = state
        .native_runtime
        .begin(input.action, &state.shutdown)
        .await?;
    let id = run.id();
    let owner = state.clone();
    state.tasks.spawn(async move {
        let _busy = busy;
        let result = execute(&owner, &run, input).await;
        run.finish(result);
    });
    Ok(id)
}

async fn execute(state: &Arc<AdminState>, run: &Run, input: Input) -> Result<String> {
    let manager = &state.native_runtime;
    match input.action {
        Action::Check => {
            let version = manager.check(run).await?;
            Ok(format!("检查完成，官方稳定版为 {version}。"))
        }
        Action::Install | Action::Activate => {
            let installation = if input.action == Action::Install {
                manager.install(run, input.version.as_deref()).await?
            } else {
                manager
                    .validate_installed(run, input.version.as_deref().unwrap())
                    .await?
            };
            run.phase("activating", "启动校验通过，正在启用版本…");
            let result = state.operations.with_idle_native_change(|| {
                state
                    .profiles
                    .with_no_inference(|| manager.activate(run, &installation))
            });
            match result {
                Ok(()) => {
                    // Account start still follows the existing account enable switch and pool.
                    super::activation::start_enabled(state);
                    Ok(format!(
                        "{} 已启用。已启用的账户将由进程池启动；其他账户可在账户管理中启用。",
                        installation
                            .app_server_version
                            .as_deref()
                            .unwrap_or("Codex")
                    ))
                }
                Err(error) if error.code == "BUSY" && input.action == Action::Install => {
                    Ok(format!(
                        "{} 已安装并通过启动校验。还有请求或工具回调在进行，请空闲后点击启用。",
                        installation
                            .app_server_version
                            .as_deref()
                            .unwrap_or("Codex")
                    ))
                }
                Err(error) => Err(error),
            }
        }
        Action::Remove => {
            let version = input.version.as_deref().unwrap();
            run.phase("removing", "正在删除旧版本…");
            let directory = manager.version_directory(version)?;
            let trash = state.operations.with_idle_native_change(|| {
                state.profiles.with_no_inference(|| {
                    if state.profiles.native_path_in_use(&directory) {
                        return Err(Error::new(
                            "CODEX_VERSION_IN_USE",
                            "仍有账户进程使用此版本，请先停止相关账户实例后再删除。",
                        ));
                    }
                    manager.detach(run, version)
                })
            })?;
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(trash))
                .await
                .map_err(|_| {
                    Error::new("CODEX_REMOVE", "版本已移出列表，但文件清理任务异常退出。")
                })??;
            Ok(format!("Codex {version} 已删除。"))
        }
    }
}

pub(super) fn maintain(state: &Arc<AdminState>) {
    let weak = Arc::downgrade(state);
    let shutdown = state.shutdown.clone();
    state.tasks.spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(60));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { _ = shutdown.cancelled() => break, _ = ticks.tick() => {} }
            let Some(state) = weak.upgrade() else { break };
            if state.native_runtime.due(crate::now_ms()) {
                // Checks are daily (with bounded retries on failure); installation always
                // requires an explicit action. Never upgrade from this background task.
                if let Err(error) = submit(
                    &state,
                    Input {
                        action: Action::Check,
                        version: None,
                    },
                )
                .await
                {
                    if !matches!(error.code.as_str(), "BUSY" | "SERVICE_DRAINING") {
                        eprintln!("codex_update_check_failed: {}", error.code);
                    }
                }
            }
        }
    });
}
