//! The send path must target the focused pane's session in split mode,
//! never the window-level `current_session` field. Reported as the
//! split-panes plan's task 5 regression: with pane A focused and pane B's
//! session still active in the window state, Enter crossed the wires and
//! pane B answered what was typed in pane A.

use std::sync::Arc;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::pane::{PaneManager, SplitDirection};

async fn app() -> App {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let app_value = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let app_value = App::new(service, context);
    // Tests must be hermetic: App::new restores the host's persisted pane
    // layout (~/.opencrabs/layout.json), so a fresh App can already carry
    // real panes with real sessions bound (3 leaves on the dev Mac made
    // ids.len() read 4 after one split). Reset to one clean unbound pane;
    // each test binds exactly what it needs.
    let mut app = app_value;
    app.pane_manager = PaneManager::new();
    app
}

/// Split a fresh app into two panes: the focused (second) pane bound to
/// `focused_session`, the root pane to `other_session`.
fn two_panes(app: &mut App, focused_session: uuid::Uuid, other_session: uuid::Uuid) {
    app.pane_manager.split(SplitDirection::Horizontal);
    let ids = app.pane_manager.pane_ids_in_order();
    assert_eq!(ids.len(), 2);
    // After a split focus sits on the NEW pane, so ids[1] is focused.
    app.pane_manager.get_mut(ids[1]).unwrap().session_id = Some(focused_session);
    app.pane_manager.get_mut(ids[0]).unwrap().session_id = Some(other_session);
}

#[tokio::test]
async fn send_target_in_split_mode_is_the_focused_panes_session() {
    let mut app = app().await;
    let a = app
        .session_service
        .create_session(Some("alpha".into()))
        .await
        .unwrap();
    let b = app
        .session_service
        .create_session(Some("beta".into()))
        .await
        .unwrap();
    two_panes(&mut app, a.id, b.id);
    // The window bind points at B on purpose: resolution must ignore it
    // and follow the focused pane.
    app.current_session = Some(b.clone());

    assert_eq!(app.send_target_session_id(), Some(a.id));
}

#[tokio::test]
async fn send_target_in_single_mode_is_the_current_session() {
    let mut app = app().await;
    let b = app
        .session_service
        .create_session(Some("beta".into()))
        .await
        .unwrap();
    // Single mode (never split): the window bind is the only target.
    app.current_session = None;
    assert_eq!(app.send_target_session_id(), None);

    app.current_session = Some(b.clone());
    assert_eq!(app.send_target_session_id(), Some(b.id));
}

/// The exact reported failure: pane A focused, pane B's session active in
/// the window state. Enter in pane A must start A's turn, never B's.
#[tokio::test]
async fn wrong_session_bind_cannot_capture_a_panes_send() {
    let mut app = app().await;
    let a = app
        .session_service
        .create_session(Some("alpha".into()))
        .await
        .unwrap();
    let b = app
        .session_service
        .create_session(Some("beta".into()))
        .await
        .unwrap();

    two_panes(&mut app, a.id, b.id);
    // The stale window bind: B is "active in background state".
    app.current_session = Some(b.clone());

    // Pre-condition: resolution already points at the focused pane, not
    // the window bind.
    assert_eq!(app.send_target_session_id(), Some(a.id));

    app.send_message("ping alpha".to_string()).await.unwrap();

    // The rebind happened: the window now follows the focused pane.
    assert_eq!(
        app.current_session.as_ref().map(|s| s.id),
        Some(a.id),
        "send must rebind the window to the focused pane's session"
    );
    // The turn registered on A, and B never saw a turn.
    assert!(
        app.processing_sessions.contains(&a.id),
        "the send must start a turn on the focused pane's session"
    );
    assert!(
        !app.processing_sessions.contains(&b.id),
        "the window-level session must not capture the pane's send"
    );
    // The user message entered the visible transcript of the rebound view.
    let last_user = app
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .expect("the sent message must appear in the transcript");
    assert!(
        last_user.content.contains("ping alpha"),
        "the visible transcript must carry the sent text, got: {}",
        last_user.content
    );
}

/// An unbound focused pane has no honest target. The send must be refused
/// outright instead of falling through to the window bind (which would
/// deliver the pane's text to whatever session happens to be current).
#[tokio::test]
async fn wrong_session_send_in_an_unbound_pane_is_refused() {
    let mut app = app().await;
    let b = app
        .session_service
        .create_session(Some("beta".into()))
        .await
        .unwrap();

    app.pane_manager.split(SplitDirection::Horizontal);
    // Focused (new) pane deliberately left with NO session; window bind
    // points at B and must not capture the send.
    app.current_session = Some(b.clone());

    app.send_message("should not land".to_string())
        .await
        .unwrap();

    assert_eq!(
        app.current_session.as_ref().map(|s| s.id),
        Some(b.id),
        "a refused send must not rebind the window"
    );
    assert!(
        !app.processing_sessions.contains(&b.id),
        "the window-level session must not capture an unbound pane's send"
    );
    assert!(
        !app.messages.iter().any(|m| m.role == "user"),
        "nothing may enter the transcript from a refused send"
    );
    let notice = app
        .messages
        .iter()
        .any(|m| m.role == "system" && m.content.contains("no session bound"));
    assert!(notice, "the refusal must say why the message was not sent");
}
