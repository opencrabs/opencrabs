//! #2255: the plan-finished chime must fire only for a plan that actually
//! closed, and only when the operator opted in.
//!
//! No audio is asserted here: CI has no output device and the point of the
//! split is that the decision is a pure function over the plan document.
//! The player itself is covered by `cargo check` on the feature and by the
//! owner's manual listen test.

use crate::config::AgentConfig;
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::completion_sound::announcement_pending;
use uuid::Uuid;

fn task(order: usize, status: TaskStatus) -> PlanTask {
    let mut t = PlanTask::new(
        order,
        format!("task {order}"),
        String::new(),
        TaskType::Other("test".to_string()),
    );
    t.status = status;
    t
}

/// An Active plan carrying the given task statuses.
fn active_plan(statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(Uuid::new_v4(), "test plan".to_string());
    plan.status = PlanStatus::Active;
    for (i, status) in statuses.iter().enumerate() {
        plan.add_task(task(i + 1, status.clone()));
    }
    plan
}

#[test]
fn completion_notification_enabled_defaults_to_false() {
    assert!(
        !AgentConfig::default().completion_notification_enabled,
        "an agent that chimes unasked is noise, so the key must default off"
    );
    let empty: AgentConfig = serde_json::from_str("{}").expect("empty agent section parses");
    assert!(
        !empty.completion_notification_enabled,
        "a config that never names the key stays silent"
    );
    let on: AgentConfig = serde_json::from_str(r#"{"completion_notification_enabled": true}"#)
        .expect("explicit key parses");
    assert!(on.completion_notification_enabled, "the opt-in must stick");
}

#[test]
fn no_plan_never_announces() {
    assert_eq!(announcement_pending(None), None, "discarded or absent plan");
}

#[test]
fn editing_plan_never_announces() {
    // A design plan can hold resolved tasks while still Editing; it is not
    // executing, so it must not chime.
    let mut plan = active_plan(&[TaskStatus::Completed, TaskStatus::Completed]);
    plan.status = PlanStatus::Editing;
    assert_eq!(announcement_pending(Some(&plan)), None);
}

#[test]
fn partial_plan_never_announces() {
    let plan = active_plan(&[
        TaskStatus::Completed,
        TaskStatus::InProgress,
        TaskStatus::Pending,
    ]);
    assert_eq!(announcement_pending(Some(&plan)), None);
}

#[test]
fn a_failed_task_never_announces() {
    let plan = active_plan(&[TaskStatus::Completed, TaskStatus::Failed]);
    assert_eq!(
        announcement_pending(Some(&plan)),
        None,
        "a plan with a failed task is not finished"
    );
}

#[test]
fn an_empty_plan_never_announces() {
    let plan = active_plan(&[]);
    assert_eq!(announcement_pending(Some(&plan)), None);
}

#[test]
fn finished_plan_announces_its_task_count() {
    let plan = active_plan(&[
        TaskStatus::Completed,
        TaskStatus::Completed,
        TaskStatus::Completed,
    ]);
    assert_eq!(announcement_pending(Some(&plan)), Some(3));
}

#[test]
fn skipped_tasks_count_as_resolved() {
    // `is_complete` treats Skipped as resolved, and the count reported to
    // surfaces must agree with the number that closed the plan.
    let plan = active_plan(&[TaskStatus::Completed, TaskStatus::Skipped]);
    assert_eq!(announcement_pending(Some(&plan)), Some(2));
}

// ---------------------------------------------------------------------------
// Hook wiring.
//
// `run_tool_loop_inner` is not callable from a unit test (a full turn needs a
// provider, a session and a live-home plan fixture, the same reason
// `claude_cli_context_anchor_test` reads the source). The hook's POSITION is
// the behaviour that matters and it cannot be reached any other way: a plan
// whose only remaining task is the trailing delivery step only becomes
// complete AFTER that task auto-completes, so an announce placed above it
// never fires for the common case. Pin the order in the source.
const TOOL_LOOP: &str = include_str!("../brain/agent/service/tool_loop.rs");

const MARKER: &str = "ProgressEvent::PlanCompleted";

#[test]
fn the_settle_hook_announces_after_the_trailing_task_and_before_archive() {
    let announce = TOOL_LOOP
        .find(MARKER)
        .expect("the settle hook emits PlanCompleted");
    let trailing = TOOL_LOOP
        .find("complete_trailing_delivery_task()")
        .expect("the trailing delivery task auto-complete");
    let archive = TOOL_LOOP
        .get(announce..)
        .expect("a str::find offset always sits on a char boundary")
        .find("archive_plan(session_id)")
        .map(|i| i + announce)
        .expect("the archive call below the announce");

    assert!(
        trailing < announce,
        "announce must sit below the trailing auto-complete, or a plan that only \
         completes once the answer is delivered never chimes"
    );
    assert!(
        announce < archive,
        "announce must sit above the archive, so the event still carries the plan title"
    );
}

#[test]
fn the_hook_uses_the_merged_callback_so_channel_turns_see_the_event() {
    let announce = TOOL_LOOP
        .find(MARKER)
        .expect("the settle hook emits PlanCompleted");
    // `saturating_sub` can land inside a multi-byte character, and this
    // source carries non-ASCII in its comments, so snap the window start
    // down to a boundary before taking the slice.
    let window_start = TOOL_LOOP.floor_char_boundary(announce.saturating_sub(600));
    let block = TOOL_LOOP
        .get(window_start..announce)
        .expect("both ends of the window sit on char boundaries");
    assert!(
        block.contains("progress_callback.as_ref()"),
        "the emit must go through the merged per-turn callback"
    );
    assert!(
        !block.contains("self.progress_callback.as_ref()"),
        "reading the service field drops the event for a channel turn whose \
         callback arrives as a per-call override"
    );
}

#[test]
fn the_hook_emits_exactly_one_announcement() {
    assert_eq!(
        TOOL_LOOP.matches(MARKER).count(),
        1,
        "a second emit site would chime twice for one finished plan"
    );
}

/// A channel surface must render nothing for `PlanCompleted` (#2255).
///
/// The event is emitted on the merged per-turn callback, so Telegram really
/// receives it. Its handler has a catch-all arm, and this pins that the
/// catch-all stays a no-op: the completion sound is a TUI decision, and a
/// channel turn gaining a surprise line for it would be a regression.
#[cfg(feature = "telegram")]
#[test]
fn telegram_progress_handler_renders_nothing_for_plan_completed() {
    use crate::brain::agent::ProgressEvent;
    use crate::channels::telegram::flow::StreamingState;
    use crate::channels::telegram::progress::apply_progress_event;
    use std::sync::{Arc, Mutex};

    let state: Arc<Mutex<StreamingState>> =
        Arc::new(Mutex::new(StreamingState::new_turn(false, false)));
    let before = {
        let s = state.lock().unwrap();
        (s.display_queue.len(), s.flow_outcome.is_some())
    };

    apply_progress_event(
        &state,
        ProgressEvent::PlanCompleted {
            title: "ship the fix".to_string(),
            tasks: 4,
        },
    );

    let after = {
        let s = state.lock().unwrap();
        (s.display_queue.len(), s.flow_outcome.is_some())
    };
    assert_eq!(
        before, after,
        "a plan completion must add no line and settle no flow on a channel surface"
    );
}

// ---------------------------------------------------------------------------
// Embedded asset.
//
// A bad swap is silent at runtime: `play_blocking` only logs a warning when
// the decoder refuses the bytes, so the operator hears nothing and nothing
// fails. Pin the shape of the file the feature ships instead.
const COMPLETION_WAV_ASSET: &[u8] = include_bytes!("../assets/sound/notification.wav");

#[test]
fn the_embedded_completion_asset_is_a_real_wav() {
    assert!(
        COMPLETION_WAV_ASSET.len() > 10_000,
        "the chime must be a real recording, not a stub (got {} bytes)",
        COMPLETION_WAV_ASSET.len()
    );
    assert_eq!(
        &COMPLETION_WAV_ASSET[0..4],
        b"RIFF",
        "the chime must be a RIFF container"
    );
    assert_eq!(
        &COMPLETION_WAV_ASSET[8..12],
        b"WAVE",
        "the chime must be a WAVE file for rodio's wav decoder"
    );
    assert!(
        COMPLETION_WAV_ASSET.windows(4).any(|w| w == b"fmt "),
        "a WAV with no fmt chunk cannot be decoded"
    );
}
