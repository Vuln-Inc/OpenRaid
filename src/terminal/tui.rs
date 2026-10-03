//! A live operator console. Board navigation is exclusively global cursor pagination.
#[path = "selection.rs"]
mod selection;
use crate::{
    metrics::{AgentStatus, Metrics, MetricsSnapshot},
    quick::{Entry, ProviderManager},
    storage::{BoardMessage, Store},
    tui_menu::{Action, Kind, Menu, Outcome},
};
use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
        TableState, Tabs, Wrap,
    },
    Frame, Terminal,
};
use std::{
    collections::VecDeque,
    io::{self, Stdout},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

const PAGE_SIZE: usize = 100;
#[path = "tps.rs"]
mod tps;
const SLATE: Color = Color::Rgb(21, 38, 56);
const ICE: Color = Color::Rgb(220, 231, 239);
const COBALT: Color = Color::Rgb(168, 184, 255);
const SEA: Color = Color::Rgb(121, 201, 187);
const AMBER: Color = Color::Rgb(232, 186, 120);
const ROSE: Color = Color::Rgb(232, 135, 152);
const MUTED: Color = Color::Rgb(143, 163, 184);

#[derive(Clone, Default)]
pub struct SessionInfo {
    pub provider: String,
    pub model: String,
    pub variant: String,
    pub objective: String,
}

const COMMANDS: [(&str, &str, KeyCode); 21] = [
    ("Send an owner instruction", "o", KeyCode::Char('o')),
    ("Toggle live following", "f", KeyCode::Char('f')),
    ("Focus the shared board", "1", KeyCode::Char('1')),
    ("Focus the agent roster", "2", KeyCode::Char('2')),
    ("Focus the selected agent stream", "3", KeyCode::Char('3')),
    ("Show all keyboard controls", "?", KeyCode::Char('h')),
    ("Close / detach the console", "q", KeyCode::Char('q')),
    (
        "Switch provider / model",
        "Ctrl+X M · /models",
        KeyCode::F(2),
    ),
    ("Connect a provider", "Ctrl+X C · /connect", KeyCode::F(3)),
    (
        "Choose thinking variant",
        "Ctrl+X T · /variant",
        KeyCode::F(4),
    ),
    ("Jump to a sent prompt", "/jump", KeyCode::F(5)),
    ("Show tiled agents", "/agents", KeyCode::F(6)),
    ("Manage parallel agents", "/members", KeyCode::F(7)),
    (
        "Add parallel agents",
        "Ctrl+X + · /add [count]",
        KeyCode::F(8),
    ),
    (
        "Remove parallel agents",
        "Ctrl+X - · /remove [IDs]",
        KeyCode::F(9),
    ),
    (
        "Browse workspace sessions",
        "Ctrl+X S · /sessions",
        KeyCode::F(10),
    ),
    ("Create a new session", "Ctrl+X N · /new", KeyCode::F(11)),
    ("Start work / edit prompt", "/start", KeyCode::Char('o')),
    (
        "Pause current work",
        "Ctrl+X P · /pause",
        KeyCode::Char('p'),
    ),
    (
        "Resume paused work",
        "Ctrl+X R · /resume",
        KeyCode::Char('r'),
    ),
    ("Stop current work", "Ctrl+X X · /stop", KeyCode::F(12)),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Board,
    Agents,
    Detail,
}

struct UiState {
    board: Vec<BoardMessage>,
    after: u64,
    latest: u64,
    selected: usize,
    table: TableState,
    focus: Focus,
    board_scroll: u16,
    detail_scroll: u16,
    detail_follow: bool,
    follow: bool,
    composing: bool,
    draft: String,
    help: bool,
    notice: String,
    rates: VecDeque<u64>,
    throughput: tps::Throughput,
    tps: f64,
    session: SessionInfo,
    palette: bool,
    palette_selected: usize,
    panels: [Rect; 3],
    managed: bool,
    leader: bool,
    menu: Option<Menu>,
    actions: VecDeque<Action>,
    menu_epoch: u64,
    pending_submit: bool,
    grid: bool,
    grid_page_size: usize,
    grid_hits: Vec<(Rect, usize)>,
    prompt_hits: Vec<(Rect, u64)>,
    busy: bool,
    model_search: Option<String>,
    palette_editing: bool,
    member_count: Option<usize>,
    draining_count: usize,
    current_votes: Option<usize>,
    paused: bool,
    stopping: bool,
    session_id: String,
    workspace: String,
    database: String,
    control_hits: Vec<(Rect, Action)>,
    selection: selection::Selection,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            board: Vec::new(),
            after: 0,
            latest: 0,
            selected: 0,
            table: TableState::default().with_selected(0),
            focus: Focus::Agents,
            board_scroll: 0,
            detail_scroll: 0,
            detail_follow: true,
            follow: true,
            composing: false,
            draft: String::new(),
            help: false,
            notice: String::new(),
            rates: VecDeque::with_capacity(120),
            throughput: tps::Throughput::default(),
            tps: 0.0,
            session: SessionInfo::default(),
            palette: false,
            palette_selected: 0,
            panels: [Rect::default(); 3],
            managed: false,
            leader: false,
            menu: None,
            actions: VecDeque::new(),
            menu_epoch: 0,
            pending_submit: false,
            grid: false,
            grid_page_size: 1,
            grid_hits: Vec::new(),
            prompt_hits: Vec::new(),
            busy: false,
            model_search: None,
            palette_editing: false,
            member_count: None,
            draining_count: 0,
            current_votes: None,
            paused: false,
            stopping: false,
            session_id: String::new(),
            workspace: String::new(),
            database: String::new(),
            control_hits: Vec::new(),
            selection: selection::Selection::default(),
        }
    }
}

impl UiState {
    async fn load(&mut self, store: &Store) -> Result<()> {
        self.latest = store.latest_seq().await?;
        if self.follow {
            self.after = self.latest.saturating_sub(PAGE_SIZE as u64);
        }
        self.board = store.read_board(self.after, PAGE_SIZE).await?;
        Ok(())
    }

    fn sample(&mut self, snapshot: &MetricsSnapshot) {
        if let Some(rate) = self
            .throughput
            .sample(Instant::now(), snapshot.output_tokens)
        {
            self.tps = rate;
            self.rates.push_back(self.tps.round() as u64);
            if self.rates.len() > 120 {
                self.rates.pop_front();
            }
        }
    }

