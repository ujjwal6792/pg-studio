use anyhow::{Context, Result};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

pub struct SshTunnel {
    pub child: Child,
    pub local_port: u16,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        println!("Shutting down SSH tunnel on local port {}", self.local_port);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn establish_tunnel(ssh_connection: &str, remote_port: &str) -> Result<SshTunnel> {
    // Find a free local port
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind to a local port to find a free one")?;
    let local_port = listener
        .local_addr()
        .context("Failed to get local address")?
        .port();
    drop(listener); // Free the port so SSH can use it

    println!(
        "Establishing SSH tunnel (local port {} -> remote port {})...",
        local_port, remote_port
    );

    let child = Command::new("ssh")
        .arg("-N")
        .arg("-L")
        .arg(format!("{}:localhost:{}", local_port, remote_port))
        .arg(ssh_connection)
        .spawn()
        .context("Failed to spawn SSH process")?;

    let tunnel = SshTunnel { child, local_port };

    // Wait a brief moment to ensure the tunnel is up and listening
    // A better approach would be to poll the local port until it accepts connections
    for _ in 0..10 {
        if TcpListener::bind(format!("127.0.0.1:{}", local_port)).is_err() {
            // Port is in use, which means SSH successfully bound to it!
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }

    Ok(tunnel)
}
