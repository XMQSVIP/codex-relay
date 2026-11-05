//! Versioned Linux Codex installations. Downloads never overwrite a running binary.
use super::{Installation, InstallationPaths, inspect_paths, save_installation};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod archive;
mod download;
#[cfg(test)]
mod tests;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const RETRY_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Action {
    Check,
    Install,
    Activate,
    Remove,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Operation {
    pub id: Uuid,
    pub action: Action,
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VersionView {
    pub version: String,
    pub installed_at: u64,
    pub in_use: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub supported: bool,
    pub architecture: Option<String>,
    pub active_version: Option<String>,
    pub latest_version: Option<String>,
    pub download_url: Option<String>,
    pub checked_at: Option<u64>,
    pub last_attempt_at: Option<u64>,
    pub versions: Vec<VersionView>,
    pub operation: Option<Operation>,
    pub error: Option<Error>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cache {
    latest_version: Option<String>,
    checked_at: Option<u64>,
    attempted_at: Option<u64>,
    failed: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Release {
    version: String,
    installed_at: u64,
    executable: String,
    sha256: String,
    integrity: String,
}

struct Runtime {
    cache: Cache,
    releases: Vec<Release>,
    operation: Option<Operation>,
    cancel: Option<CancellationToken>,
    error: Option<Error>,
}

pub(crate) struct Manager {
    root: PathBuf,
    config: PathBuf,
    architecture: Option<&'static str>,
    runtime: Mutex<Runtime>,
    registry: download::Registry,
}

pub(crate) struct Run {
    manager: Arc<Manager>,
    id: Uuid,
    pub cancel: CancellationToken,
    original_config: Option<Vec<u8>>,
    _lock: File,
    finished: bool,
}

impl Manager {
    pub(crate) fn open(root: PathBuf, config: PathBuf) -> Self {
        let architecture = if cfg!(target_os = "linux") {
            match std::env::consts::ARCH {
                "x86_64" => Some("x64"),
                "aarch64" => Some("arm64"),
                _ => None,
            }
        } else {
            None
        };
        let mut error = None;
        let cache = match read_json::<Cache>(&root.join("check.json")) {
            Ok(Some(cache))
                if cache
                    .latest_version
                    .as_deref()
                    .is_none_or(|v| version(v).is_some()) =>
            {
                cache
            }
            Ok(None) => Cache::default(),
            _ => {
                error = Some(Error::new(
                    "CODEX_STATE",
                    "更新检查记录无法读取，请重新检查更新。",
                ));
                Cache::default()
            }
        };
        let mut releases = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root.join("versions")) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if version(&name).is_some() {
                    if let Ok(release) = read_release(&root, &name) {
                        releases.push(release);
                    }
                }
            }
        }
        Self {
            root,
            config,
            architecture,
            runtime: Mutex::new(Runtime {
                cache,
                releases,
                operation: None,
                cancel: None,
                error,
            }),
            registry: download::Registry::official(),
        }
    }

    pub(crate) fn snapshot(
        &self,
        installation: Option<&Installation>,
        in_use: impl Fn(&Path) -> bool,
    ) -> View {
        let selected = installation.map(|install| {
            install
                .app_server_executable
                .canonicalize()
                .unwrap_or_else(|_| install.app_server_executable.clone())
        });
        let state = self.runtime.lock().unwrap();
        let active_version = selected
            .as_ref()
            .and_then(|selected| {
                state.releases.iter().find(|release| {
                    self.root
                        .join("versions")
                        .join(&release.version)
                        .join(&release.executable)
                        .canonicalize()
                        .ok()
                        .as_ref()
                        == Some(selected)
                })
            })
            .map(|release| release.version.clone());
        let mut versions: Vec<_> = state
            .releases
            .iter()
            .map(|release| VersionView {
                version: release.version.clone(),
                installed_at: release.installed_at,
                in_use: in_use(&self.root.join("versions").join(&release.version)),
            })
            .collect();
        versions.sort_by_key(|release| std::cmp::Reverse(version(&release.version)));
        View {
            supported: self.architecture.is_some(),
            architecture: self.architecture.map(str::to_owned),
            active_version,
            latest_version: state.cache.latest_version.clone(),
            download_url: state
                .cache
                .latest_version
                .as_ref()
                .zip(self.architecture)
                .map(|(v, arch)| {
                    format!("https://registry.npmjs.org/@openai/codex/-/codex-{v}-linux-{arch}.tgz")
                }),
            checked_at: state.cache.checked_at,
            last_attempt_at: state.cache.attempted_at,
            versions,
            operation: state.operation.clone(),
            error: state.error.clone(),
        }
    }

    pub(crate) fn due(&self, now: u64) -> bool {
        if self.architecture.is_none() {
            return false;
        }
        let state = self.runtime.lock().unwrap();
        let delay = if state.cache.failed { RETRY_MS } else { DAY_MS };
        state.cancel.is_none()
            && state
                .cache
                .attempted_at
                .is_none_or(|at| now.checked_sub(at).is_none_or(|elapsed| elapsed >= delay))
    }

    pub(crate) fn validate_action(&self, action: Action, selected: Option<&str>) -> Result<()> {
        if selected.is_some_and(|value| version(value).is_none())
            || (action == Action::Check && selected.is_some())
            || (matches!(action, Action::Activate | Action::Remove) && selected.is_none())
        {
            return Err(Error::new("INVALID_INPUT", "请选择有效的 Codex 稳定版本。"));
        }
        Ok(())
    }

    pub(crate) async fn begin(
        self: &Arc<Self>,
        action: Action,
        shutdown: &CancellationToken,
    ) -> Result<Run> {
        self.validate_action(
            action,
            if matches!(action, Action::Activate | Action::Remove) {
                Some("0.0.0")
            } else {
                None
            },
        )?;
        if shutdown.is_cancelled() {
            return Err(cancelled());
        }
        tokio::fs::create_dir_all(&self.root).await?;
        crate::platform::protect_directory(&self.root).await?;
        tokio::fs::create_dir_all(self.root.join("versions")).await?;
        crate::platform::protect_directory(&self.root.join("versions")).await?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("install.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| Error::new("BUSY", "已有 Codex 安装任务在运行。"))?;
        let original_config = config_bytes(&self.config)?;
        let cancel = shutdown.child_token();
        let id = Uuid::new_v4();
        let mut state = self.runtime.lock().unwrap();
        if state.cancel.is_some() {
            return Err(Error::new("BUSY", "已有 Codex 安装任务在运行。"));
        }
        state.error = None;
        state.cancel = Some(cancel.clone());
        state.operation = Some(Operation {
            id,
            action,
            phase: "checking".into(),
            downloaded_bytes: 0,
            total_bytes: None,
            started_at: crate::now_ms(),
            finished_at: None,
            message: "正在准备…".into(),
        });
        Ok(Run {
            manager: self.clone(),
            id,
            cancel,
            original_config,
            _lock: lock,
            finished: false,
        })
    }

    pub(crate) fn cancel(&self, id: Uuid) -> Result<()> {
        let state = self.runtime.lock().unwrap();
        if state.operation.as_ref().is_none_or(|op| op.id != id) || state.cancel.is_none() {
            return Err(Error::new("NOT_FOUND", "该安装任务已结束。"));
        }
        if state
            .operation
            .as_ref()
            .is_some_and(|op| matches!(op.phase.as_str(), "activating" | "removing"))
        {
            return Err(Error::new("BUSY", "正在完成版本切换或删除，请等待完成。"));
        }
        state.cancel.as_ref().unwrap().cancel();
        Ok(())
    }

    pub(crate) async fn check(&self, run: &Run) -> Result<String> {
        run.phase("checking", "正在检查官方稳定版…");
        let now = crate::now_ms();
        {
            let mut state = self.runtime.lock().unwrap();
            state.cache.attempted_at = Some(now);
            state.cache.failed = true;
        }
        self.save_cache()?;
        let latest = self.registry.latest(&run.cancel).await?;
        {
            let mut state = self.runtime.lock().unwrap();
            state.cache.latest_version = Some(latest.clone());
            state.cache.checked_at = Some(crate::now_ms());
            state.cache.failed = false;
        }
        self.save_cache()?;
        Ok(latest)
    }

    fn save_cache(&self) -> Result<()> {
        let cache = self.runtime.lock().unwrap().cache.clone();
        write_json(&self.root.join("check.json"), &cache)
    }

    pub(crate) async fn install(&self, run: &Run, selected: Option<&str>) -> Result<Installation> {
        let latest;
        let selected = match selected {
            Some(value) => value,
            None => {
                latest = self.check(run).await?;
                &latest
            }
        };
        if self.root.join("versions").join(selected).exists() {
            return self.validate_installed(run, selected).await;
        }
        let architecture = self
            .architecture
            .ok_or_else(|| Error::new("CODEX_PLATFORM", "当前平台不支持自动安装。"))?;
        let package = self
            .registry
            .package(selected, architecture, &run.cancel)
            .await?;
        let required = package
            .unpacked_size
            .saturating_add(download::MAX_ARCHIVE_BYTES)
            .saturating_add(64 * 1024 * 1024);
        if fs2::available_space(&self.root)? < required {
            return Err(Error::new(
                "CODEX_DISK_SPACE",
                "磁盘空间不足，请先删除不再使用的旧版本或释放空间。",
            ));
        }
        if self.architecture.is_none() {
            return Err(Error::new(
                "CODEX_PLATFORM",
                "自动安装仅支持 Linux x64 / ARM64；当前平台请使用已有安装路径。",
            ));
        }
        let archive = self.registry.download(&package, &self.root, run).await?;
        run.phase("extracting", "校验通过，正在解压运行文件…");
        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&self.root)?;
        let cancel = run.cancel.clone();
        let target = download::target(architecture)?;
        let staging = tokio::task::spawn_blocking(move || {
            archive::extract(archive.path(), staging.path(), target, &cancel)?;
            Ok::<_, Error>(staging)
        })
        .await
        .map_err(|_| Error::new("CODEX_EXTRACT", "解压任务异常退出。"))??;
        run.check_cancelled()?;
        let executable = archive::executable(staging.path(), architecture)?;
        run.phase("starting", "正在隔离启动 Codex，校验版本和协议…");
        // Do not drop this validation future during cancellation: stop() must finish
        // before deleting its executable/resources. Failed probes keep their workspace.
        let prepared = inspect_paths(
            InstallationPaths {
                desktop_executable: None,
                archive: None,
                app_server_executable: staging.path().join(&executable),
            },
            true,
        )
        .await;
        let mut installation = match prepared {
            Ok(value) => value,
            Err(error) => {
                let _ = staging.keep();
                return Err(error);
            }
        };
        run.check_cancelled()?;
        if installation.app_server_version.as_deref()
            != Some(format!("codex-cli {selected}").as_str())
        {
            return Err(Error::new(
                "CODEX_VERSION",
                "下载程序的实际版本与官方发布记录不一致。",
            ));
        }
        let release = Release {
            version: selected.into(),
            installed_at: crate::now_ms(),
            executable,
            sha256: installation.app_server_sha256.clone(),
            integrity: package.integrity,
        };
        write_json(&staging.path().join("relay-release.json"), &release)?;
        let destination = self.root.join("versions").join(selected);
        std::fs::rename(staging.path(), &destination)?;
        installation.app_server_executable = destination.join(&release.executable);
        installation.prepared_at = release.installed_at.into();
        self.runtime.lock().unwrap().releases.push(release);
        Ok(installation)
    }

    pub(crate) async fn validate_installed(
        &self,
        run: &Run,
        selected: &str,
    ) -> Result<Installation> {
        run.phase("starting", "正在校验并启动已安装的 Codex…");
        let release = read_release(&self.root, selected)?;
        let directory = self.root.join("versions").join(selected);
        let executable = directory.join(&release.executable);
        if super::hash_file(&executable)? != release.sha256 {
            return Err(Error::new(
                "CODEX_INTEGRITY",
                "已安装程序的校验值不一致，未切换当前版本。",
            ));
        }
        let mut installation = inspect_paths(
            InstallationPaths {
                desktop_executable: None,
                archive: None,
                app_server_executable: executable,
            },
            true,
        )
        .await?;
        run.check_cancelled()?;
        if installation.app_server_version.as_deref()
            != Some(format!("codex-cli {selected}").as_str())
        {
            return Err(Error::new("CODEX_VERSION", "已安装程序的版本不匹配。"));
        }
        installation.prepared_at = release.installed_at.into();
        Ok(installation)
    }

    /// Called within the existing admission/registration locks, after all process I/O.
    pub(crate) fn activate(&self, run: &Run, installation: &Installation) -> Result<()> {
        run.check_cancelled()?;
        if config_bytes(&self.config)? != run.original_config {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "安装期间原生路径已被修改；新版已保留，请重新选择启用。",
            ));
        }
        std::fs::create_dir_all(self.config.parent().unwrap())?;
        save_installation(installation, &self.config)
    }

    pub(crate) fn version_directory(&self, selected: &str) -> Result<PathBuf> {
        read_release(&self.root, selected)?;
        Ok(self.root.join("versions").join(selected))
    }

    /// Rename while admission is locked; the slower recursive removal follows outside it.
    pub(crate) fn detach(&self, run: &Run, selected: &str) -> Result<PathBuf> {
        run.check_cancelled()?;
        let directory = self.version_directory(selected)?;
        if let Some(bytes) = config_bytes(&self.config)? {
            let installation: Installation = serde_json::from_slice(&bytes)?;
            let selected = installation
                .app_server_executable
                .canonicalize()
                .unwrap_or(installation.app_server_executable);
            if selected.starts_with(directory.canonicalize()?) {
                return Err(Error::new(
                    "CODEX_CURRENT_VERSION",
                    "当前启用的版本不能删除，请先启用其他版本。",
                ));
            }
        }
        let trash = self.root.join(format!(".remove-{}", Uuid::new_v4()));
        std::fs::rename(&directory, &trash)?;
        self.runtime
            .lock()
            .unwrap()
            .releases
            .retain(|release| release.version != selected);
        Ok(trash)
    }
}

