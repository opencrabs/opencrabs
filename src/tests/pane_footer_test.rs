//! The compact pane footer (#2575).
//!
//! In split view the window status bar used to span the whole width while the
//! panes carried the sessions, so the row describing model and policy belonged
//! to no pane in particular. These are rendered-buffer assertions, not source
//! scans: they prove every pane draws its own one-row footer at its own bottom
//! edge, that the unfocused footer is dim, and that a single pane keeps the
//! full-width bar with its policy chip.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::pane::{PaneManager, SplitDirection};
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
    // `App::new` restores the host's persisted pane layout, so a fresh app can
    // already carry a split. Every test here starts from one clean pane.
    app.pane_manager = PaneManager::new();
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// The bottom row of the window as text.
fn bottom_row(buffer: &Buffer) -> String {
    let y = buffer.area.height.saturating_sub(1);
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

/// The text of one buffer row.
fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

/// Where `needle` starts in the whole buffer, as (column, row). The footer is
/// the last row INSIDE a pane's border, so it is one row above the window's
/// bottom edge in split view and not the bottom row itself.
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
async fn pane_footer_every_pane_carries_its_own_row_when_split() {
    let mut app = app().await;
    app.pane_manager.split(SplitDirection::Horizontal);
    assert!(app.pane_manager.is_split());

    let buffer = draw(&mut app, 100, 30);

    let (first_x, first_y) = find_in_buffer(&buffer, "[1/2]").expect("first pane footer");
    let (second_x, second_y) = find_in_buffer(&buffer, "[2/2]").expect("second pane footer");
    assert_eq!(
        first_y, second_y,
        "both panes draw their footer on the same row"
    );
    assert!(
        first_y < buffer.area.height - 1,
        "the footer sits inside the panel, not on the window edge: row {first_y}"
    );

    let row = row_text(&buffer, first_y);
    assert!(
        first_x < second_x,
        "each pane owns the footer in its own columns: {first_x} < {second_x}"
    );
    assert_eq!(
        row.matches("approve").count()
            + row.matches("yolo").count()
            + row.matches("auto (session)").count(),
        2,
        "each pane's footer carries the approval chip, not one shared bar: {row:?}"
    );
}

#[tokio::test]
async fn pane_footer_unfocused_pane_draws_dim() {
    let mut app = app().await;
    app.pane_manager.split(SplitDirection::Horizontal);

    let buffer = draw(&mut app, 100, 30);
    let (first, y) = find_in_buffer(&buffer, "[1/2]").expect("first pane footer");
    let (second, _) = find_in_buffer(&buffer, "[2/2]").expect("second pane footer");

    let first_fg = buffer[(first, y)].fg;
    let second_fg = buffer[(second, y)].fg;

    // Exactly one footer reads at full strength (the focused pane) and the
    // other drops to the inactive colour, so the pane holding the keyboard is
    // unambiguous at a glance.
    assert_eq!(
        [first_fg, second_fg]
            .iter()
            .filter(|fg| **fg == theme::role(Role::Success))
            .count(),
        1,
        "one footer is focused: {first_fg:?} vs {second_fg:?}"
    );
    assert_eq!(
        [first_fg, second_fg]
            .iter()
            .filter(|fg| **fg == theme::role(Role::GrayDim))
            .count(),
        1,
        "one footer is dim: {first_fg:?} vs {second_fg:?}"
    );
}

#[tokio::test]
async fn pane_footer_single_pane_keeps_the_full_window_bar() {
    let mut app = app().await;
    assert!(!app.pane_manager.is_split());

    let buffer = draw(&mut app, 100, 30);
    let bottom = bottom_row(&buffer);

    // A one-pane window draws no pane index at all, and the full bar still
    // carries the approval chip it has always carried.
    assert!(!bottom.contains("[1/1]"), "bottom row: {bottom:?}");
    assert!(
        bottom.contains("approve") || bottom.contains("yolo") || bottom.contains("auto (session)"),
        "full bar keeps its policy chip: {bottom:?}"
    );
}

#[tokio::test]
async fn pane_footer_never_repeats_the_session_name() {
    let mut app = app().await;
    app.session_service
        .create_session(Some("alpha-conversation".into()))
        .await
        .unwrap();
    app.session_service
        .create_session(Some("beta-conversation".into()))
        .await
        .unwrap();
    app.load_sessions().await.unwrap();

    app.pane_manager.split(SplitDirection::Horizontal);
    let ids = app.pane_manager.pane_ids_in_order();
    assert_eq!(ids.len(), 2);
    let wanted: Vec<String> = ["alpha-conversation", "beta-conversation"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (pane, label) in ids.iter().zip(wanted.iter()) {
        let session = app
            .sessions
            .iter()
            .find(|s| s.title.as_deref() == Some(label.as_str()))
            .cloned()
            .expect("seeded session");
        app.pane_manager.get_mut(*pane).unwrap().session_id = Some(session.id);
    }

    let buffer = draw(&mut app, 100, 30);

    // The name lives in ONE place: the pane's top border, which is the ring
    // the user reads to know which session a pane carries. Repeating it in
    // the footer only ate width the footer needs for model and policy.
    let ring = row_text(&buffer, 0);
    assert!(ring.contains("alpha-conversation"), "ring row: {ring:?}");
    assert!(ring.contains("beta-conversation"), "ring row: {ring:?}");

    let (_, footer_y) = find_in_buffer(&buffer, "[1/2]").expect("first pane footer");
    let footer = row_text(&buffer, footer_y);
    assert!(
        !footer.contains("alpha-conversation"),
        "footer repeated the session name: {footer:?}"
    );
    assert!(
        !footer.contains("beta-conversation"),
        "footer repeated the session name: {footer:?}"
    );
}
