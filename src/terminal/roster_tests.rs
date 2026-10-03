use super::*;
use ratatui::{backend::TestBackend, Terminal};

fn rendered_sidebar(app: &mut UiState, snapshot: &MetricsSnapshot) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| draw_agents(frame, frame.area(), app, snapshot))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn removing_an_active_agent_updates_sidebar_before_worker_finishes() {
    let metrics = Metrics::new(3);
    metrics.set_status("agent-001", AgentStatus::Thinking);
    metrics.record_usage("agent-001", 30, 12, 4);
    metrics.record_tool("agent-001");
    metrics.append_activity("agent-001", "retired worker history");
    let mut app = UiState::default();
    app.reconcile_roster(&metrics.snapshot());
    assert!(rendered_sidebar(&mut app, &metrics.snapshot()).contains("agent-001"));

    let members = vec!["agent-002".to_owned(), "agent-003".to_owned()];
    let visible = metrics.snapshot_for_members(&members);
    app.reconcile_roster(&visible);
    let rendered = rendered_sidebar(&mut app, &visible);
    assert!(!rendered.contains("agent-001"));
    assert!(rendered.contains("agent-002"));
    assert!(rendered.contains("agent-003"));
    assert_eq!(visible.active, 0);
    assert_eq!(visible.input_tokens, 30);
    assert_eq!(visible.output_tokens, 12);
    assert_eq!(visible.cached_tokens, 4);
    assert_eq!(visible.tools, 1);
    assert_eq!(metrics.snapshot().active, 1);
    assert!(metrics
        .agent_activity("agent-001")
        .contains("retired worker history"));
}

#[test]
fn removing_an_earlier_row_preserves_the_inspected_agent_and_scroll() {
    let metrics = Metrics::new(4);
    let mut app = UiState::default();
    app.reconcile_roster(&metrics.snapshot());
    app.selected = 2;
    app.inspecting = true;
    app.detail_scroll = 7;
    app.detail_follow = false;
    app.inspection_scroll = 23;
    app.inspection_follow = false;

    let members = vec![
        "agent-002".to_owned(),
        "agent-003".to_owned(),
        "agent-004".to_owned(),
    ];
    let visible = metrics.snapshot_for_members(&members);
    app.reconcile_roster(&visible);
    assert_eq!(visible.agents[app.selected].id, "agent-003");
    assert_eq!(app.table.selected(), Some(1));
    assert!(app.inspecting);
    assert_eq!(app.detail_scroll, 7);
    assert!(!app.detail_follow);
    assert_eq!(app.inspection_scroll, 23);
    assert!(!app.inspection_follow);
}

#[test]
fn removing_the_inspected_agent_resets_history_for_its_replacement() {
    let metrics = Metrics::new(3);
    let mut app = UiState::default();
    app.reconcile_roster(&metrics.snapshot());
    app.selected = 1;
    app.inspecting = true;
    app.detail_scroll = 9;
    app.detail_follow = false;
    app.inspection_scroll = 70;
    app.inspection_follow = false;

    let visible = metrics.snapshot_for_members(&["agent-001".to_owned(), "agent-003".to_owned()]);
    app.reconcile_roster(&visible);
    assert_eq!(visible.agents[app.selected].id, "agent-003");
    assert_eq!(app.table.selected(), Some(app.selected));
    assert_eq!(app.detail_scroll, 0);
    assert!(app.detail_follow);
    assert_eq!(app.inspection_scroll, 0);
    assert!(app.inspection_follow);
}

#[test]
fn empty_visible_roster_clears_selection_and_accepts_a_later_agent() {
    let metrics = Metrics::new(1);
    let mut app = UiState::default();
    app.reconcile_roster(&metrics.snapshot());
    let empty = metrics.snapshot_for_members(&[]);
    app.reconcile_roster(&empty);
    assert_eq!(app.table.selected(), None);
    assert!(!rendered_sidebar(&mut app, &empty).contains("agent-001"));

    metrics.add_agent("agent-002");
    let visible = metrics.snapshot_for_members(&["agent-002".to_owned()]);
    app.reconcile_roster(&visible);
    assert_eq!(app.selected, 0);
    assert_eq!(app.table.selected(), Some(0));
    assert!(rendered_sidebar(&mut app, &visible).contains("agent-002"));
}
