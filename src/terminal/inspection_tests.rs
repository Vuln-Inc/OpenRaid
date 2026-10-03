use super::*;
use ratatui::backend::TestBackend;

fn render(app: &mut UiState, metrics: &Metrics, width: u16, height: u16) -> String {
    buffer_text(&render_buffer(app, metrics, width, height), width)
}

fn render_buffer(
    app: &mut UiState,
    metrics: &Metrics,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, app, &metrics.snapshot(), metrics))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &ratatui::buffer::Buffer, width: u16) -> String {
    buffer
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_cell<'a>(buffer: &'a ratatui::buffer::Buffer, text: &str) -> &'a ratatui::buffer::Cell {
    &buffer
        .content()
        .windows(text.len())
        .find(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == text)
        .unwrap_or_else(|| panic!("missing rendered text: {text}"))[0]
}

async fn press(app: &mut UiState, store: &Store, code: KeyCode, agents: usize) -> bool {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), store, agents)
        .await
        .unwrap()
}

#[tokio::test]
async fn enter_opens_selected_agent_and_escape_or_q_restores_dashboard() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection.sqlite")).await?;
    for grid in [false, true] {
        for focus in [Focus::Agents, Focus::Detail] {
            for close in [KeyCode::Esc, KeyCode::Char('q')] {
                let mut app = UiState {
                    grid,
                    focus,
                    selected: 37,
                    after: 123,
                    follow: false,
                    board_scroll: 9,
                    ..UiState::default()
                };
                assert!(!press(&mut app, &store, KeyCode::Enter, 50).await);
                assert!(app.inspecting);
                assert_eq!(app.selected, 37);
                assert!(!press(&mut app, &store, close, 50).await);
                assert!(!app.inspecting);
                assert_eq!(app.grid, grid);
                assert!(app.focus == focus);
                assert_eq!(app.selected, 37);
                assert_eq!(app.after, 123);
                assert!(!app.follow);
                assert_eq!(app.board_scroll, 9);
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn history_navigation_does_not_page_board_or_change_grid_agent() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection-scroll.sqlite")).await?;
    let mut app = UiState {
        grid: true,
        selected: 25,
        after: 100,
        board_scroll: 6,
        follow: false,
        ..UiState::default()
    };
    app.open_inspection(50);
    app.inspection_follow = false;
    app.inspection_scroll = 100;
    app.inspection_page_size = 20;

    press(&mut app, &store, KeyCode::PageUp, 50).await;
    assert_eq!(app.inspection_scroll, 80);
    press(&mut app, &store, KeyCode::PageDown, 50).await;
    assert_eq!(app.inspection_scroll, 100);
    press(&mut app, &store, KeyCode::Up, 50).await;
    assert!(app.inspection_scroll < 100);
    assert!(!app.inspection_follow);
    press(&mut app, &store, KeyCode::Home, 50).await;
    assert_eq!(app.inspection_scroll, 0);
    assert!(!app.inspection_follow);
    press(&mut app, &store, KeyCode::PageUp, 50).await;
    assert_eq!(app.inspection_scroll, 0);
    press(&mut app, &store, KeyCode::End, 50).await;
    assert!(app.inspection_follow);
    press(&mut app, &store, KeyCode::Char('f'), 50).await;
    assert!(!app.inspection_follow);
    press(&mut app, &store, KeyCode::Char('f'), 50).await;
    assert!(app.inspection_follow);

    assert_eq!(app.selected, 25);
    assert!(app.grid);
    assert_eq!(app.after, 100);
    assert_eq!(app.board_scroll, 6);
    assert!(!app.follow);
    assert!(store.read_board(0, 100).await?.is_empty());
    Ok(())
}

#[test]
fn inspector_renders_early_history_beyond_dashboard_preview_and_isolates_agents() {
    let metrics = Metrics::new(50);
    metrics.append_output("agent-038", "EARLY_SELECTED_HISTORY\n");
    metrics.append_output("agent-038", &"middle history line\n".repeat(2_000));
    metrics.append_output("agent-038", "LATEST_SELECTED_HISTORY\n");
    metrics.append_output("agent-001", "OTHER_AGENT_PRIVATE_MARKER\n");
    assert!(!metrics
        .agent_output("agent-038")
        .contains("EARLY_SELECTED_HISTORY"));
    let mut app = UiState {
        selected: 37,
        ..UiState::default()
    };
    app.open_inspection(50);
    app.inspection_follow = false;
    app.inspection_scroll = 0;
    let top = render(&mut app, &metrics, 100, 24);
    assert!(top.contains("agent-038"));
    assert!(top.contains("EARLY_SELECTED_HISTORY"));
    assert!(!top.contains("OTHER_AGENT_PRIVATE_MARKER"));

    app.inspection_follow = true;
    let bottom = render(&mut app, &metrics, 100, 24);
    assert!(bottom.contains("LATEST_SELECTED_HISTORY"));
    assert!(!bottom.contains("OTHER_AGENT_PRIVATE_MARKER"));
}

#[test]
fn inspector_can_follow_history_beyond_u16_paragraph_scroll_limit() {
    let metrics = Metrics::new(1);
    metrics.append_output("agent-001", &"historical row\n".repeat(70_000));
    metrics.append_output("agent-001", "AFTER_SEVENTY_THOUSAND_ROWS\n");
    let mut app = UiState::default();
    app.open_inspection(1);
    let output = render(&mut app, &metrics, 100, 24);
    assert!(app.inspection_scroll > usize::from(u16::MAX));
    assert!(output.contains("AFTER_SEVENTY_THOUSAND_ROWS"));
}

#[test]
fn inspection_handles_unicode_and_terminal_resizing_without_losing_follow() {
    let metrics = Metrics::new(1);
    metrics.append_output(
        "agent-001",
        &"界 café শেষ 🦀 long streamed output\n".repeat(300),
    );
    metrics.append_output("agent-001", "LATEST_UTF8\n");
    let mut app = UiState::default();
    app.open_inspection(1);
    for (width, height) in [(1, 1), (8, 3), (24, 8), (60, 18), (100, 24)] {
        let output = render(&mut app, &metrics, width, height);
        assert!(app.inspection_follow);
        if width >= 60 && height >= 18 {
            assert!(output.contains("LATEST_UTF8"));
        }
    }
}

#[tokio::test]
async fn empty_roster_does_not_open_inspection_or_detach_console() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection-empty.sqlite")).await?;
    let mut app = UiState::default();
    assert!(!press(&mut app, &store, KeyCode::Enter, 0).await);
    assert!(!app.inspecting);
    render(&mut app, &Metrics::new(0), 80, 24);
    Ok(())
}

#[tokio::test]
async fn stop_shortcut_remains_available_while_inspecting() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection-stop.sqlite")).await?;
    let mut app = UiState {
        managed: true,
        ..UiState::default()
    };
    app.open_inspection(10);
    app.key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &store,
        10,
    )
    .await?;
    assert!(!press(&mut app, &store, KeyCode::Char('x'), 10).await);
    assert!(matches!(app.actions.pop_front(), Some(Action::Stop)));
    assert!(app.inspecting);
    assert!(store.read_board(0, 100).await?.is_empty());
    Ok(())
}

