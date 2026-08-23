use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use pg_studio::{
    app::{ActivePane, App, AppMode, ConfirmationAction, DetailsTab, FormField},
    theme,
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

    app.theme = theme::query_terminal_theme().unwrap_or_default();

    let res = run_app(&mut tui, &mut app);

    tui.exit()?;

    app.shutdown();

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    Ok(())
}

fn run_app(tui: &mut Tui, app: &mut App) -> Result<()> {
    loop {
        app.poll_auto_open();
        tui.terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.mode {
                AppMode::Normal => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        app.confirm_action = Some(ConfirmationAction::Quit);
                        app.mode = AppMode::ConfirmDialog;
                    }
                    KeyCode::Char('?') => {
                        app.mode = AppMode::Help;
                    }
                    KeyCode::Tab => app.cycle_details_tab(true),
                    KeyCode::BackTab => app.cycle_details_tab(false),
                    KeyCode::Left | KeyCode::Char('1') => {
                        app.active_pane = ActivePane::ProjectsList;
                    }
                    KeyCode::Right | KeyCode::Char('2') => {
                        app.active_pane = ActivePane::Details;
                    }
                    KeyCode::Char('[') | KeyCode::Char(']') => {
                        app.active_pane = if app.active_pane == ActivePane::ProjectsList {
                            ActivePane::Details
                        } else {
                            ActivePane::ProjectsList
                        };
                    }
                    KeyCode::Char('}') => app.cycle_details_tab(true),
                    KeyCode::Char('{') => app.cycle_details_tab(false),
                    KeyCode::Char('n') => {
                        app.reset_form();
                        app.active_pane = ActivePane::Details;
                        app.details_tab = DetailsTab::Config;
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
                        let auto_open = !key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::SHIFT);
                        if app.active_pane == ActivePane::ProjectsList {
                            // Stage one: focus the details pane; a second press launches.
                            app.focus_details_for_launch();
                        } else if auto_open {
                            app.start_selected_project(true);
                        } else {
                            app.start_selected_project(false);
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        if app.active_pane == ActivePane::ProjectsList {
                            app.focus_details_for_launch();
                        } else {
                            app.start_selected_project(false);
                        }
                    }
                    KeyCode::Char('t') => {
                        app.test_selected_connection();
                    }
                    KeyCode::Char('s') => {
                        let name = app.selected_project_name();
                        if !name.is_empty() && app.session_for(&name).is_some() {
                            app.confirm_action = Some(ConfirmationAction::StopProject);
                            app.mode = AppMode::ConfirmDialog;
                        } else {
                            app.stop_selected_project();
                        }
                    }
                    KeyCode::Char('o') => {
                        app.open_selected_url();
                    }
                    KeyCode::Char('c') => {
                        app.copy_selected_url();
                    }
                    _ => {}
                },

                AppMode::EditingForm => match key.code {
                    KeyCode::Esc => {
                        app.confirm_action = Some(ConfirmationAction::CancelEdit);
                        app.mode = AppMode::ConfirmDialog;
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        app.active_field = app.active_field.prev(app.connection_type);
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        app.active_field = app.active_field.next(app.connection_type);
                    }
                    KeyCode::Enter => {
                        if app.active_field == FormField::ConnectionType {
                            app.toggle_connection_type();
                        } else if app.save_form_to_project().is_ok() {
                            app.mode = AppMode::Normal;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if app.active_field == FormField::ConnectionType {
                            app.toggle_connection_type();
                        }
                    }
                    _ => {
                        if let Some(input) = app.input_mut(app.active_field) {
                            input.handle_event(&Event::Key(key));
                        }
                    }
                },

                AppMode::ConfirmDialog => match key.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        match app.confirm_action {
                            Some(ConfirmationAction::Quit) => {
                                return Ok(());
                            }
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
                            Some(ConfirmationAction::StopProject) => {
                                app.stop_selected_project();
                                app.confirm_action = None;
                                app.mode = AppMode::Normal;
                            }
                            None => app.mode = AppMode::Normal,
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        let was_cancel_edit =
                            app.confirm_action == Some(ConfirmationAction::CancelEdit);
                        app.confirm_action = None;
                        app.mode = if was_cancel_edit {
                            AppMode::EditingForm
                        } else {
                            AppMode::Normal
                        };
                    }
                    _ => {}
                },

                AppMode::Help => match key.code {
                    KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                        app.mode = AppMode::Normal
                    }
                    _ => {}
                },
            }
        }
    }
}