    async fn key(&mut self, mut key: KeyEvent, store: &Store, agent_count: usize) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Char('h')
                    | KeyCode::Char('?')
                    | KeyCode::Enter
                    | KeyCode::F(1)
            ) {
                self.help = false;
            }
            return Ok(false);
        }
        if self.leader {
            self.leader = false;
            self.notice.clear();
            match key.code {
                KeyCode::Char('m') => self.actions.push_back(Action::Models),
                KeyCode::Char('c') => self.actions.push_back(Action::Connect),
                KeyCode::Char('t') => self.actions.push_back(Action::Variants),
                KeyCode::Char('j') => self.actions.push_back(Action::JumpList),
                KeyCode::Char('a') => self.actions.push_back(Action::Grid),
                KeyCode::Char('s') => self.actions.push_back(Action::Sessions),
                KeyCode::Char('n') => self.actions.push_back(Action::NewSession),
                KeyCode::Char('p') => self.actions.push_back(Action::Pause),
                KeyCode::Char('r') => self.actions.push_back(Action::Resume),
                KeyCode::Char('x') => self.actions.push_back(Action::Stop),
                KeyCode::Char('+') | KeyCode::Char('=') => self.actions.push_back(Action::AddList),
                KeyCode::Char('-') => self.actions.push_back(Action::RemoveList),
                _ => {}
            }
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if key.code == KeyCode::Char('p') {
                self.menu = None;
                self.menu_epoch += 1;
                self.palette_editing = self.composing;
                self.composing = false;
                self.palette = true;
                return Ok(false);
            }
            if key.code == KeyCode::Char('x') {
                self.menu = None;
                self.menu_epoch += 1;
                self.leader = true;
                self.notice =
                    "Ctrl+X: S sessions · N new · P pause · R resume · X stop · M models · +/- agents".into();
                return Ok(false);
            }
            if key.code == KeyCode::Char('t') {
                self.actions.push_back(Action::Cycle);
                return Ok(false);
            }
        }
        if let Some(menu) = &mut self.menu {
            match menu.key(key) {
                Outcome::None => {}
                Outcome::Close => {
                    self.menu = None;
                    self.menu_epoch += 1;
                }
                Outcome::Action(action) => {
                    self.menu = None;
                    self.actions.push_back(action);
                }
                Outcome::EndpointDone(provider, endpoint) => {
                    self.menu = Some(Menu::new(
                        Kind::Key {
                            provider,
                            endpoint: Some(endpoint),
                        },
                        Vec::new(),
                        None,
                    ));
                }
                Outcome::Provider(provider) => {
                    self.actions.push_back(Action::ChooseProvider(provider));
                    self.menu = None;
                }
            }
            return Ok(false);
        }
        if self.composing {
            match key.code {
                KeyCode::Esc => {
                    self.menu_epoch += 1;
                    self.composing = false;
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.draft.push('\n')
                }
                KeyCode::Enter => {
                    if !self.draft.trim().is_empty() {
                        if let Some(action) = crate::tui_menu::command(&self.draft) {
                            self.actions.push_back(action);
                            self.draft.clear();
                            return Ok(false);
                        }
                        if self.managed {
                            if !self.pending_submit {
                                self.actions.push_back(Action::Submit(self.draft.clone()));
                                self.pending_submit = true;
                            }
                            return Ok(false);
                        }
                        let message = store.append("owner", &self.draft, true).await?;
                        self.notice = format!(
                            "owner message #{} committed; completion votes revoked",
                            message.seq
                        );
                        self.draft.clear();
                        self.composing = false;
                        self.follow = true;
                        self.load(store).await?;
                    }
                }
                KeyCode::Backspace => {
                    self.draft.pop();
                }
                KeyCode::Char('u')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.draft.clear()
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        // Windows keyboard layouts report printable AltGr characters
                        // as Ctrl+Alt (for example Turkish i and İ). They are text,
                        // not shortcuts, inside the owner composer.
                        || key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.draft.push(c)
                }
                _ => {}
            }
            if self.draft == "/" {
                self.menu = Some(Menu::commands());
                self.draft.clear();
            }
            return Ok(false);
        }
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Char('h')
                    | KeyCode::Char('?')
                    | KeyCode::Enter
                    | KeyCode::F(1)
            ) {
                self.help = false;
            }
            return Ok(false);
        }
        if self.palette {
            match key.code {
                KeyCode::Esc => self.palette = false,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.palette_selected = self.palette_selected.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.palette_selected = (self.palette_selected + 1).min(COMMANDS.len() - 1)
                }
                KeyCode::Enter => {
                    self.palette = false;
                    key.code = COMMANDS[self.palette_selected].2;
                }
                _ => {}
            }
            if key.code != COMMANDS[self.palette_selected].2 || self.palette {
                if !self.palette {
                    self.composing = self.palette_editing;
                    self.palette_editing = false;
                }
                return Ok(false);
            }
        }
        match key.code {
            KeyCode::F(2) => self.actions.push_back(Action::Models),
            KeyCode::F(3) => self.actions.push_back(Action::Connect),
            KeyCode::F(4) => self.actions.push_back(Action::Variants),
            KeyCode::F(5) => self.actions.push_back(Action::JumpList),
            KeyCode::F(6) => self.grid = !self.grid,
            KeyCode::F(7) => self.actions.push_back(Action::Members),
            KeyCode::F(8) => self.actions.push_back(Action::AddList),
            KeyCode::F(9) => self.actions.push_back(Action::RemoveList),
            KeyCode::F(10) => self.actions.push_back(Action::Sessions),
            KeyCode::F(11) => self.actions.push_back(Action::NewSession),
            KeyCode::F(12) => self.actions.push_back(Action::Stop),
            KeyCode::Char('p') => self.actions.push_back(Action::Pause),
            KeyCode::Char('r') => self.actions.push_back(Action::Resume),
            KeyCode::Char('/') => self.menu = Some(Menu::commands()),
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::F(1) => self.help = true,
            KeyCode::Char(':') => self.palette = true,
            KeyCode::Char('1') => self.focus = Focus::Board,
            KeyCode::Char('2') => self.focus = Focus::Agents,
            KeyCode::Char('3') => self.focus = Focus::Detail,
            KeyCode::Esc => {
                self.menu_epoch += 1;
                self.focus = Focus::Agents;
                self.notice.clear();
            }
            KeyCode::Char('o') => self.composing = true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Board => Focus::Agents,
                    Focus::Agents => Focus::Detail,
                    Focus::Detail => Focus::Board,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Board => Focus::Detail,
                    Focus::Agents => Focus::Board,
                    Focus::Detail => Focus::Agents,
                }
            }
            KeyCode::Char('f') => {
                if self.focus == Focus::Detail {
                    self.detail_follow = !self.detail_follow;
                } else {
                    self.follow = !self.follow;
                    self.load(store).await?;
                }
            }
            KeyCode::PageUp => {
                if self.grid {
                    self.selected = self.selected.saturating_sub(self.grid_page_size);
                    return Ok(false);
                }
                self.follow = false;
                self.after = self.after.saturating_sub(PAGE_SIZE as u64);
                self.board_scroll = 0;
                self.load(store).await?;
            }
            KeyCode::PageDown => {
                if self.grid {
                    self.selected =
                        (self.selected + self.grid_page_size).min(agent_count.saturating_sub(1));
                    return Ok(false);
                }
                let next = self.board.last().map(|m| m.seq).unwrap_or(self.after);
                if next < self.latest {
                    self.follow = false;
                    self.after = next;
                    self.board_scroll = 0;
                    self.load(store).await?;
                } else {
                    self.follow = true;
                }
            }
            KeyCode::Home if self.focus == Focus::Board => {
                self.follow = false;
                self.after = 0;
                self.board_scroll = 0;
                self.load(store).await?;
            }
            KeyCode::End if self.focus == Focus::Board => {
                self.follow = true;
                self.load(store).await?;
            }
            KeyCode::Home if self.focus == Focus::Agents => {
                self.selected = 0;
                self.table.select(Some(0));
            }
            KeyCode::End if self.focus == Focus::Agents => {
                self.selected = agent_count.saturating_sub(1);
                self.table.select(Some(self.selected));
            }
            KeyCode::Home if self.focus == Focus::Detail => {
                self.detail_follow = false;
                self.detail_scroll = 0;
            }
            KeyCode::End if self.focus == Focus::Detail => self.detail_follow = true,
            KeyCode::Down | KeyCode::Char('j') => self.navigate(1, agent_count),
            KeyCode::Up | KeyCode::Char('k') => self.navigate(-1, agent_count),
            KeyCode::Enter => {
                if self.grid {
                    self.grid = false;
                    self.focus = Focus::Detail;
                    return Ok(false);
                }
                self.focus = if self.focus == Focus::Agents {
                    Focus::Detail
                } else {
                    Focus::Agents
                }
            }
            _ => {}
        }
        if self.palette_editing && !self.palette {
            self.composing = true;
            self.palette_editing = false;
        }
        Ok(false)
    }

    fn navigate(&mut self, delta: i32, agents: usize) {
        match self.focus {
            Focus::Agents => {
                self.selected = if delta > 0 {
                    (self.selected + 1).min(agents.saturating_sub(1))
                } else {
                    self.selected.saturating_sub(1)
                };
                self.table.select(Some(self.selected));
                self.detail_scroll = 0;
                self.detail_follow = true;
            }
            Focus::Board => {
                self.follow = false;
                self.board_scroll = if delta > 0 {
                    self.board_scroll.saturating_add(3)
                } else {
                    self.board_scroll.saturating_sub(3)
                };
            }
            Focus::Detail => {
                self.detail_follow = false;
                self.detail_scroll = if delta > 0 {
                    self.detail_scroll.saturating_add(3)
                } else {
                    self.detail_scroll.saturating_sub(3)
                }
            }
        }
    }
}

/// Returning from the console detaches the display; it never stops the swarm.
pub async fn run(
    store: Store,
    metrics: Arc<Metrics>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_with_session(store, metrics, shutdown, SessionInfo::default()).await
}

pub async fn run_with_session(
    store: Store,
    metrics: Arc<Metrics>,
    shutdown: watch::Receiver<bool>,
    session: SessionInfo,
) -> Result<()> {
    run_console(store, metrics, shutdown, session, None).await
}

pub async fn run_with_controls(
    store: Store,
    metrics: Arc<Metrics>,
    shutdown: watch::Receiver<bool>,
    session: SessionInfo,
    manager: ProviderManager,
) -> Result<()> {
    run_console(store, metrics, shutdown, session, Some(manager)).await
}

enum JobResult {
    Open(Kind, Vec<Entry>),
    Selected,
    Connected(String),
    Sent(String, u64),
    Copied,
    Restored(String, bool),
    Jumped(u64),
    MembershipChanged(String),
    LifecycleChanged(String),
    BoardCleared,
    Navigate,
}

type ConsoleJob = (u64, bool, bool, tokio::task::JoinHandle<Result<JobResult>>);

fn cancel_stale_menu_job(epoch: u64, job: &mut Option<ConsoleJob>) {
    if job
        .as_ref()
        .is_some_and(|(started_epoch, _, read_only, _)| *read_only && *started_epoch != epoch)
    {
        // Discovery can wait on a remote provider indefinitely. Once its menu
        // is dismissed, release the job slot so subsequent actions can run.
        // Mutating jobs retain their handle and finish even after dismissal.
        job.take().unwrap().3.abort();
    }
}

fn roster_entries(kind: &Kind, members: &[String]) -> Vec<Entry> {
    let mut entries = Vec::new();
    if matches!(kind, Kind::Members) {
        entries.push(Entry {
            id: "add".into(),
            label: "Add parallel agents…".into(),
            detail: "Choose a batch count; joins the ongoing objective and the same global board"
                .into(),
        });
    }
    entries.extend(members.iter().map(|id| Entry {
        label: id.clone(),
        id: id.clone(),
        detail: "Space to mark; Enter removes the batch after in-flight operations finish".into(),
    }));
    entries
}

fn refresh_roster_menu(app: &mut UiState, members: &[String]) {
    let Some(menu) = &mut app.menu else {
        return;
    };
    if !matches!(menu.kind, Kind::Members | Kind::RemoveAgents)
        || menu
            .entries
            .iter()
            .filter(|entry| entry.id != "add")
            .map(|entry| &entry.id)
            .eq(members.iter())
    {
        return;
    }
    let selected = menu
        .filtered()
        .get(menu.selected)
        .map(|entry| entry.id.clone());
    menu.entries = roster_entries(&menu.kind, members);
    menu.marked.retain(|id| members.contains(id));
    menu.selected = selected
        .and_then(|id| menu.filtered().iter().position(|entry| entry.id == id))
        .unwrap_or(0);
}

fn apply_connected_result(app: &mut UiState, epoch: u64, provider: String) {
    app.notice = format!("{provider} connected");
    if epoch == app.menu_epoch {
        app.model_search = Some(provider);
        app.actions.push_back(Action::Models);
    }
}

fn mcp_entries(hub: &crate::mcp::Hub) -> Vec<Entry> {
    hub.statuses()
        .into_iter()
        .map(|(id, status)| Entry {
            label: id.clone(),
            id,
            detail: status,
        })
        .collect()
}

fn refresh_mcp_menu(app: &mut UiState, hub: &crate::mcp::Hub) {
    let Some(menu) = &mut app.menu else {
        return;
    };
    if !matches!(menu.kind, Kind::Mcp) {
        return;
    }
    let entries = mcp_entries(hub);
    if entries.len() == menu.entries.len()
        && entries
            .iter()
            .zip(&menu.entries)
            .all(|(next, old)| next.id == old.id && next.detail == old.detail)
    {
        return;
    }
    let selected = menu
        .filtered()
        .get(menu.selected)
        .map(|entry| entry.id.clone());
    menu.entries = entries;
    menu.selected = selected
        .and_then(|id| menu.filtered().iter().position(|entry| entry.id == id))
        .unwrap_or(0);
}

