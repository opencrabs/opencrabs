//! #1809: the Slack flow line carries live activity text. The flow line is
//! Slack's only processing feedback; without the what-right-now segment it
//! shows bare counts while Telegram's live footer leads with the current
//! activity (#1052). The 3-priority derivation mirrors Telegram's
//! `latest_activity_preview`: latest narration note, latest bash `#`
//! comments, latest tool label + context.

use slack_morphism::prelude::SlackChannelId;
use slack_morphism::prelude::SlackTs;

use crate::channels::slack::tool_group::{GroupEntry, GroupState, TurnOutcome, render};

fn group(entries: Vec<GroupEntry>) -> GroupState {
    GroupState::new(SlackChannelId::new("C123".into()), entries)
}

fn text_of(g: &GroupState) -> String {
    let content = render(g, &SlackTs::new("1.0".into()));
    content.text.clone().unwrap_or_default()
}

#[test]
fn activity_prefers_the_latest_narration_note() {
    let g = group(vec![
        GroupEntry::Tool {
            name: "read_file".into(),
            context: "src/main.rs".into(),
            status: Some(true),
            task_id: None,
        },
        GroupEntry::Note("Reading the config first.".into()),
        GroupEntry::Tool {
            name: "bash".into(),
            context: "cargo test".into(),
            status: None,
            task_id: None,
        },
        GroupEntry::Note("*Now grepping the logs* for the failure.".into()),
    ]);
    let text = text_of(&g);
    // Latest note wins (priority 1), markdown markers stripped, and it
    // leads the counts.
    assert!(
        text.contains("Now grepping the logs for the failure."),
        "activity text missing: {text}"
    );
    let act = text.find("Now grepping").expect("activity present");
    let counts = text.find("tool call").expect("counts present");
    assert!(act < counts, "activity must lead the counts: {text}");
}

#[test]
fn activity_falls_back_to_bash_hash_comments() {
    let g = group(vec![
        GroupEntry::Tool {
            name: "bash".into(),
            context: "# Build the thing\ncargo build --release".into(),
            status: None,
            task_id: None,
        },
        GroupEntry::Tool {
            name: "read_file".into(),
            context: "src/lib.rs".into(),
            status: None,
            task_id: None,
        },
    ]);
    let text = text_of(&g);
    // No notes: priority 2 kicks in even though a newer non-bash tool
    // exists, same as Telegram.
    assert!(
        text.contains("Build the thing"),
        "bash # comments missing: {text}"
    );
}

#[test]
fn activity_falls_back_to_latest_tool_label() {
    let g = group(vec![
        GroupEntry::Tool {
            name: "read_file".into(),
            context: "src/main.rs".into(),
            status: Some(true),
            task_id: None,
        },
        GroupEntry::Tool {
            name: "bash".into(),
            context: String::new(),
            status: None,
            task_id: None,
        },
    ]);
    let text = text_of(&g);
    // Priority 3: latest tool, empty context, bare label. Two tools, so
    // the count segment reads 2.
    assert!(text.contains("bash · *2 tool calls*"), "fallback: {text}");
}

#[test]
fn empty_group_keeps_the_bare_counts_shape() {
    // Turn-start shell (#1808): no entries, no activity segment, the line
    // stays `✅ *0 tool calls* · 🕒 ...`.
    let g = group(vec![]);
    let text = text_of(&g);
    assert!(
        text.contains("*0 tool calls*"),
        "counts missing on empty group: {text}"
    );
}

#[test]
fn activity_is_capped_and_raw_output_skipped() {
    // Raw output (single token, path-like) is not narration: skipped, the
    // older human-readable note wins.
    let long_note = format!(
        "{}{}",
        "Working through the step list, ".repeat(6),
        "done soon"
    );
    let g = group(vec![
        GroupEntry::Note(long_note.clone()),
        GroupEntry::Tool {
            name: "bash".into(),
            context: "src/foo.rs".into(),
            status: None,
            task_id: None,
        },
    ]);
    let text = text_of(&g);
    assert!(
        text.contains("Working through the step list"),
        "note lost to raw tool context: {text}"
    );
    assert!(text.contains('…'), "long activity must be capped: {text}");
    // Cap: note + separator stays lean, never the whole 160-char note.
    let act_len = text
        .split("·")
        .next()
        .map(|s| s.chars().count())
        .unwrap_or(0);
    assert!(
        act_len < 130,
        "activity segment too long: {act_len} in {text}"
    );
}

#[test]
fn settled_line_never_carries_activity() {
    // Note + tool so the group renders a real summary line (a lone note
    // renders as the bare note line, the #1805 shell case).
    let mut g = group(vec![
        GroupEntry::Note("mid-turn narration".into()),
        GroupEntry::Tool {
            name: "bash".into(),
            context: String::new(),
            status: Some(true),
            task_id: None,
        },
    ]);
    g.settle(TurnOutcome::Finished, None, Some("ctx".into()));
    let text = text_of(&g);
    assert!(
        !text.contains("mid-turn narration"),
        "settled line must stay clean: {text}"
    );
    assert!(
        text.contains("🕒") || text.contains("⏱"),
        "settled summary: {text}"
    );
}
