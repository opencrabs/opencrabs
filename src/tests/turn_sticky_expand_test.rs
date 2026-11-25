//! Blocks born mid-turn inherit the turn's expansion state (#3094).
//!
//! Ctrl+O used to be a one-shot sweep: blocks created after the press arrived
//! collapsed, so reading an agentic turn meant re-pressing Ctrl+O for every
//! new tool group and thinking row. The turn's `turn_expanded` map entry is
//! now the source of truth at construction time, and three laws hold:
//!
//! 1. New tool groups on a sticky-expanded newest turn are born expanded.
//! 2. On a folded turn, new blocks stay collapsed: the default unchanged.
//! 3. With no turn on screen (fresh app, or no messages yet), nothing
//!    inherits: `newest_turn_is_expanded` is false and the helper is inert.

use std::sync::Arc;

use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::{App, DisplayMessage, ToolCallEntry, ToolCallGroup};

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

/// A minimal user row: a user message opens a turn, which is all the anchor
/// resolution needs.
fn user_row() -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: "user".to_string(),
        content: "do the thing".to_string(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

fn fresh_group() -> ToolCallGroup {
    ToolCallGroup {
        calls: vec![ToolCallEntry {
            description: "run the check".to_string(),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
            task_id: None,
        }],
        expanded: false,
    }
}

#[tokio::test]
async fn turn_expanded_newest_turn_defaults_folded() {
    let mut app = app().await;
    app.messages.push(user_row());
    // No override entry yet: every turn defaults folded.
    assert!(!app.newest_turn_is_expanded());

    // Flagging the newest turn sticky makes new blocks inherit.
    let anchor = app.newest_turn_anchor().unwrap();
    app.turn_expanded.insert(anchor, true);
    assert!(app.newest_turn_is_expanded());

    // The construction sites stamp new groups with this boolean; apply the
    // same write a live `set_active_tool_group` performs and check the result.
    let mut group = fresh_group();
    group.expanded = app.newest_turn_is_expanded();
    assert!(group.expanded, "new group inherits the expanded turn");
}

#[tokio::test]
async fn turn_expanded_folded_turn_stays_folded_for_new_blocks() {
    let mut app = app().await;
    app.messages.push(user_row());
    // Explicitly folded, not just absent: same result.
    let anchor = app.newest_turn_anchor().unwrap();
    app.turn_expanded.insert(anchor, false);

    let mut group = fresh_group();
    group.expanded = app.newest_turn_is_expanded();
    assert!(!group.expanded, "folded turn keeps new blocks collapsed");
}

#[tokio::test]
async fn turn_expanded_no_turn_on_screen_means_nothing_inherits() {
    // Fresh app, empty transcript: no anchor resolves, so the helper must be
    // inert rather than panicking or inheriting some phantom state.
    let app = app().await;
    assert!(!app.newest_turn_is_expanded());
    assert!(app.newest_turn_anchor().is_none());

    let mut group = fresh_group();
    group.expanded = app.newest_turn_is_expanded();
    assert!(!group.expanded);
}
