use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use openraid::{
    theme::THEMES,
    tui_menu::{command, Action, Kind, Menu, Outcome},
};
use ratatui::{backend::TestBackend, Terminal};

fn press(menu: &mut Menu, code: KeyCode) -> Outcome {
    menu.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn render(menu: &Menu, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| menu.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn theme_commands_are_discoverable_and_do_not_become_prompts() {
    assert!(matches!(command("/themes"), Some(Action::Themes)));
    let commands = Menu::commands();
    assert!(commands.entries.iter().any(|entry| entry.id == "/themes"));
    let mut commands = Menu::commands();
    commands.paste("themes");
    assert!(matches!(
        press(&mut commands, KeyCode::Enter),
        Outcome::Action(Action::Themes)
    ));
}

#[test]
fn direct_theme_selection_and_malformed_usage_stay_in_command_handling() {
    for prefix in ["/themes", "/theme"] {
        assert!(matches!(
            command(&format!("{prefix} {}", THEMES[0].id)),
            Some(Action::SelectTheme(id)) if id == THEMES[0].id
        ));
        assert!(matches!(
            command(&format!("{prefix} {} extra", THEMES[0].id)),
            Some(Action::InvalidCommand(_))
        ));
    }
    let mut commands = Menu::commands();
    commands.paste(&format!("themes {}", THEMES[1].id));
    assert!(matches!(
        press(&mut commands, KeyCode::Enter),
        Outcome::Action(Action::SelectTheme(id)) if id == THEMES[1].id
    ));
}

#[test]
fn light_and_dark_searches_limit_results_to_the_requested_appearance() {
    for (query, dark) in [("light", false), ("dark", true)] {
        let mut menu = Menu::themes(THEMES[0].id);
        menu.paste(query);
        let expected = THEMES.iter().filter(|theme| theme.dark == dark).count();
        assert_eq!(menu.filtered().len(), expected);
        for entry in menu.filtered() {
            assert_eq!(
                THEMES
                    .iter()
                    .find(|theme| theme.id == entry.id)
                    .unwrap()
                    .dark,
                dark
            );
        }
    }
}

#[test]
fn filtering_selects_the_visible_theme_instead_of_the_original_row() {
    let mut menu = Menu::themes(THEMES[0].id);
    assert!(matches!(menu.kind, Kind::Themes));
    // Start at the bottom so this also exercises selection reset on search.
    for _ in 0..THEMES.len() {
        press(&mut menu, KeyCode::Down);
    }
    let wanted = THEMES
        .iter()
        .find(|theme| theme.id != THEMES[0].id)
        .unwrap();
    menu.paste(wanted.id);
    assert_eq!(menu.filtered().len(), 1);
    assert!(matches!(
        press(&mut menu, KeyCode::Enter),
        Outcome::Action(Action::SelectTheme(id)) if id == wanted.id
    ));
}

#[test]
fn empty_search_cannot_commit_and_clearing_search_recovers_navigation() {
    let mut menu = Menu::themes(THEMES[0].id);
    menu.paste("there-is-no-theme-with-this-name");
    for code in [
        KeyCode::Down,
        KeyCode::PageDown,
        KeyCode::Up,
        KeyCode::Enter,
    ] {
        assert!(matches!(press(&mut menu, code), Outcome::None));
    }
    assert!(menu.filtered().is_empty());
    menu.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(menu.filtered().len(), THEMES.len());
    assert!(matches!(
        press(&mut menu, KeyCode::Enter),
        Outcome::Action(Action::SelectTheme(_))
    ));
}

#[test]
fn cancelling_after_navigation_never_emits_a_theme_change() {
    let mut menu = Menu::themes(THEMES[0].id);
    assert!(matches!(press(&mut menu, KeyCode::Down), Outcome::None));
    assert!(matches!(press(&mut menu, KeyCode::PageDown), Outcome::None));
    assert!(matches!(press(&mut menu, KeyCode::Esc), Outcome::Close));
}

#[test]
fn picker_starts_on_the_active_theme_and_navigation_stays_in_bounds() {
    for theme in THEMES.iter() {
        let mut menu = Menu::themes(theme.id);
        assert_eq!(menu.filtered()[menu.selected].id, theme.id);
        for _ in 0..THEMES.len() + 2 {
            press(&mut menu, KeyCode::PageDown);
        }
        assert_eq!(menu.selected, menu.filtered().len() - 1);
        for _ in 0..THEMES.len() + 2 {
            press(&mut menu, KeyCode::PageUp);
        }
        assert_eq!(menu.selected, 0);
        press(&mut menu, KeyCode::End);
        assert_eq!(menu.selected, menu.filtered().len() - 1);
        press(&mut menu, KeyCode::Home);
        assert_eq!(menu.selected, 0);
    }
}

#[test]
fn browsing_keeps_the_current_marker_and_renders_the_candidate_palette() {
    let active = &THEMES[0];
    for candidate in THEMES.iter() {
        let mut menu = Menu::themes(active.id);
        menu.selected = menu
            .entries
            .iter()
            .position(|entry| entry.id == candidate.id)
            .unwrap();
        assert_eq!(menu.active_theme.as_deref(), Some(active.id));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| menu.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| {
                cell.symbol() == "●"
                    && cell.fg == candidate.palette.success
                    && cell.bg == candidate.palette.background
            }),
            "{} preview should show its own working-status colors",
            candidate.id
        );
        assert!(render(&menu, 120, 40).contains(&format!("{} ✓", active.name)));
    }
}

#[test]
fn every_preview_survives_narrow_and_short_terminal_sizes() {
    for theme in THEMES.iter() {
        let menu = Menu::themes(theme.id);
        for (width, height) in [(1, 1), (8, 3), (24, 8), (60, 18), (80, 24), (120, 40)] {
            let output = render(&menu, width, height);
            if width >= 80 && height >= 24 {
                assert!(
                    output.contains(theme.name),
                    "{} at {width}x{height}",
                    theme.id
                );
                assert!(
                    output.contains("Prompt › Build something good"),
                    "{} prompt preview clipped at {width}x{height}",
                    theme.id
                );
            }
        }
    }
}
