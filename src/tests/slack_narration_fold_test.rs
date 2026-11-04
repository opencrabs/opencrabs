//! Regression (#943): the agent's between-tool narration folds into the
//! collapsible step group instead of being posted as its own chat message.
//!
//! Posted standalone it sat in the channel looking like an answer, and on a
//! turn that ended with an empty final the empty-final guard promoted it to
//! one — permanently, because that path skips the delete-the-intermediates
//! step. Folding it in matches what Telegram's flow block already does.

use crate::channels::slack::tool_group::{GroupEntry, GroupState, render};
use slack_morphism::prelude::{SlackChannelId, SlackMessageContent, SlackTs};

fn tool(name: &str, status: Option<bool>) -> GroupEntry {
    GroupEntry::Tool {
        name: name.to_string(),
        context: String::new(),
        status,
        task_id: None,
    }
}

fn group(entries: Vec<GroupEntry>, expanded: bool) -> GroupState {
    let mut g = GroupState::new(SlackChannelId::new("C1".into()), entries);
    g.expanded = expanded;
    g
}

fn text_of(content: &SlackMessageContent) -> String {
    content.text.clone().unwrap_or_default()
}

/// The narration from the reported thread.
const NARRATION: &str = "Let me verify one thing properly before I report it";

#[test]
fn collapsed_group_hides_narration() {
    // The whole point: the narration TRANSCRIPT stays hidden until asked
    // for. Since #1809 the summary legitimately leads with the latest
    // activity (the newest note's one-line preview, Telegram live-footer
    // parity); what must never appear collapsed is the 💭 row list.
    let g = group(
        vec![
            tool("bash", Some(true)),
            GroupEntry::Note(NARRATION.to_string()),
            tool("read_file", Some(true)),
        ],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(
        !text.contains("💭"),
        "collapsed group must not list narration rows. Got:\n{text}"
    );
}

#[test]
fn expanded_group_shows_narration_in_order() {
    // Folding it away must not lose it — expanding shows the real sequence.
    let g = group(
        vec![
            tool("bash", Some(true)),
            GroupEntry::Note(NARRATION.to_string()),
            tool("read_file", Some(true)),
        ],
        true,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains(NARRATION), "expanded must show it: {text}");

    // #1809: the summary leads with the note as activity too, so the ROW
    // occurrence is the LAST one in the text (rfind), not the summary's.
    let note_at = text.rfind(NARRATION).expect("narration present");
    let bash_at = text.find("bash").expect("bash present");
    let read_at = text.find("read_file").expect("read_file present");
    assert!(
        bash_at < note_at && note_at < read_at,
        "steps must render in the order they happened. Got:\n{text}"
    );
}

#[test]
fn summary_counts_steps_and_tools_separately_when_they_differ() {
    // "3 tool calls" beside five folded lines would misdescribe the contents.
    let g = group(
        vec![
            tool("bash", Some(true)),
            GroupEntry::Note("thinking".to_string()),
            GroupEntry::Note("more thinking".to_string()),
            tool("read_file", Some(true)),
        ],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains("4 steps"), "got: {text}");
    assert!(text.contains("2 tool calls"), "got: {text}");
}

#[test]
fn summary_stays_tool_only_when_there_is_no_narration() {
    // A tools-only turn must read exactly as before this change.
    let g = group(
        vec![tool("bash", Some(true)), tool("read_file", Some(true))],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains("2 tool calls"), "got: {text}");
    assert!(
        !text.contains("step"),
        "no narration means no step count to explain. Got:\n{text}"
    );
}

#[test]
fn narration_does_not_count_as_a_running_step() {
    // A note is a record of something already said. Counting it as running
    // would leave the group showing "running" forever after the tools finish.
    let g = group(
        vec![
            tool("bash", Some(true)),
            GroupEntry::Note(NARRATION.to_string()),
        ],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(
        !text.contains("running"),
        "a finished turn must not report a running step. Got:\n{text}"
    );
    assert!(text.contains("✅"), "got: {text}");
}

#[test]
fn a_running_tool_is_still_reported_alongside_narration() {
    let g = group(
        vec![GroupEntry::Note(NARRATION.to_string()), tool("bash", None)],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains("1 running"), "got: {text}");
}

#[test]
fn a_failed_tool_is_still_reported_alongside_narration() {
    let g = group(
        vec![
            GroupEntry::Note(NARRATION.to_string()),
            tool("bash", Some(false)),
        ],
        false,
    );
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains("1 failed"), "got: {text}");
    assert!(text.contains("❌"), "got: {text}");
}

#[test]
fn a_lone_narration_step_renders_without_a_summary_header() {
    // Single-entry groups render as one plain line; a note must follow the
    // same rule rather than falling through to a bare summary with no body.
    let g = group(vec![GroupEntry::Note(NARRATION.to_string())], false);
    let text = text_of(&render(&g, &SlackTs::new("1.0".into())));
    assert!(text.contains(NARRATION), "got: {text}");
}

// ── Salvage when the final response is empty (#951) ──────────────────────────
//
// #943 stopped narration being posted standalone, which also emptied the
// buffer the empty-final guard fed on. A turn whose final came back empty then
// posted nothing at all — the answer stayed sealed in a collapsed group. These
// pin the salvage path.

use crate::channels::slack::tool_group::{consume_notes, notes_text};

#[test]
fn a_group_with_no_narration_has_nothing_to_salvage() {
    // Tool rows are a record of what ran, not something to say back. Posting
    // them as an answer would be worse than posting nothing.
    let entries = vec![tool("bash", Some(true)), tool("read_file", Some(true))];
    assert_eq!(notes_text(&entries), None);
}

#[test]
fn an_empty_group_has_nothing_to_salvage() {
    assert_eq!(notes_text(&[]), None);
}

#[test]
fn narration_is_recovered_in_order() {
    // When the final is empty this text IS the answer, so order and content
    // both have to survive.
    let entries = vec![
        GroupEntry::Note("First finding.".to_string()),
        tool("bash", Some(true)),
        GroupEntry::Note("Second finding.".to_string()),
    ];
    let salvaged = notes_text(&entries).expect("two notes are present");
    let first = salvaged.find("First finding.").expect("first present");
    let second = salvaged.find("Second finding.").expect("second present");
    assert!(first < second, "order must hold:\n{salvaged}");
    assert!(
        !salvaged.contains("bash"),
        "tool rows must not leak into the answer:\n{salvaged}"
    );
}

#[test]
fn blank_notes_do_not_produce_an_empty_answer() {
    // Posting a whitespace-only message reads as a broken reply; returning
    // None lets the caller log that it had nothing instead.
    let entries = vec![
        GroupEntry::Note("   ".to_string()),
        GroupEntry::Note("\n".to_string()),
    ];
    assert_eq!(notes_text(&entries), None);
}

// ── Salvage consumes the notes out of the group (#1805) ──────────────────────
//
// #951's salvage posts the folded narration as the answer message; leaving the
// notes in the group as well meant an expanded group displayed the answer
// TWICE — the visible duplication reported on #1805. The notes must leave the
// group when they become the answer, Telegram's take_folded_final mirror.

#[test]
fn consuming_notes_keeps_the_tool_record_and_drops_the_narration() {
    let entries = vec![
        GroupEntry::Note("First finding.".to_string()),
        tool("bash", Some(true)),
        GroupEntry::Note("Second finding.".to_string()),
    ];
    let remaining = consume_notes(&entries);
    assert_eq!(
        remaining.len(),
        1,
        "only the tool row survives:\n{remaining:?}"
    );
    assert!(
        matches!(&remaining[0], GroupEntry::Tool { name, .. } if name == "bash"),
        "the surviving entry must be the tool row:\n{remaining:?}"
    );
    assert!(
        !matches!(remaining[0], GroupEntry::Note(_)),
        "no note may survive the consume"
    );
}

#[test]
fn consuming_a_note_only_group_empties_it_so_the_shell_can_be_deleted() {
    // When the group held nothing but narration, its message has nothing left
    // to show after the salvage: the caller deletes it instead of rendering
    // a "0 tool calls" shell, and the channel keeps exactly one copy.
    let entries = vec![
        GroupEntry::Note("Only speech.".to_string()),
        GroupEntry::Note("More speech.".to_string()),
    ];
    assert!(
        consume_notes(&entries).is_empty(),
        "a note-only group must consume to empty"
    );
}

#[test]
fn consuming_a_tool_only_group_changes_nothing() {
    let entries = vec![tool("bash", Some(true)), tool("read_file", None)];
    let remaining = consume_notes(&entries);
    assert_eq!(remaining.len(), 2);
    assert!(
        remaining
            .iter()
            .all(|e| matches!(e, GroupEntry::Tool { .. })),
        "tool rows pass through untouched"
    );
}

#[test]
fn salvage_then_consume_never_show_the_same_text_twice() {
    // The end-to-end contract of the #1805 fix: whatever the salvage posts as
    // the answer is exactly what the group stops displaying.
    let entries = vec![
        GroupEntry::Note("Finding one.".to_string()),
        tool("bash", Some(true)),
        GroupEntry::Note("Finding two.".to_string()),
    ];
    let answer = notes_text(&entries).expect("salvage has the narration");
    let remaining = consume_notes(&entries);
    for entry in &remaining {
        if let GroupEntry::Note(text) = entry {
            assert!(
                !answer.contains(text.as_str()),
                "a consumed note must not survive in the group:\n{answer}"
            );
        }
    }
    assert!(
        !remaining.iter().any(|e| matches!(e, GroupEntry::Note(_))),
        "no notes may remain after consume"
    );
}
