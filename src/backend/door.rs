//! Which door an operation came in by.
//!
//! An operation runs the same whether the window calls it or a remote
//! surface does, but a few answers depend on who is asking: a remote turn
//! asks its surface before a tool runs, the window's turn never did. The door
//! travels with the call as a task-local, so no signature carries it and an
//! operation that never asks sees nothing new.
//!
//! The remote door sets it around the operation it dispatches. Everything
//! else, the window included, runs with no scope and reads [`Door::Local`].

use std::future::Future;

/// The door an operation came in by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// The crab's own process: the window, a channel, a test calling `run`.
    Local,
    /// A `gui/call` from a remote surface.
    Remote,
}

tokio::task_local! {
    static DOOR: Door;
}

/// The door of the operation running on this task. With no scope active,
/// [`Door::Local`]: only the remote door sets one.
pub fn current() -> Door {
    DOOR.try_with(|door| *door).unwrap_or(Door::Local)
}

/// Run `f` as an operation that came in by the remote door.
pub async fn remote<F: Future>(f: F) -> F::Output {
    DOOR.scope(Door::Remote, f).await
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
}
