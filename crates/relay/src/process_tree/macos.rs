//! Track native children even when they call setsid (e.g. command/exec).
//! PID + kernel start time prevents signalling a later, unrelated PID owner.
use crate::error::{Error, Result};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Copy)]
struct Identity {
    pid: i32,
    parent: i32,
    seconds: u64,
    micros: u64,
}
impl Identity {
    fn read(pid: i32) -> Option<Self> {
        if pid <= 1 {
            return None;
        }
        // SAFETY: proc_bsdinfo is a C POD struct; libproc writes at most its size.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let count = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if count != size
            || info.pbi_status == libc::SZOMB
            || info.pbi_uid != unsafe { libc::geteuid() }
        {
            return None;
        }
        Some(Self {
            pid,
            parent: info.pbi_ppid as i32,
            seconds: info.pbi_start_tvsec,
            micros: info.pbi_start_tvusec,
        })
    }
    fn alive(self) -> bool {
        Self::read(self.pid)
            .is_some_and(|now| (now.seconds, now.micros) == (self.seconds, self.micros))
    }
    fn signal(self, signal: i32) -> Result<()> {
        // SAFETY: only a still-matching, positively tracked child identity is signalled.
        if self.alive()
            && unsafe { libc::kill(self.pid, signal) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(Error::new("BACKEND_STOP", "无法回收原生后代进程。"));
        }
        Ok(())
    }
}

struct Tracked {
    owned: HashMap<i32, Identity>,
}
impl Tracked {
    fn refresh(&mut self) -> Result<()> {
        self.owned.retain(|_, identity| identity.alive());
        let mut queue: Vec<_> = self.owned.values().copied().collect();
        while let Some(parent) = queue.pop() {
            if !parent.alive() {
                continue;
            }
            // Large enough for the native worker; fail rather than silently truncate a tree.
            let mut children = vec![0i32; 4096];
            let count = unsafe {
                libc::proc_listchildpids(
                    parent.pid,
                    children.as_mut_ptr().cast(),
                    (children.len() * std::mem::size_of::<i32>()) as i32,
                )
            };
            if count < 0 || count as usize >= children.len() {
                return Err(Error::new("BACKEND_STOP", "无法完整查询原生后代进程。"));
            }
            for &pid in &children[..count as usize] {
                if self.owned.contains_key(&pid) {
                    continue;
                }
                if let Some(identity) = Identity::read(pid) {
                    // Verify parent identity again before accepting its child snapshot.
                    if identity.parent == parent.pid && parent.alive() {
                        self.owned.insert(pid, identity);
                        queue.push(identity);
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct Tracker {
    root: Identity,
    state: Arc<Mutex<Tracked>>,
    task: tokio::task::JoinHandle<()>,
}
impl Tracker {
    pub fn attach(pid: i32) -> Result<Self> {
        let identity = Identity::read(pid)
            .ok_or_else(|| Error::new("BACKEND_GROUP", "无法确认原生进程身份。"))?;
        let state = Arc::new(Mutex::new(Tracked {
            owned: HashMap::from([(pid, identity)]),
        }));
        state.lock().unwrap().refresh()?;
        let tracked = state.clone();
        let task = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_millis(10));
            loop {
                ticks.tick().await;
                let _ = tracked.lock().unwrap().refresh();
            }
        });
        Ok(Self {
            root: identity,
            state,
            task,
        })
    }
    pub fn owns_session(&self) -> bool {
        // A session can outlive its leader, but a recycled leader PID is never ours.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let count = unsafe {
            libc::proc_pidinfo(
                self.root.pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if count == size {
            return (info.pbi_start_tvsec, info.pbi_start_tvusec)
                == (self.root.seconds, self.root.micros);
        }
        (unsafe { libc::kill(self.root.pid, 0) }) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    pub fn terminate(&self) -> Result<()> {
        self.task.abort();
        let mut state = self.state.lock().unwrap();
        // Freeze known parents, then discover and freeze any children forked during the snapshot.
        let capture = (|| -> Result<()> {
            for _ in 0..8 {
                state.refresh()?;
                let before = state.owned.len();
                for identity in state.owned.values() {
                    identity.signal(libc::SIGSTOP)?;
                }
                state.refresh()?;
                if state.owned.len() == before {
                    return Ok(());
                }
            }
            Err(Error::new(
                "BACKEND_STOP",
                "原生进程树仍在变化，无法确认完整回收。",
            ))
        })();
        // Always kill known identities, even if enumeration failed after freezing a parent.
        let mut failure = capture.err();
        for identity in state.owned.values() {
            if let Err(error) = identity.signal(libc::SIGKILL) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn empty(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap()
            .owned
            .values()
            .any(|identity| identity.alive())
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
    fn stale_pid_identity_cannot_signal_a_different_process_birth() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let identity = Identity::read(child.id() as i32).unwrap();
        let stale = Identity {
            seconds: identity.seconds + 1,
            ..identity
        };
        assert!(!stale.alive());
        stale.signal(libc::SIGKILL).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
