use crate::config::{AppConfig, ProjectConfig};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tui_input::Input;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ActivePane {
    ProjectsList,
    ProjectForm,
    Logs,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AppMode {
    Normal,
    EditingForm,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    SshConnection,
    DbPort,
    DbName,
    DbUser,
    DbPass,
}

impl FormField {
    pub fn next(&self) -> Self {
        match self {
            FormField::Name => FormField::SshConnection,
            FormField::SshConnection => FormField::DbPort,
            FormField::DbPort => FormField::DbName,
            FormField::DbName => FormField::DbUser,
            FormField::DbUser => FormField::DbPass,
            FormField::DbPass => FormField::Name,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            FormField::Name => FormField::DbPass,
            FormField::SshConnection => FormField::Name,
            FormField::DbPort => FormField::SshConnection,
            FormField::DbName => FormField::DbPort,
            FormField::DbUser => FormField::DbName,
            FormField::DbPass => FormField::DbUser,
        }
    }
}

pub struct App {
    pub config: AppConfig,
    pub selected_project_idx: usize,
    pub active_pane: ActivePane,
    pub mode: AppMode,

    // Form inputs
    pub input_name: Input,
    pub input_ssh: Input,
    pub input_port: Input,
    pub input_dbname: Input,
    pub input_dbuser: Input,
    pub input_dbpass: Input,
    pub active_field: FormField,

    pub is_new_project: bool,
    pub status_message: String,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub running_process: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let mut config = AppConfig::load().unwrap_or_default();
        config
            .projects
            .sort_by(|a, b| b.last_opened.cmp(&a.last_opened));

        let mut app = Self {
            config,
            selected_project_idx: 0,
            active_pane: ActivePane::ProjectsList,
            mode: AppMode::Normal,

            input_name: Input::default(),
            input_ssh: Input::default(),
            input_port: Input::default(),
            input_dbname: Input::default(),
            input_dbuser: Input::default(),
            input_dbpass: Input::default(),
            active_field: FormField::Name,

            is_new_project: false,
            status_message: String::from(
                "Ready. Press 'n' for New Project, 'Enter' to launch selected.",
            ),
            logs: Arc::new(Mutex::new(Vec::new())),
            running_process: false,
        };

        app.load_selected_into_form();
        Ok(app)
    }

    pub fn add_log(&self, msg: String) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(msg);
        }
    }

    pub fn load_selected_into_form(&mut self) {
        if let Some(proj) = self.config.projects.get(self.selected_project_idx) {
            self.input_name = Input::from(proj.name.clone());
            self.input_ssh = Input::from(proj.ssh_connection.clone());
            self.input_port = Input::from(proj.db_port.clone());
            self.input_dbname = Input::from(proj.db_name.clone());
            self.input_dbuser = Input::from(proj.db_user.clone());
            let pass = proj.get_password().unwrap_or_default();
            self.input_dbpass = Input::from(pass);
            self.is_new_project = false;
        } else {
            self.reset_form();
            self.is_new_project = true;
        }
    }

    pub fn reset_form(&mut self) {
        self.input_name = Input::default();
        self.input_ssh = Input::default();
        self.input_port = Input::from("5432");
        self.input_dbname = Input::default();
        self.input_dbuser = Input::default();
        self.input_dbpass = Input::default();
        self.active_field = FormField::Name;
        self.is_new_project = true;
    }

    pub fn save_form_to_project(&mut self) -> Result<()> {
        let ssh = self.input_ssh.value().to_string();
        let port = self.input_port.value().to_string();
        let dbname = self.input_dbname.value().to_string();
        let dbuser = self.input_dbuser.value().to_string();
        let dbpass = self.input_dbpass.value().to_string();

        let mut name = self.input_name.value().trim().to_string();
        if name.is_empty() {
            name = format!("{}@{}", dbname, ssh);
        }

        let proj = ProjectConfig {
            name: name.clone(),
            ssh_connection: ssh,
            db_port: if port.is_empty() {
                "5432".to_string()
            } else {
                port
            },
            db_name: dbname,
            db_user: dbuser,
            last_opened: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        };

        proj.save_password(&dbpass)?;

        if self.is_new_project {
            self.config.projects.push(proj);
            self.selected_project_idx = self.config.projects.len() - 1;
        } else if let Some(existing) = self.config.projects.get_mut(self.selected_project_idx) {
            *existing = proj;
        }

        self.config.save()?;
        self.load_selected_into_form();
        self.status_message = format!("Project '{}' saved successfully!", name);
        Ok(())
    }

    pub fn delete_selected_project(&mut self) -> Result<()> {
        if !self.config.projects.is_empty()
            && self.selected_project_idx < self.config.projects.len()
        {
            let removed = self.config.projects.remove(self.selected_project_idx);
            self.config.save()?;
            if self.selected_project_idx >= self.config.projects.len()
                && self.selected_project_idx > 0
            {
                self.selected_project_idx -= 1;
            }
            self.load_selected_into_form();
            self.status_message = format!("Deleted project '{}'", removed.name);
        }
        Ok(())
    }
}
