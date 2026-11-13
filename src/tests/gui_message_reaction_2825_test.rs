//! Reactions on a stored transcript row (#2825).
//!
//! The window has no headless harness, so the wiring is read from source, the
//! way the other gui tests do it. The rule that cannot be bent is that a
//! reaction is a decoration on a row, never a precondition for drawing it: the
//! first transcript of a fresh build loads while `spawn_bootstrap` is still
//! running migrations on its own thread, so the sidecar table can be one
//! migration behind the window. A read that cannot answer is an empty map.
//! Propagating it blanked the pane with "no such table: message_reactions"
//! instead of the conversation.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const REACTION_REPO: &str = include_str!("../db/repository/message_reaction.rs");
const MIGRATION: &str = include_str!("../migrations/20261106000001_add_message_reactions.sql");

/// The read is infallible at the call site, so a pane cannot lose its rows to
/// a table that has not been created yet.
#[test]
fn the_reaction_read_never_fails_a_transcript_load() {
    let read = TRANSCRIPT
        .split("async fn reactions_for(")
        .nth(1)
        .expect("one function builds the reaction map")
        .split("pub async fn toggle_reaction(")
        .next()
        .unwrap_or_default();
    assert!(
        !read.contains("anyhow::Result"),
        "the read answers with a map, not with a result the pane has to handle"
    );
    assert!(
        read.contains("crate::logging::error(&format!(\"reactions load: {e:#}\"))"),
        "a failure is logged with its whole chain"
    );
    assert!(
        read.contains("HashMap::new()"),
        "and the transcript still draws, without chips"
    );
    assert!(
        !TRANSCRIPT.contains("reactions_for(&db, &rows).await?"),
        "no call site may propagate the reaction read into the load"
    );
}

/// The tolerance stays in the window, not in the ledger: a repository that
/// swallowed a missing table would hide a real schema problem from every
/// other caller.
#[test]
fn the_repository_still_reports_a_failed_read() {
    let read = REACTION_REPO
        .split("pub async fn for_messages(")
        .nth(1)
        .expect("the repository owns the read")
        .split("pub async fn for_message(")
        .next()
        .unwrap_or_default();
    assert!(
        read.contains("Result<HashMap<Uuid, Vec<(String, usize)>>>"),
        "the ledger read reports its own failure"
    );
    assert!(
        read.contains("Failed to read message reactions"),
        "with the context that names it"
    );
}

/// One reaction per row and picture, so a second click on the same crab is the
/// same reaction and the toggle takes it back.
#[test]
fn the_table_is_keyed_by_row_and_picture() {
    assert!(
        MIGRATION.contains("PRIMARY KEY (message_id, emoji)"),
        "the pair is the key"
    );
    assert!(
        MIGRATION.contains("ON DELETE CASCADE"),
        "pruning a message takes its reactions with it"
    );
    let toggle = REACTION_REPO
        .split("pub async fn toggle(")
        .nth(1)
        .expect("one function owns the gesture")
        .split("pub async fn for_messages(")
        .next()
        .unwrap_or_default();
    let delete = toggle
        .find("DELETE FROM message_reactions")
        .expect("the toggle tries the take-back first");
    let insert = toggle
        .find("INSERT INTO message_reactions")
        .expect("and adds it when it was not there");
    assert!(
        delete < insert,
        "the delete decides which way the click went before anything is written"
    );
}
