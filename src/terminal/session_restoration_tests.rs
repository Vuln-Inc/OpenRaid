use super::*;
use ratatui::backend::TestBackend;

#[tokio::test]
async fn reopened_console_restores_objective_outside_live_board_page() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("session.sqlite3");
    let objective = "Restore the complete saved objective İ\nincluding its second line";
    {
        let store = Store::open(&database).await?;
        store.append("owner", objective, true).await?;
        for index in 0..150 {
            store
                .append("agent-001", format!("historical progress {index}"), false)
                .await?;
        }
        store
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
    }
    let store = Store::open(&database).await?;
    let mut app = UiState {
        managed: true,
        composing: true,
        draft: "keep the unsent draft".into(),
        ..UiState::default()
    };
    app.load(&store).await?;
    assert_eq!(app.session.objective, objective);
    assert_eq!(app.draft, "keep the unsent draft");
    assert!(app.board.iter().all(|message| message.sender != "owner"));
    assert_eq!(store.latest_seq().await?, 152);

    let metrics = Metrics::new(1);
    let mut terminal = Terminal::new(TestBackend::new(140, 32))?;
    terminal.draw(|frame| draw(frame, &mut app, &metrics.snapshot(), &metrics))?;
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Objective: Restore the complete saved objective İ"));
    Ok(())
}

#[tokio::test]
async fn objective_refresh_ignores_controls_and_historical_board_cursor() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("session.sqlite3")).await?;
    store.append("owner", "first objective", true).await?;
    let mut app = UiState::default();
    app.load(&store).await?;
    assert_eq!(app.session.objective, "first objective");

    store.append("owner", "latest objective", true).await?;
    store
        .append("owner-control", "Owner paused current work.", true)
        .await?;
    app.follow = false;
    app.after = 0;
    app.load(&store).await?;
    assert_eq!(app.session.objective, "latest objective");
    assert_eq!(app.after, 0);
    assert!(!app.follow);
    Ok(())
}

#[tokio::test]
async fn empty_session_keeps_explicit_launch_objective() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("session.sqlite3")).await?;
    let mut app = UiState {
        session: SessionInfo {
            objective: "new objective awaiting admission".into(),
            ..SessionInfo::default()
        },
        ..UiState::default()
    };
    app.load(&store).await?;
    assert_eq!(app.session.objective, "new objective awaiting admission");
    assert_eq!(store.latest_seq().await?, 0);
    Ok(())
}

#[tokio::test]
async fn cleared_history_removes_restored_objective() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("session.sqlite3")).await?;
    store.append("owner", "objective to remove", true).await?;
    let mut app = UiState::default();
    app.load(&store).await?;
    assert_eq!(app.session.objective, "objective to remove");

    store.clear_board().await?;
    app.load(&store).await?;
    assert!(app.session.objective.is_empty());
    assert!(app.board.is_empty());
    Ok(())
}