async fn perform(action: Action, manager: ProviderManager, store: Store) -> Result<JobResult> {
    match action {
        Action::Board => Ok(JobResult::Open(
            Kind::Board,
            vec![
                Entry {
                    id: "export".into(),
                    label: "Export complete messageboard".into(),
                    detail: "Save every message as JSON; /export-board PATH selects a destination"
                        .into(),
                },
                Entry {
                    id: "clear".into(),
                    label: "Clear messageboard…".into(),
                    detail: "Permanent deletion; confirmation required; idle only".into(),
                },
            ],
        )),
        Action::ConfirmClearBoard => {
            anyhow::ensure!(
                !manager.control.is_busy(),
                "stop work and wait for all workers to drain before clearing the board"
            );
            Ok(JobResult::Open(Kind::ClearBoard, vec![
                Entry { id: "cancel".into(), label: "Cancel — keep history".into(), detail: "Recommended: export the board first".into() },
                Entry { id: "clear".into(), label: "Permanently delete history".into(), detail: "Deletes all board messages, prompt snapshots, worker checkpoints and votes".into() },
            ]))
        }
        Action::ClearBoard => {
            manager.control.clear_board().await?;
            Ok(JobResult::BoardCleared)
        }
        Action::ExportBoard(path) => {
            use tokio::io::AsyncWriteExt;
            let current = manager.control.current();
            let path = path.map(std::path::PathBuf::from).unwrap_or_else(|| {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                std::path::PathBuf::from(format!("messageboard-{stamp}.json"))
            });
            let path = if path.is_absolute() {
                path
            } else {
                current.config.workspace.join(path)
            };
            let messages = store.export_board().await?;
            let bytes = serde_json::to_vec_pretty(&messages)?;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .with_context(|| {
                    format!(
                        "create board export {} (existing files are not overwritten)",
                        path.display()
                    )
                })?;
            file.write_all(&bytes).await?;
            file.flush().await?;
            Ok(JobResult::LifecycleChanged(format!(
                "Exported {} messages to {}",
                messages.len(),
                path.display()
            )))
        }
        Action::Sessions => {
            let current = manager.control.current();
            let database = std::fs::canonicalize(&current.config.database)
                .unwrap_or_else(|_| current.config.database.clone());
            let sessions = crate::session_catalog::list(Some(&current.config.workspace))?;
            Ok(JobResult::Open(
                Kind::Sessions,
                sessions
                    .into_iter()
                    .map(|session| Entry {
                        label: if session.database == database {
                            format!("{} · current · {}", session.id, session.title)
                        } else {
                            format!("{} · {}", session.id, session.title)
                        },
                        detail: format!(
                            "{} · {}",
                            session.workspace.display(),
                            session.database.display()
                        ),
                        id: session.id,
                    })
                    .collect(),
            ))
        }
        Action::NewSession => {
            manager
                .control
                .request_navigation(crate::session::SessionNavigation::New)
                .await?;
            Ok(JobResult::Navigate)
        }
        Action::OpenSession(id) => {
            let session = crate::session_catalog::find(&id)?;
            let current = manager.control.current();
            let database = std::fs::canonicalize(&current.config.database)
                .unwrap_or_else(|_| current.config.database.clone());
            if session.database == database {
                return Ok(JobResult::LifecycleChanged(
                    "This session is already open.".into(),
                ));
            }
            manager
                .control
                .request_navigation(crate::session::SessionNavigation::Open(id))
                .await?;
            Ok(JobResult::Navigate)
        }
        Action::Pause => {
            manager.control.pause().await?;
            Ok(JobResult::LifecycleChanged(
                "Paused. Current operations finish; resume to continue.".into(),
            ))
        }
        Action::Resume => {
            manager.control.resume().await?;
            Ok(JobResult::LifecycleChanged("Resumed current work.".into()))
        }
        Action::Stop => {
            manager.control.stop_work().await?;
            Ok(JobResult::LifecycleChanged(
                "Stopping current work. In-flight operations drain before returning idle.".into(),
            ))
        }
        Action::Members | Action::RemoveList => {
            let kind = if matches!(action, Action::Members) {
                Kind::Members
            } else {
                Kind::RemoveAgents
            };
            let entries = roster_entries(&kind, &manager.control.members());
            Ok(JobResult::Open(kind, entries))
        }
        Action::AddList => Ok(JobResult::Open(Kind::AddAgents, Vec::new())),
        Action::AddAgents(count) => {
            let ids = manager.control.add_agents(count).await?;
            Ok(JobResult::MembershipChanged(format!(
                "Added {}. Joining the shared objective; notice committed to the global board.",
                ids.join(", ")
            )))
        }
        Action::RemoveAgents(ids) => {
            manager.control.remove_agents(ids.clone()).await?;
            Ok(JobResult::MembershipChanged(format!(
                "Removing {}. Current requests/tools finish before workers quit; global notice committed.",
                ids.join(", ")
            )))
        }
        Action::InvalidCommand(message) => anyhow::bail!("{message}"),
        Action::Mcp => Ok(JobResult::Open(
            Kind::Mcp,
            mcp_entries(&manager.control.mcp),
        )),
        Action::ToggleMcp(name) => {
            manager.control.mcp.toggle(&name).await?;
            Ok(JobResult::Open(
                Kind::Mcp,
                mcp_entries(&manager.control.mcp),
            ))
        }
        Action::Models => Ok(JobResult::Open(Kind::Models, manager.models().await?)),
        Action::Connect => Ok(JobResult::Open(Kind::Connect, manager.providers()?)),
        Action::Variants => Ok(JobResult::Open(Kind::Variants, manager.variants().await?)),
        Action::Cycle => {
            manager.cycle_variant().await?;
            Ok(JobResult::Selected)
        }
        Action::SelectModel(model) => {
            manager.select(&model, None).await?;
            Ok(JobResult::Selected)
        }
        Action::SelectVariant(variant) => {
            let active = manager.control.current();
            manager
                .select(
                    &format!("{}/{}", active.config.provider, active.config.model),
                    (!variant.is_empty()).then_some(variant.as_str()),
                )
                .await?;
            Ok(JobResult::Selected)
        }
        Action::ConnectKey {
            provider,
            key,
            endpoint,
        } => {
            manager
                .connect(&provider, &key, endpoint.as_deref())
                .await?;
            Ok(JobResult::Connected(provider))
        }
        Action::JumpList => {
            let prompts = store.prompts().await?;
            Ok(JobResult::Open(
                Kind::Jump,
                prompts
                    .into_iter()
                    .map(|prompt| Entry {
                        id: prompt.seq.to_string(),
                        label: format!(
                            "#{}  {}",
                            prompt.seq,
                            prompt
                                .body
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(90)
                                .collect::<String>()
                        ),
                        detail: prompt.body,
                    })
                    .collect(),
            ))
        }
        Action::Submit(text) => {
            let seq = manager.submit(text.clone()).await?;
            Ok(JobResult::Sent(text, seq))
        }
        Action::Copy(seq) => {
            let prompt = store
                .read_board(seq.saturating_sub(1), 1)
                .await?
                .into_iter()
                .next()
                .context("prompt missing")?;
            crate::clipboard::copy(&prompt.body).await?;
            Ok(JobResult::Copied)
        }
        Action::Restore(seq) => {
            let (text, snapshot) = manager.control.restore_prompt(seq).await?;
            Ok(JobResult::Restored(text, snapshot))
        }
        Action::Jump(seq) => Ok(JobResult::Jumped(seq)),
        _ => anyhow::bail!("unsupported background action"),
    }
}

/// Roster writes must stay usable while an unrelated provider/MCP mutation is
/// pending. They retain independent task handles and are always drained on exit.
async fn dispatch_roster_actions(
    app: &mut UiState,
    manager: Option<&ProviderManager>,
    store: &Store,
    jobs: &mut tokio::task::JoinSet<Result<JobResult>>,
    primary_mutation_pending: bool,
) -> Result<()> {
    while let Some(index) = app.actions.iter().position(|action| {
        matches!(
            action,
            Action::Members
                | Action::AddList
                | Action::RemoveList
                | Action::AddAgents(_)
                | Action::RemoveAgents(_)
                | Action::InvalidCommand(_)
                | Action::Pause
                | Action::Resume
                | Action::Stop
                | Action::Sessions
                | Action::NewSession
                | Action::OpenSession(_)
                | Action::Start
        )
    }) {
        let action = app.actions.remove(index).unwrap();
        if matches!(action, Action::Start) {
            app.composing = true;
            app.notice = "Enter an objective, then press Enter to start work.".into();
            continue;
        }
        if let Action::InvalidCommand(message) = action {
            app.notice = message;
            continue;
        }
        let Some(manager) = manager else {
            app.notice = "Session controls require the managed console".into();
            continue;
        };
        if matches!(action, Action::NewSession | Action::OpenSession(_))
            && (!jobs.is_empty() || app.pending_submit || primary_mutation_pending)
        {
            app.notice = "Wait for pending changes to finish before switching sessions.".into();
            continue;
        }
        if matches!(
            action,
            Action::Members | Action::AddList | Action::RemoveList | Action::Sessions
        ) {
            // Local menus cannot wait on the remote discovery/mutation lane.
            // A broken metadata file must not take down the live console.
            match perform(action, manager.clone(), store.clone()).await {
                Ok(JobResult::Open(kind, entries)) => {
                    app.menu_epoch += 1;
                    app.menu = Some(Menu::new(kind, entries, None));
                    app.notice.clear();
                }
                Err(error) => app.notice = format!("session control failed: {error:#}"),
                _ => {}
            }
        } else {
            let manager = manager.clone();
            let store = store.clone();
            jobs.spawn(async move { perform(action, manager, store).await });
            app.notice = "Applying session control…".into();
        }
    }
    Ok(())
}

