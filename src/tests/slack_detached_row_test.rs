//! A detached background task is one row in the turn's tool group, updated in
//! place when it finishes (#2145, #2149).
//!
//! The contract under test is the message identity: the start may post the
//! group and the finish must rewrite that same ts, never post a second bubble.
//! These drive [`super::super::channels::slack::tool_group::detached_render`],
//! the same pure decision the progress callback performs, so the assertions
//! hold without a Slack connection.

use crate::channels::slack::tool_group::{
    DetachedEvent, DetachedRender, GroupEntry, detached_render,
};

fn started<'a>(task_id: &'a str, label: &'a str) -> DetachedEvent<'a> {
    DetachedEvent::Started { task_id, label }
}

fn finished<'a>(task_id: &'a str, success: bool) -> DetachedEvent<'a> {
    DetachedEvent::Finished { task_id, success }
}

#[test]
fn detached_finish_edits_the_same_ts_the_start_created() {
    let mut entries: Vec<GroupEntry> = Vec::new();

    // Start: the turn has no group message yet, so the caller posts one and
    // keeps the ts it got back.
    let start = detached_render(&mut entries, None, started("t-1", "cargo test"));
    assert_eq!(start, DetachedRender::Post);

    let group_ts = "1712345678.000100";

    // Finish: rewrite the message the start created.
    let finish = detached_render(&mut entries, Some(group_ts), finished("t-1", true));
    assert_eq!(
        finish,
        DetachedRender::Edit(group_ts.to_string()),
        "the finish must rewrite the group the start created, never post"
    );

    assert_eq!(entries.len(), 1, "one task, one row, one message");
    let GroupEntry::Tool { status, .. } = &entries[0] else {
        panic!("a detached task is a tool row");
    };
    assert_eq!(*status, Some(true), "the row settled in place");
    assert_eq!(
        entries[0].display_name(),
        "detached task finished: cargo test",
        "the same row now reads finished"
    );
}

#[test]
fn a_start_on_an_existing_group_edits_it_instead_of_posting() {
    // The common shape: the turn already opened a tool group, so the row is
    // appended to it and the existing ts is rewritten.
    let mut entries: Vec<GroupEntry> = Vec::new();
    let action = detached_render(
        &mut entries,
        Some("1712345678.000200"),
        started("t-2", "npm build"),
    );
    assert_eq!(
        action,
        DetachedRender::Edit("1712345678.000200".to_string()),
        "a group that already exists must be edited, not replaced"
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].display_name(),
        "detached task running: npm build"
    );
}

#[test]
fn two_detached_tasks_settle_by_their_own_id_not_their_label() {
    let mut entries: Vec<GroupEntry> = Vec::new();
    let ts = "1712345678.000300";

    // Two tasks with the SAME label, which is the case a label match breaks.
    detached_render(&mut entries, Some(ts), started("t-1", "cargo test"));
    detached_render(&mut entries, Some(ts), started("t-2", "cargo test"));

    // The first one finishes: only its row may settle.
    detached_render(&mut entries, Some(ts), finished("t-1", true));

    let GroupEntry::Tool { status: first, .. } = &entries[0] else {
        panic!("row 0 is a tool row");
    };
    let GroupEntry::Tool { status: second, .. } = &entries[1] else {
        panic!("row 1 is a tool row");
    };
    assert_eq!(*first, Some(true), "the finished task settled");
    assert_eq!(*second, None, "the still-running task must not settle");
    assert_eq!(
        entries[1].display_name(),
        "detached task running: cargo test",
        "the second task keeps its running label"
    );
}

#[test]
fn an_orphan_finish_renders_nothing_and_invents_no_row() {
    let mut entries: Vec<GroupEntry> = Vec::new();
    let action = detached_render(
        &mut entries,
        Some("1712345678.000400"),
        finished("never-started-here", true),
    );
    assert_eq!(action, DetachedRender::Skip, "no row to settle, no edit");
    assert!(entries.is_empty(), "an orphan finish must not create a row");
}

#[test]
fn a_finish_never_posts_even_when_the_row_exists_but_the_block_does_not() {
    // The row landed but the group post failed (or never ran). Posting the
    // settled state here would be the second bubble this fix removes.
    let mut entries: Vec<GroupEntry> = Vec::new();
    detached_render(&mut entries, None, started("t-3", "cargo build"));
    assert_eq!(entries.len(), 1);

    let action = detached_render(&mut entries, None, finished("t-3", false));
    assert_eq!(
        action,
        DetachedRender::Skip,
        "a finish with no block must not post one"
    );

    let GroupEntry::Tool { status, .. } = &entries[0] else {
        panic!("a detached task is a tool row");
    };
    assert_eq!(*status, Some(false), "the row still records the failure");
    assert_eq!(
        entries[0].display_name(),
        "detached task failed: cargo build",
        "the label reports the failure even when the message is gone"
    );
}

#[test]
fn an_ordinary_tool_row_is_unaffected_by_the_label() {
    // Every existing render depends on a plain tool row keeping its own name.
    let plain = GroupEntry::Tool {
        name: "bash".to_string(),
        context: " cargo test".to_string(),
        status: Some(true),
        task_id: None,
    };
    assert_eq!(plain.display_name(), "bash");
}
