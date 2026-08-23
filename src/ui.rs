use crate::app::{
    ActivePane, App, AppMode, ConfirmationAction, DetailsTab, FormField, ProjectState,
};
use crate::config::{ConnectionType, ProjectConfig};
use crate::session::SessionStatus;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

/// The app's fixed layout, shared between drawing and mouse hit-testing.
/// Returns `(header, projects_list, details, logs, footer)`.
pub fn content_areas(full: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(12),   // Content (Left: List, Right: Details)
            Constraint::Length(8), // Logs / Guidance Output
            Constraint::Length(1), // Footer / Help Bar
        ])
        .split(full);
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(main_chunks[1]);
    (
        main_chunks[0],
        content[0],
        content[1],
        main_chunks[2],
        main_chunks[3],
    )
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let (header_area, list_area, details_area, logs_area, footer_area) = content_areas(f.area());

    draw_header(f, app, header_area);
    draw_projects_list(f, app, list_area);
    draw_details(f, app, details_area);

    draw_logs(f, app, logs_area);
    draw_footer(f, app, footer_area);

    if app.mode == AppMode::ConfirmDialog
        && let Some(action) = app.confirm_action
    {
        render_confirm_popup(f, app, action);
    }
    if app.mode == AppMode::Help {
        render_help_popup(f, app);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let header = Paragraph::new("").block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.muted))
            .title(Line::from(vec![
                Span::styled(
                    " PG-STUDIO ",
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " | Remote & Hosted Postgres Drizzle Studio Launcher",
                    Style::default().fg(app.theme.dim),
                ),
            ]))
            .title(
                Line::from(vec![Span::styled(
                    format!(" v{} ", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(app.theme.info),
                )])
                .right_aligned(),
            ),
    );
    f.render_widget(header, area);
}

fn draw_projects_list(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;

    let projects_block = Block::default()
        .title(" 󰍉 Projects ")
        .borders(Borders::ALL)
        .border_style(if app.active_pane == ActivePane::ProjectsList {
            Style::default().fg(app.theme.accent)
        } else {
            Style::default().fg(app.theme.muted)
        });
    let inner = projects_block.inner(area);
    f.render_widget(projects_block, area);

    // Filter row: shown while filtering or when a filter is applied.
    let filter_text = app.filter.value().to_string();
    let show_filter_row = app.mode == AppMode::Filtering || !filter_text.is_empty();
    let rows_area = if show_filter_row {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        let cursor = if app.mode == AppMode::Filtering {
            Some(Position {
                x: chunks[0].x + 1 + filter_text.chars().count() as u16,
                y: chunks[0].y,
            })
        } else {
            None
        };
        let row = Line::from(vec![
            Span::styled("/", Style::default().fg(app.theme.accent)),
            Span::styled(filter_text.clone(), Style::default().fg(app.theme.text)),
        ]);
        f.render_widget(Paragraph::new(row), chunks[0]);
        if let Some(pos) = cursor {
            f.set_cursor_position(pos);
        }
        chunks[1]
    } else {
        inner
    };

    if app.config.projects.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No projects yet. Press 'n' to add.",
                Style::default().fg(app.theme.muted),
            )),
            rows_area,
        );
        return;
    }

    let visible = app.visible_projects();
    if visible.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No projects match the filter.",
                Style::default().fg(app.theme.muted),
            )),
            rows_area,
        );
        return;
    }

    let viewport_rows = rows_area.height.max(1) as usize;
    app.clamp_project_scroll(viewport_rows);

    let end = (app.project_scroll + viewport_rows).min(visible.len());
    let lines: Vec<Line> = visible[app.project_scroll..end]
        .iter()
        .map(|&i| {
            let p = &app.config.projects[i];
            project_row(app, i == app.selected_project_idx, inner_width, p)
        })
        .collect();

    f.render_widget(Paragraph::new(lines), rows_area);
}

