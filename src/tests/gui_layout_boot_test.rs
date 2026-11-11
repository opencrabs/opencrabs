//! Source-scan pins for the layout boot path (#2689).
//!
//! The window used to open whatever `last_session` named and then refuse the
//! saved tree when that conversation was not one of its panes. That is the
//! normal case: `last_session` moves to whatever surface spoke last, so the
//! restart landed on the plain chat and the template was gone. These pins
//! hold the fix: the layout names the boot conversation, and the save
//! records which leaf was the window's own chat.

use crate::tests::gui_dashboard_source::DASHBOARD;

const LAYOUT_STORE: &str = include_str!("../gui/layout_store.rs");

#[test]
fn the_boot_pick_comes_from_the_saved_layout_first() {
    assert!(
        DASHBOARD.contains("layout_store::boot_session(saved, &known)"),
        "the launch asks the saved layout which conversation to open"
    );
}

#[test]
fn the_launch_still_falls_back_to_the_normal_pick() {
    assert!(
        DASHBOARD.contains("pick_initial_session("),
        "no layout or no surviving leaf keeps the old boot pick"
    );
}

#[test]
fn the_saved_layout_carries_the_chat_leaf_field() {
    assert!(
        LAYOUT_STORE.contains("pub chat: Option<Uuid>,"),
        "the saved layout records which leaf was the window's chat"
    );
    assert!(
        LAYOUT_STORE.contains("#[serde(default)]"),
        "files written before the field existed still load"
    );
}

#[test]
fn the_restore_still_checks_the_chat_is_a_pane() {
    assert!(
        DASHBOARD.contains("if !tree.contains(chat)"),
        "the safety net stays: a wrong window is worse than a plain one"
    );
}

/// The pin holds the ORDER of the candidates, not the name of the binding
/// that walks them. It used to assert `for candidate in [...]`, and a clippy
/// pass that flattened the two Options renamed the binding to `id` and turned
/// this test red while the behaviour stayed exactly right. The array literal
/// is the part that carries the meaning: chat leaf first, caret leaf second.
#[test]
fn the_boot_helper_prefers_the_chat_then_the_caret_then_any_survivor() {
    assert!(
        LAYOUT_STORE.contains("[saved.chat, saved.focus]"),
        "chat leaf first, caret leaf second"
    );
    assert!(
        LAYOUT_STORE.contains("leaves.into_iter().find(|id| known.contains(id))"),
        "then the first leaf the ledger still holds"
    );
}
