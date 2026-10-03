use crate::quick::Entry;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

#[derive(Clone)]
pub enum Kind {
    Sessions,
    Mcp,
    Members,
    RemoveAgents,
    Commands,
    Connect,
    Models,
    Variants,
    Jump,
    Prompt(u64),
    Endpoint(String),
    Key {
        provider: String,
        endpoint: Option<String>,
    },
}
#[derive(Clone)]
pub enum Action {
    Sessions,
    NewSession,
    OpenSession(String),
    Start,
    Pause,
    Resume,
    Stop,
    ChooseProvider(String),
    Mcp,
    Members,
    RemoveList,
    AddAgents(usize),
    RemoveAgents(Vec<String>),
    InvalidCommand(String),
    ToggleMcp(String),
    Models,
    Connect,
    Variants,
    Cycle,
    JumpList,
    Grid,
    Help,
    Quit,
    SelectModel(String),
    SelectVariant(String),
    ConnectKey {
        provider: String,
        key: String,
        endpoint: Option<String>,
    },
    Submit(String),
    Copy(u64),
    Restore(u64),
    Jump(u64),
}
pub struct Menu {
    pub kind: Kind,
    pub entries: Vec<Entry>,
    pub query: String,
    pub selected: usize,
}
pub enum Outcome {
    None,
    Close,
    Action(Action),
    EndpointDone(String, String),
    Provider(String),
}

pub fn command(text: &str) -> Option<Action> {
    let mut words = text.split_whitespace();
    match words.next()? {
        "/sessions" | "/session" => Some(
            words
                .next()
                .map(|id| Action::OpenSession(id.into()))
                .unwrap_or(Action::Sessions),
        ),
        "/new" => Some(Action::NewSession),
        "/start" => {
            let prompt = text.trim().strip_prefix("/start").unwrap_or("").trim();
            Some(if prompt.is_empty() {
                Action::Start
            } else {
                Action::Submit(prompt.into())
            })
        }
        "/pause" => Some(Action::Pause),
        "/resume" => Some(Action::Resume),
        "/stop" => Some(Action::Stop),
        "/models" | "/model" => Some(
            words
                .next()
                .map(|id| Action::SelectModel(id.into()))
                .unwrap_or(Action::Models),
        ),
        "/connect" => Some(Action::Connect),
        "/mcp" => Some(Action::Mcp),
        "/members" => Some(Action::Members),
        "/add" => {
            let count = words.next().unwrap_or("1").parse::<usize>();
            Some(match count {
                Ok(count) if count > 0 && count <= 500 && words.next().is_none() => {
                    Action::AddAgents(count)
                }
                _ => Action::InvalidCommand("Usage: /add [count] (1–500)".into()),
            })
        }
        "/remove" => {
            let ids: Vec<String> = words.map(str::to_owned).collect();
            Some(if ids.is_empty() {
                Action::RemoveList
            } else {
                Action::RemoveAgents(ids)
            })
        }
        "/variant" | "/variants" => Some(
            words
                .next()
                .map(|id| {
                    Action::SelectVariant(if id == "default" {
                        String::new()
                    } else {
                        id.into()
                    })
                })
                .unwrap_or(Action::Variants),
        ),
        "/jump" => Some(Action::JumpList),
        "/agents" | "/grid" => Some(Action::Grid),
        "/help" => Some(Action::Help),
        "/quit" => Some(Action::Quit),
        _ => None,
    }
}

