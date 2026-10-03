//! Search-first, keyboard-native launch controls shared by the setup wizard.
use anyhow::{bail, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::{self, IsTerminal, Stdout};

const INK: Color = Color::Rgb(21, 38, 56);
const PAPER: Color = Color::Rgb(220, 231, 239);
const ACCENT: Color = Color::Rgb(168, 184, 255);
const SEA: Color = Color::Rgb(121, 201, 187);
const MUTED: Color = Color::Rgb(143, 163, 184);
const AMBER: Color = Color::Rgb(232, 186, 120);

#[derive(Clone, Debug)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub disabled_reason: Option<String>,
}

impl Choice {
    pub fn new(id: impl Into<String>, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            disabled_reason: None,
        }
    }
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    }
}

pub struct SetupUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    _guard: Guard,
}

impl SetupUi {
    pub fn new() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!("interactive setup needs a terminal; use run --provider ID --model ID --no-tui instead");
        }
        enable_raw_mode()?;
        let guard = Guard;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Self {
            terminal,
            _guard: guard,
        })
    }

    /// Escape returns to the preceding wizard step. Disabled choices explain why.
    pub fn pick(
        &mut self,
        title: &str,
        subtitle: &str,
        choices: &[Choice],
        initial: Option<&str>,
    ) -> Result<Option<String>> {
        let mut query = String::new();
        let mut selected = initial
            .and_then(|id| choices.iter().position(|c| c.id == id))
            .unwrap_or(0);
        let mut state = ListState::default();
        let mut notice = String::new();
        loop {
            let filtered = filter_choices(choices, &query);
            selected = selected.min(filtered.len().saturating_sub(1));
            state.select((!filtered.is_empty()).then_some(selected));
            self.terminal.draw(|frame| {
                draw_picker(
                    frame, title, subtitle, &query, &filtered, &mut state, &notice,
                )
            })?;
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        bail!("setup cancelled");
                    }
                    match key.code {
                        KeyCode::Esc => return Ok(None),
                        KeyCode::Up => selected = selected.saturating_sub(1),
                        KeyCode::Down => {
                            selected = (selected + 1).min(filtered.len().saturating_sub(1))
                        }
                        KeyCode::PageUp => selected = selected.saturating_sub(10),
                        KeyCode::PageDown => {
                            selected = (selected + 10).min(filtered.len().saturating_sub(1))
                        }
                        KeyCode::Home => selected = 0,
                        KeyCode::End => selected = filtered.len().saturating_sub(1),
                        KeyCode::Enter => {
                            if let Some(choice) = filtered.get(selected) {
                                if let Some(reason) = &choice.disabled_reason {
                                    notice.clone_from(reason);
                                } else {
                                    return Ok(Some(choice.id.clone()));
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            query.pop();
                            selected = 0;
                            notice.clear();
                        }
                        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                            query.clear();
                            selected = 0;
                        }
                        KeyCode::Char(c) if printable(key.modifiers) => {
                            query.push(c);
                            selected = 0;
                            notice.clear();
                        }
                        _ => {}
                    }
                }
                Event::Paste(text) => {
                    query.push_str(&text.replace(['\n', '\r'], " "));
                    selected = 0;
                }
                _ => {}
            }
        }
    }

    /// Input stays in memory; credential persistence is a separate explicit action.
    pub fn input(
        &mut self,
        title: &str,
        help: &str,
        initial: &str,
        secret: bool,
        multiline: bool,
    ) -> Result<Option<String>> {
        let mut value = initial.to_owned();
        loop {
            self.terminal.draw(|frame| {
                let rows = shell(frame, title, help);
                let display = if secret {
                    "•".repeat(value.chars().count())
                } else {
                    value.clone()
                };
                let inner_width = rows[2].width.saturating_sub(4).max(1);
                let paragraph = Paragraph::new(format!("{display}▏"))
                    .wrap(Wrap { trim: false })
                    .block(panel(if secret {
                        " API key · hidden "
                    } else {
                        " Type here "
                    }));
                let scroll = paragraph
                    .line_count(inner_width)
                    .saturating_sub(usize::from(rows[2].height.saturating_sub(2)))
                    .min(usize::from(u16::MAX)) as u16;
                frame.render_widget(paragraph.scroll((scroll, 0)), rows[2]);
                frame.render_widget(
                    Paragraph::new(if multiline {
                        " Enter continue   Shift+Enter / Ctrl+J newline   Esc back   Ctrl+U clear "
                    } else {
                        " Enter continue   Esc back   Ctrl+U clear   Ctrl+C cancel "
                    })
                    .style(Style::default().fg(MUTED)),
                    rows[3],
                );
            })?;
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
                        bail!("setup cancelled");
                    }
                    match key.code {
                        KeyCode::Esc => return Ok(None),
                        KeyCode::Enter
                            if multiline && key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            value.push('\n')
                        }
                        KeyCode::Char('j')
                            if multiline && key.modifiers == KeyModifiers::CONTROL =>
                        {
                            value.push('\n')
                        }
                        KeyCode::Enter => return Ok(Some(value)),
                        KeyCode::Backspace => {
                            value.pop();
                        }
                        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                            value.clear()
                        }
                        KeyCode::Char(c) if printable(key.modifiers) => value.push(c),
                        _ => {}
                    }
                }
                Event::Paste(text) => value.push_str(if multiline { &text } else { text.trim() }),
                _ => {}
            }
        }
    }

    /// Keep device login cancellable while the async grant is being polled.
    pub async fn wait_for_login<F>(
        &mut self,
        verification_uri: &str,
        user_code: &str,
        wait: F,
    ) -> Result<Option<serde_json::Value>>
    where
        F: std::future::Future<Output = Result<serde_json::Value>>,
    {
        tokio::pin!(wait);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            self.terminal.draw(|frame| {
                let rows = shell(frame, "Connect your account", "Open the link in your browser and enter the code. Your terminal waits for confirmation.");
                frame.render_widget(Paragraph::new(vec![
                    Line::styled(verification_uri.to_owned(), Style::default().fg(ACCENT)),
                    Line::raw(""),
                    Line::styled(format!("Sign-in code: {user_code}"), Style::default().fg(SEA).add_modifier(Modifier::BOLD)),
                    Line::raw(""), Line::styled("Waiting for sign-in…", Style::default().fg(MUTED)),
                ]).wrap(Wrap { trim:false }).block(panel(" Browser sign-in ")), rows[2]);
                frame.render_widget(Paragraph::new(" Esc / Ctrl+C cancel sign-in ").style(Style::default().fg(MUTED)), rows[3]);
            })?;
            tokio::select! {
                result = &mut wait => return result.map(Some),
                _ = tick.tick() => if event::poll(std::time::Duration::ZERO)? {
                    if let Event::Key(key) = event::read()? {
                        if key.kind != KeyEventKind::Release && (key.code == KeyCode::Esc || (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)) { return Ok(None); }
                    }
                },
            }
        }
    }
}