#[test]
fn newly_streamed_output_does_not_move_user_browsing_history() {
    let metrics = Metrics::new(1);
    metrics.append_output("agent-001", "HISTORY_BEING_READ\n");
    metrics.append_output("agent-001", &"older activity\n".repeat(100));
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    app.inspection_scroll = 0;
    assert!(render(&mut app, &metrics, 100, 24).contains("HISTORY_BEING_READ"));

    metrics.append_output("agent-001", "ARRIVED_WHILE_BROWSING\n");
    let browsing = render(&mut app, &metrics, 100, 24);
    assert_eq!(app.inspection_scroll, 0);
    assert!(!app.inspection_follow);
    assert!(browsing.contains("HISTORY_BEING_READ"));
    assert!(!browsing.contains("ARRIVED_WHILE_BROWSING"));

    app.inspection_follow = true;
    assert!(render(&mut app, &metrics, 100, 24).contains("ARRIVED_WHILE_BROWSING"));
}

#[test]
fn inspector_displays_tool_arguments_and_process_output_before_tool_completes() {
    let metrics = Metrics::new(1);
    metrics.record_tool_start("agent-001", "run_command", r#"{"command":"LIVE_COMMAND"}"#);
    metrics.append_activity("agent-001", "LIVE_STDOUT_CHUNK\n");
    let mut app = UiState::default();
    app.open_inspection(1);
    let active = render(&mut app, &metrics, 100, 24);
    assert!(active.contains("run_command"));
    assert!(active.contains("LIVE_COMMAND"));
    assert!(active.contains("LIVE_STDOUT_CHUNK"));
    assert!(metrics.agent_output("agent-001").is_empty());

    metrics.record_tool_result("agent-001", "run_command", "COMPLETED_TOOL_RESULT");
    let completed = render(&mut app, &metrics, 100, 24);
    assert!(completed.contains("COMPLETED_TOOL_RESULT"));
}

#[tokio::test]
async fn explicit_grid_shortcut_leaves_inspection_for_requested_dashboard() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection-grid.sqlite")).await?;
    let mut app = UiState::default();
    app.open_inspection(10);
    assert!(!press(&mut app, &store, KeyCode::F(6), 10).await);
    assert!(!app.inspecting);
    assert!(app.grid);
    assert!(app.focus == Focus::Agents);
    Ok(())
}

