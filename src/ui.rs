use crate::app::{
    ActivePane, App, AppMode, ConfirmationAction, DetailsTab, FormField, ProjectState,
};
use crate::config::ConnectionType;
use crate::session::SessionStatus;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap},
};

pub fn draw(f: &mut Frame, app: &App) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(12),   // Content (Left: List, Right: Details)
            Constraint::Length(8), // Logs / Guidance Output
            Constraint::Length(1), // Footer / Help Bar
        ])
        .split(f.area());

    draw_header(f, app, main_chunks[0]);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(main_chunks[1]);

    draw_projects_list(f, app, content_chunks[0]);
    draw_details(f, app, content_chunks[1]);

    draw_logs(f, app, main_chunks[2]);
    draw_footer(f, app, main_chunks[3]);

    if app.mode == AppMode::ConfirmDialog
        && let Some(action) = app.confirm_action
    {
        render_confirm_popup(f, action);
    }
    if app.mode == AppMode::Help {
        render_help_popup(f);
    }
}

fn draw_header(f: &mut Frame, _app: &App, area: Rect) {
    let header = Paragraph::new("").block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Line::from(vec![
                Span::styled(
                    " PG-STUDIO ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " | Remote & Hosted Postgres Drizzle Studio Launcher",
                    Style::default().fg(Color::Gray),
                ),
            ]))
            .title(
                Line::from(vec![Span::styled(
                    format!(" v{} ", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(Color::Magenta),
                )])
                .right_aligned(),
            ),
    );
    f.render_widget(header, area);
}

