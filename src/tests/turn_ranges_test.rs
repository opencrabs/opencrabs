//! #757: a turn has no marker on DisplayMessage, so it is inferred from the
//! message list — it opens at a user message and runs to the next one.

use crate::tui::app::{DisplayMessage, ToolCallEntry, ToolCallGroup};
use crate::tui::render::chat::{turn_of, turn_ranges};
use uuid::Uuid;

fn msg(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage {
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

fn tool_group_msg() -> DisplayMessage {
    let mut m = msg("tool_group", "1 tool call");
    m.tool_group = Some(ToolCallGroup {
        calls: vec![ToolCallEntry {
            task_id: None,
            description: "read a file".to_string(),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        }],
        expanded: false,
    });
    m
}

#[test]
fn empty_list_has_no_turns() {
    assert!(turn_ranges(&[]).is_empty());
}

#[test]
fn a_full_turn_spans_user_through_its_whole_reply() {
    // user, thinking, tool_group, assistant  → one turn covering all four.
    let msgs = vec![
        msg("user", "do the thing"),
        msg("assistant", ""),
        tool_group_msg(),
        msg("assistant", "done"),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(turns.len(), 1);
    assert_eq!((turns[0].start, turns[0].end), (0, 4));
    assert_eq!(turns[0].end - turns[0].start, 4);
}

#[test]
fn each_user_message_opens_a_new_turn() {
    let msgs = vec![
        msg("user", "first"),
        msg("assistant", "a"),
        msg("user", "second"),
        msg("assistant", "b"),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(turns.len(), 2);
    assert_eq!((turns[0].start, turns[0].end), (0, 2));
    assert_eq!((turns[1].start, turns[1].end), (2, 4));
}

#[test]
fn back_to_back_user_messages_are_separate_turns() {
    let msgs = vec![msg("user", "one"), msg("user", "two")];
    let turns = turn_ranges(&msgs);
    assert_eq!(turns.len(), 2);
    assert_eq!((turns[0].start, turns[0].end), (0, 1));
    assert_eq!((turns[1].start, turns[1].end), (1, 2));
}

#[test]
fn rows_before_the_first_user_message_form_a_leading_turn() {
    // History markers / restored rows must not be dropped.
    let msgs = vec![
        msg("history_marker", "— older messages —"),
        msg("assistant", "restored"),
        msg("user", "hello"),
        msg("assistant", "hi"),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(turns.len(), 2);
    assert_eq!((turns[0].start, turns[0].end), (0, 2), "leading range kept");
    assert_eq!((turns[1].start, turns[1].end), (2, 4));
}

#[test]
fn turns_cover_every_message_without_gaps_or_overlap() {
    let msgs = vec![
        msg("system", "banner"),
        msg("user", "a"),
        msg("assistant", ""),
        tool_group_msg(),
        msg("error", "oops"),
        msg("user", "b"),
        msg("assistant", "done"),
    ];
    let turns = turn_ranges(&msgs);
    // Contiguous and complete: each turn starts where the previous ended.
    assert_eq!(turns.first().unwrap().start, 0);
    assert_eq!(turns.last().unwrap().end, msgs.len());
    for pair in turns.windows(2) {
        assert_eq!(pair[0].end, pair[1].start, "gap/overlap between turns");
    }
}

#[test]
fn turn_of_resolves_any_row_back_to_its_turn() {
    let msgs = vec![
        msg("user", "a"),
        msg("assistant", ""),
        tool_group_msg(),
        msg("user", "b"),
        msg("assistant", "done"),
    ];
    // A tool row in the middle of turn 1 resolves to turn 1.
    assert_eq!(turn_of(&msgs, 2).map(|t| (t.start, t.end)), Some((0, 3)));
    // The reply in turn 2 resolves to turn 2.
    assert_eq!(turn_of(&msgs, 4).map(|t| (t.start, t.end)), Some((3, 5)));
    // Out of range resolves to nothing.
    assert!(turn_of(&msgs, 99).is_none());
}

// ── per-turn header (#759) ──────────────────────────────────────

use crate::tui::render::chat::{TurnSummary, format_turn_header, turn_summary};

fn tool_group_with(calls: &[bool]) -> DisplayMessage {
    let mut m = msg("tool_group", "tools");
    m.tool_group = Some(ToolCallGroup {
        calls: calls
            .iter()
            .map(|ok| ToolCallEntry {
                task_id: None,
                description: "did a thing".to_string(),
                success: *ok,
                details: None,
                completed: true,
                tool_input: serde_json::Value::Null,
            })
            .collect(),
        expanded: false,
    });
    m
}

#[test]
fn a_turn_with_no_tools_gets_no_header() {
    // A plain question-and-answer turn must not gain a noise line.
    let msgs = vec![msg("user", "hi"), msg("assistant", "hello")];
    let turns = turn_ranges(&msgs);
    assert_eq!(format_turn_header(turn_summary(&msgs, turns[0]), 0), None);
}

#[test]
fn header_counts_tool_calls_across_the_whole_turn() {
    let msgs = vec![
        msg("user", "do it"),
        tool_group_with(&[true, true]),
        msg("assistant", ""),
        tool_group_with(&[true]),
        msg("assistant", "done"),
    ];
    let turns = turn_ranges(&msgs);
    let s = turn_summary(&msgs, turns[0]);
    assert_eq!(s.tool_calls, 3, "counts across groups in the turn");
    assert_eq!(s.failed, 0);
    let h = format_turn_header(s, 0).unwrap();
    assert!(h.contains("3 tool calls"), "{h}");
    assert!(h.starts_with('✓'), "{h}");
}

#[test]
fn header_marks_failure_when_a_call_failed_or_an_error_row_exists() {
    let msgs = vec![msg("user", "do it"), tool_group_with(&[true, false])];
    let s = turn_summary(&msgs, turn_ranges(&msgs)[0]);
    assert_eq!(s.failed, 1);
    let h = format_turn_header(s, 0).unwrap();
    assert!(h.starts_with('✗'), "failed turn must be marked: {h}");
    assert!(h.contains("1 failed"), "{h}");

    let with_error = vec![
        msg("user", "do it"),
        tool_group_with(&[true]),
        msg("error", "boom"),
    ];
    let s2 = turn_summary(&with_error, turn_ranges(&with_error)[0]);
    assert!(s2.has_error);
    assert!(format_turn_header(s2, 0).unwrap().starts_with('✗'));
}

#[test]
fn header_singularises_one_call_and_omits_zero_duration() {
    let s = TurnSummary {
        tool_calls: 1,
        failed: 0,
        duration_secs: 0,
        has_error: false,
    };
    // Both counts now, always: steps and tool calls are separate facts.
    assert_eq!(format_turn_header(s, 0).unwrap(), "✓ 0 steps · 1 tool call");
    assert_eq!(format_turn_header(s, 4).unwrap(), "✓ 4 steps · 1 tool call");
}

#[test]
fn header_humanises_duration() {
    let mk = |secs| TurnSummary {
        tool_calls: 2,
        failed: 0,
        duration_secs: secs,
        has_error: false,
    };
    assert!(format_turn_header(mk(45), 0).unwrap().ends_with("45s"));
    assert!(format_turn_header(mk(90), 0).unwrap().ends_with("1m 30s"));
    assert!(format_turn_header(mk(120), 0).unwrap().ends_with("2m"));
    assert!(format_turn_header(mk(7200), 0).unwrap().ends_with("2h"));
}

// ── per-turn fold (#758) ────────────────────────────────────────

use crate::tui::render::chat::{final_answer_idx, turn_is_folded, visible_when_folded};
use std::collections::HashMap;

/// user, thinking, tools, intermediate text, tools, final answer
fn worked_turn() -> Vec<DisplayMessage> {
    let mut thinking = msg("assistant", "");
    thinking.details = Some("pondering".to_string());
    vec![
        msg("user", "do it"),
        thinking,
        tool_group_with(&[true]),
        msg("assistant", "checking one more thing"),
        tool_group_with(&[true]),
        msg("assistant", "all done, here is the answer"),
    ]
}

#[test]
fn final_answer_is_the_last_visible_assistant_text() {
    let msgs = worked_turn();
    let t = turn_ranges(&msgs)[0];
    assert_eq!(final_answer_idx(&msgs, t), Some(5));
}

#[test]
fn folding_hides_the_working_out_and_keeps_question_and_answer() {
    let msgs = worked_turn();
    let t = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, t);
    let vis: Vec<usize> = (t.start..t.end)
        .filter(|i| visible_when_folded(&msgs, *i, fin))
        .collect();
    // The question and the final answer survive; thinking, both tool groups and
    // the intermediate narration fold away.
    assert_eq!(vis, vec![0, 5], "folded turn keeps only question + answer");
}

#[test]
fn folding_never_hides_errors_or_interactive_rows() {
    let mut msgs = worked_turn();
    msgs.insert(3, msg("error", "provider blew up"));
    let mut menu = msg("assistant", "");
    menu.approve_menu = Some(crate::tui::app::ApproveMenu {
        selected_option: 0,
        state: crate::tui::app::ApproveMenuState::Pending,
    });
    msgs.insert(4, menu);

    let t = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, t);
    assert!(
        visible_when_folded(&msgs, 3, fin),
        "an error row must stay visible when folded"
    );
    assert!(
        visible_when_folded(&msgs, 4, fin),
        "an approval prompt must stay reachable when folded"
    );
}

#[test]
fn every_turn_is_folded_by_default_including_a_running_one() {
    // Grouped-and-collapsed means always: no turn may render its working-out
    // as a wall, running or settled. Live progress still shows because
    // streaming text and the active tool group render from their own state.
    let overrides: HashMap<String, bool> = HashMap::new();
    let a = "user@1760000000".to_string();
    assert!(turn_is_folded(&overrides, &a), "default is folded");
}

#[test]
fn an_explicit_click_overrides_the_default_either_way() {
    let a = "user@1760000000".to_string();
    let mut overrides = HashMap::new();
    // Re-open an old turn.
    overrides.insert(a.clone(), true);
    assert!(!turn_is_folded(&overrides, &a), "click opens it");
    overrides.insert(a.clone(), false);
    assert!(turn_is_folded(&overrides, &a), "click folds it again");
}

// ── Folded-turn preview (#743 follow-up) ────────────────────────────────────
// A folded turn must not be less informative than the collapsed tool group it
// replaced, which always showed its last call.

use crate::tui::render::chat::folded_turn_preview;

fn failing_tool_group_msg(description: &str) -> DisplayMessage {
    let mut m = msg("tool_group", "1 tool call");
    m.tool_group = Some(ToolCallGroup {
        calls: vec![ToolCallEntry {
            task_id: None,
            description: description.to_string(),
            success: false,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        }],
        expanded: false,
    });
    m
}

#[test]
fn preview_shows_the_turns_last_tool_call() {
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        failing_tool_group_msg("cargo test"),
        msg("assistant", "done"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let (description, success) =
        folded_turn_preview(&msgs, turn).expect("a turn that ran tools has a preview");
    assert_eq!(description, "cargo test", "the LAST call, not the first");
    assert!(!success, "a failed call must be reported as failed");
}

#[test]
fn preview_reports_success_when_the_last_call_passed() {
    let msgs = vec![
        msg("user", "do the thing"),
        failing_tool_group_msg("first attempt"),
        tool_group_msg(),
        msg("assistant", "done"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let (description, success) = folded_turn_preview(&msgs, turn).expect("preview exists");
    assert_eq!(description, "read a file");
    assert!(success, "an earlier failure must not colour the last call");
}

#[test]
fn a_turn_with_no_tools_has_no_preview() {
    // Matches format_turn_header, which gives such a turn no header either.
    let msgs = vec![msg("user", "hello"), msg("assistant", "hi")];
    let turn = turn_ranges(&msgs)[0];
    assert_eq!(folded_turn_preview(&msgs, turn), None);
}

#[test]
fn preview_does_not_reach_outside_its_own_turn() {
    let msgs = vec![
        msg("user", "first"),
        tool_group_msg(),
        msg("user", "second"),
        msg("assistant", "no tools this time"),
    ];
    let turns = turn_ranges(&msgs);
    assert!(
        folded_turn_preview(&msgs, turns[1]).is_none(),
        "the second turn ran no tools, so the first turn's call must not leak in"
    );
}

#[test]
fn preview_survives_a_range_past_the_end() {
    // turn_ranges is index-based, so a stale range must not panic.
    let msgs = vec![msg("user", "hi")];
    let past_end = crate::tui::render::chat::TurnRange { start: 0, end: 99 };
    assert_eq!(folded_turn_preview(&msgs, past_end), None);
}

// ── A turn must never erase its own collapse control (#743 follow-up) ───────
// The header is the only way to fold a turn back up. It used to exist only
// while folded, so expanding a turn with no tool calls left it open with
// nothing on screen to close it.

#[test]
fn a_turn_with_working_out_has_a_header_in_both_states() {
    // No tool calls, so format_turn_header gives nothing and the step-count
    // fallback is the turn's only header. It must not depend on fold state.
    let msgs = vec![
        msg("user", "do the thing"),
        msg("assistant", "intermediate narration"),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert!(
        (turn_hideable_count(&msgs, turn, fin) > 0),
        "the narration row is hideable, so the header must exist whether \
         the turn is folded or open"
    );
}

#[test]
fn a_bare_question_and_answer_needs_no_header() {
    // Nothing to fold away, so no header and nothing to strand the user with.
    let msgs = vec![msg("user", "hi"), msg("assistant", "hello")];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert!(!(turn_hideable_count(&msgs, turn, fin) > 0));
}

#[test]
fn a_turn_with_tool_groups_has_working_out() {
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        msg("assistant", "done"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert!((turn_hideable_count(&msgs, turn, fin) > 0));
}

#[test]
fn a_thinking_only_row_counts_as_working_out() {
    // Reasoning rows carry empty content, so they are never the final answer
    // and always fold away. A turn holding one still needs its header.
    let mut thinking = msg("assistant", "");
    thinking.details = Some("some reasoning".to_string());
    let msgs = vec![msg("user", "hi"), thinking, msg("assistant", "answer")];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert!((turn_hideable_count(&msgs, turn, fin) > 0));
}

// ── A running turn has no final answer yet ──────────────────────────────────
// Mid-turn, each fresh intermediate text became the last non-empty assistant
// row and so got promoted into the one slot the fold must always show. One
// block of narration stayed exposed while the tool groups and thinking around
// it folded away.

use crate::tui::render::chat::settled_final_answer_idx;

#[test]
fn a_running_turn_exempts_nothing_from_folding() {
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        msg("assistant", "intermediate narration, not the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    assert_eq!(
        settled_final_answer_idx(&msgs, turn, true, true),
        None,
        "the newest text mid-turn is the latest step, not the reply"
    );
}

#[test]
fn a_settled_turn_keeps_its_answer_visible() {
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    assert_eq!(settled_final_answer_idx(&msgs, turn, true, false), Some(2));
}

#[test]
fn an_earlier_turn_keeps_its_answer_while_a_later_one_runs() {
    // Only the newest turn can be unsettled; scrollback must not blank out
    // every previous reply just because a new turn is in flight.
    let msgs = vec![
        msg("user", "first"),
        msg("assistant", "first answer"),
        msg("user", "second"),
        msg("assistant", "still working"),
    ];
    let turns = turn_ranges(&msgs);
    assert_eq!(
        settled_final_answer_idx(&msgs, turns[0], false, true),
        Some(1),
        "an older turn is finished by definition"
    );
    assert_eq!(settled_final_answer_idx(&msgs, turns[1], true, true), None);
}

#[test]
fn everything_folds_while_the_turn_runs() {
    // The whole point: with no exempt row, every step is behind the header.
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        msg("assistant", "narration"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = settled_final_answer_idx(&msgs, turn, true, true);
    assert!(!visible_when_folded(&msgs, 1, fin), "tool group folds");
    assert!(!visible_when_folded(&msgs, 2, fin), "narration folds too");
    assert!(
        visible_when_folded(&msgs, 0, fin),
        "the question always stays"
    );
}

// ── The step count reports what folding hides (#777) ────────────────────────
// It used to report the turn's total message count, which includes the user's
// own message and the final answer. A turn with one hidden row announced
// "3 steps".

use crate::tui::render::chat::{format_step_count, turn_hideable_count};

#[test]
fn the_step_count_excludes_the_question_and_the_answer() {
    // The reported shape: question, one thinking row, answer.
    let mut thinking = msg("assistant", "");
    thinking.details = Some("some reasoning".to_string());
    let msgs = vec![
        msg("user", "why did this happen?"),
        thinking,
        msg("assistant", "because X"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(
        turn_hideable_count(&msgs, turn, fin),
        1,
        "3 messages, but only the thinking row is ever hidden"
    );
}

#[test]
fn the_step_count_counts_every_hidden_row() {
    let msgs = vec![
        msg("user", "do the thing"),
        tool_group_msg(),
        msg("assistant", "narration"),
        tool_group_msg(),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(turn_hideable_count(&msgs, turn, fin), 3);
}

#[test]
fn a_bare_exchange_hides_nothing() {
    let msgs = vec![msg("user", "hi"), msg("assistant", "hello")];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(turn_hideable_count(&msgs, turn, fin), 0);
}

#[test]
fn errors_are_never_counted_as_hidden() {
    // They stay on screen when folded, so counting them would re-inflate.
    let msgs = vec![
        msg("user", "do the thing"),
        msg("error", "it broke"),
        msg("assistant", "here is what happened"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(turn_hideable_count(&msgs, turn, fin), 0);
}

#[test]
fn one_hidden_row_reads_as_singular() {
    assert_eq!(format_step_count(1), "1 step");
    assert_eq!(format_step_count(2), "2 steps");
    assert_eq!(format_step_count(0), "0 steps");
}

#[test]
fn a_turn_that_ran_no_tools_says_so_instead_of_hiding_it() {
    // The whole point of #781: "17 steps" alone let a turn that narrated
    // seventeen times without executing anything look unremarkable.
    let s = TurnSummary {
        tool_calls: 0,
        failed: 0,
        duration_secs: 0,
        has_error: false,
    };
    assert_eq!(
        format_turn_header(s, 17).unwrap(),
        "✓ 17 steps · 0 tool calls",
        "zero must be stated, never omitted"
    );
}

#[test]
fn nothing_to_report_still_gets_no_header() {
    // No steps and no tools is a plain exchange; a header there is noise.
    let s = TurnSummary {
        tool_calls: 0,
        failed: 0,
        duration_secs: 0,
        has_error: false,
    };
    assert_eq!(format_turn_header(s, 0), None);
}

// ── Harness status is not a step (#786) ─────────────────────────────────────
// Model switches, retries and phantom warnings are the harness reporting on
// itself, not work the agent did. They were hidden by the fold, which both
// buried the warnings that matter most and counted each one as a "step".

#[test]
fn a_status_notice_is_not_counted_as_a_step() {
    let msgs = vec![
        msg("user", "carry on"),
        msg("system", "[Model changed to provider/model-name]"),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(
        turn_hideable_count(&msgs, turn, fin),
        0,
        "a model switch is not a step the agent took"
    );
}

#[test]
fn a_status_notice_stays_visible_when_folded() {
    // A phantom warning nobody can see is not a warning.
    let msgs = vec![
        msg("user", "carry on"),
        msg("system", "⚡ Model reasoned without answering — nudge 1/5"),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert!(visible_when_folded(&msgs, 1, fin));
}

#[test]
fn real_work_is_still_counted_alongside_status() {
    // The status row must not mask the steps around it.
    let msgs = vec![
        msg("user", "do the thing"),
        msg("system", "[Model changed to provider/model-name]"),
        tool_group_msg(),
        msg("assistant", "narration"),
        msg("assistant", "the answer"),
    ];
    let turn = turn_ranges(&msgs)[0];
    let fin = final_answer_idx(&msgs, turn);
    assert_eq!(
        turn_hideable_count(&msgs, turn, fin),
        2,
        "tool group and narration count; the status notice does not"
    );
}
