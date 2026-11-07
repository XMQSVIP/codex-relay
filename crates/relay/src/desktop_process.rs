//! Launch the installed desktop without changing its files or terminating its window.
use crate::{
    backend::clean_env,
    error::{Error, Result},
    platform,
};
#[cfg(windows)]
use std::time::Duration;
use std::{path::Path, process::Stdio};
use tokio::process::{Child, Command};

pub enum DesktopProcess {
    Direct(Box<Child>),
    #[cfg(windows)]
    Packaged {
        handle: std::os::windows::io::OwnedHandle,
        pid: u32,
    },
}

pub(crate) fn io_error(code: &str, stage: &str, error: std::io::Error) -> Error {
    Error::new(
        code,
        &format!(
            "{stage}失败（系统错误 {}，{:?}）。",
            error
                .raw_os_error()
                .map_or_else(|| "未知".into(), |v| v.to_string()),
            error.kind()
        ),
    )
}

impl DesktopProcess {
    pub async fn start(executable: &Path, port: u16) -> Result<Self> {
        let mut command = Command::new(executable);
        clean_env(&mut command, false);
        platform::hide(&mut command);
        command
            .env(
                "CODEX_APP_SERVER_WS_URL",
                format!("ws://127.0.0.1:{port}/desktop"),
            )
            .current_dir(
                executable
                    .parent()
                    .ok_or_else(|| Error::new("DESKTOP_PATH", "桌面安装路径无效。"))?,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Dropping this handle must leave the user's desktop window open.
        match command.spawn() {
            Ok(child) => Ok(Self::Direct(Box::new(child))),
            #[cfg(windows)]
            Err(error) if error.raw_os_error() == Some(5) && packaged_path(executable) => {
                Self::start_packaged(executable, port).await
            }
            Err(error) => Err(io_error("DESKTOP_START", "启动官方桌面", error)),
        }
    }
    pub fn pid(&self) -> Result<u32> {
        match self {
            Self::Direct(child) => child
                .id()
                .ok_or_else(|| Error::new("DESKTOP_START", "桌面进程已退出。")),
            #[cfg(windows)]
            Self::Packaged { pid, .. } => Ok(*pid),
        }
    }
    pub fn exited(&mut self) -> Result<bool> {
        match self {
            Self::Direct(child) => child
                .try_wait()
                .map(|value| value.is_some())
                .map_err(|e| io_error("DESKTOP_STATUS", "读取桌面状态", e)),
            #[cfg(windows)]
            Self::Packaged { handle, .. } => {
                use std::os::windows::io::AsRawHandle;
                // SAFETY: owned process handle remains alive for this nonblocking wait.
                match unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        handle.as_raw_handle(),
                        0,
                    )
                } {
                    0 => Ok(true),
                    258 => Ok(false),
                    _ => Err(io_error(
                        "DESKTOP_STATUS",
                        "读取共享桌面状态",
                        std::io::Error::last_os_error(),
                    )),
                }
            }
        }
    }
}

#[cfg(windows)]
fn packaged_path(path: &Path) -> bool {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
        .contains("\\windowsapps\\openai.codex_")
}

