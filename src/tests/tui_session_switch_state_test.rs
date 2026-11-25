//! Switching sessions must not lie about whether a turn is still running,
//! and must not lose what that turn has already produced.
//!
//! Both failures were reported together on the same switch (#1420, #1421).

use std::sync::Arc;

use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::db::Session;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::App;
use crate::tui::events::AppMode;

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

/// The regression from #1342's indicator guard: `promote_to_foreground` used
/// to restore `is_processing` from the sidecar snapshot, which is taken at
/// demote time and goes stale. A stale `false` clobbered the correct value and
/// left a running session looking idle, so its thinking indicator vanished.
#[tokio::test]
async fn promoting_a_running_session_keeps_it_marked_processing() {
    let mut app = app().await;
    let sid = Uuid::new_v4();

    // The session is running: the authoritative set says so.
    app.processing_sessions.insert(sid);

    // ...but the sidecar carries a stale snapshot claiming it is idle.
    app.is_processing = false;
    app.streaming_reasoning = Some("weighing the options".to_string());
    app.demote_to_background(sid);

    app.is_processing = false;
    assert!(app.promote_to_foreground(sid), "sidecar entry should exist");

    assert!(
        app.is_processing,
        "a session in processing_sessions must stay marked processing after promotion; \
         restoring the snapshot instead is what hid the indicator"
    );
}

/// The other direction: promotion must not invent work that is not running.
#[tokio::test]
async fn promoting_an_idle_session_leaves_it_idle() {
    let mut app = app().await;
    let sid = Uuid::new_v4();

    // Live state, but the session is NOT in the processing set.
    app.processing_sessions.insert(sid);
    app.is_processing = true;
    app.streaming_reasoning = Some("mid thought".to_string());
    app.demote_to_background(sid);
    app.processing_sessions.remove(&sid);

    app.is_processing = true;
    assert!(app.promote_to_foreground(sid));

    assert!(
        !app.is_processing,
        "a session absent from processing_sessions must not come back marked as running"
    );
}

/// Everything else in the sidecar still round-trips; only the flag changed
/// its source of truth.
#[tokio::test]
async fn the_other_live_fields_still_survive_the_round_trip() {
    let mut app = app().await;
    let sid = Uuid::new_v4();

    app.processing_sessions.insert(sid);
    app.streaming_response = Some("partial answer".to_string());
    app.streaming_reasoning = Some("thinking out loud".to_string());
    app.streaming_output_tokens = 42;
    app.demote_to_background(sid);

    app.streaming_response = None;
    app.streaming_reasoning = None;
    app.streaming_output_tokens = 0;

    assert!(app.promote_to_foreground(sid));
    assert_eq!(app.streaming_response.as_deref(), Some("partial answer"));
    assert_eq!(
        app.streaming_reasoning.as_deref(),
        Some("thinking out loud")
    );
    assert_eq!(app.streaming_output_tokens, 42);
}

/// A session with nothing live leaves no sidecar entry, so the map stays
/// bounded across switches that had no turn in flight.
#[tokio::test]
async fn an_idle_session_leaves_no_sidecar_entry() {
    let mut app = app().await;
    let sid = Uuid::new_v4();
    app.demote_to_background(sid);
    assert!(
        !app.promote_to_foreground(sid),
        "nothing live means nothing to promote"
    );
}

// ── Picker cursor follows the session, not the slot (#1465) ──────────

/// The reported sequence: pick a session sitting low in the list, use it,
/// reopen the picker. Using it bumps `updated_at`, so `load_sessions`
/// re-sorts it upward; the cursor used to keep its old index and point at an
/// unrelated row, and Enter switched into that row instead. In the report the
/// stale slot held a dormant twin bound to the same Slack channel.
///
/// Asserted by identity rather than a hardcoded index, so the test says
/// nothing about tie-breaking between same-second timestamps.
#[tokio::test]
async fn reopening_the_picker_puts_the_cursor_on_the_current_session() {
    let mut app = app().await;

    let a = app
        .session_service
        .create_session(Some("alpha".into()))
        .await
        .unwrap();
    let _b = app
        .session_service
        .create_session(Some("beta".into()))
        .await
        .unwrap();
    let _c = app
        .session_service
        .create_session(Some("gamma".into()))
        .await
        .unwrap();

    app.current_session = Some(a.clone());

    // "Use" alpha: an ordinary update bumps updated_at exactly the way a
    // message append does, which is what re-sorts the list.
    app.session_service.update_session(&a).await.unwrap();

    // A stale cursor left over from the previous visit, pointing nowhere near
    // alpha's new position.
    app.selected_session_index = 99;

    app.switch_mode(AppMode::Sessions).await.unwrap();

    let landed = app
        .sessions
        .get(app.selected_session_index)
        .expect("cursor must land inside the loaded list");
    assert_eq!(
        landed.id, a.id,
        "the cursor must follow the current session after the list re-sorts; \
         keeping the old index is what put Enter on a same-channel twin (#1465)"
    );
}