fn draw_projects_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = if app.config.projects.is_empty() {
        vec![ListItem::new(Span::styled(
            "No projects yet. Press 'n' to add.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.config
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let state = app.project_state(&p.name);
                let selected =
                    i == app.selected_project_idx && app.active_pane == ActivePane::ProjectsList;
                let name_style = if selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if i == app.selected_project_idx {
                    Style::default().fg(Color::Cyan)
                } else {
                    match state {
                        Some(ProjectState::Running) => Style::default().fg(Color::Green),
                        Some(ProjectState::Error) => Style::default().fg(Color::Red),
                        Some(ProjectState::Stopped) => Style::default().fg(Color::DarkGray),
                        None => Style::default().fg(Color::White),
                    }
                };
                let marker = match state {
                    Some(ProjectState::Running) => {
                        Span::styled("● ", Style::default().fg(Color::Green))
                    }
                    Some(ProjectState::Error) => {
                        Span::styled("● ", Style::default().fg(Color::Red))
                    }
                    Some(ProjectState::Stopped) => {
                        Span::styled("○ ", Style::default().fg(Color::DarkGray))
                    }
                    None => Span::styled("• ", name_style),
                };
                ListItem::new(Line::from(vec![
                    marker,
                    Span::styled(p.name.clone(), name_style),
                ]))
            })
            .collect()
    };

    let list_border_style = if app.active_pane == ActivePane::ProjectsList {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let projects_list = List::new(items).block(
        Block::default()
            .title(" Projects ")
            .borders(Borders::ALL)
            .border_style(list_border_style),
    );
    f.render_widget(projects_list, area);
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.active_pane == ActivePane::Details {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_details_subtabs(f, app, chunks[0]);

    match app.details_tab {
        DetailsTab::Overview => draw_overview(f, app, chunks[1]),
        DetailsTab::Config => draw_config(f, app, chunks[1]),
        DetailsTab::Process => draw_process(f, app, chunks[1]),
    }
}

fn draw_details_subtabs(f: &mut Frame, app: &App, area: Rect) {
    let tabs = [
        ("Overview", DetailsTab::Overview),
        ("Config", DetailsTab::Config),
        ("Process", DetailsTab::Process),
    ];
    let mut spans: Vec<Span> = Vec::new();
    for (label, tab) in tabs {
        let active = app.active_pane == ActivePane::Details && app.details_tab == tab;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_overview(f: &mut Frame, app: &App, area: Rect) {
    if app.config.projects.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No project selected.",
                Style::default().fg(Color::DarkGray),
            )),
            area,
        );
        return;
    }

    let proj = &app.config.projects[app.selected_project_idx];
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" Name              : {}", proj.name),
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            format!(
                " Connection Type   : {}",
                match proj.connection_type {
                    ConnectionType::Ssh => "SSH Tunnel",
                    ConnectionType::Url => "Public URL",
                }
            ),
            Style::default().fg(Color::White),
        )),
    ];

    match proj.connection_type {
        ConnectionType::Ssh => {
            lines.push(Line::from(Span::styled(
                format!(" SSH               : {}", proj.ssh_connection),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                format!(" Remote Port       : {}", proj.db_port),
                Style::default().fg(Color::White),
            )));
        }
        ConnectionType::Url => {
            if !proj.db_url.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(" URL               : {}", proj.db_url),
                    Style::default().fg(Color::White),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" Host              : {}", proj.db_host),
                    Style::default().fg(Color::White),
                )));
                lines.push(Line::from(Span::styled(
                    format!(" Port              : {}", proj.db_port),
                    Style::default().fg(Color::White),
                )));
            }
        }
    }

    lines.push(Line::from(Span::styled(
        format!(" Database          : {}", proj.db_name),
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(Span::styled(
        format!(" User              : {}", proj.db_user),
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(Span::styled(
        format!(" Last Opened       : {}", app.formatted_last_opened()),
        Style::default().fg(Color::Magenta),
    )));

    let running = app.is_project_running(&proj.name);
    let status_line = if running {
        Span::styled(
            " Status            : ● Running",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " Status            : ○ Stopped",
            Style::default().fg(Color::DarkGray),
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
            ConnectionType::Ssh => "SSH (Enter to toggle)".to_string(),
            ConnectionType::Url => "URL (Enter to toggle)".to_string(),
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
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    "5432 (default)".to_string(),
                    Style::default().fg(Color::DarkGray),
                )
            }
        } else if *is_pass {
            if value.is_empty() && !app.is_new_project && app.mode != AppMode::EditingForm {
                (
                    "•••••••• (Stored in Keychain)".to_string(),
                    Style::default().fg(Color::DarkGray),
                )
            } else if value.is_empty() {
                ("".to_string(), Style::default().fg(Color::White))
            } else {
                (
                    "*".repeat(value.len()),
                    if is_active {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                )
            }
        } else {
            (
                value.to_string(),
                if is_active {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            )
        };

        let label_style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let prefix_indicator = if is_active { " ▶ " } else { "   " };

        let line = Line::from(vec![
            Span::styled(prefix_indicator, Style::default().fg(Color::Cyan)),
            Span::styled(format!("{:<16}: ", label), label_style),
            Span::styled(display_val, val_style),
        ]);

        f.render_widget(Paragraph::new(line), chunks[idx]);
    }

    // Render Last Opened Date
    if chunks.len() > n {
        let last_opened_str = app.formatted_last_opened();
        let date_line = Line::from(vec![
            Span::styled("   Last Opened    : ", Style::default().fg(Color::DarkGray)),
            Span::styled(last_opened_str, Style::default().fg(Color::Magenta)),
        ]);
        f.render_widget(Paragraph::new(date_line), chunks[n]);
    }

    // Render Error Message if present
    if let Some(err_msg) = &app.error_message
        && chunks.len() > n + 1
    {
        let err_line = Line::from(vec![
            Span::styled(
                "   ⚠️ Error: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err_msg, Style::default().fg(Color::LightRed)),
        ]);
        f.render_widget(Paragraph::new(err_line), chunks[n + 1]);
    }
}

fn draw_process(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = vec![];

    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No running studios. Press Enter to launch+open, or Shift+Enter / r to just launch.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " Sessions ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        let selected_name = app.selected_project_name();
        for session in &app.sessions {
            if let Ok(s) = session.lock() {
                let selected = s.project_name == selected_name;
                let marker = if selected { "▶" } else { " " };
                let (status_text, color) = match s.status {
                    SessionStatus::Starting => ("starting", Color::Yellow),
                    SessionStatus::Pulling => ("pulling", Color::Yellow),
                    SessionStatus::Running => ("running", Color::Green),
                    SessionStatus::Error => ("error", Color::Red),
                    SessionStatus::Stopped => ("stopped", Color::DarkGray),
                };
                let name_style = if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
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
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::UNDERLINED),
                        ),
                    ]));
                }
                if let Some(err) = s.error.clone() {
                    lines.push(Line::from(Span::styled(
                        format!("     error: {}", err),
                        Style::default().fg(Color::LightRed),
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
                    .fg(Color::Cyan)
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
                    Style::default().fg(Color::Gray),
                )));
            }
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let log_border_style = if app.active_pane == ActivePane::Logs {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let (bottom_title, bottom_content) = match app.mode {
        AppMode::EditingForm => {
            let (desc, example) = app.active_field.get_help();
            (
                format!(" Field Guide: {:?} ", app.active_field),
                format!("💡 {}\n\n📌 {}", desc, example),
            )
        }
        _ => {
            let logs_text = if let Ok(logs) = app.logs.lock() {
                if logs.is_empty() {
                    app.status_message.clone()
                } else {
                    let h = area.height.saturating_sub(2).max(1) as usize;
                    let start = logs.len().saturating_sub(h);
                    logs[start..].join("\n")
                }
            } else {
                app.status_message.clone()
            };
            (" Logs & Output ".to_string(), logs_text)
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
                " [←/→] Pane | [n] New | [e] Edit | [d] Delete | [Enter] Launch+Open | [⇧Enter/r] Launch | [s] Stop | [o] Open URL | [c] Copy URL | [?] Help | [q] Quit"
            }
            ActivePane::Details => {
                " [[/]] Sub-tab | [e] Edit | [Enter] Launch+Open | [⇧Enter/r] Launch | [s] Stop | [o] Open URL | [c] Copy URL | [?] Help | [q] Quit"
            }
            ActivePane::Logs => {
                " [←/→] Pane | [Enter] Launch+Open | [⇧Enter/r] Launch | [s] Stop | [o] Open URL | [c] Copy URL | [?] Help | [q] Quit"
            }
        },
        AppMode::EditingForm => {
            " [Tab/Down] Next Field | [Shift+Tab/Up] Prev Field | [Enter] Save/Toggle | [Esc] Cancel Edit"
        }
        AppMode::ConfirmDialog => " [Enter/y] Confirm | [Esc/n] Cancel",
        AppMode::Help => " [Esc/?/q] Close Help",
    };

    let footer = Paragraph::new(Span::styled(help_text, Style::default().fg(Color::Yellow)));
    f.render_widget(footer, area);
}

