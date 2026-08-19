use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    #[default]
    Ssh,
    Url,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default)]
    pub connection_type: ConnectionType,
    #[serde(default)]
    pub ssh_connection: String,
    #[serde(default)]
    pub db_url: String,
    #[serde(default)]
    pub db_host: String,
    #[serde(default)]
    pub db_port: String,
    #[serde(default)]
    pub db_name: String,
    #[serde(default)]
    pub db_user: String,
    #[serde(default)]
    pub last_opened: i64,
}

impl ProjectConfig {
    pub fn save_password(&self, password: &str) -> Result<()> {
        let entry = Entry::new("pg-studio", &self.name)?;
        entry.set_password(password)?;
        Ok(())
    }

    pub fn get_password(&self) -> Result<String> {
        let entry = Entry::new("pg-studio", &self.name)?;
        Ok(entry.get_password()?)
    }

    /// Resolves the final `postgresql://...` connection URL for this project.
    /// `tunnel_port` is required when the connection type is `Ssh`.
    pub fn connection_url(&self, tunnel_port: Option<u16>) -> Result<String> {
        match self.connection_type {
            ConnectionType::Ssh => {
                let port =
                    tunnel_port.ok_or_else(|| anyhow!("SSH tunnel local port is not available"))?;
                let pass = self.get_password().unwrap_or_default();
                Ok(format!(
                    "postgresql://{}:{}@127.0.0.1:{}/{}",
                    self.db_user, pass, port, self.db_name
                ))
            }
            ConnectionType::Url => {
                if !self.db_url.is_empty() {
                    Ok(inject_password(
                        &self.db_url,
                        &self.get_password().unwrap_or_default(),
                    ))
                } else {
                    let pass = self.get_password().unwrap_or_default();
                    let host = if self.db_host.trim().is_empty() {
                        "127.0.0.1".to_string()
                    } else {
                        self.db_host.trim().to_string()
                    };
                    let port = if self.db_port.trim().is_empty() {
                        "5432".to_string()
                    } else {
                        self.db_port.trim().to_string()
                    };
                    Ok(format!(
                        "postgresql://{}:{}@{}:{}/{}",
                        self.db_user, pass, host, port, self.db_name
                    ))
                }
            }
        }
    }

    /// Extracts an embedded `:password@` from a connection URL so it can be stored
    /// in the OS keychain instead of plaintext. Returns `(redacted_url, password)`.
    pub fn redact_url_password(url: &str) -> (String, Option<String>) {
        let Some(scheme_end) = url.find("://") else {
            return (url.to_string(), None);
        };
        let authority_start = scheme_end + 3;
        let rest = &url[authority_start..];
        let Some(at_rel) = rest.find('@') else {
            return (url.to_string(), None);
        };
        let userinfo = &rest[..at_rel];
        let Some(colon_rel) = userinfo.find(':') else {
            return (url.to_string(), None);
        };
        let password = userinfo[colon_rel + 1..].to_string();
        let user = &userinfo[..colon_rel];
        let mut redacted = url.to_string();
        redacted.replace_range(authority_start..authority_start + at_rel, user);
        (redacted, Some(password))
    }
}

fn inject_password(url: &str, password: &str) -> String {
    if password.is_empty() {
        return url.to_string();
    }
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];
    let Some(at_rel) = rest.find('@') else {
        return url.to_string();
    };
    let userinfo = &rest[..at_rel];
    if userinfo.contains(':') {
        // Password already embedded.
        return url.to_string();
    }
    let mut out = url.to_string();
    out.insert_str(authority_start + at_rel, &format!(":{}", password));
    out
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

    pub fn save(&mut self) -> Result<()> {
        // Sort projects by last opened descending before saving
        self.projects
            .sort_by(|a, b| b.last_opened.cmp(&a.last_opened));

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
