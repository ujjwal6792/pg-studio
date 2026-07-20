use crate::app::{ActivePane, App, AppMode, ConfirmationAction, FormField};
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
            Constraint::Min(12),   // Content (Left: List, Right: Form)
            Constraint::Length(8), // Logs / Guidance Output
            Constraint::Length(1), // Footer / Help Bar
        ])
        .split(f.area());

    // 1. Header
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " 🗄️  PG-STUDIO ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " | Remote Postgres Drizzle Studio Launcher",
            Style::default().fg(Color::Gray),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, main_chunks[0]);

    // 2. Main Content Split
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(main_chunks[1]);

    // 2a. Left Pane: Projects List
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
                let style = if i == app.selected_project_idx
                    && app.active_pane == ActivePane::ProjectsList
                {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if i == app.selected_project_idx {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Span::styled(format!("• {}", p.name), style))
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
    f.render_widget(projects_list, content_chunks[0]);

    // 2b. Right Pane: Project Config / Form
    let form_border_style = if app.active_pane == ActivePane::ProjectForm {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let form_title = if app.is_new_project {
        " New Project Config "
    } else {
        " Edit Project Config "
    };

    let form_block = Block::default()
        .title(form_title)
        .borders(Borders::ALL)
        .border_style(form_border_style);

    let form_inner = form_block.inner(content_chunks[1]);
    f.render_widget(form_block, content_chunks[1]);

    draw_form_fields(f, app, form_inner);

    // 3. Bottom Pane: Logs OR Field Guidance & Examples
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
                    logs.join("\n")
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
    f.render_widget(bottom_widget, main_chunks[2]);

    // 4. Footer / Help Bar
    let help_text = match app.mode {
        AppMode::Normal => match app.active_pane {
            ActivePane::ProjectsList => {
                " [Tab] Switch Pane | [n] New Project | [e] Edit | [d] Delete | [Enter] Launch Studio | [u] Self Update | [q/Esc] Quit"
            }
            ActivePane::ProjectForm => {
                " [Tab] Switch Pane | [e] Edit Form Fields | [n] Clear & New | [Enter] Launch Studio | [q/Esc] Quit"
            }
            ActivePane::Logs => " [Tab] Switch Pane | [q/Esc] Quit",
        },
        AppMode::EditingForm => {
            " [Tab/Down] Next Field | [Shift+Tab/Up] Prev Field | [Enter] Save Project | [Esc] Cancel Edit"
        }
        AppMode::ConfirmDialog => " [Enter/y] Confirm | [Esc/n] Cancel",
        AppMode::Running => " [q/Esc] Stop SSH & Exit Studio",
    };

    let footer = Paragraph::new(Span::styled(help_text, Style::default().fg(Color::Yellow)));
    f.render_widget(footer, main_chunks[3]);

    // 5. Render Centered Confirmation Modal Popup if active
    if app.mode == AppMode::ConfirmDialog {
        if let Some(action) = app.confirm_action {
            render_confirm_popup(f, action);
        }
    }
}

fn draw_form_fields(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Name
            Constraint::Length(2), // SSH
            Constraint::Length(2), // Port
            Constraint::Length(2), // DB Name
            Constraint::Length(2), // DB User
            Constraint::Length(2), // DB Pass
            Constraint::Length(2), // Last Opened Date
            Constraint::Length(2), // Error message if any
            Constraint::Min(0),
        ])
        .split(area);

    let fields = [
        (
            FormField::Name,
            "Project Name",
            app.input_name.value(),
            false,
        ),
        (
            FormField::SshConnection,
            "SSH String",
            app.input_ssh.value(),
            false,
        ),
        (
            FormField::DbPort,
            "Remote DB Port",
            app.input_port.value(),
            false,
        ),
        (
            FormField::DbName,
            "Database Name",
            app.input_dbname.value(),
            false,
        ),
        (
            FormField::DbUser,
            "Database User",
            app.input_dbuser.value(),
            false,
        ),
        (
            FormField::DbPass,
            "Database Pass",
            app.input_dbpass.value(),
            true,
        ),
    ];

    for (idx, (field_type, label, value, is_pass)) in fields.iter().enumerate() {
        if idx >= chunks.len() {
            break;
        }

        let is_active = app.active_pane == ActivePane::ProjectForm
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
            Span::styled(format!("{:<15}: ", label), label_style),
            Span::styled(display_val, val_style),
        ]);

        f.render_widget(Paragraph::new(line), chunks[idx]);
    }

    // Render Last Opened Date
    if chunks.len() > 6 {
        let last_opened_str = app.formatted_last_opened();
        let date_line = Line::from(vec![
            Span::styled("   Last Opened    : ", Style::default().fg(Color::DarkGray)),
            Span::styled(last_opened_str, Style::default().fg(Color::Magenta)),
        ]);
        f.render_widget(Paragraph::new(date_line), chunks[6]);
    }

    // Render Error Message if present
    if let Some(err_msg) = &app.error_message {
        if chunks.len() > 7 {
            let err_line = Line::from(vec![
                Span::styled(
                    "   ⚠️ Error: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(err_msg, Style::default().fg(Color::LightRed)),
            ]);
            f.render_widget(Paragraph::new(err_line), chunks[7]);
        }
    }
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
            "Are you sure you want to exit pg-studio?",
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
