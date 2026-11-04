//! Session switch must not lose the transcript.
//!
//! Reported 2026-10-20: switching to another session with /sessions and coming
//! back showed two or three rows instead of the conversation, three times in one
//! day. The data was never lost (8,193 rows intact in the database); the pane
//! was rebuilt from the database and every turn came back folded.
//!
//! The fold override map used to be keyed by the anchor message's UUID. Live
//! rows carry a random UUID minted as they stream; a reload stamps the database
//! row id. The two never match, so `turn_is_folded` found no entry and fell back
//! to its default, which is folded.

use std::sync::Arc;

use crate::brain::agent::QueuedUserMessage;
use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::db::models::Message;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::app::DisplayMessage;
use crate::tui::render::chat::{turn_anchor, turn_is_folded};
use std::collections::HashMap;
use uuid::Uuid;

async fn app() -> App {
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

fn live(role: &str, content: &str, at: i64) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        timestamp: chrono::DateTime::from_timestamp(at, 0).expect("valid timestamp"),
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

fn persisted(session_id: Uuid, sequence: i32, role: &str, content: &str, at: i64) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        role: role.to_string(),
        content: content.to_string(),
        sequence,
        created_at: chrono::DateTime::from_timestamp(at, 0).expect("valid timestamp"),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    }
}

/// The reload path stamps a different UUID on every row, which is exactly why a
/// UUID-keyed map could never survive it. The anchor must not depend on it.
#[test]
fn an_expanded_turn_survives_a_reload_from_the_database() {
    let session_id = Uuid::new_v4();
    let at = 1_760_000_000i64;

    let live_turn = [live("user", "do it", at), live("assistant", "done", at + 5)];
    let live_anchor = turn_anchor(&live_turn[0]);
    let mut turn_expanded: HashMap<String, bool> = HashMap::new();
    turn_expanded.insert(live_anchor.clone(), true);
    assert!(
        !turn_is_folded(&turn_expanded, &live_anchor),
        "sanity: the clicked-open turn is open while it is live"
    );

    let reloaded: Vec<DisplayMessage> = vec![
        persisted(session_id, 1, "user", "do it", at).into(),
        persisted(session_id, 2, "assistant", "done", at + 5).into(),
    ];

    // The reload mints different row ids. That mismatch is the defect the old
    // keying walked into; the anchor is deliberately blind to it.
    assert_ne!(
        reloaded[0].id, live_turn[0].id,
        "the reload stamps a different UUID; a UUID key could never match"
    );

    let reloaded_anchor = turn_anchor(&reloaded[0]);
    assert_eq!(
        reloaded_anchor, live_anchor,
        "the anchor must be identical on both sides of the round trip"
    );
    assert!(
        !turn_is_folded(&turn_expanded, &reloaded_anchor),
        "a turn the user expanded must still be expanded after a session reload"
    );
}

/// A different turn must not inherit the override.
#[test]
fn the_anchor_distinguishes_turns() {
    let a = live("user", "first", 1_760_000_000);
    let b = live("user", "second", 1_760_000_060);
    assert_ne!(turn_anchor(&a), turn_anchor(&b));

    let mut turn_expanded: HashMap<String, bool> = HashMap::new();
    turn_expanded.insert(turn_anchor(&a), true);
    assert!(!turn_is_folded(&turn_expanded, &turn_anchor(&a)));
    assert!(
        turn_is_folded(&turn_expanded, &turn_anchor(&b)),
        "an untouched turn stays folded"
    );
}

/// While a turn runs there is no final answer yet, so folding it hides exactly
/// A running turn folds like any other: its working-out rows sit in the turn's
/// block, and only a click opens it (#2160). The streamed answer renders from
/// the streaming buffer, so folding never hides it.
#[test]
fn a_running_turn_folds_like_any_other() {
    let anchor = turn_anchor(&live("user", "build the thing", 1_760_000_000));
    let mut turn_expanded: HashMap<String, bool> = HashMap::new();
    assert!(
        turn_is_folded(&turn_expanded, &anchor),
        "a running turn folds by default"
    );
    turn_expanded.insert(anchor.clone(), true);
    assert!(
        !turn_is_folded(&turn_expanded, &anchor),
        "a click opens a running turn"
    );
}

