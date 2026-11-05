//! Draft-preserving input history shared by the terminal and GUI composers.
//!
//! The frontend owns its key handling and persistence. This module owns only
//! the recall invariant: entering history stashes the current draft, and moving
//! past the newest entry restores that exact draft.

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
    if index.is_none() {
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
    let excess = entries.len().saturating_sub(500);
    if excess > 0 {
        entries.drain(..excess);
    }
}
