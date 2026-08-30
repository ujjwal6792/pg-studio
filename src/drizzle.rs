use crate::config::{DbTarget, Engine};
use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Marker file recording which engine the workspace's node_modules were
/// installed for, so switching a project's engine triggers a reinstall.
const ENGINE_MARKER: &str = ".pg-studio-engine";

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

/// npm packages required per engine. `drizzle-kit`/`drizzle-orm` always; the
/// rest is the wire client drizzle-kit loads for that dialect. d1-http talks
/// to the Cloudflare REST API directly and needs no client package.
fn npm_packages(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Postgres => &["drizzle-kit", "drizzle-orm", "pg"],
        Engine::Mysql => &["drizzle-kit", "drizzle-orm", "mysql2"],
        Engine::Sqlite => &["drizzle-kit", "drizzle-orm", "better-sqlite3"],
        Engine::D1 => &["drizzle-kit", "drizzle-orm"],
        Engine::Turso => &["drizzle-kit", "drizzle-orm", "@libsql/client"],
    }
}

/// Whether `init_workspace` must run (or re-run) for `engine`: first boot has
/// no package.json, and an engine switch invalidates the installed clients.
fn workspace_needs_install(
    package_json_exists: bool,
    marker_contents: Option<&str>,
    engine: Engine,
) -> bool {
    !package_json_exists
        || marker_contents
            .map(|c| c.trim() != engine.as_str())
            .unwrap_or(true)
}

fn init_workspace(
    workspace_dir: &Path,
    engine: Engine,
    logs: &Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let package_json = workspace_dir.join("package.json");

    if workspace_needs_install(
        package_json.exists(),
        fs::read_to_string(workspace_dir.join(ENGINE_MARKER))
            .ok()
            .as_deref(),
        engine,
    ) {
        log(
            logs,
            format!(
                "Installing Drizzle dependencies ({}) - this may take a minute...",
                engine.label()
            ),
        );

        if !package_json.exists() {
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
        }

        let mut install = Command::new("npm");
        install.arg("install");
        for pkg in npm_packages(engine) {
            install.arg(pkg);
        }
        let output = install
            .current_dir(workspace_dir)
            .stdin(Stdio::null())
            .output()
            .context("Failed to execute npm install")?;

        stream_output(logs, &output);

        if !output.status.success() {
            return Err(anyhow!("npm install failed"));
        }

        fs::write(workspace_dir.join(ENGINE_MARKER), engine.as_str())
            .context("Failed to write workspace engine marker")?;
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

/// Generates a drizzle.config.ts for `engine`. All credentials are read from
/// environment variables (see `DbTarget::env_vars`) so nothing secret is
/// written to disk.
fn write_drizzle_config(workspace_dir: &Path, engine: Engine) -> Result<()> {
    let dialect_line = match engine {
        Engine::Postgres => "  dialect: 'postgresql',\n",
        Engine::Mysql => "  dialect: 'mysql',\n",
        // d1-http rides on the sqlite dialect with an explicit driver.
        Engine::Sqlite | Engine::D1 => "  dialect: 'sqlite',\n",
        Engine::Turso => "  dialect: 'turso',\n",
    };
    let driver_line = match engine {
        Engine::D1 => "  driver: 'd1-http',\n",
        _ => "",
    };
    let extensions_line = match engine {
        Engine::Postgres => "  extensionsFilters: ['postgis'],\n",
        _ => "",
    };
    let credentials_block = match engine {
        Engine::Postgres | Engine::Mysql | Engine::Sqlite => {
            "  dbCredentials: {\n    url: process.env.DATABASE_URL!,\n  },\n"
        }
        Engine::D1 => {
            "  dbCredentials: {\n    accountId: process.env.CLOUDFLARE_ACCOUNT_ID!,\n    databaseId: process.env.CLOUDFLARE_DATABASE_ID!,\n    token: process.env.CLOUDFLARE_D1_TOKEN!,\n  },\n"
        }
        Engine::Turso => {
            "  dbCredentials: {\n    url: process.env.DATABASE_URL!,\n    authToken: process.env.TURSO_AUTH_TOKEN,\n  },\n"
        }
    };
    let config_content = format!(
        "import {{ defineConfig }} from 'drizzle-kit';\n\nexport default defineConfig({{\n  schema: './drizzle/schema.ts',\n  out: './drizzle',\n{dialect_line}{driver_line}{extensions_line}{credentials_block}}});\n"
    );
    let config_path = workspace_dir.join("drizzle.config.ts");
    fs::write(&config_path, config_content).context("Failed to write drizzle.config.ts")?;
    Ok(())
}

fn apply_target_env<'a>(cmd: &'a mut Command, target: &DbTarget) -> &'a mut Command {
    for (key, value) in target.env_vars() {
        cmd.env(key, value);
    }
    cmd
}