async fn run_console(
    store: Store,
    metrics: Arc<Metrics>,
    mut shutdown: watch::Receiver<bool>,
    session: SessionInfo,
    manager: Option<ProviderManager>,
) -> Result<()> {
    let (_guard, mut terminal) = open_terminal()?;
    let mut app = UiState {
        session,
        managed: manager.is_some(),
        composing: manager
            .as_ref()
            .is_some_and(|manager| manager.control.current().config.interactive_session),
        ..UiState::default()
    };
    if let Some(manager) = &manager {
        let active = manager.control.current();
        let database = std::fs::canonicalize(&active.config.database)
            .unwrap_or_else(|_| active.config.database.clone());
        app.session_id = crate::session_catalog::list(Some(&active.config.workspace))
            .unwrap_or_default()
            .into_iter()
            .find(|session| session.database == database)
            .map(|session| session.id)
            .unwrap_or_default();
        app.workspace = active.config.workspace.display().to_string();
        app.database = active.config.database.display().to_string();
    }
    app.load(&store).await?;
    let mut board_changes = store.subscribe_board();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut agent_count = metrics.snapshot().agents.len();
    let mut board_dirty = true;
    let mut job: Option<ConsoleJob> = None;
    let mut roster_jobs = tokio::task::JoinSet::new();
    let mut clipboard_jobs = tokio::task::JoinSet::new();
    let mut rendered = ratatui::buffer::Buffer::empty(Rect::default());
    let mut left_pressed = false;
    let display_result = async {
      loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            _ = tick.tick() => {
                if let Some(manager) = &manager {
                    let active = manager.control.current();
                    app.session.provider = active.config.provider.clone();
                    app.session.model = active.config.model.clone();
                    app.session.variant = active.config.variant.clone().unwrap_or_else(|| "default".into());
                    app.busy = manager.control.is_busy();
                    app.paused = manager.control.is_paused();
                    app.stopping = manager.control.is_stopping();
                    app.workspace = active.config.workspace.display().to_string();
                    app.database = active.config.database.display().to_string();
                    let members = manager.control.members();
                    refresh_roster_menu(&mut app, &members);
                    refresh_mcp_menu(&mut app, &manager.control.mcp);
                    app.member_count = Some(members.len());
                    app.draining_count = manager.control.draining_members().len();
                    let latest = store.latest_seq().await?;
                    app.current_votes = Some(store.votes().await?.iter().filter(|vote| {
                        vote.done && vote.board_seq == latest && members.contains(&vote.agent_id)
                    }).count());
                }
                if board_dirty {
                    app.load(&store).await?;
                    board_dirty = false;
                }
                let snapshot = metrics.snapshot();
                agent_count = snapshot.agents.len();
                app.selected = app.selected.min(agent_count.saturating_sub(1));
                app.table.select((agent_count > 0).then_some(app.selected));
                app.sample(&snapshot);
                terminal.draw(|frame| {
                    draw(frame, &mut app, &snapshot, &metrics);
                    rendered = frame.buffer_mut().clone();
                    if app.menu.is_none() && !app.help && !app.palette {
                        app.selection.render(frame.buffer_mut());
                    } else {
                        app.selection.cancel();
                    }
                })?;
            },
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() { break; }
            },
            result = clipboard_jobs.join_next(), if !clipboard_jobs.is_empty() => {
                app.notice = match result {
                    Some(Ok(Ok(Ok(())))) => "Selection copied to clipboard".into(),
                    Some(Ok(Ok(Err(error)))) => format!("Clipboard copy failed: {error:#}"),
                    Some(Ok(Err(_))) => "Clipboard copy timed out".into(),
                    Some(Err(error)) if error.is_cancelled() => continue,
                    Some(Err(error)) => format!("Clipboard copy failed: {error}"),
                    None => String::new(),
                };
            },
            result = board_changes.changed() => {
                if result.is_ok() { board_dirty = true; }
            },
            result = roster_jobs.join_next(), if !roster_jobs.is_empty() => {
                match result {
                    Some(Ok(Ok(JobResult::MembershipChanged(message)))) => {
                        app.notice = message; app.follow = true; board_dirty = true;
                    },
                    Some(Ok(Ok(JobResult::LifecycleChanged(message)))) => {
                        app.notice = message; board_dirty = true;
                    },
                    Some(Ok(Ok(JobResult::Navigate))) => break,
                    Some(Ok(Err(error))) => app.notice = format!("session control failed: {error:#}"),
                    Some(Err(error)) => app.notice = format!("session control failed: {error}"),
                    _ => {},
                }
            },
            result = async { (&mut job.as_mut().unwrap().3).await }, if job.is_some() => {
                let (epoch, submitted, _, _) = job.take().unwrap();
                if submitted { app.pending_submit = false; }
                match result {
                    Ok(Ok(JobResult::Open(kind,entries))) if epoch == app.menu_epoch => {
                        let active = manager.as_ref().unwrap().control.current();
                        let initial = match &kind {
                            Kind::Models => Some(format!("{}/{}",active.config.provider,active.config.model)),
                            Kind::Variants => Some(active.config.variant.clone().unwrap_or_default()),
                            Kind::Connect => Some(active.config.provider.clone()),
                            _ => None,
                        };
                        let mut menu = Menu::new(kind,entries,initial.as_deref());
                        if matches!(menu.kind,Kind::Models) { if let Some(query) = app.model_search.take() { menu.query = query; menu.selected = 0; } }
                        app.menu = Some(menu); app.notice.clear();
                    },
                    Ok(Ok(JobResult::Open(_, _))) => {},
                    Ok(Ok(JobResult::Selected)) => app.notice = "Model/variant selected. In-flight work finishes before the next request uses it.".into(),
                    Ok(Ok(JobResult::Connected(provider))) => apply_connected_result(&mut app, epoch, provider),
                    Ok(Ok(JobResult::Sent(text,seq))) => {
                        if app.draft == text { app.draft.clear(); }
                        app.session.objective = text;
                        app.notice = format!("Prompt #{seq} sent to the swarm");
                        app.follow = true; board_dirty = true;
                    },
                    Ok(Ok(JobResult::Copied)) => app.notice = "Prompt copied to clipboard".into(),
                    Ok(Ok(JobResult::MembershipChanged(message))) => {
                        app.notice = message; app.follow = true; board_dirty = true;
                    },
                    Ok(Ok(JobResult::LifecycleChanged(message))) => {
                        app.notice = message; board_dirty = true;
                    },
                    Ok(Ok(JobResult::BoardCleared)) => {
                        app.board.clear(); app.after = 0; app.latest = 0; app.board_scroll = 0;
                        app.follow = true; app.session.objective.clear(); board_dirty = true;
                        app.notice = "Messageboard and prompt/checkpoint history cleared. Workspace and roster retained.".into();
                    },
                    Ok(Ok(JobResult::Navigate)) => break,
                    Ok(Ok(JobResult::Restored(text,snapshot))) => {
                        app.draft = text; app.composing = true;
                        app.notice = if snapshot {"Prompt restored to editor; workspace reverted to its pre-prompt snapshot"} else {"Prompt restored to editor; no workspace snapshot exists for this prompt"}.into();
                        board_dirty = true;
                    },
                    Ok(Ok(JobResult::Jumped(seq))) => {
                        app.after = seq.saturating_sub(1); app.follow = false; app.board_scroll = 0; app.focus = Focus::Board; app.grid = false;
                        app.load(&store).await?; app.notice = format!("Jumped to prompt #{seq}; click it for copy/restore");
                    },
                    Ok(Err(error)) => app.notice = format!("action failed: {error:#}"),
                    Err(error) => app.notice = format!("action failed: {error}"),
                }
            },
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    left_pressed = false;
                    app.selection.cancel();
                    match app.key(key, &store, agent_count).await {
                        Ok(true) => break,
                        Ok(false) => {},
                        Err(error) => { app.notice = format!("action failed: {error:#}"); }
                    }
                },
                Some(Ok(Event::Paste(text))) => {
                    if let Some(menu) = &mut app.menu { menu.paste(&text); }
                    else if app.composing { app.draft.push_str(&text); }
                },
                Some(Ok(Event::Mouse(mouse))) if app.menu.is_none() && !app.help && !app.palette => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        left_pressed = false;
                        app.selection.cancel();
                        if let Some((_,action)) = app.control_hits.iter().find(|(rect,_)| rect.contains((mouse.column,mouse.row).into())) {
                            app.actions.push_back(action.clone());
                            continue;
                        }
                        let point = (mouse.column, mouse.row).into();
                        let pane = app.grid_hits.iter().map(|(rect, _)| rect).chain(app.panels.iter())
                            .find(|rect| rect.contains(point));
                        let area = pane.map(|rect| rect.inner(ratatui::layout::Margin::new(1, 1)))
                            .unwrap_or(rendered.area);
                        app.selection.start(&rendered, area, point);
                        left_pressed = true;
                    }
                    if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
                        app.selection.update((mouse.column, mouse.row).into());
                        continue;
                    }
                    if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                        if !left_pressed { continue; }
                        left_pressed = false;
                        if let Some(text) = app.selection.finish((mouse.column, mouse.row).into()) {
                            if !text.is_empty() {
                                clipboard_jobs.abort_all();
                                clipboard_jobs.spawn(async move {
                                    tokio::time::timeout(Duration::from_secs(3), crate::clipboard::copy(&text)).await
                                });
                            }
                            continue;
                        }
                        if let Some((_,seq)) = app.prompt_hits.iter().find(|(rect,_)| rect.contains((mouse.column,mouse.row).into())) {
                            app.menu = Some(Menu::new(Kind::Prompt(*seq), vec![
                                Entry {id:"copy".into(),label:"Copy prompt".into(),detail:"Copy the complete prompt text to your clipboard".into()},
                                Entry {id:"restore".into(),label:"Restore prompt / workspace".into(),detail:"Restore the pre-prompt workspace snapshot and reopen the text for editing; available when idle".into()},
                                Entry {id:"jump".into(),label:"Jump to this prompt".into(),detail:"Show its position on the global board".into()},
                            ], None));
                            continue;
                        }
                        if let Some((_,index)) = app.grid_hits.iter().find(|(rect,_)| rect.contains((mouse.column,mouse.row).into())) { app.selected = *index; app.grid = false; app.focus = Focus::Detail; continue; }
                    }
                    for (index, rect) in app.panels.iter().enumerate() {
                        if rect.contains((mouse.column, mouse.row).into()) {
                            app.focus = [Focus::Board, Focus::Agents, Focus::Detail][index];
                            break;
                        }
                    }
                    match mouse.kind {
                        MouseEventKind::ScrollDown => app.navigate(1, agent_count),
                        MouseEventKind::ScrollUp => app.navigate(-1, agent_count),
                        _ => {},
                    }
                },
                Some(Ok(Event::Resize(_, _))) => {
                    left_pressed = false;
                    app.selection.cancel();
                },
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.into()),
                None => break,
            }
        }
        let primary_mutation_pending = job.as_ref().is_some_and(|(_, _, read_only, _)| !read_only);
        dispatch_roster_actions(&mut app, manager.as_ref(), &store, &mut roster_jobs, primary_mutation_pending).await?;
        cancel_stale_menu_job(app.menu_epoch, &mut job);
        if job.is_none() {
            if let Some(action) = app.actions.pop_front() {
                match action {
                    Action::Grid => {
                        app.grid = !app.grid;
                        app.focus = Focus::Agents;
                        app.composing = false;
                    }
                    Action::Help => app.help = true,
                    Action::Quit => break,
                    Action::ChooseProvider(provider) => {
                        if let Some(manager) = &manager {
                            let menu = if manager.needs_endpoint(&provider)? {
                                let mut menu = Menu::new(Kind::Endpoint(provider.clone()), Vec::new(), None);
                                menu.query = manager.endpoint(&provider)?;
                                menu
                            } else {
                                Menu::new(Kind::Key { provider, endpoint: None }, Vec::new(), None)
                            };
                            app.menu = Some(menu);
                        }
                    }
                    Action::ConnectKey {
                        provider,
                        key,
                        endpoint,
                    } if key.is_empty() && endpoint.as_deref() == Some("") => {
                        if let Some(manager) = &manager {
                            let mut menu =
                                Menu::new(Kind::Endpoint(provider.clone()), Vec::new(), None);
                            menu.query = manager.endpoint(&provider)?;
                            app.menu = Some(menu);
                        }
                    }
                    action => {
                        if let Some(manager) = &manager {
                            let submitted = matches!(action, Action::Submit(_));
                            let read_only = matches!(
                                action,
                                Action::Models
                                    | Action::Connect
                                    | Action::Variants
                                    | Action::JumpList
                                    | Action::Jump(_)
                                    | Action::Mcp
                                    | Action::Members
                                    | Action::RemoveList
                            );
                            if read_only {
                                app.menu_epoch += 1;
                                app.notice =
                                    "Loading… Esc cancels the menu; the swarm keeps running".into();
                            } else {
                                app.notice = "Applying action…".into();
                            }
                            let manager = manager.clone();
                            let store = store.clone();
                            job = Some((
                                app.menu_epoch,
                                submitted,
                                read_only,
                                tokio::spawn(async move { perform(action, manager, store).await }),
                            ));
                        } else {
                            app.notice = "Session controls require the managed console".into();
                        }
                    }
                }
            }
        }
    }
      Ok::<_, anyhow::Error>(())
    }.await;
    if let Some((_, _, read_only, job)) = job {
        if read_only {
            job.abort();
        } else {
            let _ = job.await;
        }
    }
    while roster_jobs.join_next().await.is_some() {}
    display_result
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

