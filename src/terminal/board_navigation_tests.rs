use super::*;
use ratatui::backend::TestBackend;
use std::collections::BTreeSet;

fn render(app: &mut UiState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| draw_board(frame, frame.area(), app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn press(app: &mut UiState, store: &Store, code: KeyCode) {
    assert!(!app
        .key(KeyEvent::new(code, KeyModifiers::NONE), store, 1)
        .await
        .unwrap());
}

async fn populate(store: &Store, first: usize, last: usize) {
    for number in first..=last {
        store
            .append("agent-001", format!("MESSAGE_{number:03}"), false)
            .await
            .unwrap();
    }
}

fn remember_visible(text: &str, seen: &mut BTreeSet<usize>, count: usize) {
    for number in 1..=count {
        if text.contains(&format!("MESSAGE_{number:03}")) {
            seen.insert(number);
        }
    }
}

#[tokio::test]
async fn arrows_and_vi_keys_visit_every_message_across_both_history_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("board-arrows.sqlite"))
        .await
        .unwrap();
    populate(&store, 1, 250).await;
    let mut app = UiState {
        focus: Focus::Board,
        ..UiState::default()
    };
    app.load(&store).await.unwrap();
    press(&mut app, &store, KeyCode::Home).await;
    let mut forward = BTreeSet::new();
    for step in 0..400 {
        remember_visible(&render(&mut app), &mut forward, 250);
        assert!(app.board.len() <= PAGE_SIZE);
        assert!(!app.follow, "manual scrolling must remain in history mode");
        if forward.len() == 250 {
            break;
        }
        let key = if step % 2 == 0 {
            KeyCode::Down
        } else {
            KeyCode::Char('j')
        };
        press(&mut app, &store, key).await;
    }
    assert_eq!(
        forward.len(),
        250,
        "forward history navigation skipped messages"
    );

    let mut backward = BTreeSet::new();
    for step in 0..400 {
        remember_visible(&render(&mut app), &mut backward, 250);
        assert!(app.board.len() <= PAGE_SIZE);
        if backward.len() == 250 {
            break;
        }
        let key = if step % 2 == 0 {
            KeyCode::Up
        } else {
            KeyCode::Char('k')
        };
        press(&mut app, &store, key).await;
    }
    assert_eq!(
        backward.len(),
        250,
        "reverse history navigation skipped messages"
    );
    assert!(!app.follow);
}

#[tokio::test]
async fn page_down_advances_beyond_first_hundred_and_end_restores_live_tail() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("board-pages.sqlite"))
        .await
        .unwrap();
    populate(&store, 1, 250).await;
    let mut app = UiState {
        focus: Focus::Board,
        ..UiState::default()
    };
    app.load(&store).await.unwrap();
    press(&mut app, &store, KeyCode::Home).await;
    assert_eq!(app.board.first().unwrap().seq, 1);
    press(&mut app, &store, KeyCode::PageDown).await;
    assert_eq!(app.board.first().unwrap().seq, 101);
    assert_eq!(app.board.last().unwrap().seq, 200);
    assert!(!app.follow);
    press(&mut app, &store, KeyCode::PageDown).await;
    assert_eq!(app.board.first().unwrap().seq, 201);
    assert_eq!(app.board.last().unwrap().seq, 250);
    press(&mut app, &store, KeyCode::PageUp).await;
    assert_eq!(app.board.first().unwrap().seq, 101);
    press(&mut app, &store, KeyCode::End).await;
    assert!(app.follow);
    assert!(render(&mut app).contains("MESSAGE_250"));
}

#[tokio::test]
async fn wheel_navigation_shared_path_reaches_new_messages_while_history_is_paused() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("board-wheel.sqlite"))
        .await
        .unwrap();
    populate(&store, 1, 100).await;
    let mut app = UiState {
        focus: Focus::Board,
        ..UiState::default()
    };
    app.load(&store).await.unwrap();
    render(&mut app);
    app.navigate_with_store(-1, 1, &store).await.unwrap();
    let paused_scroll = app.board_scroll;
    assert!(!app.follow);
    populate(&store, 101, 125).await;
    app.load(&store).await.unwrap();
    render(&mut app);
    assert_eq!(app.board_scroll, paused_scroll);
    for _ in 0..100 {
        if render(&mut app).contains("MESSAGE_125") {
            break;
        }
        app.navigate_with_store(1, 1, &store).await.unwrap();
    }
    assert!(render(&mut app).contains("MESSAGE_125"));
    assert!(!app.follow);
    press(&mut app, &store, KeyCode::Char('f')).await;
    assert!(app.follow);
    assert!(render(&mut app).contains("MESSAGE_125"));
}

#[tokio::test]
async fn forward_navigation_refreshes_stale_latest_before_the_next_board_reload() {
    for key in [KeyCode::Down, KeyCode::PageDown] {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("board-stale-latest.sqlite"))
            .await
            .unwrap();
        populate(&store, 1, 100).await;
        let mut app = UiState {
            focus: Focus::Board,
            ..UiState::default()
        };
        app.load(&store).await.unwrap();
        assert!(render(&mut app).contains("MESSAGE_100"));
        press(&mut app, &store, KeyCode::Char('f')).await;
        render(&mut app);
        assert!(!app.follow);

        populate(&store, 101, 101).await;
        assert_eq!(
            app.latest, 100,
            "simulate input arriving before the dirty-board tick"
        );
        assert_eq!(app.board.last().unwrap().seq, 100);
        press(&mut app, &store, key).await;

        assert_eq!(app.latest, 101);
        assert!(
            render(&mut app).contains("MESSAGE_101"),
            "{key:?} must discover the new post"
        );
        assert!(!app.follow);
    }
}

#[tokio::test]
async fn wrapped_message_lines_are_not_skipped_when_crossing_a_page_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("board-wrapped.sqlite"))
        .await
        .unwrap();
    populate(&store, 1, 99).await;
    let body = format!(
        "{}\nLAST_LINE_OF_MESSAGE_100",
        "long wrapped board message text ".repeat(400)
    );
    store.append("agent-001", body, false).await.unwrap();
    populate(&store, 101, 101).await;
    let mut app = UiState {
        focus: Focus::Board,
        ..UiState::default()
    };
    press(&mut app, &store, KeyCode::Home).await;
    let mut saw_last_line = false;
    let mut saw_next_message = false;
    for _ in 0..300 {
        let text = render(&mut app);
        saw_last_line |= text.contains("LAST_LINE_OF_MESSAGE_100");
        if text.contains("MESSAGE_101") {
            saw_next_message = true;
            break;
        }
        press(&mut app, &store, KeyCode::Down).await;
    }
    assert!(
        saw_last_line,
        "the final wrapped rows must be visible before paging"
    );
    assert!(
        saw_next_message,
        "scrolling must continue after the wrapped page"
    );
}

#[tokio::test]
async fn empty_board_navigation_is_stable_and_later_posts_are_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("board-empty.sqlite"))
        .await
        .unwrap();
    let mut app = UiState {
        focus: Focus::Board,
        ..UiState::default()
    };
    app.load(&store).await.unwrap();
    for key in [
        KeyCode::Home,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
    ] {
        render(&mut app);
        press(&mut app, &store, key).await;
        assert!(app.board.is_empty());
    }
    populate(&store, 1, 1).await;
    press(&mut app, &store, KeyCode::End).await;
    assert!(render(&mut app).contains("MESSAGE_001"));
}
