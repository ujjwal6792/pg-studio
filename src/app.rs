use crate::check;
use crate::config::{AppConfig, ConnectionType, Engine, ProjectBundle, ProjectConfig};
use crate::dbbackup::{self, DumpFormat, Job, JobStatus};
use crate::drizzle::{check_dependencies, extract_tunnel_url, prepare_workspace, spawn_studio};
use crate::open::{copy_to_clipboard, open_url};
use crate::persist::{self, PersistedSession};
use crate::session::{RunningSession, SessionStatus};
use crate::ssh::{establish_tunnel, find_free_port};
use crate::theme::Theme;
use anyhow::{Result, anyhow};
use chrono::{Local, TimeZone};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tui_input::Input;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ActivePane {
    ProjectsList,
    Details,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DetailsTab {
    Overview,
    Config,
    Process,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ProjectState {
    Running,
    Stopped,
    Error,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AppMode {
    Normal,
    EditingForm,
    ConfirmDialog,
    Help,
    /// Live-filtering the projects list by name ('/').
    Filtering,
    /// Backup / restore / dump menu ('b').
    BackupMenu,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ConfirmationAction {
    DeleteProject,
    CancelEdit,
    Quit,
    StopProject,
    /// pg_dump is missing; Enter opens a package-manager installer in an
    /// external terminal (manager stored in `pending_pkg_manager`).
    MissingPgDump,
    /// Overwrite the selected project's database from
    /// `pending_restore_file`; a safety dump is always taken first.
    RestoreDatabase,
}

/// What pressing Enter on a highlighted keybinding entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpAction {
    Quit,
    FocusProjects,
    FocusDetails,
    FlipPane,
    TabsForward,
    TabsBack,
    MoveUp,
    MoveDown,
    NewProject,
    EditProject,
    DuplicateProject,
    DeleteProject,
    Filter,
    TestConn,
    BackupMenu,
    StopProject,
    OpenUrl,
    CopyUrl,
    CopyLogs,
    Connect,
    RunNoBrowser,
    ExportProjects,
    ImportProjects,
    UpdateApp,
    ScrollLogsUp,
    ScrollLogsDown,
    CloseHelp,
}

/// One row of the keybindings screen; headers are group titles.
pub struct HelpEntry {
    pub header: Option<&'static str>,
    pub key: &'static str,
    pub desc: &'static str,
    pub action: Option<HelpAction>,
}

pub fn help_entries() -> Vec<HelpEntry> {
    use HelpAction::*;
    let e = |key: &'static str, desc: &'static str, action: Option<HelpAction>| HelpEntry {
        header: None,
        key,
        desc,
        action,
    };
    let g = |header: &'static str| HelpEntry {
        header: Some(header),
        key: "",
        desc: "",
        action: None,
    };
    vec![
        g("Navigation"),
        e("←", "Focus Projects list", Some(FocusProjects)),
        e("→", "Focus Details pane", Some(FocusDetails)),
        e(
            "[ or ]",
            "Flip focus between Projects and Details",
            Some(FlipPane),
        ),
        e("Tab", "Cycle Details sub-tabs forwards", Some(TabsForward)),
        e(
            "Shift+Tab",
            "Cycle Details sub-tabs backwards",
            Some(TabsBack),
        ),
        e("↑/k", "Move selection in the projects list", Some(MoveUp)),
        e("↓/j", "Move selection in the projects list", Some(MoveDown)),
        g("Projects"),
        e("n", "New project", Some(NewProject)),
        e("e", "Edit selected project", Some(EditProject)),
        e(
            "p",
            "Duplicate selected project into the editor",
            Some(DuplicateProject),
        ),
        e("d", "Delete selected project", Some(DeleteProject)),
        e("/", "Filter projects by name (Esc clears)", Some(Filter)),
        e(
            "t",
            "Test connection reachability without launching",
            Some(TestConn),
        ),
        e(
            "Enter",
            "Focus details pane; press again to launch + open browser when ready",
            Some(Connect),
        ),
        e(
            "Shift+Enter / r",
            "Focus details pane; press again to launch without opening browser",
            Some(RunNoBrowser),
        ),
        e(
            "b",
            "Backup menu: app backup, restore, DB dump & restore",
            Some(BackupMenu),
        ),
        e(
            "s",
            "Stop selected project's studio and dumps",
            Some(StopProject),
        ),
        g("Editing"),
        e(
            "Enter / Space",
            "In the editor: cycle Connection Type / save project",
            None,
        ),
        e("Tab / ↓", "Next field (editor)", None),
        e("Shift+Tab / ↑", "Previous field (editor)", None),
        e(
            "Paste",
            "A full postgres:// URL auto-fills all fields",
            None,
        ),
        e("Esc", "Cancel edit (asks to confirm)", None),
        g("Studio"),
        e(
            "o",
            "Open the running studio URL in your browser",
            Some(OpenUrl),
        ),
        e(
            "c",
            "Copy the running studio URL to the clipboard",
            Some(CopyUrl),
        ),
        e(
            "l",
            "Copy the selected project's Drizzle Studio logs",
            Some(CopyLogs),
        ),
        e(
            "URL",
            "Studio URL is https://local.drizzle.studio?port=<port>",
            None,
        ),
        g("Global"),
        e(
            "PgUp / PgDn",
            "Scroll the Logs pane (click it to re-follow)",
            Some(ScrollLogsUp),
        ),
        e(
            "Mouse",
            "Click panes/tabs/rows, wheel scrolls lists and logs",
            None,
        ),
        e(
            "x",
            "Export all projects to a portable JSON file (no secrets)",
            Some(ExportProjects),
        ),
        e(
            "i",
            "Import projects from that file (skips existing names)",
            Some(ImportProjects),
        ),
        e("u", "Self-update pg-studio", Some(UpdateApp)),
        e(
            "q",
            "Quit (running studios keep running in background)",
            Some(Quit),
        ),
        e("?", "Toggle this keybindings screen", Some(CloseHelp)),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Engine,
    ConnectionType,
    SshConnection,
    DbUrl,
    DbHost,
    DbPort,
    DbName,
    DbUser,
    DbPass,
    DbPath,
    CfAccountId,
    CfDatabaseId,
}

impl FormField {
    /// Field navigation order for the form, driven by the selected engine
    /// (and, for wire engines, by the connection type).
    fn order(engine: Engine, ct: ConnectionType) -> &'static [FormField] {
        match engine {
            Engine::Sqlite => &[FormField::Name, FormField::Engine, FormField::DbPath],
            Engine::D1 => &[
                FormField::Name,
                FormField::Engine,
                FormField::CfAccountId,
                FormField::CfDatabaseId,
                FormField::DbPass,
            ],
            Engine::Turso => &[
                FormField::Name,
                FormField::Engine,
                FormField::DbUrl,
                FormField::DbPass,
            ],
            Engine::Postgres | Engine::Mysql => Self::wire_order(ct),
        }
    }

    fn wire_order(ct: ConnectionType) -> &'static [FormField] {
        match ct {
            ConnectionType::Ssh => &[
                FormField::Name,
                FormField::Engine,
                FormField::ConnectionType,
                FormField::SshConnection,
                FormField::DbPort,
                FormField::DbName,
                FormField::DbUser,
                FormField::DbPass,
            ],
            ConnectionType::Url => &[
                FormField::Name,
                FormField::Engine,
                FormField::ConnectionType,
                FormField::DbUrl,
                FormField::DbHost,
                FormField::DbPort,
                FormField::DbName,
                FormField::DbUser,
                FormField::DbPass,
            ],
            ConnectionType::Local => &[
                FormField::Name,
                FormField::Engine,
                FormField::ConnectionType,
                FormField::DbHost,
                FormField::DbPort,
                FormField::DbName,
                FormField::DbUser,
                FormField::DbPass,
            ],
        }
    }

    pub fn next(&self, engine: Engine, ct: ConnectionType) -> Self {
        let order = Self::order(engine, ct);
        let idx = order.iter().position(|f| f == self).unwrap_or(0);
        order[(idx + 1) % order.len()]
    }

    pub fn prev(&self, engine: Engine, ct: ConnectionType) -> Self {
        let order = Self::order(engine, ct);
        let idx = order.iter().position(|f| f == self).unwrap_or(0);
        order[(idx + order.len() - 1) % order.len()]
    }

    pub fn get_help(&self) -> (&'static str, &'static str) {
        match self {
            FormField::Name => (
                "Unique identifier for this project. If left blank, defaults to database_name@host.",
                "Examples: production-us-east, staging-db, app_db@ubuntu@192.168.1.5",
            ),
            FormField::Engine => (
                "Which database engine this project talks to. Each engine has its own fields; secrets always go to your keychain.",
                "Press Enter or Space to cycle PostgreSQL -> SQLite -> Cloudflare D1 -> Turso -> MySQL.",
            ),
            FormField::ConnectionType => (
                "How to reach the database: SSH tunnels through a remote server, URL connects directly, Local talks to a database on this machine.",
                "Press Enter or Space to cycle SSH -> URL -> Local. Choosing Local auto-fills the host with localhost.",
            ),
            FormField::SshConnection => (
                "SSH connection string to reach the remote server hosting the database.",
                "Examples: ubuntu@13.233.0.0, root@ec2.compute.amazonaws.com, admin@my-server.com",
            ),
            FormField::DbUrl => (
                "Full public connection string: postgresql://... for Postgres, mysql://... for MySQL, libsql://... for Turso.",
                "Examples: libsql://acme.turso.io - embedded passwords are moved to your keychain on save.",
            ),
            FormField::DbHost => (
                "Host for a direct or local connection (used only if no full Connection URL is provided).",
                "Examples: localhost (locally running database), 127.0.0.1, db.your-cluster.us-east-1.cockroachlabs.cloud",
            ),
            FormField::DbPort => (
                "The remote port the database is listening on. Leave blank to default to 5432 (Postgres) or 3306 (MySQL).",
                "Examples: 5432, 5433, 3306, 6432, 26257",
            ),
            FormField::DbName => (
                "Name of the target database.",
                "Examples: postgres, production_main, app_db_v2, defaultdb",
            ),
            FormField::DbUser => (
                "Database user with read/introspection permissions.",
                "Examples: postgres, db_admin, readonly_user, root",
            ),
            FormField::DbPass => (
                "Secret for the selected engine: user password (Postgres/MySQL/SSH), Cloudflare API token (D1) or Turso auth token. Securely saved in your OS Keychain (only fetched when launching).",
                "Input is masked with asterisks (*)",
            ),
            FormField::DbPath => (
                "Path of the local SQLite database file. Tilde (~) expands to your home directory; a wrangler dev D1 file works too (.wrangler/state/v3/d1/**).",
                "Examples: ~/data/app.db, ./local.sqlite3, myproject/.wrangler/state/v3/d1/miniflare-D1DatabaseObject/<hash>.sqlite",
            ),
            FormField::CfAccountId => (
                "Cloudflare account ID that owns the D1 database (found on any account page in the dashboard).",
                "Example: 023e105f4ecef8ad9ca31a8372d0c353",
            ),
            FormField::CfDatabaseId => (
                "D1 database ID (Dashboard > Storage & Databases > D1 > your database).",
                "Example: 54f4e105-2f3c-4a5d-8b6e-1c2d3e4f5a6b",
            ),
        }
    }
}

/// Shared state for an in-flight self-update so the TUI keeps responding,
/// shows live progress, and can cancel at phase boundaries.
pub struct UpdateTracker {
    pub phase: Arc<Mutex<crate::updater::UpdatePhase>>,
    pub note: Arc<Mutex<String>>,
    pub cancel: Arc<AtomicBool>,
    pub started_at: Instant,
    pub finished: Arc<Mutex<Option<Result<crate::updater::UpdateOutcome, String>>>>,
}

pub struct App {
    pub config: AppConfig,
    pub selected_project_idx: usize,
    pub active_pane: ActivePane,
    pub details_tab: DetailsTab,
    pub mode: AppMode,
    pub confirm_action: Option<ConfirmationAction>,

    // Projects list scrolling / filtering
    pub project_scroll: usize,
    pub filter: Input,
    /// Lines kept off the bottom of the Logs pane (0 = follow latest).
    pub log_scroll: usize,

    // Backup menu ('b')
    pub backup_menu_idx: usize,
    pub input_backup_path: Input,
    /// Package manager chosen for a pending pg_dump install confirmation.
    pub pending_pkg_manager: Option<crate::installer::PackageManager>,
    /// Backup file staged for a pending restore-into-database confirmation.
    pub pending_restore_file: Option<PathBuf>,

    /// In-flight self-update, if any (`u` / help screen).
    pub update_tracker: Option<Arc<UpdateTracker>>,

    // Keybindings screen ('?')
    pub help_selected: usize,
    pub help_scroll: usize,

    /// Background pg_dump jobs shown in the Process tab.
    pub jobs: Vec<Arc<Mutex<Job>>>,

    // Form inputs
    pub input_name: Input,
    pub input_ssh: Input,
    pub input_url: Input,
    pub input_host: Input,
    pub input_port: Input,
    pub input_dbname: Input,
    pub input_dbuser: Input,
    pub input_dbpass: Input,
    pub input_dbpath: Input,
    pub input_cf_account: Input,
    pub input_cf_database: Input,
    pub engine: Engine,
    pub connection_type: ConnectionType,
    pub active_field: FormField,

    pub is_new_project: bool,
    pub status_message: String,
    pub error_message: Option<String>,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub sessions: Vec<Arc<Mutex<RunningSession>>>,
    pub theme: Theme,
}

impl App {
    pub fn new() -> Result<Self> {
        // A corrupt config must never be silently discarded: back it up and
        // tell the user instead of starting empty and overwriting it on save.
        let (config, config_warning) = match AppConfig::load() {
            Ok(config) => (config, None),
            Err(e) => {
                let mut msg = format!(
                    "Config file could not be loaded, starting with an empty project list:\n{:#}",
                    e
                );
                match AppConfig::backup_corrupt_config() {
                    Ok(Some(backup)) => msg.push_str(&format!(
                        "\nThe original file was backed up to {}",
                        backup.display()
                    )),
                    Ok(None) => {}
                    Err(be) => msg.push_str(&format!(
                        "\nWARNING: could not back up the original file: {:#}",
                        be
                    )),
                }
                (AppConfig::default(), Some(msg))
            }
        };

        let mut config = config;
        config
            .projects
            .sort_by_key(|p| std::cmp::Reverse(p.last_opened));

        let mut app = Self {
            config,
            selected_project_idx: 0,
            active_pane: ActivePane::ProjectsList,
            details_tab: DetailsTab::Overview,
            mode: AppMode::Normal,
            confirm_action: None,

            project_scroll: 0,
            filter: Input::default(),
            log_scroll: 0,

            backup_menu_idx: 0,
            input_backup_path: Input::default(),
            pending_pkg_manager: None,
            pending_restore_file: None,
            update_tracker: None,

            help_selected: 0,
            help_scroll: 0,

            jobs: Vec::new(),

            input_name: Input::default(),
            input_ssh: Input::default(),
            input_url: Input::default(),
            input_host: Input::default(),
            input_port: Input::default(),
            input_dbname: Input::default(),
            input_dbuser: Input::default(),
            input_dbpass: Input::default(),
            input_dbpath: Input::default(),
            input_cf_account: Input::default(),
            input_cf_database: Input::default(),
            engine: Engine::Postgres,
            connection_type: ConnectionType::Ssh,
            active_field: FormField::Name,

            is_new_project: false,
            status_message: String::from(
                "Ready. Press 'n' for New Project, 'Enter' to focus a project (again to launch), '?' for help.",
            ),
            error_message: None,
            logs: Arc::new(Mutex::new(Vec::new())),
            sessions: Vec::new(),
            theme: Theme::default(),
        };

        app.load_selected_into_form();
        app.restore_sessions();
        if let Some(warning) = config_warning {
            app.status_message =
                "Config error: starting with an empty project list (see Logs).".to_string();
            app.add_log(warning);
        }
        Ok(app)
    }

    pub fn add_log(&self, msg: String) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(msg);
        }
    }

    /// Read-only access to the `Input` backing a form field.
    pub fn input(&self, field: FormField) -> Option<&Input> {
        match field {
            FormField::Name => Some(&self.input_name),
            FormField::SshConnection => Some(&self.input_ssh),
            FormField::DbUrl => Some(&self.input_url),
            FormField::DbHost => Some(&self.input_host),
            FormField::DbPort => Some(&self.input_port),
            FormField::DbName => Some(&self.input_dbname),
            FormField::DbUser => Some(&self.input_dbuser),
            FormField::DbPass => Some(&self.input_dbpass),
            FormField::DbPath => Some(&self.input_dbpath),
            FormField::CfAccountId => Some(&self.input_cf_account),
            FormField::CfDatabaseId => Some(&self.input_cf_database),
            FormField::Engine | FormField::ConnectionType => None,
        }
    }

    /// Mutable access to the `Input` backing a form field.
    pub fn input_mut(&mut self, field: FormField) -> Option<&mut Input> {
        match field {
            FormField::Name => Some(&mut self.input_name),
            FormField::SshConnection => Some(&mut self.input_ssh),
            FormField::DbUrl => Some(&mut self.input_url),
            FormField::DbHost => Some(&mut self.input_host),
            FormField::DbPort => Some(&mut self.input_port),
            FormField::DbName => Some(&mut self.input_dbname),
            FormField::DbUser => Some(&mut self.input_dbuser),
            FormField::DbPass => Some(&mut self.input_dbpass),
            FormField::DbPath => Some(&mut self.input_dbpath),
            FormField::CfAccountId => Some(&mut self.input_cf_account),
            FormField::CfDatabaseId => Some(&mut self.input_cf_database),
            FormField::Engine | FormField::ConnectionType => None,
        }
    }

    pub fn selected_project_name(&self) -> String {
        self.config
            .projects
            .get(self.selected_project_idx)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    pub fn formatted_last_opened(&self) -> String {
        if let Some(proj) = self.config.projects.get(self.selected_project_idx) {
            if proj.last_opened == 0 {
                return "Never".to_string();
            }
            if let Some(dt) = Local.timestamp_opt(proj.last_opened, 0).single() {
                return dt.format("%Y-%m-%d %H:%M:%S").to_string();
            }
        }
        "N/A".to_string()
    }

    /// Loads project metadata into form WITHOUT querying OS Keychain (prevents prompt on start/navigate)
    pub fn load_selected_into_form(&mut self) {
        self.error_message = None;
        if let Some(proj) = self.config.projects.get(self.selected_project_idx) {
            self.input_name = Input::from(proj.name.clone());
            self.engine = proj.engine;
            self.connection_type = proj.connection_type;
            self.input_ssh = Input::from(proj.ssh_connection.clone());
            self.input_url = Input::from(proj.db_url.clone());
            self.input_host = Input::from(proj.db_host.clone());
            let default_port = proj.engine.default_port().to_string();
            self.input_port = if proj.db_port == default_port {
                Input::default()
            } else {
                Input::from(proj.db_port.clone())
            };
            self.input_dbname = Input::from(proj.db_name.clone());
            self.input_dbuser = Input::from(proj.db_user.clone());
            self.input_dbpass = Input::default(); // DO NOT query keychain here!
            self.input_dbpath = Input::from(proj.db_path.clone());
            self.input_cf_account = Input::from(proj.cf_account_id.clone());
            self.input_cf_database = Input::from(proj.cf_database_id.clone());
            self.is_new_project = false;
        } else {
            self.reset_form();
            self.is_new_project = true;
        }
    }

    /// Fetches password from keychain ONLY when user explicitly enters edit mode
    pub fn prepare_edit_mode(&mut self) {
        if let Some(proj) = self.config.projects.get(self.selected_project_idx)
            && let Ok(pass) = proj.get_password()
        {
            self.input_dbpass = Input::from(pass);
        }
        self.active_pane = ActivePane::Details;
        self.details_tab = DetailsTab::Config;
        self.mode = AppMode::EditingForm;
    }

    /// Clones the selected project's settings into a fresh "… (copy)" draft
    /// and opens it in the editor. Nothing is persisted until the user saves.
    pub fn duplicate_selected_project(&mut self) {
        let Some(src) = self.config.projects.get(self.selected_project_idx).cloned() else {
            return;
        };

        let base = format!("{} (copy)", src.name);
        let mut name = base.clone();
        let mut counter = 2;
        while self.config.projects.iter().any(|p| p.name == name) {
            name = format!("{base} {counter}");
            counter += 1;
        }

        self.input_name = Input::from(name);
        self.engine = src.engine;
        self.connection_type = src.connection_type;
        self.input_ssh = Input::from(src.ssh_connection.clone());
        self.input_url = Input::from(src.db_url.clone());
        self.input_host = Input::from(src.db_host.clone());
        let default_port = src.engine.default_port().to_string();
        self.input_port = if src.db_port == default_port {
            Input::default()
        } else {
            Input::from(src.db_port.clone())
        };
        self.input_dbname = Input::from(src.db_name.clone());
        self.input_dbuser = Input::from(src.db_user.clone());
        self.input_dbpass = match src.get_password() {
            Ok(pass) => Input::from(pass),
            Err(_) => Input::default(),
        };
        self.input_dbpath = Input::from(src.db_path.clone());
        self.input_cf_account = Input::from(src.cf_account_id.clone());
        self.input_cf_database = Input::from(src.cf_database_id.clone());
        self.error_message = None;
        self.is_new_project = true;

        self.active_pane = ActivePane::Details;
        self.details_tab = DetailsTab::Config;
        self.mode = AppMode::EditingForm;
    }

    pub fn toggle_connection_type(&mut self) {
        self.connection_type = match self.connection_type {
            ConnectionType::Ssh => ConnectionType::Url,
            ConnectionType::Url => ConnectionType::Local,
            ConnectionType::Local => ConnectionType::Ssh,
        };
        if self.connection_type == ConnectionType::Local
            && self.input_host.value().trim().is_empty()
        {
            self.input_host = Input::from("localhost");
        }
        self.active_field = FormField::ConnectionType;
    }

    pub fn toggle_engine(&mut self) {
        self.engine = self.engine.next();
        // Reset navigation to a field that exists in every engine layout.
        if !FormField::order(self.engine, self.connection_type).contains(&self.active_field) {
            self.active_field = FormField::Engine;
        }
    }

    pub fn reset_form(&mut self) {
        self.error_message = None;
        self.input_name = Input::default();
        self.input_ssh = Input::default();
        self.input_url = Input::default();
        self.input_host = Input::default();
        self.input_port = Input::default();
        self.input_dbname = Input::default();
        self.input_dbuser = Input::default();
        self.input_dbpass = Input::default();
        self.input_dbpath = Input::default();
        self.input_cf_account = Input::default();
        self.input_cf_database = Input::default();
        self.engine = Engine::Postgres;
        self.connection_type = ConnectionType::Ssh;
        self.active_field = FormField::Name;
        self.is_new_project = true;
    }

    pub fn save_form_to_project(&mut self) -> Result<()> {
        let engine = self.engine;

        let (mut proj, secret) = match engine {
            Engine::Postgres | Engine::Mysql => self.build_wire_project(engine)?,
            Engine::Sqlite => self.build_sqlite_project()?,
            Engine::D1 => self.build_d1_project()?,
            Engine::Turso => self.build_turso_project()?,
        };

        if proj.name.is_empty() {
            proj.name = proj.derived_name();
        }
        let name = proj.name.clone();

        let existing_match = self.config.projects.iter().position(|p| p.name == name);
        if let Some(matching_idx) = existing_match
            && (self.is_new_project || matching_idx != self.selected_project_idx)
        {
            self.error_message = Some(format!("A project named '{name}' already exists!"));
            return Err(anyhow!("Project name must be unique"));
        }

        if let Some(secret) = secret
            && !secret.is_empty()
        {
            proj.save_password(&secret)?;
        }

        if self.is_new_project {
            self.config.projects.push(proj);
            self.selected_project_idx = self.config.projects.len() - 1;
        } else if let Some(existing) = self.config.projects.get_mut(self.selected_project_idx) {
            *existing = proj;
        }

        self.config.save()?;
        self.load_selected_into_form();
        self.status_message = format!("Project '{name}' saved successfully!");
        self.error_message = None;
        Ok(())
    }

    fn now_timestamp() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    /// Postgres/MySQL projects: full transport flexibility (SSH/URL/Local).
    fn build_wire_project(&self, engine: Engine) -> Result<(ProjectConfig, Option<String>)> {
        let ssh = self.input_ssh.value().to_string();
        let db_url_input = self.input_url.value().trim().to_string();
        let db_host = self.input_host.value().trim().to_string();
        let port = self.input_port.value().to_string();
        let dbname = self.input_dbname.value().trim().to_string();
        let dbuser = self.input_dbuser.value().trim().to_string();
        let dbpass = self.input_dbpass.value().to_string();

        match self.connection_type {
            ConnectionType::Ssh if ssh.trim().is_empty() => {
                return Err(anyhow!("SSH connection string is empty"));
            }
            ConnectionType::Url if db_url_input.is_empty() && db_host.is_empty() => {
                return Err(anyhow!("Provide a connection URL or a database host"));
            }
            _ => {}
        }

        let final_port = if port.trim().is_empty() {
            engine.default_port().to_string()
        } else {
            port.trim().to_string()
        };

        // In URL mode, pull any embedded password out of the URL into the keychain.
        let (db_url, extracted_pass) =
            if self.connection_type == ConnectionType::Url && !db_url_input.is_empty() {
                ProjectConfig::redact_url_password(&db_url_input)
            } else {
                (db_url_input, None)
            };

        Ok((
            ProjectConfig {
                name: self.input_name.value().trim().to_string(),
                engine,
                connection_type: self.connection_type,
                ssh_connection: ssh,
                db_url,
                db_host,
                db_port: final_port,
                db_name: dbname,
                db_user: dbuser,
                db_path: String::new(),
                cf_account_id: String::new(),
                cf_database_id: String::new(),
                last_opened: Self::now_timestamp(),
            },
            Some(extracted_pass.unwrap_or(dbpass)),
        ))
    }

    /// Local SQLite file: the path is the whole story.
    fn build_sqlite_project(&self) -> Result<(ProjectConfig, Option<String>)> {
        let db_path = self.input_dbpath.value().trim().to_string();
        anyhow::ensure!(!db_path.is_empty(), "SQLite database file path is empty");

        Ok((
            ProjectConfig {
                name: self.input_name.value().trim().to_string(),
                engine: Engine::Sqlite,
                connection_type: ConnectionType::Local,
                ssh_connection: String::new(),
                db_url: String::new(),
                db_host: String::new(),
                db_port: String::new(),
                db_name: String::new(),
                db_user: String::new(),
                db_path,
                cf_account_id: String::new(),
                cf_database_id: String::new(),
                last_opened: Self::now_timestamp(),
            },
            None,
        ))
    }

    /// Remote Cloudflare D1 over d1-http; the API token lives in the keychain.
    fn build_d1_project(&self) -> Result<(ProjectConfig, Option<String>)> {
        let account_id = self.input_cf_account.value().trim().to_string();
        let database_id = self.input_cf_database.value().trim().to_string();
        anyhow::ensure!(!account_id.is_empty(), "Cloudflare account ID is empty");
        anyhow::ensure!(!database_id.is_empty(), "Cloudflare database ID is empty");
        let token = self.input_dbpass.value().to_string();

        Ok((
            ProjectConfig {
                name: self.input_name.value().trim().to_string(),
                engine: Engine::D1,
                connection_type: ConnectionType::Url,
                ssh_connection: String::new(),
                db_url: String::new(),
                db_host: String::new(),
                db_port: String::new(),
                db_name: String::new(),
                db_user: String::new(),
                db_path: String::new(),
                cf_account_id: account_id,
                cf_database_id: database_id,
                last_opened: Self::now_timestamp(),
            },
            Some(token),
        ))
    }

    /// Turso (libsql) remote database; auth token lives in the keychain.
    fn build_turso_project(&self) -> Result<(ProjectConfig, Option<String>)> {
        let db_url = self.input_url.value().trim().to_string();
        anyhow::ensure!(!db_url.is_empty(), "Turso database URL is empty");
        let token = self.input_dbpass.value().to_string();

        Ok((
            ProjectConfig {
                name: self.input_name.value().trim().to_string(),
                engine: Engine::Turso,
                connection_type: ConnectionType::Url,
                ssh_connection: String::new(),
                db_url,
                db_host: String::new(),
                db_port: String::new(),
                db_name: String::new(),
                db_user: String::new(),
                db_path: String::new(),
                cf_account_id: String::new(),
                cf_database_id: String::new(),
                last_opened: Self::now_timestamp(),
            },
            Some(token),
        ))
    }

    /// Writes all project definitions (password-free) to the export bundle.
    pub fn export_projects(&self) -> Result<PathBuf> {
        let bundle = ProjectBundle::new(self.config.projects.clone());
        let path = AppConfig::export_file_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&bundle)?)?;
        Ok(path)
    }

    /// Merges projects from any backup bundle into the live config,
    /// skipping names that already exist.
    pub fn merge_bundle(&mut self, bundle: ProjectBundle) -> (usize, usize) {
        let (imported, skipped) = self.config.merge_bundle(&bundle);
        if imported > 0
            && let Err(e) = self.config.save()
        {
            self.add_log(format!("Failed to save imported projects: {:#}", e));
            return (0, skipped);
        }
        self.config
            .projects
            .sort_by_key(|p| std::cmp::Reverse(p.last_opened));
        self.load_selected_into_form();
        (imported, skipped)
    }

    /// Merges projects from the export bundle, skipping names that already
    /// exist. Returns `(imported, skipped)` and persists on success.
    pub fn import_projects(&mut self) -> Result<(usize, usize)> {
        let path = AppConfig::export_file_path()?;
        let bundle = crate::backup::read_backup(&path)?;
        let result = self.merge_bundle(bundle);
        if result.0 == 0 && result.1 == 0 {
            anyhow::bail!("No projects found in {:?}", path);
        }
        Ok(result)
    }

    // --- Keybindings screen ---

    /// Number of selectable (non-header) entries.
    pub fn help_selectable_count() -> usize {
        help_entries().iter().filter(|e| e.header.is_none()).count()
    }

    pub fn help_move(&mut self, delta: isize) {
        let count = Self::help_selectable_count();
        if count == 0 {
            return;
        }
        self.help_selected =
            (self.help_selected as isize + delta).clamp(0, count as isize - 1) as usize;
    }

    /// The action bound to the currently highlighted entry, if any.
    pub fn selected_help_action(&self) -> Option<HelpAction> {
        help_entries()
            .iter()
            .filter(|e| e.header.is_none())
            .nth(self.help_selected)
            .and_then(|e| e.action)
    }

    // --- Keybindings screen end ---

    // --- Backup menu ('b') ---
    pub fn backup_menu_items() -> &'static [&'static str] {
        &[
            "Download app backup",
            "Restore app backup from file",
            "Dump selected project DB (.dump custom / .sql plain)",
            "Restore DB backup into selected project",
        ]
    }

    pub fn open_backup_menu(&mut self) {
        self.backup_menu_idx = 0;
        self.input_backup_path = Input::from(
            crate::backup::default_backup_path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        self.mode = AppMode::BackupMenu;
    }

    /// Re-prefills the path input whenever the highlighted action changes.
    pub fn backup_menu_move(&mut self, delta: isize) {
        let len = Self::backup_menu_items().len();
        let current = self.backup_menu_idx as isize;
        self.backup_menu_idx = ((current + delta).rem_euclid(len as isize)) as usize;
        let default_path = match self.backup_menu_idx {
            0 => crate::backup::default_backup_path(),
            1 => AppConfig::export_file_path(),
            2 => {
                let name = self.selected_project_name();
                let format = DumpFormat::Custom;
                dbbackup::default_dump_path(&name, format)
            }
            _ => dbbackup::latest_db_backup_file()
                .map(Ok)
                .unwrap_or_else(|| {
                    crate::backup::download_dir().map(|dir| dir.join("backup.dump"))
                }),
        };
        self.input_backup_path = Input::from(
            default_path
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
    }

    pub fn execute_backup_action(&mut self) {
        let raw = self.input_backup_path.value().trim().to_string();
        if raw.is_empty() {
            self.add_log("Backup aborted: no path given.".to_string());
            return;
        }
        let path = PathBuf::from(raw);
        match self.backup_menu_idx {
            0 => match crate::backup::write_backup(&path, self.config.projects.clone()) {
                Ok(_) => {
                    self.add_log(format!(
                        "Backup written: {} ({} project(s), passwords not included)",
                        path.display(),
                        self.config.projects.len()
                    ));
                    self.mode = AppMode::Normal;
                }
                Err(e) => self.add_log(format!("Backup failed: {:#}", e)),
            },
            1 => match crate::backup::read_backup(&path) {
                Ok(bundle) => {
                    let total = bundle.projects.len();
                    let (imported, skipped) = self.merge_bundle(bundle);
                    self.add_log(format!(
                        "Restore from {}: {} imported, {skipped} skipped ({total} in file).",
                        path.display(),
                        imported
                    ));
                    self.mode = AppMode::Normal;
                }
                Err(e) => self.add_log(format!("Restore failed: {:#}", e)),
            },
            2 => {
                if self.config.projects.is_empty() {
                    self.add_log("Dump aborted: no project selected.".to_string());
                    return;
                }
                if !self.selected_project_is_postgres() {
                    self.add_log(
                        "Dump aborted: backups are only supported for PostgreSQL projects."
                            .to_string(),
                    );
                    return;
                }
                if dbbackup::find_pg_dump().is_none() {
                    self.offer_pg_tool_install();
                    return;
                }
                let proj = self.config.projects[self.selected_project_idx].clone();
                let format = DumpFormat::from_path(&path);
                self.spawn_dump_job(proj, path, format);
                self.mode = AppMode::Normal;
            }
            3 => {
                if self.config.projects.is_empty() {
                    self.add_log("Restore aborted: no project selected.".to_string());
                    return;
                }
                if !self.selected_project_is_postgres() {
                    self.add_log(
                        "Restore aborted: backups are only supported for PostgreSQL projects."
                            .to_string(),
                    );
                    return;
                }
                if !path.is_file() {
                    self.add_log(format!(
                        "Restore aborted: backup file not found: {}",
                        path.display()
                    ));
                    return;
                }
                // Both the safety dump and the restore itself need client tools.
                let tool = dbbackup::restore_tool_for(&path);
                let missing = [
                    dbbackup::find_pg_dump().is_none().then_some("pg_dump"),
                    tool.find_binary().is_none().then_some(tool.binary_name()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
                if !missing.is_empty() {
                    self.add_log(format!(
                        "Restore aborted: {missing} not found. Install the PostgreSQL client tools."
                    ));
                    self.add_log(
                        "  brew install libpq | sudo apt install postgresql-client | sudo pacman -S postgresql-libs"
                            .to_string(),
                    );
                    return;
                }
                self.pending_restore_file = Some(path.clone());
                self.confirm_action = Some(ConfirmationAction::RestoreDatabase);
                self.mode = AppMode::ConfirmDialog;
            }
            _ => {}
        }
    }

    /// pg_dump/pg_restore/psql only speak Postgres; gate the backup menu.
    fn selected_project_is_postgres(&self) -> bool {
        self.config
            .projects
            .get(self.selected_project_idx)
            .map(|p| p.engine == Engine::Postgres)
            .unwrap_or(false)
    }

    /// Offers the guided package-manager install when pg tools are missing.
    fn offer_pg_tool_install(&mut self) {
        match crate::installer::detect_package_manager() {
            Some(pm) => {
                self.pending_pkg_manager = Some(pm);
                self.confirm_action = Some(ConfirmationAction::MissingPgDump);
                self.mode = AppMode::ConfirmDialog;
            }
            None => {
                self.add_log(
                    "pg_dump not found and no supported package manager detected.".to_string(),
                );
                self.add_log("Install the PostgreSQL client tools manually, e.g.:".to_string());
                self.add_log("  brew install libpq".to_string());
                self.add_log("  sudo apt install postgresql-client".to_string());
                self.add_log("  sudo pacman -S postgresql-libs".to_string());
            }
        }
    }

    /// Starts a pg_dump for `proj` on a background thread and surfaces it in
    /// the Process tab as a cancellable job.
    pub fn spawn_dump_job(&mut self, proj: ProjectConfig, out: PathBuf, format: DumpFormat) {
        let name = proj.name.clone();
        let job = Arc::new(Mutex::new(Job {
            label: format!("DB dump · {name}"),
            status: JobStatus::Running,
            started_at: chrono::Utc::now().timestamp(),
            detail: out.to_string_lossy().to_string(),
            error: None,
            pid: Arc::new(Mutex::new(None)),
        }));
        self.jobs.push(job.clone());
        self.add_log(format!(
            "Starting pg_dump for '{name}' ({} format)...",
            match format {
                DumpFormat::Custom => "custom",
                DumpFormat::Plain => "plain SQL",
            }
        ));

        let pid_sink = Arc::new(Mutex::new(None));
        if let Ok(mut j) = job.lock() {
            j.pid = pid_sink.clone();
        }
        let global_logs = self.logs.clone();

        std::thread::spawn(move || {
            let stderr_logs = global_logs.clone();
            let log_line: dbbackup::LogFn =
                std::sync::Arc::new(move |line: String| add_global_log(&stderr_logs, line));
            match dbbackup::run_dump(&proj, out.clone(), format, pid_sink, log_line) {
                Ok((size, _guard)) => {
                    if let Ok(mut j) = job.lock()
                        && j.status == JobStatus::Running
                    {
                        j.status = JobStatus::Done;
                        j.detail = format!("{} · {}", out.display(), dbbackup::human_size(size));
                    }
                    add_global_log(
                        &global_logs,
                        format!("Dump complete: {} ({})", name, dbbackup::human_size(size)),
                    );
                }
                Err(e) => {
                    let cancelled = job
                        .lock()
                        .map(|j| j.status == JobStatus::Cancelled)
                        .unwrap_or(false);
                    if let Ok(mut j) = job.lock()
                        && j.status == JobStatus::Running
                    {
                        j.status = if cancelled {
                            JobStatus::Cancelled
                        } else {
                            JobStatus::Failed
                        };
                        j.error = Some(format!("{:#}", e));
                    }
                    if cancelled {
                        add_global_log(&global_logs, format!("Dump cancelled: {name}"));
                    } else {
                        add_global_log(&global_logs, format!("Dump failed: {name}: {:#}", e));
                    }
                }
            }
        });
    }

    /// Restores `backup_file` into `proj`'s database on a background thread,
    /// always taking a safety dump of the target first. Both phases run as
    /// one cancellable Process-tab job.
    pub fn spawn_restore_job(&mut self, proj: ProjectConfig, backup_file: PathBuf) {
        let name = proj.name.clone();
        let job = Arc::new(Mutex::new(Job {
            label: format!("DB restore · {name}"),
            status: JobStatus::Running,
            started_at: chrono::Utc::now().timestamp(),
            detail: backup_file.to_string_lossy().to_string(),
            error: None,
            pid: Arc::new(Mutex::new(None)),
        }));
        self.jobs.push(job.clone());
        let tool = dbbackup::restore_tool_for(&backup_file);
        let safety_path = match dbbackup::safety_backup_path(&backup_file) {
            Ok(p) => p,
            Err(e) => {
                self.add_log(format!("Restore aborted: {:#}", e));
                return;
            }
        };
        if safety_path.exists() {
            self.add_log(format!(
                "Restore aborted: refusing to overwrite existing file {}",
                safety_path.display()
            ));
            return;
        }
        self.add_log(format!(
            "Restoring '{}' into '{name}' ({}); a safety dump of the current database is taken first...",
            backup_file.display(),
            match tool {
                dbbackup::RestoreTool::PgRestore => "custom format",
                dbbackup::RestoreTool::Psql => "plain SQL",
            }
        ));

        let global_logs = self.logs.clone();
        let safety = safety_path.clone();

        std::thread::spawn(move || {
            // Phase 1: mandatory pre-restore safety dump.
            if let Ok(mut j) = job.lock() {
                j.detail = format!("safety dump -> {}", safety.display());
            }
            add_global_log(
                &global_logs,
                format!("Safety dump of '{name}' -> {}", safety.display()),
            );
            let pid_sink: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
            if let Ok(mut j) = job.lock() {
                j.pid = pid_sink.clone();
            }
            let stderr_logs = global_logs.clone();
            let log_line: dbbackup::LogFn =
                std::sync::Arc::new(move |line: String| add_global_log(&stderr_logs, line));
            let cancelled = || {
                job.lock()
                    .map(|j| j.status == JobStatus::Cancelled)
                    .unwrap_or(false)
            };
            let dump_result = dbbackup::run_dump(
                &proj,
                safety.clone(),
                tool.safety_dump_format(),
                pid_sink,
                log_line,
            );

            if let Err(e) = dump_result {
                let was_cancelled = cancelled();
                if let Ok(mut j) = job.lock()
                    && j.status == JobStatus::Running
                {
                    j.status = if was_cancelled {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    };
                    j.error = Some(format!("{:#}", e));
                }
                if was_cancelled {
                    add_global_log(&global_logs, format!("Restore cancelled: {name}"));
                } else {
                    // The target was never touched - make that explicit.
                    add_global_log(
                        &global_logs,
                        format!(
                            "Safety dump failed, restore aborted; '{name}' is unchanged: {:#}",
                            e
                        ),
                    );
                }
                return;
            }

            if cancelled() {
                if let Ok(mut j) = job.lock()
                    && j.status == JobStatus::Running
                {
                    j.status = JobStatus::Cancelled;
                }
                add_global_log(&global_logs, format!("Restore cancelled: {name}"));
                return;
            }

            // Phase 2: restore into the database.
            if let Ok(mut j) = job.lock() {
                j.detail = format!("restoring {}", backup_file.display());
            }
            add_global_log(
                &global_logs,
                format!("Safety dump written: {}", safety.display()),
            );
            add_global_log(
                &global_logs,
                format!("Restoring {} ...", backup_file.display()),
            );
            let pid_sink: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
            if let Ok(mut j) = job.lock() {
                j.pid = pid_sink.clone();
            }
            let stderr_logs = global_logs.clone();
            let log_line: dbbackup::LogFn =
                std::sync::Arc::new(move |line: String| add_global_log(&stderr_logs, line));
            match dbbackup::run_restore(&proj, &backup_file, pid_sink, log_line) {
                Ok(_guard) => {
                    if let Ok(mut j) = job.lock()
                        && j.status == JobStatus::Running
                    {
                        j.status = JobStatus::Done;
                        j.detail = format!("restored · safety dump: {}", safety.display());
                    }
                    add_global_log(
                        &global_logs,
                        format!(
                            "Restore complete: '{name}' now matches {}",
                            backup_file.display()
                        ),
                    );
                }
                Err(e) => {
                    let was_cancelled = cancelled();
                    if let Ok(mut j) = job.lock()
                        && j.status == JobStatus::Running
                    {
                        j.status = if was_cancelled {
                            JobStatus::Cancelled
                        } else {
                            JobStatus::Failed
                        };
                        j.error = Some(format!("{:#}", e));
                    }
                    if was_cancelled {
                        add_global_log(&global_logs, format!("Restore cancelled: {name}"));
                    } else {
                        add_global_log(&global_logs, format!("Restore failed: {name}: {:#}", e));
                    }
                    add_global_log(
                        &global_logs,
                        format!(
                            "The pre-restore safety dump is preserved at {}",
                            safety.display()
                        ),
                    );
                }
            }
        });
    }

    /// Cancels any running dump jobs belonging to `project_name`.
    pub fn stop_jobs_for(&mut self, project_name: &str) {
        for job in &self.jobs {
            let matches = job
                .lock()
                .map(|j| j.label.ends_with(project_name) && j.status == JobStatus::Running);
            if matches.unwrap_or(false)
                && let Ok(mut j) = job.lock()
            {
                j.status = JobStatus::Cancelled;
                j.cancel();
            }
        }
    }

    pub fn delete_selected_project(&mut self) -> Result<()> {
        if !self.config.projects.is_empty()
            && self.selected_project_idx < self.config.projects.len()
        {
            let removed = self.config.projects.remove(self.selected_project_idx);
            self.stop_session_for(&removed.name);
            self.stop_jobs_for(&removed.name);
            self.config.save()?;
            if self.selected_project_idx >= self.config.projects.len()
                && self.selected_project_idx > 0
            {
                self.selected_project_idx -= 1;
            }
            self.load_selected_into_form();
            self.snap_selection_to_visible();
            self.status_message = format!("Deleted project '{}'", removed.name);
        }
        Ok(())
    }

    // --- Navigation ---

    /// Project indices that survive the current name filter.
    pub fn visible_projects(&self) -> Vec<usize> {
        let query = self.filter.value().trim().to_lowercase();
        self.config
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| query.is_empty() || p.name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Moves the selection within the filtered list, snapping to the nearest
    /// visible project when the current one is filtered out.
    pub fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_projects();
        if visible.is_empty() {
            return;
        }
        let new_pos = match visible.iter().position(|&i| i == self.selected_project_idx) {
            Some(pos) => (pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize,
            None => {
                if delta < 0 {
                    visible.len() - 1
                } else {
                    0
                }
            }
        };
        self.selected_project_idx = visible[new_pos];
        self.load_selected_into_form();
    }

    /// Re-selects a visible project if the filter hid the current one.
    pub fn snap_selection_to_visible(&mut self) {
        let visible = self.visible_projects();
        if !visible.is_empty() && !visible.contains(&self.selected_project_idx) {
            self.selected_project_idx = visible[0];
            self.load_selected_into_form();
        }
    }

    /// Keeps the selected row inside the visible scroll window.
    pub fn clamp_project_scroll(&mut self, viewport_rows: usize) {
        if viewport_rows == 0 {
            return;
        }
        let pos = match self
            .visible_projects()
            .iter()
            .position(|&i| i == self.selected_project_idx)
        {
            Some(p) => p,
            None => {
                self.project_scroll = 0;
                return;
            }
        };
        if pos < self.project_scroll {
            self.project_scroll = pos;
        } else if pos >= self.project_scroll + viewport_rows {
            self.project_scroll = pos + 1 - viewport_rows;
        }
    }

    /// Scrolls the Logs pane. Positive values page towards history, zero
    /// offset means "follow latest".
    pub fn scroll_logs(&mut self, lines: isize) {
        let max = self.logs.lock().map(|l| l.len()).unwrap_or(0) as isize;
        self.log_scroll = (self.log_scroll as isize + lines).clamp(0, max) as usize;
    }

    pub fn cycle_details_tab(&mut self, forward: bool) {
        self.details_tab = match (self.details_tab, forward) {
            (DetailsTab::Overview, true) => DetailsTab::Config,
            (DetailsTab::Config, true) => DetailsTab::Process,
            (DetailsTab::Process, true) => DetailsTab::Overview,
            (DetailsTab::Overview, false) => DetailsTab::Process,
            (DetailsTab::Config, false) => DetailsTab::Overview,
            (DetailsTab::Process, false) => DetailsTab::Config,
        };
    }

    // --- Sessions ---

    pub fn session_for(&self, name: &str) -> Option<Arc<Mutex<RunningSession>>> {
        self.sessions
            .iter()
            .find(|s| s.lock().map(|g| g.project_name == name).unwrap_or(false))
            .cloned()
    }
    pub fn selected_session(&self) -> Option<Arc<Mutex<RunningSession>>> {
        self.session_for(&self.selected_project_name())
    }

    fn selected_session_log_text(&self) -> Option<(String, usize, String)> {
        let session = self.selected_session()?;
        let name = self.selected_project_name();
        let logs = session.lock().ok()?.logs.lock().ok()?.clone();
        Some((name, logs.len(), logs.join("\n")))
    }

    pub fn copy_selected_logs(&mut self) {
        let Some((project, count, text)) = self.selected_session_log_text() else {
            self.add_log("No Drizzle Studio logs for the selected project.".to_string());
            return;
        };
        if count == 0 {
            self.add_log("No Drizzle Studio logs for the selected project.".to_string());
            return;
        }
        match crate::open::copy_to_clipboard(&text) {
            Ok(()) => self.add_log(format!(
                "Copied {count} log lines for '{project}' to clipboard."
            )),
            Err(e) => self.add_log(format!("Failed to copy logs: {e:#}")),
        }
    }

    pub fn selected_url(&self) -> Option<String> {
        let session = self.selected_session()?;
        let s = session.lock().ok()?;
        s.url().map(|u| u.to_string())
    }

    pub fn is_project_running(&self, name: &str) -> bool {
        self.sessions.iter().any(|s| {
            s.lock()
                .map(|g| {
                    g.project_name == name
                        && matches!(
                            g.status,
                            SessionStatus::Starting
                                | SessionStatus::Pulling
                                | SessionStatus::Running
                        )
                })
                .unwrap_or(false)
        })
    }

    pub fn project_state(&self, name: &str) -> Option<ProjectState> {
        let session = self.session_for(name)?;
        let s = session.lock().ok()?;
        Some(match s.status {
            SessionStatus::Starting | SessionStatus::Pulling | SessionStatus::Running => {
                ProjectState::Running
            }
            SessionStatus::Error => ProjectState::Error,
            SessionStatus::Stopped => ProjectState::Stopped,
        })
    }

    // --- Launch flow ---

    /// Smart paste: recognises a complete `postgresql://...` URL, a
    /// `libsql://...` Turso URL, or an existing local SQLite file path, and
    /// fills the form accordingly. Returns `true` when the paste was consumed;
    /// otherwise the caller should insert the text into the active field.
    pub fn apply_pasted_text(&mut self, text: &str) -> bool {
        if self.mode != AppMode::EditingForm {
            return false;
        }
        let trimmed = text.trim();

        // Turso/libsql URL -> switch engine and fill the URL field.
        if trimmed.starts_with("libsql://") {
            self.engine = Engine::Turso;
            self.input_url = Input::from(trimmed.to_string());
            self.active_field = FormField::DbUrl;
            self.error_message = None;
            self.status_message =
                "Detected a Turso database URL - add your auth token to save.".to_string();
            return true;
        }

        // Existing local SQLite file path -> switch engine and fill the path.
        let looks_like_sqlite_path = !trimmed.contains("://")
            && (trimmed.ends_with(".db")
                || trimmed.ends_with(".sqlite")
                || trimmed.ends_with(".sqlite3"));
        if looks_like_sqlite_path && std::path::Path::new(trimmed).is_file() {
            self.engine = Engine::Sqlite;
            self.input_dbpath = Input::from(trimmed.to_string());
            self.active_field = FormField::DbPath;
            self.error_message = None;
            self.status_message = "Detected a local SQLite database file.".to_string();
            return true;
        }

        let Some(parsed) = check::parse_full_pg_url(text) else {
            return false;
        };

        let redacted_url = format!(
            "postgresql://{}@{}:{}/{}",
            parsed.user, parsed.host, parsed.port, parsed.dbname
        );
        self.connection_type = ConnectionType::Url;
        self.input_url = Input::from(redacted_url);
        if !parsed.user.is_empty() {
            self.input_dbuser = Input::from(parsed.user.clone());
        }
        self.input_host = Input::from(parsed.host.clone());
        self.input_port = if parsed.port == 5432 {
            Input::default()
        } else {
            Input::from(parsed.port.to_string())
        };
        self.input_dbname = Input::from(parsed.dbname.clone());
        if let Some(password) = parsed.password
            && !password.is_empty()
        {
            self.input_dbpass = Input::from(password);
        }
        self.active_field = FormField::DbUrl;
        self.error_message = None;
        self.status_message =
            "Detected a full connection URL - all fields were filled in.".to_string();
        true
    }

    /// Verifies the selected project is reachable without launching Studio.
    /// Runs on a background thread; results are written to the Logs pane.
    pub fn test_selected_connection(&mut self) {
        if self.config.projects.is_empty() {
            self.add_log("No project selected to test.".to_string());
            return;
        }
        let proj = self.config.projects[self.selected_project_idx].clone();
        let name = proj.name.clone();
        self.add_log(format!("Testing connection for '{name}'..."));
        let logs = self.logs.clone();
        std::thread::spawn(move || match check::check_connection(&proj) {
            Ok(msg) => add_global_log(&logs, format!("Connection OK: {msg}")),
            Err(e) => add_global_log(&logs, format!("Connection FAILED: {:#}", e)),
        });
    }

    /// Stage one of the two-step launch: focus the Details pane for the
    /// selected project instead of launching immediately. A second press of
    /// Enter / Shift+Enter / r while Details is focused performs the launch.
    pub fn focus_details_for_launch(&mut self) {
        if self.config.projects.is_empty() {
            self.add_log("No project selected to connect.".to_string());
            return;
        }
        let name = self.selected_project_name();
        self.active_pane = ActivePane::Details;
        self.status_message = format!(
            "Selected '{}'. Press Enter to connect (opens browser when ready), Shift+Enter or r to run without opening.",
            name
        );
    }

    /// Jumps to the Details pane's Process tab so the user can watch the
    /// session come up.
    fn show_process_tab(&mut self) {
        self.active_pane = ActivePane::Details;
        self.details_tab = DetailsTab::Process;
    }

    pub fn start_selected_project(&mut self, auto_open: bool) {
        if self.config.projects.is_empty() {
            self.add_log("No project selected to launch.".to_string());
            return;
        }

        let proj = self.config.projects[self.selected_project_idx].clone();
        let name = proj.name.clone();

        if let Some(session) = self.session_for(&name) {
            let running = session
                .lock()
                .map(|s| {
                    matches!(
                        s.status,
                        SessionStatus::Starting | SessionStatus::Pulling | SessionStatus::Running
                    )
                })
                .unwrap_or(false);
            if running {
                self.add_log(format!("Project '{}' is already running.", name));
                self.show_process_tab();
                return;
            }
            self.stop_session_for(&name);
        }

        let studio_port = find_free_port().unwrap_or(4983);
        let session = Arc::new(Mutex::new(RunningSession {
            project_name: name.clone(),
            studio_port,
            ssh: None,
            studio_child: None,
            status: SessionStatus::Starting,
            logs: Arc::new(Mutex::new(Vec::new())),
            tunnel_url: None,
            error: None,
            auto_open,
            studio_ready: false,
            studio_pid: None,
            ssh_pid: None,
            log_path: None,
            started_at: Some(chrono::Utc::now().timestamp()),
        }));

        self.sessions.push(session.clone());
        self.add_log(format!("Starting project '{}'...", name));
        self.show_process_tab();

        let global_logs = self.logs.clone();
        std::thread::spawn(move || {
            run_session(proj, session, global_logs);
        });
    }

    pub fn stop_selected_project(&mut self) {
        let name = self.selected_project_name();
        if name.is_empty() {
            return;
        }
        self.stop_jobs_for(&name);
        if let Some(session) = self.session_for(&name) {
            if let Ok(mut s) = session.lock() {
                s.stop();
            }
            self.add_log(format!("Stopped project '{}'", name));
        } else {
            self.add_log(format!("Project '{}' is not running.", name));
        }
    }

    pub fn stop_session_for(&mut self, name: &str) {
        if let Some(session) = self.session_for(name) {
            if let Ok(mut s) = session.lock() {
                s.stop();
            }
            self.sessions
                .retain(|s| s.lock().map(|g| g.project_name != name).unwrap_or(false));
        }
    }

    pub fn stop_all_sessions(&mut self) {
        for s in &self.sessions {
            if let Ok(mut g) = s.lock() {
                g.stop();
            }
        }
        self.sessions.clear();
    }

    pub fn poll_auto_open(&mut self) {
        for session in &self.sessions {
            let (should_open, url) = {
                let Ok(mut s) = session.lock() else {
                    continue;
                };
                if !s.auto_open || !s.studio_ready || s.status != SessionStatus::Running {
                    continue;
                }
                s.auto_open = false;
                let url = s.tunnel_url.clone().unwrap_or_else(|| {
                    format!("https://local.drizzle.studio?port={}", s.studio_port)
                });
                (true, url)
            };
            if should_open {
                self.add_log(format!("Opening {} in browser...", url));
                if let Err(e) = open_url(&url) {
                    self.add_log(format!("Failed to open URL: {:#}", e));
                }
            }
        }
    }

    fn restore_sessions(&mut self) {
        let entries = persist::load();
        let mut kept: Vec<PersistedSession> = Vec::new();
        for entry in entries {
            let project_exists = self
                .config
                .projects
                .iter()
                .any(|p| p.name == entry.project_name);
            if !project_exists {
                kill_detached(&entry);
                continue;
            }
            if port_in_use(entry.studio_port) {
                let log_path = PathBuf::from(&entry.log_path);
                let logs = tail_lines(&log_path, 50);
                let offset = file_len(&log_path);
                let session = Arc::new(Mutex::new(RunningSession {
                    project_name: entry.project_name.clone(),
                    studio_port: entry.studio_port,
                    ssh: None,
                    studio_child: None,
                    status: SessionStatus::Running,
                    logs: Arc::new(Mutex::new(logs)),
                    tunnel_url: Some(entry.tunnel_url.clone()),
                    error: None,
                    auto_open: false,
                    studio_ready: true,
                    studio_pid: Some(entry.studio_pid),
                    ssh_pid: entry.ssh_pid,
                    log_path: Some(log_path.clone()),
                    started_at: None, // detached: original start time unknown
                }));
                let session_clone = session.clone();
                let global = self.logs.clone();
                std::thread::spawn(move || {
                    tail_session_log(session_clone, global, log_path, offset);
                });
                self.sessions.push(session);
                self.add_log(format!(
                    "Restored running session for project '{}'.",
                    entry.project_name
                ));
                kept.push(entry);
            } else {
                kill_detached(&entry);
            }
        }
        let _ = persist::save(&kept);
    }

    pub fn shutdown(&mut self) {
        #[cfg(not(unix))]
        {
            self.stop_all_sessions();
            return;
        }
        #[cfg(unix)]
        {
            let mut persisted: Vec<PersistedSession> = Vec::new();
            for session in &self.sessions {
                let Ok(mut s) = session.lock() else {
                    continue;
                };
                if s.status == SessionStatus::Running && s.studio_pid.is_some() {
                    if let Some(log_path) = &s.log_path {
                        let url = s.tunnel_url.clone().unwrap_or_else(|| {
                            format!("https://local.drizzle.studio?port={}", s.studio_port)
                        });
                        let pid = s.studio_pid.unwrap();
                        let ssh_pid = s.ssh.as_ref().map(|t| t.child.id()).or(s.ssh_pid);
                        persisted.push(PersistedSession {
                            project_name: s.project_name.clone(),
                            studio_port: s.studio_port,
                            studio_pid: pid,
                            studio_pgid: pid,
                            ssh_pid,
                            tunnel_url: url,
                            log_path: log_path.to_string_lossy().to_string(),
                        });
                        // Detach: forget the tunnel so its Drop doesn't kill ssh.
                        let tunnel = s.ssh.take();
                        std::mem::forget(tunnel);
                        // Drop the child handle without killing it (Child has no
                        // kill-on-drop; the process is reparented and kept alive).
                        s.studio_child = None;
                    }
                } else {
                    s.stop();
                }
            }
            let _ = persist::save(&persisted);
        }
    }

    pub fn open_selected_url(&mut self) {
        match self.selected_url() {
            Some(u) => {
                self.add_log(format!("Opening {} ...", u));
                if let Err(e) = open_url(&u) {
                    self.add_log(format!("Failed to open URL: {:#}", e));
                }
            }
            None => self.add_log("No running session URL to open.".to_string()),
        }
    }

    pub fn copy_selected_url(&mut self) {
        match self.selected_url() {
            Some(u) => {
                if let Err(e) = copy_to_clipboard(&u) {
                    self.add_log(format!("Failed to copy URL: {:#}", e));
                } else {
                    self.add_log(format!("Copied {} to clipboard.", u));
                }
            }
            None => self.add_log("No running session URL to copy.".to_string()),
        }
    }

    // --- Self-update ('u') ---

    /// Kicks off a self-update on a background thread so the UI keeps
    /// responding; progress is surfaced via the update popup and Logs.
    pub fn start_update(&mut self) {
        if self.update_tracker.is_some() {
            self.add_log("An update is already in progress.".to_string());
            return;
        }
        self.add_log("Checking GitHub Releases for updates...".to_string());
        let tracker = Arc::new(UpdateTracker {
            phase: Arc::new(Mutex::new(crate::updater::UpdatePhase::Checking)),
            note: Arc::new(Mutex::new(String::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
            finished: Arc::new(Mutex::new(None)),
        });
        self.update_tracker = Some(tracker.clone());

        std::thread::spawn(move || {
            let result = crate::updater::update_with_progress(
                &|phase, note| {
                    if let Ok(mut p) = tracker.phase.lock() {
                        *p = phase;
                    }
                    if let Ok(mut n) = tracker.note.lock() {
                        *n = format!("v{note}");
                    }
                },
                &tracker.cancel,
            );
            let result = result.map_err(|e| format!("{:#}", e));
            if let Ok(mut f) = tracker.finished.lock() {
                *f = Some(result);
            }
        });
    }

    /// True while a self-update is running (popup shown, cancel available).
    pub fn update_in_progress(&self) -> bool {
        self.update_tracker.is_some()
    }

    /// Requests cancellation; takes effect at the next update phase boundary.
    pub fn cancel_update(&mut self) {
        if let Some(tracker) = &self.update_tracker {
            tracker.cancel.store(true, Ordering::Relaxed);
            self.add_log("Cancelling update after the current step...".to_string());
        }
    }

    /// Collects the finished update outcome and reports it in the Logs pane.
    pub fn poll_update_finished(&mut self) {
        let Some(tracker) = &self.update_tracker else {
            return;
        };
        let finished = tracker.finished.lock().ok().and_then(|mut f| f.take());
        let Some(result) = finished else {
            return;
        };
        self.update_tracker = None;
        match result {
            Ok(outcome) => {
                let msg = outcome.message();
                self.status_message = msg.clone();
                self.add_log(msg);
            }
            Err(e) => self.add_log(format!("Self-update error: {e}")),
        }
    }
}

fn push_session_log(session: &Arc<Mutex<RunningSession>>, msg: String) {
    if let Ok(s) = session.lock()
        && let Ok(mut l) = s.logs.lock()
    {
        l.push(msg);
    }
}

fn add_global_log(global: &Arc<Mutex<Vec<String>>>, msg: String) {
    if let Ok(mut g) = global.lock() {
        g.push(msg);
    }
}

fn set_session_error(session: &Arc<Mutex<RunningSession>>, e: String) {
    if let Ok(mut s) = session.lock() {
        s.status = SessionStatus::Error;
        s.error = Some(e);
    }
}

fn session_cancelled(session: &Arc<Mutex<RunningSession>>) -> bool {
    session
        .lock()
        .map(|s| s.status == SessionStatus::Stopped)
        .unwrap_or(true)
}

fn run_session(
    proj: ProjectConfig,
    session: Arc<Mutex<RunningSession>>,
    global_logs: Arc<Mutex<Vec<String>>>,
) {
    let name = proj.name.clone();

    if let Err(e) = check_dependencies() {
        push_session_log(&session, format!("Dependency check failed: {:#}", e));
        set_session_error(&session, format!("{:#}", e));
        add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
        return;
    }

    let studio_port = session.lock().map(|s| s.studio_port).unwrap_or(4983);

    // Only wire engines travel over TCP; file/API-backed engines need no
    // tunnel even when a stray SSH connection type is stored.
    let tunnel_needed = matches!(proj.engine, Engine::Postgres | Engine::Mysql)
        && proj.connection_type == ConnectionType::Ssh;

    let target = if tunnel_needed {
        match establish_tunnel(&proj.ssh_connection, &proj.db_port) {
            Ok(tunnel) => {
                let local_port = tunnel.local_port;
                if let Ok(mut s) = session.lock() {
                    s.ssh_pid = Some(tunnel.child.id());
                    s.ssh = Some(tunnel);
                }
                push_session_log(
                    &session,
                    format!("SSH tunnel established on local port {}", local_port),
                );
                proj.resolve_target(Some(local_port))
            }
            Err(e) => {
                push_session_log(&session, format!("SSH Tunnel error: {:#}", e));
                set_session_error(&session, format!("{:#}", e));
                add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
                return;
            }
        }
    } else {
        proj.resolve_target(None)
    };

    let target = match target {
        Ok(t) => t,
        Err(e) => {
            push_session_log(&session, format!("Connection error: {:#}", e));
            set_session_error(&session, format!("{:#}", e));
            add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
            return;
        }
    };

    if let Ok(mut s) = session.lock() {
        s.status = SessionStatus::Pulling;
    }

    let session_logs = session
        .lock()
        .map(|s| s.logs.clone())
        .unwrap_or_else(|_| Arc::new(Mutex::new(Vec::new())));
    let workspace = match prepare_workspace(&proj.name, &target, &session_logs) {
        Ok(w) => w,
        Err(e) => {
            push_session_log(&session, format!("Workspace error: {:#}", e));
            set_session_error(&session, format!("{:#}", e));
            add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
            return;
        }
    };

    if session_cancelled(&session) {
        return;
    }

    let log_path = workspace.join("studio.log");
    let child = match spawn_studio(&workspace, &target, studio_port, &log_path) {
        Ok(c) => c,
        Err(e) => {
            push_session_log(&session, format!("Drizzle Studio error: {:#}", e));
            set_session_error(&session, format!("{:#}", e));
            add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
            return;
        }
    };

    let studio_pid = child.id();

    {
        let mut s = session.lock().unwrap();
        s.studio_child = Some(child);
        s.studio_pid = Some(studio_pid);
        s.log_path = Some(log_path.clone());
        s.status = SessionStatus::Running;
        s.tunnel_url = Some(format!("https://local.drizzle.studio?port={}", studio_port));
    }

    if session_cancelled(&session) {
        if let Ok(mut s) = session.lock() {
            s.stop();
        }
        return;
    }

    {
        let session_clone = session.clone();
        let global_clone = global_logs.clone();
        std::thread::spawn(move || {
            tail_session_log(session_clone, global_clone, log_path, 0);
        });
    }

    push_session_log(
        &session,
        format!("Drizzle Studio started; waiting for readiness on port {studio_port}."),
    );
}

fn port_in_use(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

fn file_len(path: &PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn tail_lines(path: &PathBuf, count: usize) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

#[cfg(unix)]
fn kill_detached(entry: &PersistedSession) {
    unsafe {
        libc::kill(-(entry.studio_pgid as i32), libc::SIGKILL);
        libc::kill(entry.studio_pid as i32, libc::SIGKILL);
        if let Some(pid) = entry.ssh_pid {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_detached(_entry: &PersistedSession) {}

fn tail_session_log(
    session: Arc<Mutex<RunningSession>>,
    global: Arc<Mutex<Vec<String>>>,
    log_path: PathBuf,
    start_offset: u64,
) {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(&log_path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if file.seek(SeekFrom::Start(start_offset)).is_err() {
        return;
    }
    let mut offset = start_offset;
    let mut partial = String::new();
    let mut dead_ticks = 0u32;
    loop {
        let Ok(state) = session.lock() else { break };
        if matches!(state.status, SessionStatus::Stopped | SessionStatus::Error) {
            break;
        }
        drop(state);
        let mut saw_url = false;
        if let Ok(meta) = std::fs::metadata(&log_path)
            && meta.len() > offset
        {
            let mut chunk = String::new();
            if file.seek(SeekFrom::Start(offset)).is_ok() && file.read_to_string(&mut chunk).is_ok()
            {
                offset += chunk.len() as u64;
                partial.push_str(&chunk);
                while let Some(idx) = partial.find('\n') {
                    let line: String = partial.drain(..idx).collect();
                    partial.drain(..1);
                    let line = line.trim_end_matches('\r').to_string();
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(s) = session.lock()
                        && let Ok(mut l) = s.logs.lock()
                    {
                        l.push(line.clone());
                    }
                    if let Some(url) = extract_tunnel_url(&line) {
                        saw_url = true;
                        if let Ok(mut s) = session.lock() {
                            s.tunnel_url = Some(url);
                        }
                    }
                }
            }
        }
        let (port, project_name, ready, exited) = {
            let Ok(mut s) = session.lock() else { break };
            let mut exited = None;
            if let Some(child) = s.studio_child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        s.studio_child = None;
                        s.studio_pid = None;
                        exited = Some(status);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let msg = format!("Could not monitor Drizzle Studio: {e}");
                        s.fail(msg.clone());
                        add_global_log(
                            &global,
                            format!("Project '{}' failed: {msg}", s.project_name),
                        );
                        break;
                    }
                }
            }
            (
                s.studio_port,
                s.project_name.clone(),
                s.studio_ready,
                exited,
            )
        };
        if let Some(status) = exited {
            let message = if ready {
                format!("Drizzle Studio exited unexpectedly ({status}).")
            } else {
                format!("Drizzle Studio exited before becoming ready ({status}).")
            };
            if let Ok(mut s) = session.lock() {
                s.fail(message.clone());
            }
            add_global_log(
                &global,
                format!("Project '{}' failed: {message}", project_name),
            );
            break;
        }
        let became_ready = !ready && (saw_url || port_in_use(port));
        if became_ready {
            if let Ok(mut s) = session.lock() {
                if s.status == SessionStatus::Starting {
                    s.status = SessionStatus::Running;
                    s.studio_ready = true;
                }
            }
            add_global_log(&global, format!("Project '{}' is running.", project_name));
        } else if ready && port_in_use(port) {
            dead_ticks = 0;
        } else if ready {
            dead_ticks += 1;
            if dead_ticks >= 10 {
                let message = format!("Drizzle Studio stopped listening on port {port}.");
                if let Ok(mut s) = session.lock() {
                    s.fail(message.clone());
                }
                add_global_log(
                    &global,
                    format!("Project '{}' failed: {message}", project_name),
                );
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_field_order_skips_url_only_fields() {
        let order = FormField::order(Engine::Postgres, ConnectionType::Ssh);
        assert!(!order.contains(&FormField::DbUrl));
        assert!(!order.contains(&FormField::DbHost));
        assert!(order.contains(&FormField::Engine));
    }

    #[test]
    fn url_field_order_includes_url_and_host() {
        let order = FormField::order(Engine::Postgres, ConnectionType::Url);
        assert!(order.contains(&FormField::DbUrl));
        assert!(order.contains(&FormField::DbHost));
    }

    #[test]
    fn local_field_order_has_no_url_or_ssh() {
        let order = FormField::order(Engine::Postgres, ConnectionType::Local);
        assert!(!order.contains(&FormField::DbUrl));
        assert!(!order.contains(&FormField::SshConnection));
        assert!(order.contains(&FormField::DbHost));
    }

    #[test]
    fn nonwire_engine_orders_show_engine_specific_fields() {
        let sqlite = FormField::order(Engine::Sqlite, ConnectionType::Ssh);
        assert!(sqlite.contains(&FormField::DbPath));
        assert!(!sqlite.contains(&FormField::ConnectionType));
        assert!(!sqlite.contains(&FormField::DbPass));

        let d1 = FormField::order(Engine::D1, ConnectionType::Local);
        assert!(d1.contains(&FormField::CfAccountId));
        assert!(d1.contains(&FormField::CfDatabaseId));
        assert!(d1.contains(&FormField::DbPass));
        assert!(!d1.contains(&FormField::DbPath));

        let turso = FormField::order(Engine::Turso, ConnectionType::Url);
        assert!(turso.contains(&FormField::DbUrl));
        assert!(turso.contains(&FormField::DbPass));
        assert!(!turso.contains(&FormField::DbName));

        // MySQL shares the wire layouts with Postgres.
        assert_eq!(
            FormField::order(Engine::Mysql, ConnectionType::Ssh),
            FormField::order(Engine::Postgres, ConnectionType::Ssh)
        );
    }

    #[test]
    fn next_and_prev_wrap_around_for_every_type() {
        for engine in Engine::ALL {
            for ct in [
                ConnectionType::Ssh,
                ConnectionType::Url,
                ConnectionType::Local,
            ] {
                let order = FormField::order(engine, ct);
                let mut f = FormField::Name;
                for _ in 0..order.len() {
                    f = f.next(engine, ct);
                }
                assert_eq!(f, FormField::Name);
                for _ in 0..order.len() {
                    f = f.prev(engine, ct);
                }
                assert_eq!(f, FormField::Name);
            }
        }
    }

    #[test]
    fn next_from_last_field_is_first_field() {
        let order = FormField::order(Engine::Postgres, ConnectionType::Local);
        let last = *order.last().unwrap();
        assert_eq!(last.next(Engine::Postgres, ConnectionType::Local), order[0]);
    }

    fn bare_app() -> App {
        App {
            config: AppConfig::default(),
            selected_project_idx: 0,
            active_pane: ActivePane::ProjectsList,
            details_tab: DetailsTab::Overview,
            mode: AppMode::Normal,
            confirm_action: None,
            project_scroll: 0,
            filter: Input::default(),
            log_scroll: 0,
            backup_menu_idx: 0,
            input_backup_path: Input::default(),
            pending_pkg_manager: None,
            pending_restore_file: None,
            update_tracker: None,
            help_selected: 0,
            help_scroll: 0,
            jobs: Vec::new(),
            input_name: Input::from("n"),
            input_ssh: Input::from("s"),
            input_url: Input::from("u"),
            input_host: Input::from("h"),
            input_port: Input::from("p"),
            input_dbname: Input::from("d"),
            input_dbuser: Input::from("r"),
            input_dbpass: Input::from("*"),
            input_dbpath: Input::default(),
            input_cf_account: Input::default(),
            input_cf_database: Input::default(),
            engine: Engine::Postgres,
            connection_type: ConnectionType::Local,
            active_field: FormField::Name,
            is_new_project: false,
            status_message: String::new(),
            error_message: None,
            logs: Arc::new(Mutex::new(Vec::new())),
            sessions: Vec::new(),
            theme: Theme::default(),
        }
    }

    #[test]
    fn input_accessors_map_every_editable_field() {
        let app = bare_app();
        assert_eq!(app.input(FormField::Name).unwrap().value(), "n");
        assert_eq!(app.input(FormField::DbPass).unwrap().value(), "*");
        assert!(app.input(FormField::ConnectionType).is_none());
        assert!(app.input(FormField::Engine).is_none());
    }

    #[test]
    fn toggle_engine_cycles_and_resets_invalid_field() {
        let mut app = bare_app();
        app.engine = Engine::Postgres;
        app.connection_type = ConnectionType::Ssh;
        app.active_field = FormField::DbPath; // not on the Ssh layout

        app.toggle_engine();
        assert_eq!(app.engine, Engine::Sqlite);
        // DbPath exists in the SQLite layout so navigation keeps it.
        assert_eq!(app.active_field, FormField::DbPath);

        app.toggle_engine(); // -> D1 (no DbPath there)
        assert_eq!(app.engine, Engine::D1);
        assert_eq!(app.active_field, FormField::Engine);

        // Full cycle returns to Postgres.
        while app.engine != Engine::Postgres {
            app.toggle_engine();
        }
        assert_eq!(app.engine, Engine::Postgres);
    }

    #[test]
    fn paste_detects_turso_url_and_sqlite_file() {
        let mut app = bare_app();
        app.mode = AppMode::EditingForm;

        assert!(app.apply_pasted_text("libsql://acme.turso.io"));
        assert_eq!(app.engine, Engine::Turso);
        assert_eq!(app.input_url.value(), "libsql://acme.turso.io");

        let dir = std::env::temp_dir().join(format!("pg-studio-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("paste.db");
        std::fs::write(&db, b"x").unwrap();

        app.reset_form();
        app.mode = AppMode::EditingForm;
        assert!(app.apply_pasted_text(db.to_str().unwrap()));
        assert_eq!(app.engine, Engine::Sqlite);
        assert_eq!(app.input_dbpath.value(), db.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);

        // Non-matching text falls through untouched.
        app.reset_form();
        app.mode = AppMode::EditingForm;
        assert!(!app.apply_pasted_text("just some text"));
    }

    #[test]
    fn build_sqlite_project_zeroes_irrelevant_fields() {
        let mut app = bare_app();
        app.is_new_project = true;
        app.engine = Engine::Sqlite;
        app.input_name = Input::default();
        app.input_dbpath = Input::from("/tmp/x.db");

        let (proj, secret) = app.build_sqlite_project().unwrap();
        assert_eq!(proj.engine, Engine::Sqlite);
        assert_eq!(proj.db_path, "/tmp/x.db");
        assert_eq!(proj.derived_name(), "x@sqlite");
        assert!(secret.is_none());
        assert_eq!(proj.db_url, "");
        assert_eq!(proj.cf_account_id, "");

        // Empty path must fail validation.
        app.input_dbpath = Input::default();
        assert!(app.build_sqlite_project().is_err());
    }
    #[test]
    fn selected_session_log_text_preserves_order() {
        let mut app = bare_app();
        app.config.projects.push(ProjectConfig {
            name: "demo".to_string(),
            engine: Engine::Sqlite,
            connection_type: ConnectionType::Local,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: String::new(),
            db_port: String::new(),
            db_name: String::new(),
            db_user: String::new(),
            db_path: "/tmp/demo.db".to_string(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
            last_opened: 0,
        });
        let logs = Arc::new(Mutex::new(vec!["first".to_string(), "second".to_string()]));
        app.sessions.push(Arc::new(Mutex::new(RunningSession {
            project_name: "demo".to_string(),
            studio_port: 1,
            ssh: None,
            studio_child: None,
            status: SessionStatus::Starting,
            logs: logs.clone(),
            tunnel_url: None,
            error: None,
            auto_open: false,
            studio_ready: false,
            studio_pid: None,
            ssh_pid: None,
            log_path: None,
            started_at: None,
        })));
        let (name, count, text) = app.selected_session_log_text().unwrap();
        assert_eq!(
            (name, count, text),
            ("demo".to_string(), 2, "first\nsecond".to_string())
        );
        *logs.lock().unwrap() = Vec::new();
        assert_eq!(app.selected_session_log_text().unwrap().1, 0);
    }
    #[cfg(unix)]
    #[test]
    fn studio_exit_marks_session_error() {
        let path = std::env::temp_dir().join(format!("pg-studio-exit-{}", std::process::id()));
        std::fs::write(&path, "").unwrap();
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let session = Arc::new(Mutex::new(RunningSession {
            project_name: "demo".to_string(),
            studio_port: 0,
            ssh: None,
            studio_child: Some(child),
            status: SessionStatus::Starting,
            logs: Arc::new(Mutex::new(Vec::new())),
            tunnel_url: None,
            error: None,
            auto_open: true,
            studio_ready: false,
            studio_pid: None,
            ssh_pid: None,
            log_path: Some(path.clone()),
            started_at: None,
        }));
        let global = Arc::new(Mutex::new(Vec::new()));
        tail_session_log(session.clone(), global.clone(), path.clone(), 0);
        let state = session.lock().unwrap();
        assert_eq!(state.status, SessionStatus::Error);
        assert!(!state.auto_open);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("exited before becoming ready")
        );
        assert!(
            global
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("demo' failed"))
        );
        let _ = std::fs::remove_file(path);
    }
}
