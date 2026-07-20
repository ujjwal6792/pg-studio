use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn check_dependencies() -> Result<()> {
    which::which("npm")
        .context("Node.js 'npm' is not installed or not in PATH. Please install Node.js.")?;
    which::which("npx")
        .context("Node.js 'npx' is not installed or not in PATH. Please install Node.js.")?;
    Ok(())
}

fn get_workspace_dir() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "dbstudio", "pg-studio")
        .context("Could not determine project directories")?;
    let data_dir = proj_dirs.data_dir().join("workspace");
    if !data_dir.exists() {
        fs::create_dir_all(&data_dir).context("Failed to create workspace directory")?;
    }
    Ok(data_dir)
}

fn init_workspace(workspace_dir: &Path) -> Result<()> {
    let package_json = workspace_dir.join("package.json");

    if !package_json.exists() {
        println!(
            "Initializing workspace and installing Drizzle dependencies (this may take a minute)..."
        );

        let status = Command::new("npm")
            .arg("init")
            .arg("-y")
            .current_dir(workspace_dir)
            .status()
            .context("Failed to execute npm init")?;

        if !status.success() {
            return Err(anyhow!("npm init failed"));
        }

        let status = Command::new("npm")
            .arg("install")
            .arg("drizzle-kit")
            .arg("drizzle-orm")
            .arg("pg")
            .current_dir(workspace_dir)
            .status()
            .context("Failed to execute npm install")?;

        if !status.success() {
            return Err(anyhow!("npm install failed"));
        }
    }

    Ok(())
}

fn write_drizzle_config(workspace_dir: &Path) -> Result<()> {
    let config_content = r#"
import { defineConfig } from 'drizzle-kit';

export default defineConfig({
  schema: './schema.ts',
  out: './drizzle',
  dialect: 'postgresql',
  dbCredentials: {
    url: process.env.DATABASE_URL!,
  },
});
"#;
    let config_path = workspace_dir.join("drizzle.config.ts");
    fs::write(&config_path, config_content).context("Failed to write drizzle.config.ts")?;
    Ok(())
}

pub fn run_drizzle_studio(
    local_port: u16,
    db_name: &str,
    db_user: &str,
    db_pass: &str,
) -> Result<()> {
    let workspace_dir = get_workspace_dir()?;

    init_workspace(&workspace_dir)?;
    write_drizzle_config(&workspace_dir)?;

    let db_url = format!(
        "postgresql://{}:{}@localhost:{}/{}",
        db_user, db_pass, local_port, db_name
    );

    println!("Pulling database schema...");
    let pull_status = Command::new("npx")
        .arg("drizzle-kit")
        .arg("pull")
        .current_dir(&workspace_dir)
        .env("DATABASE_URL", &db_url)
        .status()
        .context("Failed to execute drizzle-kit pull")?;

    if !pull_status.success() {
        return Err(anyhow!(
            "drizzle-kit pull failed. Check your database credentials and connection."
        ));
    }

    println!("Launching Drizzle Studio...");
    let mut studio_child = Command::new("npx")
        .arg("drizzle-kit")
        .arg("studio")
        .current_dir(&workspace_dir)
        .env("DATABASE_URL", &db_url)
        .spawn()
        .context("Failed to spawn drizzle-kit studio")?;

    studio_child
        .wait()
        .context("Drizzle Studio exited with an error")?;

    Ok(())
}
