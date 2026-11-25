use crate::app::{
    ActivePane, App, BottomTab, ConnectionFormState, FormField, Modal, PreviewState,
    TransferDirection, TransferStatus,
};
use crate::fs_model::{FileEntry, format_bytes};
use crate::remote::LogLevel;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top header bar
            Constraint::Min(10),   // Dual pane browser
            Constraint::Length(9), // Transfers / Log panel
            Constraint::Length(1), // Footer keybindings
        ])
        .split(frame.area());

    draw_header(frame, app, root[0]);

    let panes_area = root[1];
    let left_pct = app.left_pane_pct;
    let right_pct = 100u16.saturating_sub(left_pct);
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Length(1),
            Constraint::Percentage(right_pct),
        ])
        .split(panes_area);

    app.areas.panes_area = panes_area;
    app.areas.local_pane = panes[0];
    app.areas.splitter = panes[1];
    app.areas.remote_pane = panes[2];
    app.areas.bottom_panel = root[2];
    app.areas.modal_area = Rect::default();
    app.areas.modal_list_area = Rect::default();

    draw_file_pane(
        frame,
        panes[0],
        &format!(" Locale: {} ", app.local_dir.display()),
        &app.local_entries,
        &mut app.local_state,
        app.active_pane == ActivePane::Local,
        None,
    );

    draw_splitter(frame, panes[1], app.dragging_splitter);

    let remote_title = if let Some(name) = &app.connected_name {
        format!(" Remoto ({name}): {} ", app.remote_dir)
    } else if app.connecting {
        " Remoto: connessione in corso... ".to_string()
    } else {
        " Remoto: non connesso ".to_string()
    };

    let empty_hint = if app.connected_name.is_none() && !app.connecting {
        Some("Nessuna connessione attiva.\n\nPremi [s] per aprire il Site Manager\no [n] per creare una nuova connessione.")
    } else {
        None
    };

    draw_file_pane(
        frame,
        panes[2],
        &remote_title,
        &app.remote_entries,
        &mut app.remote_state,
        app.active_pane == ActivePane::Remote,
        empty_hint,
    );

    draw_bottom_panel(frame, app, root[2]);
    draw_footer(frame, app, root[3]);

    // Modals overlay
    match &app.modal {
        Modal::None => {}
        Modal::SiteManager { selected } => {
            let (modal_rect, list_rect) = draw_site_manager(frame, app, *selected);
            app.areas.modal_area = modal_rect;
            app.areas.modal_list_area = list_rect;
        }
        Modal::ConnectionForm(form) => {
            app.areas.modal_area = draw_connection_form(frame, form);
        }
        Modal::InputPrompt {
            title,
            hint,
            value,
            masked,
            ..
        } => {
            app.areas.modal_area = draw_input_prompt(frame, title, hint, value, *masked);
        }
        Modal::Confirm { title, message, .. } => {
            app.areas.modal_area = draw_confirm(frame, title, message);
        }
        Modal::Preview(state) => {
            app.areas.modal_area = draw_preview(frame, state);
        }
        Modal::Help => {
            app.areas.modal_area = draw_help(frame);
        }
    }
}

