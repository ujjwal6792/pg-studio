use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub ssh_connection: Option<String>,
    pub db_port: Option<String>,
    pub db_name: Option<String>,
    pub db_user: Option<String>,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_file_path()?;
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config file at {:?}", config_path))?;
            let config: AppConfig =
                toml::from_str(&content).with_context(|| "Failed to parse config file as TOML")?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_file_path()?;
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory at {:?}", parent))?;
        }
        let content =
            toml::to_string(self).with_context(|| "Failed to serialize config to TOML")?;
        fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file to {:?}", config_path))?;
        Ok(())
    }

    pub fn config_file_path() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from("com", "dbstudio", "pg-studio")
            .context("Could not determine project directories")?;
        Ok(proj_dirs.config_dir().join("config.toml"))
    }
}