fn project_row(app: &App, selected: bool, inner_width: usize, p: &ProjectConfig) -> Line<'static> {
    let state = app.project_state(&p.name);
    let focused = app.active_pane == ActivePane::ProjectsList;

    let (marker, marker_color) = match state {
        Some(ProjectState::Running) => ("●", app.theme.success),
        Some(ProjectState::Error) => ("●", app.theme.error),
        Some(ProjectState::Stopped) => ("○", app.theme.muted),
        None => ("•", app.theme.muted),
    };

    let name_color = if selected {
        app.theme.text
    } else {
        match state {
            Some(ProjectState::Running) => app.theme.success,
            Some(ProjectState::Error) => app.theme.error,
            Some(ProjectState::Stopped) => app.theme.muted,
            None => app.theme.text,
        }
    };

    let bg = if selected && focused {
        app.theme.highlight_bg
    } else if selected {
        app.theme.muted
    } else {
        Color::Reset
    };

    let icon = match p.connection_type {
        ConnectionType::Ssh => "󰒋",
        ConnectionType::Url => "󰖟",
        ConnectionType::Local => "",
    };

    let bold = if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let marker_span = Span::styled(
        format!(" {} ", marker),
        Style::default().fg(marker_color).bg(bg),
    );
    let name_text = format!("{} {} ", icon, p.name);
    let name_span = Span::styled(
        name_text,
        Style::default().fg(name_color).bg(bg).add_modifier(bold),
    );
    let pad = inner_width.saturating_sub(3 + 1 + 1 + p.name.chars().count());
    let pad_span = Span::styled(" ".repeat(pad), Style::default().bg(bg));
    Line::from(vec![marker_span, name_span, pad_span])
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.active_pane == ActivePane::Details {
        Style::default().fg(app.theme.accent)
    } else {
        Style::default().fg(app.theme.muted)
    };

    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(inner);

    draw_details_subtabs(f, app, chunks[0]);

    match app.details_tab {
        DetailsTab::Overview => draw_overview(f, app, chunks[1]),
        DetailsTab::Config => draw_config(f, app, chunks[1]),
        DetailsTab::Process => draw_process(f, app, chunks[1]),
    }
}

