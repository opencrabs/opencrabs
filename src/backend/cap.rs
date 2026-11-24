//! What a backend can do beyond its operations: the one place a local crab
//! and a remote one are allowed to differ.
//!
//! An operation means the same thing on every transport. A capability is
//! about the machine the window sits on, so a remote backend answers `false`
//! and the surface greys the action out with a reason instead of doing it on
//! the wrong machine.

use serde::{Deserialize, Serialize};

/// A thing only some backends can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// Open a terminal in the session's folder (`bang.rs::open_in_terminal`).
    /// Only local: a remote crab's folder is not on this machine.
    OpenTerminal,
    /// Open a file in an external app. Remote does it on the cached copy.
    OpenExternal,
}

impl Cap {
    /// Every capability.
    pub const ALL: &'static [Cap] = &[Cap::OpenTerminal, Cap::OpenExternal];

    /// Whether a backend in this process (`local`) or across the wire has it.
    pub fn available(self, local: bool) -> bool {
        match self {
            Cap::OpenTerminal => local,
            Cap::OpenExternal => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_backend_has_every_capability() {
        assert!(Cap::ALL.iter().all(|cap| cap.available(true)));
    }

    #[test]
    fn a_remote_backend_cannot_open_a_terminal() {
        assert!(!Cap::OpenTerminal.available(false));
        assert!(Cap::OpenExternal.available(false));
    }
}