impl Menu {
    pub fn new(kind: Kind, entries: Vec<Entry>, initial: Option<&str>) -> Self {
        let selected = initial
            .and_then(|id| entries.iter().position(|entry| entry.id == id))
            .unwrap_or(0);
        Self {
            kind,
            entries,
            query: String::new(),
            selected,
        }
    }
    pub fn commands() -> Self {
        Self::new(
            Kind::Commands,
            [
                (
                    "/sessions",
                    "Browse workspace sessions",
                    "Ctrl+X S · switch to a saved session when idle",
                ),
                (
                    "/new",
                    "Create a new session",
                    "Ctrl+X N · fresh board and history in this workspace",
                ),
                (
                    "/start",
                    "Start work",
                    "Focus the prompt editor, or /start your objective",
                ),
                (
                    "/pause",
                    "Pause work",
                    "Ctrl+X P · current operations finish; no new work starts",
                ),
                (
                    "/resume",
                    "Resume work",
                    "Ctrl+X R · continue the paused objective",
                ),
                (
                    "/stop",
                    "Stop current work",
                    "Ctrl+X X · drain current operations and return idle",
                ),
                (
                    "/members",
                    "Manage parallel agents",
                    "Inspect the current roster; add or gracefully remove workers",
                ),
                (
                    "/add",
                    "Add a parallel agent",
                    "Ctrl+X then + · /add 5 adds five collaborators",
                ),
                (
                    "/remove",
                    "Remove parallel agents",
                    "Ctrl+X then - · current requests and tools drain gracefully",
                ),
                (
                    "/mcp",
                    "Manage MCP servers",
                    "Inspect connections; enable, disable, or retry",
                ),
                (
                    "/models",
                    "Switch provider and model",
                    "Ctrl+X then M · connected providers only",
                ),
                (
                    "/connect",
                    "Connect a provider",
                    "Ctrl+X then C · save a key securely",
                ),
                (
                    "/variant",
                    "Choose thinking depth",
                    "Ctrl+X then T · Ctrl+T cycles variants",
                ),
                (
                    "/jump",
                    "Jump to a sent prompt",
                    "Search every prompt in this workspace",
                ),
                (
                    "/agents",
                    "Toggle tiled agents",
                    "Watch the whole swarm in a paged grid",
                ),
                (
                    "/help",
                    "Keyboard controls",
                    "Show all commands and navigation",
                ),
                (
                    "/quit",
                    "Close or detach",
                    "Close interactive sessions gracefully; unfinished work remains resumable",
                ),
            ]
            .into_iter()
            .map(|(id, label, detail)| Entry {
                id: id.into(),
                label: label.into(),
                detail: detail.into(),
            })
            .collect(),
            None,
        )
    }
    pub fn filtered(&self) -> Vec<&Entry> {
        let query = self.query.to_lowercase();
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                let text = format!("{} {} {}", entry.id, entry.label, entry.detail).to_lowercase();
                query.split_whitespace().all(|word| text.contains(word))
            })
            .collect();
        if matches!(self.kind, Kind::Commands) {
            let exact = format!("/{}", query.trim().trim_start_matches('/'));
            entries.sort_by_key(|entry| entry.id != exact);
        }
        entries
    }
    pub fn paste(&mut self, text: &str) {
        self.query.push_str(&text.replace(['\r', '\n'], " "));
        self.selected = 0;
    }
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.query.clear();
                self.selected = 0;
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    || key
                        .modifiers
                        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(character);
                self.selected = 0;
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.filtered().len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.filtered().len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if matches!(self.kind, Kind::Commands) && self.query.split_whitespace().count() > 1
                {
                    if let Some(action) =
                        command(&format!("/{}", self.query.trim().trim_start_matches('/')))
                    {
                        return Outcome::Action(action);
                    }
                }
                match &self.kind {
                    Kind::Key { provider, endpoint } => {
                        return Outcome::Action(Action::ConnectKey {
                            provider: provider.clone(),
                            key: self.query.clone(),
                            endpoint: endpoint.clone(),
                        })
                    }
                    Kind::Endpoint(provider) => {
                        return Outcome::EndpointDone(
                            provider.clone(),
                            self.query.trim().to_owned(),
                        )
                    }
                    _ => {}
                }
                let filtered = self.filtered();
                let Some(entry) = filtered.get(self.selected) else {
                    return if matches!(self.kind, Kind::Commands) {
                        Outcome::Action(
                            command(&format!("/{}", self.query.trim_start_matches('/')))
                                .unwrap_or_else(|| Action::Submit(format!("/{}", self.query))),
                        )
                    } else {
                        Outcome::None
                    };
                };
                return match self.kind {
                    Kind::Commands => Outcome::Action(command(&entry.id).unwrap()),
                    Kind::Sessions => Outcome::Action(Action::OpenSession(entry.id.clone())),
                    Kind::Connect => Outcome::Provider(entry.id.clone()),
                    Kind::Models => Outcome::Action(Action::SelectModel(entry.id.clone())),
                    Kind::Mcp => Outcome::Action(Action::ToggleMcp(entry.id.clone())),
                    Kind::Members if entry.id == "add" => Outcome::Action(Action::AddAgents(1)),
                    Kind::Members | Kind::RemoveAgents => {
                        Outcome::Action(Action::RemoveAgents(vec![entry.id.clone()]))
                    }
                    Kind::Variants => Outcome::Action(Action::SelectVariant(entry.id.clone())),
                    Kind::Jump => Outcome::Action(Action::Jump(entry.id.parse().unwrap_or(0))),
                    Kind::Prompt(seq) => Outcome::Action(match entry.id.as_str() {
                        "copy" => Action::Copy(seq),
                        "restore" => Action::Restore(seq),
                        _ => Action::Jump(seq),
                    }),
                    _ => Outcome::None,
                };
            }
            _ => {}
        }
        Outcome::None
    }
    pub fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(100);
        let height = area.height.saturating_sub(4).min(24);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let (title, help) = match &self.kind {
            Kind::Sessions => (" /sessions · this workspace ", "Enter opens the selected session when idle · /new creates fresh history"),
            Kind::Members => (
                " /members · parallel agents ",
                "Enter adds a worker or removes the selected agent after its current operations drain",
            ),
            Kind::RemoveAgents => (
                " /remove · parallel agents ",
                "Enter removes the selected agent; /remove ID ID removes a batch",
            ),
            Kind::Mcp => (
                " /mcp · servers ",
                "Enter enables, disables, or retries the selected server",
            ),
            Kind::Commands => (" Commands ", "Type to search · Enter run · Esc close"),
            Kind::Connect => (
                " /connect · providers ",
                "Choose a provider. Default endpoints are automatic.",
            ),
            Kind::Models => (
                " /models · connected providers ",
                "Search provider/model names · Enter switch · Esc close",
            ),
            Kind::Variants => (
                " /variant · thinking ",
                "Enter select · Ctrl+T cycles available variants",
            ),
            Kind::Jump => (
                " /jump · sent prompts ",
                "Search all prompts · Enter jump · click a prompt for actions",
            ),
            Kind::Prompt(_) => (
                " Prompt actions ",
                "Copy text, restore the prompt/workspace, or jump to it",
            ),
            Kind::Endpoint(provider) if provider == "codex-lb" => (
                " Codex LB endpoint ",
                "API base URL, normally http://127.0.0.1:2455/v1",
            ),
            Kind::Endpoint(_) => (
                " Connect · API endpoint ",
                "Enter the HTTP or HTTPS API base URL for your custom provider",
            ),
            Kind::Key { .. } => (
                " Connect · API key ",
                "Key is hidden · Enter keeps existing credentials · Esc cancel",
            ),
        };
        frame.render_widget(Clear, popup);
        let block = Block::new()
            .borders(Borders::ALL)
            .title(title)
            .style(
                Style::default()
                    .bg(Color::Rgb(21, 38, 56))
                    .fg(Color::Rgb(220, 231, 239)),
            )
            .border_style(Style::default().fg(Color::Rgb(168, 184, 255)));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(help).style(Style::default().fg(Color::Rgb(143, 163, 184))),
            rows[0],
        );
        let query = if matches!(self.kind, Kind::Key { .. }) {
            "•".repeat(self.query.chars().count())
        } else {
            self.query.clone()
        };
        frame.render_widget(
            Paragraph::new(format!("{query}▏")).block(Block::new().borders(Borders::ALL).title(
                if matches!(self.kind, Kind::Key { .. }) {
                    " Key · hidden "
                } else {
                    " Search / input "
                },
            )),
            rows[1],
        );
        if !matches!(self.kind, Kind::Key { .. } | Kind::Endpoint(_)) {
            let filtered = self.filtered();
            let items: Vec<_> = filtered
                .iter()
                .map(|entry| {
                    ListItem::new(format!(
                        "{}\n{}",
                        entry.label,
                        entry
                            .detail
                            .replace('\n', " ")
                            .chars()
                            .take(160)
                            .collect::<String>()
                    ))
                })
                .collect();
            let mut state = ListState::default().with_selected(
                (!items.is_empty()).then_some(self.selected.min(items.len().saturating_sub(1))),
            );
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(
                        Style::default()
                            .bg(Color::Rgb(48, 66, 87))
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                rows[2],
                &mut state,
            );
            frame.render_widget(
                Paragraph::new(if filtered.is_empty() {
                    "No matching entries. Adjust the search or press Esc."
                } else {
                    "↑/↓ choose · Enter select · Esc back"
                }),
                rows[3],
            );
        }
    }
}
