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

/// Locates a PostgreSQL client binary: PATH first, then known Homebrew
/// keg-only prefixes for the libpq formula (which is never symlinked into
/// PATH by default).
fn find_tool(name: &str) -> Option<PathBuf> {
    if which::which(name).is_ok() {
        return Some(PathBuf::from(name));
    }
    find_in(&[
        PathBuf::from(format!("/opt/homebrew/opt/libpq/bin/{name}")),
        PathBuf::from(format!("/usr/local/opt/libpq/bin/{name}")),
    ])
}

pub fn find_pg_dump() -> Option<PathBuf> {
    find_tool("pg_dump")
}

pub fn find_pg_restore() -> Option<PathBuf> {
    find_tool("pg_restore")
}

pub fn find_psql() -> Option<PathBuf> {
    find_tool("psql")
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

/// A background pg_dump/pg_restore job shown in the Process tab.
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
    /// Terminates the running pg_dump/pg_restore/psql process, if any.
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
            spawn_and_wait(args, &password, &pid_sink, &log_line)?;
            verify_dump_output(&out)?;
            let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
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
            spawn_and_wait(args, &password, &pid_sink, &log_line)?;
            verify_dump_output(&out)?;
            let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            Ok((size, SshTunnelGuard(None)))
        }
    }
}

/// Guards against a silent empty dump (e.g. a dropped connection).
fn verify_dump_output(out: &Path) -> Result<()> {
    if fs::metadata(out).map(|m| m.len()).unwrap_or(0) == 0 {
        bail!("pg_dump produced an empty file");
    }
    Ok(())
}

/// Keeps an SSH tunnel alive for the duration of a dump.
pub struct SshTunnelGuard(Option<SshTunnel>);

impl SshTunnelGuard {
    fn port(&self) -> u16 {
        self.0.as_ref().expect("guard holds tunnel").local_port
    }
}

/// Tool that performs a restore for a given backup file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTool {
    /// `.dump` custom-format archives, restored with pg_restore.
    PgRestore,
    /// `.sql` plain scripts, replayed with psql.
    Psql,
}

impl RestoreTool {
    pub fn binary_name(self) -> &'static str {
        match self {
            RestoreTool::PgRestore => "pg_restore",
            RestoreTool::Psql => "psql",
        }
    }

    pub fn find_binary(self) -> Option<PathBuf> {
        match self {
            RestoreTool::PgRestore => find_pg_restore(),
            RestoreTool::Psql => find_psql(),
        }
    }

    /// Safety dumps use the same format as the restore so both can be
    /// replayed with the same tooling.
    pub fn safety_dump_format(self) -> DumpFormat {
        match self {
            RestoreTool::PgRestore => DumpFormat::Custom,
            RestoreTool::Psql => DumpFormat::Plain,
        }
    }
}

/// Picks the restore tool purely from the file extension (.sql = psql).
pub fn restore_tool_for(path: &Path) -> RestoreTool {
    match DumpFormat::from_path(path) {
        DumpFormat::Plain => RestoreTool::Psql,
        DumpFormat::Custom => RestoreTool::PgRestore,
    }
}

/// Pure argument builder for `pg_restore`. The password travels via the
/// PGPASSWORD environment variable, never argv.
pub fn build_pg_restore_args(
    bin: &str,
    host: &str,
    port: u16,
    user: &str,
    dbname: &str,
    file: &Path,
) -> Vec<String> {
    [
        bin.to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
        "--username".to_string(),
        user.to_string(),
        // Drop existing objects so the backup fully replaces the schema/data.
        "--clean".to_string(),
        "--if-exists".to_string(),
        "--no-owner".to_string(),
        "--no-privileges".to_string(),
        // Fail loudly instead of leaving a half-restored database behind.
        "--exit-on-error".to_string(),
        "--dbname".to_string(),
        dbname.to_string(),
        file.display().to_string(),
    ]
    .to_vec()
}

/// Pure argument builder for replaying plain SQL via `psql`.
pub fn build_psql_args(
    bin: &str,
    host: &str,
    port: u16,
    user: &str,
    dbname: &str,
    file: &Path,
) -> Vec<String> {
    [
        bin.to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
        "--username".to_string(),
        user.to_string(),
        "--dbname".to_string(),
        dbname.to_string(),
        "--set".to_string(),
        "ON_ERROR_STOP=1".to_string(),
        "--file".to_string(),
        file.display().to_string(),
    ]
    .to_vec()
}

