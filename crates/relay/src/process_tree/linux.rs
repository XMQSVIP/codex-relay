//! Follow owned descendants across setsid, retaining kernel handles across parent exit.
//! pidfds and /proc birth times prevent a recycled PID from receiving our signals.
use crate::error::{Error, Result};
use std::{
    collections::HashMap,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct Stat {
    parent: i32,
    session: i32,
    start: u64,
    state: u8,
}
impl Stat {
    fn parse(text: &str) -> Option<Self> {
        // comm is parenthesized and may itself contain spaces and parentheses.
        let fields: Vec<_> = text
            .get(text.rfind(')')? + 1..)?
            .split_whitespace()
            .collect();
        Some(Self {
            state: fields.first()?.as_bytes().first().copied()?,
            parent: fields.get(1)?.parse().ok()?,
            session: fields.get(3)?.parse().ok()?,
            start: fields.get(19)?.parse().ok()?,
        })
    }
    fn read(pid: i32) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        Self::parse(&text).ok_or_else(|| std::io::Error::other("invalid process stat"))
    }
}

#[derive(Clone)]
struct Identity {
    pid: i32,
    birth: Stat,
    handle: Arc<OwnedFd>,
}
impl Identity {
    fn read(pid: i32) -> Result<Option<Self>> {
        if pid <= 1 {
            return Ok(None);
        }
        let birth = match Stat::read(pid) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::new("BACKEND_STOP", "无法读取原生进程身份。")),
        };
        // SAFETY: pidfd_open takes a PID and zero flags, returning a new owned descriptor.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
        if raw < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(Error::new(
                "BACKEND_GROUP",
                "无法建立原生进程句柄；Linux 原生账户需要支持 pidfd 的内核（5.3 或以上）。",
            ));
        }
        // SAFETY: a successful pidfd_open returns a valid, owned file descriptor.
        let handle = Arc::new(unsafe { OwnedFd::from_raw_fd(raw as i32) });
        let identity = Self { pid, birth, handle };
        // Reject a PID that changed owners while opening its handle.
        let current = match Stat::read(pid) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::new("BACKEND_STOP", "无法复核原生进程身份。")),
        };
        Ok((current.start == birth.start && !identity.exited()).then_some(identity))
    }
    fn current(&self) -> Option<Stat> {
        Stat::read(self.pid)
            .ok()
            .filter(|now| now.start == self.birth.start)
    }
    fn alive(&self) -> bool {
        if self.exited() {
            return false;
        }
        // Only the pidfd confirms that the whole process exited: its leader may
        // already be a zombie while other threads still run. /proc errors also
        // must not be mistaken for exit.
        Stat::read(self.pid)
            .map(|now| now.start == self.birth.start)
            .unwrap_or(true)
    }
    fn exited(&self) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives one valid descriptor and never blocks (zero timeout).
        (unsafe { libc::poll(&mut descriptor, 1, 0) }) == 1
            && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0
    }
    fn stopped(&self) -> bool {
        !self.alive()
            || self
                .current()
                .is_some_and(|stat| matches!(stat.state, b'T' | b't'))
    }
    fn signal(&self, signal: i32) -> Result<()> {
        if !self.alive() {
            return Ok(());
        }
        // SAFETY: the descriptor pins the process identity even if its PID is reused
        // between the birth check and this syscall. A dead handle returns ESRCH.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.handle.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(Error::new("BACKEND_STOP", "无法回收原生后代进程。"));
        }
        Ok(())
    }
}