/// The persisted form rewrites `<<IMG:...>>` markers into a hint, so the stored
/// text differs from what the live row holds. The anchor must not include it,
/// or an attachment turn would lose its override on every reload.
#[test]
fn the_anchor_ignores_content_that_the_database_rewrites() {
    let at = 1_760_000_000i64;
    let live_row = live("user", "look <<IMG:/tmp/shot.png>>", at);
    let stored = persisted(
        Uuid::new_v4(),
        1,
        "user",
        "look [image attached: /tmp/shot.png]",
        at,
    );
    let reloaded: DisplayMessage = stored.into();
    assert_ne!(
        live_row.content, reloaded.content,
        "the text really does differ"
    );
    assert_eq!(turn_anchor(&live_row), turn_anchor(&reloaded));
}

// ── Cancelling one session must not kill another session's turn (#2155) ──

/// The abort handle used to be a single app-level `Option`. Whichever task
/// started last owned it, so an Escape in one session aborted a turn running in
/// a different session. Keyed by session, a cancel reaches only its own turn.
#[tokio::test]
async fn cancelling_one_session_does_not_abort_another_sessions_turn() {
    let mut app = app().await;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    // Two live turns, each parked forever until aborted.
    let turn_a = tokio::spawn(std::future::pending::<()>());
    let turn_b = tokio::spawn(std::future::pending::<()>());
    app.task_abort_handles.insert(a, turn_a.abort_handle());
    app.task_abort_handles.insert(b, turn_b.abort_handle());

    app.abort_session_task(a);

    let killed = turn_a.await;
    assert!(
        killed.is_err() && killed.unwrap_err().is_cancelled(),
        "the cancelled session's own turn must die"
    );
    assert!(
        !turn_b.is_finished(),
        "another session's turn must keep running when a different session cancels"
    );
    assert!(
        app.task_abort_handles.contains_key(&b),
        "the other session's handle must still be registered"
    );
    assert!(
        !app.task_abort_handles.contains_key(&a),
        "a cancelled handle is consumed, so a second cancel is a no-op"
    );

    // And a second cancel of the same session leaves the other turn alone.
    app.abort_session_task(a);
    assert!(!turn_b.is_finished());

    app.abort_session_task(b);
    let killed_b = turn_b.await;
    assert!(killed_b.is_err() && killed_b.unwrap_err().is_cancelled());
}

/// The switch itself must leave the abort map alone: it is keyed by session, so
/// nothing about focus can reach into it.
#[tokio::test]
async fn switching_sessions_leaves_the_running_turn_registered() {
    let mut app = app().await;
    let a = app
        .session_service
        .create_session(Some("first".into()))
        .await
        .unwrap();
    let b = app
        .session_service
        .create_session(Some("second".into()))
        .await
        .unwrap();

    let turn = tokio::spawn(std::future::pending::<()>());
    app.task_abort_handles.insert(a.id, turn.abort_handle());
    app.processing_sessions.insert(a.id);

    app.load_session(b.id).await.unwrap();
    app.load_session(a.id).await.unwrap();

    assert!(
        app.task_abort_handles.contains_key(&a.id),
        "a switch must not drop the running turn's abort handle"
    );
    assert!(
        !turn.is_finished(),
        "a switch must not kill the turn the user left running"
    );
    assert!(
        app.is_processing,
        "coming back to a running session must still show it as running"
    );
    turn.abort();
}

// ── A queued message must survive a switch away and back (#2155) ──

/// Typed while the session is mid-turn, the message is held in the per-session
/// queue until the agent's tool loop drains it. A switch must not touch that
/// queue: the same map is the source of truth for both the UI indicator and the
/// drain callback, and both are keyed by session rather than by focus.
#[tokio::test]
async fn a_queued_message_survives_a_switch_away_and_back() {
    let mut app = app().await;
    let a = app
        .session_service
        .create_session(Some("first".into()))
        .await
        .unwrap();
    let b = app
        .session_service
        .create_session(Some("second".into()))
        .await
        .unwrap();

    app.queued_messages
        .lock()
        .unwrap()
        .entry(a.id)
        .or_default()
        .push(QueuedUserMessage::plain(
            "and also check the logs".to_string(),
        ));

    app.load_session(b.id).await.unwrap();
    app.load_session(a.id).await.unwrap();

    let queued: Vec<String> = app
        .queued_messages
        .lock()
        .unwrap()
        .get(&a.id)
        .map(|msgs| msgs.iter().map(|m| m.display_text.clone()).collect())
        .unwrap_or_default();
    assert_eq!(
        queued,
        vec!["and also check the logs".to_string()],
        "the message typed during the turn must still be queued after the round trip"
    );
}
