//! Real native processes with a local sleep fixture. No credentials or model requests.
#![cfg(unix)]
use codex_relay::{backend::Backend, installation::Installation};
use serde_json::json;
use std::time::Duration;

async fn running(pid: u32) -> bool {
    let result = tokio::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .await
        .unwrap();
    result.status.success()
        && String::from_utf8_lossy(&result.stdout)
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim().starts_with('Z'))
}

#[tokio::test]
#[ignore = "Requires BRIDGE_NATIVE_INSTALLATION; owned native command descendants, no model usage"]
async fn native_command_descendants_are_reaped_on_stop_drop_and_parent_exit() {
    let install: Installation = serde_json::from_slice(
        &std::fs::read(std::env::var("BRIDGE_NATIVE_INSTALLATION").unwrap()).unwrap(),
    )
    .unwrap();
    for mode in ["stop", "drop", "parent-exit"] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let pid_file = temp.path().join("descendant.pid");
        let backend = Backend::start_direct(&install, temp.path(), Some(&home))
            .await
            .unwrap();
        let root_pid = backend.pid;
        let rpc = backend.rpc.clone();
        let params = json!({"command":["/bin/sh","-c","sleep 60 & echo $! > \"$1\"; wait","bridge-fixture",pid_file],"cwd":temp.path(),"sandboxPolicy":{"type":"dangerFullAccess"},"timeoutMs":60000});
        let task = tokio::spawn(async move {
            rpc.request_timeout("command/exec", params, Duration::from_secs(65))
                .await
        });
        let descendant: u32 = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&pid_file).await {
                    if let Ok(pid) = text.trim().parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("native command did not spawn the local fixture");
        assert!(running(descendant).await);
        eprintln!(
            "native fixture {mode}: root session {}, descendant session {}",
            unsafe { libc::getsid(root_pid as i32) },
            unsafe { libc::getsid(descendant as i32) }
        );
        if mode == "parent-exit" {
            // Exercise a crashed worker with an established native command tree.
            tokio::time::sleep(Duration::from_millis(100)).await;
            // SAFETY: only the native child started by this test is signalled.
            assert_eq!(unsafe { libc::kill(root_pid as i32, libc::SIGKILL) }, 0);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if mode != "drop" {
            backend.stop().await.unwrap();
        }
        drop(backend);
        task.abort();
        let gone = tokio::time::timeout(Duration::from_secs(5), async {
            while running(descendant).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        if !gone {
            // Do not leave a failing test's known sleep process running.
            unsafe {
                libc::kill(descendant as i32, libc::SIGKILL);
            }
        }
        assert!(gone, "native descendant survived {mode}");
        assert!(!running(root_pid).await);
    }
}