pub fn build_restore_args(tool: RestoreTool, bin: &str, target: &RestoreTarget) -> Vec<String> {
    match tool {
        RestoreTool::PgRestore => build_pg_restore_args(
            bin,
            &target.host,
            target.port,
            &target.user,
            &target.dbname,
            &target.file,
        ),
        RestoreTool::Psql => build_psql_args(
            bin,
            &target.host,
            target.port,
            &target.user,
            &target.dbname,
            &target.file,
        ),
    }
}

/// Where a restore writes to: everything needed to dial the database.
#[derive(Debug, Clone)]
pub struct RestoreTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dbname: String,
    pub file: PathBuf,
}

/// Path of the mandatory safety dump taken before a restore. Written next to
/// the backup being restored as `<stem>.pre-restore-<timestamp>.<ext>` so it
/// is easy to find and never silently overwrites another file (the caller
/// must refuse to proceed if the path already exists).
pub fn safety_backup_path(backup_file: &Path) -> Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let stem = backup_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("backup");
    let ext = restore_tool_for(backup_file)
        .safety_dump_format()
        .default_extension();
    let dir = backup_file.parent().unwrap_or_else(|| Path::new("."));
    Ok(dir.join(format!("{stem}.pre-restore-{stamp}.{ext}")))
}

/// Newest `.dump`/`.sql` file in `dir`, skipping pre-restore safety backups;
/// used to pre-fill the TUI restore path field.
pub fn latest_db_backup_in(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains(".pre-restore-") {
            continue;
        }
        let is_backup = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("dump") | Some("sql")
        );
        if !is_backup {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Convenience wrapper scanning the default downloads directory.
pub fn latest_db_backup_file() -> Option<PathBuf> {
    latest_db_backup_in(&crate::backup::download_dir().ok()?)
}

/// Restores `backup_file` into `proj`'s database. For SSH projects an SSH
/// tunnel is established first and torn down on return. The child pid is
/// published through `pid_sink` right after spawn so the caller can cancel.
///
/// Callers MUST take a safety dump first ([`run_dump`] with
/// [`RestoreTool::safety_dump_format`]) - this function alone destroys data.
pub fn run_restore(
    proj: &ProjectConfig,
    backup_file: &Path,
    pid_sink: Arc<Mutex<Option<u32>>>,
    log_line: LogFn,
) -> Result<SshTunnelGuard> {
    let tool = restore_tool_for(backup_file);
    let bin = tool.find_binary().ok_or_else(|| {
        anyhow::anyhow!(
            "{} not found on PATH or in known install locations. \
             Install the PostgreSQL client tools (e.g. brew install libpq, \
             apt install postgresql-client, pacman -S postgresql-libs).",
            tool.binary_name()
        )
    })?;
    if !backup_file.is_file() {
        bail!("Backup file not found: {}", backup_file.display());
    }
    if proj.db_name.trim().is_empty() {
        bail!("Project has no database name configured");
    }
    let password = proj.get_password().unwrap_or_default();

    match proj.connection_type {
        ConnectionType::Ssh => {
            let tunnel = establish_tunnel(&proj.ssh_connection, &proj.db_port)?;
            let guard = SshTunnelGuard(Some(tunnel));
            let args = build_restore_args(
                tool,
                bin.to_string_lossy().as_ref(),
                &RestoreTarget {
                    host: "127.0.0.1".into(),
                    port: guard.port(),
                    user: proj.db_user.clone(),
                    dbname: proj.db_name.clone(),
                    file: backup_file.to_path_buf(),
                },
            );
            spawn_and_wait(args, &password, &pid_sink, &log_line)?;
            Ok(guard)
        }
        ConnectionType::Url | ConnectionType::Local => {
            let (host, port) = direct_target(proj);
            let args = build_restore_args(
                tool,
                bin.to_string_lossy().as_ref(),
                &RestoreTarget {
                    host,
                    port,
                    user: proj.db_user.clone(),
                    dbname: proj.db_name.clone(),
                    file: backup_file.to_path_buf(),
                },
            );
            spawn_and_wait(args, &password, &pid_sink, &log_line)?;
            Ok(SshTunnelGuard(None))
        }
    }
}

fn direct_target(proj: &ProjectConfig) -> (String, u16) {
    crate::check::direct_target(proj)
}

fn spawn_and_wait(
    args: Vec<String>,
    password: &str,
    pid_sink: &Arc<Mutex<Option<u32>>>,
    log_line: &LogFn,
) -> Result<()> {
    let mut child: Child = Command::new(&args[0])
        .args(&args[1..])
        .env("PGPASSWORD", password)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn {}", args[0]))?;

    if let Ok(mut slot) = pid_sink.lock() {
        *slot = Some(child.id());
    }

    // Drain stderr concurrently so the pipe never back-pressures the child.
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

    let status = child.wait().context("child process wait failed")?;
    if !status.success() {
        bail!("{} exited with {status}", args[0]);
    }
    Ok(())
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
mod restore_tests {
    use super::*;

    #[test]
    fn pg_restore_args_clean_and_fail_fast() {
        let args = build_pg_restore_args(
            "pg_restore",
            "127.0.0.1",
            5433,
            "alice",
            "app",
            Path::new("/tmp/x.dump"),
        );
        let expected = [
            "pg_restore",
            "--host",
            "127.0.0.1",
            "--port",
            "5433",
            "--username",
            "alice",
            "--clean",
            "--if-exists",
            "--no-owner",
            "--no-privileges",
            "--exit-on-error",
            "--dbname",
            "app",
            "/tmp/x.dump",
        ];
        assert_eq!(
            args,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn psql_args_stop_on_error() {
        let args = build_psql_args(
            "psql",
            "localhost",
            5432,
            "bob",
            "mydb",
            Path::new("/tmp/x.sql"),
        );
        let expected = [
            "psql",
            "--host",
            "localhost",
            "--port",
            "5432",
            "--username",
            "bob",
            "--dbname",
            "mydb",
            "--set",
            "ON_ERROR_STOP=1",
            "--file",
            "/tmp/x.sql",
        ];
        assert_eq!(
            args,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn restore_tool_follows_extension() {
        assert_eq!(
            restore_tool_for(Path::new("/tmp/a.dump")),
            RestoreTool::PgRestore
        );
        assert_eq!(restore_tool_for(Path::new("/tmp/a.sql")), RestoreTool::Psql);
        assert_eq!(
            restore_tool_for(Path::new("/tmp/a")),
            RestoreTool::PgRestore
        );
        // Safety dumps are always replayable with the same tool.
        for tool in [RestoreTool::PgRestore, RestoreTool::Psql] {
            let fmt = tool.safety_dump_format();
            assert_eq!(
                restore_tool_for(Path::new(&format!("x.{}", fmt.default_extension()))),
                tool
            );
        }
    }

    #[test]
    fn safety_path_sits_next_to_backup_with_timestamp() {
        let path = safety_backup_path(Path::new("/backups/app.dump")).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(path.parent().unwrap() == Path::new("/backups"));
        assert!(name.starts_with("app.pre-restore-"), "{name}");
        assert!(name.ends_with(".dump"), "{name}");

        let sql = safety_backup_path(Path::new("/backups/app.sql")).unwrap();
        assert!(sql.file_name().unwrap().to_str().unwrap().ends_with(".sql"));
    }

    #[test]
    fn latest_backup_picks_newest_and_skips_safety_files() {
        let dir = std::env::temp_dir().join(format!("pgs-latest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.dump");
        let new = dir.join("new.dump");
        let safety = dir.join("new.pre-restore-20300101-000000.dump");
        let noise = dir.join("notes.txt");
        fs::write(&old, b"o").unwrap();
        fs::write(&new, b"n").unwrap();
        fs::write(&safety, b"s").unwrap();
        fs::write(&noise, b"x").unwrap();

        let file_times = [
            (&old, 1_000_000_000),
            (&new, 2_000_000_000),
            (&safety, 3_000_000_000),
            (&noise, 4_000_000_000),
        ];
        for (path, secs) in file_times {
            use std::fs::FileTimes;
            let f = fs::File::options().append(true).open(path).unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            f.set_times(FileTimes::new().set_modified(t)).unwrap();
        }

        assert_eq!(latest_db_backup_in(&dir), Some(new));
        let _ = fs::remove_dir_all(dir);
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
