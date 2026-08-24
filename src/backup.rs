use crate::config::{ProjectBundle, ProjectConfig};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
/// Directory backups are "downloaded" to by default: the user's Downloads
/// folder, falling back to their home directory.
pub fn download_dir() -> Result<PathBuf> {
    if let Some(user) = directories::UserDirs::new()
        && let Some(dl) = user.download_dir()
    {
        return Ok(dl.to_path_buf());
    }
    let base = directories::BaseDirs::new().context("Could not determine home directory")?;
    Ok(base.home_dir().to_path_buf())
}

pub fn default_backup_path() -> Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    Ok(download_dir()?.join(format!("pg-studio-backup-{stamp}.json")))
}

/// Writes a password-free bundle of all projects. Refuses to clobber an
/// existing file so a backup can never destroy another one by accident.
pub fn write_backup(path: &Path, projects: Vec<ProjectConfig>) -> Result<()> {
    if path.exists() {
        bail!("Refusing to overwrite existing file: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {:?}", parent))?;
    }
    let bundle = ProjectBundle::new(projects);
    fs::write(path, serde_json::to_string_pretty(&bundle)?)
        .with_context(|| format!("Failed to write backup to {:?}", path))?;
    Ok(())
}

pub fn read_backup(path: &Path) -> Result<ProjectBundle> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read backup file {:?}", path))?;
    let bundle: ProjectBundle = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse backup file {:?}", path))?;
    if bundle.version != 1 {
        anyhow::bail!(
            "Unsupported backup version {} (expected 1) in {:?}",
            bundle.version,
            path
        );
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pg-studio-test-{tag}-{stamp}.json"))
    }

    fn sample_projects() -> Vec<ProjectConfig> {
        vec![ProjectConfig {
            name: "test-proj".into(),
            engine: crate::config::Engine::Postgres,
            connection_type: crate::config::ConnectionType::Local,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: "localhost".into(),
            db_port: "5432".into(),
            db_name: "postgres".into(),
            db_user: "postgres".into(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
            last_opened: 7,
        }]
    }

    #[test]
    fn backup_round_trips_through_disk() {
        let path = temp_path("roundtrip");
        write_backup(&path, sample_projects()).expect("write");
        let bundle = read_backup(&path).expect("read");
        assert_eq!(bundle.version, 1);
        assert_eq!(bundle.projects.len(), 1);
        assert_eq!(bundle.projects[0].name, "test-proj");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn backup_refuses_to_overwrite() {
        let path = temp_path("overwrite");
        write_backup(&path, sample_projects()).expect("first write");
        let err = write_backup(&path, sample_projects());
        assert!(err.is_err(), "second write must fail");
        let _ = fs::remove_file(&path);
    }
}
