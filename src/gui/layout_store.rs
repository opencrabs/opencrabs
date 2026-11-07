//! The window's own layout, kept so a restart puts the panes back (#2675).
//!
//! The terminal saves its split tree to `layout.json`; the window saves its
//! own to `gui_layout.json`. They are separate files on purpose: a window leaf
//! carries the share a divider drag produced, so one file for both surfaces
//! would let either one corrupt the other. A missing, unreadable or outdated
//! file is not an error: the window opens on the plain chat, exactly as a
//! first run does.

use crate::logging;
use opencrabs::ui_core::pane_tree::PaneTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The file's name under the OpenCrabs home, beside the terminal's
/// `layout.json`.
pub const FILE: &str = "gui_layout.json";

/// The snapshot's version. A file written by another one is discarded rather
/// than guessed at: the tree's fields may move between releases, and half a
/// tree draws a wrong window.
pub const VERSION: u32 = 1;

/// What the window writes down: the pane tree and the leaf that held the
/// caret. `focus` is `None` when the chat itself owned it, the same `None` the
/// window uses in memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub version: u32,
    pub panes: Option<PaneTree>,
    pub focus: Option<Uuid>,
}

/// Where the window keeps its layout.
pub fn path() -> PathBuf {
    opencrabs::config::opencrabs_home().join(FILE)
}

/// Read a snapshot. `None` when the file is absent, corrupt, from another
/// version, or holds no tree. A file this function refuses is removed, so the
/// next write starts clean instead of reading the same bad bytes every boot.
pub fn load(path: &Path) -> Option<SavedLayout> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<SavedLayout>(&data) {
        Ok(saved) if saved.version == VERSION && saved.panes.is_some() => Some(saved),
        Ok(saved) => {
            logging::warn(&format!(
                "gui layout: version {} is not {VERSION}, starting on the chat",
                saved.version
            ));
            discard(path);
            None
        }
        Err(e) => {
            logging::warn(&format!(
                "gui layout: unreadable ({e}), starting on the chat"
            ));
            discard(path);
            None
        }
    }
}

/// Write the layout the window is showing. One pane is the plain chat, so the
/// file is removed instead: no file means "no custom layout", which is the
/// signal the terminal uses too, and it keeps a first run read-free.
pub fn save(path: &Path, panes: Option<&PaneTree>, focus: Option<Uuid>) {
    let split = panes.is_some_and(|tree| tree.leaves().len() > 1);
    if !split {
        discard(path);
        return;
    }
    let saved = SavedLayout {
        version: VERSION,
        panes: panes.cloned(),
        focus,
    };
    let Ok(json) = serde_json::to_string_pretty(&saved) else {
        logging::error("gui layout: could not serialise the pane tree");
        return;
    };
    if let Err(e) = std::fs::write(path, json) {
        logging::warn(&format!("gui layout: write failed ({e})"));
    }
}

/// The tree with the leaves the ledger no longer holds taken out. `None` when
/// fewer than two panes survive: one pane is not a split, and showing the
/// survivor is the plain chat's job, not the tree's.
pub fn prune(mut tree: PaneTree, known: &HashSet<Uuid>) -> Option<PaneTree> {
    let gone: Vec<Uuid> = tree
        .leaves()
        .into_iter()
        .filter(|id| !known.contains(id))
        .collect();
    for session in gone {
        tree.close(session);
    }
    (tree.leaves().len() > 1).then_some(tree)
}

/// Remove the file, treating "it was already gone" as success.
fn discard(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => logging::warn(&format!("gui layout: remove failed ({e})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::split::Layout;

    /// Two leaves side by side.
    fn split_tree(left: Uuid, right: Uuid) -> PaneTree {
        let mut tree = PaneTree::leaf(left);
        assert!(tree.split(left, Layout::Side, right), "split the leaf");
        tree
    }

    #[test]
    fn a_saved_split_comes_back_with_its_shares_and_its_caret() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let mut tree = split_tree(left, right);
        assert!(tree.drag_divider(&[], 0, 80.0, 800.0), "drag the divider");
        save(&file, Some(&tree), Some(right));

        let saved = load(&file).expect("a saved layout reads back");
        assert_eq!(saved.version, VERSION);
        assert_eq!(saved.focus, Some(right), "the caret comes back");
        let restored = saved.panes.expect("the tree");
        assert_eq!(restored.leaves(), vec![left, right], "same draw order");
        // The drag moved the first pane off even: a restart must not snap the
        // panes back to 50/50.
        assert_ne!(restored.leaf_width_share(left), Some(0.5));
    }

    #[test]
    fn one_pane_is_not_a_layout_and_leaves_no_file() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        save(&file, Some(&split_tree(left, right)), None);
        assert!(file.exists(), "a split writes the file");
        save(&file, Some(&PaneTree::leaf(left)), None);
        assert!(!file.exists(), "back to one pane removes it");
    }

    #[test]
    fn a_corrupt_file_is_discarded_instead_of_guessed_at() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        std::fs::write(&file, "{ not a layout").expect("write the bad file");
        assert!(load(&file).is_none());
        assert!(
            !file.exists(),
            "the bad bytes are gone, not read every boot"
        );
    }

    #[test]
    fn a_snapshot_from_another_version_is_discarded() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let saved = SavedLayout {
            version: VERSION + 1,
            panes: Some(split_tree(left, right)),
            focus: None,
        };
        std::fs::write(&file, serde_json::to_string(&saved).expect("json")).expect("write");
        assert!(load(&file).is_none());
        assert!(!file.exists());
    }

    #[test]
    fn a_pane_whose_session_is_gone_is_dropped_from_the_tree() {
        let (left, middle, right) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut tree = split_tree(left, middle);
        assert!(tree.split(middle, Layout::Stack, right), "nested split");
        let known: HashSet<Uuid> = [left, right].into_iter().collect();
        let pruned = prune(tree, &known).expect("two panes survive");
        assert_eq!(pruned.leaves(), vec![left, right]);
    }

    #[test]
    fn a_tree_below_two_survivors_is_no_split_at_all() {
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let known: HashSet<Uuid> = [left].into_iter().collect();
        assert!(
            prune(split_tree(left, right), &known).is_none(),
            "one survivor means the plain chat, not a one-pane split"
        );
    }
}
