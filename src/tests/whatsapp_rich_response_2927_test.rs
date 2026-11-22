//! A markdown table leaves WhatsApp as a grid, not a line of pipes (#2927).
//!
//! The outbound path only ever set `conversation`, so every table the model
//! wrote arrived as `| a | b |` with dashes under it. The wire format carries a
//! table natively: `Message.richResponseMessage` (field 97), whose submessages
//! are text, table rows of plain string cells, and code blocks. These tests pin
//! the construction and, more importantly, the two things that keep it
//! harmless while nobody has confirmed the client renders it: a reply with no
//! table never takes the path, and a table whose reply is too long for one
//! message stays plain so the chunked send can split it.

#![cfg(feature = "whatsapp")]

use crate::channels::whatsapp::rich_response::{RICH_MAX, build, summary};

const TABLE: &str = "\
Here is the comparison:

| Policy | Behavior |
|---|---|
| **ask** | prompt only for dangerous tools |
| `allow` | no prompt |
";

/// The header row is marked as a heading, and every cell is plain text.
#[test]
fn a_table_becomes_one_table_submessage_with_a_heading_row() {
    let message = build(TABLE).expect("a reply with a table takes the rich path");
    let rich = message
        .rich_response_message
        .as_option()
        .expect("the reply carries the rich field");
    assert_eq!(
        rich.message_type,
        Some(waproto::whatsapp::AIRichResponseMessageType::AiRichResponseTypeStandard)
    );
    let tables: Vec<_> = rich
        .submessages
        .iter()
        .filter_map(|sub| sub.table_metadata.as_option())
        .collect();
    assert_eq!(tables.len(), 1, "one table, one submessage");
    let rows = &tables[0].rows;
    assert_eq!(rows.len(), 3, "header plus two body rows");
    assert_eq!(rows[0].items, vec!["Policy", "Behavior"]);
    assert_eq!(rows[0].is_heading, Some(true));
    assert_eq!(
        rows[1].items,
        vec!["ask", "prompt only for dangerous tools"]
    );
    assert_eq!(rows[1].is_heading, None, "only the header is a heading");
    assert_eq!(rows[2].items, vec!["allow", "no prompt"]);
}

/// The prose around a table stays prose, in the order the model wrote it, so
/// the answer does not lose its lead-in.
#[test]
fn the_text_around_a_table_rides_along_in_order() {
    let message = build(TABLE).expect("the reply is rich");
    let rich = message.rich_response_message.as_option().unwrap();
    let kinds: Vec<_> = rich
        .submessages
        .iter()
        .map(|sub| sub.message_type)
        .collect();
    assert_eq!(
        kinds,
        vec![
            Some(waproto::whatsapp::AIRichResponseSubMessageType::AiRichResponseText),
            Some(waproto::whatsapp::AIRichResponseSubMessageType::AiRichResponseTable),
        ],
        "the lead-in comes before the grid"
    );
    assert!(
        rich.submessages[0]
            .message_text
            .as_deref()
            .unwrap_or_default()
            .contains("Here is the comparison:")
    );
}

/// A fenced block beside the table keeps its language and its content, and the
/// backticks do not reach the reader.
#[test]
fn a_fenced_block_becomes_a_code_submessage() {
    let text = format!("{TABLE}\n```rust\nlet x = 1;\n```\n");
    let message = build(&text).expect("the reply is rich");
    let rich = message.rich_response_message.as_option().unwrap();
    let code = rich
        .submessages
        .iter()
        .find_map(|sub| sub.code_metadata.as_option())
        .expect("the fence is a code submessage");
    assert_eq!(code.code_language.as_deref(), Some("rust"));
    assert_eq!(code.code_blocks.len(), 1);
    assert_eq!(
        code.code_blocks[0].code_content.as_deref(),
        Some("let x = 1;")
    );
}

/// The gate that makes this safe to ship unverified: a reply without a table
/// keeps the plain send that works today.
#[test]
fn prose_and_code_alone_stay_plain_text() {
    assert!(build("just a sentence, nothing structured").is_none());
    assert!(
        build("```rust\nlet x = 1;\n```").is_none(),
        "a code block alone already reads fine as text"
    );
    assert!(
        build("| not a table\n| just pipes in prose").is_none(),
        "a pipe line with no separator row is not a table"
    );
}

/// A reply too long for one message stays plain: the chunked send is the only
/// path that can split it, and losing the tail of an answer is worse than pipes.
#[test]
fn a_reply_past_the_limit_stays_plain() {
    let mut text = TABLE.to_string();
    while text.len() <= RICH_MAX {
        text.push_str("padding line to push this reply past the rich limit\n");
    }
    assert!(build(&text).is_none());
}

/// The log line names what went out, so a blank bubble on a real account can be
/// traced to this path instead of guessed at.
#[test]
fn the_summary_counts_what_the_message_carries() {
    let text = format!("{TABLE}\n```sh\necho hi\n```\n");
    assert_eq!(summary(&text), "1 table(s), 1 code block(s)");
    assert_eq!(summary(TABLE), "1 table(s), 0 code block(s)");
}

/// One table written without outer pipes is still a table, and its cells are
/// the words rather than the emphasis they were typed with.
#[test]
fn a_table_without_outer_pipes_parses() {
    let message = build("Name | State\n--- | ---\n**bold** | fine").expect("it is a table");
    let rich = message.rich_response_message.as_option().unwrap();
    let table = rich
        .submessages
        .iter()
        .find_map(|sub| sub.table_metadata.as_option())
        .expect("one table");
    assert_eq!(table.rows[0].items, vec!["Name", "State"]);
    assert_eq!(table.rows[1].items, vec!["bold", "fine"]);
}
