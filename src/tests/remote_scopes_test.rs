use crate::remote::scopes::*;

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
