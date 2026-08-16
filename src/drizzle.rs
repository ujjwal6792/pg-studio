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
  schema: './drizzle/schema.ts',
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

    // --- FIX: Sanitize drizzle generated schema to remove broken SQL defaults ---
    let schema_path = workspace_dir.join("drizzle").join("schema.ts");
    if schema_path.exists() {
        let schema = fs::read_to_string(&schema_path)
            .context("Failed to read schema.ts for sanitization")?;

        // drizzle-kit sometimes outputs invalid TS for default values containing escaped quotes (e.g. \'hex\') or type casts (::text)
        // We strip out the specific .default(...) blocks that look broken to prevent esbuild syntax errors,
        // using a bracket-matching approach to preserve .notNull() or .primaryKey() chaining.
        let mut sanitized = false;
        let mut new_schema = String::new();

        for line in schema.lines() {
            let mut current_line = line.to_string();

            // Fix empty string defaults corrupted by drizzle-kit (e.g. .default(') or .default("))
            if current_line.contains(".default(')") {
                current_line = current_line.replace(".default(')", ".default('')");
                sanitized = true;
            }
            if current_line.contains(".default(\")") {
                current_line = current_line.replace(".default(\")", ".default(\"\")");
                sanitized = true;
            }

            if current_line.contains(".default(") && (current_line.contains("\\'") || current_line.contains("::")) {
                if let Some(start_idx) = current_line.find(".default(") {
                    let mut open_brackets = 0;
                    let mut end_idx = start_idx;
                    for (i, c) in current_line[start_idx..].char_indices() {
                        if c == '(' {
                            open_brackets += 1;
                        } else if c == ')' {
                            open_brackets -= 1;
                            if open_brackets == 0 {
                                end_idx = start_idx + i;
                                break;
                            }
                        }
                    }
                    if open_brackets == 0 {
                        let mut fixed = String::new();
                        fixed.push_str(&current_line[..start_idx]);
                        fixed.push_str(&current_line[end_idx + 1..]);
                        new_schema.push_str(&fixed);
                        new_schema.push('\n');
                        sanitized = true;
                        continue;
                    }
                }
            }
            new_schema.push_str(&current_line);
            new_schema.push('\n');
        }

        // Fix for "ReferenceError: unknown is not defined" when drizzle-kit encounters unsupported types (e.g. bytea)
        if new_schema.contains("unknown(") {
            new_schema = new_schema.replace("unknown(", "text(");
            sanitized = true;
        }

        if sanitized {
            println!("Sanitizing invalid default values in generated schema...");
            fs::write(&schema_path, new_schema).context("Failed to write sanitized schema.ts")?;
        }
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
