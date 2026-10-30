//! A job owns only a newly spawned backend and its descendants, never the desktop.
use crate::error::Result;
use tokio::process::Child;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
use {
    crate::error::Error,
    std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
};

pub struct ProcessTree {
    #[cfg(windows)]
    job: OwnedHandle,
    #[cfg(unix)]
    group: i32,
    #[cfg(unix)]
    terminated: std::sync::atomic::AtomicBool,
    #[cfg(target_os = "macos")]
    descendants: macos::Tracker,
}
impl ProcessTree {
    /// Establish an owned session before exec; native tools may create their own groups.
    pub fn configure(command: &mut tokio::process::Command) {
        #[cfg(unix)]
        {
            // SAFETY: setsid is async-signal-safe; this closure allocates nothing on success.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        #[cfg(not(unix))]
        let _ = command;
    }
    pub fn attach(child: &Child) -> Result<Self> {
        #[cfg(windows)]
        {
            let failure = || Error::new("BACKEND_JOB", "无法建立原生后端进程树回收约束。");
            let process = child.raw_handle().ok_or_else(failure)?;
            // SAFETY: null attributes/name create an unnamed, non-inheritable job.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(failure());
            }
            // SAFETY: successful CreateJobObjectW returns an owned, valid handle.
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the initialized structure and owned job handle remain valid
            // for these calls; the child is retained and has not been awaited.
            let configured = unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if configured == 0
                || unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0
            {
                return Err(failure());
            }
            Ok(Self { job })
        }
        #[cfg(unix)]
        {
            let failure =
                || crate::error::Error::new("BACKEND_GROUP", "无法建立独立原生后端进程组。");
            let group = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .ok_or_else(failure)?;
            // SAFETY: read-only query; a child can only be attached to its own group.
            if group <= 1
                || unsafe { libc::getpgid(group) } != group
                || unsafe { libc::getsid(group) } != group
            {
                return Err(failure());
            }
            Ok(Self {
                group,
                terminated: std::sync::atomic::AtomicBool::new(false),
                #[cfg(target_os = "macos")]
                descendants: macos::Tracker::attach(group)?,
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = child;
            Err(crate::error::Error::new(
                "PLATFORM_UNSUPPORTED",
                "不支持进程树回收。",
            ))
        }
    }
    pub fn terminate(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;
            if !self.terminated.swap(true, Ordering::SeqCst) {
                // Freeze the session leader's group before enumerating other native tool groups.
                #[cfg(not(target_os = "macos"))]
                unsafe {
                    libc::kill(-self.group, libc::SIGSTOP);
                }
                #[cfg(target_os = "macos")]
                let tracked = self.descendants.terminate();
                let result = self.terminate_members();
                // SAFETY: negative PID targets only the new group configured before spawn.
                // Never signal the bridge's or the personal desktop's process group.
                #[cfg(not(target_os = "macos"))]
                if unsafe { libc::kill(-self.group, libc::SIGKILL) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    self.terminated.store(false, Ordering::SeqCst);
                    return Err(crate::error::Error::new(
                        "BACKEND_STOP",
                        "无法回收原生后端进程组。",
                    ));
                }
                result?;
                #[cfg(target_os = "macos")]
                tracked?;
            }
        }
        #[cfg(windows)]
        {
            // SAFETY: this handle owns only this backend's job. It remains open
            // throughout termination, including when the parent exited earlier.
            if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
                return Err(Error::new("BACKEND_STOP", "无法回收原生后端进程树。"));
            }
        }
        Ok(())
    }
    pub async fn wait(&self) -> Result<()> {
        #[cfg(unix)]
        {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let members = self.members()?;
                    #[cfg(target_os = "macos")]
                    let descendants_gone = self.descendants.empty();
                    #[cfg(not(target_os = "macos"))]
                    let descendants_gone = true;
                    if members.is_empty() && descendants_gone {
                        return Ok::<(), crate::error::Error>(());
                    }
                    // Re-scan the session after the leader is gone, including reparented grandchildren.
                    self.terminate_members()?;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| crate::error::Error::new("BACKEND_STOP", "原生后代进程未完全退出。"))??;
        }
        Ok(())
    }
    #[cfg(unix)]
    fn members(&self) -> Result<Vec<i32>> {
        #[cfg(target_os = "macos")]
        if !self.descendants.owns_session() {
            return Ok(vec![]);
        }
        let output = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,stat="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(crate::error::Error::new(
                "BACKEND_STOP",
                "无法检查原生会话后代。",
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse::<i32>().ok()?;
                let state = fields.next()?;
                // SAFETY: read-only membership check; the personal desktop has a different session.
                (pid > 1 && !state.starts_with('Z') && unsafe { libc::getsid(pid) } == self.group)
                    .then_some(pid)
            })
            .collect())
    }
    #[cfg(unix)]
    fn terminate_members(&self) -> Result<()> {
        for pid in self.members()? {
            // SAFETY: recheck membership immediately before signalling to avoid stale snapshots.
            if unsafe { libc::getsid(pid) } == self.group
                && unsafe { libc::kill(pid, libc::SIGKILL) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                return Err(crate::error::Error::new(
                    "BACKEND_STOP",
                    "无法回收原生会话后代。",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    #[tokio::test]
    async fn owned_job_reaps_descendants_after_parent_exit_and_when_dropped() {
        for stop_parent_first in [true, false] {
            let mut command = Command::new("powershell.exe");
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                "$taskChild = Start-Process powershell.exe -ArgumentList '-NoLogo -NoProfile -NonInteractive -Command Start-Sleep -Seconds 60' -WindowStyle Hidden -PassThru; $taskChild.Id; Start-Sleep -Seconds 60"])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
            crate::platform::hide(&mut command);
            let mut child = command.spawn().unwrap();
            let tree = ProcessTree::attach(&child).unwrap();
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(10),
                BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            let descendant: u32 = line.trim().parse().unwrap();
            // SAFETY: query only the test child just reported by our own process;
            // keep its handle to observe termination without PID-reuse races.
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, descendant) };
            assert!(!raw.is_null());
            let descendant = unsafe { OwnedHandle::from_raw_handle(raw) };
            if stop_parent_first {
                child.kill().await.unwrap();
                tree.terminate().unwrap();
            }
            drop(tree);
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap();
            // SAFETY: handle is open and only used to wait for the test descendant.
            assert_eq!(
                unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
                0
            );
        }
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
    };

    async fn running(pid: u32) -> bool {
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .await
            .unwrap();
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|s| !s.trim().is_empty() && !s.trim().starts_with('Z'))
    }

    #[tokio::test]
    async fn owned_group_reaps_descendants_after_parent_exit_and_on_drop() {
        for parent_exits_first in [true, false] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", "sleep 60 & echo $!; wait"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            ProcessTree::configure(&mut command);
            let mut child = command.spawn().unwrap();
            let tree = ProcessTree::attach(&child).unwrap();
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(5),
                BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            let descendant = line.trim().parse().unwrap();
            assert!(running(descendant).await);
            if parent_exits_first {
                child.kill().await.unwrap();
                tree.terminate().unwrap();
            }
            drop(tree);
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while running(descendant).await {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("owned descendant survived group cleanup");
        }
    }

    #[tokio::test]
    async fn refuses_to_attach_an_unowned_process_group() {
        let mut child = Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        assert!(ProcessTree::attach(&child).is_err());
        child.kill().await.unwrap();
    }
}
