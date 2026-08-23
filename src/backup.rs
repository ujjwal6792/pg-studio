use crate::config::{ProjectBundle, ProjectConfig};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
/// Directory backups are "downloaded" to by default: the user's Downloads
/// folder, falling back to their home directory.
fn download_dir() -> Result<PathBuf> {
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
