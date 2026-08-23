use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use pg_studio::{
    app::{ActivePane, App, AppMode, ConfirmationAction, DetailsTab, FormField, HelpAction},
    backup,
    config::{AppConfig, ConnectionType, ProjectConfig},
    open::{copy_to_clipboard, open_url},
    theme,
    tui::Tui,
    ui::{content_areas, draw},
    updater::{check_for_update, update_cli},
};
use ratatui::layout::Rect;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tui_input::backend::crossterm::EventHandler;

/// Two Ctrl+C presses within this window quit the app.
const DOUBLE_PRESS_WINDOW: Duration = Duration::from_secs(2);

const EXAMPLES: &str = "\
Projects:
  pg-studio                          launch the interactive TUI
  pg-studio list                     show configured projects and their status
  pg-studio new --type ssh --ssh ubuntu@host -d app -u admin --password-stdin
  pg-studio new --type local -d postgres -u postgres
  pg-studio remove my-project        delete (asks to confirm; -y skips)
  pg-studio test my-project          check reachability without launching

Studio:
  pg-studio start my-project         launch detached, print URL when ready
  pg-studio start my-project --open  ...and open the browser
  pg-studio url my-project           print the running studio URL
  pg-studio stop my-project          stop a studio (\"all\" stops everything)

Backups:
  pg-studio backup [FILE]            password-free backup of all projects
  pg-studio restore FILE             import projects from a backup
  pg-studio dump my-project          database dump via pg_dump
";

#[derive(Parser)]
#[command(
    name = "pg-studio",
    about = "Manage Postgres projects and launch Drizzle Studio - interactive TUI by default, full CLI for everything else.",
    after_help = EXAMPLES,
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
    /// Dump a project's database with pg_dump (needs the PostgreSQL client tools).
    Dump {
        /// Project name
        project: String,
        /// Destination file (default: ~/Downloads/<project>-<timestamp>.dump)
        #[arg(short = 'o', long = "out")]
        out: Option<PathBuf>,
        /// Format override: "custom" or "sql" (default: from extension, else custom)
        #[arg(long)]
        format: Option<String>,
        /// If pg_dump is missing, open an installer terminal via the detected
        /// package manager instead of failing.
        #[arg(long)]
        install: bool,
    },
    /// List configured projects.
    List,
    /// Create a project non-interactively.
    New {
        /// Project name (default: derived from database and host)
        name: Option<String>,
        /// Connection type: ssh, url, or local
        #[arg(long)]
        r#type: Option<String>,
        /// SSH connection string (for --type ssh)
        #[arg(long)]
        ssh: Option<String>,
        /// Full postgres:// URL (for --type url)
        #[arg(long)]
        url: Option<String>,
        /// Database host (url/local types; default localhost for local)
        #[arg(long)]
        host: Option<String>,
        /// Database port (default 5432)
        #[arg(long)]
        port: Option<String>,
        /// Database name
        #[arg(short = 'd', long)]
        db: Option<String>,
        /// Database user
        #[arg(short = 'u', long)]
        user: Option<String>,
        /// Read the password from stdin (one line)
        #[arg(long)]
        password_stdin: bool,
    },
    /// Delete a project from the config.
    Remove {
        /// Project name
        name: String,
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Test whether a project is reachable.
    Test {
        /// Project name
        name: String,
    },
    /// Print the running studio URL for a project.
    Url {
        /// Project name
        name: String,
        /// Open it in the browser instead of printing
        #[arg(long)]
        open: bool,
        /// Copy it to the clipboard instead of printing
        #[arg(long)]
        copy: bool,
    },
    /// Launch a project's studio detached; prints the URL when ready.
    Start {
        /// Project name
        name: String,
        /// Open the browser once the studio is ready
        #[arg(long)]
        open: bool,
        /// Seconds to wait for the studio to become ready
        #[arg(long, default_value_t = 90)]
        timeout_secs: u64,
    },
    /// Stop a project's studio, or all of them with "all".
    Stop {
        /// Project name, or "all"
        name: String,
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
        Commands::Dump {
            project,
            out,
            format,
            install,
        } => {
            run_dump_command(project, out, format, install)?;
        }
        Commands::List => run_list_command()?,
        Commands::New {
            name,
            r#type,
            ssh,
            url,
            host,
            port,
            db,
            user,
            password_stdin,
        } => {
            run_new_command(name, r#type, ssh, url, host, port, db, user, password_stdin)?;
        }
        Commands::Remove { name, yes } => run_remove_command(name, yes)?,
        Commands::Test { name } => run_test_command(name)?,
        Commands::Url { name, open, copy } => run_url_command(name, open, copy)?,
        Commands::Start {
            name,
            open,
            timeout_secs,
        } => run_start_command(name, open, timeout_secs)?,
        Commands::Stop { name } => run_stop_command(name)?,
    }
    Ok(())
}

fn run_dump_command(
    project: String,
    out: Option<PathBuf>,
    format: Option<String>,
    install: bool,
) -> Result<()> {
    // Offer a guided install when pg_dump is missing.
    if pg_studio::dbbackup::find_pg_dump().is_none() {
        match pg_studio::installer::detect_package_manager() {
            Some(pm) if install => {
                let script = pg_studio::installer::open_installer_terminal(pm)?;
                println!(
                    "Installer opened in a new terminal window (script: {}).",
                    script.display()
                );
                println!("Complete it there, then rerun this command.");
                return Ok(());
            }
            Some(pm) => {
                eprintln!(
                    "pg_dump is not installed. Install via {} with:\n  {}\nOr rerun with --install to open an installer terminal.",
                    pm.name(),
                    pg_studio::installer::suggest_command(pm)
                );
                std::process::exit(1);
            }
            None => {
                eprintln!(
                    "pg_dump not found and no supported package manager detected. Install the PostgreSQL client tools manually, e.g. brew install libpq."
                );
                std::process::exit(1);
            }
        }
    }
    let config = AppConfig::load()?;
    let proj = config
        .projects
        .iter()
        .find(|p| p.name == project)
        .or_else(|| {
            config
                .projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(&project))
        })
        .cloned();
    let Some(proj) = proj else {
        eprintln!(
            "No project named '{project}'. Projects: {}",
            if config.projects.is_empty() {
                "(none)".into()
            } else {
                config
                    .projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        std::process::exit(1);
    };

    let format = match format.as_deref() {
        Some("custom") | None => match &out {
            Some(p) => pg_studio::dbbackup::DumpFormat::from_path(p),
            None => pg_studio::dbbackup::DumpFormat::Custom,
        },
        Some("sql") => pg_studio::dbbackup::DumpFormat::Plain,
        Some(other) => {
            eprintln!("Unknown format '{other}' (expected \"custom\" or \"sql\").");
            std::process::exit(1);
        }
    };
    let path = match out {
        Some(p) => p,
        None => pg_studio::dbbackup::default_dump_path(&proj.name, format)?,
    };

    println!("Dumping '{}' to {} ...", proj.name, path.display());
    let pid_sink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let started = Instant::now();
    match pg_studio::dbbackup::run_dump(
        &proj,
        path.clone(),
        format,
        pid_sink,
        std::sync::Arc::new(move |line: String| eprintln!("{line}")),
    ) {
        Ok((size, _guard)) => println!(
            "Done in {:.1}s: {} ({})",
            started.elapsed().as_secs_f32(),
            path.display(),
            pg_studio::dbbackup::human_size(size)
        ),
        Err(e) => {
            eprintln!("Dump failed: {:#}", e);
            std::process::exit(1);
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
                do_update(app);
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
            KeyCode::Char('x') => do_export(app),
            KeyCode::Char('i') => do_import(app),
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
                Some(ConfirmationAction::MissingPgDump) => {
                    let pm = app.pending_pkg_manager.take();
                    app.confirm_action = None;
                    app.mode = AppMode::Normal;
                    if let Some(pm) = pm {
                        match pg_studio::installer::open_installer_terminal(pm) {
                            Ok(_) => {
                                app.add_log(format!(
                                    "Installer opened in a new terminal window ({}).",
                                    pg_studio::installer::suggest_command(pm)
                                ));
                                app.add_log(
                                    "Complete the install there, then press b and retry the dump."
                                        .to_string(),
                                );
                            }
                            Err(e) => {
                                app.add_log(format!("Could not open installer terminal: {:#}", e))
                            }
                        }
                    }
                }
                None => app.mode = AppMode::Normal,
            },
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                let was_cancel_edit = app.confirm_action == Some(ConfirmationAction::CancelEdit);
                app.confirm_action = None;
                app.pending_pkg_manager = None;
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
            KeyCode::Up | KeyCode::Char('k') => app.help_move(-1),
            KeyCode::Down | KeyCode::Char('j') => app.help_move(1),
            KeyCode::PageUp => app.help_move(-5),
            KeyCode::PageDown => app.help_move(5),
            KeyCode::Enter => {
                let action = app.selected_help_action();
                app.mode = AppMode::Normal;
                if let Some(action) = action
                    && action != HelpAction::CloseHelp
                {
                    return execute_help_action(app, action);
                }
            }
            _ => {}
        },
    }
    Ok(false)
}

/// Runs the action bound to a keybindings-screen entry.
fn execute_help_action(app: &mut App, action: HelpAction) -> Result<bool> {
    use HelpAction::*;
    match action {
        Quit => {
            app.confirm_action = Some(ConfirmationAction::Quit);
            app.mode = AppMode::ConfirmDialog;
        }
        FocusProjects => app.active_pane = ActivePane::ProjectsList,
        FocusDetails => app.active_pane = ActivePane::Details,
        FlipPane => {
            app.active_pane = if app.active_pane == ActivePane::ProjectsList {
                ActivePane::Details
            } else {
                ActivePane::ProjectsList
            };
        }
        TabsForward => app.cycle_details_tab(true),
        TabsBack => app.cycle_details_tab(false),
        MoveUp => app.move_selection(-1),
        MoveDown => app.move_selection(1),
        NewProject => {
            app.reset_form();
            app.active_pane = ActivePane::Details;
            app.details_tab = DetailsTab::Config;
            app.mode = AppMode::EditingForm;
        }
        EditProject => {
            if !app.config.projects.is_empty() {
                app.prepare_edit_mode();
            }
        }
        DuplicateProject => app.duplicate_selected_project(),
        DeleteProject => {
            if app.active_pane == ActivePane::ProjectsList && !app.config.projects.is_empty() {
                app.confirm_action = Some(ConfirmationAction::DeleteProject);
                app.mode = AppMode::ConfirmDialog;
            }
        }
        Filter => {
            app.active_pane = ActivePane::ProjectsList;
            app.mode = AppMode::Filtering;
        }
        TestConn => app.test_selected_connection(),
        BackupMenu => app.open_backup_menu(),
        StopProject => app.stop_selected_project(),
        OpenUrl => app.open_selected_url(),
        CopyUrl => app.copy_selected_url(),
        Connect => {
            if app.active_pane == ActivePane::ProjectsList {
                app.focus_details_for_launch();
            } else {
                app.start_selected_project(true);
            }
        }
        RunNoBrowser => {
            if app.active_pane == ActivePane::ProjectsList {
                app.focus_details_for_launch();
            } else {
                app.start_selected_project(false);
            }
        }
        ExportProjects => do_export(app),
        ImportProjects => do_import(app),
        UpdateApp => do_update(app),
        ScrollLogsUp => app.scroll_logs(-10),
        ScrollLogsDown => app.scroll_logs(10),
        CloseHelp => app.mode = AppMode::Normal,
    }
    Ok(false)
}

fn do_export(app: &mut App) {
    match app.export_projects() {
        Ok(path) => app.add_log(format!(
            "Exported {} projects to {} (passwords stay in your keychain).",
            app.config.projects.len(),
            path.display()
        )),
        Err(e) => app.add_log(format!("Export failed: {:#}", e)),
    }
}

fn do_import(app: &mut App) {
    match app.import_projects() {
        Ok((imported, skipped)) if imported + skipped == 0 => {
            app.add_log("Nothing to import: export file is empty or missing.".to_string())
        }
        Ok((imported, skipped)) => app.add_log(format!(
            "Imported {imported} project(s), skipped {skipped} existing."
        )),
        Err(e) => app.add_log(format!("Import failed: {:#}", e)),
    }
}

fn do_update(app: &mut App) {
    app.add_log("Checking GitHub Releases for updates...".to_string());
    match update_cli() {
        Ok(msg) => app.add_log(msg),
        Err(e) => app.add_log(format!("Self-update error: {:#}", e)),
    }
}

fn find_project_index(config: &AppConfig, name: &str) -> Option<usize> {
    config
        .projects
        .iter()
        .position(|p| p.name == name)
        .or_else(|| {
            config
                .projects
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(name))
        })
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn run_list_command() -> Result<()> {
    let config = AppConfig::load()?;
    if config.projects.is_empty() {
        println!("No projects configured. Launch the TUI and press 'n' to add one.");
        return Ok(());
    }
    let running: Vec<String> = pg_studio::persist::load()
        .into_iter()
        .map(|s| s.project_name)
        .collect();
    println!(
        "{:<26} {:<5} {:<38} {:<16} STATUS",
        "NAME", "TYPE", "TARGET", "DATABASE"
    );
    for p in &config.projects {
        let ty = match p.connection_type {
            ConnectionType::Ssh => "ssh",
            ConnectionType::Url => "url",
            ConnectionType::Local => "local",
        };
        let raw_target: String = match p.connection_type {
            ConnectionType::Ssh => p.ssh_connection.clone(),
            ConnectionType::Url => {
                if p.db_url.is_empty() {
                    p.db_host.clone()
                } else {
                    p.db_url.clone()
                }
            }
            ConnectionType::Local => {
                if p.db_host.is_empty() {
                    "localhost".to_string()
                } else {
                    p.db_host.clone()
                }
            }
        };
        let (mut target, _) = ProjectConfig::redact_url_password(&raw_target);
        if target.is_empty() {
            target = "-".to_string();
        }
        let status = if running.contains(&p.name) {
            "running"
        } else {
            "-"
        };
        println!(
            "{:<26} {:<5} {:<38} {:<16} {}",
            trunc(&p.name, 24),
            ty,
            trunc(&target, 36),
            trunc(&p.db_name, 14),
            status
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_new_command(
    name: Option<String>,
    conn_type: Option<String>,
    ssh: Option<String>,
    url: Option<String>,
    host: Option<String>,
    port: Option<String>,
    db: Option<String>,
    user: Option<String>,
    password_stdin: bool,
) -> Result<()> {
    let fail = |msg: &str| -> ! {
        eprintln!("{msg}");
        std::process::exit(1);
    };

    let ct: ConnectionType = match &conn_type {
        Some(t) => match t.parse() {
            Ok(ct) => ct,
            Err(e) => fail(&format!("{e:#}")),
        },
        None => fail("Missing --type (ssh, url or local)."),
    };
    let ssh = ssh.unwrap_or_default();
    let url = url.unwrap_or_default();
    let host = host.unwrap_or_default();
    let dbname = match db {
        Some(d) if !d.trim().is_empty() => d.trim().to_string(),
        _ => fail("Missing --db <database-name>."),
    };
    let dbuser = user.unwrap_or_default();

    match ct {
        ConnectionType::Ssh if ssh.trim().is_empty() => {
            fail("--type ssh requires --ssh <user@host>.")
        }
        ConnectionType::Url if url.trim().is_empty() && host.trim().is_empty() => {
            fail("--type url requires --url <postgres://...> or --host <host>.")
        }
        _ => {}
    }

    let derived = ProjectConfig::derive_default_name(ct, &ssh, &host, &dbname);
    let proj = ProjectConfig {
        name: name.filter(|n| !n.trim().is_empty()).unwrap_or(derived),
        connection_type: ct,
        ssh_connection: ssh,
        db_url: url,
        db_host: host,
        db_port: port.unwrap_or_default(),
        db_name: dbname,
        db_user: dbuser,
        last_opened: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
    };

    let mut config = AppConfig::load()?;
    if config.projects.iter().any(|p| p.name == proj.name) {
        fail(&format!("A project named '{}' already exists.", proj.name));
    }
    if password_stdin {
        print!("Password: ");
        std::io::stdout().flush()?;
        let mut pw = String::new();
        std::io::stdin().read_line(&mut pw)?;
        let pw = pw.trim_end_matches(['\r', '\n']);
        if !pw.is_empty() {
            proj.save_password(pw)?;
        }
    }
    let saved_name = proj.name.clone();
    config.projects.push(proj);
    config.save()?;
    println!("Created project '{saved_name}'.");
    Ok(())
}

fn run_remove_command(name: String, yes: bool) -> Result<()> {
    let mut app = App::new()?;
    let Some(idx) = find_project_index(&app.config, &name) else {
        eprintln!("No project named '{name}'.");
        std::process::exit(1);
    };
    app.selected_project_idx = idx;
    app.active_pane = ActivePane::ProjectsList;
    let display_name = app.selected_project_name();
    if !yes {
        print!("Delete '{display_name}'? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    app.delete_selected_project()?;
    println!("Deleted '{display_name}'.");
    Ok(())
}

fn run_test_command(name: String) -> Result<()> {
    let config = AppConfig::load()?;
    let Some(idx) = find_project_index(&config, &name) else {
        eprintln!("No project named '{name}'.");
        std::process::exit(1);
    };
    let proj = config.projects[idx].clone();
    print!("Testing '{}' ... ", proj.name);
    std::io::stdout().flush()?;
    match pg_studio::check::check_connection(&proj) {
        Ok(msg) => println!("OK ({msg})"),
        Err(e) => {
            eprintln!("FAILED: {:#}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}

fn run_url_command(name: String, open_browser: bool, copy: bool) -> Result<()> {
    let app = App::new()?;
    let Some(session) = app.session_for(&name) else {
        eprintln!("'{name}' is not running. Start it with: pg-studio start {name}");
        std::process::exit(1);
    };
    let url = session
        .lock()
        .ok()
        .and_then(|s| s.url().map(str::to_string));
    let Some(url) = url else {
        eprintln!("'{name}' has no URL yet.");
        std::process::exit(1);
    };
    if open_browser {
        open_url(&url)?;
        println!("Opened {url}");
    } else if copy {
        copy_to_clipboard(&url)?;
        println!("Copied {url}");
    } else {
        println!("{url}");
    }
    Ok(())
}

fn run_start_command(name: String, open_browser: bool, timeout_secs: u64) -> Result<()> {
    let mut app = App::new()?;
    let Some(idx) = find_project_index(&app.config, &name) else {
        eprintln!("No project named '{name}'.");
        std::process::exit(1);
    };
    app.selected_project_idx = idx;

    // Already running? Just report the URL.
    if let Some(session) = app.session_for(&app.selected_project_name()) {
        let running = session
            .lock()
            .map(|s| matches!(s.status, pg_studio::session::SessionStatus::Running));
        if running.unwrap_or(false)
            && let Some(url) = session
                .lock()
                .ok()
                .and_then(|s| s.url().map(str::to_string))
        {
            println!("Studio already running: {url}");
            return Ok(());
        }
    }

    app.start_selected_project(open_browser);

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        app.poll_auto_open();
        if let Some(session) = app.session_for(&name) {
            let (status, error, url) = {
                let Ok(s) = session.lock() else {
                    continue;
                };
                (s.status, s.error.clone(), s.url().map(str::to_string))
            };
            use pg_studio::session::SessionStatus as S;
            match status {
                S::Running => {
                    if let Some(url) = url {
                        println!("Studio running: {url}");
                    }
                    break;
                }
                S::Error => {
                    app.shutdown();
                    eprintln!(
                        "Start failed: {}",
                        error.unwrap_or_else(|| "unknown error".into())
                    );
                    std::process::exit(1);
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            app.stop_session_for(&name);
            app.shutdown();
            eprintln!("Timed out after {timeout_secs}s waiting for '{name}' to become ready.");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Detach the running studio so it survives this process exiting.
    app.shutdown();
    Ok(())
}

fn run_stop_command(name: String) -> Result<()> {
    let mut app = App::new()?;
    if name.eq_ignore_ascii_case("all") {
        let count = app.sessions.len();
        app.stop_all_sessions();
        app.shutdown();
        println!("Stopped {count} studio(s).");
        return Ok(());
    }
    if find_project_index(&app.config, &name).is_none() {
        eprintln!("No project named '{name}'.");
        std::process::exit(1);
    }
    app.stop_jobs_for(&name);
    match app.session_for(&name) {
        Some(session) => {
            if let Ok(mut s) = session.lock() {
                s.stop();
            }
            app.shutdown();
            println!("Stopped '{name}'.");
        }
        None => {
            app.shutdown();
            println!("'{name}' is not running.");
        }
    }
    Ok(())
}
