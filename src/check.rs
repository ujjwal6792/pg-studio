use crate::config::{ConnectionType, ProjectConfig, expand_tilde};
use anyhow::{Context, Result, anyhow};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Verifies that the selected project is reachable *without* launching
/// Drizzle Studio. Wire engines (Postgres/MySQL) get an SSH exec probe or a
/// TCP dial; SQLite checks the database file; D1 validates identifiers and,
/// when `curl` is available, probes the Cloudflare API; Turso dials its host.
pub fn check_connection(proj: &ProjectConfig) -> Result<String> {
    match proj.engine {
        crate::config::Engine::Sqlite => check_sqlite_file(&proj.db_path),
        crate::config::Engine::D1 => {
            let token = proj.get_password().unwrap_or_default();
            check_d1(&proj.cf_account_id, &proj.cf_database_id, &token)
        }
        crate::config::Engine::Turso => check_turso(&proj.db_url),
        crate::config::Engine::Postgres | crate::config::Engine::Mysql => {
            match proj.connection_type {
                ConnectionType::Ssh => check_ssh(&proj.ssh_connection),
                ConnectionType::Url | ConnectionType::Local => {
                    let (host, port) = direct_target(proj);
                    check_tcp(&host, port)
                }
            }
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
    let default_port = match proj.engine.default_port() {
        0 => 5432,
        p => p,
    };
    if proj.connection_type == ConnectionType::Url
        && !proj.db_url.is_empty()
        && let Some((host, port)) = parse_db_url_host_port(&proj.db_url, default_port)
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
    let port = proj.db_port.trim().parse().unwrap_or(default_port);
    (host, port)
}

/// Extracts `(host, port)` from a `scheme://user[:pass]@host[:port]/db` URL,
/// falling back to `default_port` when the URL carries no explicit port.
pub fn parse_db_url_host_port(url: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = url.split_once("://")?.1.split(['/', '?']).next()?;
    let hostport = match authority.rsplit_once('@') {
        Some((_, h)) => h.to_string(),
        None => authority.to_string(),
    };
    // IPv6 literals arrive as `[::1]:5432`; keep the brackets so socket
    // address parsing still recognises them later.
    if let Some(inner) = hostport.strip_prefix('[') {
        let (host, rest) = inner.split_once(']')?;
        let port = rest
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return Some((format!("[{host}]"), port));
    }
    match hostport.split_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((hostport, default_port)),
    }
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

/// SQLite projects are files, not services: the file must exist and be
/// readable (drizzle-kit's better-sqlite3 opens it read-only for `pull`).
fn check_sqlite_file(raw_path: &str) -> Result<String> {
    let trimmed = raw_path.trim();
    anyhow::ensure!(!trimmed.is_empty(), "SQLite database file path is empty");
    let path = expand_tilde(trimmed);
    anyhow::ensure!(
        path.exists(),
        "SQLite database file not found: {}",
        path.display()
    );
    anyhow::ensure!(
        path.is_file(),
        "SQLite path is not a regular file: {}",
        path.display()
    );
    std::fs::File::open(&path)
        .with_context(|| format!("SQLite file is not readable: {}", path.display()))?;
    Ok(format!("SQLite file {} readable", path.display()))
}

/// Extracts the host from a `libsql://` (or `https://`) Turso URL.
pub fn parse_libsql_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("libsql://")
        .or_else(|| trimmed.strip_prefix("https://"))?;
    let host = rest.split(['/', '?']).next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Turso: dial the libsql endpoint's TCP port to prove DNS + reachability.
fn check_turso(url: &str) -> Result<String> {
    let trimmed = url.trim();
    anyhow::ensure!(!trimmed.is_empty(), "Turso database URL is empty");
    let hostport = parse_libsql_host(trimmed)
        .ok_or_else(|| anyhow!("Turso URL must look like libsql://<host>"))?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse().context("Turso URL has an invalid port")?,
        ),
        None => (hostport, 443),
    };
    check_tcp(&host, port)
}