fn open_terminal() -> Result<(TerminalGuard, Terminal<CrosstermBackend<Stdout>>)> {
    enable_raw_mode()?;
    let guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok((guard, terminal))
}

fn block(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::new()
        .borders(Borders::ALL)
        .title(title.into())
        .border_style(Style::default().fg(if focused { COBALT } else { MUTED }))
}

fn status_color(status: AgentStatus) -> Color {
    match status {
        AgentStatus::Thinking | AgentStatus::Tool => COBALT,
        AgentStatus::Retry => AMBER,
        AgentStatus::Error => ROSE,
        AgentStatus::Voted | AgentStatus::Finished => SEA,
        _ => MUTED,
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut UiState, snapshot: &MetricsSnapshot, metrics: &Metrics) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(SLATE).fg(ICE)),
        area,
    );
    let rows = Layout::vertical([
        Constraint::Length(if area.height >= 24 {
            if app.managed {
                6
            } else {
                4
            }
        } else if app.managed {
            4
        } else {
            2
        }),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(if app.managed { 6 } else { 3 }),
        Constraint::Length(if app.managed { 3 } else { 2 }),
    ])
    .split(area);
    let uptime = snapshot.elapsed_secs as u64;
    let members = app.member_count.unwrap_or(snapshot.agents.len());
    let threshold = members.saturating_mul(3).div_ceil(4);
    let status = Line::from(vec![
        Span::styled(
            if area.width >= 100 {
                " openraid by vuln.industries  "
            } else {
                " openraid  "
            },
            Style::default().fg(SEA).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.managed {
                if app.stopping {
                    "STOPPING  "
                } else if app.paused {
                    "PAUSED  "
                } else if app.busy {
                    "RUNNING  "
                } else {
                    "IDLE  "
                }
            } else {
                ""
            },
            Style::default()
                .fg(if app.paused || app.stopping {
                    AMBER
                } else {
                    SEA
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{members} agents  {} draining  ", app.draining_count),
            Style::default().fg(COBALT),
        ),
        Span::raw(format!(
            "{} active  {} done votes / {} required  ",
            snapshot.active,
            app.current_votes.unwrap_or(snapshot.voted),
            threshold
        )),
        Span::styled(
            format!("{:.1} tps (1m)  ", app.tps),
            Style::default().fg(AMBER),
        ),
        Span::raw(format!(
            "{:02}:{:02}:{:02}",
            uptime / 3600,
            (uptime / 60) % 60,
            uptime % 60
        )),
    ]);
    let mut header = vec![
        status,
        Line::styled(
            format!(
                " {} / {}   thinking: {}",
                app.session.provider,
                app.session.model,
                if app.session.variant.is_empty() {
                    "default"
                } else {
                    &app.session.variant
                }
            ),
            Style::default().fg(COBALT),
        ),
    ];
    if app.managed {
        header.push(Line::styled(
            format!(" cwd: {}", app.workspace),
            Style::default().fg(ICE),
        ));
        header.push(Line::styled(
            format!(
                " Session: {}{}{}",
                app.session_id,
                if app.session_id.is_empty() {
                    ""
                } else {
                    " · "
                },
                app.database
            ),
            Style::default().fg(MUTED),
        ));
    }
    if area.height >= 24 {
        header.push(Line::styled(
            format!(
                " Objective: {}",
                app.session
                    .objective
                    .lines()
                    .next()
                    .unwrap_or("Collaborate on the shared workspace")
            ),
            Style::default().fg(ICE),
        ));
        header.push(Line::styled(
            format!(
                " input {}  output {}  cached {}  tools {}  retries {}  finished {}",
                snapshot.input_tokens,
                snapshot.output_tokens,
                snapshot.cached_tokens,
                snapshot.tools,
                snapshot.retries,
                snapshot.finished
            ),
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(Paragraph::new(header), rows[0]);

    frame.render_widget(
        Tabs::new(["1 Board", "2 Agents", "3 Stream"])
            .select(match app.focus {
                Focus::Board => 0,
                Focus::Agents => 1,
                Focus::Detail => 2,
            })
            .style(Style::default().fg(MUTED))
            .highlight_style(Style::default().fg(SEA).add_modifier(Modifier::BOLD))
            .divider("  "),
        rows[1],
    );
    app.panels = [Rect::default(); 3];
    app.prompt_hits.clear();
    app.grid_hits.clear();

    if app.grid {
        draw_grid(frame, rows[2], app, snapshot, metrics);
    } else if rows[2].width >= 100 && area.height >= 24 {
        let columns = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(rows[2]);
        draw_board(frame, columns[0], app);
        let right = Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(columns[1]);
        draw_agents(frame, right[0], app, snapshot);
        draw_detail(frame, right[1], app, snapshot, metrics);
        app.panels = [columns[0], right[0], right[1]];
    } else if area.width >= 70 && area.height >= 24 {
        let panels = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[2]);
        draw_board(frame, panels[0], app);
        if app.focus == Focus::Detail {
            draw_detail(frame, panels[1], app, snapshot, metrics);
            app.panels = [panels[0], Rect::default(), panels[1]];
        } else {
            draw_agents(frame, panels[1], app, snapshot);
            app.panels = [panels[0], panels[1], Rect::default()];
        }
    } else {
        match app.focus {
            Focus::Board => {
                draw_board(frame, rows[2], app);
                app.panels[0] = rows[2];
            }
            Focus::Agents => {
                draw_agents(frame, rows[2], app, snapshot);
                app.panels[1] = rows[2];
            }
            Focus::Detail => {
                draw_detail(frame, rows[2], app, snapshot, metrics);
                app.panels[2] = rows[2];
            }
        }
    }
    let rates: Vec<u64> = app.rates.iter().copied().collect();
    if app.managed {
        let text = if app.draft.is_empty() {
            "Type your prompt, or / for commands".to_owned()
        } else {
            app.draft.clone()
        };
        frame.render_widget(
            Paragraph::new(format!("{text}{}", if app.composing { "▏" } else { "" }))
                .wrap(Wrap { trim: false })
                .block(block(
                    if app.stopping {
                        " Prompt · stopping · wait for in-flight operations to drain "
                    } else if app.paused {
                        " Prompt · paused · /resume continues · /stop ends work "
                    } else if app.busy {
                        " Prompt · working · Enter sends a follow-up "
                    } else {
                        " Prompt · ready · Enter starts work "
                    },
                    app.composing,
                )),
            rows[3],
        );
    } else {
        frame.render_widget(
            Sparkline::default()
                .data(&rates)
                .style(Style::default().fg(COBALT))
                .block(block(
                    format!(" output tokens / second (1m avg)  {:.1}", app.tps),
                    false,
                )),
            rows[3],
        );
    }
    app.control_hits.clear();
    let footer = if app.managed {
        let footer_rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(2)]).split(rows[4]);
        draw_session_controls(frame, footer_rows[0], app);
        footer_rows[1]
    } else {
        rows[4]
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                " / commands · Ctrl+X S sessions · P pause · R resume · X stop · Esc then q detach",
                Style::default().fg(SEA),
            ),
            Line::styled(app.notice.clone(), Style::default().fg(AMBER)),
        ]),
        footer,
    );
    if app.composing && !app.managed {
        draw_composer(frame, app);
    }
    if app.help {
        draw_help(frame);
    }
    if app.palette {
        draw_palette(frame, app);
    }
    if let Some(menu) = &app.menu {
        menu.draw(frame);
    }
}

fn draw_session_controls(frame: &mut Frame<'_>, area: Rect, app: &mut UiState) {
    let controls = [
        (" Sessions ", Action::Sessions),
        (" New ", Action::NewSession),
        (" Start ", Action::Start),
        (
            if app.paused { " Resume " } else { " Pause " },
            if app.paused {
                Action::Resume
            } else {
                Action::Pause
            },
        ),
        (" Stop ", Action::Stop),
    ];
    let mut spans = Vec::new();
    let mut x = area.x;
    for (label, action) in controls {
        let width = (label.len() as u16).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        app.control_hits
            .push((Rect::new(x, area.y, width, area.height.min(1)), action));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(SEA)
                .bg(Color::Rgb(45, 66, 96))
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        x = x.saturating_add(width + 1);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut UiState,
    snapshot: &MetricsSnapshot,
    metrics: &Metrics,
) {
    let count = snapshot.agents.len().max(1);
    let columns = ((count as f64).sqrt().ceil() as usize)
        .min(6)
        .min((area.width / 22).max(1) as usize)
        .max(1);
    let rows = count
        .div_ceil(columns)
        .min((area.height / 5).max(1) as usize)
        .max(1);
    let capacity = rows * columns;
    app.grid_page_size = capacity;
    let first = app.selected / capacity * capacity;
    let vertical = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    for (row, row_area) in vertical.iter().enumerate() {
        let horizontal = Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns])
            .split(*row_area);
        for (column, cell) in horizontal.iter().enumerate() {
            let index = first + row * columns + column;
            if let Some(agent) = snapshot.agents.get(index) {
                let tail = metrics.agent_tail(&agent.id, 768);
                let detail = metrics.agent_detail(&agent.id);
                let output = tail
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                let lines = vec![
                    Line::styled(
                        format!(
                            "{} · {} out · {} tools",
                            agent.status.label(),
                            agent.output_tokens,
                            agent.tools
                        ),
                        Style::default().fg(status_color(agent.status)),
                    ),
                    Line::styled(detail, Style::default().fg(AMBER)),
                    Line::raw(output),
                ];
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .block(block(
                            format!(" {} · click for stream ", agent.id),
                            index == app.selected,
                        )),
                    *cell,
                );
                app.grid_hits.push((*cell, index));
            }
        }
    }
}

