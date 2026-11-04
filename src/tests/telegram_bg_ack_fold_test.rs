//! #1377: bare background-task acks fold into the session's settled flow
//! card instead of spraying standalone checkbox bubbles. Pure assertions —
//! no bot, no runtime: the fold's state mutation (line append + counter
//! re-stamp), the ack-line grammar, the cardless refusal that routes to the
//! #1221 bubble lane, and the settled-header flip at zero.

use crate::brain::agent::BgTaskMeta;
use crate::channels::telegram::flow::{
    FlowEntry, FlowOutcome, StreamingState, SubagentCounts, settled_icon_verb,
};
use crate::channels::telegram::resume::{apply_bg_ack_fold, bg_ack_line};
use teloxide::types::MessageId;

/// Minimal settled-card state. Mirrors the literal construction in
/// `telegram_stream_loop_resume_test.rs` (StreamingState has no Default —
/// every field is explicit by design, so new fields break this compile and
/// get consciously added).
fn base_state(with_card: bool) -> StreamingState {
    StreamingState {
        is_dm: false,
        pending_suggestions: None,
        pending_trailer: None,
        delivered_files: Vec::new(),
        media_intermediates: Vec::new(),
        msg_id: None,
        thinking: String::new(),
        tool_msgs: Vec::new(),
        display_queue: Vec::new(),
        open_group_msg_id: if with_card { Some(MessageId(42)) } else { None },
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
    }
}

fn meta(success: bool, label: &str, tail: &str) -> BgTaskMeta {
    BgTaskMeta {
        task_id: "t-1".to_string(),
        success,
        label: label.to_string(),
        elapsed_secs: 5.0,
        tail: tail.to_string(),
    }
}

#[test]
fn fold_appends_system_line_and_stamps_counters() {
    let mut s = base_state(true);
    let line = bg_ack_line(&meta(true, "cargo test", "test result: ok"));
    let folded = apply_bg_ack_fold(
        &mut s,
        line.clone(),
        Some("1 task running".to_string()),
        Some(1),
    );
    assert!(folded);
    assert_eq!(s.flow_entries.len(), 1);
    match &s.flow_entries[0] {
        FlowEntry::System(l) => assert_eq!(l, &line),
        other => panic!("expected System entry, got {other:?}"),
    }
    assert_eq!(s.bg_count, Some(1));
    assert_eq!(s.bg_indicator.as_deref(), Some("1 task running"));
}

#[test]
fn fold_refuses_cardless_state_and_touches_nothing() {
    // No registered card: the fold must refuse WITHOUT mutating — the caller
    // then falls back to the #1221 standalone bubble lane.
    let mut s = base_state(false);
    let folded = apply_bg_ack_fold(&mut s, "✅ `x` 🕒 1s".into(), None, Some(0));
    assert!(!folded);
    assert!(s.flow_entries.is_empty());
    assert_eq!(s.bg_count, None);
}

#[test]
fn bg_ack_line_formats_success_with_preview() {
    let line = bg_ack_line(&meta(true, "cargo test", "test result: ok"));
    assert_eq!(line, "✅ `cargo test` 🕒 5s · test result: ok");
}

#[test]
fn bg_ack_line_failure_icon_and_empty_tail_drop_preview() {
    let line = bg_ack_line(&meta(false, "deploy", ""));
    assert_eq!(line, "❌ `deploy` 🕒 5s");
    assert!(!line.contains('·'));
}

#[test]
fn bg_ack_line_strips_backticks_and_falls_back_label() {
    // Backticks inside the label would escape the inline-code span.
    let line = bg_ack_line(&meta(true, "run `cmd-wrap`", ""));
    assert_eq!(line, "✅ `run cmd-wrap` 🕒 5s");
    let fallback = bg_ack_line(&meta(true, "", "out"));
    assert_eq!(fallback, "✅ `background task` 🕒 5s · out");
}

#[test]
fn settled_header_flips_to_finished_at_zero() {
    let none_left = SubagentCounts {
        working: 0,
        awaiting: 0,
    };
    // Registry drained: the settled card reads Finished again.
    assert_eq!(
        settled_icon_verb(Some(0), none_left, FlowOutcome::Finished),
        ("✅", "Finished".to_string())
    );
    // Still one alive: the waiting override holds.
    assert_eq!(
        settled_icon_verb(Some(1), none_left, FlowOutcome::Finished),
        ("⏳", "Waiting for 1 background task".to_string())
    );
}

#[test]
fn two_folds_decrement_then_flip_to_finished() {
    // The lifecycle Adi's screenshot sketched: card settles with 2 tasks
    // pending, two completions fold in, counters walk 2 → 1 → 0 and the
    // settled header flips to Finished on the last stamp.
    let mut s = base_state(true);
    s.bg_count = Some(2);
    s.bg_indicator = Some("2 tasks running".to_string());

    let folded = apply_bg_ack_fold(
        &mut s,
        bg_ack_line(&meta(true, "task a", "")),
        Some("1 task running".to_string()),
        Some(1),
    );
    assert!(folded);
    assert_eq!(s.bg_count, Some(1));

    let folded = apply_bg_ack_fold(
        &mut s,
        bg_ack_line(&meta(false, "task b", "")),
        None,
        Some(0),
    );
    assert!(folded);
    assert_eq!(s.bg_count, Some(0));
    assert_eq!(s.flow_entries.len(), 2);

    // Zero alive + finished outcome → the header reads Finished, not waiting.
    let (icon, verb) = settled_icon_verb(
        s.bg_count,
        s.subagent_counts,
        s.flow_outcome.unwrap_or(FlowOutcome::Finished),
    );
    assert_eq!(icon, "✅");
    assert_eq!(verb, "Finished");
}
