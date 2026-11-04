//! /help dialog responsiveness + canonical footer (#1775).
//!
//! Under 100 columns the old fixed 50/50 split squeezed both panes into
//! unreadable slivers. The fix renders a single full-width column (right
//! content appended below left) and a shared footer row at the bottom.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::app::events::AppMode;
use crate::tui::render::render;

async fn help_app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);
    app.mode = AppMode::Help;
    app
}

fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

#[tokio::test]
async fn help_is_single_column_when_narrow_and_keeps_footer() {
    let mut app = help_app().await;

    let width = 80u16;
    let height = 30u16;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Narrow: exactly ONE content pane. The right-column sections (SESSIONS
    // etc.) are stacked below the fold of the combined single column, so the
    // structural proof is positional: nothing may start in the right half of
    // the screen. (The pane borders this used to look for are gone entirely
    // since the dialogs went full-bleed, #2162, so asserting their absence
    // would pass no matter how the columns were laid out.)
    let rows: Vec<String> = (0..height).map(|y| row_text(&buf, y, width)).collect();
    let about = rows.iter().position(|r| r.contains("ABOUT"));
    assert!(about.is_some(), "ABOUT section missing in narrow mode");
    let mid = width / 2;
    if let Some(row) = rows.iter().find(|r| r.contains("SESSIONS")) {
        let idx = row.find("SESSIONS").unwrap();
        assert!(
            idx < mid as usize,
            "narrow /help must stack sections in one column, found SESSIONS at col {idx} (right pane)"
        );
    }

    // The shared footer row: canonical key: verb spans, at the bottom of
    // the help area.
    let footer = rows
        .iter()
        .find(|r| r.contains("Esc: back"))
        .unwrap_or_else(|| panic!("no canonical footer row with 'Esc: back': {rows:?}"));
    assert!(
        footer.contains("/: search"),
        "footer missing / search: {footer:?}"
    );
    assert!(
        !footer.contains('['),
        "bracket style leaked into footer: {footer:?}"
    );
}

#[tokio::test]
async fn help_is_two_columns_when_wide() {
    let mut app = help_app().await;

    let width = 120u16;
    let height = 30u16;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Wide: ABOUT (left) and SESSIONS (right) share a row band — SESSIONS
    // must start within the right half of the screen.
    let rows: Vec<String> = (0..height).map(|y| row_text(&buf, y, width)).collect();
    let sessions_row = rows
        .iter()
        .find(|r| r.contains("SESSIONS"))
        .unwrap_or_else(|| panic!("SESSIONS missing in wide mode"));
    let idx = sessions_row.find("SESSIONS").unwrap();
    assert!(
        idx > 60,
        "wide /help must place SESSIONS in the right column (col {idx})"
    );

    let footer = rows
        .iter()
        .find(|r| r.contains("Esc: back"))
        .unwrap_or_else(|| panic!("no canonical footer row: {rows:?}"));
    assert!(
        footer.contains("/: search"),
        "footer missing / search: {footer:?}"
    );
}
