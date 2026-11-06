//! Regression for #1522: persisted ledgers and reasoning blocks are cleaned
//! from loaded rows for every provider, CLI providers included.
//!
//! A session at ~168K on an API provider jumped to ~705K on switching to
//! claude-cli and the first request was refused as too long. Both loaded
//! the same 85 rows after the compaction marker; the API path counted them
//! at 141K tokens, the CLI path at 679K. The loader stripped the
//! `<!-- tools-v2 -->` replay ledgers and `<!-- reasoning -->` blocks for
//! API providers only, under a comment claiming the CLI subprocess never
//! saw that content. The CLI prompt builder flattens every text block
//! verbatim, so it saw all of it: of 2.6M characters, 1.9M were ledgers and
//! 0.4M were reasoning.

use chrono::Utc;
use uuid::Uuid;

use crate::brain::agent::service::context_rows::clean_rows_for_llm;
use crate::db::models::Message as DbMessage;

fn row(role: &str, content: &str, thinking: Option<&str>) -> DbMessage {
    DbMessage {
        forward: None,
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        sequence: 0,
        created_at: Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: thinking.map(String::from),
        duration_secs: None,
    }
}

/// An assistant row the way the tool loop persists it: a reasoning block,
/// the visible answer, and a replay ledger many times larger than both.
fn ledger_row() -> DbMessage {
    let ledger_entry = r#"{"d":"bash: cargo test --all-features","i":{"command":"cargo test"},"o":"running 8300 tests ... test result: ok"}"#;
    let ledger = format!(
        "<!-- tools-v2: [{}] -->",
        std::iter::repeat_n(ledger_entry, 40)
            .collect::<Vec<_>>()
            .join(",")
    );
    let reasoning = format!(
        "<!-- reasoning -->\n{}\n<!-- /reasoning -->\n\n",
        "The user wants the suite run before the edit lands. ".repeat(30)
    );
    row(
        "assistant",
        &format!("{reasoning}Suite green, landing the edit now.\n\n{ledger}"),
        None,
    )
}

fn visible(rows: &[DbMessage]) -> String {
    rows.iter()
        .map(|r| r.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ledgers_and_reasoning_are_dropped_without_a_thinking_hoist() {
    let mut rows = vec![ledger_row()];
    let raw_len = rows[0].content.len();

    clean_rows_for_llm(&mut rows, false);

    let text = visible(&rows);
    assert_eq!(text.trim(), "Suite green, landing the edit now.");
    assert!(
        rows[0].thinking.is_none(),
        "no hoist: the reasoning is gone, not moved"
    );
    assert!(
        text.len() * 10 < raw_len,
        "the cleaned row is a small fraction of the persisted one ({} of {raw_len})",
        text.len()
    );
}

#[test]
fn a_thinking_hoist_keeps_the_reasoning_in_the_thinking_column() {
    let mut rows = vec![ledger_row()];

    clean_rows_for_llm(&mut rows, true);

    assert_eq!(visible(&rows).trim(), "Suite green, landing the edit now.");
    let thinking = rows[0].thinking.as_deref().expect("reasoning hoisted");
    assert!(thinking.starts_with("The user wants the suite run"));
    assert!(
        !thinking.contains("<!--"),
        "markers never reach the thinking column"
    );
}

#[test]
fn user_rows_and_plain_rows_pass_through_untouched() {
    let mut rows = vec![
        row("user", "please run the suite", None),
        row("assistant", "Running it.", Some("kept as is")),
    ];
    clean_rows_for_llm(&mut rows, false);
    assert_eq!(rows[0].content, "please run the suite");
    assert_eq!(rows[1].content, "Running it.");
    assert_eq!(rows[1].thinking.as_deref(), Some("kept as is"));
}

#[test]
fn the_compaction_banner_is_removed_from_the_marker_row() {
    let mut rows = vec![row(
        "user",
        "[CONTEXT COMPACTION marker line]\n\nThe summary body.",
        None,
    )];
    clean_rows_for_llm(&mut rows, false);
    assert_eq!(rows[0].content, "The summary body.");
}

/// The loader cleans rows for every provider and hoists reasoning only for
/// API models that need it back as `reasoning_content`.
#[test]
fn the_loader_no_longer_exempts_cli_providers() {
    const SRC: &str = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        !SRC.contains("CLI providers MUST keep markers"),
        "the exemption and its rationale are gone"
    );
    let call = SRC
        .find("context_rows::clean_rows_for_llm(&mut db_messages, preserve_thinking)")
        .expect("the loader cleans through context_rows");
    let before = &SRC[call.saturating_sub(400)..call];
    assert!(
        before.contains("let preserve_thinking = !is_cli_provider"),
        "a CLI provider gets no thinking hoist"
    );
}
