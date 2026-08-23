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
pub fn direct_target(proj: &ProjectConfig) -> (String, u16) {
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

/// A fully-parsed `postgresql://` URL with every component the form needs.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedPgUrl {
    pub user: String,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    pub dbname: String,
}

/// Parses a complete `postgresql://user[:pass]@host[:port]/db[?params]` URL.
/// Returns `None` for partial input (e.g. while a user is still typing), so
/// callers can safely use it as an auto-detection gate on paste.
pub fn parse_full_pg_url(url: &str) -> Option<ParsedPgUrl> {
    let trimmed = url.trim();
    let scheme = trimmed.split_once("://")?.0;
    if !scheme.eq_ignore_ascii_case("postgresql") && !scheme.eq_ignore_ascii_case("postgres") {
        return None;
    }
    let rest = &trimmed[scheme.len() + 3..];
    // No database segment -> treat as incomplete.
    let i = rest.find('/')?;
    let (authority, path) = (&rest[..i], &rest[i + 1..]);
    if authority.is_empty() {
        return None;
    }
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (u, h),
        None => ("", authority),
    };
    let (user, password) = userinfo
        .split_once(':')
        .map(|(u, p)| (u.to_string(), Some(p.to_string())))
        .unwrap_or((userinfo.to_string(), None));
    // IPv6 literals arrive as `[::1]:5432`.
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (host, rest) = inner.split_once(']')?;
        let port = rest.strip_prefix(':').unwrap_or("5432");
        (format!("[{host}]"), port)
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h.to_string(), p),
            None => (hostport.to_string(), "5432"),
        }
    };
    let dbname = path.split('?').next().unwrap_or("");
    if host.is_empty() || dbname.is_empty() {
        return None;
    }
    Some(ParsedPgUrl {
        user,
        password,
        host,
        port: port.parse().ok()?,
        dbname: dbname.to_string(),
    })
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

    #[test]
    fn parses_full_url_with_every_component() {
        let parsed =
            parse_full_pg_url("postgresql://alice:s3cret@db.example.com:6543/app?sslmode=require")
                .unwrap();
        assert_eq!(
            parsed,
            ParsedPgUrl {
                user: "alice".into(),
                password: Some("s3cret".into()),
                host: "db.example.com".into(),
                port: 6543,
                dbname: "app".into(),
            }
        );
    }

    #[test]
    fn parses_minimal_full_url_with_defaults() {
        let parsed = parse_full_pg_url("postgres://bob@localhost/mydb").unwrap();
        assert_eq!(parsed.port, 5432);
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.host, "localhost");
        assert_eq!(parsed.dbname, "mydb");
    }

    #[test]
    fn rejects_incomplete_urls_so_typing_does_not_trigger() {
        for partial in [
            "postgres",
            "postgresql://",
            "postgresql://loc",
            "postgresql://loc:5432", // no /dbname yet
            "postgresql:///db",      // no host
        ] {
            assert_eq!(parse_full_pg_url(partial), None, "should reject {partial}");
        }
    }

    #[test]
    fn rejects_non_postgres_schemes() {
        assert_eq!(parse_full_pg_url("mysql://root@host/db"), None);
    }
}
