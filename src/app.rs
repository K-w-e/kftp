use crate::config::{
    AuthKind, ConnectionProfile, Protocol, expand_tilde, load_connections, save_connections,
};
use crate::fs_model::{FileEntry, default_local_dir, list_local_dir, remote_join, remote_parent};
use crate::preview::{PreviewKind, build_preview};
use crate::remote::{LogLevel, RemoteWorkerHandle, WorkerCommand, WorkerEvent, spawn_worker};
use chrono::Local;
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::TableState;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const PREVIEW_MAX_BYTES: usize = 256 * 1024;
pub const MIN_LEFT_PANE_PCT: u16 = 18;
pub const MAX_LEFT_PANE_PCT: u16 = 80;
pub const DEFAULT_LEFT_PANE_PCT: u16 = 34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    Queued,
    InProgress,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct TransferItem {
    pub id: u64,
    pub direction: TransferDirection,
    pub file_name: String,
    pub done: u64,
    pub total: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    Transfers,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Protocol,
    Host,
    Port,
    Username,
    AuthKind,
    KeyPath,
    Secret,
    SaveSecret,
    RemoteDir,
    LocalDir,
}

impl FormField {
    pub fn visible_fields(protocol: Protocol, auth_kind: AuthKind) -> Vec<FormField> {
        let mut fields = vec![
            FormField::Name,
            FormField::Protocol,
            FormField::Host,
            FormField::Port,
            FormField::Username,
        ];
        if protocol == Protocol::Sftp {
            fields.push(FormField::AuthKind);
        }
        match (protocol, auth_kind) {
            (Protocol::Sftp, AuthKind::KeyFile) => {
                fields.push(FormField::KeyPath);
                fields.push(FormField::Secret); // passphrase (optional)
                fields.push(FormField::SaveSecret);
            }
            (Protocol::Sftp, AuthKind::SshAgent) => {}
            _ => {
                fields.push(FormField::Secret);
                fields.push(FormField::SaveSecret);
            }
        }
        fields.push(FormField::RemoteDir);
        fields.push(FormField::LocalDir);
        fields
    }