fn render_confirm_popup(f: &mut Frame, action: ConfirmationAction) {
    let area = centered_rect(60, 25, f.area());

    let (title, prompt, theme_color) = match action {
        ConfirmationAction::DeleteProject => (
            " Delete Project ",
            "Are you sure you want to delete this project?",
            Color::Red,
        ),
        ConfirmationAction::CancelEdit => (
            " Discard Changes ",
            "Are you sure you want to discard unsaved changes?",
            Color::Yellow,
        ),
        ConfirmationAction::Quit => (
            " Exit pg-studio ",
            "Are you sure you want to exit pg-studio? Running studios will be stopped.",
            Color::Cyan,
        ),
    };

    let popup_text = vec![
        Line::from(""),
        Line::from(Span::styled(
            prompt,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " ⏎ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Confirm        ", Style::default().fg(Color::Gray)),
            Span::styled(
                " ⎋ ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Cancel  ", Style::default().fg(Color::Gray)),
        ]),
    ];

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
        .padding(Padding::horizontal(4));

    let paragraph = Paragraph::new(popup_text)
        .alignment(Alignment::Center)
        .block(popup_block);

    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn render_help_popup(f: &mut Frame) {
    let area = centered_rect(80, 92, f.area());

    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Navigation",
            &[
                ("←/→ or Tab", "Switch pane (Projects / Details / Logs)"),
                (
                    "[ / ]",
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
                ("d", "Delete selected project"),
                ("Enter", "Launch studio + auto-open browser when ready"),
                ("Shift+Enter / r", "Launch studio without opening browser"),
                ("s", "Stop selected project's studio"),
            ],
        ),
        (
            "Editing",
            &[
                ("Enter / Space", "Toggle Connection Type (SSH vs URL)"),
                ("Tab / ↓", "Next field"),
                ("Shift+Tab / ↑", "Previous field"),
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in *entries {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<16}", key), Style::default().fg(Color::Yellow)),
                Span::styled(*desc, Style::default().fg(Color::White)),
            ]));
        }
        lines.push(Line::from(""));
    }

    let block = Block::default()
        .title(" Keybindings ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

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