#[cfg(windows)]
impl DesktopProcess {
    async fn start_packaged(executable: &Path, port: u16) -> Result<Self> {
        use base64::Engine;
        use serde_json::{Value, json};
        let result_path = std::env::current_dir()?
            .join(".cache")
            .join(format!("desktop-launch-{}.json", uuid::Uuid::new_v4()));
        let request = json!({"executable":executable,"url":format!("ws://127.0.0.1:{port}/desktop"),"result":result_path,"deadline":crate::now_ms()+15000});
        let data = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&request)?);
        let helper =
            include_str!("desktop_package_helper.ps1").replace("__REQUEST_BASE64__", &data);
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            helper
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let launched = platform::powershell(r#"
$ErrorActionPreference = 'Stop'
try {
    $package = Get-AppxPackage -Name OpenAI.Codex | Where-Object { [IO.Path]::GetFullPath((Join-Path $_.InstallLocation 'app\ChatGPT.exe')) -eq $env:BRIDGE_DESKTOP_EXE } | Select-Object -First 1
    if (-not $package) { throw 'Package mismatch' }
    $manifest = Get-AppxPackageManifest $package
    $apps = @($manifest.Package.Applications.Application | Where-Object { [IO.Path]::GetFullPath((Join-Path $package.InstallLocation $_.Executable)) -eq $env:BRIDGE_DESKTOP_EXE })
    if ($apps.Count -ne 1) { throw 'Application mismatch' }
    Invoke-CommandInDesktopPackage -PackageFamilyName $package.PackageFamilyName -AppId $apps[0].Id -Command (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -Args ('-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand ' + $env:BRIDGE_DESKTOP_HELPER) -PreventBreakaway
    @{ok=$true} | ConvertTo-Json -Compress
} catch { @{ok=$false; hresult=$_.Exception.HResult} | ConvertTo-Json -Compress }
"#, &[("BRIDGE_DESKTOP_EXE", executable.to_string_lossy().into_owned()), ("BRIDGE_DESKTOP_HELPER", encoded)]).await?;
        let launched: Value = serde_json::from_str(&launched)?;
        if launched["ok"] != true {
            return Err(Error::new(
                "DESKTOP_PACKAGE_LAUNCH",
                &format!(
                    "Windows 应用包启动失败（HRESULT {}）。请确认当前 Windows 用户已安装 Codex，并重新检测安装。",
                    launched["hresult"]
                ),
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(18);
        loop {
            match tokio::fs::read(&result_path).await {
                Ok(bytes) => {
                    let _ = tokio::fs::remove_file(&result_path).await;
                    let value: Value = serde_json::from_slice(&bytes)?;
                    if let Some(pid) = value["pid"]
                        .as_u64()
                        .and_then(|p| u32::try_from(p).ok())
                        .filter(|p| *p > 0)
                    {
                        return Self::open_packaged(pid, executable);
                    }
                    return Err(Error::new(
                        "DESKTOP_PACKAGE_START",
                        &format!(
                            "应用包内启动桌面失败（系统错误 {}，HRESULT {}）。",
                            value["win32"], value["hresult"]
                        ),
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error("DESKTOP_LAUNCH_RESULT", "读取桌面启动结果", e)),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::new(
                    "DESKTOP_PACKAGE_TIMEOUT",
                    "Windows 应用包启动超时，未获得桌面进程。请稍后重新启动。",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    fn open_packaged(pid: u32, executable: &Path) -> Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            QueryFullProcessImageNameW,
        };
        // SAFETY: open only query/wait rights, then retain the handle to avoid PID reuse.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return Err(io_error(
                "DESKTOP_PROCESS",
                "打开共享桌面进程",
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: successful OpenProcess transfers ownership of a valid handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        // SAFETY: buffer is valid for length wide characters; owned handle stays alive.
        if unsafe {
            QueryFullProcessImageNameW(handle.as_raw_handle(), 0, path.as_mut_ptr(), &mut length)
        } == 0
        {
            return Err(io_error(
                "DESKTOP_PROCESS",
                "核验共享桌面路径",
                std::io::Error::last_os_error(),
            ));
        }
        if !String::from_utf16_lossy(&path[..length as usize])
            .eq_ignore_ascii_case(&executable.to_string_lossy())
        {
            return Err(Error::new("DESKTOP_PROCESS", "共享桌面的进程路径不匹配。"));
        }
        Ok(Self::Packaged { handle, pid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn io_errors_include_stage_and_system_code_without_raw_paths() {
        let error = io_error(
            "DESKTOP_START",
            "启动官方桌面",
            std::io::Error::from_raw_os_error(5),
        );
        assert_eq!(error.code, "DESKTOP_START");
        assert!(error.message.contains('5'));
        assert!(error.message.contains("启动官方桌面"));
    }
    #[cfg(windows)]
    #[test]
    fn fallback_only_matches_the_installed_codex_package() {
        assert!(packaged_path(Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1_x64__id\app\ChatGPT.exe"
        )));
        assert!(!packaged_path(Path::new(
            r"C:\Program Files\Other\ChatGPT.exe"
        )));
    }
    #[cfg(windows)]
    #[test]
    fn packaged_monitor_verifies_path_and_never_terminates_the_process() {
        let exe = std::env::current_exe().unwrap();
        let mut monitor = DesktopProcess::open_packaged(std::process::id(), &exe).unwrap();
        assert_eq!(monitor.pid().unwrap(), std::process::id());
        assert!(!monitor.exited().unwrap());
        drop(monitor);
        assert!(
            DesktopProcess::open_packaged(std::process::id(), Path::new(r"C:\wrong.exe")).is_err()
        );
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn package_helper_reports_spawn_error_and_honors_expired_launches() {
        use base64::Engine;
        let temp = tempfile::tempdir().unwrap();
        for expired in [false, true] {
            let output = temp.path().join(if expired {
                "expired.json"
            } else {
                "failed.json"
            });
            let request = serde_json::json!({"executable":temp.path().join("missing.exe"),"url":"ws://127.0.0.1:4501/desktop","result":output,"deadline":if expired {0} else {crate::now_ms()+30000}});
            let data = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&request).unwrap());
            let helper =
                include_str!("desktop_package_helper.ps1").replace("__REQUEST_BASE64__", &data);
            platform::powershell(&helper, &[]).await.unwrap();
            if expired {
                assert!(!output.exists());
            } else {
                let result: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
                assert!(result["pid"].is_null());
                assert_eq!(result["win32"], 2);
            }
        }
    }
}
