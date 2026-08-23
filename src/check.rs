use crate::config::{ConnectionType, ProjectConfig};
use anyhow::{Context, Result, anyhow};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Verifies that the selected project is reachable *without* launching
/// Drizzle Studio: SSH projects get an `ssh exit` probe, URL/Local projects
/// get a TCP dial against the resolved host:port.
pub fn check_connection(proj: &ProjectConfig) -> Result<String> {
    match proj.connection_type {
        ConnectionType::Ssh => check_ssh(&proj.ssh_connection),
        ConnectionType::Url | ConnectionType::Local => {
            let (host, port) = direct_target(proj);
            check_tcp(&host, port)
        }
    }
}

fn check_ssh(ssh_connection: &str) -> Result<String> {
    let target = ssh_connection.trim();
    if target.is_empty() {
        return Err(anyhow!("SSH connection string is empty"));
    }
    let output = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ])
        .arg(target)
        .arg("exit")
        .stdin(Stdio::null())
        .output()
        .context("Failed to spawn ssh for connection test")?;
    if output.status.success() {
        Ok(format!("SSH host '{target}' reachable"))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        Err(anyhow!(
            "SSH host '{target}' unreachable{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ))
    }
}

/// Host/port a direct connection would actually dial, including defaults and
/// embedded URL targets.
fn direct_target(proj: &ProjectConfig) -> (String, u16) {
    if proj.connection_type == ConnectionType::Url
        && !proj.db_url.is_empty()
        && let Some((host, port)) = parse_db_url_host_port(&proj.db_url)
    {
        return (host, port);
    }
    let host = if proj.db_host.trim().is_empty() {
        match proj.connection_type {
            ConnectionType::Local => "localhost".to_string(),
            _ => "127.0.0.1".to_string(),
        }
    } else {
        proj.db_host.trim().to_string()
    };
    let port = proj.db_port.trim().parse().unwrap_or(5432);
    (host, port)
}

/// Extracts `(host, port)` from a `postgresql://user[:pass]@host[:port]/db` URL.
pub fn parse_db_url_host_port(url: &str) -> Option<(String, u16)> {
    let authority = url.split_once("://")?.1.split(['/', '?']).next()?;
    let hostport = match authority.rsplit_once('@') {
        Some((_, h)) => h.to_string(),
        None => authority.to_string(),
    };
    // IPv6 literals arrive as `[::1]:5432`; keep the brackets so socket
    // address parsing still recognises them later.
    if let Some(inner) = hostport.strip_prefix('[') {
        let (host, rest) = inner.split_once(']')?;
        let port = rest.strip_prefix(':')?;
        return Some((format!("[{host}]"), port.parse().ok()?));
    }
    let (host, port) = hostport.split_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

fn check_tcp(host: &str, port: u16) -> Result<String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("Failed to resolve {host}:{port}"))?
        .collect();
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(_) => return Ok(format!("{host}:{port} reachable")),
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!(
        "{}:{port} not reachable ({})",
        host,
        last_err
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no addresses resolved".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_pass_host_port() {
        assert_eq!(
            parse_db_url_host_port("postgresql://alice:s3cret@db.example.com:6543/app"),
            Some(("db.example.com".to_string(), 6543))
        );
    }

    #[test]
    fn parses_without_user_or_port() {
        assert_eq!(
            parse_db_url_host_port("postgresql://db.example.com/app"),
            None // no ':' means no explicit port -> parser requires one
        );
        assert_eq!(
            parse_db_url_host_port("postgresql://alice@db.example.com:5432/app"),
            Some(("db.example.com".to_string(), 5432))
        );
    }

    #[test]
    fn parses_ipv6_literal() {
        assert_eq!(
            parse_db_url_host_port("postgresql://alice@[2001:db8::1]:5433/app"),
            Some(("[2001:db8::1]".to_string(), 5433))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_db_url_host_port("not-a-url"), None);
        assert_eq!(parse_db_url_host_port(""), None);
    }
}
