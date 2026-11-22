//! Rich replies for tables and fenced code (#2927).
//!
//! A markdown table reaches a WhatsApp chat as a line of pipe characters,
//! because the outbound path only ever sets `conversation`. The wire format has
//! a field for this: `Message.richResponseMessage` (field 97), whose
//! submessages carry a table as rows of plain string cells and a code block as
//! its language plus its content. `waproto` already ships the whole shape, so
//! this is construction, not protocol work.
//!
//! Whether the receiving client draws a table sent by a third-party account is
//! NOT established. Every rendering seen in the wild is on a Meta AI reply, and
//! the server may strip the field for anyone else. That risk decides the shape
//! of this module:
//!
//! - Only a reply that actually carries a table takes this path. Prose, and a
//!   reply whose only structure is a code block, keep the plain send that works
//!   today, so the common case cannot regress.
//! - A failed send falls back to the plain-text chunks the caller already
//!   builds, so a refusal costs nothing.
//! - The send is logged with what it carried, so a blank bubble on a real
//!   account can be traced to this path instead of guessed at.
//! - A reply over [`RICH_MAX`] bytes stays plain: the rich message is one
//!   message, and the chunked path is the only one that can split it.
//!
//! The parser is deliberately local. `channels::telegram::rich` has a full GFM
//! table parser, but it produces an inline AST for Telegram's renderer and its
//! cells are `Vec<Inline>`; a WhatsApp cell is a plain string. Reusing it would
//! mean a cell flattener and a cross-channel dependency for one caller.

use waproto::whatsapp::ai_rich_response_code_metadata::AIRichResponseCodeBlock;
use waproto::whatsapp::ai_rich_response_table_metadata::AIRichResponseTableRow;
use waproto::whatsapp::{
    AIRichResponseCodeMetadata, AIRichResponseMessage, AIRichResponseMessageType,
    AIRichResponseSubMessage, AIRichResponseSubMessageType, AIRichResponseTableMetadata, Message,
};

/// The largest reply that goes out as one rich message. Past this the reply is
/// chunked as text, which is the only path that can split one answer.
pub(crate) const RICH_MAX: usize = 4000;

/// One piece of a reply, in the order the model wrote it.
#[derive(Debug, PartialEq)]
enum Piece {
    Text(String),
    Table(Table),
    Code {
        language: Option<String>,
        body: String,
    },
}

/// A markdown table, already reduced to the plain cells WhatsApp carries.
#[derive(Debug, PartialEq)]
struct Table {
    heading: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// Build the rich reply for `text`, or `None` when it has no table.
///
/// `None` is the answer for prose, for a reply whose only structure is a code
/// block, and for anything past [`RICH_MAX`]: all three keep the plain send.
pub(crate) fn build(text: &str) -> Option<Message> {
    if text.len() > RICH_MAX {
        return None;
    }
    let pieces = split(text);
    if !pieces.iter().any(|piece| matches!(piece, Piece::Table(_))) {
        return None;
    }
    let submessages: Vec<AIRichResponseSubMessage> =
        pieces.into_iter().filter_map(submessage).collect();
    if submessages.is_empty() {
        return None;
    }
    Some(Message {
        rich_response_message: AIRichResponseMessage {
            message_type: Some(AIRichResponseMessageType::AiRichResponseTypeStandard),
            submessages,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    })
}

/// What a built reply carried, for the log line beside the send.
pub(crate) fn summary(text: &str) -> String {
    let pieces = split(text);
    let tables = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Table(_)))
        .count();
    let code = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Code { .. }))
        .count();
    format!("{tables} table(s), {code} code block(s)")
}

