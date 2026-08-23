use crate::check;
use crate::config::{AppConfig, ConnectionType, ProjectBundle, ProjectConfig};
use crate::drizzle::{check_dependencies, extract_tunnel_url, prepare_workspace, spawn_studio};
use crate::open::{copy_to_clipboard, open_url};
use crate::persist::{self, PersistedSession};
use crate::session::{RunningSession, SessionStatus};
use crate::ssh::{establish_tunnel, find_free_port};
use crate::theme::Theme;
use anyhow::{Result, anyhow};
use chrono::{Local, TimeZone};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    ConnectionType,
    SshConnection,
    DbUrl,
    DbHost,
    DbPort,
    DbName,
    DbUser,
    DbPass,
}

impl FormField {
    fn order(ct: ConnectionType) -> &'static [FormField] {
        match ct {
            ConnectionType::Ssh => &[
                FormField::Name,
                FormField::ConnectionType,
                FormField::SshConnection,
                FormField::DbPort,
                FormField::DbName,
                FormField::DbUser,
                FormField::DbPass,
            ],
            ConnectionType::Url => &[
                FormField::Name,
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
                FormField::ConnectionType,
                FormField::DbHost,
                FormField::DbPort,
                FormField::DbName,
                FormField::DbUser,
                FormField::DbPass,
            ],
        }
    }

    pub fn next(&self, ct: ConnectionType) -> Self {
        let order = Self::order(ct);
        let idx = order.iter().position(|f| f == self).unwrap_or(0);
        order[(idx + 1) % order.len()]
    }

    pub fn prev(&self, ct: ConnectionType) -> Self {
        let order = Self::order(ct);
        let idx = order.iter().position(|f| f == self).unwrap_or(0);
        order[(idx + order.len() - 1) % order.len()]
    }

    pub fn get_help(&self) -> (&'static str, &'static str) {
        match self {
            FormField::Name => (
                "Unique identifier for this project. If left blank, defaults to database_name@host.",
                "Examples: production-us-east, staging-db, app_db@ubuntu@192.168.1.5",
            ),
            FormField::ConnectionType => (
                "How to reach the database: SSH tunnels through a remote server, URL connects directly, Local talks to a database on this machine.",
                "Press Enter or Space to cycle SSH -> URL -> Local. Choosing Local auto-fills the host with localhost.",
            ),
            FormField::SshConnection => (
                "SSH connection string to reach the remote server hosting the Postgres database.",
                "Examples: ubuntu@13.233.0.0, root@ec2.compute.amazonaws.com, admin@my-server.com",
            ),
            FormField::DbUrl => (
                "Full public connection string for a hosted database (PlanetScale, CockroachDB, etc.).",
                "Examples: postgresql://user:pass@host:5432/db — password is moved to your keychain on save.",
            ),
            FormField::DbHost => (
                "Host for a direct or local connection (used only if no full Connection URL is provided).",
                "Examples: localhost (locally running Postgres), 127.0.0.1, db.your-cluster.us-east-1.cockroachlabs.cloud",
            ),
            FormField::DbPort => (
                "The remote port Postgres is listening on. Leave blank to default to 5432.",
                "Examples: 5432, 5433, 6432, 26257",
            ),
            FormField::DbName => (
                "Name of the target Postgres database.",
                "Examples: postgres, production_main, app_db_v2, defaultdb",
            ),
            FormField::DbUser => (
                "Postgres database user with read/introspection permissions.",
                "Examples: postgres, db_admin, readonly_user",
            ),
            FormField::DbPass => (
                "Postgres user password. Securely saved in your OS Keychain (only fetched when launching).",
                "Input is masked with asterisks (*)",
            ),
        }
    }
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

    // Form inputs
    pub input_name: Input,
    pub input_ssh: Input,
    pub input_url: Input,
    pub input_host: Input,
    pub input_port: Input,
    pub input_dbname: Input,
    pub input_dbuser: Input,
    pub input_dbpass: Input,
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
            .sort_by(|a, b| b.last_opened.cmp(&a.last_opened));

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

            input_name: Input::default(),
            input_ssh: Input::default(),
            input_url: Input::default(),
            input_host: Input::default(),
            input_port: Input::default(),
            input_dbname: Input::default(),
            input_dbuser: Input::default(),
            input_dbpass: Input::default(),
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
            FormField::ConnectionType => None,
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
            FormField::ConnectionType => None,
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
            self.connection_type = proj.connection_type;
            self.input_ssh = Input::from(proj.ssh_connection.clone());
            self.input_url = Input::from(proj.db_url.clone());
            self.input_host = Input::from(proj.db_host.clone());
            self.input_port = if proj.db_port == "5432" {
                Input::default()
            } else {
                Input::from(proj.db_port.clone())
            };
            self.input_dbname = Input::from(proj.db_name.clone());
            self.input_dbuser = Input::from(proj.db_user.clone());
            self.input_dbpass = Input::default(); // DO NOT query keychain here!
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
        self.connection_type = src.connection_type;
        self.input_ssh = Input::from(src.ssh_connection.clone());
        self.input_url = Input::from(src.db_url.clone());
        self.input_host = Input::from(src.db_host.clone());
        self.input_port = if src.db_port == "5432" {
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
        self.connection_type = ConnectionType::Ssh;
        self.active_field = FormField::Name;
        self.is_new_project = true;
    }

    pub fn save_form_to_project(&mut self) -> Result<()> {
        let ssh = self.input_ssh.value().to_string();
        let db_url_input = self.input_url.value().trim().to_string();
        let db_host = self.input_host.value().trim().to_string();
        let port = self.input_port.value().to_string();
        let dbname = self.input_dbname.value().to_string();
        let dbuser = self.input_dbuser.value().to_string();
        let dbpass = self.input_dbpass.value().to_string();

        let final_port = if port.trim().is_empty() {
            "5432".to_string()
        } else {
            port.trim().to_string()
        };

        let mut name = self.input_name.value().trim().to_string();
        if name.is_empty() {
            let host = match self.connection_type {
                ConnectionType::Ssh => ssh.clone(),
                ConnectionType::Url | ConnectionType::Local => {
                    if db_host.is_empty() {
                        dbname.clone()
                    } else {
                        db_host.clone()
                    }
                }
            };
            name = format!("{}@{}", dbname, host);
        }

        let existing_match = self.config.projects.iter().position(|p| p.name == name);
        if let Some(matching_idx) = existing_match
            && (self.is_new_project || matching_idx != self.selected_project_idx)
        {
            self.error_message = Some(format!("A project named '{}' already exists!", name));
            return Err(anyhow!("Project name must be unique"));
        }

        // In URL mode, pull any embedded password out of the URL into the keychain.
        let (db_url, extracted_pass) =
            if self.connection_type == ConnectionType::Url && !db_url_input.is_empty() {
                let (redacted, pass) = ProjectConfig::redact_url_password(&db_url_input);
                (redacted, pass)
            } else {
                (db_url_input, None)
            };

        let proj = ProjectConfig {
            name: name.clone(),
            connection_type: self.connection_type,
            ssh_connection: ssh,
            db_url,
            db_host,
            db_port: final_port,
            db_name: dbname,
            db_user: dbuser,
            last_opened: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        };

        let pass_to_save = extracted_pass.unwrap_or(dbpass);
        if !pass_to_save.is_empty() {
            proj.save_password(&pass_to_save)?;
        }

        if self.is_new_project {
            self.config.projects.push(proj);
            self.selected_project_idx = self.config.projects.len() - 1;
        } else if let Some(existing) = self.config.projects.get_mut(self.selected_project_idx) {
            *existing = proj;
        }

        self.config.save()?;
        self.load_selected_into_form();
        self.status_message = format!("Project '{}' saved successfully!", name);
        self.error_message = None;
        Ok(())
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
        let mut imported = 0;
        let mut skipped = 0;
        for project in bundle.projects {
            if self.config.projects.iter().any(|p| p.name == project.name) {
                skipped += 1;
                continue;
            }
            self.config.projects.push(project);
            imported += 1;
        }
        if imported > 0
            && let Err(e) = self.config.save()
        {
            self.add_log(format!("Failed to save imported projects: {:#}", e));
            return (0, skipped);
        }
        self.config
            .projects
            .sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
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

    // --- Backup menu ('b') ---

    pub fn backup_menu_items() -> &'static [&'static str] {
        &["Download app backup", "Restore app backup from file"]
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
        let default_path = if self.backup_menu_idx == 0 {
            crate::backup::default_backup_path()
        } else {
            AppConfig::export_file_path()
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
            _ => {}
        }
    }

    pub fn delete_selected_project(&mut self) -> Result<()> {
        if !self.config.projects.is_empty()
            && self.selected_project_idx < self.config.projects.len()
        {
            let removed = self.config.projects.remove(self.selected_project_idx);
            self.stop_session_for(&removed.name);
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

    /// Smart paste: recognises a complete `postgresql://...` URL and fills
    /// every form field from it. Returns `true` when the paste was consumed;
    /// otherwise the caller should insert the text into the active field.
    pub fn apply_pasted_text(&mut self, text: &str) -> bool {
        if self.mode != AppMode::EditingForm {
            return false;
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

    let db_url = match proj.connection_type {
        ConnectionType::Ssh => match establish_tunnel(&proj.ssh_connection, &proj.db_port) {
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
                let pass = proj.get_password().unwrap_or_default();
                format!(
                    "postgresql://{}:{}@127.0.0.1:{}/{}",
                    proj.db_user, pass, local_port, proj.db_name
                )
            }
            Err(e) => {
                push_session_log(&session, format!("SSH Tunnel error: {:#}", e));
                set_session_error(&session, format!("{:#}", e));
                add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
                return;
            }
        },
        ConnectionType::Url | ConnectionType::Local => match proj.connection_url(None) {
            Ok(url) => url,
            Err(e) => {
                push_session_log(&session, format!("Connection error: {:#}", e));
                set_session_error(&session, format!("{:#}", e));
                add_global_log(&global_logs, format!("Project '{}' failed to start.", name));
                return;
            }
        },
    };

    if let Ok(mut s) = session.lock() {
        s.status = SessionStatus::Pulling;
    }

    let session_logs = session
        .lock()
        .map(|s| s.logs.clone())
        .unwrap_or_else(|_| Arc::new(Mutex::new(Vec::new())));
    let workspace = match prepare_workspace(&proj.name, &db_url, &session_logs) {
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
    let child = match spawn_studio(&workspace, &db_url, studio_port, &log_path) {
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
        format!(
            "Drizzle Studio running at https://local.drizzle.studio?port={}",
            studio_port
        ),
    );
    add_global_log(&global_logs, format!("Project '{}' is running.", name));
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
        {
            let Ok(s) = session.lock() else {
                break;
            };
            if matches!(s.status, SessionStatus::Stopped | SessionStatus::Error) {
                break;
            }
        }

        if let Ok(meta) = std::fs::metadata(&log_path)
            && meta.len() > offset
        {
            let mut chunk = String::new();
            if file.seek(SeekFrom::Start(offset)).is_ok() && file.read_to_string(&mut chunk).is_ok()
            {
                offset += chunk.len() as u64;
                partial.push_str(&chunk);
                let mut drained: Vec<String> = Vec::new();
                while let Some(idx) = partial.find('\n') {
                    let line: String = partial.drain(..idx).collect();
                    partial.drain(..1);
                    drained.push(line);
                }
                for line in drained {
                    let line = line.trim_end_matches('\r').to_string();
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(s) = session.lock()
                        && let Ok(mut l) = s.logs.lock()
                    {
                        l.push(line.clone());
                    }
                    if let Some(url) = extract_tunnel_url(&line)
                        && let Ok(mut s) = session.lock()
                    {
                        s.tunnel_url = Some(url);
                        s.studio_ready = true;
                    }
                }
            }
        }

        let (port, project_name, ready) = {
            let Ok(s) = session.lock() else {
                break;
            };
            (s.studio_port, s.project_name.clone(), s.studio_ready)
        };
        if ready && port_in_use(port) {
            dead_ticks = 0;
        } else if ready {
            dead_ticks += 1;
            if dead_ticks >= 10 {
                if let Ok(mut s) = session.lock()
                    && s.status == SessionStatus::Running
                {
                    s.stop();
                }
                add_global_log(&global, format!("Project '{}' exited.", project_name));
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
        let order = FormField::order(ConnectionType::Ssh);
        assert!(!order.contains(&FormField::DbUrl));
        assert!(!order.contains(&FormField::DbHost));
    }

    #[test]
    fn url_field_order_includes_url_and_host() {
        let order = FormField::order(ConnectionType::Url);
        assert!(order.contains(&FormField::DbUrl));
        assert!(order.contains(&FormField::DbHost));
    }

    #[test]
    fn local_field_order_has_no_url_or_ssh() {
        let order = FormField::order(ConnectionType::Local);
        assert!(!order.contains(&FormField::DbUrl));
        assert!(!order.contains(&FormField::SshConnection));
        assert!(order.contains(&FormField::DbHost));
    }

    #[test]
    fn next_and_prev_wrap_around_for_every_type() {
        for ct in [
            ConnectionType::Ssh,
            ConnectionType::Url,
            ConnectionType::Local,
        ] {
            let order = FormField::order(ct);
            let mut f = FormField::Name;
            for _ in 0..order.len() {
                f = f.next(ct);
            }
            assert_eq!(f, FormField::Name);
            for _ in 0..order.len() {
                f = f.prev(ct);
            }
            assert_eq!(f, FormField::Name);
        }
    }

    #[test]
    fn next_from_last_field_is_first_field() {
        let order = FormField::order(ConnectionType::Local);
        let last = *order.last().unwrap();
        assert_eq!(last.next(ConnectionType::Local), order[0]);
    }

    #[test]
    fn input_accessors_map_every_editable_field() {
        let app = App {
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
            input_name: Input::from("n"),
            input_ssh: Input::from("s"),
            input_url: Input::from("u"),
            input_host: Input::from("h"),
            input_port: Input::from("p"),
            input_dbname: Input::from("d"),
            input_dbuser: Input::from("r"),
            input_dbpass: Input::from("*"),
            connection_type: ConnectionType::Local,
            active_field: FormField::Name,
            is_new_project: false,
            status_message: String::new(),
            error_message: None,
            logs: Arc::new(Mutex::new(Vec::new())),
            sessions: Vec::new(),
            theme: Theme::default(),
        };
        assert_eq!(app.input(FormField::Name).unwrap().value(), "n");
        assert_eq!(app.input(FormField::DbPass).unwrap().value(), "*");
        assert!(app.input(FormField::ConnectionType).is_none());
    }
}
