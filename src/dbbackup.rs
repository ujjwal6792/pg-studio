use crate::config::{ConnectionType, ProjectConfig};
use crate::ssh::{SshTunnel, establish_tunnel};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Shared stderr-line sink that can be cloned into reader threads.
pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpFormat {
    /// Compressed Postgres custom format; restores with pg_restore.
    Custom,
    /// Plain SQL text.
    Plain,
}

impl DumpFormat {
    /// Format inferred from the chosen file extension (.sql = plain).
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("sql") => DumpFormat::Plain,
            _ => DumpFormat::Custom,
        }
    }

    pub fn default_extension(self) -> &'static str {
        match self {
            DumpFormat::Custom => "dump",
            DumpFormat::Plain => "sql",
        }
    }
}

/// Human-readable byte size, e.g. "4.2 MiB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Locates pg_dump: PATH first, then known Homebrew keg-only prefixes for
/// the libpq formula (which is never symlinked into PATH by default).
pub fn find_pg_dump() -> Option<PathBuf> {
    if which::which("pg_dump").is_ok() {
        return Some(PathBuf::from("pg_dump"));
    }
    find_in(&[
        PathBuf::from("/opt/homebrew/opt/libpq/bin/pg_dump"),
        PathBuf::from("/usr/local/opt/libpq/bin/pg_dump"),
    ])
}

