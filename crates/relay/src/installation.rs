use crate::{
    error::{Error, Result},
    platform,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

pub struct ArchiveInspection {
    pub source_files: Vec<SourceFile>,
    pub shared_websocket: bool,
    pub native_client_info: NativeClientInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub format: u32,
    #[serde(default)]
    pub manually_configured: bool,
    pub prepared_at: Value,
    pub desktop_executable: Option<PathBuf>,
    pub app_server_executable: PathBuf,
    pub app_server_sha256: String,
    pub archive: Option<PathBuf>,
    pub source_files: Vec<SourceFile>,
    #[serde(default)]
    pub app_server_version: Option<String>,
    #[serde(default)]
    pub native_client_info: Option<NativeClientInfo>,
}

impl Installation {
    pub fn desktop_paths(&self) -> Result<(&Path, &Path)> {
        self.desktop_executable
            .as_deref()
            .zip(self.archive.as_deref())
            .ok_or_else(|| {
                Error::new(
                    "DESKTOP_UNAVAILABLE",
                    "当前为独立 Codex 模式，未配置桌面程序和应用归档；桌面调试不可用。",
                )
            })
    }
    pub fn client_info(&self) -> Result<&NativeClientInfo> {
        self.native_client_info.as_ref().ok_or_else(|| {
            Error::new(
                "NATIVE_CLIENT_INFO",
                "缺少原生客户端信息，请重新检测或设置安装路径。",
            )
        })
    }
}

fn has_native_client_info(network_source: &str) -> bool {
    // Match observed desktop identity declarations, without executing bundle code.
    // 26.1016 embeds clientInfo in initialize; 26.1021 serializes the same identity
    // as --startup-client-info when reading the native startup configuration.
    // 26.1027 keeps the startup declaration but moves it into early-bootstrap chunks.
    ['`', '"', '\''].iter().any(|q| {
        network_source.contains(&format!(
            "clientInfo:{{name:{q}codex_desktop{q},title:{q}Codex Desktop{q},version:"
        ))
            || network_source.contains(&format!(
                "{q}--startup-client-info{q},JSON.stringify({{name:{q}codex_desktop{q},title:{q}Codex Desktop{q},version:"
            ))
    })
}

fn native_client_info(package: &Value, network_source: &str) -> Result<NativeClientInfo> {
    let version = package["version"].as_str().filter(|v| {
        !v.is_empty()
            && v.len() <= 128
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    });
    if !has_native_client_info(network_source)
        || package["name"] != "openai-codex-electron"
        || package["productName"] != "Codex"
        || version.is_none()
    {
        return Err(Error::new(
            "NATIVE_CLIENT_INFO",
            "无法识别安装包的原生桌面客户端信息；需要适配当前版本。",
        ));
    }
    Ok(NativeClientInfo {
        name: "codex_desktop".into(),
        title: "Codex Desktop".into(),
        // Electron app.getVersion() reads package.json, not the Appx or CLI version.
        version: version.unwrap().into(),
    })
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn inspect_archive(path: &Path) -> Result<ArchiveInspection> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix)?;
    let number = |i| u32::from_le_bytes(prefix[i..i + 4].try_into().unwrap()) as u64;
    let header_size = number(4);
    let json_size = number(12);
    if number(0) != 4
        || !(8..=32 * 1024 * 1024).contains(&header_size)
        || json_size > header_size - 8
        || 8 + header_size > length
    {
        return Err(Error::new("ASAR_HEADER", "应用归档头无效。"));
    }
    let mut bytes = vec![0; json_size as usize];
    file.read_exact(&mut bytes)?;
    let header: Value = serde_json::from_slice(&bytes)?;
    let data_start = 8 + header_size;
    let (offset, size) = archive_entry(&header["files"]["package.json"])?;
    if size > 1024 * 1024 {
        return Err(Error::new("ASAR_SIZE", "应用包元数据大小超限。"));
    }
    let package_bytes = read_archive_bytes(&mut file, data_start, offset, size, length)?;
    let package: Value = serde_json::from_slice(&package_bytes)?;
    let main = package["main"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("./");
    if main.is_empty()
        || main.contains(['\\', ':'])
        || main
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(Error::new("ASAR_ENTRY", "应用包启动入口无效。"));
    }
    let source_directory = main
        .rsplit_once('/')
        .map_or("", |(directory, _)| &main[..directory.len() + 1]);
    let mut entries = Vec::new();
    fn visit(
        node: &Value,
        path: &str,
        depth: usize,
        source_directory: &str,
        entries: &mut Vec<(String, u64, u64)>,
    ) -> Result<()> {
        if depth > 32 {
            return Err(Error::new("ASAR_DEPTH", "应用归档层级超限。"));
        }
        if let Some(files) = node.get("files").and_then(Value::as_object) {
            for (name, child) in files {
                if name.contains(['/', '\\']) || name == ".." {
                    return Err(Error::new("ASAR_PATH", "应用归档路径无效。"));
                }
                // Dependencies are not application startup declarations, even for a root entrypoint.
                if name == "node_modules" {
                    continue;
                }
                visit(
                    child,
                    &if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}/{name}")
                    },
                    depth + 1,
                    source_directory,
                    entries,
                )?;
            }
        } else if path.starts_with(source_directory)
            && [".js", ".cjs", ".mjs"]
                .iter()
                .any(|suffix| path.ends_with(suffix))
            && node.get("unpacked") != Some(&Value::Bool(true))
        {
            if entries.len() >= 256 {
                return Err(Error::new("ASAR_AMBIGUOUS", "归档候选文件过多。"));
            }
            let (offset, size) = archive_entry(node)?;
            entries.push((path.into(), offset, size));
        }
        Ok(())
    }
    visit(&header, "", 0, source_directory, &mut entries)?;
    if entries.iter().map(|(_, _, size)| size).sum::<u64>() > 128 * 1024 * 1024 {
        return Err(Error::new("ASAR_SIZE", "归档候选文件总大小超限。"));
    }
    if !entries.iter().any(|(path, _, _)| path == main) {
        return Err(Error::new(
            "ASAR_ENTRY",
            "应用包启动入口不在可读取的脚本中。",
        ));
    }
    let mut files = vec![SourceFile {
        path: "package.json".into(),
        sha256: hex::encode(Sha256::digest(&package_bytes)),
    }];
    let mut websocket = false;
    let mut network_source = None;
    for (path, offset, size) in entries {
        let bytes = read_archive_bytes(&mut file, data_start, offset, size, length)?;
        let source = String::from_utf8_lossy(&bytes);
        websocket |= source.contains("CODEX_APP_SERVER_WS_URL")
            && source.contains("CODEX_APP_SERVER_FORCE_CLI");
        if has_native_client_info(&source) && network_source.replace(source.into_owned()).is_some()
        {
            return Err(Error::new(
                "ASAR_AMBIGUOUS",
                "存在多个原生客户端初始化文件。",
            ));
        }
        files.push(SourceFile {
            path,
            sha256: hex::encode(Sha256::digest(&bytes)),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ArchiveInspection {
        source_files: files,
        shared_websocket: websocket,
        native_client_info: native_client_info(&package, network_source.as_deref().unwrap_or(""))?,
    })
}

fn archive_entry(node: &Value) -> Result<(u64, u64)> {
    let offset = node["offset"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| Error::new("ASAR_OFFSET", "归档偏移无效。"))?;
    let size = node["size"]
        .as_u64()
        .filter(|s| *s <= 32 * 1024 * 1024)
        .ok_or_else(|| Error::new("ASAR_OFFSET", "归档大小无效。"))?;
    if node.get("unpacked") == Some(&Value::Bool(true)) {
        return Err(Error::new("ASAR_PATH", "应用元数据必须位于归档内。"));
    }
    Ok((offset, size))
}

fn read_archive_bytes(
    file: &mut std::fs::File,
    data_start: u64,
    offset: u64,
    size: u64,
    length: u64,
) -> Result<Vec<u8>> {
    let start = data_start
        .checked_add(offset)
        .ok_or_else(|| Error::new("ASAR_OFFSET", "归档偏移溢出。"))?;
    if start.checked_add(size).is_none_or(|end| end > length) {
        return Err(Error::new("ASAR_OFFSET", "归档偏移越界。"));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; size as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub async fn read() -> Result<Installation> {
    let bytes = tokio::fs::read(".cache/launch-config.json")
        .await
        .map_err(|_| Error::new("LAUNCH_CONFIG", "当前目录尚未完成首次准备。请在“系统设置 → 环境与诊断”选择自动模式检测安装，或选择独立模式指定 Codex 路径。"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn paths_missing(installation: &Installation) -> bool {
    !installation.app_server_executable.is_file()
        || installation
            .desktop_executable
            .iter()
            .chain(installation.archive.iter())
            .any(|path| !path.is_file())
}

/// Discover the installation on first use, or recover after Codex has been upgraded. Windows
/// installs use versioned paths for both the desktop package and app-server,
/// so an old launch-config can otherwise produce a generic process I/O error.
pub async fn read_with_refresh() -> Result<Installation> {
    refresh_installation(read().await, prepare(None)).await
}

async fn refresh_installation(
    cached: Result<Installation>,
    preparation: impl std::future::Future<Output = Result<Installation>>,
) -> Result<Installation> {
    let installation = match cached {
        Err(error) if error.code == "LAUNCH_CONFIG" => return preparation.await,
        result => result?,
    };
    if !paths_missing(&installation) {
        return Ok(installation);
    }
    if installation.manually_configured {
        return Err(Error::new(
            "DESKTOP_PATH",
            "手动指定的原生安装文件不存在，请在“系统设置 → 环境与诊断”中更新路径。",
        ));
    }
    preparation.await.map_err(|error| {
        if matches!(
            error.code.as_str(),
            "DESKTOP_SELECTION" | "DESKTOP_BACKEND" | "WINDOWS_DISCOVERY"
        ) {
            Error::new(
                "DESKTOP_INSTALLATION_STALE",
                "检测到 Codex 桌面已升级。请先启动并保持官方 Codex 桌面运行，然后重试；系统会自动刷新原生安装配置。",
            )
        } else {
            error
        }
    })
}

pub async fn version(executable: &Path) -> Result<String> {
    let mut command = tokio::process::Command::new(executable);
    platform::hide(&mut command);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        command.arg("--version").kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| Error::new("VERSION_TIMEOUT", "版本查询超时。"))??;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || !text.starts_with("codex-cli ") || text.len() > 128 {
        return Err(Error::new("VERSION_INVALID", "无法识别原生版本。"));
    }
    Ok(text)
}

pub async fn prepare(pid: Option<u32>) -> Result<Installation> {
    let snapshot = platform::discover().await?;
    let pid = match pid {
        Some(pid) => Some(pid),
        None => match std::env::var("CODEX_APP_TOOLS_PIPE_PATH") {
            Ok(pipe) => platform::pipe_owner(&pipe).await.ok(),
            Err(_) => None,
        },
    };
    let desktops: Vec<_> = platform::desktop_owners(&snapshot)
        .into_iter()
        .filter(|d| pid.is_none_or(|pid| pid == d.pid))
        .collect();
    if desktops.len() != 1 {
        return Err(Error::new(
            "DESKTOP_SELECTION",
            "无法确定唯一桌面，请指定 --desktop-pid。",
        ));
    }
    let desktop = &desktops[0];
    let mut backends: Vec<_> = snapshot
        .processes
        .iter()
        .filter(|p| p.is_app_server && p.parent_pid == desktop.pid)
        .collect();
    // Recent desktops run more than one app-server from the same signed binary.
    backends.sort_by(|a, b| a.executable.cmp(&b.executable));
    backends.dedup_by(|a, b| a.executable == b.executable);
    if backends.len() != 1 {
        return Err(Error::new(
            "DESKTOP_BACKEND",
            "无法确定桌面正在使用的后端。",
        ));
    }
    let desktop_executable = PathBuf::from(desktop.executable.as_ref().unwrap());
    let app_server_executable = PathBuf::from(
        backends[0]
            .executable
            .as_ref()
            .ok_or_else(|| Error::new("DESKTOP_BACKEND", "无法读取后端路径。"))?,
    );
    #[cfg(target_os = "macos")]
    let archive = platform::macos::bundle(&desktop_executable)?.join("Contents/Resources/app.asar");
    #[cfg(not(target_os = "macos"))]
    let archive = desktop_executable
        .parent()
        .unwrap()
        .join("resources/app.asar");
    prepare_paths(
        InstallationPaths {
            desktop_executable: Some(desktop_executable),
            app_server_executable,
            archive: Some(archive),
        },
        false,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CodexPath {
    app_server_executable: PathBuf,
}

impl CodexPath {
    pub(crate) fn validate(&self) -> Result<()> {
        self.paths().validate()
    }

    fn paths(&self) -> InstallationPaths {
        InstallationPaths {
            app_server_executable: self.app_server_executable.clone(),
            desktop_executable: None,
            archive: None,
        }
    }
}

pub(crate) async fn prepare_codex(input: CodexPath) -> Result<Installation> {
    prepare_paths(input.paths(), true).await
}

#[derive(Debug)]
struct InstallationPaths {
    desktop_executable: Option<PathBuf>,
    app_server_executable: PathBuf,
    archive: Option<PathBuf>,
}

impl InstallationPaths {
    fn validate(&self) -> Result<()> {
        if self.desktop_executable.is_some() != self.archive.is_some() {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面程序和应用归档需同时填写；独立 Codex 模式请同时留空。",
            ));
        }
        for (label, path) in [
            ("桌面程序", self.desktop_executable.as_ref()),
            ("Codex", Some(&self.app_server_executable)),
            ("应用归档", self.archive.as_ref()),
        ] {
            let Some(path) = path else {
                continue;
            };
            if !path.is_absolute() || !path.is_file() {
                return Err(Error::new(
                    "INVALID_INPUT",
                    &format!("{label}路径必须是中转站所在机器上已存在的文件绝对路径。"),
                ));
            }
        }
        Ok(())
    }
}

async fn prepare_paths(
    paths: InstallationPaths,
    manually_configured: bool,
) -> Result<Installation> {
    let result = inspect_paths(paths, manually_configured).await?;
    tokio::fs::create_dir_all(".cache").await?;
    save_installation(&result, Path::new(".cache/launch-config.json"))?;
    Ok(result)
}

async fn inspect_paths(
    paths: InstallationPaths,
    manually_configured: bool,
) -> Result<Installation> {
    paths.validate()?;
    let InstallationPaths {
        desktop_executable,
        app_server_executable,
        archive,
    } = paths;
    let (source_files, native_client_info) =
        if let Some((desktop, archive)) = desktop_executable.as_ref().zip(archive.as_ref()) {
            #[cfg(not(target_os = "macos"))]
            let _ = desktop;
            #[cfg(target_os = "macos")]
            platform::macos::validate_bundle(desktop, &app_server_executable, archive).await?;
            let inspection = inspect_archive(archive)?;
            if !inspection.shared_websocket {
                return Err(Error::new(
                    "DESKTOP_WS_UNSUPPORTED",
                    "安装版本不支持已知共享连接入口。",
                ));
            }
            (inspection.source_files, Some(inspection.native_client_info))
        } else {
            (vec![], None)
        };
    let app_server_version = version(&app_server_executable).await?;
    let native_client_info = match native_client_info {
        Some(info) => info,
        None => standalone_client_info(&app_server_version)?,
    };
    let result = Installation {
        format: 1,
        manually_configured,
        prepared_at: crate::now_ms().into(),
        desktop_executable,
        app_server_sha256: hash_file(&app_server_executable)?,
        app_server_version: Some(app_server_version),
        app_server_executable,
        archive,
        source_files,
        native_client_info: Some(native_client_info),
    };
    if result.desktop_executable.is_none() {
        verify_standalone(&result).await?;
    }
    Ok(result)
}

fn standalone_client_info(version: &str) -> Result<NativeClientInfo> {
    let version = version
        .strip_prefix("codex-cli ")
        .filter(|v| {
            !v.is_empty()
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
        })
        .ok_or_else(|| Error::new("VERSION_INVALID", "无法从 Codex 可执行文件读取有效版本。"))?;
    Ok(NativeClientInfo {
        name: "codex_desktop".into(),
        title: "Codex Desktop".into(),
        // Standalone mode has no Electron package: use the executable's real version.
        version: version.into(),
    })
}

fn save_installation(result: &Installation, path: &Path) -> Result<()> {
    // Replace only after every check succeeds; readers never see a partial configuration.
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut file, result)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

async fn verify_standalone(installation: &Installation) -> Result<()> {
    // Keep the directory on failed/cancelled startup; it must not be deleted before
    // the owned process tree has confirmed exit. No account or model is used here.
    let directory = tempfile::Builder::new()
        .prefix("relay-installation-check-")
        .tempdir()?
        .keep();
    let home = directory.join("home");
    tokio::fs::create_dir(&home).await?;
    let backend =
        crate::backend::Backend::start_direct(installation, &directory, Some(&home)).await?;
    backend.stop().await?;
    tokio::fs::remove_dir_all(&directory).await?;
    Ok(())
}

pub async fn validate(installation: &Installation) -> Result<()> {
    if installation.format != 1
        || hash_file(&installation.app_server_executable)? != installation.app_server_sha256
    {
        return Err(Error::new(
            "CLI_VERSION",
            "原生后端已变化，请重新 prepare。",
        ));
    }
    if installation.desktop_executable.is_none() && installation.archive.is_none() {
        installation.client_info()?;
        return Ok(());
    }
    let (desktop_executable, archive) = installation.desktop_paths()?;
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = desktop_executable;
    #[cfg(windows)]
    if !installation.manually_configured {
        let installed = platform::powershell("$p = Get-AppxPackage -Name 'OpenAI.Codex' | Sort-Object Version -Descending | Select-Object -First 1; if ($p) { $p.InstallLocation }",&[]).await?;
        let expected = PathBuf::from(installed).join("app").join(
            desktop_executable
                .file_name()
                .ok_or_else(|| Error::new("DESKTOP_VERSION", "桌面路径无效。"))?,
        );
        if !expected
            .to_string_lossy()
            .eq_ignore_ascii_case(&desktop_executable.to_string_lossy())
        {
            return Err(Error::new("DESKTOP_VERSION", "桌面安装版本已变化。"));
        }
    }
    #[cfg(target_os = "macos")]
    platform::macos::validate_bundle(
        desktop_executable,
        &installation.app_server_executable,
        archive,
    )
    .await?;
    #[cfg(not(any(windows, target_os = "macos")))]
    return Err(Error::new(
        "PLATFORM_UNSUPPORTED",
        "安装校验目前支持 Windows 和 macOS。",
    ));
    let inspection = inspect_archive(archive)?;
    let mut saved = installation.source_files.clone();
    saved.sort_by(|a, b| a.path.cmp(&b.path));
    if !inspection.shared_websocket
        || inspection.source_files != saved
        || &inspection.native_client_info != installation.client_info()?
    {
        return Err(Error::new(
            "DESKTOP_VERSION",
            "桌面协议或客户端信息已变化，请重新 prepare。",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cached_installation(path: &Path) -> Installation {
        Installation {
            format: 1,
            manually_configured: false,
            prepared_at: Value::Null,
            desktop_executable: Some(path.into()),
            app_server_executable: path.into(),
            app_server_sha256: "fixture".into(),
            archive: Some(path.into()),
            source_files: vec![],
            app_server_version: None,
            native_client_info: None,
        }
    }

    #[tokio::test]
    async fn missing_installation_is_prepared_on_first_use_and_reports_discovery_errors() {
        let directory = tempfile::tempdir().unwrap();
        let prepared = cached_installation(directory.path());
        let result =
            refresh_installation(Err(Error::new("LAUNCH_CONFIG", "not prepared")), async {
                Ok(prepared.clone())
            })
            .await
            .unwrap();
        assert_eq!(result.app_server_executable, prepared.app_server_executable);
        let error = refresh_installation(Err(Error::new("LAUNCH_CONFIG", "not prepared")), async {
            Err(Error::new("DESKTOP_SELECTION", "keep the desktop running"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "DESKTOP_SELECTION");
        assert_eq!(error.message, "keep the desktop running");
    }

    #[tokio::test]
    async fn cached_installation_is_reused_and_corrupt_cache_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native");
        std::fs::write(&path, "fixture").unwrap();
        let result = refresh_installation(Ok(cached_installation(&path)), async {
            panic!("valid cache must not trigger discovery")
        })
        .await
        .unwrap();
        assert_eq!(result.app_server_executable, path);
        let error = refresh_installation(Err(Error::new("JSON_ERROR", "corrupt cache")), async {
            panic!("corrupt cache must not be silently replaced")
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "JSON_ERROR");
    }

    #[tokio::test]
    async fn standalone_configuration_persists_and_validates_without_desktop_files() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("codex");
        std::fs::write(&binary, "fixture").unwrap();
        let mut install = cached_installation(&binary);
        install.manually_configured = true;
        install.desktop_executable = None;
        install.archive = None;
        install.app_server_sha256 = hash_file(&binary).unwrap();
        install.native_client_info = Some(NativeClientInfo {
            name: "codex_desktop".into(),
            title: "Codex Desktop".into(),
            version: "0.163.0".into(),
        });
        let path = directory.path().join("launch-config.json");
        std::fs::write(&path, "previous configuration").unwrap();
        save_installation(&install, &path).unwrap();
        let saved: Installation = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(saved.manually_configured);
        assert!(saved.desktop_executable.is_none() && saved.archive.is_none());
        assert_eq!(
            saved.desktop_paths().unwrap_err().code,
            "DESKTOP_UNAVAILABLE"
        );
        validate(&saved).await.unwrap();
        refresh_installation(Ok(saved.clone()), async {
            panic!("standalone must not discover desktop")
        })
        .await
        .unwrap();
        std::fs::write(&binary, "changed").unwrap();
        assert_eq!(validate(&saved).await.unwrap_err().code, "CLI_VERSION");
    }

    #[tokio::test]
    #[ignore = "Requires BRIDGE_NATIVE_BINARY; isolated initialization only, no credentials or model requests"]
    async fn standalone_binary_initializes_without_desktop() {
        let install = inspect_paths(
            InstallationPaths {
                app_server_executable: std::env::var_os("BRIDGE_NATIVE_BINARY").unwrap().into(),
                desktop_executable: None,
                archive: None,
            },
            true,
        )
        .await
        .unwrap();
        assert_eq!(install.client_info().unwrap().name, "codex_desktop");
        assert_eq!(install.client_info().unwrap().title, "Codex Desktop");
        assert_eq!(
            install.client_info().unwrap().version,
            install
                .app_server_version
                .as_ref()
                .unwrap()
                .strip_prefix("codex-cli ")
                .unwrap()
        );
        assert!(install.desktop_executable.is_none() && install.archive.is_none());
        validate(&install).await.unwrap();
    }

    #[test]
    fn standalone_identity_uses_codex_version_and_rejects_invalid_metadata() {
        let info = standalone_client_info("codex-cli 0.163.0-alpha.11.2").unwrap();
        assert_eq!(info.name, "codex_desktop");
        assert_eq!(info.title, "Codex Desktop");
        assert_eq!(info.version, "0.163.0-alpha.11.2");
        for version in ["", "0.163.0", "codex-cli ", "codex-cli bad\r\nheader"] {
            assert!(standalone_client_info(version).is_err());
        }
    }

    #[test]
    fn standalone_paths_require_only_codex_and_reject_partial_desktop_settings() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("codex");
        std::fs::write(&binary, "fixture").unwrap();
        let mut paths = InstallationPaths {
            app_server_executable: binary.clone(),
            desktop_executable: None,
            archive: None,
        };
        paths.validate().unwrap();
        paths.desktop_executable = Some(binary);
        assert_eq!(paths.validate().unwrap_err().code, "INVALID_INPUT");
    }

    #[tokio::test]
    async fn missing_manual_paths_never_fall_back_to_discovery() {
        let directory = tempfile::tempdir().unwrap();
        let mut cached = cached_installation(&directory.path().join("removed-native"));
        cached.manually_configured = true;
        let error = refresh_installation(Ok(cached), async {
            panic!("manual paths must not silently switch installations")
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "DESKTOP_PATH");
    }

    #[tokio::test]
    async fn manual_paths_validate_all_files_and_archive_before_executing() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("not-an-executable");
        std::fs::write(&executable, "fixture").unwrap();
        let paths = |path: PathBuf| InstallationPaths {
            desktop_executable: Some(executable.clone()),
            app_server_executable: executable.clone(),
            archive: Some(path),
        };
        for invalid in [
            PathBuf::new(),
            PathBuf::from("relative"),
            directory.path().to_owned(),
            directory.path().join("missing"),
        ] {
            assert_eq!(paths(invalid).validate().unwrap_err().code, "INVALID_INPUT");
        }
        // A malformed archive is rejected before trying to run the fake executable.
        let archive = directory.path().join("app.asar");
        std::fs::write(&archive, [0; 16]).unwrap();
        let error = prepare_paths(paths(archive), true).await.unwrap_err();
        #[cfg(not(target_os = "macos"))]
        assert_eq!(error.code, "ASAR_HEADER");
        #[cfg(target_os = "macos")]
        assert_eq!(error.code, "DESKTOP_PATH");
    }

    #[tokio::test]
    async fn missing_cached_paths_still_report_upgrade_recovery_guidance() {
        let directory = tempfile::tempdir().unwrap();
        let cached = cached_installation(&directory.path().join("removed-native"));
        let error = refresh_installation(Ok(cached), async {
            Err(Error::new("DESKTOP_SELECTION", "desktop not running"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "DESKTOP_INSTALLATION_STALE");
    }

    #[test]
    fn startup_client_info_accepts_observed_identity_and_rejects_unrelated_literals() {
        let package =
            json!({"name":"openai-codex-electron","productName":"Codex","version":"26.1021.31345"});
        // Reduced from application-network-startup-B4Wezh-6.js in 26.1021.
        let source = "let n=[`--print-startup-config`,`--startup-client-info`,JSON.stringify({name:`codex_desktop`,title:`Codex Desktop`,version:t})]";
        for quote in ["`", "\"", "'"] {
            let source = source.replace('`', quote);
            let info = native_client_info(&package, &source).unwrap();
            assert_eq!(
                info,
                NativeClientInfo {
                    name: "codex_desktop".into(),
                    title: "Codex Desktop".into(),
                    version: "26.1021.31345".into(),
                }
            );
        }
        for source in [
            source.replace("--startup-client-info", "--unrelated-option"),
            source.replace("JSON.stringify", "unrelated"),
            source.replace("codex_desktop", "other_client"),
            source.replace("Codex Desktop", "Other Client"),
            "let name=`codex_desktop`,title=`Codex Desktop`,flag=`--startup-client-info`;".into(),
        ] {
            assert_eq!(
                native_client_info(&package, &source).unwrap_err().code,
                "NATIVE_CLIENT_INFO"
            );
        }
        for (field, value) in [
            ("name", "other-package"),
            ("productName", "Other"),
            ("version", ""),
            ("version", "bad\r\nheader"),
        ] {
            let mut invalid = package.clone();
            invalid[field] = value.into();
            assert!(native_client_info(&invalid, source).is_err());
        }
    }

    #[test]
    fn archive_inspection_records_startup_identity_and_source_hash() {
        let package = serde_json::to_vec(&json!({"name":"openai-codex-electron","productName":"Codex","version":"26.1021.31345","main":".vite/build/application-network-startup-fixture.js"})).unwrap();
        let source = b"let n=[`--startup-client-info`,JSON.stringify({name:`codex_desktop`,title:`Codex Desktop`,version:t})];const ws=`CODEX_APP_SERVER_WS_URL`,cli=`CODEX_APP_SERVER_FORCE_CLI`;";
        let entry = "application-network-startup-fixture.js";
        let header = serde_json::to_vec(&json!({"files":{
            "package.json":{"offset":"0","size":package.len()},
            ".vite":{"files":{"build":{"files":{entry:{"offset":package.len().to_string(),"size":source.len()}}}}}
        }})).unwrap();
        let aligned = (header.len() + 3) & !3;
        let mut bytes = Vec::new();
        for n in [
            4,
            8 + aligned as u32,
            4 + aligned as u32,
            header.len() as u32,
        ] {
            bytes.extend_from_slice(&n.to_le_bytes());
        }
        bytes.extend_from_slice(&header);
        bytes.resize(16 + aligned, 0);
        bytes.extend_from_slice(&package);
        bytes.extend_from_slice(source);
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("app.asar");
        std::fs::write(&archive, bytes).unwrap();
        let inspected = inspect_archive(&archive).unwrap();
        assert!(inspected.shared_websocket);
        assert_eq!(inspected.native_client_info.version, "26.1021.31345");
        assert_eq!(inspected.source_files.len(), 2);
        assert!(inspected.source_files.contains(&SourceFile {
            path: format!(".vite/build/{entry}"),
            sha256: hex::encode(Sha256::digest(source)),
        }));
    }

    #[test]
    fn bootstrap_archive_finds_identity_among_chunks_and_rejects_ambiguity() {
        // Reduced from early-bootstrap-824-e56448c7829c41c2.js in 26.1027.
        let source = "async function k(e,t){let r=[\"--print-startup-config\",\"--startup-client-info\",JSON.stringify({name:\"codex_desktop\",title:\"Codex Desktop\",version:t})]}const ws=\"CODEX_APP_SERVER_WS_URL\",cli=\"CODEX_APP_SERVER_FORCE_CLI\";";
        let unrelated = "let name=\"codex_desktop\",title=\"Codex Desktop\";";
        let names: Vec<_> = (0..18)
            .map(|i| format!("early-bootstrap-{i}-fixture.js"))
            .collect();
        let mut scripts: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), unrelated))
            .collect();
        scripts[8].1 = source;
        let (_temp, archive) = bootstrap_archive_fixture(&scripts);
        let inspected = inspect_archive(&archive).unwrap();
        assert!(inspected.shared_websocket);
        assert_eq!(inspected.native_client_info.version, "26.1027.30824");
        assert_eq!(inspected.source_files.len(), 19);
        assert!(inspected.source_files.contains(&SourceFile {
            path: format!(".vite/build/{}", names[8]),
            sha256: hex::encode(Sha256::digest(source.as_bytes())),
        }));

        let (_temp, archive) = bootstrap_archive_fixture(&[("early-bootstrap.js", source)]);
        assert!(inspect_archive(&archive).is_ok());
        scripts[9].1 = source;
        let (_temp, archive) = bootstrap_archive_fixture(&scripts);
        assert_eq!(
            inspect_archive(&archive).err().unwrap().code,
            "ASAR_AMBIGUOUS"
        );
        let (_temp, archive) =
            bootstrap_archive_fixture(&[("early-bootstrap-824-fixture.js", unrelated)]);
        assert_eq!(
            inspect_archive(&archive).err().unwrap().code,
            "NATIVE_CLIENT_INFO"
        );
        let (_temp, archive) = bootstrap_archive_fixture(&[("unrelated.js", source)]);
        assert!(inspect_archive(&archive).is_ok());
    }

    #[test]
    fn archive_discovers_renamed_entry_directory_and_modules_by_content() {
        let source = "clientInfo:{name:`codex_desktop`,title:`Codex Desktop`,version:r};const ws=`CODEX_APP_SERVER_WS_URL`,cli=`CODEX_APP_SERVER_FORCE_CLI`;";
        for main in ["changed/runtime/start.cjs", "start.js"] {
            let directory = main
                .rsplit_once('/')
                .map_or("", |(prefix, _)| &main[..prefix.len() + 1]);
            let chunk = format!("{directory}chunks/arbitrary-new-name.mjs");
            let dependency = format!("{directory}node_modules/dependency/index.js");
            let (_temp, archive) = application_archive_fixture(
                &format!("./{main}"),
                &[(main, ""), (&chunk, source), (&dependency, source)],
            );
            let inspected = inspect_archive(&archive).unwrap();
            assert!(inspected.shared_websocket);
            assert_eq!(inspected.native_client_info.version, "26.1027.30824");
            assert_eq!(inspected.source_files.len(), 3);
        }
        // Identity literals in renderer files outside the entry directory are not startup evidence.
        let (_temp, archive) = application_archive_fixture(
            "runtime/main.js",
            &[("runtime/main.js", ""), ("renderer/app.js", source)],
        );
        assert_eq!(
            inspect_archive(&archive).err().unwrap().code,
            "NATIVE_CLIENT_INFO"
        );
        for main in [
            "",
            "../main.js",
            "/main.js",
            "C:/main.js",
            "runtime/missing.js",
        ] {
            let (_temp, archive) =
                application_archive_fixture(main, &[("runtime/main.js", source)]);
            assert_eq!(inspect_archive(&archive).err().unwrap().code, "ASAR_ENTRY");
        }
    }

    #[test]
    fn bootstrap_archive_keeps_candidate_count_and_total_size_bounded() {
        let names: Vec<_> = (0..257)
            .map(|i| format!("early-bootstrap-{i}-fixture.js"))
            .collect();
        let scripts: Vec<_> = names.iter().map(|name| (name.as_str(), "")).collect();
        let (_temp, archive) = bootstrap_archive_fixture(&scripts);
        assert_eq!(
            inspect_archive(&archive).err().unwrap().code,
            "ASAR_AMBIGUOUS"
        );
        let package =
            serde_json::to_vec(&json!({"main":".vite/build/early-bootstrap-0-fixture.js"}))
                .unwrap();
        let files: serde_json::Map<_, _> = (0..5)
            .map(|i| {
                (
                    format!("early-bootstrap-{i}-fixture.js"),
                    json!({"offset":"0","size":32 * 1024 * 1024}),
                )
            })
            .collect();
        let (_temp, archive) = write_archive_fixture(
            json!({"files":{
                "package.json":{"offset":"0","size":package.len()},
                ".vite":{"files":{"build":{"files":files}}}
            }}),
            &package,
        );
        assert_eq!(inspect_archive(&archive).err().unwrap().code, "ASAR_SIZE");
    }

    fn bootstrap_archive_fixture(scripts: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let paths: Vec<_> = scripts
            .iter()
            .map(|(name, _)| format!(".vite/build/{name}"))
            .collect();
        let scripts: Vec<_> = paths
            .iter()
            .zip(scripts)
            .map(|(path, (_, source))| (path.as_str(), *source))
            .collect();
        application_archive_fixture(&paths[0], &scripts)
    }

    fn application_archive_fixture(
        main: &str,
        scripts: &[(&str, &str)],
    ) -> (tempfile::TempDir, PathBuf) {
        let mut contents = serde_json::to_vec(&json!({"name":"openai-codex-electron","productName":"Codex","version":"26.1027.30824","main":main})).unwrap();
        let mut header = json!({"files":{"package.json":{"offset":"0","size":contents.len()}}});
        for (path, source) in scripts {
            let mut parent = &mut header;
            let parts: Vec<_> = path.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                parent = parent["files"]
                    .as_object_mut()
                    .unwrap()
                    .entry(*part)
                    .or_insert_with(|| json!({"files":{}}));
            }
            parent["files"][parts[parts.len() - 1]] =
                json!({"offset":contents.len().to_string(),"size":source.len()});
            contents.extend_from_slice(source.as_bytes());
        }
        write_archive_fixture(header, &contents)
    }

    fn write_archive_fixture(header: Value, contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let header = serde_json::to_vec(&header).unwrap();
        let aligned = (header.len() + 3) & !3;
        let mut bytes = Vec::new();
        for n in [
            4,
            8 + aligned as u32,
            4 + aligned as u32,
            header.len() as u32,
        ] {
            bytes.extend_from_slice(&n.to_le_bytes());
        }
        bytes.extend_from_slice(&header);
        bytes.resize(16 + aligned, 0);
        bytes.extend_from_slice(contents);
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("app.asar");
        std::fs::write(&archive, bytes).unwrap();
        (temp, archive)
    }

    #[test]
    #[ignore = "Requires BRIDGE_NATIVE_ARCHIVE; reads the installed archive without executing code or changing configuration"]
    fn installed_archive_identity_is_recognized() {
        let archive = PathBuf::from(std::env::var_os("BRIDGE_NATIVE_ARCHIVE").unwrap());
        let inspected = inspect_archive(&archive).unwrap();
        assert!(inspected.shared_websocket);
        assert_eq!(inspected.native_client_info.name, "codex_desktop");
        assert_eq!(inspected.native_client_info.title, "Codex Desktop");
        assert!(!inspected.native_client_info.version.is_empty());
        println!(
            "Detected desktop {} from {} checked files",
            inspected.native_client_info.version,
            inspected.source_files.len()
        );
    }

    #[test]
    fn native_identity_uses_desktop_package_version_and_requires_known_initializer() {
        let package =
            json!({"name":"openai-codex-electron","productName":"Codex","version":"26.1016.51333"});
        let source = "clientInfo:{name:`codex_desktop`,title:`Codex Desktop`,version:r}";
        let info = native_client_info(&package, source).unwrap();
        assert_eq!(info.version, "26.1016.51333");
        assert_eq!(info.name, "codex_desktop");
        assert!(native_client_info(&package, "").is_err());
        assert!(
            native_client_info(&package, &source.replace("codex_desktop", "unrecognized")).is_err()
        );
        assert!(native_client_info(&json!({"version":"0.163.0"}), source).is_err());
        let mut invalid = package;
        invalid["version"] = "bad\r\nheader".into();
        assert!(native_client_info(&invalid, source).is_err());
    }
}
