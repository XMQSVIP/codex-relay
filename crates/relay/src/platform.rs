use crate::error::{Error, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::{desktop_owners, discover, verify_peer};

pub fn hide(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    #[cfg(not(windows))]
    let _ = command;
}

pub async fn powershell(script: &str, env: &[(&str, String)]) -> Result<String> {
    if !cfg!(windows) {
        return Err(Error::new("WINDOWS_REQUIRED", "桌面连接需要 Windows。"));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    hide(&mut command);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| Error::new("PROCESS_TIMEOUT", "Windows 状态查询超时。"))??;
    if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
        return Err(Error::new("WINDOWS_DISCOVERY", "Windows 状态查询失败。"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_start_matches('\u{feff}')
        .trim()
        .to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub executable: Option<String>,
    pub is_app_server: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listener {
    pub pid: u32,
    pub port: u16,
    pub address: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub processes: Vec<Process>,
    pub pipes: Vec<String>,
    pub listeners: Vec<Listener>,
}

#[cfg(not(target_os = "macos"))]
pub async fn discover() -> Result<Snapshot> {
    let value = powershell(r#"
$ErrorActionPreference = 'Stop'
$processes = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(codex|ChatGPT|node|codex-relay|codex-bridge)\.exe$' } | ForEach-Object {
 [pscustomobject]@{ pid=[int]$_.ProcessId; parentPid=[int]$_.ParentProcessId; name=$_.Name; executable=$_.ExecutablePath; isAppServer=[bool]($_.Name -ieq 'codex.exe' -and $_.CommandLine -match '\bapp-server\b') }
})
$pipes = @(Get-ChildItem -LiteralPath '\\.\pipe\' | Where-Object { $_.Name -like 'codex-app-tools-*' } | ForEach-Object { '\\.\pipe\' + $_.Name })
$listeners = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.OwningProcess -in $processes.pid } | ForEach-Object { [pscustomobject]@{ pid=[int]$_.OwningProcess; port=[int]$_.LocalPort; address=$_.LocalAddress } })
[pscustomobject]@{ processes=$processes; pipes=$pipes; listeners=$listeners } | ConvertTo-Json -Depth 5 -Compress
"#, &[]).await?;
    Ok(serde_json::from_str(&value)?)
}

#[cfg(not(target_os = "macos"))]
pub fn desktop_owners(snapshot: &Snapshot) -> Vec<Process> {
    snapshot
        .processes
        .iter()
        .filter(|p| {
            !p.is_app_server
                && ["chatgpt.exe", "codex.exe"].contains(&p.name.to_lowercase().as_str())
                && p.executable.as_ref().is_some_and(|path| {
                    path.to_lowercase()
                        .replace('/', "\\")
                        .contains("\\windowsapps\\openai.codex_")
                })
                && !snapshot
                    .processes
                    .iter()
                    .any(|parent| parent.pid == p.parent_pid && parent.executable == p.executable)
        })
        .cloned()
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub async fn verify_peer(peer: u16, server: u16, pid: u32, executable: &Path) -> bool {
    if pid == 0 {
        return false;
    }
    powershell(r#"
$ErrorActionPreference = 'Stop'
$connection = @(Get-NetTCPConnection -State Established -LocalAddress '127.0.0.1' -LocalPort $env:BRIDGE_PEER -RemoteAddress '127.0.0.1' -RemotePort $env:BRIDGE_SERVER -ErrorAction SilentlyContinue)
$owner = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $env:BRIDGE_PID)
if ($connection.Count -eq 1 -and $connection[0].OwningProcess -eq [int]$env:BRIDGE_PID -and $owner.ExecutablePath -eq $env:BRIDGE_EXE) { 'verified' }
"#, &[("BRIDGE_PEER", peer.to_string()), ("BRIDGE_SERVER", server.to_string()), ("BRIDGE_PID", pid.to_string()), ("BRIDGE_EXE", executable.to_string_lossy().into_owned())]).await.is_ok_and(|s| s == "verified")
}

pub async fn pipe_owner(pipe: &str) -> Result<u32> {
    let suffix = pipe
        .strip_prefix(r"\\.\pipe\codex-app-tools-")
        .ok_or_else(|| Error::new("PIPE_NAME", "只允许本机 Codex 工具管道。"))?;
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::new("PIPE_NAME", "管道名称无效。"));
    }
    let output = powershell(r#"
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class BridgePipe { [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GetNamedPipeServerProcessId(IntPtr pipe, out uint pid); }
'@
$stream = [System.IO.Pipes.NamedPipeClientStream]::new('.', $env:BRIDGE_PIPE.Substring(9), [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
try { $stream.Connect(2000); [uint32]$p = 0; if (-not [BridgePipe]::GetNamedPipeServerProcessId($stream.SafePipeHandle.DangerousGetHandle(), [ref]$p)) { throw 'owner' }; $p } finally { $stream.Dispose() }
"#, &[("BRIDGE_PIPE", pipe.into())]).await?;
    output
        .parse::<u32>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| Error::new("PIPE_OWNER", "无法核验管道进程。"))
}

pub async fn protect_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = tokio::fs::symlink_metadata(path).await?;
        // Never follow a symlink or chmod a directory owned by another user.
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::new(
                "PRIVATE_DIRECTORY",
                "账户目录必须由当前用户拥有，且不能是符号链接。",
            ));
        }
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
        Ok(())
    }
    #[cfg(windows)]
    {
        powershell(r#"
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& icacls.exe $env:BRIDGE_PRIVATE_DIR /inheritance:r /grant:r ('*' + $sid + ':(OI)(CI)F') '*S-1-5-18:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'acl' }
"#, &[("BRIDGE_PRIVATE_DIR", path.to_string_lossy().into_owned())]).await?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    Err(Error::new(
        "PLATFORM_UNSUPPORTED",
        "当前平台不支持账户目录权限保护。",
    ))
}