#[tokio::test]
async fn inspector_palette_action_opens_from_board_and_keeps_composer_draft() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("inspection-palette.sqlite")).await?;
    for composing in [false, true] {
        let mut app = UiState {
            focus: Focus::Board,
            composing,
            draft: "preserve this owner draft".into(),
            selected: 6,
            ..UiState::default()
        };
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &store,
            10,
        )
        .await?;
        assert!(app.palette);
        app.palette_selected = COMMANDS
            .iter()
            .position(|(name, _, _)| *name == "Inspect selected agent activity")
            .expect("agent inspector must be discoverable in command palette");
        assert!(!press(&mut app, &store, KeyCode::Enter, 10).await);
        assert!(app.inspecting);
        assert!(!app.palette);
        assert!(!app.composing);
        assert_eq!(app.selected, 6);
        assert_eq!(app.draft, "preserve this owner draft");
        press(&mut app, &store, KeyCode::Esc, 10).await;
        assert!(!app.inspecting);
        assert!(app.focus == Focus::Board);
    }
    Ok(())
}

#[test]
fn inspector_distinguishes_response_tool_and_result_with_readable_theme_styles() {
    let metrics = Metrics::new(1);
    metrics.append_output("agent-001", "RESPONSE_PAYLOAD\n");
    metrics.record_tool_start("agent-001", "read_file", "ARGUMENT_PAYLOAD");
    metrics.record_tool_result("agent-001", "read_file", "RESULT_PAYLOAD");
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    let buffer = render_buffer(&mut app, &metrics, 120, 32);
    let palette = crate::theme::current_palette();

    for label in ["Response", "Tool run", "Tool result"] {
        let cell = text_cell(&buffer, label);
        assert!(cell.modifier.contains(Modifier::BOLD), "{label}");
        assert_ne!(cell.fg, palette.muted, "{label}");
    }
    for payload in ["RESPONSE_PAYLOAD", "ARGUMENT_PAYLOAD", "RESULT_PAYLOAD"] {
        let cell = text_cell(&buffer, payload);
        assert_eq!(cell.fg, palette.text, "{payload}");
        assert!(!cell.modifier.contains(Modifier::BOLD), "{payload}");
    }
    assert_eq!(text_cell(&buffer, "ARGUMENT_PAYLOAD").bg, palette.surface);
    assert_eq!(text_cell(&buffer, "RESULT_PAYLOAD").bg, palette.surface);
    assert_eq!(
        text_cell(&buffer, "RESPONSE_PAYLOAD").bg,
        palette.background
    );
    let output = buffer_text(&buffer, 120);
    assert!(output.contains("read_file"));
    assert!(output.contains("Following") || output.contains("History"));
    assert!(output.contains("Esc"));
}

