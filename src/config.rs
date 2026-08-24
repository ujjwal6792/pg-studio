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

/// Which database engine/provider a project talks to. `ConnectionType` stays
/// the *transport* (SSH tunnel vs direct vs local socket host) and only applies
/// to the wire-protocol engines (`Postgres`, `Mysql`); file/API-backed engines
/// ignore it.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Postgres,
    Sqlite,
    D1,
    Turso,
    Mysql,
}

impl Engine {
    pub const ALL: [Engine; 5] = [
        Engine::Postgres,
        Engine::Sqlite,
        Engine::D1,
        Engine::Turso,
        Engine::Mysql,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
            Self::D1 => "d1",
            Self::Turso => "turso",
            Self::Mysql => "mysql",
        }
    }

    /// Human-facing label used in lists and forms.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::D1 => "Cloudflare D1",
            Self::Turso => "Turso",
            Self::Mysql => "MySQL",
        }
    }

    /// Engines that speak a wire protocol over TCP and therefore support SSH
    /// tunnels and host/port style connections.
    pub fn is_wire(&self) -> bool {
        matches!(self, Self::Postgres | Self::Mysql)
    }

    pub fn default_port(&self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            _ => 0,
        }
    }

    pub fn next(&self) -> Self {
        let idx = Self::ALL.iter().position(|e| e == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

impl std::str::FromStr for Engine {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(Self::Postgres),
            "sqlite" | "sqlite3" => Ok(Self::Sqlite),
            "d1" | "d1-http" | "cloudflare" => Ok(Self::D1),
            "turso" | "libsql" => Ok(Self::Turso),
            "mysql" | "mariadb" => Ok(Self::Mysql),
            other => Err(anyhow!(
                "unknown engine '{other}' (use postgres, sqlite, d1, turso or mysql)"
            )),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    #[default]
    Ssh,
    Url,
    Local,
}

impl std::str::FromStr for ConnectionType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ssh" => Ok(Self::Ssh),
            "url" => Ok(Self::Url),
            "local" => Ok(Self::Local),
            other => Err(anyhow!(
                "unknown connection type '{other}' (use ssh, url or local)"
            )),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default)]
    pub engine: Engine,
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
    /// SQLite engine only: filesystem path of the database file.
    #[serde(default)]
    pub db_path: String,
    /// D1 engine only (non-secret identifiers).
    #[serde(default)]
    pub cf_account_id: String,
    #[serde(default)]
    pub cf_database_id: String,
    #[serde(default)]
    pub last_opened: i64,
}

