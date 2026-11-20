//! The window's own layout, kept so a restart puts the panes back (#2675).
//!
//! The terminal saves its split tree to `layout.json`; the window saves its
//! own to `gui_layout.json`. They are separate files on purpose: a window leaf
//! carries the share a divider drag produced, so one file for both surfaces
//! would let either one corrupt the other. A missing, unreadable or outdated
//! file is not an error: the window opens on the plain chat, exactly as a
//! first run does.
//!
//! The file also holds the frame the window was showing (#3082), because where
//! a user put the window is worth keeping even when the panes are the plain
//! chat. That is the one thing a file can hold on its own: a frame with no tree
//! loads, a tree with no frame is a file from before that change and still
//! loads.

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

/// What the window writes down: the pane tree, the leaf that held the
/// caret, the leaf that was the window's own chat, and the frame it was
/// showing. `focus` is `None` when the chat itself owned it, the same `None`
/// the window uses in memory, and `chat` says which leaf that chat was
/// (#2689).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub version: u32,
    pub panes: Option<PaneTree>,
    pub focus: Option<Uuid>,
    /// The window's own transcript leaf. Files written before #2689 have no
    /// such key and read as `None`; the boot pick then uses the caret leaf.
    #[serde(default)]
    pub chat: Option<Uuid>,
    /// The frame the window was showing (#3082). Files written before that
    /// have no such key and read as `None`, which opens on the fitted default.
    #[serde(default)]
    pub window: Option<WindowState>,
}

/// The frame the window was showing (#3082): where it sat, how big it was, and
/// whether it was maximized.
///
/// Logical pixels, the same units `Window::bounds` reports and `WindowOptions`
/// takes, so the file round-trips through the one type the platform speaks
/// rather than a physical size that a scale change would misplace.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Maximized is a state, not a size. The four lengths above are the frame
    /// the window restores to, so un-maximizing does not land on the full
    /// screen.
    #[serde(default)]
    pub maximized: bool,
}

/// Where the window keeps its layout.
pub fn path() -> PathBuf {
    opencrabs::config::opencrabs_home().join(FILE)
}

