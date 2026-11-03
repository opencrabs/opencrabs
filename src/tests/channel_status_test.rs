//! The pure channel-ownership model (#2452).

use crate::ui_core::channel_status::{Ownership, ownership, status_line};

#[test]
fn the_tui_owning_the_lock_draws_no_chip() {
    // A live instance holds the channels: the GUI stays quiet.
    let own = ownership(true, true);
    assert_eq!(own, Ownership::Running);
    assert!(status_line(&["Telegram"], &own).is_none());
}

#[test]
fn a_dead_owner_names_the_closed_instance() {
    let own = ownership(true, false);
    assert_eq!(own, Ownership::Stale);
    let line = status_line(&["Telegram", "Discord"], &own).expect("chip line");
    assert!(line.contains("Telegram"), "names the channel: {line}");
    assert!(line.contains("Discord"), "names every channel: {line}");
    assert!(line.contains("daemon"), "names the action: {line}");
}

#[test]
fn no_lock_at_all_is_unowned_and_still_offers_the_daemon() {
    let own = ownership(false, false);
    assert_eq!(own, Ownership::Unowned);
    let line = status_line(&["Slack"], &own).expect("chip line");
    assert!(line.contains("nothing is serving them"), "{line}");
    assert!(line.contains("start the daemon"), "{line}");
}

#[test]
fn nothing_configured_draws_no_chip_even_when_unowned() {
    assert!(status_line(&[], &Ownership::Unowned).is_none());
    assert!(status_line(&[], &Ownership::Stale).is_none());
}
