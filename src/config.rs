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
    Local,
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
            ConnectionType::Local => {
                let pass = self.get_password().unwrap_or_default();
                let host = if self.db_host.trim().is_empty() {
                    "localhost".to_string()
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
    #[cfg(test)]
    pub(crate) fn with_projects(projects: Vec<ProjectConfig>) -> Self {
        Self { projects }
    }
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

    /// Copies the existing config file to `config.toml.bak` so a corrupt file
    /// is never silently destroyed by a later save. Returns `Ok(None)` when
    /// there is no config file to back up.
    pub fn backup_corrupt_config() -> Result<Option<PathBuf>> {
        let path = Self::config_file_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let backup = path.with_extension("toml.bak");
        fs::copy(&path, &backup)
            .with_context(|| format!("Failed to back up config file to {:?}", backup))?;
        Ok(Some(backup))
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

    /// Path of the portable project bundle written by `x` / read by `i`.
    /// Passwords never appear here: they stay in the OS keychain.
    /// Merges a bundle's projects into this config, skipping names that
    /// already exist. Returns `(imported, skipped)`. Does NOT save.
    pub fn merge_bundle(&mut self, bundle: &ProjectBundle) -> (usize, usize) {
        let mut imported = 0;
        let mut skipped = 0;
        for project in &bundle.projects {
            if self.projects.iter().any(|p| p.name == project.name) {
                skipped += 1;
                continue;
            }
            self.projects.push(project.clone());
            imported += 1;
        }
        (imported, skipped)
    }

    pub fn export_file_path() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from("com", "dbstudio", "pg-studio")
            .context("Could not determine project directories")?;
        Ok(proj_dirs.config_dir().join("projects-export.json"))
    }
}

/// Portable bundle of project definitions (no secrets).
#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectBundle {
    pub version: u32,
    pub exported_at: i64,
    pub projects: Vec<ProjectConfig>,
}

impl ProjectBundle {
    pub fn new(projects: Vec<ProjectConfig>) -> Self {
        Self {
            version: 1,
            exported_at: chrono::Utc::now().timestamp(),
            projects,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_password_extracts_and_strips_password() {
        let url = "postgresql://alice:s3cret@db.example.com:5432/app";
        let (redacted, pass) = ProjectConfig::redact_url_password(url);
        assert_eq!(pass.as_deref(), Some("s3cret"));
        assert_eq!(redacted, "postgresql://alice@db.example.com:5432/app");
        assert!(!redacted.contains("s3cret"));
    }

    #[test]
    fn redact_url_password_leaves_url_without_password_alone() {
        for url in [
            "postgresql://alice@db.example.com:5432/app",
            "postgresql://db.example.com:5432/app",
            "not-a-url",
            "",
        ] {
            let (redacted, pass) = ProjectConfig::redact_url_password(url);
            assert_eq!(pass, None);
            assert_eq!(redacted, url);
        }
    }

    #[test]
    fn inject_password_adds_missing_password() {
        let out = inject_password("postgresql://alice@db.example.com:5432/app", "pw");
        assert_eq!(out, "postgresql://alice:pw@db.example.com:5432/app");
    }

    #[test]
    fn inject_password_keeps_existing_password() {
        let url = "postgresql://alice:existing@db.example.com:5432/app";
        assert_eq!(inject_password(url, "pw"), url);
    }

    #[test]
    fn inject_password_ignores_empty_or_unusable_urls() {
        assert_eq!(
            inject_password("postgresql://alice@host/db", ""),
            "postgresql://alice@host/db"
        );
        assert_eq!(inject_password("no-scheme-no-at", "pw"), "no-scheme-no-at");
        assert_eq!(
            inject_password("postgresql://host:5432/db", "pw"),
            "postgresql://host:5432/db"
        );
    }

    #[test]
    fn config_toml_round_trips_local_connection_type() {
        let project = ProjectConfig {
            name: "local-dev".into(),
            connection_type: ConnectionType::Local,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: "localhost".into(),
            db_port: "5432".into(),
            db_name: "postgres".into(),
            db_user: "postgres".into(),
            last_opened: 0,
        };
        let toml_str = toml::to_string(&AppConfig::with_projects(vec![project])).unwrap();
        assert!(toml_str.contains("connection_type = \"local\""));
        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.projects[0].connection_type, ConnectionType::Local);
    }

    #[test]
    fn legacy_config_without_connection_type_defaults_to_ssh() {
        let legacy = r#"
[[projects]]
name = "old-project"
ssh_connection = "ubuntu@10.0.0.1"
db_port = "5432"
db_name = "app"
db_user = "admin"
last_opened = 123
"#;
        let parsed: AppConfig = toml::from_str(legacy).unwrap();
        assert_eq!(parsed.projects.len(), 1);
        assert_eq!(parsed.projects[0].connection_type, ConnectionType::Ssh);
    }

    #[test]
    fn backup_path_replaces_extension() {
        let path = std::path::PathBuf::from("/data/pg-studio/config.toml");
        assert_eq!(
            path.with_extension("toml.bak"),
            std::path::PathBuf::from("/data/pg-studio/config.toml.bak")
        );
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    #[test]
    fn project_bundle_round_trips_without_passwords() {
        let bundle = ProjectBundle::new(vec![ProjectConfig {
            name: "prod".into(),
            connection_type: ConnectionType::Url,
            ssh_connection: String::new(),
            db_url: "postgresql://alice@db.example.com:5432/app".into(),
            db_host: String::new(),
            db_port: "5432".into(),
            db_name: "app".into(),
            db_user: "alice".into(),
            last_opened: 42,
        }]);
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(!json.contains("password"));
        let parsed: ProjectBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.projects.len(), 1);
        assert_eq!(parsed.projects[0].name, "prod");
        assert_eq!(parsed.version, 1);
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn project(name: &str) -> ProjectConfig {
        ProjectConfig {
            name: name.into(),
            connection_type: ConnectionType::Ssh,
            ssh_connection: "user@host".into(),
            db_url: String::new(),
            db_host: String::new(),
            db_port: "5432".into(),
            db_name: "db".into(),
            db_user: "user".into(),
            last_opened: 0,
        }
    }

    #[test]
    fn merge_imports_new_and_skips_existing() {
        let mut config = AppConfig::with_projects(vec![project("existing")]);
        let bundle = ProjectBundle::new(vec![project("existing"), project("fresh")]);
        let (imported, skipped) = config.merge_bundle(&bundle);
        assert_eq!((imported, skipped), (1, 1));
        assert_eq!(config.projects.len(), 2);
        assert!(config.projects.iter().any(|p| p.name == "fresh"));
    }
}
