//! Per-row card data for the sessions dialog, and the two transient prompts
//! the row actions open (#2737).
//!
//! The GUI rail shows each session as a card: a preview of its newest
//! message, a plan progress bar, a cost, and a glyph when a plan is waiting
//! for a hand. The TUI keeps its single-column tree (the owner is explicit
//! that full-screen rows beat a sidebar), so the same facts ride on the row
//! itself. Everything here is state plus pure helpers; the repository reads
//! live in `messaging.rs` and the drawing in `render::sessions`.

use std::collections::HashMap;
use uuid::Uuid;

use crate::ui_core::session_preview::preview_line;

/// What the newest message says, kept small: the row only draws one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPreview {
    pub role: String,
    pub text: String,
}

impl SessionPreview {
    /// The card's preview line, worded by `ui_core` so the rail and the row
    /// name the speaker the same way.
    pub fn line(&self) -> String {
        preview_line(&self.role, &self.text)
    }
}

/// Every session's preview, keyed by session id.
pub type PreviewMap = HashMap<Uuid, SessionPreview>;

/// One row in the group-topics picker: a session that can be attached under
/// the target, with the pick state the dialog tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCandidate {
    pub id: Uuid,
    pub title: String,
    pub selected: bool,
}

/// The group-topics prompt (`g`). The candidates are resolved once when the
/// prompt opens; the database guards do the refusing, so this only tracks
/// what the user pointed at.
#[derive(Debug, Clone)]
pub struct GroupTopicsPick {
    pub parent: Uuid,
    pub parent_title: String,
    pub candidates: Vec<GroupCandidate>,
    pub index: usize,
    /// Why the last attach attempt was refused, shown on the prompt so the
    /// user is not left guessing at a guard they cannot see.
    pub error: Option<String>,
}

impl GroupTopicsPick {
    pub fn open(
        parent: Uuid,
        parent_title: impl Into<String>,
        candidates: Vec<GroupCandidate>,
    ) -> Self {
        Self {
            parent,
            parent_title: parent_title.into(),
            candidates,
            index: 0,
            error: None,
        }
    }

    /// The ids the user marked. Empty means "nothing to do", not "all".
    pub fn selected_ids(&self) -> Vec<Uuid> {
        self.candidates
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.id)
            .collect()
    }

    pub fn toggle(&mut self) {
        if let Some(candidate) = self.candidates.get_mut(self.index) {
            candidate.selected = !candidate.selected;
        }
    }

    /// Move the cursor, clamped: the list never wraps, because wrapping on a
    /// two-entry list makes Down look like nothing happened.
    pub fn move_by(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            self.index = 0;
            return;
        }
        let last = self.candidates.len() - 1;
        let next = self.index as isize + delta;
        self.index = next.clamp(0, last as isize) as usize;
    }
}

/// The delete confirmation (`d`). Deleting a session used to fire on the
/// keystroke itself, which is one slipped finger away from losing a
/// conversation, so the row action asks first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteConfirm {
    pub id: Uuid,
    pub title: String,
}

/// The chat's topic bar (#2737), the row of chips above the transcript.
///
/// The window draws the same bar from `ui_core::topic_bar`; the terminal keeps
/// it to ONE row, `All` first, because the owner wants no sidebar here either.
/// `All` opens the conversation itself, every other entry opens a topic.
#[derive(Debug, Clone, Default)]
pub struct ChatTopicBar {
    /// The conversation every entry belongs to.
    pub root: Uuid,
    /// The entries in drawing order, `All` first.
    pub entries: Vec<crate::ui_core::topic_bar::BarEntry>,
    /// The conversation currently on screen, which marks its own entry.
    pub open: Uuid,
    /// A topic id to the name the bar prints for it.
    pub labels: HashMap<Uuid, String>,
}

impl ChatTopicBar {
    /// The label for one entry: `All` for the root, the topic's own name
    /// otherwise. A topic with no name gets a positional placeholder rather
    /// than an empty chip the user cannot aim at.
    pub fn label(&self, entry: crate::ui_core::topic_bar::BarEntry, index: usize) -> String {
        match entry {
            crate::ui_core::topic_bar::BarEntry::All => "All".to_string(),
            crate::ui_core::topic_bar::BarEntry::Topic(id) => self
                .labels
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("topic {}", index)),
        }
    }

    /// The session one step left or right of the open one, for Alt+←/→.
    /// Clamped at both ends: the bar is a strip, not a carousel.
    pub fn step(&self, delta: isize) -> Option<Uuid> {
        let at = self
            .entries
            .iter()
            .position(|e| e.is_open(self.root, self.open))?;
        let last = self.entries.len().saturating_sub(1);
        let next = (at as isize + delta).clamp(0, last as isize) as usize;
        self.entries.get(next).map(|e| e.session(self.root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(titles: &[&str]) -> GroupTopicsPick {
        let candidates = titles
            .iter()
            .enumerate()
            .map(|(i, t)| GroupCandidate {
                id: Uuid::from_u128(i as u128 + 1),
                title: (*t).to_string(),
                selected: false,
            })
            .collect();
        GroupTopicsPick::open(Uuid::from_u128(99), "target", candidates)
    }

    #[test]
    fn toggle_marks_only_the_row_under_the_cursor() {
        let mut p = pick(&["a", "b"]);
        p.toggle();
        assert_eq!(p.selected_ids(), vec![Uuid::from_u128(1)]);
        p.move_by(1);
        p.toggle();
        assert_eq!(p.selected_ids().len(), 2);
        p.toggle();
        assert_eq!(p.selected_ids(), vec![Uuid::from_u128(1)]);
    }

    #[test]
    fn move_clamps_instead_of_wrapping() {
        let mut p = pick(&["a", "b"]);
        p.move_by(-1);
        assert_eq!(p.index, 0);
        p.move_by(5);
        assert_eq!(p.index, 1);
    }

    #[test]
    fn empty_list_keeps_the_cursor_at_zero() {
        let mut p = pick(&[]);
        p.move_by(3);
        assert_eq!(p.index, 0);
        p.toggle();
        assert!(p.selected_ids().is_empty());
    }

    #[test]
    fn preview_line_rides_on_the_shared_wording() {
        let preview = SessionPreview {
            role: "assistant".to_string(),
            text: "hi".to_string(),
        };
        assert_eq!(preview.line(), "Crabs: hi");
    }
}
