//! Capability scopes for remote A2A access.
//!
//! Four scopes, plain wire strings, stored verbatim on pairing codes and
//! session tokens:
//!
//! | Scope        | Grants                                                  |
//! |--------------|---------------------------------------------------------|
//! | `a2a:read`   | `tasks/get`, `session/notify-status`                     |
//! | `a2a:send`   | `message/send`, `message/stream`, `tasks/cancel`, `session/notify` |
//! | `a2a:manage` | writes to the other crab's ledger (rename, close, topics, undo) |
//! | `a2a:admin`  | model, working directory, settings, shell, deletes       |
//!
//! `a2a:manage` and `a2a:admin` gate no JSON-RPC method of their own; the
//! backend operations that need them declare it on `backend::op::Scope`.
//!
//! Rules:
//! - Scopes only ever narrow: a pairing exchange picks a subset of
//!   [`ALL_SCOPES`], never adds to what the code carried.
//! - [`ELEVATED_SCOPES`] are never granted by default: a pairing code
//!   carries them only when the operator asked for them by name.
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
/// Ledger-write scope: rename, close, favourite, topics, new session, undo.
pub const SCOPE_MANAGE: &str = "a2a:manage";
/// Owner scope: model and provider, working directory, settings, shell,
/// session delete.
pub const SCOPE_ADMIN: &str = "a2a:admin";

/// Every scope the system knows. Mint-time validation rejects anything
/// else so stored state never contains typos that silently grant nothing.
pub const ALL_SCOPES: &[&str] = &[SCOPE_READ, SCOPE_SEND, SCOPE_MANAGE, SCOPE_ADMIN];

/// Scopes a pairing code carries only when the operator named them. Every
/// default the pairing surface offers stays inside the complement.
pub const ELEVATED_SCOPES: &[&str] = &[SCOPE_MANAGE, SCOPE_ADMIN];

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

/// Is `s` a scope that must be asked for by name? Exact match.
pub fn is_elevated(s: &str) -> bool {
    ELEVATED_SCOPES.contains(&s)
}

/// Does a session's granted scope list satisfy `required`?
pub fn allows(granted: &[String], required: &str) -> bool {
    granted.iter().map(String::as_str).any(|g| g == required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::op::Scope;

    #[test]
    fn every_operation_scope_is_a_known_wire_scope() {
        for scope in Scope::ALL {
            assert!(
                is_known_scope(scope.as_str()),
                "{scope} missing from ALL_SCOPES"
            );
        }
    }

    #[test]
    fn every_wire_scope_is_an_operation_scope() {
        for s in ALL_SCOPES {
            assert!(Scope::parse(s).is_some(), "{s} has no backend::op::Scope");
        }
        assert_eq!(ALL_SCOPES.len(), Scope::ALL.len());
    }

    #[test]
    fn manage_and_admin_are_known_and_elevated() {
        assert_eq!(SCOPE_MANAGE, Scope::Manage.as_str());
        assert_eq!(SCOPE_ADMIN, Scope::Admin.as_str());
        for s in [SCOPE_MANAGE, SCOPE_ADMIN] {
            assert!(is_known_scope(s));
            assert!(is_elevated(s));
        }
        assert!(!is_elevated(SCOPE_READ));
        assert!(!is_elevated(SCOPE_SEND));
        assert!(!is_known_scope("a2a:root"));
    }

    #[test]
    fn elevated_scopes_gate_no_json_rpc_method() {
        for m in [
            "message/send",
            "message/stream",
            "tasks/get",
            "tasks/cancel",
            "session/notify",
            "session/notify-status",
        ] {
            let s = scope_for_method(m).expect("known method");
            assert!(!is_elevated(s), "{m} must not need {s}");
        }
    }

    #[test]
    fn holding_admin_does_not_imply_manage() {
        let admin_only = vec![SCOPE_ADMIN.to_string()];
        assert!(allows(&admin_only, SCOPE_ADMIN));
        assert!(!allows(&admin_only, SCOPE_MANAGE));
        assert!(!allows(&admin_only, SCOPE_READ));
    }
}
