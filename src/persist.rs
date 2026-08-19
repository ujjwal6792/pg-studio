use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedSession {
    pub project_name: String,
    pub studio_port: u16,
    pub studio_pid: u32,
    pub studio_pgid: u32,
    pub ssh_pid: Option<u32>,
    pub tunnel_url: String,
    pub log_path: String,
}

pub fn sessions_path() -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "dbstudio", "pg-studio")?;
    Some(proj_dirs.data_dir().join("sessions.json"))
}

pub fn load() -> Vec<PersistedSession> {
    let Some(path) = sessions_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(sessions: &[PersistedSession]) -> Result<()> {
    let Some(path) = sessions_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create data directory")?;
    }
    let json = serde_json::to_string_pretty(sessions)?;
    std::fs::write(&path, json).context("Failed to write sessions.json")?;
    Ok(())
}