fn draw_board(frame: &mut Frame<'_>, area: Rect, app: &mut UiState) {
    let first = app.board.first().map(|m| m.seq).unwrap_or(0);
    let last = app.board.last().map(|m| m.seq).unwrap_or(0);
    let title = format!(
        " global board  #{first}..{last} / {}  {}",
        app.latest,
        if app.follow { "following" } else { "history" }
    );
    let mut lines = Vec::new();
    let mut prompt_rows = Vec::new();
    let mut wrapped_rows = 0usize;
    for message in &app.board {
        let first_line = lines.len();
        let role = if message.owner {
            "owner"
        } else {
            message.sender.as_str()
        };
        lines.push(Line::styled(
            format!("#{}  [{}]", message.seq, role),
            Style::default()
                .fg(if message.owner { AMBER } else { COBALT })
                .add_modifier(Modifier::BOLD),
        ));
        for line in message.body.lines() {
            lines.push(Line::raw(line.to_owned()));
        }
        lines.push(Line::raw(""));
        let height = Paragraph::new(lines[first_line..].to_vec())
            .wrap(Wrap { trim: false })
            .line_count(area.width.saturating_sub(2).max(1));
        if message.owner && message.sender == "owner" {
            prompt_rows.push((message.seq, wrapped_rows, height));
        }
        wrapped_rows += height;
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "the board is empty. press o to send the swarm an owner instruction.",
            Style::default().fg(MUTED),
        ));
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let last_scroll = paragraph
        .line_count(area.width.saturating_sub(2).max(1))
        .saturating_sub(usize::from(area.height.saturating_sub(2)))
        .min(usize::from(u16::MAX)) as u16;
    app.board_scroll = if app.follow {
        last_scroll
    } else {
        app.board_scroll.min(last_scroll)
    };
    for (seq, start, height) in prompt_rows {
        let top = start as i64 - i64::from(app.board_scroll);
        let bottom = top + height as i64;
        let visible_top = top.max(0).min(i64::from(area.height.saturating_sub(2)));
        let visible_bottom = bottom.max(0).min(i64::from(area.height.saturating_sub(2)));
        if visible_bottom > visible_top {
            app.prompt_hits.push((
                Rect::new(
                    area.x + 1,
                    area.y + 1 + visible_top as u16,
                    area.width.saturating_sub(2),
                    (visible_bottom - visible_top) as u16,
                ),
                seq,
            ));
        }
    }
    frame.render_widget(
        paragraph
            .scroll((app.board_scroll, 0))
            .block(block(title, app.focus == Focus::Board)),
        area,
    );
}

fn draw_agents(frame: &mut Frame<'_>, area: Rect, app: &mut UiState, snapshot: &MetricsSnapshot) {
    let rows = snapshot.agents.iter().map(|agent| {
        Row::new(vec![
            Cell::from(agent.id.clone()),
            Cell::from(agent.status.label()).style(Style::default().fg(status_color(agent.status))),
            Cell::from(agent.output_tokens.to_string()),
            Cell::from(agent.tools.to_string()),
            Cell::from(agent.retries.to_string()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(["agent", "state", "out tok", "tools", "retry"]).style(Style::default().fg(MUTED)),
    )
    .block(block(
        " agents  enter drills into selected agent",
        app.focus == Focus::Agents,
    ))
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(45, 66, 96))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("> ");
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn draw_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut UiState,
    snapshot: &MetricsSnapshot,
    metrics: &Metrics,
) {
    let Some(agent) = snapshot.agents.get(app.selected) else {
        return;
    };
    let detail = metrics.agent_detail(&agent.id);
    let output = metrics.agent_output(&agent.id);
    let mut lines = vec![
        Line::styled(
            format!("{}  {}", agent.id, agent.status.label()),
            Style::default().fg(status_color(agent.status)),
        ),
        Line::raw(format!(
            "input {}  output {}  cached {}",
            agent.input_tokens, agent.output_tokens, agent.cached_tokens
        )),
        Line::styled(detail, Style::default().fg(AMBER)),
        Line::raw(""),
    ];
    if output.is_empty() {
        lines.push(Line::styled(
            "waiting for streaming output",
            Style::default().fg(MUTED),
        ));
    } else {
        for line in output.lines() {
            lines.push(Line::raw(line.to_owned()));
        }
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let last_scroll = paragraph
        .line_count(area.width.saturating_sub(2).max(1))
        .saturating_sub(usize::from(area.height.saturating_sub(2)))
        .min(usize::from(u16::MAX)) as u16;
    app.detail_scroll = if app.detail_follow {
        last_scroll
    } else {
        app.detail_scroll.min(last_scroll)
    };
    frame.render_widget(
        paragraph.scroll((app.detail_scroll, 0)).block(block(
            " agent detail  live stream preview (last 16 KiB)",
            app.focus == Focus::Detail,
        )),
        area,
    );
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn draw_composer(frame: &mut Frame<'_>, app: &UiState) {
    let area = popup(frame.area(), 84, 10);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n\nenter posts to every agent  shift-enter newline  esc cancels  ctrl-u clears",
            app.draft
        ))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(SLATE).fg(ICE))
        .block(block(
            " owner instruction  committing revokes done votes",
            true,
        )),
        area,
    );
}

fn draw_help(frame: &mut Frame<'_>) {
    let area = popup(frame.area(), 104, 38);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(
            "/sessions         browse sessions for this workspace; /session ID opens any session\n\
         /new              create fresh session history in this workspace (idle only)\n\
         /start [prompt]   start an objective, or focus the prompt editor\n\
         /pause            pause at safe operation boundaries; current operations finish\n\
         /resume           continue paused work with the same context\n\
         /stop             drain current work and return idle; no completion vote required\n\
         Ctrl+X S/N        workspace sessions / new session\n\
         Ctrl+X P/R/X      pause / resume / stop, even while editing a prompt\n\
         /models (/model)   choose from connected providers only\n\
         /connect          add or replace a provider key\n\
         /variant          thinking menu; Ctrl+T cycles variants\n\
         /jump             search every sent prompt\n\
         /agents           tiled agent streams; click a tile to drill in\n\
         /members          manage the current parallel-agent roster\n\
         /add [count]      add collaborators to ongoing work (default 1)\n\
         /remove [IDs]     remove a batch, or open the graceful-remove menu\n\
         Ctrl+X then M/C/T models / connect / variants (no time limit)\n\
         Ctrl+X then J/A   prompt history / agent grid\n\
         Ctrl+X then +/-   choose add count / mark agents to remove\n\
         click a prompt    copy / restore prompt and workspace / jump\n\
         drag text         copy selection immediately on release\n\
         Ctrl+P            command palette; Esc closes overlays\n\
         Tab / Shift+Tab   focus board / agents / stream\n\
         ↑/↓ or j/k        navigate focused panel\n\
         PgUp/PgDn         board pages, or agent-grid pages\n\
         Home/End, F       first/latest and follow\n\
         O                 focus prompt editor; Enter submits\n\
         ? / H / F1        help\n\
         Esc then Q        close interactive sessions gracefully; preserve unfinished work\n\
         /quit             same action; noninteractive runs continue headless\n\n\
         The board stays global, durable, and unfiltered.\n\
         Restore uses pre-prompt Git snapshots when available and runs only idle.",
        )
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(SLATE).fg(ICE))
        .block(block(
            " Help · openraid by vuln.industries · Esc closes ",
            true,
        )),
        area,
    );
}

