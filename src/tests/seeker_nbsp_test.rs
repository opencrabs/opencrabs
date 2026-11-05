//! Terminal Option+Space arrives as ordinary space or NBSP on macOS (#2558).

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::ui_core::palette::{opens, opens_in_terminal};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const NBSP: char = '\u{a0}';

async fn test_app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    {
        App::new(
            service,
            context,
            Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
        )
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        App::new(service, context)
    }
}

#[test]
fn seeker_nbsp_terminal_gate_covers_option_space_encodings() {
    assert!(opens_in_terminal(true, " "));
    assert!(opens_in_terminal(true, "space"));
    assert!(opens_in_terminal(true, &NBSP.to_string()));
    assert!(opens_in_terminal(false, &NBSP.to_string()));

    assert!(!opens_in_terminal(false, " "));
    assert!(!opens_in_terminal(false, "space"));
    assert!(!opens_in_terminal(true, "\t"));
    assert!(!opens_in_terminal(true, "\n"));
    assert!(!opens_in_terminal(true, "f"));
}

#[test]
fn seeker_nbsp_gui_gate_is_unchanged() {
    assert!(opens(true, false, " "));
    assert!(opens(true, false, "space"));
    assert!(opens(false, true, "k"));
    assert!(!opens(true, false, &NBSP.to_string()));
    assert!(!opens(false, false, &NBSP.to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn seeker_nbsp_handler_opens_without_inserting_into_composer() {
    for (key, modifiers) in [
        (' ', KeyModifiers::ALT),
        (NBSP, KeyModifiers::ALT),
        (NBSP, KeyModifiers::NONE),
    ] {
        let mut app = test_app().await;
        app.handle_key_event(KeyEvent::new(KeyCode::Char(key), modifiers))
            .await
            .unwrap();

        assert!(
            app.command_palette.is_some(),
            "{key:?} with {modifiers:?} must open Seeker"
        );
        assert_eq!(app.input_buffer, "");
        assert_eq!(app.cursor_position, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn seeker_nbsp_plain_space_remains_composer_input() {
    let mut app = test_app().await;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.command_palette.is_none());
    assert_eq!(app.input_buffer, " ");
    assert_eq!(app.cursor_position, 1);
}
