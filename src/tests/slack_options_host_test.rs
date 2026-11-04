//! Regression (#2029): a Slack turn that halts on `suggest_options` posts the
//! answer it halted on before the buttons, not folded inside the step group.

use crate::channels::slack::options_host::{held_paragraphs, take_trailing_answer};
use crate::channels::slack::tool_group::GroupEntry;

fn tool(name: &str) -> GroupEntry {
    GroupEntry::Tool {
        name: name.to_string(),
        context: String::new(),
        status: Some(true),
        task_id: None,
    }
}

fn note(text: &str) -> GroupEntry {
    GroupEntry::Note(text.to_string())
}

#[test]
fn takes_the_notes_directly_before_the_options_tool() {
    let mut steps = vec![
        tool("bash"),
        note("checking"),
        note("the answer"),
        tool("suggest_options"),
    ];
    let answer = take_trailing_answer(&mut steps);
    assert_eq!(answer.as_deref(), Some("checking\n\nthe answer"));
    assert!(
        matches!(steps.as_slice(), [GroupEntry::Tool { name, .. }, GroupEntry::Tool { name: opt, .. }] if name == "bash" && opt == "suggest_options")
    );
}

#[test]
fn a_tool_between_the_note_and_the_options_tool_keeps_the_note_folded() {
    let mut steps = vec![
        note("an earlier step"),
        tool("bash"),
        tool("suggest_options"),
    ];
    assert_eq!(take_trailing_answer(&mut steps), None);
    assert_eq!(steps.len(), 3);
}

#[test]
fn notes_after_the_options_tool_are_left_for_the_sign_off() {
    let mut steps = vec![
        note("the answer"),
        tool("suggest_options"),
        note("sign-off"),
    ];
    assert_eq!(
        take_trailing_answer(&mut steps).as_deref(),
        Some("the answer")
    );
    assert!(matches!(steps.last(), Some(GroupEntry::Note(t)) if t == "sign-off"));
}

#[test]
fn without_an_options_entry_the_trailing_notes_are_taken() {
    let mut steps = vec![tool("bash"), note("the answer")];
    assert_eq!(
        take_trailing_answer(&mut steps).as_deref(),
        Some("the answer")
    );
    assert_eq!(steps.len(), 1);
}

#[test]
fn blank_notes_are_not_an_answer() {
    let mut steps = vec![note("   "), tool("suggest_options")];
    assert_eq!(take_trailing_answer(&mut steps), None);
    assert_eq!(steps.len(), 2);
}

#[test]
fn held_answer_paragraphs_strip_a_repeated_sign_off() {
    assert!(held_paragraphs(&[]).is_empty());
    assert_eq!(
        held_paragraphs(&["first\n\nsecond".to_string()]),
        vec!["first".to_string(), "second".to_string()]
    );
}