fn draw_palette(frame: &mut Frame<'_>, app: &UiState) {
    let area = popup(frame.area(), 72, 12);
    frame.render_widget(Clear, area);
    let items = COMMANDS
        .iter()
        .map(|(name, shortcut, _)| ListItem::new(format!("{name:<48} {shortcut}")));
    let mut selected = ListState::default().with_selected(Some(app.palette_selected));
    frame.render_stateful_widget(
        List::new(items)
            .style(Style::default().bg(SLATE).fg(ICE))
            .block(block(
                " Commands · ↑/↓ choose · Enter run · Esc close ",
                true,
            ))
            .highlight_style(Style::default().bg(Color::Rgb(48, 66, 87)).fg(SEA))
            .highlight_symbol("› "),
        area,
        &mut selected,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_shortcuts_work_inside_composer_and_do_not_post_commands() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("lifecycle-ui.sqlite")).await?;
        let mut app = UiState {
            managed: true,
            composing: true,
            draft: "draft objective stays intact".into(),
            ..UiState::default()
        };
        for (key, matches_action) in [('p', 0), ('r', 1), ('x', 2), ('s', 3), ('n', 4)] {
            app.key(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                &store,
                1,
            )
            .await?;
            app.key(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &store,
                1,
            )
            .await?;
            let action = app
                .actions
                .pop_front()
                .context("shortcut did not queue action")?;
            assert!(match matches_action {
                0 => matches!(action, Action::Pause),
                1 => matches!(action, Action::Resume),
                2 => matches!(action, Action::Stop),
                3 => matches!(action, Action::Sessions),
                _ => matches!(action, Action::NewSession),
            });
            assert_eq!(app.draft, "draft objective stays intact");
        }
        for command in ["/pause", "/resume", "/stop", "/sessions", "/new"] {
            app.draft = command.into();
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 1)
                .await?;
            assert!(app.draft.is_empty());
            assert!(!app.actions.is_empty());
            app.actions.clear();
        }
        assert!(store.read_board(0, 100).await?.is_empty());
        assert!(
            matches!(crate::tui_menu::command("/start fix session controls"), Some(Action::Submit(text)) if text == "fix session controls")
        );
        assert!(
            matches!(crate::tui_menu::command("/session external-id"), Some(Action::OpenSession(id)) if id == "external-id")
        );
        Ok(())
    }

    #[test]
    fn managed_console_shows_workspace_state_and_clickable_controls_at_compact_sizes() {
        let metrics = Metrics::new(1);
        for (width, height) in [(60, 18), (80, 24), (150, 44)] {
            let mut app = UiState {
                managed: true,
                busy: true,
                paused: true,
                workspace: "workspace-alpha".into(),
                database: "session-alpha.sqlite3".into(),
                ..UiState::default()
            };
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            for text in [
                "PAUSED",
                "cwd: workspace-alpha",
                "Session: session-alpha.sqlite3",
                "Sessions",
                "New",
                "Start",
                "Resume",
                "Stop",
            ] {
                assert!(
                    rendered.contains(text),
                    "missing {text} at {width}x{height}"
                );
            }
            assert_eq!(app.control_hits.len(), 5);
            assert!(app.control_hits.iter().all(|(rect, _)| rect.width > 0
                && rect.height == 1
                && rect.right() <= width
                && rect.bottom() <= height));
            assert!(matches!(app.control_hits[3].1, Action::Resume));
        }
    }

    #[tokio::test]
    #[ignore = "requires Node.js for a real MCP status-transition fixture"]
    async fn open_mcp_selector_follows_background_backend_transitions_without_losing_search_or_selection(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let gate = directory.path().join("initialize-gate");
        let observed = directory.path().join("initialize-started");
        let script = r#"
const fs = require('node:fs');
require('node:readline').createInterface({input:process.stdin}).on('line', message => {
  const request = JSON.parse(message);
  if (request.id === undefined) return;
  const send = result => process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:request.id,result})+'\n');
  if (request.method === 'initialize') {
    fs.writeFileSync(process.env.OBSERVED, 'started');
    const check = setInterval(() => {
      if (!fs.existsSync(process.env.GATE)) return;
      clearInterval(check);
      send({protocolVersion:request.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'live-status',version:'1'}});
    }, 5);
  } else if (request.method === 'tools/list') send({tools:[]});
});
"#;
        let hub = crate::mcp::Hub::new(
            std::collections::BTreeMap::from([
                (
                    "ready-fixture".into(),
                    crate::mcp::ServerConfig {
                        kind: "local".into(),
                        command: vec!["node".into(), "--eval".into(), script.into()],
                        environment: std::collections::BTreeMap::from([
                            ("GATE".into(), gate.display().to_string()),
                            ("OBSERVED".into(), observed.display().to_string()),
                        ]),
                        enabled: true,
                        ..Default::default()
                    },
                ),
                (
                    "failed-fixture".into(),
                    crate::mcp::ServerConfig {
                        kind: "local".into(),
                        command: vec![directory
                            .path()
                            .join("missing-program")
                            .display()
                            .to_string()],
                        enabled: true,
                        ..Default::default()
                    },
                ),
            ]),
            directory.path().to_owned(),
        );
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let mut app = UiState {
                menu: Some(Menu::new(
                    Kind::Mcp,
                    mcp_entries(&hub),
                    Some("ready-fixture"),
                )),
                ..UiState::default()
            };
            app.menu.as_mut().unwrap().query = "fixture".into();
            hub.prepare_enabled().await;
            while !observed.is_file()
                || !hub
                    .statuses()
                    .iter()
                    .any(|(id, status)| id == "failed-fixture" && status.contains("failed"))
            {
                tokio::task::yield_now().await;
            }
            refresh_mcp_menu(&mut app, &hub);
            let menu = app.menu.as_ref().unwrap();
            assert_eq!(menu.filtered()[menu.selected].id, "ready-fixture");
            assert_eq!(menu.filtered()[menu.selected].detail, "connecting");
            assert!(menu
                .entries
                .iter()
                .any(|entry| entry.id == "failed-fixture" && entry.detail.contains("failed")));
            // Release a real held handshake, rather than changing UI state directly.
            std::fs::write(&gate, "continue")?;
            while !hub
                .statuses()
                .iter()
                .any(|(id, status)| id == "ready-fixture" && status.starts_with("ready"))
            {
                tokio::task::yield_now().await;
            }
            refresh_mcp_menu(&mut app, &hub);
            let menu = app.menu.as_ref().unwrap();
            assert_eq!(menu.query, "fixture");
            assert_eq!(menu.filtered()[menu.selected].id, "ready-fixture");
            assert!(menu.filtered()[menu.selected].detail.starts_with("ready"));
            hub.toggle("ready-fixture").await?;
            refresh_mcp_menu(&mut app, &hub);
            let menu = app.menu.as_ref().unwrap();
            assert_eq!(menu.filtered()[menu.selected].detail, "disabled");
            assert_eq!(menu.query, "fixture");
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("MCP live-menu fixture stalled")?;
        hub.shutdown().await;
        result
    }

    #[tokio::test]
    async fn roster_controls_run_while_unrelated_mutation_is_pending_and_keep_navigation_responsive(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let harness = crate::runtime::Harness::new(crate::config::Config {
            agents: 2,
            mock: true,
            interactive_session: true,
            objective: String::new(),
            workspace: directory.path().to_owned(),
            database: directory.path().join("responsive.sqlite"),
            ..crate::config::Config::default()
        })
        .await?;
        let auth =
            crate::auth::AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
        let manager = ProviderManager::new(
            crate::catalog::Catalog::from_json("{}")?,
            harness.control.clone(),
            |_, config, _, _, _, _| Ok(config.clone()),
            &auth,
        );
        let (release, held) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            held.await?;
            Ok(JobResult::Connected("fixture".into()))
        });
        let mut main_job = Some((0, false, false, pending));
        let mut app = UiState {
            managed: true,
            draft: "retain this draft".into(),
            ..UiState::default()
        };
        app.actions
            .extend([Action::Models, Action::AddAgents(2), Action::Members]);
        let mut roster_jobs = tokio::task::JoinSet::new();
        dispatch_roster_actions(
            &mut app,
            Some(&manager),
            &harness.store,
            &mut roster_jobs,
            true,
        )
        .await?;
        cancel_stale_menu_job(app.menu_epoch, &mut main_job);
        assert!(
            main_job.as_ref().is_some_and(|job| !job.3.is_finished()),
            "opening roster controls never cancels the pending mutation"
        );
        assert!(
            matches!(app.actions.pop_front(), Some(Action::Models)),
            "unrelated queued actions keep their order"
        );
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.kind),
            Some(Kind::Members)
        ));
        assert!(matches!(
            roster_jobs.join_next().await.unwrap()??,
            JobResult::MembershipChanged(_)
        ));
        assert_eq!(manager.control.members().len(), 4);
        {
            let menu = app.menu.as_mut().unwrap();
            menu.query = "agent".into();
            menu.selected = menu
                .filtered()
                .iter()
                .position(|entry| entry.id == "agent-002")
                .unwrap();
        }
        refresh_roster_menu(&mut app, &manager.control.members());
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.entries.len(), 5);
        assert_eq!(menu.query, "agent");
        assert_eq!(
            menu.filtered()[menu.selected].id,
            "agent-002",
            "live menu refresh preserves the highlighted agent and search"
        );
        app.menu = None;
        app.key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &harness.store,
            4,
        )
        .await?;
        assert_eq!(
            app.selected, 1,
            "navigation still runs while another action is pending"
        );
        assert_eq!(app.draft, "retain this draft");
        let removed = manager.control.members().last().unwrap().clone();
        app.actions
            .extend([Action::RemoveAgents(vec![removed]), Action::RemoveList]);
        dispatch_roster_actions(
            &mut app,
            Some(&manager),
            &harness.store,
            &mut roster_jobs,
            true,
        )
        .await?;
        assert!(matches!(
            roster_jobs.join_next().await.unwrap()??,
            JobResult::MembershipChanged(_)
        ));
        assert_eq!(manager.control.members().len(), 3);
        refresh_roster_menu(&mut app, &manager.control.members());
        assert_eq!(
            app.menu.as_ref().unwrap().entries.len(),
            3,
            "an open removal selector catches up after parallel mutations"
        );
        assert!(harness.store.prompts().await?.is_empty());
        for (action, paused) in [
            (Action::Pause, true),
            (Action::Resume, false),
            (Action::Stop, false),
        ] {
            app.actions.push_back(action);
            dispatch_roster_actions(
                &mut app,
                Some(&manager),
                &harness.store,
                &mut roster_jobs,
                true,
            )
            .await?;
            let result = tokio::time::timeout(Duration::from_secs(2), roster_jobs.join_next())
                .await?
                .context("missing lifecycle job")???;
            assert!(matches!(result, JobResult::LifecycleChanged(_)));
            assert_eq!(manager.control.is_paused(), paused);
            assert!(
                main_job.as_ref().is_some_and(|job| !job.3.is_finished()),
                "lifecycle controls cannot wait on a remote action"
            );
        }
        assert!(harness.store.prompts().await?.is_empty());
        app.actions.extend([
            Action::NewSession,
            Action::OpenSession("held-session".into()),
        ]);
        dispatch_roster_actions(
            &mut app,
            Some(&manager),
            &harness.store,
            &mut roster_jobs,
            true,
        )
        .await?;
        assert!(app.notice.contains("Wait for pending changes"));
        assert!(
            roster_jobs.is_empty(),
            "navigation must not detach while a remote mutation is pending"
        );
        assert!(!manager.control.is_closing());
        assert!(manager.control.take_navigation().is_none());
        release.send(()).unwrap();
        let (epoch, _, _, job) = main_job.take().unwrap();
        if let JobResult::Connected(provider) = job.await?? {
            apply_connected_result(&mut app, epoch, provider);
        } else {
            anyhow::bail!("held connection returned an unexpected result");
        }
        assert!(
            app.actions.is_empty(),
            "an older connection cannot steal focus with /models"
        );
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.kind),
            Some(Kind::RemoveAgents)
        ));
        app.menu = None;
        let current_epoch = app.menu_epoch;
        apply_connected_result(&mut app, current_epoch, "fixture".into());
        assert!(
            matches!(app.actions.pop_front(), Some(Action::Models)),
            "a current connection still opens its normal model-selection follow-up"
        );
        Ok(())
    }

    #[tokio::test]
    async fn managed_membership_actions_update_roster_silently_before_prompt_submission(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let harness = crate::runtime::Harness::new(crate::config::Config {
            agents: 2,
            mock: true,
            interactive_session: true,
            objective: String::new(),
            workspace: directory.path().to_owned(),
            database: directory.path().join("menu.sqlite"),
            ..crate::config::Config::default()
        })
        .await?;
        let auth =
            crate::auth::AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
        let manager = ProviderManager::new(
            crate::catalog::Catalog::from_json("{}")?,
            harness.control.clone(),
            |_, config, _, _, _, _| Ok(config.clone()),
            &auth,
        );
        let store = harness.store.clone();
        let result = perform(Action::AddAgents(2), manager.clone(), store.clone()).await?;
        assert!(matches!(result, JobResult::MembershipChanged(_)));
        let ids = manager.control.members();
        assert_eq!(ids.len(), 4);
        let removed = ids[3].clone();
        perform(
            Action::RemoveAgents(vec![removed.clone()]),
            manager.clone(),
            store.clone(),
        )
        .await?;
        assert_eq!(manager.control.members().len(), 3);
        let result = perform(Action::RemoveList, manager.clone(), store.clone()).await?;
        assert!(
            matches!(result, JobResult::Open(Kind::RemoveAgents, entries)
            if entries.len() == 3 && entries.iter().all(|entry| entry.id != removed))
        );
        let before = store.latest_seq().await?;
        assert!(perform(
            Action::InvalidCommand("Usage: /add [count]".into()),
            manager,
            store.clone()
        )
        .await
        .is_err());
        assert_eq!(store.latest_seq().await?, before);
        assert!(store.prompts().await?.is_empty());
        let reopened = Store::open(directory.path().join("menu.sqlite")).await?;
        assert_eq!(reopened.membership().await?.agent_ids.len(), 3);
        assert!(
            reopened.read_board(0, 100).await?.is_empty(),
            "pre-objective add/remove controls do not post shared-board notices"
        );
        Ok(())
    }

    #[tokio::test]
    async fn membership_commands_and_shortcuts_preserve_drafts_without_posting_tasks() -> Result<()>
    {
        let store = Store::open(":memory:").await?;
        let mut app = UiState {
            managed: true,
            composing: true,
            draft: "unfinished owner instruction".into(),
            ..UiState::default()
        };
        for (key, add) in [('+', true), ('-', false)] {
            app.key(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                &store,
                2,
            )
            .await?;
            app.key(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &store,
                2,
            )
            .await?;
            assert!(matches!(
                (app.actions.pop_front(), add),
                (Some(Action::AddList), true) | (Some(Action::RemoveList), false)
            ));
            assert_eq!(app.draft, "unfinished owner instruction");
        }
        for text in [
            "/add 4",
            "/remove agent-002 agent-003",
            "/add invalid",
            "/members",
        ] {
            app.draft = text.into();
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 2)
                .await?;
            let action = app.actions.pop_front().unwrap();
            match text {
                "/add 4" => assert!(matches!(action, Action::AddAgents(4))),
                "/remove agent-002 agent-003" => assert!(matches!(action,
                    Action::RemoveAgents(ids) if ids == ["agent-002", "agent-003"])),
                "/add invalid" => assert!(matches!(action, Action::InvalidCommand(_))),
                _ => assert!(matches!(action, Action::Members)),
            }
        }
        assert_eq!(
            store.latest_seq().await?,
            0,
            "operator controls never become owner objectives"
        );
        let mut menu = Menu::commands();
        menu.paste("add");
        assert!(matches!(
            menu.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Action(Action::AddList)
        ));
        menu.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(
                menu.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Outcome::Action(Action::Members)
            ),
            "search-result navigation chooses the highlighted menu entry"
        );
        let mut menu = Menu::commands();
        menu.paste("add 7");
        assert!(matches!(
            menu.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Action(Action::AddAgents(7))
        ));
        let mut menu = Menu::new(
            Kind::RemoveAgents,
            vec![Entry {
                id: "agent-002".into(),
                label: "agent-002".into(),
                detail: "drain gracefully".into(),
            }],
            None,
        );
        assert!(
            matches!(menu.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Action(Action::RemoveAgents(ids)) if ids == ["agent-002"])
        );
        Ok(())
    }

    #[test]
    fn header_quorum_uses_current_members_instead_of_historical_metrics_slots() {
        let metrics = Metrics::new(8);
        let mut app = UiState {
            member_count: Some(3),
            draining_count: 2,
            current_votes: Some(1),
            ..UiState::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("3 agents  2 draining"));
        assert!(rendered.contains("1 done votes / 3 required"));
    }

    #[tokio::test]
    async fn dismissing_pending_discovery_frees_job_slot_without_cancelling_mutations() {
        let (dropped, notification) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _notify_on_drop = dropped;
            std::future::pending::<Result<JobResult>>().await
        });
        let mut job = Some((1, false, true, handle));
        cancel_stale_menu_job(1, &mut job);
        assert!(job.is_some(), "current menu discovery stays active");
        cancel_stale_menu_job(2, &mut job);
        assert!(
            job.is_none(),
            "dismissed discovery releases the action slot"
        );
        assert!(notification.await.is_err(), "discovery task was aborted");

        let (finish, pending) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            pending.await?;
            Ok(JobResult::Copied)
        });
        job = Some((1, false, false, handle));
        cancel_stale_menu_job(2, &mut job);
        assert!(job.is_some(), "side effects keep running after dismissal");
        finish.send(()).unwrap();
        assert!(matches!(
            job.take().unwrap().3.await.unwrap().unwrap(),
            JobResult::Copied
        ));
    }

    #[tokio::test]
    async fn leader_shortcuts_and_variant_cycle_preserve_unsent_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("keys.sqlite3")).await.unwrap();
        let mut app = UiState {
            managed: true,
            composing: true,
            draft: "keep this draft".into(),
            ..UiState::default()
        };
        app.key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(app.leader);
        assert!(app.actions.is_empty());
        app.key(
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(matches!(app.actions.pop_front(), Some(Action::Models)));
        app.key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(
            matches!(app.actions.pop_front(), Some(Action::Variants)),
            "leader T opens the menu rather than cycling"
        );
        app.key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(matches!(app.actions.pop_front(), Some(Action::Cycle)));
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(app.palette);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert!(app.composing);
        assert_eq!(app.draft, "keep this draft");
        assert!(store.prompts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn slash_commands_are_not_sent_as_prompts_and_prompt_hit_regions_track_wrapping() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("slash.sqlite3"))
            .await
            .unwrap();
        let mut app = UiState {
            managed: true,
            composing: true,
            draft: "/models".into(),
            ..UiState::default()
        };
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert!(matches!(app.actions.pop_front(), Some(Action::Models)));
        assert_eq!(store.latest_seq().await.unwrap(), 0);
        let prompt = store
            .append(
                "owner",
                "a visible prompt with wrapped words for click actions",
                true,
            )
            .await
            .unwrap();
        app.load(&store).await.unwrap();
        let metrics = Metrics::new(8);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
            .unwrap();
        assert!(app.prompt_hits.iter().any(|(_, seq)| *seq == prompt.seq));
        app.grid = true;
        let large = Metrics::new(500);
        app.selected = 499;
        terminal
            .draw(|frame| draw(frame, &mut app, &large.snapshot(), &large))
            .unwrap();
        assert!(app.grid_hits.len() <= app.grid_page_size);
        assert!(app.grid_hits.iter().any(|(_, index)| *index == 499));
        assert!(
            app.prompt_hits.is_empty(),
            "hidden board prompts have no stale mouse regions"
        );
    }
    use ratatui::backend::TestBackend;

    #[tokio::test]
    async fn command_palette_routes_actions_and_escape_only_closes_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("palette.sqlite"))
            .await
            .unwrap();
        let mut app = UiState::default();
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(app.palette);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert!(!app.palette);
        assert!(!app.composing);
        app.key(
            KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
            &store,
            8,
        )
        .await
        .unwrap();
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert!(app.composing);
        assert!(!app.palette);
        app.draft = "palette-selected owner message".into();
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert_eq!(
            store.read_board(0, 10).await.unwrap()[0].body,
            "palette-selected owner message"
        );
    }

    #[test]
    fn compact_console_keeps_selected_stream_and_connection_visible() {
        let metrics = Metrics::new(1);
        metrics.append_output("agent-001", "Visible selected stream");
        let mut app = UiState {
            focus: Focus::Detail,
            session: SessionInfo {
                provider: "codex-lb".into(),
                model: "gpt-5.4".into(),
                variant: "high".into(),
                objective: "Ship the objective".into(),
            },
            ..UiState::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("codex-lb / gpt-5.4"));
        assert!(rendered.contains("Visible selected stream"));
        assert!(rendered.contains("3 Stream"));
        assert!(app.panels[2].height > 0);
    }

    #[test]
    fn renders_operator_board_agent_and_throughput_panels_at_two_sizes() {
        let metrics = Metrics::new(500);
        metrics.set_status("agent-001", AgentStatus::Tool);
        metrics.append_output("agent-001", "working on the shared workspace");
        let mut app = UiState::default();
        app.board.push(BoardMessage {
            seq: 1,
            sender: "owner".into(),
            body: "verify the integration".into(),
            owner: true,
            created_at_ms: 0,
        });
        app.latest = 1;
        for (width, height) in [(150, 44), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("openraid"));
            assert!(rendered.contains("global board"));
            assert!(rendered.contains("verify the integration"));
            assert!(rendered.contains("agent-001"));
        }
    }

    #[tokio::test]
    async fn owner_composer_commits_to_global_board_and_detach_preserves_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("ui.sqlite")).await.unwrap();
        let mut app = UiState {
            composing: true,
            draft: "owner instruction".into(),
            ..UiState::default()
        };
        assert!(!app
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap());
        let board = store.read_board(0, 100).await.unwrap();
        assert_eq!(board.len(), 1);
        assert!(board[0].owner);
        assert_eq!(board[0].body, "owner instruction");
        assert!(app
            .key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &store,
                8
            )
            .await
            .unwrap());
        assert_eq!(store.latest_seq().await.unwrap(), 1);
    }

    #[test]
    fn auto_follow_reaches_final_lines_after_word_wrapping() {
        let metrics = Metrics::new(1);
        metrics.append_output(
            "agent-001",
            &format!("{}\nlast-stream-line", "abcdefgh ".repeat(300)),
        );
        let mut app = UiState::default();
        app.board.push(BoardMessage {
            seq: 1,
            sender: "owner".into(),
            body: format!("{}\nlast-board-line", "abcdefgh ".repeat(300)),
            owner: true,
            created_at_ms: 0,
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("last-board-line"));
        assert!(rendered.contains("last-stream-line"));
        assert!(app.board_scroll > 0);
        assert!(app.detail_scroll > 0);
    }

    #[tokio::test]
    async fn owner_composer_preserves_altgr_text_and_standalone_shortcuts() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("altgr.sqlite")).await.unwrap();
        let mut app = UiState {
            composing: true,
            ..UiState::default()
        };
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        for (character, modifiers) in [
            ('i', altgr),
            ('İ', altgr | KeyModifiers::SHIFT),
            ('u', altgr),
            ('q', KeyModifiers::NONE),
            ('o', KeyModifiers::NONE),
            ('x', KeyModifiers::CONTROL),
            ('y', KeyModifiers::ALT),
        ] {
            assert!(!app
                .key(
                    KeyEvent::new(KeyCode::Char(character), modifiers),
                    &store,
                    8
                )
                .await
                .unwrap());
        }
        app.key(
            KeyEvent::new_with_kind(KeyCode::Char('i'), altgr, KeyEventKind::Release),
            &store,
            8,
        )
        .await
        .unwrap();
        assert_eq!(app.draft, "iİuqo");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &store, 8)
            .await
            .unwrap();
        assert_eq!(store.read_board(0, 100).await.unwrap()[0].body, "iİuqo");
        app.composing = true;
        app.draft = "clear this".into();
        app.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &store,
            8,
        )
        .await
        .unwrap();
        assert!(app.draft.is_empty());
    }
}