/// No current session (fresh start): the cursor has nothing to anchor to and
/// must sit at the top rather than keep a stale index.
#[tokio::test]
async fn picker_falls_back_to_the_top_when_there_is_no_current_session() {
    let mut app = app().await;
    app.session_service
        .create_session(Some("alpha".into()))
        .await
        .unwrap();
    app.current_session = None;
    app.selected_session_index = 42;

    app.switch_mode(AppMode::Sessions).await.unwrap();

    assert_eq!(app.selected_session_index, 0);
}

/// The index is relative to the *filtered* view, so with a search active the
/// cursor must be the current session's position among the matches, not its
/// position in the full list.
#[tokio::test]
async fn cursor_is_resolved_against_the_filtered_view() {
    let mut app = app().await;

    let keep = app
        .session_service
        .create_session(Some("keep me".into()))
        .await
        .unwrap();
    app.sessions = vec![
        Session {
            title: Some("noise one".into()),
            ..keep.clone()
        },
        Session {
            id: uuid::Uuid::new_v4(),
            title: Some("noise two".into()),
            ..keep.clone()
        },
        keep.clone(),
    ];
    // Give the decoys distinct ids so only the real one can match.
    app.sessions[0].id = uuid::Uuid::new_v4();

    app.current_session = Some(keep.clone());
    app.session_search = "keep".to_string();

    app.focus_current_session();

    let visible = app.visible_session_indices();
    let idx = visible
        .get(app.selected_session_index)
        .and_then(|&i| app.sessions.get(i))
        .expect("cursor must land inside the filtered view");
    assert_eq!(
        idx.id, keep.id,
        "with a search active the cursor indexes the matches, not the full list"
    );
}

// ── Switch-back re-paint of mid-turn rows (#3179) ────────────────────

/// While a session sits in background the agent still persists rows to
/// the DB: queued-message continuations and the final answer row. When
/// the user switches back, `load_session` reloads those rows into
/// `self.messages` and `promote_to_foreground` appends the sidecar's
/// `pending_messages` on top. The sidecar holds the echo of the SAME
/// rows, so the extend painted every persisted mid-turn row twice:
/// the duplicated user bubble and the doubled answer from the report.
/// The merge must drop pending rows whose role and text the DB reload
/// already contains, and must keep genuinely new deltas.
#[tokio::test]
async fn promotion_does_not_repaint_rows_the_db_reload_already_has() {
    let mut app = app().await;
    let sid = Uuid::new_v4();

    // The DB reload already contains the persisted queued-message
    // continuation and the final answer row.
    app.messages = vec![
        msg_at("user", "more critical is this"),
        msg_at("assistant", "The DB has the review text in one row."),
    ];

    // The sidecar accumulated the live echo of the SAME rows while the
    // session was off-screen, plus one delta the DB has never seen.
    let sidecar =
        crate::tui::app::background_session::BackgroundSessionState {
            pending_messages: vec![
                msg_at("user", "more critical is this"),
                msg_at("assistant", "The DB has the review text in one row."),
                msg_at("user", "and the fresh one"),
            ],
            ..Default::default()
        };
    app.background_sessions.insert(sid, sidecar);

    assert!(app.promote_to_foreground(sid));

    let user_dups = app
        .messages
        .iter()
        .filter(|m| m.role == "user" && m.content == "more critical is this")
        .count();
    let answer_dups = app
        .messages
        .iter()
        .filter(|m| m.role == "assistant" && m.content == "The DB has the review text in one row.")
        .count();
    assert_eq!(
        user_dups, 1,
        "the queued-message echo must not re-paint the row the DB reload already has"
    );
    assert_eq!(
        answer_dups, 1,
        "the answer draft must not re-paint the persisted answer row"
    );
    assert!(
        app.messages
            .iter()
            .any(|m| m.role == "user" && m.content == "and the fresh one"),
        "a pending delta the DB has never seen must still merge in"
    );
    assert_eq!(
        app.messages.len(),
        3,
        "three DB rows plus one new delta: no fourth row"
    );
}

/// Exact-text equality only. A pending draft that merely resembles a
/// persisted row (streaming partial, different text) is a real delta and
/// must survive the merge untouched.
#[tokio::test]
async fn promotion_keeps_pending_rows_that_only_resemble_persisted_ones() {
    let mut app = app().await;
    let sid = Uuid::new_v4();

    app.messages = vec![msg_at("assistant", "the full answer text")];
    let sidecar =
        crate::tui::app::background_session::BackgroundSessionState {
            pending_messages: vec![msg_at(
                "assistant",
                "the full answer",
            )],
            ..Default::default()
        };
    app.background_sessions.insert(sid, sidecar);

    assert!(app.promote_to_foreground(sid));
    assert_eq!(
        app.messages.len(),
        2,
        "a partial draft is a live row, not a duplicate of the persisted text"
    );
}

fn msg_at(role: &str, content: &str) -> crate::tui::app::DisplayMessage {
    crate::tui::app::DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
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
