//! #2146: a detached task is ONE row inside the turn's tool block, and the
//! finish settles that row in place. It is never appended as a second row, and
//! it never opens a second message.
//!
//! The identity is the task id, not the label. Two detached tasks can share a
//! label (`cargo test` twice), and a label match would settle whichever row it
//! found first, leaving the user watching a finished task and a "running" row
//! for something that already exited.

use std::sync::Arc;

use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::db::Database;
use crate::db::models::Session;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::events::TuiEvent;
use crate::tui::app::{App, DisplayMessage, ToolCallEntry, ToolCallGroup};

async fn app_with_current_session() -> (App, Uuid) {
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

    let session = Session::new(Some("row test".to_string()), None, None);
    let sid = session.id;
    app.current_session = Some(session);
    (app, sid)
}

fn started(sid: Uuid, task_id: &str, label: &str) -> TuiEvent {
    TuiEvent::DetachedTaskStarted {
        session_id: sid,
        task_id: task_id.to_string(),
        label: label.to_string(),
    }
}

fn finished(sid: Uuid, task_id: &str, label: &str, success: bool) -> TuiEvent {
    TuiEvent::DetachedTaskFinished {
        session_id: sid,
        task_id: task_id.to_string(),
        label: label.to_string(),
        success,
        elapsed_secs: 42.0,
    }
}

fn calls(app: &App) -> Vec<ToolCallEntry> {
    app.active_tool_group
        .as_ref()
        .expect("a detached task draws a row")
        .calls
        .clone()
}

#[tokio::test]
async fn a_start_draws_one_row_and_the_finish_settles_it_in_place() {
    let (mut app, sid) = app_with_current_session().await;

    app.handle_event(started(sid, "bg-1", "cargo test"))
        .await
        .unwrap();

    let drawn = calls(&app);
    assert_eq!(drawn.len(), 1, "the start draws exactly one row");
    assert_eq!(drawn[0].task_id.as_deref(), Some("bg-1"));
    assert!(!drawn[0].completed, "a running task is not completed");
    let messages_before = app.messages.len();

    app.handle_event(finished(sid, "bg-1", "cargo test", false))
        .await
        .unwrap();

    let settled = calls(&app);
    assert_eq!(
        settled.len(),
        1,
        "the finish updates the row the start drew, it does not append another"
    );
    assert!(settled[0].completed, "the finish settles the row");
    assert!(!settled[0].success, "a failed exit is carried onto the row");
    assert_eq!(
        app.messages.len(),
        messages_before,
        "the finish opens no new message"
    );
}

#[tokio::test]
async fn a_finish_settles_the_row_with_its_own_id_not_the_first_with_that_label() {
    let (mut app, sid) = app_with_current_session().await;

    app.handle_event(started(sid, "bg-a", "npm run build"))
        .await
        .unwrap();
    app.handle_event(started(sid, "bg-b", "npm run build"))
        .await
        .unwrap();
    assert_eq!(calls(&app).len(), 2);

    app.handle_event(finished(sid, "bg-b", "npm run build", true))
        .await
        .unwrap();

    let rows = calls(&app);
    assert_eq!(rows.len(), 2, "settling one task never adds a row");
    assert!(
        !rows[0].completed,
        "the first task with this label is still running"
    );
    assert!(rows[1].completed, "the row carrying bg-b settled");
    assert_eq!(rows[1].task_id.as_deref(), Some("bg-b"));
}

#[tokio::test]
async fn a_finish_after_the_block_settled_edits_the_flushed_row() {
    let (mut app, sid) = app_with_current_session().await;

    app.handle_event(started(sid, "bg-late", "cargo build --release"))
        .await
        .unwrap();

    // The turn ended and its group was flushed into the transcript, which is
    // where a slow task's finish actually lands.
    let group = app.active_tool_group.take().expect("group exists");
    app.messages.push(DisplayMessage {
        id: Uuid::new_v4(),
        role: "tool_group".to_string(),
        content: String::new(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: Some(ToolCallGroup {
            calls: group.calls.clone(),
            expanded: false,
        }),
        duration_secs: None,
    });
    let messages_before = app.messages.len();

    app.handle_event(finished(sid, "bg-late", "cargo build --release", true))
        .await
        .unwrap();

    assert_eq!(
        app.messages.len(),
        messages_before,
        "a late finish edits the settled block instead of opening a new message"
    );
    let flushed = app
        .messages
        .iter()
        .filter_map(|m| m.tool_group.as_ref())
        .flat_map(|g| g.calls.iter())
        .find(|c| c.task_id.as_deref() == Some("bg-late"))
        .expect("the flushed row is still there");
    assert!(
        flushed.completed,
        "the settled block's row carries the finish"
    );
}

#[tokio::test]
async fn a_finish_with_no_row_leaves_the_transcript_alone() {
    let (mut app, sid) = app_with_current_session().await;
    let messages_before = app.messages.len();

    app.handle_event(finished(sid, "bg-ghost", "cargo test", true))
        .await
        .unwrap();

    assert_eq!(
        app.messages.len(),
        messages_before,
        "an unmatched finish is not an error and draws nothing"
    );
    assert!(
        app.active_tool_group.is_none(),
        "an unmatched finish does not open a block"
    );
}

#[test]
fn a_running_row_reads_running_and_a_settled_one_reads_done_or_failed() {
    use crate::tui::render::detached_state_suffix;

    let mut row = ToolCallEntry {
        task_id: Some("bg-1".to_string()),
        description: "cargo test".to_string(),
        success: true,
        details: None,
        completed: false,
        tool_input: serde_json::Value::Null,
    };
    assert_eq!(detached_state_suffix(&row), Some(" (running)"));

    row.completed = true;
    assert_eq!(detached_state_suffix(&row), Some(" (done)"));

    row.success = false;
    assert_eq!(detached_state_suffix(&row), Some(" (failed)"));

    row.task_id = None;
    assert_eq!(
        detached_state_suffix(&row),
        None,
        "a plain tool call carries no detached state"
    );
}
