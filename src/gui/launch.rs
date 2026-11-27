//! What a launch asks the window to do before it builds anything,
//! and the conversation it opens.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// The conversation a launch opens: the one the user was in last time when the
/// ledger still holds it, else the newest. Pure, so the choice is testable
/// without a window. A pointer at a session that was deleted (or a first run
/// with no pointer at all) falls back to the rail's order, which is newest
/// first (#2444).
///
/// `known` is every live id in the ledger, not the rail's page: the rail is
/// capped, so a conversation outside the newest page is still real and must
/// open instead of the newest (#3197).
pub(crate) fn pick_initial_session(
    items: &[opencrabs::db::models::Session],
    last: Option<Uuid>,
    known: &std::collections::HashSet<Uuid>,
) -> Option<Uuid> {
    match last {
        Some(id) if known.contains(&id) => Some(id),
        _ => items.first().map(|s| s.id),
    }
}

/// Record the conversation the window is showing, so the next launch (this
/// surface or the terminal) reopens it. Split out from `open_session` so the
/// write is exercised on its own, under a scratch home, instead of through a
/// whole Dashboard.
pub(crate) fn remember_open_session(id: Uuid) {
    opencrabs::utils::last_session::write(id);
}

/// What a launch asks the window to do before it builds anything (#2841).
///
/// A packaging step, a smoke test or a hash check runs `opencrabs-gui
/// --version` and expects a string back. Without this the flag was ignored and
/// the whole application booted: the ledger opened, the channel connectors
/// started and the A2A listener bound its port. That turned a version check
/// into a second live instance, which is a known conflict on one bot token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launch {
    /// Open the window.
    Window,
    /// Print the version and exit 0.
    Version,
    /// Print the flags and exit 0.
    Help,
    /// A flag this binary does not answer. Rejected rather than booted.
    Refused(String),
}

/// Read the launch intent from the arguments, with the program name already
/// skipped.
///
/// Only the two flags every packaging tool assumes are answered. A bare word
/// is left alone on purpose: the protocol handler starts this binary with the
/// activation URI in argv (#2751), and that launch has to open the window. A
/// dash-prefixed word that matches nothing is refused, because a typo in a
/// script must fail loudly instead of starting the app.
pub(crate) fn launch_intent<I: IntoIterator<Item = String>>(args: I) -> Launch {
    for arg in args {
        match arg.as_str() {
            "--version" | "-V" => return Launch::Version,
            "--help" | "-h" => return Launch::Help,
            flag if flag.starts_with('-') => return Launch::Refused(arg),
            _ => {}
        }
    }
    Launch::Window
}

/// The flags this binary answers, so a caller does not have to read the source
/// to find out (#2841).
pub(crate) fn print_launch_help() {
    println!(
        "opencrabs-gui {}\n\nUSAGE:\n    opencrabs-gui [--version] [--help]\n\nOPTIONS:\n    -V, --version    Print the version and exit\n    -h, --help       Print this help and exit",
        env!("CARGO_PKG_VERSION")
    );
}