#[test]
fn inspector_keeps_code_fenced_header_like_payload_as_content() {
    let metrics = Metrics::new(1);
    metrics.record_tool_start("agent-001", "read_file", "SOURCE_ARGUMENTS");
    metrics.record_tool_result(
        "agent-001",
        "read_file",
        "```text\n\n[tool] literal_marker\n\n[result] literal_result\n```\nRESULT_END",
    );
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    let buffer = render_buffer(&mut app, &metrics, 120, 32);
    let output = buffer_text(&buffer, 120);
    for payload in [
        "SOURCE_ARGUMENTS",
        "literal_marker",
        "literal_result",
        "RESULT_END",
    ] {
        assert!(output.contains(payload), "{payload}");
        assert_eq!(
            text_cell(&buffer, payload).fg,
            crate::theme::current_palette().text
        );
        assert!(!text_cell(&buffer, payload)
            .modifier
            .contains(Modifier::BOLD));
    }
    assert_eq!(output.matches("Tool run").count(), 1);
    assert_eq!(output.matches("Tool result").count(), 1);
}

#[test]
fn inspector_responsive_layout_keeps_transcript_and_history_state_readable() {
    let metrics = Metrics::new(1);
    metrics.record_usage("agent-001", 1234, 567, 89);
    metrics.append_output("agent-001", "RESPONSIVE_TRANSCRIPT\n");
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    for (width, height) in [(60, 18), (99, 24), (100, 16), (120, 32)] {
        let output = render(&mut app, &metrics, width, height);
        assert!(
            output.contains("RESPONSIVE_TRANSCRIPT"),
            "{width}x{height}\n{output}"
        );
        assert!(output.contains("agent-001"), "{width}x{height}");
        assert!(output.contains("History"), "{width}x{height}\n{output}");
        assert!(output.contains("Esc"), "{width}x{height}");
        assert!(!app.inspection_follow);
        assert_eq!(app.inspection_scroll, 0);
        if width >= 100 && height >= 16 {
            for section in ["Session", "Usage", "Activity", "Tools", "Retries"] {
                assert!(
                    output.contains(section),
                    "{section}: {width}x{height}\n{output}"
                );
            }
        }
    }
}

#[test]
fn inspector_formats_real_activity_markers_after_unterminated_streams() {
    let metrics = Metrics::new(1);
    metrics.append_output("agent-001", "UNTERMINATED_RESPONSE");
    metrics.set_detail("agent-001", "STATUS_PAYLOAD");
    metrics.append_output("agent-001", "SECOND_UNTERMINATED_RESPONSE");
    metrics.record_tool_start("agent-001", "run_command", "TOOL_ARGUMENTS");
    metrics.append_activity("agent-001", "UNTERMINATED_PROCESS_OUTPUT");
    metrics.record_tool_result("agent-001", "run_command", "TOOL_COMPLETED");
    metrics.append_output("agent-001", "POST_TOOL_GENERATION_");
    metrics.append_output("agent-001", "COMPLETED");
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    let buffer = render_buffer(&mut app, &metrics, 120, 40);
    let output = buffer_text(&buffer, 120);

    for label in ["Response", "Status", "Tool run", "Tool result"] {
        assert!(output.contains(label), "{label}\n{output}");
    }
    for raw_marker in ["[status]", "[tool]", "[result]", "[response]"] {
        assert!(!output.contains(raw_marker), "{raw_marker}\n{output}");
    }
    for payload in [
        "UNTERMINATED_RESPONSE",
        "STATUS_PAYLOAD",
        "SECOND_UNTERMINATED_RESPONSE",
        "TOOL_ARGUMENTS",
        "UNTERMINATED_PROCESS_OUTPUT",
        "TOOL_COMPLETED",
        "POST_TOOL_GENERATION_COMPLETED",
    ] {
        assert!(output.contains(payload), "{payload}\n{output}");
    }
    assert_eq!(
        text_cell(&buffer, "Tool run").fg,
        crate::theme::current_palette().accent
    );
    assert_eq!(
        text_cell(&buffer, "Tool result").fg,
        crate::theme::current_palette().success
    );
    assert_eq!(output.matches("Response").count(), 3, "{output}");
    let resumed = text_cell(&buffer, "POST_TOOL_GENERATION_COMPLETED");
    assert_eq!(resumed.fg, crate::theme::current_palette().text);
    assert_eq!(resumed.bg, crate::theme::current_palette().background);
    assert!(!resumed.modifier.contains(Modifier::BOLD));
}
