use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use pg_studio::{
    app::{ActivePane, App, AppMode, ConfirmationAction, DetailsTab, FormField},
    backup,
    config::AppConfig,
    theme,
    tui::Tui,
    ui::{content_areas, draw},
    updater::{check_for_update, update_cli},
};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tui_input::backend::crossterm::EventHandler;

/// Two Ctrl+C presses within this window quit the app.
const DOUBLE_PRESS_WINDOW: Duration = Duration::from_secs(2);

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

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Write a password-free backup of all projects to a JSON file.
    Backup {
        /// Destination file (default: ~/Downloads/pg-studio-backup-<timestamp>.json)
        file: Option<PathBuf>,
    },
    /// Merge projects from a backup file into your config (skips existing names).
    Restore {
        /// Backup file to read
        file: PathBuf,
    },
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

    if let Some(command) = cli.command {
        return run_command(command);
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

fn run_command(command: Commands) -> Result<()> {
    match command {
        Commands::Backup { file } => {
            let config = AppConfig::load()?;
            if config.projects.is_empty() {
                eprintln!("No projects to back up.");
                std::process::exit(1);
            }
            let path = match file {
                Some(f) => f,
                None => backup::default_backup_path()?,
            };
            backup::write_backup(&path, config.projects.clone())?;
            println!(
                "Backup written: {} ({} project(s), passwords not included)",
                path.display(),
                config.projects.len()
            );
        }
        Commands::Restore { file } => {
            let mut config = AppConfig::load()?;
            let bundle = backup::read_backup(&file)?;
            let total = bundle.projects.len();
            let (imported, skipped) = config.merge_bundle(&bundle);
            if imported > 0 {
                config.save()?;
            }
            println!(
                "Restored from {}: {imported} imported, {skipped} skipped ({total} in file).",
                file.display()
            );
        }
    }
    Ok(())
}

fn run_app(tui: &mut Tui, app: &mut App) -> Result<()> {
    let mut last_ctrl_c: Option<Instant> = None;
    loop {
        app.poll_auto_open();
        let size = tui.terminal.size()?;
        let frame_area = Rect::new(0, 0, size.width, size.height);
        tui.terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Double Ctrl+C quits from any mode, bypassing dialogs.
                    if key.code == KeyCode::Char('c')
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        let now = Instant::now();
                        if last_ctrl_c.is_some_and(|t| now.duration_since(t) < DOUBLE_PRESS_WINDOW)
                        {
                            break;
                        }
                        last_ctrl_c = Some(now);
                        app.add_log("Press Ctrl+C again to quit.".to_string());
                        continue;
                    }
                    last_ctrl_c = None;
                    if handle_key(app, key)? {
                        break;
                    }
                }
                Event::Paste(data) => {
                    // Smart paste: a full postgres:// URL fills the whole form;
                    // anything else lands in the focused field.
                    if !app.apply_pasted_text(&data)
                        && let Some(input) = app.input_mut(app.active_field)
                    {
                        for ch in data.chars() {
                            input.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
                                KeyCode::Char(ch),
                                KeyModifiers::NONE,
                            )));
                        }
                    }
                }
                Event::Mouse(mouse) => handle_mouse(app, mouse, frame_area),
                _ => {}
            }
        }
    }
    Ok(())
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, term: Rect) {
    if app.mode != AppMode::Normal && app.mode != AppMode::Filtering {
        return;
    }
    let (_, list, details, logs, _) = content_areas(term);

    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            if rect_contains(logs, mouse.column, mouse.row) {
                app.scroll_logs(if up { -10 } else { 10 });
            } else if rect_contains(list, mouse.column, mouse.row) {
                app.move_selection(if up { -1 } else { 1 });
            }
        }
        MouseEventKind::Down(_) => {
            if rect_contains(list, mouse.column, mouse.row) {
                app.active_pane = ActivePane::ProjectsList;
                select_project_at_row(app, mouse.row, list);
            } else if rect_contains(details, mouse.column, mouse.row) {
                app.active_pane = ActivePane::Details;
                select_tab_at_column(app, mouse.column, details);
            } else if rect_contains(logs, mouse.column, mouse.row) {
                app.scroll_logs(0); // clicking the logs pane snaps back to latest
            }
        }
        _ => {}
    }
}

