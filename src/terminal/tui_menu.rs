use crate::quick::Entry;
use crate::theme::{self, Palette};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

#[derive(Clone)]
pub enum Kind {
    Sessions,
    Mcp,
    Members,
    AddAgents,
    RemoveAgents,
    Commands,
    Board,
    ClearBoard,
    Connect,
    Models,
    Themes,
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
    AddList,
    RemoveList,
    AddAgents(usize),
    RemoveAgents(Vec<String>),
    InvalidCommand(String),
    ToggleMcp(String),
    Models,
    Themes,
    SelectTheme(String),
    Connect,
    Variants,
    Cycle,
    JumpList,
    Grid,
    Help,
    Quit,
    Board,
    ConfirmClearBoard,
    ClearBoard,
    ExportBoard(Option<String>),
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
    pub marked: std::collections::BTreeSet<String>,
    pub active_theme: Option<String>,
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
        "/board" => Some(Action::Board),
        "/clear-board" => Some(Action::ConfirmClearBoard),
        "/export-board" => Some(Action::ExportBoard(
            text.trim()
                .strip_prefix("/export-board")
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        )),
        "/models" | "/model" => Some(
            words
                .next()
                .map(|id| Action::SelectModel(id.into()))
                .unwrap_or(Action::Models),
        ),
        "/connect" => Some(Action::Connect),
        "/themes" | "/theme" => Some(match words.next() {
            None => Action::Themes,
            Some(id) if words.next().is_none() => Action::SelectTheme(id.into()),
            Some(_) => Action::InvalidCommand("Usage: /themes [theme-id]".into()),
        }),
        "/mcp" => Some(Action::Mcp),
        "/members" => Some(Action::Members),
        "/add" => {
            let Some(count) = words.next() else {
                return Some(Action::AddList);
            };
            let count = count.parse::<usize>();
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
            marked: Default::default(),
            active_theme: None,
        }
    }
    pub fn themes(active_id: &str) -> Self {
        let active = theme::find_theme(active_id).unwrap_or_else(theme::current_theme);
        let entries = theme::THEMES
            .iter()
            .map(|theme| Entry {
                id: theme.id.into(),
                label: theme.name.into(),
                detail: format!(
                    "{} · {}",
                    if theme.dark { "Dark" } else { "Light" },
                    theme.description
                ),
            })
            .collect();
        let mut menu = Self::new(Kind::Themes, entries, Some(active.id));
        menu.active_theme = Some(active.id.into());
        menu
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
                    "Ctrl+X X · immediately cancel agents and active operations; return idle",
                ),
                (
                    "/members",
                    "Manage parallel agents",
                    "Inspect the current roster; add or gracefully remove workers",
                ),
                (
                    "/board",
                    "Manage the shared messageboard",
                    "Export all messages or clear history with a warning",
                ),
                (
                    "/export-board",
                    "Export the complete messageboard",
                    "Optional destination path; existing files are never overwritten",
                ),
                (
                    "/clear-board",
                    "Clear the messageboard",
                    "Idle only; requires confirmation and deletes history/checkpoints",
                ),
                (
                    "/add",
                    "Add parallel agents",
                    "Ctrl+X then + · choose a count, or /add 5",
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
                    "/themes",
                    "Choose your terminal theme",
                    "Preview ten dark and light palettes · /themes theme-id applies directly",
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
                    "Watch the whole swarm in a paged grid · Enter inspects full agent activity",
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
                query.split_whitespace().all(|word| {
                    if matches!(self.kind, Kind::Themes) && matches!(word, "dark" | "light") {
                        return theme::find_theme(&entry.id)
                            .is_some_and(|theme| theme.dark == (word == "dark"));
                    }
                    text.contains(word)
                })
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
            KeyCode::Char(' ') if matches!(self.kind, Kind::Members | Kind::RemoveAgents) => {
                if let Some(id) = self
                    .filtered()
                    .get(self.selected)
                    .filter(|entry| entry.id != "add")
                    .map(|entry| entry.id.clone())
                {
                    if !self.marked.remove(&id) {
                        self.marked.insert(id);
                    }
                }
            }
            KeyCode::Char('a')
                if key.modifiers == KeyModifiers::CONTROL
                    && matches!(self.kind, Kind::Members | Kind::RemoveAgents) =>
            {
                let ids: Vec<_> = self
                    .filtered()
                    .iter()
                    .filter(|entry| entry.id != "add")
                    .map(|entry| entry.id.clone())
                    .collect();
                if ids.iter().all(|id| self.marked.contains(id)) {
                    for id in ids {
                        self.marked.remove(&id);
                    }
                } else {
                    self.marked.extend(ids);
                }
            }
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
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.filtered().len().saturating_sub(1),
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
                    Kind::AddAgents => {
                        let count = self.query.trim().parse::<usize>();
                        return Outcome::Action(match count {
                            Ok(count) if (1..=500).contains(&count) => Action::AddAgents(count),
                            _ => Action::InvalidCommand("Enter an agent count from 1 to 500; the total roster cannot exceed 500.".into()),
                        });
                    }
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
                if matches!(self.kind, Kind::Members | Kind::RemoveAgents)
                    && !self.marked.is_empty()
                {
                    let ids = self
                        .entries
                        .iter()
                        .filter(|entry| self.marked.contains(&entry.id))
                        .map(|entry| entry.id.clone())
                        .collect::<Vec<_>>();
                    if !ids.is_empty() {
                        return Outcome::Action(Action::RemoveAgents(ids));
                    }
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
                    Kind::Board => Outcome::Action(if entry.id == "clear" {
                        Action::ConfirmClearBoard
                    } else {
                        Action::ExportBoard(None)
                    }),
                    Kind::ClearBoard => {
                        if entry.id == "clear" {
                            Outcome::Action(Action::ClearBoard)
                        } else {
                            Outcome::Close
                        }
                    }
                    Kind::Commands => Outcome::Action(command(&entry.id).unwrap()),
                    Kind::Sessions => Outcome::Action(Action::OpenSession(entry.id.clone())),
                    Kind::Connect => Outcome::Provider(entry.id.clone()),
                    Kind::Models => Outcome::Action(Action::SelectModel(entry.id.clone())),
                    Kind::Themes => Outcome::Action(Action::SelectTheme(entry.id.clone())),
                    Kind::Mcp => Outcome::Action(Action::ToggleMcp(entry.id.clone())),
                    Kind::Members if entry.id == "add" => Outcome::Action(Action::AddList),
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
        let palette = theme::current_theme().palette;
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(100);
        let height = area.height.saturating_sub(4).min(24);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        if matches!(self.kind, Kind::Themes) {
            self.draw_themes(frame, popup, palette);
            return;
        }
        let (title, help) = match &self.kind {
            Kind::Board => (" Messageboard controls ", "Export includes every message, not only the visible page"),
            Kind::ClearBoard => (" WARNING: permanently clear history? ", "Deletes messages, prompts/snapshots, checkpoints and votes. Workspace and roster remain. Esc cancels."),
            Kind::Sessions => (" /sessions · this workspace ", "Enter opens the selected session when idle · /new creates fresh history"),
            Kind::Members => (
                " /members · parallel agents ",
                "Space marks agents · Ctrl+A toggles visible agents · Enter removes marked agents or opens add count",
            ),
            Kind::AddAgents => (
                " /add · parallel agents ",
                "Enter how many agents to add (1–500). Total roster cannot exceed 500. Esc cancels.",
            ),
            Kind::RemoveAgents => (
                " /remove · parallel agents ",
                "Space marks agents · Ctrl+A toggles visible agents · Enter removes marked agents (or highlighted agent)",
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
            Kind::Themes => unreachable!("theme picker has its own layout"),
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
            .style(Style::default().bg(palette.surface).fg(palette.text))
            .border_style(Style::default().fg(palette.accent));
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
            Paragraph::new(help).style(Style::default().fg(palette.muted)),
            rows[0],
        );
        let query = if matches!(self.kind, Kind::Key { .. }) {
            "•".repeat(self.query.chars().count())
        } else {
            self.query.clone()
        };
        frame.render_widget(
            Paragraph::new(format!("{query}▏")).block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border))
                    .title(if matches!(self.kind, Kind::Key { .. }) {
                        " Key · hidden "
                    } else {
                        " Search / input "
                    }),
            ),
            rows[1],
        );
        if !matches!(
            self.kind,
            Kind::Key { .. } | Kind::Endpoint(_) | Kind::AddAgents
        ) {
            let filtered = self.filtered();
            let items: Vec<_> = filtered
                .iter()
                .map(|entry| {
                    ListItem::new(format!(
                        "{}\n{}",
                        if matches!(self.kind, Kind::Members | Kind::RemoveAgents)
                            && entry.id != "add"
                        {
                            format!(
                                "[{}] {}",
                                if self.marked.contains(&entry.id) {
                                    "x"
                                } else {
                                    " "
                                },
                                entry.label
                            )
                        } else {
                            entry.label.clone()
                        },
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
                            .bg(palette.selection)
                            .fg(palette.selection_text)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                rows[2],
                &mut state,
            );
            frame.render_widget(
                Paragraph::new(
                    if matches!(self.kind, Kind::Members | Kind::RemoveAgents)
                        && !self.marked.is_empty()
                    {
                        format!(
                            "{} marked across all filters · Enter removes batch · Esc cancels",
                            self.marked.len()
                        )
                    } else if filtered.is_empty() {
                        "No matching entries. Adjust the search or press Esc.".into()
                    } else if matches!(self.kind, Kind::Members | Kind::RemoveAgents) {
                        "Space mark · Ctrl+A visible · Enter select · keep at least one agent"
                            .into()
                    } else {
                        "↑/↓ choose · Enter select · Esc back".into()
                    },
                )
                .style(Style::default().fg(palette.muted)),
                rows[3],
            );
        }
    }

    fn draw_themes(&self, frame: &mut Frame<'_>, popup: Rect, palette: Palette) {
        frame.render_widget(Clear, popup);
        let block = Block::bordered()
            .title(" /themes · choose a palette ")
            .style(Style::default().bg(palette.surface).fg(palette.text))
            .border_style(Style::default().fg(palette.accent));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new("Browse a preview; Enter saves your choice.")
                .style(Style::default().fg(palette.muted)),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(format!("{}▏", self.query)).block(
                Block::bordered()
                    .title(" Search names / dark / light ")
                    .border_style(Style::default().fg(palette.border)),
            ),
            rows[1],
        );
        let filtered = self.filtered();
        let selected = self.selected.min(filtered.len().saturating_sub(1));
        let candidate = filtered
            .get(selected)
            .and_then(|entry| theme::find_theme(&entry.id));
        let (list_area, preview_area) = if rows[2].width >= 74 {
            let panes =
                Layout::horizontal([Constraint::Length(32), Constraint::Min(1)]).split(rows[2]);
            (panes[0], Some(panes[1]))
        } else if rows[2].height >= 12 {
            let panes =
                Layout::vertical([Constraint::Min(5), Constraint::Length(7)]).split(rows[2]);
            (panes[0], Some(panes[1]))
        } else {
            (rows[2], None)
        };
        if filtered.is_empty() {
            frame.render_widget(
                Paragraph::new("No matching themes. Clear the search with Ctrl+U.")
                    .style(Style::default().fg(palette.muted))
                    .wrap(Wrap { trim: true }),
                list_area,
            );
        } else {
            let items: Vec<_> = filtered
                .iter()
                .map(|entry| {
                    let active = self.active_theme.as_deref() == Some(entry.id.as_str());
                    ListItem::new(Line::from(vec![Span::raw(format!(
                        "{}{}",
                        entry.label,
                        if active { " ✓" } else { "" }
                    ))]))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(selected));
            frame.render_stateful_widget(
                List::new(items).highlight_symbol("› ").highlight_style(
                    Style::default()
                        .bg(palette.selection)
                        .fg(palette.selection_text)
                        .add_modifier(Modifier::BOLD),
                ),
                list_area,
                &mut state,
            );
        }
        if let (Some(preview_area), Some(candidate)) = (preview_area, candidate) {
            let p = candidate.palette;
            let compact = preview_area.height < 10;
            let mut lines = vec![
                Line::styled(candidate.description, Style::default().fg(p.muted)),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("● Working", Style::default().fg(p.success)),
                    Span::styled("  ◐ Waiting", Style::default().fg(p.warning)),
                ]),
            ];
            if !compact {
                lines.extend([
                    Line::styled(
                        "Swarm / agent-01",
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                    Line::raw("Reviewing workspace changes…"),
                    Line::styled("Shared board · 10 agents", Style::default().fg(p.muted)),
                    Line::raw(""),
                    Line::styled(
                        "› Selected agent",
                        Style::default()
                            .bg(p.selection)
                            .fg(p.selection_text)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled("! Request needs attention", Style::default().fg(p.error)),
                ]);
            }
            lines.push(Line::styled(
                "Prompt › Build something good▏",
                Style::default().fg(p.accent),
            ));
            frame.render_widget(
                Paragraph::new(lines)
                    .style(Style::default().bg(p.background).fg(p.text))
                    .block(
                        Block::bordered()
                            .title(format!(
                                " {} · {} preview ",
                                candidate.name,
                                if candidate.dark { "dark" } else { "light" }
                            ))
                            .border_style(Style::default().fg(p.border)),
                    )
                    .wrap(Wrap { trim: false }),
                preview_area,
            );
        }
        let footer = candidate.map_or_else(
            || "Ctrl+U clear search · Esc cancel".to_owned(),
            |candidate| {
                format!(
                    "↑/↓ browse · Enter apply · Esc cancel · {} · ✓ current",
                    candidate.id
                )
            },
        );
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(palette.muted)),
            rows[3],
        );
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn agents() -> Vec<Entry> {
        (1..=3)
            .map(|n| Entry {
                id: format!("agent-{n:03}"),
                label: format!("agent-{n:03}"),
                detail: String::new(),
            })
            .collect()
    }

    #[test]
    fn add_popup_accepts_counts_and_rejects_out_of_bounds() {
        assert!(matches!(command("/add"), Some(Action::AddList)));
        for count in ["1", "25", "500"] {
            let mut menu = Menu::new(Kind::AddAgents, Vec::new(), None);
            menu.paste(count);
            assert!(
                matches!(menu.key(key(KeyCode::Enter)), Outcome::Action(Action::AddAgents(n)) if n == count.parse::<usize>().unwrap())
            );
        }
        for count in ["", "0", "501", "-1", "1 2", "invalid"] {
            let mut menu = Menu::new(Kind::AddAgents, Vec::new(), None);
            menu.paste(count);
            assert!(matches!(
                menu.key(key(KeyCode::Enter)),
                Outcome::Action(Action::InvalidCommand(_))
            ));
        }
    }

    #[test]
    fn marked_agents_survive_filtering_and_submit_as_one_batch() {
        let mut menu = Menu::new(Kind::RemoveAgents, agents(), None);
        menu.key(key(KeyCode::Char(' ')));
        menu.key(key(KeyCode::Down));
        menu.key(key(KeyCode::Char(' ')));
        menu.paste("003");
        assert!(
            matches!(menu.key(key(KeyCode::Enter)), Outcome::Action(Action::RemoveAgents(ids)) if ids == ["agent-001", "agent-002"])
        );
    }

    #[test]
    fn select_all_toggles_visible_agents_and_never_marks_add_entry() {
        let mut entries = agents();
        entries.insert(
            0,
            Entry {
                id: "add".into(),
                label: "Add".into(),
                detail: String::new(),
            },
        );
        let mut menu = Menu::new(Kind::Members, entries, None);
        menu.key(key(KeyCode::Char(' ')));
        assert!(menu.marked.is_empty());
        menu.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(menu.marked.len(), 3);
        menu.paste("002");
        menu.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(menu.marked.len(), 2);
        assert!(!menu.marked.contains("agent-002"));
        assert!(
            matches!(menu.key(key(KeyCode::Enter)), Outcome::Action(Action::RemoveAgents(ids)) if ids == ["agent-001", "agent-003"])
        );
    }
}
