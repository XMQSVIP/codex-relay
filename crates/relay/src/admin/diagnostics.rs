//! Desktop debugging, overview reports and tracked diagnostic jobs.

use super::AdminState;
use crate::{
    desktop,
    error::{Error, Result},
    installation, probe,
};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: uuid::Uuid,
    pub kind: String,
    pub state: String,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub report: Option<Value>,
    pub error: Option<Error>,
}
pub(super) async fn start_desktop(State(state): State<Arc<AdminState>>) -> Result<Json<Value>> {
    Ok(Json(state.desktop_control.start(&state.shutdown).await?))
}
pub(super) async fn stop_desktop(State(state): State<Arc<AdminState>>) -> Json<Value> {
    Json(state.desktop_control.stop().await)
}
pub(super) async fn overview(State(state): State<Arc<AdminState>>) -> Json<Value> {
    let (accounts, installation, automatic_proxy, desktop) = tokio::join!(
        state.profiles.list(),
        installation::read(),
        crate::app_server_proxy::ProxyConfig::automatic_view(),
        async { state.desktop_control.status() }
    );
    let mut reports = serde_json::Map::new();
    for kind in ["shared", "inspect-provider", "doctor"] {
        if let Ok(bytes) = tokio::fs::read(format!(".cache/rust-{kind}-probe.json")).await {
            if bytes.len() <= 1024 * 1024 {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    reports.insert(kind.into(), value);
                }
            }
        }
    }
    Json(
        json!({"implementation":"rust","version":env!("CARGO_PKG_VERSION"),"inferenceEnabled":state.operations.has_inference_routes(),"installation":installation.ok(),"automaticProxy":automatic_proxy,"desktop":desktop,"accounts":accounts,"operations":state.operations_snapshot(),"jobs":state.jobs.lock().await.clone(),"reports":reports,"busy":state.busy.load(Ordering::SeqCst)}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewJob {
    kind: String,
    paths: Option<installation::CodexPath>,
}
struct BusyGuard(Arc<AdminState>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}
pub(super) async fn start_job(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<NewJob>,
) -> Result<(StatusCode, Json<Value>)> {
    if ![
        "shared",
        "guard",
        "stream",
        "provider",
        "inspect-provider",
        "doctor",
        "prepare",
    ]
    .contains(&input.kind.as_str())
    {
        return Err(Error::new("INVALID_INPUT", "不支持的诊断操作。"));
    }
    if let Some(paths) = &input.paths {
        if input.kind != "prepare" {
            return Err(Error::new("INVALID_INPUT", "仅检测安装支持指定路径。"));
        }
        paths.validate()?;
    }
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err(Error::new("BUSY", "已有诊断在运行，请等待完成。"));
    }
    let busy = BusyGuard(state.clone());
    let lease = state
        .operations
        .acquire(None, None, "diagnostic", &input.kind)
        .await?;
    let job = Job {
        id: uuid::Uuid::new_v4(),
        kind: input.kind.clone(),
        state: "running".into(),
        created_at: crate::now_ms(),
        finished_at: None,
        report: None,
        error: None,
    };
    let id = job.id;
    {
        let mut jobs = state.jobs.lock().await;
        if jobs.len() >= 20 {
            jobs.remove(0);
        }
        jobs.push(job);
    }
    state.tasks.clone().spawn(async move {
        let _busy = busy;
        let raw = run_job_with_paths(&input.kind, input.paths).await;
        let passed = raw.as_ref().is_ok_and(|value| {
            !value["checks"]
                .as_array()
                .is_some_and(|checks| checks.iter().any(|c| c["state"] == "blocked"))
        });
        let settled = lease.complete(if passed {
            Ok(())
        } else {
            Err(Error::new("DIAGNOSTIC_FAILED", "诊断未通过。"))
        });
        let result = if settled
            .as_ref()
            .err()
            .is_some_and(|e| e.code == "OPERATIONS_STORAGE")
        {
            Err(settled.unwrap_err())
        } else {
            raw
        };
        let mut jobs = state.jobs.lock().await;
        if let Some(job) = jobs.iter_mut().find(|job| job.id == id) {
            job.finished_at = Some(crate::now_ms());
            match result {
                Ok(value) => {
                    let blocked = value["checks"]
                        .as_array()
                        .is_some_and(|checks| checks.iter().any(|c| c["state"] == "blocked"));
                    job.state = if blocked { "failed" } else { "completed" }.into();
                    job.report = Some(value);
                }
                Err(error) => {
                    job.state = "failed".into();
                    job.error = Some(error);
                }
            }
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}
pub async fn run_job(kind: &str) -> Result<Value> {
    run_job_with_paths(kind, None).await
}

async fn run_job_with_paths(kind: &str, paths: Option<installation::CodexPath>) -> Result<Value> {
    let value = match kind {
        "doctor" => desktop::doctor().await?,
        "prepare" => serde_json::to_value(match paths {
            Some(paths) => installation::prepare_codex(paths).await?,
            None => installation::prepare(None).await?,
        })?,
        _ => {
            let install = installation::read().await?;
            installation::validate(&install).await?;
            match kind {
                "shared" => serde_json::to_value(probe::shared(&install).await?)?,
                "guard" | "provider" => serde_json::to_value(probe::guard(&install).await?)?,
                "stream" => serde_json::to_value(probe::stream(&install).await?)?,
                "inspect-provider" => probe::inspect_provider(&install).await?,
                _ => return Err(Error::new("INVALID_INPUT", "诊断类型无效。")),
            }
        }
    };
    tokio::fs::create_dir_all(".cache").await?;
    tokio::fs::write(
        format!(".cache/rust-{kind}-probe.json"),
        serde_json::to_vec_pretty(&value)?,
    )
    .await?;
    Ok(value)
}