/// Maps a click inside the projects pane to a visible row.
fn select_project_at_row(app: &mut App, row: u16, list_area: Rect) {
    let inner_y = list_area.y + 1; // top border
    let show_filter_row = app.mode == AppMode::Filtering || !app.filter.value().is_empty();
    let data_start = inner_y + u16::from(show_filter_row);
    if row < data_start {
        return; // clicked the filter row or border
    }
    let pos = (row - data_start) as usize + app.project_scroll;
    let visible = app.visible_projects();
    if let Some(&idx) = visible.get(pos)
        && idx != app.selected_project_idx
    {
        app.selected_project_idx = idx;
        app.load_selected_into_form();
    }
}

/// Maps a click inside the Details pane to one of the three sub-tab boxes.
/// Must mirror the tab widths used by `draw_details_subtabs`.
fn select_tab_at_column(app: &mut App, column: u16, details_area: Rect) {
    const TAB_WIDTHS: [(DetailsTab, u16); 3] = [
        (DetailsTab::Overview, 15),
        (DetailsTab::Config, 13),
        (DetailsTab::Process, 14),
    ];
    let mut x = details_area.x + 1; // left border
    for (tab, w) in TAB_WIDTHS {
        if column >= x && column < x + w {
            app.details_tab = tab;
            return;
        }
        x += w;
    }
}

/// Dispatches a key press. Returns `Ok(true)` when the app should exit.
fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> Result<bool> {
    match app.mode {
        AppMode::Normal => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                app.confirm_action = Some(ConfirmationAction::Quit);
                app.mode = AppMode::ConfirmDialog;
            }
            KeyCode::Char('?') => {
                app.mode = AppMode::Help;
            }
            KeyCode::Char('/') => {
                app.active_pane = ActivePane::ProjectsList;
                app.mode = AppMode::Filtering;
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                app.open_backup_menu();
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
            KeyCode::Char('p') | KeyCode::Char('P') => {
                app.duplicate_selected_project();
            }
            KeyCode::Char('d') | KeyCode::Backspace => {
                if app.active_pane == ActivePane::ProjectsList && !app.config.projects.is_empty() {
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
                app.move_selection(-1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_selection(1);
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
            KeyCode::Char('x') => match app.export_projects() {
                Ok(path) => app.add_log(format!(
                    "Exported {} projects to {} (passwords stay in your keychain).",
                    app.config.projects.len(),
                    path.display()
                )),
                Err(e) => app.add_log(format!("Export failed: {:#}", e)),
            },
            KeyCode::Char('i') => match app.import_projects() {
                Ok((imported, skipped)) if imported + skipped == 0 => {
                    app.add_log("Nothing to import: export file is empty or missing.".to_string())
                }
                Ok((imported, skipped)) => app.add_log(format!(
                    "Imported {imported} project(s), skipped {skipped} existing."
                )),
                Err(e) => app.add_log(format!("Import failed: {:#}", e)),
            },
            KeyCode::Char('t') => {
                app.test_selected_connection();
            }
            KeyCode::PageUp => app.scroll_logs(-10),
            KeyCode::PageDown => app.scroll_logs(10),
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

        AppMode::Filtering => match key.code {
            KeyCode::Esc => {
                app.filter = tui_input::Input::default();
                app.mode = AppMode::Normal;
                app.snap_selection_to_visible();
            }
            KeyCode::Enter => {
                app.mode = AppMode::Normal;
                app.snap_selection_to_visible();
            }
            KeyCode::Up | KeyCode::BackTab => app.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => app.move_selection(1),
            _ => {
                app.filter.handle_event(&Event::Key(key));
            }
        },

        AppMode::BackupMenu => match key.code {
            KeyCode::Esc => app.mode = AppMode::Normal,
            KeyCode::Up | KeyCode::BackTab => app.backup_menu_move(-1),
            KeyCode::Down | KeyCode::Tab => app.backup_menu_move(1),
            KeyCode::Enter => app.execute_backup_action(),
            _ => {
                app.input_backup_path.handle_event(&Event::Key(key));
            }
        },

        AppMode::ConfirmDialog => match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => match app.confirm_action {
                Some(ConfirmationAction::Quit) => {
                    return Ok(true); // exit run_app; shutdown persists sessions
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
            },
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                let was_cancel_edit = app.confirm_action == Some(ConfirmationAction::CancelEdit);
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
    Ok(false)
}
