//! Regression tests for #3091 — `session_search` formatted `messages.content`
//! verbatim, so a hit came back wrapped in `<!-- reasoning -->` markers and
//! `<!-- tools-v2: ... -->` ledgers. The result lands in the model's context,
//! which taught it the storage format, fed it its own deliberation as history,
//! and inflated every hit with text the display paths fold away.

use crate::brain::tools::session_search::{SessionSearchTool, visible_text};
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::db::Database;
use crate::db::models::{Message, Session};
use crate::db::repository::{MessageRepository, SessionRepository};
use serde_json::json;
use uuid::Uuid;

const OPEN: &str = "<!-- reasoning -->";
const CLOSE: &str = "<!-- /reasoning -->";

async fn setup() -> (Database, SessionSearchTool) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let tool = SessionSearchTool::new(db.pool().clone());
    (db, tool)
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4())
}

async fn seed(db: &Database, title: &str, rows: Vec<(&str, String)>) -> Session {
    let session = Session::new(Some(title.to_string()), Some("m".to_string()), None);
    let srepo = SessionRepository::new(db.pool().clone());
    let mrepo = MessageRepository::new(db.pool().clone());
    srepo.create(&session).await.unwrap();
    for (i, (role, content)) in rows.into_iter().enumerate() {
        mrepo
            .create(&Message::new(
                session.id,
                role.to_string(),
                content,
                i as i32 + 1,
            ))
            .await
            .unwrap();
    }
    session
}

#[test]
fn keeps_the_answer_and_drops_the_reasoning() {
    let content = format!("{OPEN}\nThe user asks about QRIS.\n{CLOSE}\nHere is the fix.");
    assert_eq!(visible_text(&content), "Here is the fix.");
}

#[test]
fn drops_a_replay_ledger_with_its_json_body() {
    // The v2 payload embeds arbitrary stdout, rustc span arrows included.
    let content =
        "Before.\n<!-- tools-v2: [{\"d\":\"cargo test --> src/foo.rs:10\",\"s\":true}] -->\nAfter.";
    let visible = visible_text(content);
    assert!(!visible.contains("tools-v2"), "ledger survived: {visible}");
    assert!(
        !visible.contains("src/foo.rs"),
        "ledger body survived: {visible}"
    );
    assert!(visible.contains("Before.") && visible.contains("After."));
}

#[test]
fn drops_a_phantom_blocked_section() {
    let content = "Real answer.\n<!-- phantom_blocked=1 -->\nI will now call the tool.\n<!-- /phantom_blocked=1 -->\n";
    assert_eq!(visible_text(content), "Real answer.");
}

#[test]
fn an_unclosed_reasoning_block_takes_the_tail() {
    // A truncated write must not dump half a chain as the answer.
    let content = "Answer first.\n<!-- reasoning -->\nhalf a chain";
    assert_eq!(visible_text(content), "Answer first.");
}

#[test]
fn a_quoted_marker_does_not_cut_the_answer() {
    // #1587: the model quotes markers mid-sentence when it reads its own
    // history. Only a marker at the start of a line opens a block.
    let content =
        "We fixed the <!-- reasoning --> bug, it was the marker.\nThe rest of the answer.";
    let visible = visible_text(content);
    assert!(
        visible.contains("The rest of the answer."),
        "answer cut at a quoted marker: {visible}"
    );
}

#[tokio::test]
async fn search_returns_visible_text_only() {
    let (db, tool) = setup().await;
    seed(
        &db,
        "telegram",
        vec![(
            "assistant",
            format!("{OPEN}\nsecret deliberation about qris\n{CLOSE}\nHere is the fix for qris."),
        )],
    )
    .await;

    let result = tool
        .execute(
            json!({"operation": "search", "query": "qris", "n": 5}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(result.success, "error: {:?}", result.error);
    assert!(
        result.output.contains("Here is the fix for qris."),
        "answer missing: {}",
        result.output
    );
    assert!(
        !result.output.contains("secret deliberation"),
        "reasoning leaked: {}",
        result.output
    );
    assert!(
        !result.output.contains("<!--"),
        "markers leaked: {}",
        result.output
    );
}

#[tokio::test]
async fn a_hit_inside_reasoning_says_so_instead_of_leaking_it() {
    let (db, tool) = setup().await;
    seed(
        &db,
        "telegram",
        vec![(
            "assistant",
            format!("{OPEN}\nsecret deliberation\n{CLOSE}\nHere is the answer."),
        )],
    )
    .await;

    let result = tool
        .execute(
            json!({"operation": "search", "query": "deliberation", "n": 5}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(result.success, "error: {:?}", result.error);
    assert!(
        result.output.contains("reasoning blocks or replay ledgers"),
        "expected the hidden-match note: {}",
        result.output
    );
    assert!(
        !result.output.contains("secret deliberation"),
        "reasoning leaked: {}",
        result.output
    );
}

#[tokio::test]
async fn tail_names_a_row_with_no_visible_text() {
    let (db, tool) = setup().await;
    seed(
        &db,
        "telegram",
        vec![
            ("user", "go".to_string()),
            ("assistant", format!("{OPEN}\nthinking only\n{CLOSE}\n")),
        ],
    )
    .await;

    let result = tool
        .execute(json!({"operation": "tail", "n": 5}), &ctx())
        .await
        .unwrap();
    assert!(result.success, "error: {:?}", result.error);
    assert!(
        result.output.contains("(no visible text)"),
        "blank row not named: {}",
        result.output
    );
    assert!(
        !result.output.contains("thinking only"),
        "reasoning leaked into tail: {}",
        result.output
    );
    assert!(
        !result.output.contains("<!--"),
        "markers leaked into tail: {}",
        result.output
    );
}
