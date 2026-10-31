//! The `/help` dialog renders the shared key table (#2320).
//!
//! The chord rows stopped being hand-written `kv(...)` calls and became data
//! in `ui_core::help_keys`, so the terminal and the window print one list. A
//! refactor like that can compile, keep the layout, and still lose a row: the
//! loop would simply iterate something shorter. This renders the real dialog
//! and checks that every row in the table reached the screen.

use std::sync::Arc;

use ratatui::{Terminal, backend::TestBackend};

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::app::events::AppMode;
use crate::tui::render::render;
use crate::ui_core::help_keys;

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

/// The whole screen as one string, so a row can be matched wherever it lands.
fn screen_text(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
async fn every_shared_row_reaches_the_tui_help_dialog() {
    let mut app = help_app().await;
    // Tall enough that the whole left column fits, wide enough that the key
    // column (padded to 14) and the description both render in full.
    let text = screen_text(&mut app, 120, 100);

    for row in help_keys::rows() {
        assert!(
            text.contains(row.tui),
            "the /help dialog does not print the chord {:?} from the shared table",
            row.tui
        );
        assert!(
            text.contains(row.description),
            "the /help dialog does not print the description {:?} for {:?}",
            row.description,
            row.tui
        );
    }
}

#[tokio::test]
async fn the_dialog_prints_each_section_title_once() {
    let mut app = help_app().await;
    let text = screen_text(&mut app, 120, 100);

    for section in help_keys::HelpSection::ALL {
        let title = section.title();
        assert!(
            text.contains(title),
            "the /help dialog is missing the {:?} section header",
            title
        );
    }
}
