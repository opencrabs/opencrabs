//! Capability scopes for remote A2A access.
//!
//! Two scopes, plain wire strings, stored verbatim on pairing codes and
//! session tokens:
//!
//! | Scope      | Grants                                                  |
//! |------------|---------------------------------------------------------|
//! | `a2a:read` | `tasks/get`, `session/notify-status`                     |
//! | `a2a:send` | `message/send`, `message/stream`, `tasks/cancel`, `session/notify` |
//!
//! Rules (t3code invariants, adapted):
//! - Scopes only ever narrow: a pairing exchange picks a subset of
//!   [`ALL_SCOPES`], never adds to what the code carried.
//! - The config `[a2a].api_key` bearer implicitly holds every scope
//!   (full access, backward compatible with existing callers).
//! - Flat strings, exact membership: no wildcards, no hierarchy, no
//!   implication graph. A scope check is one string comparison.
//! - Unknown JSON-RPC methods get no scope gate here; the dispatcher
//!   rejects them with `MethodNotFound` regardless of credentials.

/// Read-only scope: fetch task state, check notification delivery.
pub const SCOPE_READ: &str = "a2a:read";
/// Operate scope: send/stream messages, cancel tasks, push notifications.
pub const SCOPE_SEND: &str = "a2a:send";

/// Every scope the system knows. Mint-time validation rejects anything
/// else so stored state never contains typos that silently grant nothing.
pub const ALL_SCOPES: &[&str] = &[SCOPE_READ, SCOPE_SEND];

/// The scope required by one JSON-RPC method, or `None` when the method
/// needs no scope gate (unknown methods: the dispatcher rejects them).
///
/// Keep in sync with the method surface in `src/a2a/handler/dispatch.rs`
/// and the SSE path in `src/a2a/server.rs`. The exhaustiveness test below
/// is the guard.
pub fn scope_for_method(method: &str) -> Option<&'static str> {
    match method {
        "tasks/get" | "session/notify-status" => Some(SCOPE_READ),
        "message/send" | "message/stream" | "tasks/cancel" | "session/notify" => Some(SCOPE_SEND),
        _ => None,
    }
}

/// Is `s` a scope this system knows? Case-sensitive, exact match.
pub fn is_known_scope(s: &str) -> bool {
    ALL_SCOPES.contains(&s)
}

/// Does a session's granted scope list satisfy `required`?
pub fn allows(granted: &[String], required: &str) -> bool {
    granted.iter().map(String::as_str).any(|g| g == required)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The complete dispatched method surface at time of writing:
    /// dispatch.rs (message/send, tasks/get, tasks/cancel, session/notify,
    /// session/notify-status) + server.rs SSE (message/stream).
    const KNOWN_METHODS: &[&str] = &[
        "message/send",
        "message/stream",
        "tasks/get",
        "tasks/cancel",
        "session/notify",
        "session/notify-status",
    ];

    #[test]
    fn every_dispatched_method_has_a_scope() {
        for m in KNOWN_METHODS {
            assert!(
                scope_for_method(m).is_some(),
                "{m} missing from scope table"
            );
        }
    }

    #[test]
    fn unknown_methods_are_not_gated_here() {
        assert_eq!(scope_for_method("agent/info"), None);
        assert_eq!(scope_for_method("nonexistent/method"), None);
        assert_eq!(scope_for_method(""), None);
    }

    #[test]
    fn scope_assignment_matrix() {
        assert_eq!(scope_for_method("tasks/get"), Some(SCOPE_READ));
        assert_eq!(scope_for_method("session/notify-status"), Some(SCOPE_READ));
        assert_eq!(scope_for_method("message/send"), Some(SCOPE_SEND));
        assert_eq!(scope_for_method("message/stream"), Some(SCOPE_SEND));
        assert_eq!(scope_for_method("tasks/cancel"), Some(SCOPE_SEND));
        assert_eq!(scope_for_method("session/notify"), Some(SCOPE_SEND));
    }

    #[test]
    fn known_scopes_validate() {
        assert!(is_known_scope(SCOPE_READ));
        assert!(is_known_scope(SCOPE_SEND));
        assert!(!is_known_scope("a2a:admin"));
        assert!(!is_known_scope("a2a:*"));
        assert!(!is_known_scope(""));
        assert!(!is_known_scope("A2A:READ"));
    }

    #[test]
    fn allows_is_exact_membership() {
        let read_only = vec![SCOPE_READ.to_string()];
        let full = vec![SCOPE_READ.to_string(), SCOPE_SEND.to_string()];
        assert!(allows(&read_only, SCOPE_READ));
        assert!(!allows(&read_only, SCOPE_SEND));
        assert!(allows(&full, SCOPE_READ));
        assert!(allows(&full, SCOPE_SEND));
        assert!(!allows(&[], SCOPE_READ));
    }

    #[test]
    fn every_known_method_maps_to_a_known_scope() {
        for m in KNOWN_METHODS {
            let s = scope_for_method(m).expect("known method");
            assert!(is_known_scope(s), "{m} maps to unknown scope {s}");
        }
    }
}
