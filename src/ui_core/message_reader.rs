//! What a long prompt does in the conversation column (#3064).
//!
//! A message the reader typed is stored whole and stays whole: the agent reads
//! every line, copy and edit read every line, and the document reader shows
//! every line. The only question here is how much of it the ROW draws. Past a
//! bound a pasted log swallows the conversation, and the transcript has no
//! virtualization: one long body is thousands of elements rebuilt on every
//! repaint, the same cost #2805 bounded on generated working-out.
//!
//! So a long prompt folds to its opening lines and announces the rest with a
//! document card. That card opens the same text in the document reader, which
//! is the comfortable place to read it at full width (#3053). These are the
//! rules that decide the fold, pure so they are testable without a window.

/// Lines a folded prompt keeps in the row.
pub const FOLD_KEEP_LINES: usize = 20;

/// Bytes a folded prompt keeps, so one unbroken line still folds.
///
/// A line bound alone leaves the worst case untouched: a single 60 KB line is
/// one line and passes any count, while costing the frame exactly what a
/// thousand short ones cost. This is where that head stops.
pub const FOLD_KEEP_BYTES: usize = 2_000;

/// Lines past which a prompt folds at all.
///
/// Below the bound nothing changes: a prompt that reads in the column keeps
/// its click free, so a stray click on a short question never throws a modal
/// over the chat.
pub const FOLD_AFTER_LINES: usize = 30;

/// Bytes past which a prompt folds even when it is a single line.
pub const FOLD_BYTES: usize = 4_000;

/// Characters of the first line a reader's header prints.
pub const TITLE_CHARS: usize = 60;

/// Characters a card's document name is built from.
pub const NAME_CHARS: usize = 40;

/// Whether a body is long enough to fold into a document card.
///
/// Both bounds count: a body of two hundred short lines folds on the line
/// count, and a body of one 60 KB line folds on the byte count.
pub fn opens_in_reader(content: &str) -> bool {
    content.lines().count() > FOLD_AFTER_LINES || content.len() > FOLD_BYTES
}

/// A prompt the row will not draw whole: the head it keeps, and what the
/// document card says about the rest.
#[derive(Clone, Debug)]
pub struct Fold {
    /// The opening lines, cut on a character boundary and with the trailing
    /// blank run dropped, so the head never ends on empty space.
    pub head: String,
    /// The card's name: the message's own opening words and `.md`.
    pub name: String,
    /// Lines and size, as the card's second line reads them.
    pub summary: String,
}

/// The fold a body takes, or `None` when it draws whole.
pub fn fold(content: &str) -> Option<Fold> {
    if !opens_in_reader(content) {
        return None;
    }
    Some(Fold {
        head: head(content),
        name: document_name(content),
        summary: summary(content),
    })
}

/// The opening lines of a body, at most [`FOLD_KEEP_LINES`] lines and
/// [`FOLD_KEEP_BYTES`] bytes.
///
/// Built character by character rather than sliced: a byte index lands inside
/// a multi-byte character on the first accented line, which is #2271, and the
/// head is drawn on every frame of a row that already costs too much.
fn head(content: &str) -> String {
    let mut out = String::new();
    let mut lines = 0usize;
    for ch in content.chars() {
        out.push(ch);
        if ch == '\n' {
            lines += 1;
            if lines == FOLD_KEEP_LINES {
                break;
            }
        }
        if out.len() >= FOLD_KEEP_BYTES {
            break;
        }
    }
    out.trim_end().to_string()
}

/// What the card calls the document: the message's own opening words, cut to
/// [`NAME_CHARS`] and joined with dashes, then `.md`.
///
/// Named after the message instead of after the row id, so a card reads as the
/// thing that was pasted and two cards in one turn are told apart at a glance.
pub fn document_name(content: &str) -> String {
    let opening = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut name = String::new();
    let mut used = 0usize;
    let mut dash = false;
    for ch in opening.chars() {
        if used == NAME_CHARS {
            break;
        }
        if ch.is_alphanumeric() {
            name.push(ch);
            used += 1;
            dash = false;
        } else if !dash && !name.is_empty() {
            name.push('-');
            dash = true;
        }
    }
    let name = name.trim_end_matches('-');
    if name.is_empty() {
        "message.md".to_string()
    } else {
        format!("{name}.md")
    }
}

/// How big the folded part is, in the words the card prints: a line count and
/// a size. Both, because either one alone lies about a body of the other shape.
fn summary(content: &str) -> String {
    let lines = content.lines().count();
    let bytes = content.len();
    let size = if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    };
    if lines > 1 {
        format!("{lines} lines \u{00b7} {size}")
    } else {
        size
    }
}

/// What the reader's header calls a message: the first line that carries
/// text, shortened, so two long prompts are told apart by their own opening
/// words instead of both reading "message".
pub fn title(content: &str) -> String {
    let first = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let fitted = crate::ui_core::session_preview::fit(first, TITLE_CHARS);
    if fitted.trim().is_empty() {
        "message".to_string()
    } else {
        fitted
    }
}