impl Run {
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }
    pub(crate) fn phase(&self, phase: &str, message: &str) {
        if let Some(op) = self
            .manager
            .runtime
            .lock()
            .unwrap()
            .operation
            .as_mut()
            .filter(|op| op.id == self.id)
        {
            op.phase = phase.into();
            op.message = message.into();
        }
    }
    fn progress(&self, downloaded: u64, total: Option<u64>) {
        if let Some(op) = self
            .manager
            .runtime
            .lock()
            .unwrap()
            .operation
            .as_mut()
            .filter(|op| op.id == self.id)
        {
            op.downloaded_bytes = downloaded;
            op.total_bytes = total;
        }
    }
    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }
    pub(crate) fn finish(mut self, result: Result<String>) {
        self.complete(result);
        self.finished = true;
    }
    fn complete(&self, result: Result<String>) {
        let mut state = self.manager.runtime.lock().unwrap();
        let Some(op) = state.operation.as_mut().filter(|op| op.id == self.id) else {
            return;
        };
        op.finished_at = Some(crate::now_ms());
        match &result {
            Ok(message) => {
                op.phase = "completed".into();
                op.message = message.clone();
            }
            Err(error) => {
                op.phase = if error.code == "CODEX_CANCELLED" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                op.message = error.message.clone();
            }
        }
        state.error = result.err().filter(|error| error.code != "CODEX_CANCELLED");
        state.cancel = None;
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel.cancel();
            self.complete(Err(cancelled()));
        }
    }
}

