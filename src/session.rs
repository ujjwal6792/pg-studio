use crate::ssh::SshTunnel;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Pulling,
    Running,
    Error,
    Stopped,
}

pub struct RunningSession {
    pub project_name: String,
    pub studio_port: u16,
    pub ssh: Option<SshTunnel>,
    pub studio_child: Option<Child>,
    pub status: SessionStatus,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub tunnel_url: Option<String>,
    pub error: Option<String>,
    pub auto_open: bool,
    pub studio_ready: bool,
    pub studio_pid: Option<u32>,
    pub ssh_pid: Option<u32>,
    pub log_path: Option<PathBuf>,
    /// Unix timestamp (secs) when the session started; `None` for restored
    /// detached sessions whose original start time is unknown.
    pub started_at: Option<i64>,
}

impl RunningSession {
    fn terminate_processes(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.studio_pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        #[cfg(unix)]
        if let Some(pid) = self.ssh_pid.take() {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        if let Some(child) = self.studio_child.take() {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
        }
        self.ssh = None;
        self.studio_pid = None;
        self.log_path = None;
    }

    pub fn stop(&mut self) {
        self.terminate_processes();
        self.auto_open = false;
        self.status = SessionStatus::Stopped;
    }

    pub fn fail(&mut self, error: String) {
        self.terminate_processes();
        self.auto_open = false;
        self.studio_ready = false;
        self.error = Some(error);
        self.status = SessionStatus::Error;
    }

    pub fn url(&self) -> Option<&str> {
        self.tunnel_url.as_deref()
    }

    /// Human-readable uptime since `started_at`, e.g. "2h 5m" or "48s".
    pub fn uptime(&self) -> Option<String> {
        let started = self.started_at?;
        let secs = (chrono::Utc::now().timestamp() - started).max(0);
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        Some(if h > 0 {
            format!("{h}h {m}m")
        } else if m > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{s}s")
        })
    }
}
