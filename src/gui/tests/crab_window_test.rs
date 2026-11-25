//! Tests for the window state kept per crab (C7): the layout and the drafts a
//! pick on the instance strip leaves behind, and the files each crab writes.
//!
//! The swap is asserted on the values the window parks
//! (`layout_store::CrabWindows`). Where the rule is about where the window
//! calls it, the source is pinned, the way the other window guards pin theirs.

use opencrabs::ui_core::composer::{ComposerDraft, DraftStore};
use opencrabs::ui_core::pane_tree::PaneTree;
use opencrabs::ui_core::split::Layout;
use uuid::Uuid;

use crate::layout_store::{self, CrabWindows, ParkedCrab, SavedLayout};
use crate::rail_activity;

const PANES: &str = include_str!("../dashboard_panes.rs");
const APP: &str = include_str!("../app.rs");

fn draft(text: &str) -> ComposerDraft {
    ComposerDraft {
        text: text.to_string(),
        cursor: text.len(),
        history_index: None,
        history_stash: String::new(),
    }
}

/// A two pane split with the chat on the left and the caret on the right.
fn split_layout(chat: Uuid, other: Uuid) -> SavedLayout {
    let mut tree = PaneTree::leaf(chat);
    assert!(tree.split(chat, Layout::Side, other), "split the leaf");
    SavedLayout {
        version: layout_store::VERSION,
        panes: Some(tree),
        focus: Some(other),
        chat: Some(chat),
        window: None,
        rail_open: Some(true),
    }
}

/// The acceptance of C7: pick another crab and come back, and the split and
/// the half-typed text of the window's own crab are where they were.
#[test]
fn switching_crab_and_back_keeps_the_draft_and_the_layout() {
    let dir = tempfile::tempdir().expect("temp home");
    let (chat, other) = (Uuid::new_v4(), Uuid::new_v4());
    let layout = split_layout(chat, other);
    let mut drafts = DraftStore::new();
    drafts.store(chat, draft("half a message"));
    drafts.store(other, draft("the pane's own"));

    let mut windows = CrabWindows::default();
    // Leave the window's own crab for a remote one.
    windows.park(
        None,
        ParkedCrab {
            layout: Some(layout.clone()),
            drafts,
        },
    );
    let remote = windows.unpark_or_load(Some("a:1"), &dir.path().join("absent.json"));
    assert!(remote.layout.is_none(), "a crab never seen has no layout");
    assert!(remote.drafts.is_empty(), "nor the drafts of another crab");

    // Type something on the remote, then go home.
    let remote_session = Uuid::new_v4();
    let mut remote_drafts = remote.drafts;
    remote_drafts.store(remote_session, draft("remote text"));
    windows.park(
        Some("a:1".to_string()),
        ParkedCrab {
            layout: None,
            drafts: remote_drafts,
        },
    );
    let home = windows.unpark_or_load(None, &dir.path().join("absent.json"));
    assert_eq!(home.layout, Some(layout), "the split comes back as it was");
    assert_eq!(home.drafts.peek(chat), Some(&draft("half a message")));
    assert_eq!(home.drafts.peek(other), Some(&draft("the pane's own")));
    assert!(
        home.drafts.peek(remote_session).is_none(),
        "the remote's draft stays with the remote"
    );

    // And back to the remote: its own draft is still there.
    let again = windows.unpark_or_load(Some("a:1"), &dir.path().join("absent.json"));
    assert_eq!(
        again.drafts.peek(remote_session),
        Some(&draft("remote text"))
    );
}

/// A crab the window has not left in this run reads its own file, so a
/// restart still puts back the split last shown on that crab.
#[test]
fn a_first_visit_reads_the_crab_s_own_file() {
    let dir = tempfile::tempdir().expect("temp home");
    let file = dir.path().join(layout_store::file_for("b:2"));
    let (chat, other) = (Uuid::new_v4(), Uuid::new_v4());
    let layout = split_layout(chat, other);
    layout_store::save(
        &file,
        layout.panes.as_ref(),
        layout.focus,
        layout.chat,
        None,
        true,
    );
    let mut windows = CrabWindows::default();
    let parked = windows.unpark_or_load(Some("b:2"), &file);
    let back = parked.layout.expect("the saved split reads back");
    assert_eq!(back.chat, Some(chat));
    assert_eq!(back.focus, Some(other));
    assert_eq!(
        back.panes.map(|tree| tree.leaves()),
        Some(vec![chat, other])
    );
}

/// The window's own crab keeps `gui_layout.json` and `rail_seen.json`, the
/// files it always had; only a remote crab gets files of its own.
#[test]
fn the_local_crab_keeps_its_files_and_a_remote_gets_its_own() {
    assert_eq!(layout_store::path_for(None), layout_store::path());
    assert!(layout_store::path_for(None).ends_with(layout_store::FILE));
    assert!(rail_activity::seen_path_for(None).ends_with("rail_seen.json"));

    let remote = layout_store::path_for(Some("a:1"));
    assert_ne!(remote, layout_store::path());
    assert_eq!(remote.parent(), layout_store::path().parent());
    assert!(remote.ends_with("gui_layout.a~3a1.json"));
    assert!(rail_activity::seen_path_for(Some("a:1")).ends_with("rail_seen.a~3a1.json"));
}

/// The id is the remote's own: a slash cannot leave the home directory, and
/// two ids never share a file.
#[test]
fn a_crab_file_name_is_flat_and_never_shared() {
    let names = ["a:1", "a_1", "a~3a1", "../x", "a/1"]
        .map(layout_store::file_for)
        .to_vec();
    for name in &names {
        assert!(!name.contains('/'), "{name} stays in the home directory");
    }
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "{names:?}");
}

/// Where the window does it: the layout is written to the file of the crab on
/// screen, the swap runs once the picked crab's rows land, and the boot reads
/// the local crab's file.
#[test]
fn the_window_writes_and_swaps_per_crab() {
    assert!(PANES.contains("layout_store::path_for(self.layout_crab.as_deref())"));
    let notify = PANES
        .split_once("pub(crate) fn notify_panes(")
        .map(|(_, rest)| rest)
        .expect("notify_panes");
    let body = notify.split_once("\n    }\n").map(|(b, _)| b).unwrap_or("");
    assert!(body.contains("self.follow_crab(cx);"), "{body}");

    let follow = PANES
        .split_once("pub(crate) fn follow_crab(")
        .map(|(_, rest)| rest)
        .expect("follow_crab");
    let body = follow.split_once("\n    }\n").map(|(b, _)| b).unwrap_or("");
    for needle in [
        "self.stash_all_pane_drafts(cx);",
        "self.crab_windows.park(left, leaving);",
        "self.crab_windows\n            .unpark_or_load(",
        "self.restore_layout(Some(layout), cx);",
    ] {
        assert!(body.contains(needle), "follow_crab lost {needle:?}");
    }

    assert!(APP.contains("layout_store::load(&layout_store::path_for(None))"));
    assert!(APP.contains("rail_activity::seen_path_for(None)"));
    assert!(APP.contains("layout_crab: None,"));
}
