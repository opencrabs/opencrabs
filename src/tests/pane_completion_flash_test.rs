//! The completion pulse on the pane that finished (#2593).
//!
//! The chime says "a task settled". It cannot say WHICH panel settled it, and
//! in split view the window bar that used to carry that context is gone. So the
//! pane that owns the settled session pulses its ring in amber, slowly, and in
//! a colour the focused pane never uses. These tests pin the phase arithmetic
//! and then prove the pulse lands on the right pane in a rendered buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::pane::{PaneManager, SplitDirection};
use crate::tui::render::panes::completion_flash_role;
use crate::tui::render::render;
use crate::tui::render::theme::{self, Role};

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let app_value = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let app_value = App::new(service, context);
    let mut app = app_value;
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

fn find_in_buffer(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in 0..buffer.area.height {
        let text = row_text(buffer, y);
        if let Some(byte) = text.find(needle) {
            // Count the chars the match sits after rather than slicing at the
            // byte offset: `str::find` returns a char boundary, but the
            // byte-slice ratchet cannot know that, and a blessed exception in
            // a test helper is noise in every review that touches this file.
            let column = text.char_indices().take_while(|(i, _)| *i < byte).count() as u16;
            return Some((column, y));
        }
    }
    None
}

/// Two panes, each bound to its own session.
async fn split_with_sessions(app: &mut App) -> (uuid::Uuid, uuid::Uuid) {
    app.session_service
        .create_session(Some("left-pane".into()))
        .await
        .unwrap();
    app.session_service
        .create_session(Some("right-pane".into()))
        .await
        .unwrap();
    app.load_sessions().await.unwrap();

    app.pane_manager.split(SplitDirection::Horizontal);
    let ids = app.pane_manager.pane_ids_in_order();
    assert_eq!(ids.len(), 2);
    let left = app
        .sessions
        .iter()
        .find(|s| s.title.as_deref() == Some("left-pane"))
        .cloned()
        .expect("left session");
    let right = app
        .sessions
        .iter()
        .find(|s| s.title.as_deref() == Some("right-pane"))
        .cloned()
        .expect("right session");
    app.pane_manager.get_mut(ids[0]).unwrap().session_id = Some(left.id);
    app.pane_manager.get_mut(ids[1]).unwrap().session_id = Some(right.id);
    (left.id, right.id)
}

/// The buffer cell of a pane's top-left ring corner: the ring title is drawn
/// one column right of it.
fn ring_corner(buffer: &Buffer, session_title: &str) -> (u16, u16) {
    let (x, y) = find_in_buffer(buffer, session_title).expect("pane ring title");
    (x.saturating_sub(1), y)
}

#[test]
fn flash_is_a_slow_blink_that_expires() {
    // On for half of each cycle, then gone for good once the window closes.
    assert_eq!(
        completion_flash_role(Duration::from_millis(0)),
        Some(Role::Warning)
    );
    assert_eq!(
        completion_flash_role(Duration::from_millis(700)),
        Some(Role::Warning)
    );
    assert_eq!(completion_flash_role(Duration::from_millis(900)), None);
    assert_eq!(
        completion_flash_role(Duration::from_millis(1700)),
        Some(Role::Warning)
    );
    assert_eq!(completion_flash_role(Duration::from_secs(10)), None);
}

#[tokio::test]
async fn pane_that_finished_pulses_amber_while_the_other_stays_calm() {
    let mut app = app().await;
    let (_, right) = split_with_sessions(&mut app).await;

    // The right pane's plan just settled. The left pane is idle and holds the
    // keyboard (`split` moves focus to the new pane, so put it back).
    app.plan_completion_flash.insert(right, Instant::now());
    let ids = app.pane_manager.pane_ids_in_order();
    app.pane_manager.focused = ids[0];

    let buffer = draw(&mut app, 120, 30);
    let (right_x, right_y) = ring_corner(&buffer, "right-pane");
    assert_eq!(
        buffer[(right_x, right_y)].fg,
        theme::role(Role::Warning),
        "the pane that finished pulses amber"
    );

    let (left_x, left_y) = ring_corner(&buffer, "left-pane");
    assert_eq!(
        buffer[(left_x, left_y)].fg,
        theme::role(Role::Success),
        "the idle focused pane keeps its normal ring"
    );
}

#[tokio::test]
async fn the_pulse_beats_the_focused_pane_colour() {
    let mut app = app().await;
    let (_, right) = split_with_sessions(&mut app).await;
    app.plan_completion_flash.insert(right, Instant::now());

    // Focus the pane that finished: its ring must still say "finished" rather
    // than blending into the focused colour, which is the whole point of the
    // pulse.
    let ids = app.pane_manager.pane_ids_in_order();
    app.pane_manager.focused = ids[1];

    let buffer = draw(&mut app, 120, 30);
    let (x, y) = ring_corner(&buffer, "right-pane");
    assert_eq!(buffer[(x, y)].fg, theme::role(Role::Warning));
}

#[tokio::test]
async fn expired_flash_leaves_no_colour_behind() {
    let mut app = app().await;
    let (_, right) = split_with_sessions(&mut app).await;

    // Older than the announcement window: settled panes must go back to their
    // ordinary ring instead of blinking forever.
    app.plan_completion_flash
        .insert(right, Instant::now() - Duration::from_secs(30));

    let ids = app.pane_manager.pane_ids_in_order();
    app.pane_manager.focused = ids[1];

    let buffer = draw(&mut app, 120, 30);
    let (x, y) = ring_corner(&buffer, "right-pane");
    assert_eq!(buffer[(x, y)].fg, theme::role(Role::Success));
}