impl ProjectConfig {
    /// Fallback project name used when the user leaves it blank, matching
    /// the TUI editor: `dbname@host` for wire engines and engine-specific
    /// equivalents for the file/API-backed ones.
    pub fn derived_name(&self) -> String {
        match self.engine {
            Engine::Sqlite => {
                let stem = std::path::Path::new(&self.db_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sqlite");
                format!("{stem}@sqlite")
            }
            Engine::D1 => format!(
                "{}@d1",
                if self.cf_database_id.trim().is_empty() {
                    "database"
                } else {
                    self.cf_database_id.trim()
                }
            ),
            Engine::Turso => {
                let host = self
                    .db_url
                    .split_once("://")
                    .map(|(_, rest)| rest.split(['/', '?']).next().unwrap_or(rest))
                    .unwrap_or("")
                    .trim();
                if host.is_empty() {
                    "turso@turso".to_string()
                } else {
                    format!("{host}@turso")
                }
            }
            Engine::Postgres | Engine::Mysql => {
                let host = match self.connection_type {
                    ConnectionType::Ssh => self.ssh_connection.clone(),
                    _ => {
                        if self.db_host.trim().is_empty() {
                            self.db_name.clone()
                        } else {
                            self.db_host.trim().to_string()
                        }
                    }
                };
                format!("{}@{host}", self.db_name)
            }
        }
    }

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
    /// Postgres engine only; other engines use [`ProjectConfig::resolve_target`].
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

    /// Builds a `mysql://...` connection URL (mirrors `connection_url`).
    fn mysql_url(&self, tunnel_port: Option<u16>) -> Result<String> {
        match self.connection_type {
            ConnectionType::Ssh => {
                let port =
                    tunnel_port.ok_or_else(|| anyhow!("SSH tunnel local port is not available"))?;
                let pass = self.get_password().unwrap_or_default();
                Ok(format!(
                    "mysql://{}:{}@127.0.0.1:{}/{}",
                    self.db_user, pass, port, self.db_name
                ))
            }
            _ => {
                if self.connection_type == ConnectionType::Url && !self.db_url.is_empty() {
                    return Ok(inject_password(
                        &self.db_url,
                        &self.get_password().unwrap_or_default(),
                    ));
                }
                let pass = self.get_password().unwrap_or_default();
                let host = if self.db_host.trim().is_empty() {
                    "127.0.0.1".to_string()
                } else {
                    self.db_host.trim().to_string()
                };
                let port = if self.db_port.trim().is_empty() {
                    "3306".to_string()
                } else {
                    self.db_port.trim().to_string()
                };
                Ok(format!(
                    "mysql://{}:{}@{}:{}/{}",
                    self.db_user, pass, host, port, self.db_name
                ))
            }
        }
    }

    /// Resolves the concrete launch target for this project. `tunnel_port` is
    /// required for wire engines when the connection type is `Ssh`.
    pub fn resolve_target(&self, tunnel_port: Option<u16>) -> Result<DbTarget> {
        match self.engine {
            Engine::Postgres => Ok(DbTarget::Postgres {
                url: self.connection_url(tunnel_port)?,
            }),
            Engine::Mysql => Ok(DbTarget::Mysql {
                url: self.mysql_url(tunnel_port)?,
            }),
            Engine::Sqlite => {
                let raw = self.db_path.trim();
                anyhow::ensure!(!raw.is_empty(), "SQLite database file path is empty");
                Ok(DbTarget::Sqlite {
                    path: expand_tilde(raw),
                })
            }
            Engine::D1 => {
                let account_id = self.cf_account_id.trim();
                anyhow::ensure!(!account_id.is_empty(), "Cloudflare account ID is empty");
                let database_id = self.cf_database_id.trim();
                anyhow::ensure!(!database_id.is_empty(), "Cloudflare database ID is empty");
                // NOTE: keychain is only consulted once the non-secret fields
                // validated so tests never touch the OS keyring.
                Ok(DbTarget::D1 {
                    account_id: account_id.to_string(),
                    database_id: database_id.to_string(),
                    token: self.get_password().unwrap_or_default(),
                })
            }
            Engine::Turso => {
                let url = self.db_url.trim();
                anyhow::ensure!(!url.is_empty(), "Turso database URL is empty");
                Ok(DbTarget::Turso {
                    url: url.to_string(),
                    auth_token: self.get_password().unwrap_or_default(),
                })
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

/// A fully resolved, engine-specific launch target. This is the single value
/// every downstream stage (drizzle config generation, env injection,
/// reachability checks) consumes instead of raw connection strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbTarget {
    Postgres {
        url: String,
    },
    Mysql {
        url: String,
    },
    Sqlite {
        path: PathBuf,
    },
    D1 {
        account_id: String,
        database_id: String,
        token: String,
    },
    Turso {
        url: String,
        auth_token: String,
    },
}

impl DbTarget {
    pub fn engine(&self) -> Engine {
        match self {
            Self::Postgres { .. } => Engine::Postgres,
            Self::Mysql { .. } => Engine::Mysql,
            Self::Sqlite { .. } => Engine::Sqlite,
            Self::D1 { .. } => Engine::D1,
            Self::Turso { .. } => Engine::Turso,
        }
    }

    /// Environment variables handed to every drizzle-kit invocation so secrets
    /// stay out of the generated config files on disk.
    pub fn env_vars(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Postgres { url } | Self::Mysql { url } => {
                vec![("DATABASE_URL", url.clone())]
            }
            // better-sqlite3 accepts a plain filesystem path as `url`.
            Self::Sqlite { path } => vec![("DATABASE_URL", path.to_string_lossy().to_string())],
            Self::D1 {
                account_id,
                database_id,
                token,
            } => vec![
                ("CLOUDFLARE_ACCOUNT_ID", account_id.clone()),
                ("CLOUDFLARE_DATABASE_ID", database_id.clone()),
                ("CLOUDFLARE_D1_TOKEN", token.clone()),
            ],
            Self::Turso { url, auth_token } => vec![
                ("DATABASE_URL", url.clone()),
                ("TURSO_AUTH_TOKEN", auth_token.clone()),
            ],
        }
    }
}

/// Expands a leading `~` to the user's home directory.
pub(crate) fn expand_tilde(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if trimmed == "~" {
        if let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()) {
            return home;
        }
    } else if let Some(rest) = trimmed.strip_prefix("~/")
        && let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
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
            .sort_by_key(|p| std::cmp::Reverse(p.last_opened));

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
            engine: Engine::Postgres,
            connection_type: ConnectionType::Local,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: "localhost".into(),
            db_port: "5432".into(),
            db_name: "postgres".into(),
            db_user: "postgres".into(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
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
            engine: Engine::Postgres,
            connection_type: ConnectionType::Url,
            ssh_connection: String::new(),
            db_url: "postgresql://alice@db.example.com:5432/app".into(),
            db_host: String::new(),
            db_port: "5432".into(),
            db_name: "app".into(),
            db_user: "alice".into(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
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
            engine: Engine::Postgres,
            connection_type: ConnectionType::Ssh,
            ssh_connection: "user@host".into(),
            db_url: String::new(),
            db_host: String::new(),
            db_port: "5432".into(),
            db_name: "db".into(),
            db_user: "user".into(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
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

#[cfg(test)]
mod engine_tests {
    use super::*;

    #[test]
    fn legacy_config_without_engine_defaults_to_postgres() {
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
        assert_eq!(parsed.projects[0].engine, Engine::Postgres);
    }

    #[test]
    fn engine_toml_round_trips_for_every_variant() {
        for engine in Engine::ALL {
            let project = ProjectConfig {
                name: "p".into(),
                engine,
                connection_type: ConnectionType::Local,
                ssh_connection: String::new(),
                db_url: String::new(),
                db_host: String::new(),
                db_port: String::new(),
                db_name: String::new(),
                db_user: String::new(),
                db_path: String::new(),
                cf_account_id: String::new(),
                cf_database_id: String::new(),
                last_opened: 0,
            };
            let toml_str = toml::to_string(&AppConfig::with_projects(vec![project])).unwrap();
            assert!(toml_str.contains(&format!("engine = \"{}\"", engine.as_str())));
            let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(parsed.projects[0].engine, engine);
        }
    }

    #[test]
    fn engine_parses_aliases() {
        assert_eq!("postgresql".parse::<Engine>().unwrap(), Engine::Postgres);
        assert_eq!("pg".parse::<Engine>().unwrap(), Engine::Postgres);
        assert_eq!("libsql".parse::<Engine>().unwrap(), Engine::Turso);
        assert_eq!("d1-http".parse::<Engine>().unwrap(), Engine::D1);
        assert!("redis".parse::<Engine>().is_err());
    }

    fn project_with(engine: Engine) -> ProjectConfig {
        ProjectConfig {
            name: "p".into(),
            engine,
            connection_type: ConnectionType::Local,
            ssh_connection: String::new(),
            db_url: "libsql://acme.turso.io".into(),
            db_host: String::new(),
            db_port: String::new(),
            db_name: String::new(),
            db_user: String::new(),
            db_path: "~/data/app.db".into(),
            cf_account_id: "acct".into(),
            cf_database_id: "dbid".into(),
            last_opened: 0,
        }
    }

    #[test]
    fn resolve_target_sqlite_expands_home() {
        let proj = project_with(Engine::Sqlite);
        match proj.resolve_target(None).unwrap() {
            DbTarget::Sqlite { path } => {
                assert!(path.is_absolute(), "expected absolute, got {path:?}");
                assert!(path.ends_with("app.db"));
            }
            other => panic!("unexpected target {other:?}"),
        }
    }

    #[test]
    fn resolve_target_rejects_empty_fields_before_keychain() {
        let mut proj = project_with(Engine::D1);
        proj.cf_account_id = String::new();
        assert!(proj.resolve_target(None).is_err());

        let mut proj = project_with(Engine::Turso);
        proj.db_url = String::new();
        assert!(proj.resolve_target(None).is_err());

        let mut proj = project_with(Engine::Sqlite);
        proj.db_path = String::new();
        assert!(proj.resolve_target(None).is_err());
    }

    #[test]
    fn target_env_vars_carry_secrets_without_touching_disk() {
        let d1 = DbTarget::D1 {
            account_id: "a".into(),
            database_id: "d".into(),
            token: "t".into(),
        };
        let vars = d1.env_vars();
        assert!(vars.contains(&("CLOUDFLARE_D1_TOKEN", "t".to_string())));

        let pg = DbTarget::Postgres {
            url: "postgresql://u:p@h/db".into(),
        };
        assert_eq!(
            pg.env_vars(),
            vec![("DATABASE_URL", "postgresql://u:p@h/db".to_string())]
        );
    }

    #[test]
    fn derived_name_per_engine() {
        let mut proj = project_with(Engine::Sqlite);
        assert_eq!(proj.derived_name(), "app@sqlite");

        proj.engine = Engine::D1;
        assert_eq!(proj.derived_name(), "dbid@d1");
        proj.cf_database_id = String::new();
        assert_eq!(proj.derived_name(), "database@d1");

        proj = project_with(Engine::Turso);
        assert_eq!(proj.derived_name(), "acme.turso.io@turso");

        proj.engine = Engine::Mysql;
        proj.connection_type = ConnectionType::Local;
        proj.db_name = "shop".into();
        // Empty host falls back to the database name (preserved legacy behavior).
        assert_eq!(proj.derived_name(), "shop@shop");
        proj.db_host = "db.internal".into();
        assert_eq!(proj.derived_name(), "shop@db.internal");
    }
}
