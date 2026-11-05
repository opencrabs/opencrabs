//! Running background commands belong to the pane that started them (#2576).
//!
//! The running-task row used to read the window-level session, so in split view
//! one pane's build appeared on another pane's composer and the pane actually
//! waiting on it showed nothing. These are rendered-buffer assertions: a task
//! mirrored for the left pane's session must land in the left pane's columns
//! and must not appear in the right one, and both directions hold at once.
//!
//! The roster is keyed by session, so a task whose session no pane shows is
//! dropped rather than reassigned to whichever pane happens to be focused.
//! Showing another session's command on this pane's border is the bug; guessing
//! an owner would be the same bug with extra steps.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use uuid::Uuid;

use crate::brain::agent::service::{AgentService, MessageEnqueueCallback, QueuedUserMessage};
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::pane::{PaneManager, SplitDirection};
use crate::tui::render::render;

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
    // The background roster lives on the service, which only creates it once an
    // enqueue producer is wired (the real surface always wires one). Without it
    // `background_manager()` is `None` and there is nothing to render.
    let cb: MessageEnqueueCallback = Arc::new(|_sid, _msg: QueuedUserMessage| {});
    let service = Arc::new(
        AgentService::new_for_test(provider, context.clone())
            .await
            .with_message_enqueue_callback(Some(cb)),
    );
    #[cfg(feature = "whatsapp")]
    let app_value = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let app_value = App::new(service, context);
    let mut app = app_value;
    // A fresh app can restore the host's persisted split; every test here
    // starts from one clean pane and builds its own.
    app.pane_manager = PaneManager::new();
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

/// Where `needle` starts in the whole buffer, as (column, row).
fn find_in_buffer(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in 0..buffer.area.height {
        let text = row_text(buffer, y);
        if let Some(byte) = text.find(needle) {
            return Some((text[..byte].chars().count() as u16, y));
        }
    }
    None
}

#[tokio::test]
async fn running_tasks_render_on_the_pane_that_owns_the_session() {
    let mut app = app().await;
    app.pane_manager.split(SplitDirection::Horizontal);
    assert!(app.pane_manager.is_split());

    let ids = app.pane_manager.pane_ids_in_order();
    assert_eq!(ids.len(), 2, "two panes to give a session each");
    let left = Uuid::new_v4();
    let right = Uuid::new_v4();
    app.pane_manager.get_mut(ids[0]).unwrap().session_id = Some(left);
    app.pane_manager.get_mut(ids[1]).unwrap().session_id = Some(right);

    let manager = app
        .agent_service
        .background_manager()
        .expect("background manager")
        .clone();
    manager.mirror_started(left, "cargo build left");
    manager.mirror_started(right, "cargo test right");

    let buffer = draw(&mut app, 120, 30);

    let (left_x, left_y) = find_in_buffer(&buffer, "cargo build left")
        .unwrap_or_else(|| panic!("left pane's own task: {}", dump(&buffer)));
    let (right_x, right_y) = find_in_buffer(&buffer, "cargo test right")
        .unwrap_or_else(|| panic!("right pane's own task: {}", dump(&buffer)));

    assert!(
        left_x < right_x,
        "each task sits in its owner's columns: left {left_x} < right {right_x}"
    );
    assert_eq!(
        left_y, right_y,
        "both panes draw their row on the same line"
    );

    // The failure this guards: one pane's command showing up on the other.
    let left_row = row_text(&buffer, left_y);
    let left_columns: String = left_row.chars().take(right_x as usize).collect();
    assert!(
        !left_columns.contains("cargo test right"),
        "the left pane never borrows the right pane's task: {left_row:?}"
    );
    let right_columns: String = left_row.chars().skip(right_x as usize).collect();
    assert!(
        !right_columns.contains("cargo build left"),
        "the right pane never borrows the left pane's task: {left_row:?}"
    );
}

#[tokio::test]
async fn a_task_from_a_session_no_pane_shows_stays_hidden() {
    let mut app = app().await;
    app.pane_manager.split(SplitDirection::Horizontal);
    let ids = app.pane_manager.pane_ids_in_order();
    let left = Uuid::new_v4();
    app.pane_manager.get_mut(ids[0]).unwrap().session_id = Some(left);

    let manager = app
        .agent_service
        .background_manager()
        .expect("background manager")
        .clone();
    manager.mirror_started(Uuid::new_v4(), "orphan build");

    let buffer = draw(&mut app, 120, 30);
    assert!(
        find_in_buffer(&buffer, "orphan build").is_none(),
        "an unowned task is not guessed onto a pane: {}",
        dump(&buffer)
    );
}

fn dump(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| format!("{y:>2}|{}", row_text(buffer, y)))
        .collect::<Vec<_>>()
        .join("\n")
}