/// Initializes the per-project workspace, writes config, pulls the schema and
/// sanitizes it. Streams all output into the provided log buffer.
pub fn prepare_workspace(
    project_name: &str,
    target: &DbTarget,
    logs: &Arc<Mutex<Vec<String>>>,
) -> Result<PathBuf> {
    let workspace_dir = get_workspace_dir(project_name)?;
    let engine = target.engine();

    init_workspace(&workspace_dir, engine, logs)?;
    write_drizzle_config(&workspace_dir, engine)?;

    log(logs, "Pulling database schema...".to_string());
    let mut pull = Command::new("npx");
    pull.arg("drizzle-kit")
        .arg("pull")
        .current_dir(&workspace_dir);
    apply_target_env(&mut pull, target);
    let output = pull
        .stdin(Stdio::null())
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

    // Sanitize generated artifacts only for dialects with known invalid output.
    if matches!(engine, Engine::Postgres | Engine::D1) {
        let schema_path = workspace_dir.join("drizzle").join("schema.ts");
        if schema_path.exists() {
            let schema = fs::read_to_string(&schema_path)
                .context("Failed to read schema.ts for sanitization")?;
            let (new_schema, changed) = if engine == Engine::D1 {
                let (schema, count) = sanitize_d1_schema(&schema);
                if count > 0 {
                    log(
                        logs,
                        format!(
                            "Removed {count} generated D1 CHECK declarations that Drizzle Studio cannot load."
                        ),
                    );
                }
                (schema, count > 0)
            } else {
                sanitize_pg_schema(&schema)
            };
            if changed {
                if engine == Engine::Postgres {
                    log(
                        logs,
                        "Sanitizing invalid default values in generated schema...".to_string(),
                    );
                }
                fs::write(&schema_path, new_schema)
                    .context("Failed to write sanitized schema.ts")?;
            }
        }
    }

    Ok(workspace_dir)
}

/// Removes Postgres-specific invalid default values from a generated
/// schema.ts. Returns `(sanitized_output, changed)`.
/// Removes Postgres-specific invalid default values from a generated schema.ts.
fn sanitize_pg_schema(schema: &str) -> (String, bool) {
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
                new_schema.push_str(&current_line[..start_idx]);
                new_schema.push_str(&current_line[end_idx + 1..]);
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
    (new_schema, sanitized)
}

