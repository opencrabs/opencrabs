//! Channel ownership, as the GUI reports it (#2452).
//!
//! The channels (Telegram, Discord, Slack, WhatsApp, Trello) are served by
//! the running instance: the daemon or the TUI. The GUI alone never spawns
//! them, so when it runs by itself the bots go quiet. This module is the
//! pure model behind the quiet chip that says so: who owns the channels
//! right now, and the one line the chip draws. Both frontends can share it;
//! the file reads it can stay in the caller.

/// Who is serving the configured channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// A live instance holds the locks: the bots are answering.
    Running,
    /// Lock files exist but name no live process: the instance that owned
    /// them is gone and took the bots with it.
    Stale,
    /// No locks at all: nothing has claimed the channels on this machine.
    Unowned,
}

/// Classify channel ownership from two cheap probes.
///
/// `lock_present`: at least one channel token lock file exists on disk.
/// `live_responder`: the instance lock names a live process (the daemon or
/// TUI that would run the channels).
pub fn ownership(lock_present: bool, live_responder: bool) -> Ownership {
    match (lock_present, live_responder) {
        (false, _) => Ownership::Unowned,
        (true, true) => Ownership::Running,
        (true, false) => Ownership::Stale,
    }
}

/// The line the chip draws, or `None` for no chip at all.
///
/// No chip when every channel is being served (`Running`) or when nothing
/// is configured: the chip exists to explain silence, not to decorate.
/// The line names the channels and the action, in that order.
pub fn status_line(channels: &[&str], own: &Ownership) -> Option<String> {
    if channels.is_empty() {
        return None;
    }
    let listed = channels.join(", ");
    match own {
        Ownership::Running => None,
        Ownership::Stale => Some(format!(
            "{listed} configured · the instance serving them closed · start the daemon"
        )),
        Ownership::Unowned => Some(format!(
            "{listed} configured · nothing is serving them · start the daemon"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_instance_means_running() {
        assert_eq!(ownership(true, true), Ownership::Running);
    }

    #[test]
    fn locks_without_a_live_owner_mean_stale() {
        assert_eq!(ownership(true, false), Ownership::Stale);
    }

    #[test]
    fn no_locks_mean_unowned_even_if_something_else_is_live() {
        assert_eq!(ownership(false, false), Ownership::Unowned);
        assert_eq!(ownership(false, true), Ownership::Unowned);
    }
}
