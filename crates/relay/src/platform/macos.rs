//! macOS process identity comes from libproc, never from a process's argv[0].
use super::{Listener, Process, Snapshot};
use crate::error::{Error, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

/// Rust's HTTP stack does not automatically inherit macOS System Settings proxies.
/// Only supply missing environment settings; upstream transport stays in app-server.
pub async fn apply_system_proxy(command: &mut Command) -> Result<()> {
    let explicit = std::env::vars_os().any(|(key, _)| {
        matches!(
            key.to_string_lossy().to_ascii_uppercase().as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
        )
    });
    if explicit {
        return Ok(());
    }
    let settings = output("/usr/sbin/scutil", &["--proxy"]).await?;
    for (key, value) in manual_proxies(&settings) {
        command.env(key, value);
    }
    Ok(())
}

fn manual_proxies(settings: &str) -> Vec<(&'static str, String)> {
    // Parse only top-level settings; scoped/supplemental dictionaries are not global proxies.
    let mut depth = 0usize;
    let mut values = std::collections::HashMap::new();
    for line in settings.lines() {
        let line = line.trim();
        if line.ends_with('{') {
            depth += 1;
            continue;
        }
        if line == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 1 {
            if let Some((key, value)) = line.split_once(" : ") {
                values.insert(key, value);
            }
        }
    }
    let mut result = Vec::new();
    for (kind, variable) in [("HTTP", "HTTP_PROXY"), ("HTTPS", "HTTPS_PROXY")] {
        if values.get(format!("{kind}Enable").as_str()) != Some(&"1") {
            continue;
        }
        let host = values
            .get(format!("{kind}Proxy").as_str())
            .copied()
            .unwrap_or("");
        let port = values
            .get(format!("{kind}Port").as_str())
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|p| *p > 0);
        if host.is_empty()
            || host
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b".-:[]".contains(&b))
        {
            continue;
        }
        if let Some(port) = port {
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host.to_owned()
            };
            result.push((variable, format!("http://{host}:{port}")));
        }
    }
    result
}

pub fn executable(pid: u32) -> Option<PathBuf> {
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for its reported length; libproc only writes it.
    let count = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if count <= 0 {
        return None;
    }
    let end = buffer.iter().position(|b| *b == 0)?;
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(
        buffer[..end].to_vec(),
    )))
}