fn draw_details_subtabs(f: &mut Frame, app: &App, area: Rect) {
    let tabs: [(&str, DetailsTab); 3] = [
        ("󰍉 Overview", DetailsTab::Overview),
        ("󰒓 Config", DetailsTab::Config),
        ("󰐊 Process", DetailsTab::Process),
    ];
    let widths: [Constraint; 3] = [
        Constraint::Length(15),
        Constraint::Length(13),
        Constraint::Length(14),
    ];
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(widths)
        .split(area);

    for (i, (label, tab)) in tabs.iter().enumerate() {
        let active = app.details_tab == *tab;
        let (text_style, border_style) = if active {
            (
                Style::default()
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.accent),
            )
        } else {
            (
                Style::default().fg(app.theme.dim),
                Style::default().fg(app.theme.muted),
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(chunks[i]);
        f.render_widget(block, chunks[i]);
        let tab_widget =
            Paragraph::new(Span::styled(*label, text_style)).alignment(Alignment::Center);
        f.render_widget(tab_widget, inner);
    }
}

fn draw_overview(f: &mut Frame, app: &App, area: Rect) {
    if app.config.projects.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No project selected.",
                Style::default().fg(app.theme.muted),
            )),
            area,
        );
        return;
    }

    let proj = &app.config.projects[app.selected_project_idx];
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" Name              : {}", proj.name),
            Style::default().fg(app.theme.text),
        )),
        Line::from(Span::styled(
            format!(
                " Connection Type   : {}",
                match proj.connection_type {
                    ConnectionType::Ssh => "SSH Tunnel",
                    ConnectionType::Url => "Public URL",
                    ConnectionType::Local => "Local",
                }
            ),
            Style::default().fg(app.theme.text),
        )),
    ];

    match proj.connection_type {
        ConnectionType::Ssh => {
            lines.push(Line::from(Span::styled(
                format!(" SSH               : {}", proj.ssh_connection),
                Style::default().fg(app.theme.text),
            )));
            lines.push(Line::from(Span::styled(
                format!(" Remote Port       : {}", proj.db_port),
                Style::default().fg(app.theme.text),
            )));
        }
        ConnectionType::Url => {
            if !proj.db_url.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(" URL               : {}", proj.db_url),
                    Style::default().fg(app.theme.text),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" Host              : {}", proj.db_host),
                    Style::default().fg(app.theme.text),
                )));
                lines.push(Line::from(Span::styled(
                    format!(" Port              : {}", proj.db_port),
                    Style::default().fg(app.theme.text),
                )));
            }
        }
        ConnectionType::Local => {
            lines.push(Line::from(Span::styled(
                format!(" Host              : {}", proj.db_host),
                Style::default().fg(app.theme.text),
            )));
            lines.push(Line::from(Span::styled(
                format!(" Port              : {}", proj.db_port),
                Style::default().fg(app.theme.text),
            )));
        }
    }

    lines.push(Line::from(Span::styled(
        format!(" Database          : {}", proj.db_name),
        Style::default().fg(app.theme.text),
    )));
    lines.push(Line::from(Span::styled(
        format!(" User              : {}", proj.db_user),
        Style::default().fg(app.theme.text),
    )));
    lines.push(Line::from(Span::styled(
        format!(" Last Opened       : {}", app.formatted_last_opened()),
        Style::default().fg(app.theme.info),
    )));

    let running = app.is_project_running(&proj.name);
    let status_line = if running {
        Span::styled(
            " Status            : ● Running",
            Style::default()
                .fg(app.theme.success)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " Status            : ○ Stopped",
            Style::default().fg(app.theme.muted),
        )
    };
    lines.push(Line::from(status_line));

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_config(f: &mut Frame, app: &App, area: Rect) {
    let mut fields: Vec<(FormField, String, String, bool)> = vec![(
        FormField::Name,
        "Project Name".to_string(),
        app.input_name.value().to_string(),
        false,
    )];

    fields.push((
        FormField::ConnectionType,
        "Connection Type".to_string(),
        match app.connection_type {
            ConnectionType::Ssh => "SSH (Enter to cycle)".to_string(),
            ConnectionType::Url => "URL (Enter to cycle)".to_string(),
            ConnectionType::Local => "Local (Enter to cycle)".to_string(),
        },
        false,
    ));

    match app.connection_type {
        ConnectionType::Ssh => {
            fields.push((
                FormField::SshConnection,
                "SSH String".to_string(),
                app.input_ssh.value().to_string(),
                false,
            ));
        }
        ConnectionType::Url => {
            fields.push((
                FormField::DbUrl,
                "Connection URL".to_string(),
                app.input_url.value().to_string(),
                false,
            ));
            fields.push((
                FormField::DbHost,
                "Database Host".to_string(),
                app.input_host.value().to_string(),
                false,
            ));
        }
        ConnectionType::Local => {
            fields.push((
                FormField::DbHost,
                "Database Host".to_string(),
                app.input_host.value().to_string(),
                false,
            ));
        }
    }

    fields.push((
        FormField::DbPort,
        "Remote DB Port".to_string(),
        app.input_port.value().to_string(),
        false,
    ));
    fields.push((
        FormField::DbName,
        "Database Name".to_string(),
        app.input_dbname.value().to_string(),
        false,
    ));
    fields.push((
        FormField::DbUser,
        "Database User".to_string(),
        app.input_dbuser.value().to_string(),
        false,
    ));
    fields.push((
        FormField::DbPass,
        "Database Pass".to_string(),
        app.input_dbpass.value().to_string(),
        true,
    ));

    let n = fields.len();
    let mut constraints: Vec<Constraint> = fields.iter().map(|_| Constraint::Length(2)).collect();
    constraints.push(Constraint::Length(2)); // Last Opened
    constraints.push(Constraint::Length(2)); // Error
    constraints.push(Constraint::Min(0));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (idx, (field_type, label, value, is_pass)) in fields.iter().enumerate() {
        if idx >= chunks.len() {
            break;
        }
        let is_active = app.active_pane == ActivePane::Details
            && app.active_field == *field_type
            && app.mode == AppMode::EditingForm;

        let (display_val, val_style) = if *field_type == FormField::DbPort && value.is_empty() {
            if is_active {
                (
                    "5432 (default)".to_string(),
                    Style::default()
                        .fg(app.theme.text)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    "5432 (default)".to_string(),
                    Style::default().fg(app.theme.muted),
                )
            }
        } else if *is_pass {
            if value.is_empty() && !app.is_new_project && app.mode != AppMode::EditingForm {
                (
                    "•••••••• (Stored in Keychain)".to_string(),
                    Style::default().fg(app.theme.muted),
                )
            } else if value.is_empty() {
                ("".to_string(), Style::default().fg(app.theme.text))
            } else {
                (
                    "*".repeat(value.len()),
                    if is_active {
                        Style::default()
                            .fg(app.theme.text)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(app.theme.text)
                    },
                )
            }
        } else {
            (
                value.to_string(),
                if is_active {
                    Style::default()
                        .fg(app.theme.text)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text)
                },
            )
        };

        let label_style = if is_active {
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.dim)
        };

        let prefix_indicator = if is_active { " ▶ " } else { "   " };

        let line = Line::from(vec![
            Span::styled(prefix_indicator, Style::default().fg(app.theme.accent)),
            Span::styled(format!("{:<16}: ", label), label_style),
            Span::styled(display_val, val_style),
        ]);

        f.render_widget(Paragraph::new(line), chunks[idx]);

        // Show the terminal caret at the input's visual cursor position so the
        // user can see where edits (arrows, backspace) will land.
        if is_active {
            const VALUE_COL: usize = 21; // 3 (" ▶ ") + 16 (label) + 2 (": ")
            let width = chunks[idx].width as usize;
            if width > VALUE_COL + 1 {
                let avail = width - VALUE_COL;
                let cursor = app
                    .input(*field_type)
                    .map(|i| i.visual_cursor())
                    .unwrap_or(0);
                let col = VALUE_COL + cursor.min(avail - 1);
                f.set_cursor_position(Position {
                    x: chunks[idx].x + col as u16,
                    y: chunks[idx].y,
                });
            }
        }
    }

    // Render Last Opened Date
    if chunks.len() > n {
        let last_opened_str = app.formatted_last_opened();
        let date_line = Line::from(vec![
            Span::styled("   Last Opened    : ", Style::default().fg(app.theme.muted)),
            Span::styled(last_opened_str, Style::default().fg(app.theme.info)),
        ]);
        f.render_widget(Paragraph::new(date_line), chunks[n]);
    }

    // Render Error Message if present
    if let Some(err_msg) = &app.error_message
        && chunks.len() > n + 1
    {
        let err_line = Line::from(vec![
            Span::styled(
                "    Error: ",
                Style::default()
                    .fg(app.theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(err_msg, Style::default().fg(app.theme.error)),
        ]);
        f.render_widget(Paragraph::new(err_line), chunks[n + 1]);
    }
}

fn draw_process(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = vec![];

    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No running studios. Press Enter to focus a project's details, then Enter again to launch.",
            Style::default().fg(app.theme.muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " Sessions ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        let selected_name = app.selected_project_name();
        for session in &app.sessions {
            if let Ok(s) = session.lock() {
                let selected = s.project_name == selected_name;
                let marker = if selected { "▶" } else { " " };
                let (status_text, color) = match s.status {
                    SessionStatus::Starting => ("starting", app.theme.warn),
                    SessionStatus::Pulling => ("pulling", app.theme.warn),
                    SessionStatus::Running => ("running", app.theme.success),
                    SessionStatus::Error => ("error", app.theme.error),
                    SessionStatus::Stopped => ("stopped", app.theme.muted),
                };
                let name_style = if selected {
                    Style::default()
                        .fg(app.theme.text)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} {} ", marker, s.project_name), name_style),
                    Span::styled(format!("[{}]", status_text), Style::default().fg(color)),
                ]));
                if let Some(url) = s.url().map(|u| u.to_string()) {
                    lines.push(Line::from(vec![
                        Span::styled("     ", Style::default()),
                        Span::styled(
                            url,
                            Style::default()
                                .fg(app.theme.accent)
                                .add_modifier(Modifier::UNDERLINED),
                        ),
                    ]));
                }
                if let Some(err) = s.error.clone() {
                    lines.push(Line::from(Span::styled(
                        format!("     error: {}", err),
                        Style::default().fg(app.theme.error),
                    )));
                }
            }
        }

        if let Some(session) = app.selected_session()
            && let Ok(s) = session.lock()
            && let Ok(logs) = s.logs.lock()
            && !logs.is_empty()
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Logs (selected project) ",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            let used = lines.len() as u16;
            let avail = area.height.saturating_sub(used) as usize;
            let width = (area.width as usize).max(1);
            let mut budget = avail;
            let mut start = logs.len();
            for l in logs.iter().rev() {
                let wrapped = (l.chars().count().div_ceil(width)).max(1);
                if wrapped > budget {
                    break;
                }
                budget -= wrapped;
                start -= 1;
            }
            for l in &logs[start..] {
                lines.push(Line::from(Span::styled(
                    l.clone(),
                    Style::default().fg(app.theme.dim),
                )));
            }
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let log_border_style = Style::default().fg(app.theme.muted);

    let (bottom_title, bottom_content) = match app.mode {
        AppMode::EditingForm => {
            let (desc, example) = app.active_field.get_help();
            (
                format!(" Field Guide: {:?} ", app.active_field),
                format!("{} {}\n\n{} {}", '\u{f0eb}', desc, '\u{f02b}', example),
            )
        }
        _ => {
            let logs_text = if let Ok(logs) = app.logs.lock() {
                if logs.is_empty() {
                    app.status_message.clone()
                } else {
                    let h = area.height.saturating_sub(2).max(1) as usize;
                    let end = logs.len().saturating_sub(app.log_scroll.min(logs.len()));
                    let start = end.saturating_sub(h);
                    logs[start..end].join("\n")
                }
            } else {
                app.status_message.clone()
            };
            let title = if app.log_scroll > 0 {
                format!(" Logs & Output  (+{} lines up) ", app.log_scroll)
            } else {
                " Logs & Output ".to_string()
            };
            (title, logs_text)
        }
    };

    let bottom_widget = Paragraph::new(bottom_content)
        .block(
            Block::default()
                .title(bottom_title)
                .borders(Borders::ALL)
                .border_style(log_border_style),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(bottom_widget, area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.mode {
        AppMode::Normal => match app.active_pane {
            ActivePane::ProjectsList => {
                " [←/1] Projects · [→/2] Details · [[/]] Flip · [Tab/⇧Tab] Sub-tab | [n] New | [e] Edit | [p] Dup | [d] Delete | [/] Filter | [t] Test conn | [Enter] Focus details · again to launch | [s] Stop | [o/c] URL | [PgUp/Dn] Logs | [?] Help | [q] Quit"
            }
            ActivePane::Details => {
                " [←/1] Projects · [→/2] Details · [[/]] Flip · [Tab/⇧Tab] Sub-tab | [e] Edit | [t] Test conn | [Enter] Connect+Open Browser | [⇧Enter/r] Run | [s] Stop | [o/c] URL | [?] Help | [q] Quit"
            }
        },
        AppMode::EditingForm => {
            " [Tab/Down] Next Field | [Shift+Tab/Up] Prev Field | [Enter] Save/Toggle | [Esc] Cancel Edit"
        }
        AppMode::Filtering => {
            " Type to filter | [↑/↓] Navigate results | [Enter] Apply | [Esc] Clear filter"
        }
        AppMode::ConfirmDialog => " [Enter/y] Confirm | [Esc/n] Cancel",
        AppMode::Help => " [Esc/?/q] Close Help",
    };

    let footer = Paragraph::new(Span::styled(help_text, Style::default().fg(app.theme.warn)));
    f.render_widget(footer, area);
}

fn render_confirm_popup(f: &mut Frame, app: &App, action: ConfirmationAction) {
    let (title, prompt, theme_color) = match action {
        ConfirmationAction::DeleteProject => (
            " Delete Project ",
            "Are you sure you want to delete this project?".to_string(),
            app.theme.error,
        ),
        ConfirmationAction::CancelEdit => (
            " Discard Changes ",
            "Are you sure you want to discard unsaved changes?".to_string(),
            app.theme.warn,
        ),
        ConfirmationAction::Quit => (
            " Exit pg-studio ",
            "Are you sure you want to exit pg-studio? Running studios will keep running in the background."
                .to_string(),
            app.theme.accent,
        ),
        ConfirmationAction::StopProject => (
            " Stop Project ",
            format!(
                "Are you sure you want to stop the studio for project '{}'?",
                app.selected_project_name()
            ),
            app.theme.warn,
        ),
    };

    let term = f.area();
    const H_PAD: u16 = 4;
    const V_PAD: u16 = 2;
    let max_width = term.width.saturating_sub(8).clamp(24, 72);
    let inner_width = max_width.saturating_sub(2 + H_PAD * 2) as usize;

    let wrapped = wrap_text(&prompt, inner_width.max(1));
    let prompt_lines: Vec<Line> = wrapped
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.as_str(),
                Style::default()
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    let hint_line = Line::from(vec![
        Span::styled(
            " ⏎ ",
            Style::default()
                .fg(app.theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Confirm      ", Style::default().fg(app.theme.dim)),
        Span::styled(
            " ⎋ ",
            Style::default()
                .fg(app.theme.error)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Cancel", Style::default().fg(app.theme.dim)),
    ]);

    let longest_prompt = wrapped
        .iter()
        .map(|l| l.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let hint_width = 21u16;
    let content_width = longest_prompt
        .max(hint_width)
        .max(title.chars().count() as u16);
    let width = (content_width + 2 + H_PAD * 2)
        .clamp(24, max_width)
        .min(term.width.saturating_sub(2));

    let content_height = prompt_lines.len() as u16 + 1 + 1; // prompt + blank + hint
    let height = (2 + V_PAD * 2 + content_height).min(term.height.saturating_sub(2));

    let area = centered_rect_fixed(width, height, term);

    let mut popup_text = prompt_lines;
    popup_text.push(Line::from(""));
    popup_text.push(hint_line);

    let popup_block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme_color)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme_color))
        .style(Style::default().bg(app.theme.highlight_bg))
        .padding(Padding::new(H_PAD, H_PAD, V_PAD, V_PAD));

    let paragraph = Paragraph::new(popup_text)
        .alignment(Alignment::Center)
        .block(popup_block);

    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

fn centered_rect_fixed(width: u16, height: u16, r: Rect) -> Rect {
    let w = width.min(r.width);
    let h = height.min(r.height);
    let x = r.x + r.width.saturating_sub(w) / 2;
    let y = r.y + r.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

fn render_help_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(80, 92, f.area());

    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Navigation",
            &[
                ("← / →", "Focus Projects list / Details pane"),
                ("[ or ]", "Flip focus between Projects and Details"),
                ("1 / 2", "Jump to Projects / Details"),
                (
                    "Tab / Shift+Tab",
                    "Cycle Details sub-tabs (Overview / Config / Process)",
                ),
                (
                    "{ / }",
                    "Cycle Details sub-tabs (Overview / Config / Process)",
                ),
                ("↑/k  ↓/j", "Move selection in the projects list"),
            ],
        ),
        (
            "Projects",
            &[
                ("n", "New project"),
                ("e", "Edit selected project"),
                ("p", "Duplicate selected project into the editor"),
                ("d", "Delete selected project"),
                (
                    "Enter",
                    "Focus details pane; press again to launch + open browser when ready",
                ),
                (
                    "Shift+Enter / r",
                    "Focus details pane; press again to launch without opening browser",
                ),
                ("t", "Test connection reachability without launching"),
                ("/", "Filter projects by name (Esc clears)"),
                ("s", "Stop selected project's studio"),
            ],
        ),
        (
            "Editing",
            &[
                (
                    "Enter / Space",
                    "Cycle Connection Type (SSH -> URL -> Local)",
                ),
                ("Tab / ↓", "Next field"),
                ("Shift+Tab / ↑", "Previous field"),
                ("Paste", "A full postgres:// URL auto-fills all fields"),
                ("Enter", "Save project"),
                ("Esc", "Cancel edit"),
            ],
        ),
        (
            "Studio",
            &[
                ("o", "Open the running studio URL in your browser"),
                ("c", "Copy the running studio URL to the clipboard"),
                (
                    "URL",
                    "Studio URL is https://local.drizzle.studio?port=<port>",
                ),
            ],
        ),
        (
            "Global",
            &[
                ("?", "Toggle this help"),
                ("PgUp / PgDn", "Scroll the Logs pane (click it to re-follow)"),
                ("Mouse", "Click panes/tabs/rows, wheel scrolls lists and logs"),
                ("u", "Self-update pg-studio"),
                ("q / Esc", "Quit (stops all running studios)"),
            ],
        ),
    ];

    let mut lines: Vec<Line> = vec![];
    for (group, entries) in groups {
        lines.push(Line::from(Span::styled(
            format!(" {} ", group),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in *entries {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<16}", key),
                    Style::default().fg(app.theme.warn),
                ),
                Span::styled(*desc, Style::default().fg(app.theme.text)),
            ]));
        }
        lines.push(Line::from(""));
    }

    let block = Block::default()
        .title(" Keybindings ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent));

    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_respects_width() {
        let wrapped = wrap_text("aaaa bbbb cccc dddd", 9);
        assert!(wrapped.iter().all(|l| l.chars().count() <= 9));
        assert_eq!(wrapped.join(" "), "aaaa bbbb cccc dddd");
    }

    #[test]
    fn wrap_text_splits_on_newlines() {
        let wrapped = wrap_text("one\ntwo\n\nthree", 20);
        assert_eq!(
            wrapped,
            vec![
                "one".to_string(),
                "two".to_string(),
                String::new(),
                "three".to_string()
            ]
        );
    }

    #[test]
    fn wrap_text_breaks_single_long_words() {
        // A word longer than the width still lands on its own line.
        let wrapped = wrap_text("abcdefghij", 5);
        assert_eq!(wrapped, vec!["abcdefghij".to_string()]);
    }
}
