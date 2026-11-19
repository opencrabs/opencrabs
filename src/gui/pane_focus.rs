//! Which leaf owns the keyboard in a split.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// The leaf that owns the keyboard in a split, `None` meaning the chat leaf.
///
/// `pane_focus` can name a pane the tree no longer holds: a closed leaf, a
/// session moved into the chat, a split that came back with one leaf. Read
/// literally, that id made EVERY leaf take the unfocused branch, so the split
/// drew a dim placeholder in every pane and no frame painted a real field at
/// all. A file dropped then landed in the window's editor, which nothing was
/// drawing, so the drop looked ignored (#2620).
///
/// A single leaf is never a split and the chat owns the keyboard, so the id is
/// not consulted at all. A live id names its own leaf, with the chat leaf
/// reading as `None` the way it does everywhere else. A stale id hands the
/// keyboard back to the chat when the chat is in the tree, and to the first
/// surviving leaf when it is not, so the window always draws one real composer
/// and one real drop target.
pub(crate) fn resolve_split_focus(
    leaves: &[Uuid],
    pane_focus: Option<Uuid>,
    chat: Option<Uuid>,
) -> Option<Uuid> {
    if leaves.len() < 2 {
        return None;
    }
    if let Some(id) = pane_focus.filter(|id| leaves.contains(id)) {
        return (Some(id) != chat).then_some(id);
    }
    if chat.is_some_and(|id| leaves.contains(&id)) {
        None
    } else {
        leaves.first().copied()
    }
}
