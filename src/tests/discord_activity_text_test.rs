//! Tests for the Discord flow line's live activity preview (#1844), the
//! twin of the Slack activity contracts (#1809): priority order (note >
//! bash # comments > tool label), hygiene (raw output skipped, 100-char
//! cap), bare-counts shape for empty groups, settled line stays clean, and
//! the context leading-space double-space regression.

use crate::channels::discord::tool_group::{GroupEntry, GroupState, render_content};
use std::time::{Duration, Instant};

fn tool(name: &str, context: &str, status: Option<bool>) -> GroupEntry {
    GroupEntry {
        call_id: String::new(),
        task_id: None,
        name: name.to_string(),
        context: context.to_string(),
        status,
    }
}

fn group_with(entries: Vec<GroupEntry>, notes: Vec<String>) -> GroupState {
    GroupState {
        page: 0,
        entries,
        expanded: false,
        notes,
        started_at: Instant::now(),
        settled: None,
    }
}

fn settled(group: &mut GroupState, ctx: Option<String>) {
    group.settled = Some(crate::channels::discord::tool_group::SettledStatus {
        elapsed: Duration::from_secs(42),
        ctx,
        waiting: None,
        outcome: None,
    });
}

#[test]
fn activity_prefers_the_latest_narration_note() {
    let mut g = group_with(
        vec![
            tool("grep", " pattern src", Some(true)),
            tool("cargo", " test", None),
        ],
        vec![
            "Reading the old renderer".to_string(),
            "Scanning the repository structure".to_string(),
        ],
    );
    let text = render_content(&g);
    let first = text.lines().next().expect("non-empty render");
    assert!(
        first.contains("⚙️ Scanning the repository structure · **2 tool calls**"),
        "latest note must be the activity, leading the live line: {first}"
    );
    assert!(
        !first.contains("Reading the old renderer"),
        "older note never leads: {first}"
    );
    // Discord renders narration as always-visible `-#` subtext rows (the
    // transcript); the priority contract is about the summary line, not
    // about hiding rows.
    assert!(
        text.contains("-# Reading the old renderer"),
        "narration rows stay visible: {text}"
    );
    settled(&mut g, None);
    let settled_text = render_content(&g);
    let settled_first = settled_text.lines().next().expect("non-empty render");
    assert!(
        settled_first.starts_with("✅ **2 tool calls** · ⏱️ 0:42"),
        "settled line stays clean with the frozen clock: {settled_first}"
    );
    assert!(
        !settled_first.contains("Scanning"),
        "settled line never carries activity: {settled_first}"
    );
}

#[test]
fn activity_falls_back_to_bash_hash_comments() {
    let g = group_with(
        vec![tool(
            "bash",
            " # deploying the schema\npsql -f migrate.sql",
            None,
        )],
        Vec::new(),
    );
    let text = render_content(&g);
    assert!(
        text.contains("deploying the schema"),
        "bash # comments become activity: {text}"
    );
}

#[test]
fn activity_falls_back_to_latest_tool_label() {
    let g = group_with(
        vec![
            tool("bash", " echo done", Some(true)),
            tool("cargo", " build", None),
        ],
        Vec::new(),
    );
    let text = render_content(&g);
    assert!(
        text.contains("cargo build · **2 tool calls**"),
        "latest tool label + context leads: {text}"
    );
}

#[test]
fn empty_group_keeps_the_bare_counts_shape() {
    let g = group_with(Vec::new(), Vec::new());
    let text = render_content(&g);
    assert!(
        text.contains("✅ **0 tool calls** · 🕒"),
        "bare counts shape preserved: {text}"
    );
}

#[test]
fn activity_is_capped_and_raw_output_skipped() {
    let long_note = "rebuilding the flow renderer so every channel reports \
                     what the agent is doing right now with a live timestamp"
        .to_string();
    // Two entries: single-tool groups render the lone tool row, so the
    // summary line (and its activity segment) only exists for 2+.
    let g = group_with(
        vec![
            tool("grep", " pattern src", Some(true)),
            tool("bash", " ls", None),
        ],
        vec!["/tmp/build-output.txt".to_string(), long_note.clone()],
    );
    let text = render_content(&g);
    let first = text.lines().next().expect("non-empty render");
    assert!(
        first.contains('…'),
        "long activity must be capped in the summary line: {first}"
    );
    assert!(
        !first.contains("/tmp/build-output.txt"),
        "bare paths are never the activity: {first}"
    );
    assert!(
        !first.contains("timestamp"),
        "activity is clipped at 100 chars: {first}"
    );
    assert!(
        text.contains("-# /tmp/build-output.txt"),
        "the raw note stays visible as a transcript row: {text}"
    );
}

#[test]
fn context_leading_space_never_double_spaces() {
    // tool1 is the LATEST entry so its label+context becomes the activity.
    let g = group_with(
        vec![
            tool("grep", " pattern", Some(true)),
            tool("tool1", " (arg1)", None),
        ],
        Vec::new(),
    );
    let text = render_content(&g);
    let first = text.lines().next().expect("non-empty render");
    assert!(
        first.contains("tool1 (arg1) · **2 tool calls**"),
        "single canonical space after trim: {first}"
    );
    assert!(!text.contains("tool1  (arg1)"), "no double space: {text}");
}
