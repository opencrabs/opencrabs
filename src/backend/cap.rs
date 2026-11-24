//! What a backend can do beyond its operations: the one place a local crab
//! and a remote one are allowed to differ.
//!
//! An operation means the same thing on every transport. A capability is
//! about the machine the window sits on, so a remote backend answers `false`
//! and the surface greys the action out with a reason instead of doing it on
//! the wrong machine.

use serde::{Deserialize, Serialize};

use super::hello::HelloReply;
use super::{BackendError, Scope};

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

/// What an action asks of the crab before the surface lets it be picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The operation called this, at the scope the crab declares for it.
    Op(&'static str),
    /// A capability of the backend the window holds.
    Cap(Cap),
}

/// Why an action is greyed out: the reason the surface prints beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// The caller's token does not hold the scope the operation needs.
    MissingScope { op: String, scope: Scope },
    /// The crab does not declare the operation: it is older than the window.
    OldVersion { op: String, version: String },
    /// The action works on this machine only.
    LocalOnly(Cap),
}

impl Unavailable {
    /// The line the surface prints next to the greyed out action.
    pub fn reason(&self) -> String {
        match self {
            Unavailable::MissingScope { scope, .. } => {
                format!("needs {scope}: this crab's token does not grant it")
            }
            Unavailable::OldVersion { version, .. } if version.is_empty() => {
                "this crab is older than the window and cannot do this".to_string()
            }
            Unavailable::OldVersion { version, .. } => {
                format!("this crab ({version}) is older than the window and cannot do this")
            }
            Unavailable::LocalOnly(_) => "only on a crab on this machine".to_string(),
        }
    }

    /// The reason an operation was refused, when the refusal is about access
    /// rather than about the operation itself.
    ///
    /// A 403 (`Forbidden`) is a missing scope and an unknown name is an older
    /// crab. Anything else is the operation's own failure and keeps its text.
    pub fn from_error(op: &str, err: &BackendError) -> Option<Self> {
        match err {
            BackendError::Forbidden { scope } => Some(Unavailable::MissingScope {
                op: op.to_string(),
                scope: *scope,
            }),
            BackendError::UnknownOp(name) => Some(Unavailable::OldVersion {
                op: name.clone(),
                version: String::new(),
            }),
            _ => None,
        }
    }
}

/// Whether the crab that answered `hello` lets the caller do what `needs`
/// lists, or the first reason it does not.
///
/// Read off the handshake, so the surface greys an action out before it is
/// picked instead of calling it and failing. An empty `needs` always passes.
pub fn check(hello: &HelloReply, needs: &[Need]) -> Result<(), Unavailable> {
    for need in needs {
        match *need {
            Need::Op(name) => {
                let Some(known) = hello.ops.iter().find(|known| known.name == name) else {
                    return Err(Unavailable::OldVersion {
                        op: name.to_string(),
                        version: hello.version.clone(),
                    });
                };
                if !hello.scopes.contains(&known.scope) {
                    return Err(Unavailable::MissingScope {
                        op: name.to_string(),
                        scope: known.scope,
                    });
                }
            }
            Need::Cap(cap) => {
                if !hello.caps.contains(&cap) {
                    return Err(Unavailable::LocalOnly(cap));
                }
            }
        }
    }
    Ok(())
}

/// The notice a surface shows when `op` came back refused.
///
/// A refusal about access says why in the same words the greyed out action
/// would; any other failure keeps the text it always had.
pub fn notice(op: &str, err: &BackendError) -> String {
    match Unavailable::from_error(op, err) {
        Some(why) => format!("{op}: {}", why.reason()),
        None => err.clone().into_message(),
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

    fn remote_hello(scopes: &[Scope]) -> HelloReply {
        HelloReply::for_caller(scopes, false)
    }

    #[test]
    fn a_local_crab_lets_every_operation_and_capability_through() {
        let hello = HelloReply::for_caller(Scope::ALL, true);
        let mut needs: Vec<Need> = super::super::registry()
            .into_iter()
            .map(|(name, _)| Need::Op(name))
            .collect();
        needs.extend(Cap::ALL.iter().copied().map(Need::Cap));
        assert_eq!(check(&hello, &needs), Ok(()));
    }

    #[test]
    fn nothing_needed_is_never_greyed_out() {
        assert_eq!(check(&remote_hello(&[]), &[]), Ok(()));
    }

    #[test]
    fn a_missing_scope_names_the_scope() {
        // `sessions.rename` writes the other crab's ledger: Manage.
        let hello = remote_hello(&[Scope::Read]);
        let why = check(&hello, &[Need::Op("sessions.rename")]).expect_err("read only");
        assert_eq!(
            why,
            Unavailable::MissingScope {
                op: "sessions.rename".to_string(),
                scope: Scope::Manage,
            }
        );
        assert!(why.reason().contains("a2a:manage"), "{}", why.reason());
        // The same token reads.
        assert_eq!(check(&hello, &[Need::Op("sessions.load")]), Ok(()));
    }

    #[test]
    fn an_undeclared_operation_reads_as_an_older_crab() {
        let mut hello = remote_hello(Scope::ALL);
        hello.version = "0.1.0".to_string();
        let why = check(&hello, &[Need::Op("no_such.op")]).expect_err("not declared");
        assert_eq!(
            why,
            Unavailable::OldVersion {
                op: "no_such.op".to_string(),
                version: "0.1.0".to_string(),
            }
        );
        assert!(why.reason().contains("0.1.0"), "{}", why.reason());
        assert!(why.reason().contains("older"), "{}", why.reason());
    }

    #[test]
    fn a_local_only_capability_is_greyed_out_on_a_remote_crab() {
        let hello = remote_hello(Scope::ALL);
        let why = check(&hello, &[Need::Cap(Cap::OpenTerminal)]).expect_err("remote");
        assert_eq!(why, Unavailable::LocalOnly(Cap::OpenTerminal));
        assert!(why.reason().contains("this machine"), "{}", why.reason());
        assert_eq!(check(&hello, &[Need::Cap(Cap::OpenExternal)]), Ok(()));
    }

    #[test]
    fn the_first_missing_need_is_the_reason() {
        let hello = remote_hello(&[Scope::Read]);
        let why = check(
            &hello,
            &[
                Need::Op("sessions.load"),
                Need::Cap(Cap::OpenTerminal),
                Need::Op("sessions.rename"),
            ],
        )
        .expect_err("two needs missing");
        assert_eq!(why, Unavailable::LocalOnly(Cap::OpenTerminal));
    }

    #[test]
    fn a_403_becomes_a_notice_that_names_the_scope() {
        let err = BackendError::Forbidden {
            scope: Scope::Admin,
        };
        let line = notice("session_ops.delete", &err);
        assert!(line.contains("session_ops.delete"), "{line}");
        assert!(line.contains("a2a:admin"), "{line}");
        assert_eq!(
            Unavailable::from_error("session_ops.delete", &err),
            Some(Unavailable::MissingScope {
                op: "session_ops.delete".to_string(),
                scope: Scope::Admin,
            })
        );
    }

    #[test]
    fn an_unknown_operation_becomes_an_older_crab_notice() {
        let err = BackendError::UnknownOp("tasks.snapshot".to_string());
        let line = notice("tasks.snapshot", &err);
        assert!(line.contains("older"), "{line}");
    }

    #[test]
    fn any_other_failure_keeps_its_text() {
        let err = BackendError::Failed("db connect: nope".to_string());
        assert_eq!(Unavailable::from_error("sessions.load", &err), None);
        assert_eq!(notice("sessions.load", &err), "db connect: nope");
    }
}