/// D1: validate identifiers, then probe the Cloudflare API with `curl` when
/// available so bad tokens surface before drizzle-kit runs.
fn check_d1(account_id: &str, database_id: &str, token: &str) -> Result<String> {
    let account_id = account_id.trim();
    let database_id = database_id.trim();
    anyhow::ensure!(!account_id.is_empty(), "Cloudflare account ID is empty");
    anyhow::ensure!(!database_id.is_empty(), "Cloudflare database ID is empty");

    let Ok(curl) = which::which("curl") else {
        return Ok(
            "Cloudflare credentials present (curl unavailable - skipped live probe)".to_string(),
        );
    };

    let url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{account_id}/d1/database/{database_id}"
    );
    let output = Command::new(curl)
        .args(["-sS", "-m", "8"])
        .arg("-H")
        .arg(format!("Authorization: Bearer {token}"))
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .context("Failed to run curl for the Cloudflare D1 probe")?;

    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = match text.rsplit_once('\n') {
        Some((b, code)) => (b, code.trim().to_string()),
        None => (text.as_ref(), String::new()),
    };
    if status == "200" && body.contains("\"success\":true") {
        Ok(format!(
            "D1 database '{database_id}' reachable via Cloudflare API"
        ))
    } else if status == "403" {
        Err(anyhow!(
            "Cloudflare API rejected the token (HTTP 403); check the API token and its permissions"
        ))
    } else if status == "404" {
        Err(anyhow!(
            "D1 database or account not found (HTTP 404); check the IDs"
        ))
    } else if status.is_empty() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(detail.is_empty(), "Cloudflare API probe failed: {detail}");
        Err(anyhow!("Cloudflare API probe failed"))
    } else {
        Err(anyhow!("Cloudflare API returned HTTP {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_pass_host_port() {
        assert_eq!(
            parse_db_url_host_port("postgresql://alice:s3cret@db.example.com:6543/app", 5432),
            Some(("db.example.com".to_string(), 6543))
        );
    }

    #[test]
    fn falls_back_to_default_port_when_missing() {
        assert_eq!(
            parse_db_url_host_port("postgresql://db.example.com/app", 5432),
            Some(("db.example.com".to_string(), 5432))
        );
        assert_eq!(
            parse_db_url_host_port("mysql://root@localhost/db", 3306),
            Some(("localhost".to_string(), 3306))
        );
    }

    #[test]
    fn parses_explicit_port_without_user() {
        assert_eq!(
            parse_db_url_host_port("postgresql://alice@db.example.com:5432/app", 9999),
            Some(("db.example.com".to_string(), 5432))
        );
    }

    #[test]
    fn parses_ipv6_literal_with_default() {
        assert_eq!(
            parse_db_url_host_port("postgresql://alice@[2001:db8::1]:5433/app", 5432),
            Some(("[2001:db8::1]".to_string(), 5433))
        );
        assert_eq!(
            parse_db_url_host_port("postgresql://[2001:db8::1]/app", 5432),
            Some(("[2001:db8::1]".to_string(), 5432))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_db_url_host_port("not-a-url", 5432), None);
        assert_eq!(parse_db_url_host_port("", 5432), None);
    }

    #[test]
    fn rejects_invalid_port_even_with_default() {
        assert_eq!(
            parse_db_url_host_port("postgresql://h:notaport/db", 5432),
            None
        );
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

    #[test]
    fn libsql_hosts_parse_and_reject_others() {
        assert_eq!(
            parse_libsql_host("libsql://acme.turso.io"),
            Some("acme.turso.io".to_string())
        );
        assert_eq!(
            parse_libsql_host("libsql://acme.turso.io/path?x=1"),
            Some("acme.turso.io".to_string())
        );
        assert_eq!(
            parse_libsql_host("https://fallback.example.com"),
            Some("fallback.example.com".to_string())
        );
        assert_eq!(parse_libsql_host("libsql://"), None);
        assert_eq!(parse_libsql_host("http://plain.io"), None);
    }

    #[test]
    fn turso_check_validates_shape_before_dialing() {
        let err = check_turso("").unwrap_err().to_string();
        assert!(err.contains("URL is empty"));

        let err = check_turso("postgres://nope").unwrap_err().to_string();
        assert!(err.contains("libsql://"));
    }

    #[test]
    fn sqlite_check_requires_existing_readable_file() {
        assert!(check_sqlite_file("").is_err());
        assert!(check_sqlite_file("/nonexistent/dir/x.db").is_err());

        let dir = std::env::temp_dir().join(format!("pg-studio-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A directory is not a file.
        let err = check_sqlite_file(dir.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a regular file"));

        let db = dir.join("t.db");
        std::fs::write(&db, b"not really a database but readable").unwrap();
        let msg = check_sqlite_file(db.to_str().unwrap()).unwrap();
        assert!(msg.contains("readable"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn d1_check_validates_ids_before_any_network_call() {
        let err = check_d1("", "dbid", "tok").unwrap_err().to_string();
        assert!(err.contains("account ID is empty"));
        let err = check_d1("acct", "", "tok").unwrap_err().to_string();
        assert!(err.contains("database ID is empty"));
    }

    #[test]
    fn direct_target_uses_engine_default_ports() {
        use crate::config::{ConnectionType as CT, Engine, ProjectConfig};
        let mk = |engine: Engine| ProjectConfig {
            name: "p".into(),
            engine,
            connection_type: CT::Url,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: "db.internal".into(),
            db_port: String::new(),
            db_name: "app".into(),
            db_user: String::new(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
            last_opened: 0,
        };
        assert_eq!(
            direct_target(&mk(Engine::Postgres)),
            ("db.internal".into(), 5432)
        );
        assert_eq!(
            direct_target(&mk(Engine::Mysql)),
            ("db.internal".into(), 3306)
        );

        // URL targets win over the fields and default the port per engine.
        let mut mysql = mk(Engine::Mysql);
        mysql.db_url = "mysql://root@gateway.internal/prod".into();
        assert_eq!(direct_target(&mysql), ("gateway.internal".into(), 3306));
    }
}
