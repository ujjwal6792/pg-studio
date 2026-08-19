use anyhow::{Context, Result};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

pub struct SshTunnel {
    pub child: Child,
    pub local_port: u16,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn find_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind to a local port to find a free one")?;
    let port = listener
        .local_addr()
        .context("Failed to get local address")?
        .port();
    // Dropping the listener releases the port for reuse.
    Ok(port)
}

pub fn establish_tunnel(ssh_connection: &str, remote_port: &str) -> Result<SshTunnel> {
    let local_port = find_free_port()?;

    let child = Command::new("ssh")
        .arg("-N")
        .arg("-L")
        .arg(format!("{}:localhost:{}", local_port, remote_port))
        .arg(ssh_connection)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn SSH process")?;

    let tunnel = SshTunnel { child, local_port };

    // Wait a brief moment to ensure the tunnel is up and listening.
    for _ in 0..10 {
        if TcpListener::bind(format!("127.0.0.1:{}", local_port)).is_err() {
            // Port is in use, which means SSH successfully bound to it.
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }

    Ok(tunnel)
}
