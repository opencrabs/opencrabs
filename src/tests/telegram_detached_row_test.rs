//! #2147: a detached background task is ONE row on Telegram, not two.
//!
//! The start draws the row inside the turn's tool block; the finish settles
//! that same row and queues no new display item. The message-id discipline is
//! the contract: because the finish adds nothing to `display_queue`, the row
//! keeps the message id it was sent with, so the surface edits in place
//! instead of posting a second bubble (#2156 is the sibling defect where a
//! restick lost its id and the answer landed twice).
//!
//! Pure assertions, no bot: `apply_progress_event` is the seam that decides
//! what the stream loop will later send or edit.

use crate::brain::agent::ProgressEvent;
use crate::channels::telegram::flow::{
    DisplayItem, FlowOutcome, StreamingState, SubagentCounts, row_display_name,
};
use crate::channels::telegram::progress::apply_progress_event;
use std::sync::{Arc, Mutex};
use teloxide::types::MessageId;

/// `StreamingState` has no `Default` on purpose: every field is explicit so a
/// new one breaks this literal and gets consciously added here. Mirrors
/// `telegram_bg_ack_fold_test::base_state`.
fn base_state() -> Arc<Mutex<StreamingState>> {
    Arc::new(Mutex::new(StreamingState {
        is_dm: false,
        pending_suggestions: None,
        pending_trailer: None,
        delivered_files: Vec::new(),
        media_intermediates: Vec::new(),
        msg_id: None,
        thinking: String::new(),
        tool_msgs: Vec::new(),
        display_queue: Vec::new(),
        open_group_msg_id: None,
        rich_transport_failures: 0,
        flow_entries: Vec::new(),
        flow_status: None,
        flow_rich: false,
        response: String::new(),
        dirty: false,
        recreate: false,
        header_preview: None,
        compacting: false,
        sections: Default::default(),
        retained_goal: None,
        tool_round_count: 0,
        tools_started_at: None,
        turn_started_at: std::time::Instant::now(),
        flow_outcome: Some(FlowOutcome::Finished),
        bg_indicator: None,
        bg_count: None,
        subagent_counts: SubagentCounts {
            working: 0,
            awaiting: 0,
        },
        sent_intermediates: Vec::new(),
        intermediate_bubbles: Vec::new(),
        voice_msg_ids: Vec::new(),
        applied_plan_kb: Default::default(),
        processing: false,
        final_bubble: None,
        is_cli: false,
    }))
}

fn start(id: &str, label: &str) -> ProgressEvent {
    ProgressEvent::DetachedTaskStarted {
        task_id: id.to_string(),
        label: label.to_string(),
    }
}

fn finish(id: &str, label: &str, success: bool) -> ProgressEvent {
    ProgressEvent::DetachedTaskFinished {
        task_id: id.to_string(),
        label: label.to_string(),
        success,
        elapsed_secs: 42.0,
    }
}

/// The start is what draws the row, and it carries the identity the finish
/// will look for.
#[test]
fn detached_start_draws_one_row_with_its_task_id() {
    let st = base_state();
    apply_progress_event(&st, start("t-1", "cargo test"));

    let s = st.lock().unwrap();
    assert_eq!(s.tool_msgs.len(), 1, "a start draws exactly one row");
    let row = &s.tool_msgs[0];
    assert_eq!(row.task_id.as_deref(), Some("t-1"));
    assert_eq!(row.name, "cargo test");
    assert_eq!(row.completed, None, "a fresh detached row is running");
    assert!(row.dirty, "the new row must be rendered");
    assert!(
        matches!(s.display_queue.as_slice(), [DisplayItem::NewTool(0)]),
        "the start queues the send for that one row, got {:?}",
        s.display_queue.len()
    );
}

/// The core contract: one message id across start and finish.
///
/// The stream loop assigns the id when it sends the queued row, so the test
/// stamps it the way the loop would and then proves the finish neither adds a
/// display item nor moves the id.
#[test]
fn detached_finish_settles_the_same_row_and_queues_no_new_message() {
    let st = base_state();
    apply_progress_event(&st, start("t-1", "cargo test"));

    // What the send path does after draining display_queue: stamp the id.
    let sent_id = MessageId(777);
    {
        let mut s = st.lock().unwrap();
        s.tool_msgs[0].msg_id = Some(sent_id);
        s.tool_msgs[0].dirty = false;
        s.display_queue.clear();
    }

    apply_progress_event(&st, finish("t-1", "cargo test", true));

    let s = st.lock().unwrap();
    assert_eq!(
        s.tool_msgs.len(),
        1,
        "the finish must settle the existing row, never append a second one"
    );
    assert_eq!(
        s.tool_msgs[0].msg_id,
        Some(sent_id),
        "the row keeps the message id it was sent with, so the edit is in place"
    );
    assert_eq!(
        s.tool_msgs[0].completed,
        Some(true),
        "the row carries the outcome"
    );
    assert!(s.tool_msgs[0].dirty, "the settled row must be re-rendered");
    assert!(
        s.display_queue.is_empty(),
        "a finish queues no new message; a display item here would post a second bubble"
    );
}

/// A finish can arrive with no row: the process restarted, or the turn ended
/// and the block settled. It must stay silent rather than open a duplicate
/// block. Since #2218 the receipt push only runs for a task with no progress
/// callback, so this arm cannot assume an announcement follows.
#[test]
fn detached_finish_with_no_matching_row_queues_nothing() {
    let st = base_state();
    apply_progress_event(&st, finish("t-404", "cargo test", false));

    let s = st.lock().unwrap();
    assert!(s.tool_msgs.is_empty(), "no row to settle, none invented");
    assert!(
        s.display_queue.is_empty(),
        "an orphan finish must not open a new block"
    );
}

/// Identity, not label: two detached tasks can both be `cargo test`, and a
/// label match would settle whichever row it hit first.
#[test]
fn two_detached_tasks_settle_by_their_own_id() {
    let st = base_state();
    apply_progress_event(&st, start("t-1", "cargo test"));
    apply_progress_event(&st, start("t-2", "cargo test"));

    apply_progress_event(&st, finish("t-2", "cargo test", false));

    let s = st.lock().unwrap();
    assert_eq!(s.tool_msgs.len(), 2, "both rows exist");
    assert_eq!(
        s.tool_msgs[0].completed, None,
        "the first task is still running and must not be settled by the second"
    );
    assert_eq!(
        s.tool_msgs[1].completed,
        Some(false),
        "the matching row carries its own outcome"
    );
}

/// The row text says what it is and whether it is still going, because a
/// detached task outlives the turn that started it.
#[test]
fn row_display_name_marks_detached_rows_only() {
    assert_eq!(
        row_display_name("read_file", None, Some(true)),
        "read_file",
        "an ordinary tool call keeps its bare name"
    );
    assert_eq!(
        row_display_name("cargo test", Some("t-1"), None),
        "detached task running: cargo test"
    );
    assert_eq!(
        row_display_name("cargo test", Some("t-1"), Some(true)),
        "detached task finished: cargo test"
    );
    assert_eq!(
        row_display_name("cargo test", Some("t-1"), Some(false)),
        "detached task failed: cargo test"
    );
}