fn find_in(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

/// Default dump destination: ~/Downloads/<name>-<timestamp>.<ext>
pub fn default_dump_path(project_name: &str, format: DumpFormat) -> Result<PathBuf> {
    let dir = crate::backup::download_dir()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let safe: String = project_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(dir.join(format!(
        "{safe}-{stamp}.{ext}",
        ext = format.default_extension()
    )))
}

/// Pure argument builder (unit-testable). The password never appears here -
/// it travels via the PGPASSWORD environment variable. `pg_dump_bin` allows
/// an absolute path for tools that are not on PATH (e.g. Homebrew libpq).
pub fn build_dump_args(
    pg_dump_bin: &str,
    host: &str,
    port: u16,
    user: &str,
    dbname: &str,
    out: &Path,
    format: DumpFormat,
) -> Vec<String> {
    let mut args = vec![pg_dump_bin.to_string()];
    args.extend([
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
        "--username".to_string(),
        user.to_string(),
        "--no-owner".to_string(),
        "--no-privileges".to_string(),
        "--file".to_string(),
        out.display().to_string(),
    ]);
    match format {
        DumpFormat::Custom => args.push("--format=custom".to_string()),
        DumpFormat::Plain => args.push("--format=plain".to_string()),
    }
    args.push(dbname.to_string());
    args
}

/// A background pg_dump job shown in the Process tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

pub struct Job {
    pub label: String,
    pub status: JobStatus,
    pub started_at: i64,
    /// Destination path while running; "path · size" once done.
    pub detail: String,
    pub error: Option<String>,
    /// pg_dump pid used for cancellation.
    pub pid: Arc<Mutex<Option<u32>>>,
}

impl Job {
    /// Terminates the pg_dump process, if still running.
    pub fn cancel(&self) {
        let pid = self.pid.lock().ok().and_then(|mut slot| slot.take());
        if let Some(pid) = pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    /// Human-readable elapsed time since the job started.
    pub fn elapsed(&self) -> String {
        let secs = (chrono::Utc::now().timestamp() - self.started_at).max(0);
        let m = secs / 60;
        let s = secs % 60;
        if m > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{s}s")
        }
    }
}

/// Runs a full dump of `proj` to `out`. For SSH projects an SSH tunnel is
/// established first and torn down on return. The pg_dump pid is published
/// through `pid_sink` right after spawn so the caller can cancel. Returns
/// the size of the written file.
///
/// `log_line` receives pg_dump's stderr lines (password-redacted).
pub fn run_dump(
    proj: &ProjectConfig,
    out: PathBuf,
    format: DumpFormat,
    pid_sink: Arc<Mutex<Option<u32>>>,
    log_line: LogFn,
) -> Result<(u64, SshTunnelGuard)> {
    let pg_dump = find_pg_dump().ok_or_else(|| {
        anyhow::anyhow!(
            "pg_dump not found on PATH or in known install locations. \
             Install the PostgreSQL client tools (e.g. brew install libpq, \
             apt install postgresql-client, pacman -S postgresql-libs)."
        )
    })?;
    if proj.db_name.trim().is_empty() {
        bail!("Project has no database name configured");
    }
    let password = proj.get_password().unwrap_or_default();

    match proj.connection_type {
        ConnectionType::Ssh => {
            let tunnel = establish_tunnel(&proj.ssh_connection, &proj.db_port)?;
            let guard = SshTunnelGuard(Some(tunnel));
            let local_port = guard.port();
            let args = build_dump_args(
                pg_dump.to_string_lossy().as_ref(),
                "127.0.0.1",
                local_port,
                &proj.db_user,
                &proj.db_name,
                &out,
                format,
            );
            let size = spawn_and_wait(args, &password, &out, &pid_sink, &log_line)?;
            Ok((size, guard))
        }
        ConnectionType::Url | ConnectionType::Local => {
            let (host, port) = direct_target(proj);
            let args = build_dump_args(
                pg_dump.to_string_lossy().as_ref(),
                &host,
                port,
                &proj.db_user,
                &proj.db_name,
                &out,
                format,
            );
            let size = spawn_and_wait(args, &password, &out, &pid_sink, &log_line)?;
            Ok((size, SshTunnelGuard(None)))
        }
    }
}

/// Keeps an SSH tunnel alive for the duration of a dump.
pub struct SshTunnelGuard(Option<SshTunnel>);

impl SshTunnelGuard {
    fn port(&self) -> u16 {
        self.0.as_ref().expect("guard holds tunnel").local_port
    }
}

fn direct_target(proj: &ProjectConfig) -> (String, u16) {
    crate::check::direct_target(proj)
}

fn spawn_and_wait(
    args: Vec<String>,
    password: &str,
    out: &Path,
    pid_sink: &Arc<Mutex<Option<u32>>>,
    log_line: &LogFn,
) -> Result<u64> {
    let mut child: Child = Command::new(&args[0])
        .args(&args[1..])
        .env("PGPASSWORD", password)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn pg_dump")?;

    if let Ok(mut slot) = pid_sink.lock() {
        *slot = Some(child.id());
    }

    // Drain stderr concurrently so the pipe never back-pressures pg_dump.
    if let Some(stderr) = child.stderr.take() {
        let redact = password.to_string();
        let log_line = log_line.clone();
        std::thread::spawn(move || {
            for line in BufRead::lines(std::io::BufReader::new(stderr)).map_while(Result::ok) {
                if line.contains(&redact) && !redact.is_empty() {
                    log_line(line.replace(&redact, "***"));
                } else {
                    log_line(line);
                }
            }
        });
    }

    let status = child.wait().context("pg_dump wait failed")?;
    if !status.success() {
        bail!("pg_dump exited with {status} - see Process tab output");
    }
    let size = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        bail!("pg_dump produced an empty file");
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_args_cover_connection_target_and_format() {
        let out = Path::new("/tmp/x.dump");
        let args = build_dump_args(
            "pg_dump",
            "127.0.0.1",
            5433,
            "alice",
            "app",
            out,
            DumpFormat::Custom,
        );
        let expected = [
            "pg_dump",
            "--host",
            "127.0.0.1",
            "--port",
            "5433",
            "--username",
            "alice",
            "--no-owner",
            "--no-privileges",
            "--file",
            "/tmp/x.dump",
            "--format=custom",
            "app",
        ];
        assert_eq!(
            args,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        // The password must never travel through argv.
        assert!(!args.join(" ").contains("PGPASSWORD"));
    }

    #[test]
    fn format_from_path_uses_extension() {
        assert_eq!(
            DumpFormat::from_path(Path::new("/tmp/a.sql")),
            DumpFormat::Plain
        );
        assert_eq!(
            DumpFormat::from_path(Path::new("/tmp/b.dump")),
            DumpFormat::Custom
        );
        assert_eq!(
            DumpFormat::from_path(Path::new("/tmp/noext")),
            DumpFormat::Custom
        );
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }
}

#[cfg(test)]
mod find_tests {
    use super::*;

    #[test]
    fn finds_pg_dump_in_known_locations() {
        let dir = std::env::temp_dir().join(format!("pgs-find-{}", std::process::id()));
        let bin = dir.join("bin").join("pg_dump");
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        assert_eq!(find_in(&[dir.join("bin").join("pg_dump")]), Some(bin));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn misses_when_absent() {
        assert_eq!(
            find_in(&[PathBuf::from("/nonexistent/pg-studio/pg_dump")]),
            None
        );
    }
}
