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
//! Rules:
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