fn submessage(piece: Piece) -> Option<AIRichResponseSubMessage> {
    match piece {
        Piece::Text(text) => {
            let text = text.trim_matches('\n').to_string();
            if text.trim().is_empty() {
                return None;
            }
            Some(AIRichResponseSubMessage {
                message_type: Some(AIRichResponseSubMessageType::AiRichResponseText),
                message_text: Some(text),
                ..Default::default()
            })
        }
        Piece::Table(table) => Some(AIRichResponseSubMessage {
            message_type: Some(AIRichResponseSubMessageType::AiRichResponseTable),
            table_metadata: AIRichResponseTableMetadata {
                rows: table_rows(&table),
                title: None,
            }
            .into(),
            ..Default::default()
        }),
        Piece::Code { language, body } => Some(AIRichResponseSubMessage {
            message_type: Some(AIRichResponseSubMessageType::AiRichResponseCode),
            code_metadata: AIRichResponseCodeMetadata {
                code_language: language,
                code_blocks: vec![AIRichResponseCodeBlock {
                    highlight_type: None,
                    code_content: Some(body),
                }],
            }
            .into(),
            ..Default::default()
        }),
    }
}

/// The header row first, marked as the heading, then the body rows.
fn table_rows(table: &Table) -> Vec<AIRichResponseTableRow> {
    let mut rows = Vec::with_capacity(table.rows.len() + 1);
    if !table.heading.is_empty() {
        rows.push(AIRichResponseTableRow {
            items: table.heading.clone(),
            is_heading: Some(true),
        });
    }
    rows.extend(table.rows.iter().map(|row| AIRichResponseTableRow {
        items: row.clone(),
        is_heading: None,
    }));
    rows
}

/// Split markdown into text, tables and fenced code, in order.
fn split(text: &str) -> Vec<Piece> {
    let lines: Vec<&str> = text.lines().collect();
    let mut pieces = Vec::new();
    let mut text_run: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(language) = fence_language(line) {
            // A fence runs to its closing fence, or to the end when the model
            // never closed it: a half-written block still reads as code.
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !is_fence(lines[i]) {
                body.push(lines[i]);
                i += 1;
            }
            i += 1;
            flush(&mut text_run, &mut pieces);
            pieces.push(Piece::Code {
                language,
                body: body.join("\n"),
            });
            continue;
        }
        if let Some((table, next)) = parse_table(&lines, i) {
            flush(&mut text_run, &mut pieces);
            pieces.push(Piece::Table(table));
            i = next;
            continue;
        }
        text_run.push(line);
        i += 1;
    }
    flush(&mut text_run, &mut pieces);
    pieces
}

fn flush(text_run: &mut Vec<&str>, pieces: &mut Vec<Piece>) {
    if text_run.is_empty() {
        return;
    }
    let text = text_run.join("\n");
    text_run.clear();
    pieces.push(Piece::Text(text));
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// The language a fence names, if any. `Some(None)` still means "this is a
/// fence": the distinction is the label after the backticks.
fn fence_language(line: &str) -> Option<Option<String>> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("```")?;
    let language = rest.trim();
    if language.is_empty() {
        Some(None)
    } else {
        Some(Some(language.to_string()))
    }
}

/// Parse a table starting at `start`, returning it and the line after it.
fn parse_table(lines: &[&str], start: usize) -> Option<(Table, usize)> {
    let heading = lines.get(start)?;
    let separator = lines.get(start + 1)?;
    if !looks_like_row(heading) || !is_separator(separator) {
        return None;
    }
    let heading = cells(heading);
    let mut rows = Vec::new();
    let mut i = start + 2;
    while i < lines.len() && looks_like_row(lines[i]) {
        rows.push(cells(lines[i]));
        i += 1;
    }
    Some((Table { heading, rows }, i))
}

/// A line that could be a table row: has a pipe and is not blank.
fn looks_like_row(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.contains('|')
}

/// A separator row: every cell is dashes with optional alignment colons.
fn is_separator(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let core = cell.trim_start_matches(':').trim_end_matches(':');
            !core.is_empty() && core.chars().all(|ch| ch == '-')
        })
}

/// Split a pipe row into plain cells, dropping the empty cells that flank a row
/// written with outer pipes. Emphasis markers are removed here: a WhatsApp cell
/// is a plain string, so `**ask**` would otherwise reach the reader with its
/// asterisks on.
fn cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|cell| plain(cell.trim())).collect()
}

/// A cell's text without the markdown emphasis it was written with.
fn plain(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut rest = cell;
    while let Some(at) = rest.find("**") {
        out.push_str(&rest[..at]);
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out.replace('`', "").trim().to_string()
}
