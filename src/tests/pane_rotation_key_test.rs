//! Pane rotation keymap (#2580, #2579).
//!
//! `Tab` used to rotate pane focus in split view, which collided with its own
//! job of accepting the follow-up suggestion and the emoji picker. Rotation
//! now lives on `Alt+Tab` (clockwise) and `Alt+Shift+Tab` (anticlockwise), so
//! the two are distinguishable at the parser level: these are pure predicate
//! tests over manufactured key events, plus the manager walk the handler
//! drives.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::events::keys;
use crate::tui::pane::{PaneManager, SplitDirection};

fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn alt_tab_is_the_clockwise_rotation() {
    let alt_tab = event(KeyCode::Tab, KeyModifiers::ALT);
    assert!(
        keys::is_pane_rotate_next(&alt_tab),
        "alt+tab rotates forward"
    );
    assert!(
        !keys::is_pane_rotate_prev(&alt_tab),
        "alt+tab must not also claim the reverse direction"
    );
}

#[test]
fn alt_shift_tab_is_the_anticlockwise_rotation() {
    let alt_shift_tab = event(KeyCode::Tab, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert!(
        keys::is_pane_rotate_prev(&alt_shift_tab),
        "alt+shift+tab rotates backward"
    );
    assert!(
        !keys::is_pane_rotate_next(&alt_shift_tab),
        "alt+shift+tab must not also claim the forward direction"
    );
    // Terminals that fold shift into the glyph report BackTab instead.
    let back_tab = event(KeyCode::BackTab, KeyModifiers::ALT);
    assert!(keys::is_pane_rotate_prev(&back_tab), "alt+backtab too");
    // A real terminal keeps SHIFT attached to BackTab, so the chord must read
    // the modifier set, not compare it exactly (#2656).
    let real_back_tab = event(KeyCode::BackTab, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert!(
        keys::is_pane_rotate_prev(&real_back_tab),
        "alt+shift+backtab is what a terminal actually sends"
    );
    assert!(
        !keys::is_pane_rotate_prev(&event(KeyCode::BackTab, KeyModifiers::SHIFT)),
        "a bare shift+backtab is not a pane chord"
    );
    assert!(
        !keys::is_pane_rotate_prev(&event(
            KeyCode::Tab,
            KeyModifiers::ALT | KeyModifiers::CONTROL
        )),
        "ctrl+alt+tab must stay the window's, not rotate"
    );
}

#[test]
fn tab_alone_keeps_its_own_action() {
    let tab = event(KeyCode::Tab, KeyModifiers::NONE);
    assert!(
        !keys::is_pane_rotate_next(&tab) && !keys::is_pane_rotate_prev(&tab),
        "bare Tab must not rotate panes: it accepts a suggestion or an emoji"
    );
    assert!(keys::is_tab(&tab), "bare Tab still maps to its own action");
}

#[test]
fn rotation_wraps_in_both_directions() {
    let mut mgr = PaneManager::new();
    mgr.split(SplitDirection::Horizontal);
    mgr.split(SplitDirection::Vertical);
    let ids = mgr.pane_ids_in_order();
    assert_eq!(ids.len(), 3, "three leaves to walk");

    // Forward from the last wraps to the first.
    mgr.focused = ids[2];
    mgr.focus_next();
    assert_eq!(mgr.focused, ids[0], "clockwise wraps past the last pane");

    // Backward from the first wraps to the last.
    mgr.focus_prev();
    assert_eq!(
        mgr.focused, ids[2],
        "anticlockwise wraps past the first pane"
    );
}
