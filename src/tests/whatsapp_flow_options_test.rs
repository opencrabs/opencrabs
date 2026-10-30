//! Regression (#2030): a WhatsApp background-completion turn that halts on
//! `suggest_options` posts the answer it halted on before the suggestions,
//! and the sign-off does not repeat it.

use crate::channels::whatsapp::flow_render::{Step, drop_held, take_trailing_answer};

fn tool(name: &str) -> Step {
    Step::Tool {
        name: name.to_string(),
        context: String::new(),
        status: Some(true),
        task_id: None,
    }
}

fn note(text: &str) -> Step {
    Step::Note(text.to_string())
}

#[test]
fn takes_the_narration_directly_before_the_options_tool() {
    let mut steps = vec![
        tool("bash"),
        note("checking"),
        note("the answer"),
        tool("suggest_options"),
    ];
    assert_eq!(
        take_trailing_answer(&mut steps).as_deref(),
        Some("checking\n\nthe answer")
    );
    assert_eq!(steps.len(), 2);
    assert!(matches!(&steps[0], Step::Tool { name, .. } if name == "bash"));
    assert!(matches!(&steps[1], Step::Tool { name, .. } if name == "suggest_options"));
}

#[test]
fn narration_behind_another_tool_stays_in_the_block() {
    let mut steps = vec![
        note("an earlier step"),
        tool("bash"),
        tool("suggest_options"),
    ];
    assert_eq!(take_trailing_answer(&mut steps), None);
    assert_eq!(steps.len(), 3);
}

#[test]
fn narration_after_the_options_tool_is_left_for_the_sign_off() {
    let mut steps = vec![
        note("the answer"),
        tool("suggest_options"),
        note("sign-off"),
    ];
    assert_eq!(
        take_trailing_answer(&mut steps).as_deref(),
        Some("the answer")
    );
    assert!(matches!(steps.last(), Some(Step::Note(t)) if t == "sign-off"));
}

#[test]
fn without_an_options_step_the_trailing_narration_is_taken() {
    let mut steps = vec![tool("bash"), note("the answer")];
    assert_eq!(
        take_trailing_answer(&mut steps).as_deref(),
        Some("the answer")
    );
    assert_eq!(steps.len(), 1);
}

#[test]
fn a_sign_off_that_repeats_the_held_answer_drops_that_paragraph() {
    let held = vec!["the answer".to_string()];
    assert_eq!(
        drop_held("the answer\n\nAnything else?", &held),
        "Anything else?"
    );
    assert_eq!(drop_held("the answer", &held), "");
    assert_eq!(drop_held("fresh text", &[]), "fresh text");
}