fn sanitize_d1_schema(schema: &str) -> (String, usize) {
    let mut output = String::new();
    let mut removed = 0;
    for line in schema.lines() {
        if line.trim_start().starts_with("check(") {
            removed += 1;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !schema.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }
    (output, removed)
}

/// Spawns `drizzle-kit studio` as a background process bound to a specific port,
/// with stdout/stderr redirected to the given log file (so the process can
/// survive the parent and keep logging).
pub fn spawn_studio(
    workspace_dir: &Path,
    target: &DbTarget,
    port: u16,
    log_path: &Path,
) -> Result<Child> {
    let log_file = fs::File::create(log_path).context("Failed to create studio log file")?;
    let mut cmd = Command::new("npx");
    cmd.arg("drizzle-kit")
        .arg("studio")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(workspace_dir);
    apply_target_env(&mut cmd, target);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(
            log_file.try_clone().context("Failed to clone log file")?,
        ))
        .stderr(Stdio::from(log_file));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_packages_cover_every_engine() {
        for engine in Engine::ALL {
            let pkgs = npm_packages(engine);
            assert!(pkgs.contains(&"drizzle-kit"));
            assert!(pkgs.contains(&"drizzle-orm"));
        }
        assert!(npm_packages(Engine::Postgres).contains(&"pg"));
        assert!(npm_packages(Engine::Mysql).contains(&"mysql2"));
        assert!(npm_packages(Engine::Sqlite).contains(&"better-sqlite3"));
        assert!(npm_packages(Engine::Turso).contains(&"@libsql/client"));
        // d1-http uses the REST API; no client package.
        assert_eq!(npm_packages(Engine::D1).len(), 2);
    }

    #[test]
    fn workspace_reinstall_decision_tracks_engine_marker() {
        assert!(workspace_needs_install(false, None, Engine::Postgres));
        assert!(!workspace_needs_install(
            true,
            Some("postgres"),
            Engine::Postgres
        ));
        // Engine switch must trigger a reinstall.
        assert!(workspace_needs_install(
            true,
            Some("postgres"),
            Engine::Sqlite
        ));
        assert!(workspace_needs_install(
            true,
            Some("postgres\n"),
            Engine::Turso
        ));
        assert!(workspace_needs_install(true, None, Engine::D1));
    }

    fn generated_config(engine: Engine) -> String {
        let dir = std::env::temp_dir().join(format!(
            "pg-studio-cfgtest-{}-{}",
            engine.as_str(),
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        write_drizzle_config(&dir, engine).unwrap();
        let content = fs::read_to_string(dir.join("drizzle.config.ts")).unwrap();
        let _ = fs::remove_dir_all(&dir);
        content
    }

    #[test]
    fn config_templates_match_engine_dialects() {
        let pg = generated_config(Engine::Postgres);
        assert!(pg.contains("dialect: 'postgresql'"));
        assert!(pg.contains("url: process.env.DATABASE_URL!"));
        assert!(!pg.contains("driver:"));
        assert!(pg.contains("extensionsFilters: ['postgis']"));

        let my = generated_config(Engine::Mysql);
        assert!(my.contains("dialect: 'mysql'"));
        assert!(!my.contains("extensionsFilters"));

        let lite = generated_config(Engine::Sqlite);
        assert!(lite.contains("dialect: 'sqlite'"));
        assert!(lite.contains("url: process.env.DATABASE_URL!"));
        assert!(!lite.contains("extensionsFilters"));

        let d1 = generated_config(Engine::D1);
        assert!(d1.contains("dialect: 'sqlite'"));
        assert!(d1.contains("driver: 'd1-http'"));
        assert!(d1.contains("accountId: process.env.CLOUDFLARE_ACCOUNT_ID!"));
        assert!(d1.contains("token: process.env.CLOUDFLARE_D1_TOKEN!"));
        assert!(!d1.contains("extensionsFilters"));

        let turso = generated_config(Engine::Turso);
        assert!(turso.contains("dialect: 'turso'"));
        assert!(turso.contains("authToken: process.env.TURSO_AUTH_TOKEN"));
        assert!(!turso.contains("extensionsFilters"));
    }

    #[test]
    fn sanitizer_fixes_pg_defaults() {
        let (out, changed) = sanitize_pg_schema(
            "export const t = pgTable('t', {\n  a: text('a').default('::unknown'),\n});",
        );
        assert!(changed);
        assert!(!out.contains(".default("));

        let (out, changed) = sanitize_pg_schema("a: unknown('x'),");
        assert!(changed);
        assert!(out.contains("text("));
        assert!(!out.contains("unknown("));

        let (_, changed) = sanitize_pg_schema("export const clean = 1;");
        assert!(!changed);
        #[test]
        fn d1_sanitizer_removes_generated_check_declarations() {
            let input = "export const orgs = sqliteTable('orgs', {\n  id: integer('id').primaryKey()\n  check(\"orgs_check_1\", sql`id > 0`),\n  check(\"orgs_check_1\", sql`id < 100`),\n});";
            let (output, removed) = sanitize_d1_schema(input);
            assert_eq!(removed, 2);
            assert_eq!(
                output,
                "export const orgs = sqliteTable('orgs', {\n  id: integer('id').primaryKey()\n});"
            );
            assert_eq!(sanitize_d1_schema("const clean = 1;").0, "const clean = 1;");
        }
    }
}
