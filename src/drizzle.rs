use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

pub fn check_dependencies() -> Result<()> {
    which::which("npm")
        .context("Node.js 'npm' is not installed or not in PATH. Please install Node.js.")?;
    which::which("npx")
        .context("Node.js 'npx' is not installed or not in PATH. Please install Node.js.")?;
    Ok(())
}

fn log(logs: &Arc<Mutex<Vec<String>>>, msg: String) {
    if let Ok(mut l) = logs.lock() {
        l.push(msg);
    }
}

fn sanitize_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("project");
    }
    out
}

fn get_workspace_dir(project_name: &str) -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "dbstudio", "pg-studio")
        .context("Could not determine project directories")?;
    let data_dir = proj_dirs
        .data_dir()
        .join("workspace")
        .join(sanitize_name(project_name));
    if !data_dir.exists() {
        fs::create_dir_all(&data_dir).context("Failed to create workspace directory")?;
    }
    Ok(data_dir)
}

fn init_workspace(workspace_dir: &Path, logs: &Arc<Mutex<Vec<String>>>) -> Result<()> {
    let package_json = workspace_dir.join("package.json");

    if !package_json.exists() {
        log(
            logs,
            "Initializing workspace and installing Drizzle dependencies (this may take a minute)..."
                .to_string(),
        );

        let output = Command::new("npm")
            .arg("init")
            .arg("-y")
            .current_dir(workspace_dir)
            .stdin(Stdio::null())
            .output()
            .context("Failed to execute npm init")?;

        stream_output(logs, &output);

        if !output.status.success() {
            return Err(anyhow!("npm init failed"));
        }

        let output = Command::new("npm")
            .arg("install")
            .arg("drizzle-kit")
            .arg("drizzle-orm")
            .arg("pg")
            .current_dir(workspace_dir)
            .stdin(Stdio::null())
            .output()
            .context("Failed to execute npm install")?;

        stream_output(logs, &output);

        if !output.status.success() {
            return Err(anyhow!("npm install failed"));
        }
    }

    Ok(())
}

fn stream_output(logs: &Arc<Mutex<Vec<String>>>, output: &std::process::Output) {
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.trim().is_empty() {
            log(logs, line.to_string());
        }
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        if !line.trim().is_empty() {
            log(logs, line.to_string());
        }
    }
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

/// Initializes the per-project workspace, writes config, pulls the schema and
/// sanitizes it. Streams all output into the provided log buffer.
pub fn prepare_workspace(
    project_name: &str,
    db_url: &str,
    logs: &Arc<Mutex<Vec<String>>>,
) -> Result<PathBuf> {
    let workspace_dir = get_workspace_dir(project_name)?;

    init_workspace(&workspace_dir, logs)?;
    write_drizzle_config(&workspace_dir)?;

    log(logs, "Pulling database schema...".to_string());
    let output = Command::new("npx")
        .arg("drizzle-kit")
        .arg("pull")
        .current_dir(&workspace_dir)
        .env("DATABASE_URL", db_url)
        .output()
        .context("Failed to execute drizzle-kit pull")?;

    if !output.stdout.is_empty() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if !line.trim().is_empty() {
                log(logs, line.to_string());
            }
        }
    }
    if !output.stderr.is_empty() {
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if !line.trim().is_empty() {
                log(logs, line.to_string());
            }
        }
    }

    if !output.status.success() {
        return Err(anyhow!(
            "drizzle-kit pull failed. Check your database credentials and connection."
        ));
    }

    // --- FIX: Sanitize drizzle generated schema to remove broken SQL defaults ---
    let schema_path = workspace_dir.join("drizzle").join("schema.ts");
    if schema_path.exists() {
        let schema = fs::read_to_string(&schema_path)
            .context("Failed to read schema.ts for sanitization")?;

        let mut sanitized = false;
        let mut new_schema = String::new();

        for line in schema.lines() {
            let mut current_line = line.to_string();

            if current_line.contains(".default(')") {
                current_line = current_line.replace(".default(')", ".default('')");
                sanitized = true;
            }
            if current_line.contains(".default(\")") {
                current_line = current_line.replace(".default(\")", ".default(\"\")");
                sanitized = true;
            }

            if current_line.contains(".default(")
                && (current_line.contains("\\'") || current_line.contains("::"))
                && let Some(start_idx) = current_line.find(".default(")
            {
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
            new_schema.push_str(&current_line);
            new_schema.push('\n');
        }

        if new_schema.contains("unknown(") {
            new_schema = new_schema.replace("unknown(", "text(");
            sanitized = true;
        }

        if sanitized {
            log(
                logs,
                "Sanitizing invalid default values in generated schema...".to_string(),
            );
            fs::write(&schema_path, new_schema).context("Failed to write sanitized schema.ts")?;
        }
    }

    Ok(workspace_dir)
}

/// Spawns `drizzle-kit studio` as a background process bound to a specific port,
/// with stdout/stderr piped for streaming.
pub fn spawn_studio(workspace_dir: &Path, db_url: &str, port: u16) -> Result<Child> {
    let mut cmd = Command::new("npx");
    cmd.arg("drizzle-kit")
        .arg("studio")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(workspace_dir)
        .env("DATABASE_URL", db_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("Failed to spawn drizzle-kit studio")
}

/// Extracts the first `https://...` URL from a line of studio output (e.g. the
/// `local.drizzle.studio` tunnel URL).
pub fn extract_tunnel_url(line: &str) -> Option<String> {
    let idx = line.find("https://")?;
    let rest = &line[idx..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let mut url = rest[..end].to_string();
    while url.ends_with('.')
        || url.ends_with(',')
        || url.ends_with(')')
        || url.ends_with(']')
        || url.ends_with('}')
    {
        url.pop();
    }
    Some(url)
}
