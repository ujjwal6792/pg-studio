use crate::ssh::SshTunnel;
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
}

impl RunningSession {
    pub fn stop(&mut self) {
        if let Some(child) = self.studio_child.take() {
            let pid = child.id();
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
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
