//! The window comes back to the conversation it was showing (#3197).
//!
//! A window with one pane writes no tree (`panes: null`) but it does write the
//! chat it was on. `boot_session` started with the tree and returned `None`
//! when it was missing, so every single-pane restart fell through to the shared
//! `last_session` pointer, which the terminal and the channels also write: the
//! restart landed on whatever surface spoke last instead of the conversation
//! the window was showing.
//!
//! The window has no headless harness, so the helper is called directly. It is
//! pure, which is why the boot choice is testable at all.

use crate::layout_store::{SavedLayout, VERSION, boot_session};
use std::collections::HashSet;
use uuid::Uuid;

/// A layout written by a window with one pane: no tree, a chat, and a frame.
fn single_pane(chat: Option<Uuid>, focus: Option<Uuid>) -> SavedLayout {
    SavedLayout {
        version: VERSION,
        panes: None,
        focus,
        chat,
        window: None,
        rail_open: Some(true),
    }
}

#[test]
fn a_single_pane_restart_opens_the_chat_it_was_showing() {
    let chat = Uuid::new_v4();
    let known: HashSet<Uuid> = [chat].into_iter().collect();
    assert_eq!(
        boot_session(&single_pane(Some(chat), None), &known),
        Some(chat),
        "no tree is one pane, not an empty layout: the chat still names the conversation"
    );
}

#[test]
fn a_single_pane_restart_skips_a_chat_the_ledger_no_longer_holds() {
    let chat = Uuid::new_v4();
    let known: HashSet<Uuid> = HashSet::new();
    assert_eq!(
        boot_session(&single_pane(Some(chat), None), &known),
        None,
        "a deleted conversation falls through to the launch's own pick"
    );
}

#[test]
fn a_single_pane_restart_falls_back_to_the_caret_leaf() {
    let chat = Uuid::new_v4();
    let caret = Uuid::new_v4();
    let known: HashSet<Uuid> = [caret].into_iter().collect();
    assert_eq!(
        boot_session(&single_pane(Some(chat), Some(caret)), &known),
        Some(caret),
        "the caret leaf is the second candidate, as it is for a split window"
    );
}