/// Read a snapshot. `None` when the file is absent, corrupt, from another
/// version, or holds neither a tree nor a frame. A file this function refuses
/// is removed, so the next write starts clean instead of reading the same bad
/// bytes every boot.
pub fn load(path: &Path) -> Option<SavedLayout> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<SavedLayout>(&data) {
        Ok(saved) if saved.version != VERSION => {
            logging::warn(&format!(
                "gui layout: version {} is not {VERSION}, starting on the chat",
                saved.version
            ));
            discard(path);
            None
        }
        Ok(saved) if saved.panes.is_some() || saved.window.is_some() => Some(saved),
        Ok(_) => {
            logging::warn("gui layout: the file holds no tree and no frame, starting on the chat");
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

/// Write the layout the window is showing. One pane is the plain chat, so no
/// tree is written: no tree means "no custom layout", which is the signal the
/// terminal uses too, and it keeps a first run read-free. The frame is written
/// either way, because where the window sat is worth keeping even when it holds
/// nothing but the chat (#3082). A file with neither a tree nor a frame is
/// removed instead.
pub fn save(
    path: &Path,
    panes: Option<&PaneTree>,
    focus: Option<Uuid>,
    chat: Option<Uuid>,
    window: Option<WindowState>,
) {
    let split = panes.is_some_and(|tree| tree.leaves().len() > 1);
    if !split && window.is_none() {
        discard(path);
        return;
    }
    let saved = SavedLayout {
        version: VERSION,
        panes: if split { panes.cloned() } else { None },
        focus,
        chat,
        window,
    };
    let Ok(json) = serde_json::to_string_pretty(&saved) else {
        logging::error("gui layout: could not serialise the pane tree");
        return;
    };
    if let Err(e) = std::fs::write(path, json) {
        logging::warn(&format!("gui layout: write failed ({e})"));
    }
}

/// The conversation a launch opens so the saved tree can come back (#2689).
/// The chat leaf first: the window's own transcript has to exist before the
/// tree can agree with it. Then the caret leaf, for files written before the
/// chat was recorded. Then the first leaf the ledger still holds. `None` when
/// no leaf survives, and the launch falls back to its normal pick.
pub fn boot_session(saved: &SavedLayout, known: &HashSet<Uuid>) -> Option<Uuid> {
    let leaves = saved.panes.as_ref()?.leaves();
    for id in [saved.chat, saved.focus].into_iter().flatten() {
        if leaves.contains(&id) && known.contains(&id) {
            return Some(id);
        }
    }
    leaves.into_iter().find(|id| known.contains(id))
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
    use opencrabs::ui_core::pane_tree::DropHalf;
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
        save(&file, Some(&tree), Some(right), Some(left), None);

        let saved = load(&file).expect("a saved layout reads back");
        assert_eq!(saved.version, VERSION);
        assert_eq!(saved.focus, Some(right), "the caret comes back");
        let restored = saved.panes.expect("the tree");
        assert_eq!(restored.leaves(), vec![left, right], "same draw order");
        // The drag moved the first pane off even: a restart must not snap the
        // panes back to 50/50.
        assert_ne!(restored.leaf_width_share(left), Some(0.5));
    }

    /// A moved pane survives a restart: the tree the move produced, with
    /// its shares, is the tree that loads back (#2683).
    #[test]
    fn a_moved_pane_survives_a_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut tree = PaneTree::leaf(a);
        assert!(tree.split(a, Layout::Side, b), "split the leaf");
        assert!(tree.split(b, Layout::Stack, c), "split the second pane");
        assert!(
            tree.move_leaf(
                c,
                a,
                DropHalf {
                    layout: Layout::Side,
                    after: false,
                }
            ),
            "move c beside a"
        );
        let moved_leaves = tree.leaves();
        let moved_shares: Vec<Option<f32>> = moved_leaves
            .iter()
            .map(|session| tree.leaf_width_share(*session))
            .collect();
        save(&file, Some(&tree), Some(a), Some(a), None);

        let saved = load(&file).expect("a saved layout reads back");
        let restored = saved.panes.expect("the tree");
        assert_eq!(restored.leaves(), moved_leaves, "the move comes back");
        let round_shares: Vec<Option<f32>> = moved_leaves
            .iter()
            .map(|session| restored.leaf_width_share(*session))
            .collect();
        assert_eq!(round_shares, moved_shares, "the shares come back");
        assert!(restored.duplicates().is_empty(), "no session twice");
    }

    /// A custom share inside a nested split loads back unchanged: the
    /// snapshot keeps the whole tree's numbers, not just the root pair
    /// (#2684).
    #[test]
    fn a_nested_custom_share_survives_a_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut tree = PaneTree::leaf(a);
        assert!(tree.split(a, Layout::Side, b), "split the leaf");
        assert!(tree.split(b, Layout::Stack, c), "split the second pane");
        // The nested pair holds half of 2400px, so 120px of travel moves it
        // off even without touching the floor.
        assert!(tree.drag_divider(&[1], 0, 120.0, 2400.0), "drag the pair");
        let nested_before = tree.leaf_width_share(b);
        assert_ne!(nested_before, Some(0.25), "the drag moved the share");
        save(&file, Some(&tree), Some(b), Some(a), None);

        let saved = load(&file).expect("a saved layout reads back");
        let restored = saved.panes.expect("the tree");
        assert_eq!(restored.leaves(), vec![a, b, c], "same draw order");
        assert_eq!(
            restored.leaf_width_share(b),
            nested_before,
            "the nested share comes back unchanged"
        );
    }

    #[test]
    fn one_pane_is_not_a_layout_and_leaves_no_file() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        save(&file, Some(&split_tree(left, right)), None, Some(left), None);
        assert!(file.exists(), "a split writes the file");
        save(&file, Some(&PaneTree::leaf(left)), None, Some(left), None);
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
            chat: None,
            window: None,
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

    #[test]
    fn boot_opens_the_saved_chat_leaf_even_when_the_caret_is_elsewhere() {
        // #2689: last_session drifts to whatever surface spoke last, so the
        // boot pick is rarely one of the panes. The layout names its own chat.
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let saved = SavedLayout {
            version: VERSION,
            panes: Some(split_tree(left, right)),
            focus: Some(right),
            chat: Some(left),
            window: None,
        };
        let known: HashSet<Uuid> = [left, right].into_iter().collect();
        assert_eq!(boot_session(&saved, &known), Some(left));
    }

    #[test]
    fn boot_falls_back_to_the_caret_leaf_when_the_chat_is_gone() {
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let saved = SavedLayout {
            version: VERSION,
            panes: Some(split_tree(left, right)),
            focus: Some(right),
            chat: Some(left),
            window: None,
        };
        let known: HashSet<Uuid> = [right].into_iter().collect();
        assert_eq!(boot_session(&saved, &known), Some(right));
    }

    #[test]
    fn boot_falls_back_to_the_first_leaf_the_ledger_still_holds() {
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let saved = SavedLayout {
            version: VERSION,
            panes: Some(split_tree(left, right)),
            focus: None,
            chat: None,
            window: None,
        };
        let known: HashSet<Uuid> = [right].into_iter().collect();
        assert_eq!(boot_session(&saved, &known), Some(right));
    }

    #[test]
    fn boot_with_no_surviving_leaf_leaves_the_pick_to_the_launch() {
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let saved = SavedLayout {
            version: VERSION,
            panes: Some(split_tree(left, right)),
            focus: Some(right),
            chat: Some(left),
            window: None,
        };
        assert_eq!(boot_session(&saved, &HashSet::new()), None);
    }

    #[test]
    fn an_old_file_without_the_chat_field_still_loads() {
        // Files written before #2689 have no chat key; they must not be
        // discarded as unreadable.
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let json = serde_json::json!({
            "version": VERSION,
            "panes": split_tree(left, right),
            "focus": serde_json::Value::Null,
        });
        std::fs::write(&file, json.to_string()).expect("write the old file");
        let saved = load(&file).expect("an old file reads back");
        assert_eq!(saved.chat, None);
        let known: HashSet<Uuid> = [left, right].into_iter().collect();
        assert_eq!(boot_session(&saved, &known), Some(left));
    }

    #[test]
    fn the_chat_leaf_survives_a_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        save(
            &file,
            Some(&split_tree(left, right)),
            Some(right),
            Some(left),
            None,
        );
        let saved = load(&file).expect("the layout reads back");
        assert_eq!(saved.chat, Some(left), "the chat leaf comes back");
    }

    /// Where the window sat comes back, so a restart does not re-centre a frame
    /// the user placed by hand (#3082).
    #[test]
    fn the_frame_survives_a_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let frame = WindowState {
            x: -1450.0,
            y: 40.0,
            width: 1200.0,
            height: 800.0,
            maximized: false,
        };
        save(&file, None, None, None, Some(frame));
        let saved = load(&file).expect("a frame on its own is worth reading");
        assert_eq!(saved.window, Some(frame));
        assert_eq!(saved.panes, None, "no tree means the plain chat");
    }

    /// A file that holds only the frame is a real file, not a corrupt one: the
    /// window writes it the first time it settles, before any split exists.
    #[test]
    fn a_file_that_holds_only_a_frame_loads() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let json = serde_json::json!({
            "version": VERSION,
            "panes": serde_json::Value::Null,
            "focus": serde_json::Value::Null,
            "window": { "x": 10.0, "y": 20.0, "width": 900.0, "height": 700.0 },
        });
        std::fs::write(&file, json.to_string()).expect("write the frame file");
        let saved = load(&file).expect("the frame reads back");
        assert_eq!(
            saved.window,
            Some(WindowState {
                x: 10.0,
                y: 20.0,
                width: 900.0,
                height: 700.0,
                maximized: false,
            }),
            "a frame without the maximize key is a windowed frame"
        );
        assert!(file.exists(), "a readable file is left alone");
    }

    /// A file from before #3082 has no frame key and must still load its tree.
    #[test]
    fn an_old_file_without_a_frame_still_loads() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let json = serde_json::json!({
            "version": VERSION,
            "panes": split_tree(left, right),
            "focus": serde_json::Value::Null,
            "chat": serde_json::Value::Null,
        });
        std::fs::write(&file, json.to_string()).expect("write the old file");
        let saved = load(&file).expect("an old file reads back");
        assert_eq!(saved.window, None, "no frame means the fitted default");
        assert_eq!(saved.panes.expect("the tree").leaves(), vec![left, right]);
    }

    /// Nothing to say means no file: no tree and no frame is the first run.
    #[test]
    fn no_tree_and_no_frame_leaves_no_file() {
        let dir = tempfile::tempdir().expect("temp home");
        let file = dir.path().join("gui_layout.json");
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        save(&file, Some(&split_tree(left, right)), None, Some(left), None);
        assert!(file.exists(), "a split writes the file");
        save(&file, None, None, None, None);
        assert!(!file.exists(), "nothing to remember removes it");
    }
}
