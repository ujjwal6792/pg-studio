use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode};
use pg_studio::{
    app::{ActivePane, App, AppMode, ConfirmationAction, FormField},
    drizzle::{check_dependencies, run_drizzle_studio},
    ssh::establish_tunnel,
    tui::Tui,
    ui::draw,
    updater::{check_for_update, update_cli},
};
use std::time::Duration;
use tui_input::backend::crossterm::EventHandler;

#[derive(Parser)]
#[command(
    name = "pg-studio",
    about = "A CLI tool to introspect a remote Postgres database via SSH tunnel and launch Drizzle Studio",
    disable_version_flag = true
)]
struct Cli {
    /// Check for and install the latest release from GitHub, then exit.
    #[arg(short, long)]
    update: bool,

    /// Check for the latest release without installing it.
    #[arg(short, long)]
    check: bool,

    /// Print version.
    #[arg(short = 'v', long = "version", visible_short_alias = 'V')]
    version: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("pg-studio {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if cli.update {
        match update_cli() {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("Self-update error: {:#}", e);
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    if cli.check {
        match check_for_update() {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("Update check error: {:#}", e);
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let mut app = App::new()?;
    let mut tui = Tui::new()?;
    tui.enter()?;

    let res = run_app(&mut tui, &mut app);

    tui.exit()?;

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    Ok(())
}

fn run_app(tui: &mut Tui, app: &mut App) -> Result<()> {
    loop {
        tui.terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match app.mode {
                    AppMode::Normal => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.confirm_action = Some(ConfirmationAction::Quit);
                            app.mode = AppMode::ConfirmDialog;
                        }
                        KeyCode::Tab => {
                            app.active_pane = match app.active_pane {
                                ActivePane::ProjectsList => ActivePane::ProjectForm,
                                ActivePane::ProjectForm => ActivePane::Logs,
                                ActivePane::Logs => ActivePane::ProjectsList,
                            };
                        }
                        KeyCode::Char('n') => {
                            app.reset_form();
                            app.active_pane = ActivePane::ProjectForm;
                            app.mode = AppMode::EditingForm;
                        }
                        KeyCode::Char('e') => {
                            if !app.config.projects.is_empty() {
                                app.prepare_edit_mode();
                            }
                        }
                        KeyCode::Char('d') | KeyCode::Backspace => {
                            if app.active_pane == ActivePane::ProjectsList
                                && !app.config.projects.is_empty()
                            {
                                app.confirm_action = Some(ConfirmationAction::DeleteProject);
                                app.mode = AppMode::ConfirmDialog;
                            }
                        }
                        KeyCode::Char('u') => {
                            app.add_log("Checking GitHub Releases for updates...".to_string());
                            match update_cli() {
                                Ok(msg) => app.add_log(msg),
                                Err(e) => app.add_log(format!("Self-update error: {:#}", e)),
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.active_pane == ActivePane::ProjectsList
                                && app.selected_project_idx > 0
                            {
                                app.selected_project_idx -= 1;
                                app.load_selected_into_form();
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.active_pane == ActivePane::ProjectsList
                                && !app.config.projects.is_empty()
                                && app.selected_project_idx < app.config.projects.len() - 1
                            {
                                app.selected_project_idx += 1;
                                app.load_selected_into_form();
                            }
                        }
                        KeyCode::Enter => {
                            if let Err(e) = launch_project(tui, app) {
                                app.add_log(format!("Execution error: {}", e));
                            }
                        }
                        _ => {}
                    },

                    AppMode::EditingForm => match key.code {
                        KeyCode::Esc => {
                            app.confirm_action = Some(ConfirmationAction::CancelEdit);
                            app.mode = AppMode::ConfirmDialog;
                        }
                        KeyCode::Up | KeyCode::BackTab => {
                            app.active_field = app.active_field.prev();
                        }
                        KeyCode::Down | KeyCode::Tab => {
                            app.active_field = app.active_field.next();
                        }
                        KeyCode::Enter => {
                            if app.save_form_to_project().is_ok() {
                                app.mode = AppMode::Normal;
                            }
                        }
                        _ => {
                            let input_req = match app.active_field {
                                FormField::Name => &mut app.input_name,
                                FormField::SshConnection => &mut app.input_ssh,
                                FormField::DbPort => &mut app.input_port,
                                FormField::DbName => &mut app.input_dbname,
                                FormField::DbUser => &mut app.input_dbuser,
                                FormField::DbPass => &mut app.input_dbpass,
                            };
                            input_req.handle_event(&Event::Key(key));
                        }
                    },

                    AppMode::ConfirmDialog => match key.code {
                        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                            match app.confirm_action {
                                Some(ConfirmationAction::Quit) => return Ok(()),
                                Some(ConfirmationAction::DeleteProject) => {
                                    app.delete_selected_project()?;
                                    app.confirm_action = None;
                                    app.mode = AppMode::Normal;
                                }
                                Some(ConfirmationAction::CancelEdit) => {
                                    app.load_selected_into_form();
                                    app.confirm_action = None;
                                    app.mode = AppMode::Normal;
                                }
                                None => app.mode = AppMode::Normal,
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.confirm_action = None;
                            app.mode = if app.active_pane == ActivePane::ProjectForm {
                                AppMode::EditingForm
                            } else {
                                AppMode::Normal
                            };
                        }
                        _ => {}
                    },

                    AppMode::Running => {
                        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                            app.mode = AppMode::Normal;
                        }
                    }
                }
            }
        }
    }
}

fn launch_project(tui: &mut Tui, app: &mut App) -> Result<()> {
    if app.config.projects.is_empty() {
        app.add_log("No project selected to launch.".to_string());
        return Ok(());
    }

    let proj = app.config.projects[app.selected_project_idx].clone();
    let dbpass = proj.get_password().unwrap_or_default();

    app.add_log(format!("Starting project '{}'...", proj.name));

    // Exit TUI raw mode for studio output
    tui.exit()?;

    println!("\n=== Starting {} ===", proj.name);
    if let Err(e) = check_dependencies() {
        eprintln!("Dependency check failed: {}", e);
    } else {
        match establish_tunnel(&proj.ssh_connection, &proj.db_port) {
            Ok(tunnel) => {
                println!("SSH Tunnel established on local port {}", tunnel.local_port);
                if let Err(e) =
                    run_drizzle_studio(tunnel.local_port, &proj.db_name, &proj.db_user, &dbpass)
                {
                    eprintln!("Drizzle Studio error: {}", e);
                }
            }
            Err(e) => {
                eprintln!("SSH Tunnel error: {}", e);
            }
        }
    }

    println!("\nPress Enter to return to pg-studio TUI...");
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);

    // Re-enter TUI
    tui.enter()?;

    // Update last_opened timestamp
    if let Some(p) = app.config.projects.get_mut(app.selected_project_idx) {
        p.last_opened = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
    }
    app.config.save()?;

    Ok(())
}