struct Tracked {
    owned: HashMap<i32, Identity>,
    failure: Option<Error>,
}
impl Tracked {
    fn refresh(&mut self) -> Result<()> {
        self.owned.retain(|_, identity| identity.alive());
        let mut queue: Vec<_> = self.owned.values().cloned().collect();
        while let Some(parent) = queue.pop() {
            if !parent.alive() {
                continue;
            }
            let tasks = match std::fs::read_dir(format!("/proc/{}/task", parent.pid)) {
                Ok(tasks) => tasks,
                Err(_) if !parent.alive() => continue,
                Err(_) => return Err(Error::new("BACKEND_STOP", "无法枚举原生进程线程。")),
            };
            // Children belong to the thread that forked them, not necessarily the leader.
            for task in tasks {
                let path = task?.path().join("children");
                let children = match std::fs::read_to_string(&path) {
                    Ok(children) => children,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(Error::new("BACKEND_STOP", "无法完整查询原生后代进程。")),
                };
                for child in children.split_whitespace() {
                    let pid = child
                        .parse::<i32>()
                        .map_err(|_| Error::new("BACKEND_STOP", "原生后代 PID 无效。"))?;
                    if self.owned.contains_key(&pid) {
                        continue;
                    }
                    if let Some(identity) = Identity::read(pid)? {
                        if identity.birth.parent == parent.pid && parent.alive() {
                            if self.owned.len() >= 4096 {
                                return Err(Error::new(
                                    "BACKEND_STOP",
                                    "原生后代数量超限，无法完整回收。",
                                ));
                            }
                            self.owned.insert(pid, identity.clone());
                            queue.push(identity);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) struct Tracker {
    root: Identity,
    state: Arc<Mutex<Tracked>>,
    task: tokio::task::JoinHandle<()>,
}
impl Tracker {
    pub(super) fn attach(pid: i32) -> Result<Self> {
        let root = Identity::read(pid)?
            .ok_or_else(|| Error::new("BACKEND_GROUP", "无法确认原生进程身份。"))?;
        let state = Arc::new(Mutex::new(Tracked {
            owned: HashMap::from([(pid, root.clone())]),
            failure: None,
        }));
        state.lock().unwrap().refresh()?;
        let tracked = state.clone();
        let task = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_millis(10));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                let mut state = tracked.lock().unwrap();
                if let Err(error) = state.refresh() {
                    // Once a snapshot was incomplete, an exited parent could have
                    // left an unobserved child. Do not later claim complete cleanup.
                    state.failure.get_or_insert(error);
                }
            }
        });
        Ok(Self { root, state, task })
    }
    pub(super) fn owns_session(&self) -> bool {
        match Stat::read(self.root.pid) {
            Ok(now) => now.start == self.root.birth.start,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
    pub(super) fn terminate_member(&self, pid: i32) -> Result<()> {
        if self.owns_session() {
            if let Some(member) = Identity::read(pid)? {
                if member.birth.session == self.root.pid && self.owns_session() {
                    member.signal(libc::SIGKILL)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn terminate(&self) -> Result<()> {
        self.task.abort();
        let mut state = self.state.lock().unwrap();
        let capture = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                state.refresh()?;
                let before = state.owned.len();
                for identity in state.owned.values() {
                    identity.signal(libc::SIGSTOP)?;
                }
                // Wait until the stop has taken effect before taking the final fork snapshot.
                while !state.owned.values().all(Identity::stopped) {
                    if Instant::now() >= deadline {
                        return Err(Error::new("BACKEND_STOP", "无法暂停原生进程树以完成回收。"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                state.refresh()?;
                if state.owned.len() == before && state.owned.values().all(Identity::stopped) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(Error::new(
                        "BACKEND_STOP",
                        "原生进程树仍在变化，无法确认完整回收。",
                    ));
                }
            }
        })();
        let mut failure = state.failure.clone().or_else(|| capture.err());
        // Never leave frozen children behind when a partial capture failed.
        for identity in state.owned.values() {
            if let Err(error) = identity.signal(libc::SIGKILL) {
                failure.get_or_insert(error);
            }
        }
        state.failure = failure.clone();
        failure.map_or(Ok(()), Err)
    }
    pub(super) fn empty(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.failure.is_none() && !state.owned.values().any(Identity::alive)
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_handles_parentheses_and_spaces_in_process_names() {
        let text = "123 (worker (child) name)) S 10 123 123 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 9876";
        let stat = Stat::parse(text).unwrap();
        assert_eq!((stat.parent, stat.session, stat.start), (10, 123, 9876));
        assert!(Stat::parse("123 (incomplete)").is_none());
    }

    #[test]
    fn stale_birth_cannot_signal_another_process() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let mut stale = Identity::read(child.id() as i32).unwrap().unwrap();
        stale.birth.start += 1;
        assert!(!stale.alive());
        stale.signal(libc::SIGKILL).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    async fn incomplete_tracking_cannot_be_reported_as_clean() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let tracker = Tracker::attach(child.id() as i32).unwrap();
        tracker.state.lock().unwrap().failure =
            Some(Error::new("BACKEND_STOP", "incomplete snapshot"));
        assert_eq!(tracker.terminate().unwrap_err().code, "BACKEND_STOP");
        child.wait().unwrap();
        assert!(!tracker.empty());
    }
}
