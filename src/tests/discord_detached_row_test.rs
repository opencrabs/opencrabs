//! Discord renders a detached task as one updating row in the turn's tool
//! block (#2148).
//!
//! Pure assertions, no bot: `detached_render` is the seam that decides where a
//! detached-task event renders, and the handler acts on its answer. That is
//! what makes the contract testable at all, because the property worth pinning
//! is "start and finish share one message id", and standing up a Discord
//! gateway to prove it would be a test of the network rather than the code.

use crate::channels::discord::tool_card::collapse_repeats;
use crate::channels::discord::tool_group::{
    DetachedEvent, DetachedRender, GroupEntry, detached_render,
};

/// A tool row as the turn builds it, for the collapse test.
fn tool(name: &str, context: &str) -> GroupEntry {
    GroupEntry {
        name: name.to_string(),
        context: context.to_string(),
        call_id: String::new(),
        status: None,
        task_id: None,
    }
}

#[test]
fn detached_start_posts_when_the_turn_has_no_block_yet() {
    let mut entries: Vec<GroupEntry> = Vec::new();

    let render = detached_render(
        &mut entries,
        None,
        DetachedEvent::Started {
            task_id: "t-1",
            label: "cargo test",
        },
    );

    assert_eq!(
        render,
        DetachedRender::Post,
        "the first detached row has no block to edit, so it posts one"
    );
    assert_eq!(entries.len(), 1, "a start draws exactly one row");
    assert_eq!(entries[0].task_id.as_deref(), Some("t-1"));
    assert_eq!(entries[0].status, None, "a fresh detached row is running");
    assert_eq!(
        entries[0].display_name(),
        "detached task running: cargo test"
    );
}

#[test]
fn detached_finish_edits_the_same_message_id_the_start_created() {
    let mut entries: Vec<GroupEntry> = Vec::new();

    // Start: no block yet, so the caller posts and keeps the id it got back.
    let start = detached_render(
        &mut entries,
        None,
        DetachedEvent::Started {
            task_id: "t-1",
            label: "cargo test",
        },
    );
    assert_eq!(start, DetachedRender::Post);

    // What the handler stores after posting: one id, reused from here on.
    let block_mid = 777u64;

    let finish = detached_render(
        &mut entries,
        Some(block_mid),
        DetachedEvent::Finished {
            task_id: "t-1",
            success: true,
        },
    );

    assert_eq!(
        finish,
        DetachedRender::Edit(block_mid),
        "the finish must rewrite the block the start created, never post"
    );
    assert_eq!(entries.len(), 1, "one task, one row, one message");
    assert_eq!(entries[0].status, Some(true), "the row settled in place");
    assert_eq!(entries[0].task_id.as_deref(), Some("t-1"));
    assert_eq!(
        entries[0].display_name(),
        "detached task finished: cargo test",
        "the same row now reads finished"
    );
}

#[test]
fn two_detached_tasks_settle_by_their_own_id_not_their_label() {
    let mut entries: Vec<GroupEntry> = Vec::new();

    // Two tasks with the SAME label, which is the case a label match breaks.
    detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Started {
            task_id: "t-1",
            label: "cargo test",
        },
    );
    detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Started {
            task_id: "t-2",
            label: "cargo test",
        },
    );
    assert_eq!(entries.len(), 2, "both rows exist, same label or not");

    let finish = detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Finished {
            task_id: "t-2",
            success: false,
        },
    );

    assert_eq!(finish, DetachedRender::Edit(100));
    assert_eq!(entries[0].status, None, "t-1 is still running");
    assert_eq!(entries[1].status, Some(false), "t-2 failed");
    assert_eq!(
        entries[1].display_name(),
        "detached task failed: cargo test"
    );
}

#[test]
fn an_orphan_finish_renders_nothing_and_invents_no_row() {
    let mut entries: Vec<GroupEntry> = Vec::new();

    let render = detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Finished {
            task_id: "t-gone",
            success: true,
        },
    );

    assert_eq!(
        render,
        DetachedRender::Skip,
        "a finish with no matching row stays silent"
    );
    assert!(entries.is_empty(), "no row is invented for it");
}

#[test]
fn a_finish_never_posts_even_when_the_row_exists_but_the_block_does_not() {
    let mut entries: Vec<GroupEntry> = Vec::new();
    detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Started {
            task_id: "t-1",
            label: "cargo test",
        },
    );

    let render = detached_render(
        &mut entries,
        None,
        DetachedEvent::Finished {
            task_id: "t-1",
            success: true,
        },
    );

    assert_eq!(
        render,
        DetachedRender::Skip,
        "a finish without a block must not open one"
    );
}

#[test]
fn detached_rows_never_collapse_into_one_but_repeated_tool_rows_do() {
    let mut entries: Vec<GroupEntry> = Vec::new();
    detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Started {
            task_id: "t-1",
            label: "cargo test",
        },
    );
    detached_render(
        &mut entries,
        Some(100),
        DetachedEvent::Started {
            task_id: "t-2",
            label: "cargo test",
        },
    );

    assert_eq!(
        collapse_repeats(&entries).len(),
        2,
        "two detached tasks stay two rows, so each can settle on its own"
    );

    let repeated = vec![tool("cargo test", " (a)"), tool("cargo test", " (a)")];
    let rows = collapse_repeats(&repeated);
    assert_eq!(rows.len(), 1, "plain repeated calls still collapse");
    assert_eq!(rows[0].count, 2);
}
