use crate::app::{ActivePane, App, AppMode, FormField};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

pub fn draw(f: &mut Frame, app: &App) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Content (Left: List, Right: Form)
            Constraint::Length(8), // Logs / Output
            Constraint::Length(1), // Help / Footer
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

    // 2. Main Content Split (Left: Projects List, Right: Project Details/Form)
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

    let form_block = Block::default()
        .title(if app.is_new_project {
            " New Project Config "
        } else {
            " Edit Project Config "
        })
        .borders(Borders::ALL)
        .border_style(form_border_style);

    let form_inner = form_block.inner(content_chunks[1]);
    f.render_widget(form_block, content_chunks[1]);

    draw_form_fields(f, app, form_inner);

    // 3. Bottom Pane: Logs
    let log_border_style = if app.active_pane == ActivePane::Logs {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let logs_text = if let Ok(logs) = app.logs.lock() {
        logs.join("\n")
    } else {
        String::new()
    };

    let logs_widget = Paragraph::new(logs_text)
        .block(
            Block::default()
                .title(" Logs & Output ")
                .borders(Borders::ALL)
                .border_style(log_border_style),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(logs_widget, main_chunks[2]);

    // 4. Help / Footer
    let help_text = match app.mode {
        AppMode::Normal => match app.active_pane {
            ActivePane::ProjectsList => {
                " [Tab] Switch Pane | [n] New Project | [e] Edit | [d] Delete | [Enter] Launch Studio | [u] Self Update | [q] Quit"
            }
            ActivePane::ProjectForm => {
                " [Tab] Switch Pane | [e] Edit Form Fields | [n] Clear & New | [Enter] Launch Studio | [q] Quit"
            }
            ActivePane::Logs => " [Tab] Switch Pane | [q] Quit",
        },
        AppMode::EditingForm => {
            " [Tab/Down] Next Field | [Shift+Tab/Up] Prev Field | [Enter] Save & Finish | [Esc] Cancel Edit"
        }
        AppMode::Running => " [q/Esc] Stop SSH & Exit Studio",
    };

    let footer = Paragraph::new(Span::styled(help_text, Style::default().fg(Color::Yellow)));
    f.render_widget(footer, main_chunks[3]);
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
            Constraint::Min(0),
        ])
        .split(area);

    let fields = [
        (
            FormField::Name,
            "Project Name:",
            app.input_name.value(),
            false,
        ),
        (
            FormField::SshConnection,
            "SSH String:",
            app.input_ssh.value(),
            false,
        ),
        (
            FormField::DbPort,
            "Remote DB Port:",
            app.input_port.value(),
            false,
        ),
        (
            FormField::DbName,
            "Database Name:",
            app.input_dbname.value(),
            false,
        ),
        (
            FormField::DbUser,
            "Database User:",
            app.input_dbuser.value(),
            false,
        ),
        (
            FormField::DbPass,
            "Database Pass:",
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

        let display_val = if *is_pass {
            "*".repeat(value.len())
        } else {
            value.to_string()
        };

        let label_style = if is_active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let val_style = if is_active {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };

        let line = Line::from(vec![
            Span::styled(format!("{:<15} ", label), label_style),
            Span::styled(display_val, val_style),
        ]);

        f.render_widget(Paragraph::new(line), chunks[idx]);
    }
}
