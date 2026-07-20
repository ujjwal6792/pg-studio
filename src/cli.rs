use anyhow::{Context, Result};
use inquire::{Password, PasswordDisplayMode, Text};

use crate::config::AppConfig;

#[derive(Debug, Clone)]
pub struct AppState {
    pub ssh_connection: String,
    pub remote_db_port: String,
    pub db_name: String,
    pub db_user: String,
    pub db_pass: String,
}

pub fn run_prompts(mut config: AppConfig) -> Result<(AppState, AppConfig)> {
    println!("Welcome to pg-studio!");

    let ssh_connection = Text::new("SSH Connection String (e.g. user@hostname):")
        .with_default(config.ssh_connection.as_deref().unwrap_or(""))
        .prompt()
        .context("Failed to read SSH connection string")?;

    let remote_db_port = Text::new("Remote Database Port:")
        .with_default(config.db_port.as_deref().unwrap_or("5432"))
        .prompt()
        .context("Failed to read remote database port")?;

    let db_name = Text::new("Database Name:")
        .with_default(config.db_name.as_deref().unwrap_or(""))
        .prompt()
        .context("Failed to read database name")?;

    let db_user = Text::new("Database Username:")
        .with_default(config.db_user.as_deref().unwrap_or(""))
        .prompt()
        .context("Failed to read database username")?;

    let db_pass = Password::new("Database Password:")
        .with_display_mode(PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()
        .context("Failed to read database password")?;

    // Update config with the new inputs, except password
    config.ssh_connection = Some(ssh_connection.clone());
    config.db_port = Some(remote_db_port.clone());
    config.db_name = Some(db_name.clone());
    config.db_user = Some(db_user.clone());

    Ok((
        AppState {
            ssh_connection,
            remote_db_port,
            db_name,
            db_user,
            db_pass,
        },
        config,
    ))
}
