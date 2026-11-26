//! Ctrl+O while a turn streams toggles the live reasoning view (#3093).
//!
//! The live stream used to be unwritable from the user's side: the capped
//! excerpt (#742) and the rolling 12-line window were the only views, and
//! both the full text and the interrupt decision it informs had to wait for
//! the turn to settle. Three laws pinned here:
//!
//! 1. Streaming with reasoning: the toggle flips, capped to full and back.
//! 2. Idle: the flag never moves, so Ctrl+O keeps its stored-turn meaning.
//! 3. A fresh send resets the view: the full wall never persists into the
//!    next turn.

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    return App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    App::new(service, context)
}

#[tokio::test]
async fn streaming_ctrl_o_flips_the_live_reasoning_view_both_ways() {
    let mut app = app().await;
    app.is_processing = true;
    app.streaming_reasoning = Some("weighing two designs".to_string());

    app.toggle_live_reasoning();
    assert!(app.live_reasoning_full, "first ctrl+o expands the stream");

    app.toggle_live_reasoning();
    assert!(!app.live_reasoning_full, "second ctrl+o caps it again");
}

#[tokio::test]
async fn idle_ctrl_o_never_touches_the_live_reasoning_view() {
    let mut app = app().await;
    // Reasoning left over from a settled turn, nothing running.
    app.is_processing = false;
    app.streaming_reasoning = Some("an old thought".to_string());

    app.toggle_live_reasoning();
    assert!(!app.live_reasoning_full);

    // Running with no reasoning yet (text-only stream) is idle for this key.
    app.is_processing = true;
    app.streaming_reasoning = None;
    app.toggle_live_reasoning();
    assert!(!app.live_reasoning_full);
}

#[tokio::test]
async fn a_new_app_starts_capped_again() {
    // The send paths reset the flag beside the other streaming fields, so a
    // fresh turn always shows the status view until the key is pressed
    // again. The constructor default is the floor of that law.
    let app = app().await;
    assert!(!app.live_reasoning_full);
}