    pub fn label(self, auth_kind: AuthKind) -> &'static str {
        match self {
            FormField::Name => "Nome profilo",
            FormField::Protocol => "Protocollo",
            FormField::Host => "Host",
            FormField::Port => "Porta",
            FormField::Username => "Utente",
            FormField::AuthKind => "Autenticazione",
            FormField::KeyPath => "Percorso chiave privata",
            FormField::Secret => match auth_kind {
                AuthKind::KeyFile => "Passphrase chiave (opz.)",
                _ => "Password",
            },
            FormField::SaveSecret => "Salva password/passphrase",
            FormField::RemoteDir => "Cartella remota iniziale",
            FormField::LocalDir => "Cartella locale iniziale",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionFormState {
    pub editing_index: Option<usize>,
    pub profile: ConnectionProfile,
    pub port_text: String,
    pub focused: FormField,
    pub error: Option<String>,
}

impl ConnectionFormState {
    pub fn new_profile() -> Self {
        let profile = ConnectionProfile::new_default();
        let port_text = profile.port.to_string();
        Self {
            editing_index: None,
            profile,
            port_text,
            focused: FormField::Name,
            error: None,
        }
    }

    pub fn from_existing(index: usize, profile: ConnectionProfile) -> Self {
        let port_text = profile.port.to_string();
        Self {
            editing_index: Some(index),
            profile,
            port_text,
            focused: FormField::Name,
            error: None,
        }
    }

    pub fn visible_fields(&self) -> Vec<FormField> {
        FormField::visible_fields(self.profile.protocol, self.profile.auth_kind)
    }

    pub fn ensure_valid_focus(&mut self) {
        let fields = self.visible_fields();
        if !fields.contains(&self.focused) {
            self.focused = fields[0];
        }
    }

    pub fn next_field(&mut self) {
        let fields = self.visible_fields();
        if let Some(pos) = fields.iter().position(|&f| f == self.focused) {
            self.focused = fields[(pos + 1) % fields.len()];
        } else {
            self.focused = fields[0];
        }
    }

    pub fn prev_field(&mut self) {
        let fields = self.visible_fields();
        if let Some(pos) = fields.iter().position(|&f| f == self.focused) {
            self.focused = fields[(pos + fields.len() - 1) % fields.len()];
        } else {
            self.focused = fields[0];
        }
    }

    pub fn build_profile(&mut self) -> Result<ConnectionProfile, String> {
        let host = self.profile.host.trim();
        if host.is_empty() {
            return Err("L'host è obbligatorio".to_string());
        }
        let port: u16 = self
            .port_text
            .trim()
            .parse()
            .map_err(|_| "Porta non valida (1-65535)".to_string())?;
        if port == 0 {
            return Err("Porta non valida (1-65535)".to_string());
        }
        if self.profile.protocol == Protocol::Sftp && self.profile.auth_kind == AuthKind::KeyFile {
            if self.profile.key_path.trim().is_empty() {
                return Err("Specifica il percorso della chiave privata".to_string());
            }
        }
        let mut p = self.profile.clone();
        p.host = host.to_string();
        p.port = port;
        p.username = p.username.trim().to_string();
        if p.name.trim().is_empty() {
            p.name = if p.username.is_empty() {
                p.host.clone()
            } else {
                format!("{}@{}", p.username, p.host)
            };
        } else {
            p.name = p.name.trim().to_string();
        }
        p.key_path = p.key_path.trim().to_string();
        p.remote_dir = p.remote_dir.trim().to_string();
        p.local_dir = p.local_dir.trim().to_string();
        Ok(p)
    }
}

#[derive(Debug, Clone)]
pub enum PromptAction {
    Mkdir { pane: ActivePane },
    Rename { pane: ActivePane, original: FileEntry },
    GotoPath { pane: ActivePane },
    RuntimeSecret { profile: ConnectionProfile },
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteFile { pane: ActivePane, entry: FileEntry },
    DeleteConnection { index: usize, name: String },
}

#[derive(Debug, Clone, Default)]
pub struct LayoutAreas {
    pub panes_area: Rect,
    pub local_pane: Rect,
    pub splitter: Rect,
    pub remote_pane: Rect,
    pub bottom_panel: Rect,
    pub modal_area: Rect,
    pub modal_list_area: Rect,
}

#[derive(Debug, Clone)]
pub struct PreviewState {
    pub title: String,
    pub file_name: String,
    pub is_remote: bool,
    pub entry: Option<FileEntry>,
    pub loading: bool,
    pub raw_bytes: Vec<u8>,
    pub pretty: bool,
    pub wrap: bool,
    pub kind: PreviewKind,
    pub formatted: bool,
    pub lines: Vec<Line<'static>>,
    pub total_size: Option<u64>,
    pub truncated: bool,
    pub scroll: u16,
    pub h_scroll: u16,
}

impl PreviewState {
    pub fn rebuild(&mut self) {
        let built = build_preview(&self.file_name, &self.raw_bytes, self.pretty);
        self.kind = built.kind;
        self.formatted = built.formatted;
        self.lines = built.lines;
        let max_scroll = self.lines.len().saturating_sub(5) as u16;
        self.scroll = self.scroll.min(max_scroll);
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    None,
    SiteManager {
        selected: usize,
    },
    ConnectionForm(ConnectionFormState),
    InputPrompt {
        title: String,
        hint: String,
        value: String,
        masked: bool,
        action: PromptAction,
    },
    Confirm {
        title: String,
        message: String,
        action: ConfirmAction,
    },
    Preview(PreviewState),
    Help,
}

pub struct App {
    pub should_quit: bool,
    pub active_pane: ActivePane,
    pub show_hidden: bool,
    pub bottom_tab: BottomTab,

    // Local browser
    pub local_dir: PathBuf,
    pub local_entries: Vec<FileEntry>,
    pub local_state: TableState,

    // Remote browser
    pub connected_name: Option<String>,
    pub connecting: bool,
    pub remote_dir: String,
    pub remote_entries: Vec<FileEntry>,
    pub remote_state: TableState,

    // Saved connections
    pub connections: Vec<ConnectionProfile>,

    // Transfers & logs
    pub transfers: Vec<TransferItem>,
    pub next_transfer_id: u64,
    pub logs: Vec<LogEntry>,

    // Active modal
    pub modal: Modal,

    // Layout & mouse interaction
    pub left_pane_pct: u16,
    pub dragging_splitter: bool,
    pub areas: LayoutAreas,
    last_click: Option<(ActivePane, usize, Instant)>,

    // Background worker
    worker: RemoteWorkerHandle,
}

impl App {
    pub fn new() -> Self {
        let local_dir = default_local_dir();
        let local_entries = list_local_dir(&local_dir, false).unwrap_or_default();
        let mut local_state = TableState::default();
        if !local_entries.is_empty() {
            local_state.select(Some(0));
        }

        let connections = load_connections();
        let worker = spawn_worker();

        let mut app = Self {
            should_quit: false,
            active_pane: ActivePane::Local,
            show_hidden: false,
            bottom_tab: BottomTab::Transfers,
            local_dir,
            local_entries,
            local_state,
            connected_name: None,
            connecting: false,
            remote_dir: String::new(),
            remote_entries: Vec::new(),
            remote_state: TableState::default(),
            connections,
            transfers: Vec::new(),
            next_transfer_id: 1,
            logs: Vec::new(),
            modal: Modal::None,
            left_pane_pct: DEFAULT_LEFT_PANE_PCT,
            dragging_splitter: false,
            areas: LayoutAreas::default(),
            last_click: None,
            worker,
        };

        app.log(
            LogLevel::Info,
            "KFTP pronto. [s] Site Manager | [n] Nuova conn. | [v/Spazio] Preview | Trascina il separatore col mouse | [?] Aiuto",
        );
        app
    }

    pub fn shutdown(&self) {
        let _ = self.worker.tx.send(WorkerCommand::Shutdown);
    }

    pub fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.logs.push(LogEntry {
            timestamp,
            level,
            message: message.into(),
        });
        if self.logs.len() > 200 {
            let excess = self.logs.len() - 200;
            self.logs.drain(0..excess);
        }
    }

    pub fn poll_worker(&mut self) {
        while let Ok(event) = self.worker.rx.try_recv() {
            match event {
                WorkerEvent::Log { level, message } => self.log(level, message),
                WorkerEvent::Connected {
                    profile_name,
                    remote_pwd,
                    entries,
                } => {
                    self.connecting = false;
                    self.connected_name = Some(profile_name.clone());
                    self.remote_dir = remote_pwd.clone();
                    self.remote_entries = entries;
                    self.remote_state.select(if self.remote_entries.is_empty() {
                        None
                    } else {
                        Some(0)
                    });
                    self.active_pane = ActivePane::Remote;
                    self.log(
                        LogLevel::Success,
                        format!("Connesso a {profile_name} ({remote_pwd})"),
                    );
                }
                WorkerEvent::Disconnected => {
                    self.connecting = false;
                    self.connected_name = None;
                    self.remote_dir.clear();
                    self.remote_entries.clear();
                    self.remote_state.select(None);
                    self.log(LogLevel::Info, "Disconnesso dal server remoto.");
                }
                WorkerEvent::DirListed { path, entries } => {
                    self.remote_dir = path;
                    self.remote_entries = entries;
                    self.remote_state.select(if self.remote_entries.is_empty() {
                        None
                    } else {
                        Some(0)
                    });
                }
                WorkerEvent::TransferProgress {
                    transfer_id,
                    done,
                    total,
                } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == transfer_id) {
                        t.status = TransferStatus::InProgress;
                        t.done = done;
                        t.total = total;
                    }
                }
                WorkerEvent::TransferFinished {
                    transfer_id,
                    ok,
                    message,
                } => {
                    let mut was_upload = false;
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == transfer_id) {
                        was_upload = t.direction == TransferDirection::Upload;
                        if ok {
                            t.status = TransferStatus::Done;
                            t.done = t.total.max(t.done);
                        } else {
                            t.status = TransferStatus::Failed;
                            t.error = Some(message.clone());
                        }
                    }
                    if ok {
                        self.log(LogLevel::Success, message);
                        if was_upload {
                            self.refresh_remote();
                        } else {
                            self.refresh_local();
                        }
                    } else {
                        self.log(LogLevel::Error, message);
                    }
                }
                WorkerEvent::OperationDone {
                    message,
                    refresh_remote,
                } => {
                    self.log(LogLevel::Success, message);
                    if refresh_remote {
                        self.refresh_remote();
                    }
                }
                WorkerEvent::PreviewLoaded {
                    title,
                    bytes,
                    total_size,
                    truncated,
                } => {
                    if let Modal::Preview(state) = &mut self.modal {
                        state.title = title;
                        state.loading = false;
                        state.raw_bytes = bytes;
                        state.total_size = total_size;
                        state.truncated = truncated;
                        state.scroll = 0;
                        state.h_scroll = 0;
                        state.rebuild();
                    }
                }
                WorkerEvent::Error(err) => {
                    self.connecting = false;
                    if let Modal::Preview(state) = &mut self.modal
                        && state.loading
                    {
                        state.loading = false;
                        state.lines = vec![Line::from(Span::styled(
                            format!("Errore caricamento preview: {err}"),
                            Style::default().fg(Color::Red),
                        ))];
                    }
                    self.log(LogLevel::Error, err);
                }
            }
        }
    }

    pub fn refresh_local(&mut self) {
        let prev_name = self.selected_local().map(|e| e.name.clone());
        match list_local_dir(&self.local_dir, self.show_hidden) {
            Ok(entries) => {
                self.local_entries = entries;
                if self.local_entries.is_empty() {
                    self.local_state.select(None);
                } else if let Some(name) = prev_name
                    && let Some(idx) = self.local_entries.iter().position(|e| e.name == name)
                {
                    self.local_state.select(Some(idx));
                } else {
                    self.local_state.select(Some(0));
                }
            }
            Err(err) => {
                self.log(
                    LogLevel::Error,
                    format!("Errore lettura cartella locale: {err}"),
                );
            }
        }
    }

    pub fn refresh_remote(&mut self) {
        if self.connected_name.is_some() && !self.remote_dir.is_empty() {
            let _ = self.worker.tx.send(WorkerCommand::ListDir {
                path: self.remote_dir.clone(),
                show_hidden: self.show_hidden,
            });
        }
    }

    pub fn selected_local(&self) -> Option<&FileEntry> {
        self.local_state
            .selected()
            .and_then(|i| self.local_entries.get(i))
    }

    pub fn selected_remote(&self) -> Option<&FileEntry> {
        self.remote_state
            .selected()
            .and_then(|i| self.remote_entries.get(i))
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match &self.modal {
            Modal::None => self.handle_main_key(key),
            Modal::SiteManager { .. } => self.handle_site_manager_key(key),
            Modal::ConnectionForm(_) => self.handle_form_key(key),
            Modal::InputPrompt { .. } => self.handle_prompt_key(key),
            Modal::Confirm { .. } => self.handle_confirm_key(key),
            Modal::Preview(_) => self.handle_preview_key(key),
            Modal::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.modal = Modal::None;
                }
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let col = mouse.column;
        let row = mouse.row;

        // Handle splitter drag release anywhere
        if self.dragging_splitter {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.resize_splitter_to(col);
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.resize_splitter_to(col);
                    self.dragging_splitter = false;
                    return;
                }
                _ => {
                    self.dragging_splitter = false;
                }
            }
        }

        // If a modal is open, route mouse events to it
        match &mut self.modal {
            Modal::None => {}
            Modal::Preview(state) => {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        let max_scroll = state.lines.len().saturating_sub(5) as u16;
                        state.scroll = (state.scroll + 3).min(max_scroll);
                    }
                    MouseEventKind::ScrollUp => {
                        state.scroll = state.scroll.saturating_sub(3);
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if !rect_contains(self.areas.modal_area, col, row) =>
                    {
                        self.modal = Modal::None;
                    }
                    _ => {}
                }
                return;
            }
            Modal::SiteManager { selected } => {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        if !self.connections.is_empty() {
                            *selected = (*selected + 1).min(self.connections.len() - 1);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        *selected = selected.saturating_sub(1);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if !rect_contains(self.areas.modal_area, col, row) {
                            self.modal = Modal::None;
                        } else if rect_contains(self.areas.modal_list_area, col, row)
                            && !self.connections.is_empty()
                        {
                            let idx = (row - self.areas.modal_list_area.y) as usize;
                            if idx < self.connections.len() {
                                if *selected == idx {
                                    let profile = self.connections[idx].clone();
                                    self.modal = Modal::None;
                                    self.start_connect(profile);
                                } else {
                                    *selected = idx;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                return;
            }
            Modal::Help => {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && !rect_contains(self.areas.modal_area, col, row)
                {
                    self.modal = Modal::None;
                }
                return;
            }
            Modal::ConnectionForm(_) | Modal::InputPrompt { .. } | Modal::Confirm { .. } => {
                return;
            }
        }

        // Main screen mouse interactions
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check splitter hit (including 1-column tolerance on either side)
                let splitter = self.areas.splitter;
                if row >= splitter.y
                    && row < splitter.y.saturating_add(splitter.height)
                    && col >= splitter.x.saturating_sub(1)
                    && col <= splitter.x.saturating_add(splitter.width)
                {
                    self.dragging_splitter = true;
                    return;
                }

                if rect_contains(self.areas.local_pane, col, row) {
                    self.active_pane = ActivePane::Local;
                    self.click_file_pane(ActivePane::Local, row);
                } else if rect_contains(self.areas.remote_pane, col, row) {
                    self.active_pane = ActivePane::Remote;
                    self.click_file_pane(ActivePane::Remote, row);
                } else if rect_contains(self.areas.bottom_panel, col, row)
                    && row == self.areas.bottom_panel.y
                {
                    // Click on bottom panel header switches tab
                    self.bottom_tab = match self.bottom_tab {
                        BottomTab::Transfers => BottomTab::Log,
                        BottomTab::Log => BottomTab::Transfers,
                    };
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click selects file and opens preview
                if rect_contains(self.areas.local_pane, col, row) {
                    self.active_pane = ActivePane::Local;
                    self.select_row_at(ActivePane::Local, row);
                    self.open_preview_selected();
                } else if rect_contains(self.areas.remote_pane, col, row) {
                    self.active_pane = ActivePane::Remote;
                    self.select_row_at(ActivePane::Remote, row);
                    self.open_preview_selected();
                }
            }
            MouseEventKind::ScrollDown => {
                if rect_contains(self.areas.local_pane, col, row) {
                    self.active_pane = ActivePane::Local;
                    self.move_selection(3);
                } else if rect_contains(self.areas.remote_pane, col, row) {
                    self.active_pane = ActivePane::Remote;
                    self.move_selection(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if rect_contains(self.areas.local_pane, col, row) {
                    self.active_pane = ActivePane::Local;
                    self.move_selection(-3);
                } else if rect_contains(self.areas.remote_pane, col, row) {
                    self.active_pane = ActivePane::Remote;
                    self.move_selection(-3);
                }
            }
            _ => {}
        }
    }

    fn resize_splitter_to(&mut self, col: u16) {
        let panes = self.areas.panes_area;
        if panes.width < 10 {
            return;
        }
        let rel = col.saturating_sub(panes.x) as u32;
        let pct = ((rel * 100) / panes.width as u32) as u16;
        self.left_pane_pct = pct.clamp(MIN_LEFT_PANE_PCT, MAX_LEFT_PANE_PCT);
    }

    fn select_row_at(&mut self, pane: ActivePane, row: u16) -> Option<usize> {
        let pane_rect = match pane {
            ActivePane::Local => self.areas.local_pane,
            ActivePane::Remote => self.areas.remote_pane,
        };
        // Table layout inside pane: top border (1) + header row (1) = 2 rows before first data row
        let first_data_y = pane_rect.y.saturating_add(2);
        let bottom_border_y = pane_rect.y.saturating_add(pane_rect.height.saturating_sub(1));
        if row < first_data_y || row >= bottom_border_y {
            return None;
        }
        let rel_row = (row - first_data_y) as usize;
        let (len, state) = match pane {
            ActivePane::Local => (self.local_entries.len(), &mut self.local_state),
            ActivePane::Remote => (self.remote_entries.len(), &mut self.remote_state),
        };
        let idx = state.offset() + rel_row;
        if idx < len {
            state.select(Some(idx));
            Some(idx)
        } else {
            None
        }
    }

    fn click_file_pane(&mut self, pane: ActivePane, row: u16) {
        let Some(idx) = self.select_row_at(pane, row) else {
            return;
        };
        let now = Instant::now();
        let is_double = self.last_click.is_some_and(|(last_pane, last_idx, ts)| {
            last_pane == pane
                && last_idx == idx
                && now.duration_since(ts) <= Duration::from_millis(450)
        });

        if is_double {
            self.last_click = None;
            self.activate_selected();
        } else {
            self.last_click = Some((pane, idx, now));
        }
    }

    fn handle_main_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => {
                self.active_pane = match self.active_pane {
                    ActivePane::Local => ActivePane::Remote,
                    ActivePane::Remote => ActivePane::Local,
                };
            }
            KeyCode::Left | KeyCode::Char('h') => self.active_pane = ActivePane::Local,
            KeyCode::Right | KeyCode::Char('l') => self.active_pane = ActivePane::Remote,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Backspace => self.go_parent(),
            KeyCode::Char('v') | KeyCode::Char(' ') => self.open_preview_selected(),
            KeyCode::Char('[') => {
                self.left_pane_pct = self
                    .left_pane_pct
                    .saturating_sub(4)
                    .clamp(MIN_LEFT_PANE_PCT, MAX_LEFT_PANE_PCT);
            }
            KeyCode::Char(']') => {
                self.left_pane_pct = (self.left_pane_pct + 4).clamp(MIN_LEFT_PANE_PCT, MAX_LEFT_PANE_PCT);
            }
            KeyCode::Char('u') => self.trigger_transfer(),
            KeyCode::Char('s') => {
                let selected = if self.connections.is_empty() { 0 } else { 0 };
                self.modal = Modal::SiteManager { selected };
            }
            KeyCode::Char('n') => {
                self.modal = Modal::ConnectionForm(ConnectionFormState::new_profile());
            }
            KeyCode::Char('D') => {
                if self.connected_name.is_some() || self.connecting {
                    let _ = self.worker.tx.send(WorkerCommand::Disconnect);
                }
            }
            KeyCode::Char('m') => {
                if self.active_pane == ActivePane::Remote && self.connected_name.is_none() {
                    self.log(LogLevel::Warn, "Connettiti prima a un server remoto.");
                    return;
                }
                let pane = self.active_pane;
                self.modal = Modal::InputPrompt {
                    title: match pane {
                        ActivePane::Local => "Nuova cartella locale".to_string(),
                        ActivePane::Remote => "Nuova cartella remota".to_string(),
                    },
                    hint: "Inserisci il nome della nuova cartella".to_string(),
                    value: String::new(),
                    masked: false,
                    action: PromptAction::Mkdir { pane },
                };
            }
            KeyCode::Char('d') | KeyCode::Delete => self.prompt_delete_selected(),
            KeyCode::Char('r') => self.prompt_rename_selected(),
            KeyCode::Char('p') => {
                if self.active_pane == ActivePane::Remote && self.connected_name.is_none() {
                    self.log(LogLevel::Warn, "Connettiti prima a un server remoto.");
                    return;
                }
                let pane = self.active_pane;
                let current = match pane {
                    ActivePane::Local => self.local_dir.to_string_lossy().to_string(),
                    ActivePane::Remote => self.remote_dir.clone(),
                };
                self.modal = Modal::InputPrompt {
                    title: match pane {
                        ActivePane::Local => "Vai a percorso locale".to_string(),
                        ActivePane::Remote => "Vai a percorso remoto".to_string(),
                    },
                    hint: "Inserisci il percorso di destinazione".to_string(),
                    value: current,
                    masked: false,
                    action: PromptAction::GotoPath { pane },
                };
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.refresh_local();
                self.refresh_remote();
            }
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.refresh_local();
                self.refresh_remote();
            }
            KeyCode::Char('t') => {
                self.bottom_tab = match self.bottom_tab {
                    BottomTab::Transfers => BottomTab::Log,
                    BottomTab::Log => BottomTab::Transfers,
                };
            }
            KeyCode::Char('c') => {
                self.transfers.retain(|t| {
                    matches!(t.status, TransferStatus::Queued | TransferStatus::InProgress)
                });
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.modal = Modal::Help;
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let (len, state) = match self.active_pane {
            ActivePane::Local => (self.local_entries.len(), &mut self.local_state),
            ActivePane::Remote => (self.remote_entries.len(), &mut self.remote_state),
        };
        if len == 0 {
            state.select(None);
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, (len - 1) as isize) as usize;
        state.select(Some(next));
    }

    fn select_first(&mut self) {
        match self.active_pane {
            ActivePane::Local if !self.local_entries.is_empty() => self.local_state.select(Some(0)),
            ActivePane::Remote if !self.remote_entries.is_empty() => {
                self.remote_state.select(Some(0))
            }
            _ => {}
        }
    }

    fn select_last(&mut self) {
        match self.active_pane {
            ActivePane::Local if !self.local_entries.is_empty() => {
                self.local_state.select(Some(self.local_entries.len() - 1));
            }
            ActivePane::Remote if !self.remote_entries.is_empty() => {
                self.remote_state
                    .select(Some(self.remote_entries.len() - 1));
            }
            _ => {}
        }
    }

    fn activate_selected(&mut self) {
        match self.active_pane {
            ActivePane::Local => {
                let Some(entry) = self.selected_local().cloned() else {
                    return;
                };
                if entry.is_dir {
                    self.local_dir = PathBuf::from(&entry.path);
                    self.refresh_local();
                } else {
                    self.queue_upload(&entry);
                }
            }
            ActivePane::Remote => {
                let Some(entry) = self.selected_remote().cloned() else {
                    return;
                };
                if entry.is_dir {
                    let _ = self.worker.tx.send(WorkerCommand::ListDir {
                        path: entry.path,
                        show_hidden: self.show_hidden,
                    });
                } else {
                    self.queue_download(&entry);
                }
            }
        }
    }

    fn go_parent(&mut self) {
        match self.active_pane {
            ActivePane::Local => {
                if let Some(parent) = self.local_dir.parent() {
                    self.local_dir = parent.to_path_buf();
                    self.refresh_local();
                }
            }
            ActivePane::Remote => {
                if self.connected_name.is_some() && self.remote_dir != "/" {
                    let parent = remote_parent(&self.remote_dir);
                    let _ = self.worker.tx.send(WorkerCommand::ListDir {
                        path: parent,
                        show_hidden: self.show_hidden,
                    });
                }
            }
        }
    }

    fn open_preview_selected(&mut self) {
        match self.active_pane {
            ActivePane::Local => {
                let Some(entry) = self.selected_local().cloned() else {
                    return;
                };
                if entry.is_dir {
                    self.log(LogLevel::Warn, "La preview è disponibile solo per i file.");
                    return;
                }
                match read_local_preview(&entry.path, PREVIEW_MAX_BYTES) {
                    Ok((bytes, total_size, truncated)) => {
                        let mut state = PreviewState {
                            title: entry.path.clone(),
                            file_name: entry.name.clone(),
                            is_remote: false,
                            entry: Some(entry),
                            loading: false,
                            raw_bytes: bytes,
                            pretty: true,
                            wrap: false,
                            kind: PreviewKind::PlainText,
                            formatted: false,
                            lines: Vec::new(),
                            total_size,
                            truncated,
                            scroll: 0,
                            h_scroll: 0,
                        };
                        state.rebuild();
                        self.modal = Modal::Preview(state);
                    }
                    Err(err) => {
                        self.log(LogLevel::Error, format!("Errore preview locale: {err}"));
                    }
                }
            }
            ActivePane::Remote => {
                if self.connected_name.is_none() {
                    return;
                }
                let Some(entry) = self.selected_remote().cloned() else {
                    return;
                };
                if entry.is_dir {
                    self.log(LogLevel::Warn, "La preview è disponibile solo per i file.");
                    return;
                }
                let remote_path = entry.path.clone();
                let file_name = entry.name.clone();
                self.modal = Modal::Preview(PreviewState {
                    title: remote_path.clone(),
                    file_name,
                    is_remote: true,
                    entry: Some(entry),
                    loading: true,
                    raw_bytes: Vec::new(),
                    pretty: true,
                    wrap: false,
                    kind: PreviewKind::PlainText,
                    formatted: false,
                    lines: vec![Line::from(Span::styled(
                        "Caricamento preview dal server remoto...",
                        Style::default().fg(Color::Yellow),
                    ))],
                    total_size: None,
                    truncated: false,
                    scroll: 0,
                    h_scroll: 0,
                });
                let _ = self.worker.tx.send(WorkerCommand::Preview {
                    remote_path,
                    max_bytes: PREVIEW_MAX_BYTES,
                });
            }
        }
    }

    fn handle_preview_key(&mut self, key: KeyEvent) {
        let Modal::Preview(mut state) = std::mem::replace(&mut self.modal, Modal::None) else {
            return;
        };

        let max_scroll = state.lines.len().saturating_sub(5) as u16;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') | KeyCode::Char(' ') => {
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.scroll = (state.scroll + 1).min(max_scroll);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.scroll = state.scroll.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !state.wrap {
                    state.h_scroll = state.h_scroll.saturating_add(4);
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                state.h_scroll = state.h_scroll.saturating_sub(4);
            }
            KeyCode::PageDown => {
                state.scroll = (state.scroll + 15).min(max_scroll);
            }
            KeyCode::PageUp => {
                state.scroll = state.scroll.saturating_sub(15);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                state.scroll = 0;
                state.h_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                state.scroll = max_scroll;
            }
            KeyCode::Char('f') => {
                if !state.loading {
                    state.pretty = !state.pretty;
                    state.scroll = 0;
                    state.h_scroll = 0;
                    state.rebuild();
                }
            }
            KeyCode::Char('w') => {
                state.wrap = !state.wrap;
                if state.wrap {
                    state.h_scroll = 0;
                }
            }
            KeyCode::Char('u') | KeyCode::Enter => {
                if let Some(entry) = state.entry.clone() {
                    if state.is_remote {
                        self.queue_download(&entry);
                    } else {
                        self.queue_upload(&entry);
                    }
                    return;
                }
            }
            _ => {}
        }

        self.modal = Modal::Preview(state);
    }

    fn trigger_transfer(&mut self) {
        match self.active_pane {
            ActivePane::Local => {
                if let Some(entry) = self.selected_local().cloned() {
                    if entry.is_dir {
                        self.log(
                            LogLevel::Warn,
                            "Seleziona un file da caricare (upload directory intere non ancora supportato).",
                        );
                    } else {
                        self.queue_upload(&entry);
                    }
                }
            }
            ActivePane::Remote => {
                if let Some(entry) = self.selected_remote().cloned() {
                    if entry.is_dir {
                        self.log(
                            LogLevel::Warn,
                            "Seleziona un file da scaricare (download directory intere non ancora supportato).",
                        );
                    } else {
                        self.queue_download(&entry);
                    }
                }
            }
        }
    }

    fn queue_upload(&mut self, entry: &FileEntry) {
        if self.connected_name.is_none() {
            self.log(
                LogLevel::Warn,
                "Connettiti prima a un server remoto per caricare file.",
            );
            return;
        }
        let transfer_id = self.next_transfer_id;
        self.next_transfer_id += 1;

        let local_path = PathBuf::from(&entry.path);
        let remote_path = remote_join(&self.remote_dir, &entry.name);

        self.transfers.push(TransferItem {
            id: transfer_id,
            direction: TransferDirection::Upload,
            file_name: entry.name.clone(),
            done: 0,
            total: entry.size,
            status: TransferStatus::Queued,
            error: None,
        });
        self.bottom_tab = BottomTab::Transfers;
        self.log(
            LogLevel::Info,
            format!("In coda upload: {} -> {}", entry.name, remote_path),
        );
        let _ = self.worker.tx.send(WorkerCommand::Upload {
            local_path,
            remote_path,
            transfer_id,
        });
    }

    fn queue_download(&mut self, entry: &FileEntry) {
        if self.connected_name.is_none() {
            return;
        }
        let transfer_id = self.next_transfer_id;
        self.next_transfer_id += 1;

        let local_path = self.local_dir.join(&entry.name);
        let remote_path = entry.path.clone();

        self.transfers.push(TransferItem {
            id: transfer_id,
            direction: TransferDirection::Download,
            file_name: entry.name.clone(),
            done: 0,
            total: entry.size,
            status: TransferStatus::Queued,
            error: None,
        });
        self.bottom_tab = BottomTab::Transfers;
        self.log(
            LogLevel::Info,
            format!(
                "In coda download: {} -> {}",
                entry.name,
                local_path.display()
            ),
        );
        let _ = self.worker.tx.send(WorkerCommand::Download {
            remote_path,
            local_path,
            transfer_id,
            total_size: entry.size,
        });
    }

    fn prompt_delete_selected(&mut self) {
        let pane = self.active_pane;
        let entry = match pane {
            ActivePane::Local => self.selected_local().cloned(),
            ActivePane::Remote => self.selected_remote().cloned(),
        };
        let Some(entry) = entry else { return };
        if entry.is_parent {
            return;
        }
        let kind = if entry.is_dir { "cartella" } else { "file" };
        let where_str = match pane {
            ActivePane::Local => "locale",
            ActivePane::Remote => "remoto",
        };
        self.modal = Modal::Confirm {
            title: "Conferma eliminazione".to_string(),
            message: format!("Eliminare {kind} {where_str} '{}'?", entry.name),
            action: ConfirmAction::DeleteFile { pane, entry },
        };
    }

    fn prompt_rename_selected(&mut self) {
        let pane = self.active_pane;
        let entry = match pane {
            ActivePane::Local => self.selected_local().cloned(),
            ActivePane::Remote => self.selected_remote().cloned(),
        };
        let Some(entry) = entry else { return };
        if entry.is_parent {
            return;
        }
        self.modal = Modal::InputPrompt {
            title: "Rinomina".to_string(),
            hint: format!("Nuovo nome per '{}'", entry.name),
            value: entry.name.clone(),
            masked: false,
            action: PromptAction::Rename {
                pane,
                original: entry,
            },
        };
    }

    fn handle_site_manager_key(&mut self, key: KeyEvent) {
        let Modal::SiteManager { mut selected } = self.modal else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.connections.is_empty() {
                    selected = (selected + 1).min(self.connections.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
            }
            KeyCode::Char('n') => {
                self.modal = Modal::ConnectionForm(ConnectionFormState::new_profile());
                return;
            }
            KeyCode::Char('e') => {
                if let Some(profile) = self.connections.get(selected).cloned() {
                    self.modal =
                        Modal::ConnectionForm(ConnectionFormState::from_existing(selected, profile));
                    return;
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(profile) = self.connections.get(selected) {
                    let name = profile.name.clone();
                    self.modal = Modal::Confirm {
                        title: "Elimina connessione".to_string(),
                        message: format!("Eliminare il profilo salvato '{name}'?"),
                        action: ConfirmAction::DeleteConnection {
                            index: selected,
                            name,
                        },
                    };
                    return;
                }
            }
            KeyCode::Enter => {
                if let Some(profile) = self.connections.get(selected).cloned() {
                    self.modal = Modal::None;
                    self.start_connect(profile);
                    return;
                }
            }
            _ => {}
        }
        self.modal = Modal::SiteManager { selected };
    }

    fn start_connect(&mut self, profile: ConnectionProfile) {
        if !profile.local_dir.trim().is_empty() {
            let expanded = expand_tilde(profile.local_dir.trim());
            if expanded.is_dir() {
                self.local_dir = expanded;
                self.refresh_local();
            }
        }

        // If Password auth and no saved secret, ask at runtime.
        let needs_password = profile.auth_kind == AuthKind::Password
            && profile.secret.is_empty()
            && !profile.username.is_empty();

        if needs_password {
            self.modal = Modal::InputPrompt {
                title: format!("Password per {}@{}", profile.username, profile.host),
                hint: "Inserisci la password (non verrà salvata)".to_string(),
                value: String::new(),
                masked: true,
                action: PromptAction::RuntimeSecret { profile },
            };
            return;
        }

        self.connecting = true;
        self.log(
            LogLevel::Info,
            format!(
                "Avvio connessione {} ({}://{}:{})",
                profile.name,
                profile.protocol.label(),
                profile.host,
                profile.port
            ),
        );
        let _ = self.worker.tx.send(WorkerCommand::Connect {
            profile,
            runtime_secret: None,
        });
    }

    fn handle_form_key(&mut self, key: KeyEvent) {
        let Modal::ConnectionForm(mut form) = std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };

        form.error = None;

        // Ctrl+S: Save only; Ctrl+Enter or F2: Save & Connect
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            match form.build_profile() {
                Ok(profile) => {
                    self.save_profile(form.editing_index, profile);
                    let idx = form
                        .editing_index
                        .unwrap_or_else(|| self.connections.len().saturating_sub(1));
                    self.modal = Modal::SiteManager { selected: idx };
                    return;
                }
                Err(msg) => {
                    form.error = Some(msg);
                    self.modal = Modal::ConnectionForm(form);
                    return;
                }
            }
        }

        match key.code {
            KeyCode::Esc => {
                let selected = form.editing_index.unwrap_or(0);
                self.modal = Modal::SiteManager {
                    selected: selected.min(self.connections.len().saturating_sub(1)),
                };
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                form.next_field();
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.prev_field();
            }
            KeyCode::Enter => match form.build_profile() {
                Ok(profile) => {
                    self.save_profile(form.editing_index, profile.clone());
                    self.start_connect(profile);
                    return;
                }
                Err(msg) => {
                    form.error = Some(msg);
                }
            },
            KeyCode::Left => match form.focused {
                FormField::Protocol => {
                    let old_default = form.profile.protocol.default_port().to_string();
                    form.profile.protocol = form.profile.protocol.prev();
                    if form.port_text == old_default {
                        form.port_text = form.profile.protocol.default_port().to_string();
                    }
                    if form.profile.protocol != Protocol::Sftp {
                        form.profile.auth_kind = AuthKind::Password;
                    }
                    form.ensure_valid_focus();
                }
                FormField::AuthKind => {
                    form.profile.auth_kind = form.profile.auth_kind.prev(form.profile.protocol);
                    form.ensure_valid_focus();
                }
                FormField::SaveSecret => {
                    form.profile.save_secret = !form.profile.save_secret;
                }
                _ => {}
            },
            KeyCode::Right => match form.focused {
                FormField::Protocol => {
                    let old_default = form.profile.protocol.default_port().to_string();
                    form.profile.protocol = form.profile.protocol.next();
                    if form.port_text == old_default {
                        form.port_text = form.profile.protocol.default_port().to_string();
                    }
                    if form.profile.protocol != Protocol::Sftp {
                        form.profile.auth_kind = AuthKind::Password;
                    }
                    form.ensure_valid_focus();
                }
                FormField::AuthKind => {
                    form.profile.auth_kind = form.profile.auth_kind.next(form.profile.protocol);
                    form.ensure_valid_focus();
                }
                FormField::SaveSecret => {
                    form.profile.save_secret = !form.profile.save_secret;
                }
                _ => {}
            },
            KeyCode::Char(' ')
                if matches!(
                    form.focused,
                    FormField::Protocol | FormField::AuthKind | FormField::SaveSecret
                ) =>
            {
                match form.focused {
                    FormField::Protocol => {
                        let old_default = form.profile.protocol.default_port().to_string();
                        form.profile.protocol = form.profile.protocol.next();
                        if form.port_text == old_default {
                            form.port_text = form.profile.protocol.default_port().to_string();
                        }
                        if form.profile.protocol != Protocol::Sftp {
                            form.profile.auth_kind = AuthKind::Password;
                        }
                        form.ensure_valid_focus();
                    }
                    FormField::AuthKind => {
                        form.profile.auth_kind = form.profile.auth_kind.next(form.profile.protocol);
                        form.ensure_valid_focus();
                    }
                    FormField::SaveSecret => {
                        form.profile.save_secret = !form.profile.save_secret;
                    }
                    _ => {}
                }
            }
            KeyCode::Backspace => match form.focused {
                FormField::Name => {
                    form.profile.name.pop();
                }
                FormField::Host => {
                    form.profile.host.pop();
                }
                FormField::Port => {
                    form.port_text.pop();
                }
                FormField::Username => {
                    form.profile.username.pop();
                }
                FormField::KeyPath => {
                    form.profile.key_path.pop();
                }
                FormField::Secret => {
                    form.profile.secret.pop();
                }
                FormField::RemoteDir => {
                    form.profile.remote_dir.pop();
                }
                FormField::LocalDir => {
                    form.profile.local_dir.pop();
                }
                _ => {}
            },
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    match form.focused {
                        FormField::Name => form.profile.name.push(ch),
                        FormField::Host => form.profile.host.push(ch),
                        FormField::Port if ch.is_ascii_digit() && form.port_text.len() < 5 => {
                            form.port_text.push(ch);
                        }
                        FormField::Username => form.profile.username.push(ch),
                        FormField::KeyPath => form.profile.key_path.push(ch),
                        FormField::Secret => form.profile.secret.push(ch),
                        FormField::RemoteDir => form.profile.remote_dir.push(ch),
                        FormField::LocalDir => form.profile.local_dir.push(ch),
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        self.modal = Modal::ConnectionForm(form);
    }

    fn save_profile(&mut self, editing_index: Option<usize>, profile: ConnectionProfile) {
        if let Some(idx) = editing_index
            && idx < self.connections.len()
        {
            self.connections[idx] = profile.clone();
        } else {
            self.connections.push(profile.clone());
        }
        match save_connections(&self.connections) {
            Ok(path) => {
                self.log(
                    LogLevel::Success,
                    format!(
                        "Profilo '{}' salvato in {}",
                        profile.name,
                        path.display()
                    ),
                );
            }
            Err(err) => {
                self.log(
                    LogLevel::Error,
                    format!("Errore salvataggio connessioni: {err}"),
                );
            }
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Modal::InputPrompt {
            title,
            hint,
            mut value,
            masked,
            action,
        } = std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };

        match key.code {
            KeyCode::Esc => {
                return;
            }
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Enter => {
                self.execute_prompt_action(action, value);
                return;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(ch);
            }
            _ => {}
        }

        self.modal = Modal::InputPrompt {
            title,
            hint,
            value,
            masked,
            action,
        };
    }

    fn execute_prompt_action(&mut self, action: PromptAction, value: String) {
        let trimmed = value.trim();
        match action {
            PromptAction::Mkdir { pane } => {
                if trimmed.is_empty() {
                    return;
                }
                match pane {
                    ActivePane::Local => {
                        let target = self.local_dir.join(trimmed);
                        match std::fs::create_dir_all(&target) {
                            Ok(()) => {
                                self.log(
                                    LogLevel::Success,
                                    format!("Cartella locale creata: {}", target.display()),
                                );
                                self.refresh_local();
                            }
                            Err(err) => {
                                self.log(
                                    LogLevel::Error,
                                    format!("Errore creazione cartella locale: {err}"),
                                );
                            }
                        }
                    }
                    ActivePane::Remote => {
                        let remote_path = remote_join(&self.remote_dir, trimmed);
                        let _ = self.worker.tx.send(WorkerCommand::Mkdir { remote_path });
                    }
                }
            }
            PromptAction::Rename { pane, original } => {
                if trimmed.is_empty() || trimmed == original.name {
                    return;
                }
                match pane {
                    ActivePane::Local => {
                        let from = PathBuf::from(&original.path);
                        let to = self.local_dir.join(trimmed);
                        match std::fs::rename(&from, &to) {
                            Ok(()) => {
                                self.log(
                                    LogLevel::Success,
                                    format!("Rinominato localmente: {} -> {trimmed}", original.name),
                                );
                                self.refresh_local();
                            }
                            Err(err) => {
                                self.log(
                                    LogLevel::Error,
                                    format!("Errore rinomina locale: {err}"),
                                );
                            }
                        }
                    }
                    ActivePane::Remote => {
                        let to = remote_join(&self.remote_dir, trimmed);
                        let _ = self.worker.tx.send(WorkerCommand::Rename {
                            from: original.path,
                            to,
                        });
                    }
                }
            }
            PromptAction::GotoPath { pane } => {
                if trimmed.is_empty() {
                    return;
                }
                match pane {
                    ActivePane::Local => {
                        let target = expand_tilde(trimmed);
                        if target.is_dir() {
                            self.local_dir = target;
                            self.refresh_local();
                        } else {
                            self.log(
                                LogLevel::Error,
                                format!("Percorso locale non valido: {}", target.display()),
                            );
                        }
                    }
                    ActivePane::Remote => {
                        let _ = self.worker.tx.send(WorkerCommand::ListDir {
                            path: trimmed.to_string(),
                            show_hidden: self.show_hidden,
                        });
                    }
                }
            }
            PromptAction::RuntimeSecret { profile } => {
                self.connecting = true;
                self.log(
                    LogLevel::Info,
                    format!("Connessione a {}...", profile.name),
                );
                let _ = self.worker.tx.send(WorkerCommand::Connect {
                    profile,
                    runtime_secret: Some(value),
                });
            }
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        let Modal::Confirm {
            title,
            message,
            action,
        } = std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.execute_confirm_action(action);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
            _ => {
                self.modal = Modal::Confirm {
                    title,
                    message,
                    action,
                };
            }
        }
    }

    fn execute_confirm_action(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteFile { pane, entry } => match pane {
                ActivePane::Local => {
                    let path = PathBuf::from(&entry.path);
                    let res = if entry.is_dir {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    match res {
                        Ok(()) => {
                            self.log(
                                LogLevel::Success,
                                format!("Eliminato localmente: {}", entry.name),
                            );
                            self.refresh_local();
                        }
                        Err(err) => {
                            self.log(
                                LogLevel::Error,
                                format!("Errore eliminazione locale: {err}"),
                            );
                        }
                    }
                }
                ActivePane::Remote => {
                    let _ = self.worker.tx.send(WorkerCommand::Delete {
                        remote_path: entry.path,
                        is_dir: entry.is_dir,
                    });
                }
            },
            ConfirmAction::DeleteConnection { index, name } => {
                if index < self.connections.len() {
                    self.connections.remove(index);
                    match save_connections(&self.connections) {
                        Ok(_) => {
                            self.log(LogLevel::Success, format!("Profilo '{name}' eliminato."));
                        }
                        Err(err) => {
                            self.log(
                                LogLevel::Error,
                                format!("Errore salvataggio connessioni: {err}"),
                            );
                        }
                    }
                }
                let next_sel = index.min(self.connections.len().saturating_sub(1));
                self.modal = Modal::SiteManager { selected: next_sel };
            }
        }
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn read_local_preview(path: &str, max_bytes: usize) -> anyhow::Result<(Vec<u8>, Option<u64>, bool)> {
    let total_size = std::fs::metadata(path).ok().map(|m| m.len());
    let mut file = std::fs::File::open(path)?;
    let mut out = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    while out.len() <= max_bytes {
        let want = (max_bytes + 1 - out.len()).min(buf.len());
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    if out.len() > max_bytes {
        out.truncate(max_bytes);
        truncated = true;
    }
    Ok((out, total_size, truncated))
}