fn cancelled() -> Error {
    Error::new("CODEX_CANCELLED", "操作已取消，当前运行版本未改变。")
}

fn version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 48 {
        return None;
    }
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || (p.len() > 1 && p.starts_with('0'))
                || !p.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ])
}

fn config_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() <= 64 * 1024 => Ok(Some(std::fs::read(path)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(Error::new("CODEX_STATE", "原生安装配置无法读取或过大。")),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    config_bytes(path)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(Error::from))
        .transpose()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read_release(root: &Path, selected: &str) -> Result<Release> {
    if version(selected).is_none() {
        return Err(Error::new("INVALID_INPUT", "版本号无效。"));
    }
    let directory = root.join("versions").join(selected);
    let metadata = std::fs::symlink_metadata(&directory)
        .map_err(|_| Error::new("NOT_FOUND", "此版本尚未安装。"))?;
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err(Error::new("CODEX_STATE", "版本目录必须是普通目录。"));
    }
    let release: Release = read_json(&directory.join("relay-release.json"))?
        .ok_or_else(|| Error::new("CODEX_STATE", "缺少版本记录。"))?;
    if release.version != selected
        || !["bin/codex", "codex/codex"].contains(&release.executable.as_str())
        || release.sha256.len() != 64
        || !release.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::new("CODEX_STATE", "版本记录无效。"));
    }
    let executable = directory.join(&release.executable);
    if executable.symlink_metadata()?.is_symlink()
        || !executable
            .canonicalize()?
            .starts_with(directory.canonicalize()?)
    {
        return Err(Error::new("CODEX_STATE", "版本文件指向安装目录之外。"));
    }
    Ok(release)
}
