//! The ordinary turn's live tool block (#2609).
//!
//! The block's paint decision is pure (`flow_render::paints`), so what it does
//! on a turn that calls no tools is testable without a client. The two
//! promises that live in the handler's await chain are pinned structurally:
//! the block is config-gated, and it settles before the answer is sent.

use crate::channels::whatsapp::flow_render::{Step, paints};
use std::path::Path;

fn tool_row(status: Option<bool>) -> Step {
    Step::Tool {
        name: "bash".to_string(),
        context: String::new(),
        status,
        task_id: None,
    }
}

/// A turn that only talked: narration, no tool call, so no message at all.
#[test]
fn a_reply_with_no_tool_rows_paints_nothing() {
    let steps = vec![Step::Note("here you go".to_string())];
    assert!(!paints(&steps, false, true));
}

/// An empty turn (nothing streamed yet) is also silent.
#[test]
fn an_empty_turn_paints_nothing() {
    assert!(!paints(&[], false, true));
}

/// The first tool call is what opens the message, running or already done.
#[test]
fn the_first_tool_row_opens_the_block() {
    assert!(paints(&[tool_row(None)], false, true));
    assert!(paints(&[tool_row(Some(true))], false, true));
}

/// Narration after a row does not un-paint a block that is already open.
#[test]
fn a_posted_block_keeps_painting_as_it_settles() {
    let steps = vec![tool_row(Some(true)), Step::Note("done".to_string())];
    assert!(paints(&steps, true, true));
}

/// Background completions keep posting eagerly (#2023): there the block is the
/// only sign of life, so waiting for a tool row would show nothing.
#[test]
fn an_eager_block_paints_before_its_first_row() {
    assert!(paints(&[], false, false));
    assert!(paints(&[tool_row(None)], false, false));
}

fn handler_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/whatsapp/handler.rs");
    std::fs::read_to_string(&path).expect("handler.rs is readable")
}

/// Off by default means off by config: the block is only built when
/// `live_tool_block` is set, and the callback chain is skipped otherwise.
#[test]
fn the_handler_gates_the_block_on_config() {
    let src = handler_source();
    assert!(
        src.contains("wa_cfg.live_tool_block.then("),
        "the ordinary turn's block must be gated on `live_tool_block`"
    );
    assert!(
        src.contains("wait_for_first_tool: true"),
        "an ordinary turn must wait for its first tool row before posting"
    );
    assert!(
        src.contains("let rows = flow.row_callback();"),
        "the block must receive tool rows through `row_callback`"
    );
}

/// #2030: a finished turn never folds its reply into the tool list, so the
/// block's last edit has to land before the answer goes out.
#[test]
fn the_block_settles_before_the_answer_is_sent() {
    let src = handler_source();
    let settle = src
        .find("flow.settle(")
        .expect("the ordinary turn must settle its block");
    let answer = src
        .find("match result {")
        .expect("the turn must branch on its result");
    assert!(
        settle < answer,
        "the block must settle before the answer branch, not after it"
    );
}
