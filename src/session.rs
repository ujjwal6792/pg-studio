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
}

impl RunningSession {
    pub fn stop(&mut self) {
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
        // Dropping the SSH tunnel kills the ssh process.
        self.ssh = None;
        self.auto_open = false;
        self.status = SessionStatus::Stopped;
    }

    pub fn url(&self) -> Option<&str> {
        self.tunnel_url.as_deref()
    }
}