pub async fn output(program: &str, args: &[&str]) -> Result<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::new("MACOS_TIMEOUT", "macOS 安装或进程查询超时。"))??;
    if !result.status.success() || result.stdout.len() > 4 * 1024 * 1024 {
        return Err(Error::new("MACOS_DISCOVERY", "macOS 安装或进程查询失败。"));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

pub async fn discover() -> Result<Snapshot> {
    let rows = output("/bin/ps", &["-axo", "pid=,ppid="]).await?;
    let mut processes = Vec::new();
    for row in rows.lines() {
        let ids: Vec<u32> = row
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if ids.len() != 2 {
            continue;
        }
        let Some(path) = executable(ids[0]) else {
            continue;
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if ![
            "ChatGPT",
            "Codex",
            "codex",
            "node",
            "codex-relay",
            "codex-bridge",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let is_app_server = name == "codex"
            && output("/bin/ps", &["-p", &ids[0].to_string(), "-o", "args="])
                .await
                .is_ok_and(|args| args.split_whitespace().any(|v| v == "app-server"));
        processes.push(Process {
            pid: ids[0],
            parent_pid: ids[1],
            name,
            executable: Some(path.to_string_lossy().into_owned()),
            is_app_server,
        });
    }
    let mut listeners = Vec::new();
    // lsof returns 1 if there are no matching sockets. Discovery can still find installations.
    if let Ok(sockets) = output("/usr/sbin/lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]).await {
        let mut pid = 0;
        for line in sockets.lines() {
            if let Some(value) = line.strip_prefix('p') {
                pid = value.parse().unwrap_or(0);
            }
            if let Some(value) = line.strip_prefix('n') {
                if processes.iter().any(|p| p.pid == pid) {
                    if let Some((address, port)) = value.rsplit_once(':') {
                        if let Ok(port) = port.parse() {
                            listeners.push(Listener {
                                pid,
                                port,
                                address: address.into(),
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(Snapshot {
        processes,
        pipes: vec![],
        listeners,
    })
}

pub fn bundle(desktop: &Path) -> Result<PathBuf> {
    let macos = desktop
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "MacOS"));
    let contents = macos
        .and_then(Path::parent)
        .filter(|p| p.file_name().is_some_and(|n| n == "Contents"));
    contents
        .and_then(Path::parent)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::new("DESKTOP_PATH", "桌面路径不是 macOS 应用包主程序。"))
}

pub fn desktop_owners(snapshot: &Snapshot) -> Vec<Process> {
    snapshot
        .processes
        .iter()
        .filter(|p| {
            !p.is_app_server
                && ["ChatGPT", "Codex"].contains(&p.name.as_str())
                && p.executable
                    .as_ref()
                    .is_some_and(|e| bundle(Path::new(e)).is_ok())
                && !snapshot
                    .processes
                    .iter()
                    .any(|parent| parent.pid == p.parent_pid && parent.executable == p.executable)
        })
        .cloned()
        .collect()
}

pub async fn validate_bundle(desktop: &Path, backend: &Path, archive: &Path) -> Result<()> {
    let root = bundle(desktop)?.canonicalize()?;
    let resources = root.join("Contents/Resources");
    if archive.canonicalize()? != resources.join("app.asar").canonicalize()?
        || !backend.canonicalize()?.starts_with(&resources)
        || backend.file_name().is_none_or(|n| n != "codex")
    {
        return Err(Error::new(
            "DESKTOP_PATH",
            "原生后端和归档必须属于同一个 Codex 应用包。",
        ));
    }
    let plist = root.join("Contents/Info.plist");
    let identifier = output(
        "/usr/libexec/PlistBuddy",
        &["-c", "Print :CFBundleIdentifier", &plist.to_string_lossy()],
    )
    .await?;
    let main = output(
        "/usr/libexec/PlistBuddy",
        &["-c", "Print :CFBundleExecutable", &plist.to_string_lossy()],
    )
    .await?;
    if identifier != "com.openai.codex"
        || desktop.canonicalize()? != root.join("Contents/MacOS").join(main).canonicalize()?
    {
        return Err(Error::new(
            "DESKTOP_IDENTITY",
            "应用包不是已知的官方 Codex 桌面。",
        ));
    }
    // Validate the signed bundle seal and the bundled native executable, including publisher.
    let requirement = "=anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    for path in [&root, backend] {
        output(
            "/usr/bin/codesign",
            &[
                "--verify",
                "--strict",
                "-R",
                requirement,
                &path.to_string_lossy(),
            ],
        )
        .await
        .map_err(|_| Error::new("DESKTOP_SIGNATURE", "官方 Codex 安装签名或资源校验失败。"))?;
    }
    Ok(())
}

pub async fn verify_peer(peer: u16, server: u16, pid: u32, expected: &Path) -> bool {
    let Ok(expected) = expected.canonicalize() else {
        return false;
    };
    if executable(pid).and_then(|p| p.canonicalize().ok()).as_ref() != Some(&expected) {
        return false;
    }
    let Ok(sockets) = output(
        "/usr/sbin/lsof",
        &[
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ],
    )
    .await
    else {
        return false;
    };
    let mut owner = 0;
    let endpoint = format!("n127.0.0.1:{peer}->127.0.0.1:{server}");
    let matched = sockets.lines().any(|line| {
        if let Some(value) = line.strip_prefix('p') {
            owner = value.parse().unwrap_or(0);
        }
        owner == pid && line == endpoint
    });
    matched && executable(pid).and_then(|p| p.canonicalize().ok()).as_ref() == Some(&expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_proxy_uses_global_manual_settings_and_rejects_invalid_endpoints() {
        let value = "<dictionary> {\n HTTPEnable : 1\n HTTPProxy : 127.0.0.1\n HTTPPort : 7897\n HTTPSEnable : 1\n HTTPSProxy : ::1\n HTTPSPort : 7897\n __SCOPED__ : <dictionary> {\n en0 : <dictionary> {\n HTTPProxy : evil.invalid\n }\n }\n}";
        assert_eq!(
            manual_proxies(value),
            vec![
                ("HTTP_PROXY", "http://127.0.0.1:7897".into()),
                ("HTTPS_PROXY", "http://[::1]:7897".into())
            ]
        );
        assert!(
            manual_proxies(
                "<dictionary> {\n HTTPEnable : 1\n HTTPProxy : user:pass@proxy\n HTTPPort : 7897\n}"
            )
            .is_empty()
        );
        assert!(
            manual_proxies(
                "<dictionary> {\n HTTPEnable : 0\n HTTPProxy : proxy\n HTTPPort : 7897\n}"
            )
            .is_empty()
        );
    }
    #[tokio::test]
    async fn connection_verification_requires_endpoint_pid_and_executable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap().port();
        let client = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, server))
            .await
            .unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        let peer = client.local_addr().unwrap().port();
        let pid = std::process::id();
        let exe = std::env::current_exe().unwrap();
        assert!(verify_peer(peer, server, pid, &exe).await);
        assert!(!verify_peer(peer, server, 0, &exe).await);
        assert!(!verify_peer(peer, server, pid, Path::new("/bin/sh")).await);
        assert!(!verify_peer(server, peer, pid + 1, &exe).await);
        drop(client);
    }
}
