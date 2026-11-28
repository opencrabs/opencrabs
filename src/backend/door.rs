//! Which door an operation came in by, and who is asking at it.
//!
//! An operation runs the same whether the window calls it or a remote
//! surface does, but a few answers depend on who is asking: a remote turn
//! asks its surface before a tool runs, the window's turn never did. The door
//! travels with the call as a task-local, so no signature carries it and an
//! operation that never asks sees nothing new.
//!
//! The remote door sets it around the operation it dispatches, with the
//! [`Caller`] its credential names: the principal and the scopes it carries.
//! Everything else, the window included, runs with no scope, reads
//! [`Door::Local`] and has no caller.

use std::future::Future;

use super::Scope;

/// The door an operation came in by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// The crab's own process: the window, a channel, a test calling `run`.
    Local,
    /// A `gui/call` from a remote surface.
    Remote,
}

/// Who called through the remote door.
///
/// `principal` names the credential (a pairing's id, `api_key`, or the open
/// loopback posture) so a turn it starts answers to it alone: only the same
/// principal approves that turn's tools or cancels it. `scopes` are the ones
/// the credential carries, which is more than the scope the operation itself
/// needed to pass the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub principal: String,
    pub scopes: Vec<Scope>,
}

impl Caller {
    /// Whether the credential carries `scope`. Flat and exact, like the gate.
    pub fn holds(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

tokio::task_local! {
    static CALLER: Caller;
}

/// The door of the operation running on this task. With no scope active,
/// [`Door::Local`]: only the remote door sets one.
pub fn current() -> Door {
    if CALLER.try_with(|_| ()).is_ok() {
        Door::Remote
    } else {
        Door::Local
    }
}

/// The remote caller of the operation running on this task, `None` at the
/// local door.
pub fn caller() -> Option<Caller> {
    CALLER.try_with(Caller::clone).ok()
}

/// Run `f` as an operation that came in by the remote door for `caller`.
pub async fn remote_as<F: Future>(caller: Caller, f: F) -> F::Output {
    CALLER.scope(caller, f).await
}

/// Run `f` by the remote door for a caller holding every scope, as the
/// tests that predate [`Caller`] call it.
#[cfg(test)]
pub async fn remote<F: Future>(f: F) -> F::Output {
    remote_as(
        Caller {
            principal: "test".to_string(),
            scopes: Scope::ALL.to_vec(),
        },
        f,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_any_scope_the_door_is_local() {
        assert_eq!(current(), Door::Local);
    }

    #[tokio::test]
    async fn inside_remote_the_door_is_remote_and_only_there() {
        assert_eq!(remote(async { current() }).await, Door::Remote);
        assert_eq!(current(), Door::Local, "the scope ended with the future");
    }

    #[tokio::test]
    async fn a_task_spawned_from_a_remote_call_does_not_inherit_it() {
        let spawned = remote(async { tokio::spawn(async { current() }).await })
            .await
            .expect("task ran");
        assert_eq!(spawned, Door::Local);
    }

    #[tokio::test]
    async fn the_caller_is_the_one_the_door_was_opened_for() {
        assert_eq!(caller(), None, "the local door has no caller");
        let alice = Caller {
            principal: "alice".to_string(),
            scopes: vec![Scope::Read, Scope::Send],
        };
        let seen = remote_as(alice.clone(), async { (current(), caller()) }).await;
        assert_eq!(seen, (Door::Remote, Some(alice)));
        assert_eq!(caller(), None, "the scope ended with the future");
    }

    #[test]
    fn a_caller_holds_only_the_scopes_it_carries() {
        let send_only = Caller {
            principal: "loopback-open".to_string(),
            scopes: vec![Scope::Read, Scope::Send],
        };
        assert!(send_only.holds(Scope::Send));
        assert!(!send_only.holds(Scope::Manage));
        assert!(!send_only.holds(Scope::Admin));
    }
}