fn draw_splitter(frame: &mut Frame, area: Rect, dragging: bool) {
    let style = if dragging {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mid = area.height / 2;
    let lines: Vec<Line> = (0..area.height)
        .map(|y| {
            let ch = if dragging {
                "┃"
            } else if y == mid || y.saturating_add(1) == mid || y == mid.saturating_add(1) {
                "⋮"
            } else {
                "│"
            };
            Line::from(Span::styled(ch, style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let status = if app.connecting {
        Span::styled(
            " ● Connessione in corso... ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(name) = &app.connected_name {
        Span::styled(
            format!(" ● Connesso: {name} "),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " ○ Disconnesso ",
            Style::default().fg(Color::DarkGray),
        )
    };

    let hidden_badge = if app.show_hidden {
        Span::styled(" [Nascosti: ON] ", Style::default().fg(Color::Cyan))
    } else {
        Span::styled(" [Nascosti: OFF] ", Style::default().fg(Color::DarkGray))
    };

    let active_transfers = app
        .transfers
        .iter()
        .filter(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::InProgress))
        .count();
    let transfer_badge = if active_transfers > 0 {
        Span::styled(
            format!(" [Trasferimenti attivi: {active_transfers}] "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let line = Line::from(vec![
        Span::styled(
            " KFTP ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        status,
        hidden_badge,
        transfer_badge,
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let p = Paragraph::new(line).block(block);
    frame.render_widget(p, area);
}

fn draw_file_pane(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    entries: &[FileEntry],
    state: &mut TableState,
    focused: bool,
    empty_hint: Option<&str>,
) {
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    let block = Block::default()
        .title(Span::styled(title, title_style))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    if let Some(hint) = empty_hint {
        let p = Paragraph::new(hint)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(p, area);
        return;
    }

    let compact = area.width < 52;
    let header = if compact {
        Row::new(vec![Cell::from("Nome"), Cell::from("Dim.")])
    } else {
        Row::new(vec![
            Cell::from("Nome"),
            Cell::from("Dimensione"),
            Cell::from("Modificato"),
            Cell::from("Permessi"),
        ])
    }
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(0);

    let rows: Vec<Row> = entries
        .iter()
        .map(|entry| {
            let icon = if entry.is_parent {
                "↩ "
            } else if entry.is_dir {
                "▸ "
            } else if entry.is_symlink {
                "@ "
            } else {
                "  "
            };

            let name_style = if entry.is_dir {
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD)
            } else if entry.is_symlink {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::White)
            };

            if compact {
                Row::new(vec![
                    Cell::from(format!("{icon}{}", entry.name)).style(name_style),
                    Cell::from(entry.formatted_size()).style(Style::default().fg(Color::Gray)),
                ])
            } else {
                Row::new(vec![
                    Cell::from(format!("{icon}{}", entry.name)).style(name_style),
                    Cell::from(entry.formatted_size()).style(Style::default().fg(Color::Gray)),
                    Cell::from(entry.formatted_date()).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(entry.permissions.as_str())
                        .style(Style::default().fg(Color::DarkGray)),
                ])
            }
        })
        .collect();

    let widths: Vec<Constraint> = if compact {
        vec![Constraint::Min(12), Constraint::Length(10)]
    } else {
        vec![
            Constraint::Min(18),
            Constraint::Length(11),
            Constraint::Length(17),
            Constraint::Length(11),
        ]
    };

    let highlight_style = if focused {
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::Rgb(40, 40, 40))
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(highlight_style)
        .highlight_symbol(if focused { "» " } else { "  " });

    frame.render_stateful_widget(table, area, state);
}

fn draw_bottom_panel(frame: &mut Frame, app: &App, area: Rect) {
    let transfers_title = match app.bottom_tab {
        BottomTab::Transfers => Span::styled(
            format!(" [Trasferimenti ({})] ", app.transfers.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        BottomTab::Log => Span::styled(
            format!(" Trasferimenti ({}) ", app.transfers.len()),
            Style::default().fg(Color::DarkGray),
        ),
    };
    let log_title = match app.bottom_tab {
        BottomTab::Log => Span::styled(
            format!(" [Log ({})] ", app.logs.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        BottomTab::Transfers => Span::styled(
            format!(" Log ({}) ", app.logs.len()),
            Style::default().fg(Color::DarkGray),
        ),
    };

    let block = Block::default()
        .title(Line::from(vec![
            transfers_title,
            Span::raw("│"),
            log_title,
            Span::styled("  ([t] cambia tab, [c] pulisci coda) ", Style::default().fg(Color::DarkGray)),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    match app.bottom_tab {
        BottomTab::Transfers => {
            if app.transfers.is_empty() {
                let p = Paragraph::new(
                    "Nessun trasferimento in coda. Seleziona un file e premi [Enter] o [u] per trasferirlo.",
                )
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
                frame.render_widget(p, area);
                return;
            }

            let inner_height = area.height.saturating_sub(2) as usize;
            let start = app.transfers.len().saturating_sub(inner_height);
            let items: Vec<ListItem> = app.transfers[start..]
                .iter()
                .map(|t| {
                    let dir_span = match t.direction {
                        TransferDirection::Upload => {
                            Span::styled("↑ UP  ", Style::default().fg(Color::LightCyan))
                        }
                        TransferDirection::Download => {
                            Span::styled("↓ DN  ", Style::default().fg(Color::LightGreen))
                        }
                    };
                    let pct = if t.total > 0 {
                        ((t.done as f64 / t.total as f64) * 100.0).min(100.0)
                    } else if t.status == TransferStatus::Done {
                        100.0
                    } else {
                        0.0
                    };
                    let bar = render_bar(pct, 16);
                    let (status_span, detail) = match t.status {
                        TransferStatus::Queued => (
                            Span::styled("IN CODA ", Style::default().fg(Color::Yellow)),
                            format!("{}/{}", format_bytes(t.done), format_bytes(t.total)),
                        ),
                        TransferStatus::InProgress => (
                            Span::styled(
                                format!("{pct:5.1}%  "),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            format!("{}/{}", format_bytes(t.done), format_bytes(t.total)),
                        ),
                        TransferStatus::Done => (
                            Span::styled("COMPLET ", Style::default().fg(Color::Green)),
                            format!("{}", format_bytes(t.done)),
                        ),
                        TransferStatus::Failed => (
                            Span::styled("ERRORE  ", Style::default().fg(Color::Red)),
                            t.error.clone().unwrap_or_default(),
                        ),
                    };

                    ListItem::new(Line::from(vec![
                        dir_span,
                        status_span,
                        Span::styled(bar, Style::default().fg(Color::Cyan)),
                        Span::raw("  "),
                        Span::styled(&t.file_name, Style::default().fg(Color::White)),
                        Span::raw("  "),
                        Span::styled(detail, Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect();

            let list = List::new(items).block(block);
            frame.render_widget(list, area);
        }
        BottomTab::Log => {
            let inner_height = area.height.saturating_sub(2) as usize;
            let start = app.logs.len().saturating_sub(inner_height);
            let items: Vec<ListItem> = app.logs[start..]
                .iter()
                .map(|entry| {
                    let color = match entry.level {
                        LogLevel::Info => Color::Gray,
                        LogLevel::Success => Color::Green,
                        LogLevel::Warn => Color::Yellow,
                        LogLevel::Error => Color::Red,
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("[{}] ", entry.timestamp),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(&entry.message, Style::default().fg(color)),
                    ]))
                })
                .collect();

            let list = List::new(items).block(block);
            frame.render_widget(list, area);
        }
    }
}

fn render_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let transfer_hint = match app.active_pane {
        ActivePane::Local => "[u/Enter] Upload",
        ActivePane::Remote => "[u/Enter] Download",
    };
    let text = format!(
        " [Tab] Pannello │ {transfer_hint} │ [v/Spazio/Click dx] Preview │ [s] Site Mgr │ [n] Nuova │ [m] Mkdir │ [r] Rinomina │ [d] Elimina │ [[ ]] Ridimensiona │ [?] Aiuto │ [q] Esci"
    );
    let p = Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Gray));
    frame.render_widget(p, area);
}

fn draw_site_manager(frame: &mut Frame, app: &App, selected: usize) -> (Rect, Rect) {
    let area = centered_rect(76, 70, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Site Manager — Connessioni salvate ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(inner);

    if app.connections.is_empty() {
        let p = Paragraph::new(
            "\nNessuna connessione salvata.\n\nPremi [n] per creare una nuova connessione SFTP / FTP / FTPS.",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Gray));
        frame.render_widget(p, chunks[0]);
    } else {
        let items: Vec<ListItem> = app
            .connections
            .iter()
            .map(|c| {
                let auth_tag = match c.auth_kind {
                    crate::config::AuthKind::Password => "Password",
                    crate::config::AuthKind::KeyFile => "SSH Key",
                    crate::config::AuthKind::SshAgent => "SSH Agent",
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("{:<20} ", c.name),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("[{:<4}] ", match c.protocol {
                            crate::config::Protocol::Sftp => "SFTP",
                            crate::config::Protocol::Ftp => "FTP",
                            crate::config::Protocol::Ftps => "FTPS",
                        }),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        format!("{}@{}:{}  ", c.username, c.host, c.port),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(
                        format!("({auth_tag})"),
                        Style::default().fg(Color::Yellow),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(selected.min(app.connections.len() - 1)));

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("» ");
        frame.render_stateful_widget(list, chunks[0], &mut state);
    }

    let help = Paragraph::new(
        "[Enter/Doppio click] Connetti │ [n] Nuova │ [e] Modifica │ [d] Elimina │ [Esc] Chiudi",
    )
    .alignment(Alignment::Center)
    .style(Style::default().fg(Color::Yellow));
    frame.render_widget(help, chunks[1]);
    (area, chunks[0])
}

fn draw_connection_form(frame: &mut Frame, form: &ConnectionFormState) -> Rect {
    let area = centered_rect(72, 78, frame.area());
    frame.render_widget(Clear, area);

    let title = if form.editing_index.is_some() {
        " Modifica Connessione "
    } else {
        " Nuova Connessione "
    };

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);

    let fields = form.visible_fields();
    let mut lines: Vec<Line> = Vec::new();
    for field in fields {
        let is_focused = field == form.focused;
        let label = format!("{:<26}", field.label(form.profile.auth_kind));

        let value_display = match field {
            FormField::Name => form.profile.name.clone(),
            FormField::Protocol => format!("◀ {} ▶", form.profile.protocol.label()),
            FormField::Host => form.profile.host.clone(),
            FormField::Port => form.port_text.clone(),
            FormField::Username => form.profile.username.clone(),
            FormField::AuthKind => format!("◀ {} ▶", form.profile.auth_kind.label()),
            FormField::KeyPath => form.profile.key_path.clone(),
            FormField::Secret => "•".repeat(form.profile.secret.chars().count()),
            FormField::SaveSecret => {
                if form.profile.save_secret {
                    "[x] Sì (salvato in connections.toml con permessi 600)".to_string()
                } else {
                    "[ ] No (richiesta a runtime se necessaria)".to_string()
                }
            }
            FormField::RemoteDir => form.profile.remote_dir.clone(),
            FormField::LocalDir => form.profile.local_dir.clone(),
        };

        let cursor = if is_focused
            && !matches!(
                field,
                FormField::Protocol | FormField::AuthKind | FormField::SaveSecret
            ) {
            "▏"
        } else {
            ""
        };

        let row_style = if is_focused {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };

        let label_style = if is_focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let val_style = if is_focused {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(
            Line::from(vec![
                Span::raw(if is_focused { "» " } else { "  " }),
                Span::styled(label, label_style),
                Span::raw(": "),
                Span::styled(format!("{value_display}{cursor}"), val_style),
            ])
            .style(row_style),
        );
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(p, chunks[0]);

    if let Some(err) = &form.error {
        let err_p = Paragraph::new(err.as_str())
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(err_p, chunks[1]);
    }

    let hint = Paragraph::new(
        "[Tab/↑↓] Campo  │  [←→/Spazio] Cambia opzione  │  [Enter] Salva & Connetti  │  [Ctrl+S] Solo Salva  │  [Esc] Annulla",
    )
    .alignment(Alignment::Center)
    .style(Style::default().fg(Color::Yellow));
    frame.render_widget(hint, chunks[2]);
    area
}

fn draw_input_prompt(frame: &mut Frame, title: &str, hint: &str, value: &str, masked: bool) -> Rect {
    let area = centered_rect(58, 24, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let shown = if masked {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };

    let lines = vec![
        Line::from(Span::styled(hint, Style::default().fg(Color::Gray))),
        Line::from(""),
        Line::from(Span::styled(
            format!("> {shown}▏"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] Conferma   │   [Esc] Annulla",
            Style::default().fg(Color::Yellow),
        )),
    ];

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
    area
}

fn draw_confirm(frame: &mut Frame, title: &str, message: &str) -> Rect {
    let area = centered_rect(52, 22, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(message, Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(Span::styled(
            "[y / Enter] Conferma   │   [n / Esc] Annulla",
            Style::default().fg(Color::Yellow),
        )),
    ];

    let p = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
    area
}

fn draw_preview(frame: &mut Frame, state: &PreviewState) -> Rect {
    let area = centered_rect(88, 86, frame.area());
    frame.render_widget(Clear, area);

    let origin = if state.is_remote { "Remoto" } else { "Locale" };
    let mode = if state.loading {
        "Caricamento...".to_string()
    } else if state.formatted {
        format!("{} · Pretty", state.kind.label())
    } else if !state.pretty {
        format!("{} · Raw", state.kind.label())
    } else {
        state.kind.label().to_string()
    };
    let size_info = state
        .total_size
        .map(|s| format!(" — {}", format_bytes(s)))
        .unwrap_or_default();
    let trunc = if state.truncated {
        " (troncato a 256 KB)"
    } else {
        ""
    };
    let wrap_badge = if state.wrap { " [Wrap]" } else { "" };

    let block = Block::default()
        .title(Span::styled(
            format!(
                " Preview [{origin} · {mode}]: {}{size_info}{trunc}{wrap_badge} ",
                state.title
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    let p = if state.wrap {
        Paragraph::new(state.lines.clone())
            .wrap(Wrap { trim: false })
            .scroll((state.scroll, 0))
    } else {
        Paragraph::new(state.lines.clone()).scroll((state.scroll, state.h_scroll))
    };
    frame.render_widget(p, chunks[0]);

    let action_label = if state.is_remote {
        "[Enter/u] Scarica"
    } else {
        "[Enter/u] Carica"
    };
    let pretty_label = if state.pretty {
        "[f] Raw"
    } else {
        "[f] Pretty"
    };
    let wrap_label = if state.wrap {
        "[w] No-Wrap"
    } else {
        "[w] Wrap"
    };
    let footer = Paragraph::new(format!(
        " [↑↓←→/Rotella] Scorri │ {pretty_label} │ {wrap_label} │ {action_label} │ [Esc/v/Spazio] Chiudi"
    ))
    .style(Style::default().fg(Color::Yellow));
    frame.render_widget(footer, chunks[1]);

    area
}

fn draw_help(frame: &mut Frame) -> Rect {
    let area = centered_rect(70, 74, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Span::styled(
            " Guida Comandi — KFTP ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let help_lines = vec![
        Line::from(Span::styled(
            "Navigazione",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Tab / ← → / h l    Sposta il focus tra pannello Locale e Remoto"),
        Line::from("  ↑ ↓ / j k          Muovi selezione su / giù"),
        Line::from("  Enter              Apri cartella oppure carica/scarica il file selezionato"),
        Line::from("  Backspace          Torna alla cartella superiore"),
        Line::from("  p                  Vai direttamente a un percorso (locale o remoto)"),
        Line::from("  .                  Mostra / nascondi file nascosti"),
        Line::from("  R / F5             Aggiorna entrambe le liste"),
        Line::from(""),
        Line::from(Span::styled(
            "File & Trasferimenti",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  u                  Upload (dal pannello Locale) o Download (dal Remoto)"),
        Line::from("  v / Spazio         Anteprima file (testo o hex, fino a 64 KB) senza scaricarlo"),
        Line::from("  [ / ]              Riduci / espandi il pannello Locale"),
        Line::from("  m                  Crea nuova cartella nel pannello attivo"),
        Line::from("  r                  Rinomina file o cartella selezionata"),
        Line::from("  d / Canc           Elimina file o cartella selezionata"),
        Line::from("  t                  Alterna pannello inferiore tra Trasferimenti e Log"),
        Line::from("  c                  Pulisce i trasferimenti completati/falliti"),
        Line::from(""),
        Line::from(Span::styled(
            "Mouse",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Click sx           Seleziona file / cambia pannello"),
        Line::from("  Doppio click sx    Apri cartella o trasferisci file"),
        Line::from("  Click dx           Apri anteprima del file"),
        Line::from("  Rotella            Scorri file o anteprima"),
        Line::from("  Trascina │ (⋮)     Ridimensiona il pannello sinistro drag & drop"),
        Line::from(""),
        Line::from(Span::styled(
            "Connessioni (SFTP / FTP / FTPS)",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  s                  Apri Site Manager (connessioni salvate)"),
        Line::from("  n                  Crea nuova connessione (supporta Password, Chiave SSH, SSH Agent)"),
        Line::from("  D                  Disconnetti dal server remoto"),
        Line::from("  q / Ctrl+C         Esci da KFTP"),
        Line::from(""),
        Line::from(Span::styled(
            "Premi [Esc] o [?] per chiudere",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let p = Paragraph::new(help_lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
    area
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