fn printable(modifiers: KeyModifiers) -> bool {
    !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        || modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn filter_choices<'a>(choices: &'a [Choice], query: &str) -> Vec<&'a Choice> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    choices
        .iter()
        .filter(|choice| {
            let haystack =
                format!("{} {} {}", choice.id, choice.name, choice.detail).to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(MUTED))
}

fn shell(
    frame: &mut Frame<'_>,
    title: &str,
    subtitle: &str,
) -> std::rc::Rc<[ratatui::layout::Rect]> {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(INK).fg(PAPER)),
        area,
    );
    let heading_height = (subtitle.lines().count() as u16 + 2).clamp(4, 8);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(heading_height),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .margin(if area.width > 60 { 2 } else { 0 })
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if area.width >= 80 {
                    " openraid by vuln.industries "
                } else {
                    " openraid "
                },
                Style::default().fg(SEA).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / launch a swarm", Style::default().fg(MUTED)),
        ])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                title.to_owned(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Line::raw(subtitle.to_owned()),
        ])
        .wrap(Wrap { trim: false }),
        rows[1],
    );
    rows
}

fn draw_picker(
    frame: &mut Frame<'_>,
    title: &str,
    subtitle: &str,
    query: &str,
    choices: &[&Choice],
    state: &mut ListState,
    notice: &str,
) {
    let rows = shell(frame, title, subtitle);
    let body = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(rows[2]);
    frame.render_widget(
        Paragraph::new(format!(" {query}▏"))
            .block(panel(&format!(" Search · {} matches ", choices.len()))),
        body[0],
    );
    if choices.is_empty() {
        frame.render_widget(
            Paragraph::new(" No matches. Backspace edits your search; Ctrl+U clears it.")
                .style(Style::default().fg(AMBER)),
            body[1],
        );
    } else {
        let items = choices.iter().map(|choice| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        choice.name.clone(),
                        Style::default().fg(if choice.disabled_reason.is_some() {
                            MUTED
                        } else {
                            PAPER
                        }),
                    ),
                    Span::styled(format!("  {}", choice.id), Style::default().fg(MUTED)),
                ]),
                Line::styled(
                    format!(
                        "  {}",
                        choice.disabled_reason.as_deref().unwrap_or(&choice.detail)
                    ),
                    Style::default().fg(MUTED),
                ),
            ])
        });
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(48, 66, 87))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› "),
            body[1],
            state,
        );
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                " Type to search   ↑/↓ select   Enter continue   Esc back   Ctrl+C cancel",
                Style::default().fg(SEA),
            ),
            Line::styled(notice.to_owned(), Style::default().fg(AMBER)),
        ]),
        rows[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn search_matches_provider_model_metadata_and_unicode() {
        let choices = vec![
            Choice::new("codex-lb", "Custom Codex", "local Responses"),
            Choice::new("google", "Gemini", "Türkiye"),
        ];
        assert_eq!(
            filter_choices(&choices, "CODEX responses")[0].id,
            "codex-lb"
        );
        assert_eq!(filter_choices(&choices, "türkiye")[0].id, "google");
        assert!(filter_choices(&choices, "missing").is_empty());
    }

    #[test]
    fn picker_handles_small_terminals_and_empty_results() {
        for (width, height) in [(120, 36), (60, 18), (24, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = ListState::default();
            terminal
                .draw(|frame| {
                    draw_picker(
                        frame,
                        "Choose a provider",
                        "All providers, searchable",
                        "unknown",
                        &[],
                        &mut state,
                        "",
                    )
                })
                .unwrap();
        }
    }
}
