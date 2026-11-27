//! Draft-preserving input history shared by the terminal and GUI composers.
//!
//! The frontend owns its key handling and persistence. This module owns only
//! the recall invariant: entering history stashes the current draft, and moving
//! past the newest entry restores that exact draft.

/// Entries kept in the submitted history. One number for both surfaces, so a
/// terminal left open and a window cannot cap the same file differently.
pub const HISTORY_CAP: usize = 500;

/// Which way a vertical key goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertical {
    Up,
    Down,
}

/// What a vertical key does in a composer: it moves the caret inside the
/// text, or it moves the history to another entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAction {
    Caret,
    History,
}

/// Who owns the vertical key right now.
///
/// The caret keeps the key while it still has somewhere to go: Up moves the
/// caret until it sits at the very start of the text, and only a caret there
/// hands the key to the history. Down is the mirror. A recalled entry is
/// walked the same way a draft is, line by line, and the history takes over
/// once the caret is out of the text.
///
/// Both surfaces ask this, so the terminal and the window walk the history
/// the same way instead of each inventing a rule (#3188).
///
/// The interceptors that come before the answer are their own: the terminal
/// hands the key to a queued message and to a stashed draft before the
/// history sees it, and the window hands it to a follow-up offer above the
/// field (#3101). Neither surface lets those interceptors take the key from a
/// draft that still has rows of its own to walk.
pub fn vertical_action(caret: usize, content_len: usize, direction: Vertical) -> VerticalAction {
    let caret_moves = match direction {
        Vertical::Up => caret > 0,
        Vertical::Down => caret < content_len,
    };
    if caret_moves {
        VerticalAction::Caret
    } else {
        VerticalAction::History
    }
}

/// Recall an older entry. Returns false when there is no history to enter.
pub fn input_history_up(
    entries: &[String],
    buffer: &mut String,
    index: &mut Option<usize>,
    draft: &mut String,
) -> bool {
    if entries.is_empty() {
        return false;
    }
    let next = match *index {
        None => entries.len() - 1,
        Some(current) if current < entries.len() => current.saturating_sub(1),
        Some(_) => return false,
    };
    // Stash what the user was writing, but never overwrite a draft that is
    // already parked there with an empty buffer. Esc twice clears the field
    // and leaves the text in the stash; entering history from that empty
    // field must keep it, or the next Down restores nothing (#2494).
    if index.is_none() && !buffer.is_empty() {
        *draft = buffer.clone();
    }
    *index = Some(next);
    *buffer = entries[next].clone();
    true
}

/// Recall a newer entry, or restore the stashed draft after the newest one.
pub fn input_history_down(
    entries: &[String],
    buffer: &mut String,
    index: &mut Option<usize>,
    draft: &mut String,
) -> bool {
    let Some(current) = *index else {
        return false;
    };
    if current + 1 < entries.len() {
        let next = current + 1;
        *index = Some(next);
        *buffer = entries[next].clone();
    } else {
        *index = None;
        *buffer = std::mem::take(draft);
    }
    true
}

/// Record submitted text with consecutive deduplication and the TUI's cap.
pub fn record_input_history(entries: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() || entries.last().is_some_and(|last| last == text) {
        return;
    }
    entries.push(text.to_string());
    let excess = entries.len().saturating_sub(HISTORY_CAP);
    if excess > 0 {
        entries.drain(..excess);
    }
}
