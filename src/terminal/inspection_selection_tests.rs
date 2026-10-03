use super::*;
use ratatui::{backend::TestBackend, layout::Position};

#[test]
fn inspection_drag_copies_first_and_last_transcript_rows_without_header_or_rail() {
    let metrics = Metrics::new(1);
    metrics.append_output(
        "agent-001",
        &(0..20)
            .map(|row| format!("ROW_{row}\n"))
            .collect::<String>(),
    );
    let mut app = UiState::default();
    app.open_inspection(1);
    app.inspection_follow = false;
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))
        .unwrap();
    let pane = app.panels[2];
    let area = app.pane_selection_area(pane);
    assert_eq!(area.y, pane.y);
    assert_eq!(area.bottom(), pane.bottom());
    assert_eq!(area.x, pane.x + 2);
    assert_eq!(area.right(), pane.right() - 1);
    app.selection.start(
        terminal.backend().buffer(),
        area,
        Position::new(area.x, area.y),
    );
    let copied = app
        .selection
        .finish(Position::new(area.right() - 1, area.bottom() - 1))
        .unwrap();
    assert!(copied.starts_with("Response\n"), "{copied}");
    assert!(copied.ends_with("ROW_5"), "{copied}");
    assert!(!copied.contains("agent-001"));
    assert!(!copied.contains('│'));
}

#[test]
fn dashboard_drag_area_still_excludes_all_four_borders() {
    let app = UiState::default();
    assert_eq!(
        app.pane_selection_area(Rect::new(3, 4, 20, 10)),
        Rect::new(4, 5, 18, 8)
    );
}
