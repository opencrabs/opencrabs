use crate::menu::MenuKind;
use opencrabs::backend::cap::Need;
use opencrabs::backend::hello::HelloReply;
use opencrabs::backend::{Op, Scope, sessions};

const ALL_KINDS: [MenuKind; 10] = [
    MenuKind::Pin,
    MenuKind::Rename,
    MenuKind::AssignProject,
    MenuKind::Delete,
    MenuKind::EnableTopics,
    MenuKind::NewTopic,
    MenuKind::DisableTopics,
    MenuKind::GroupTopics,
    MenuKind::SplitSide,
    MenuKind::SplitStack,
];

#[test]
fn every_needed_operation_is_declared_by_the_crab() {
    let declared: Vec<&str> = opencrabs::backend::registry()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for kind in &ALL_KINDS {
        for need in kind.needs() {
            if let Need::Op(op) = need {
                assert!(declared.contains(op), "a menu item needs undeclared {op}");
            }
        }
    }
}

#[test]
fn nothing_is_greyed_out_before_the_handshake_or_on_a_local_crab() {
    let local = HelloReply::for_caller(Scope::ALL, true);
    for kind in &ALL_KINDS {
        assert_eq!(kind.unavailable(None), None);
        assert_eq!(kind.unavailable(Some(&local)), None);
    }
}

#[test]
fn a_read_only_token_greys_out_every_ledger_write() {
    let hello = HelloReply::for_caller(&[Scope::Read], false);
    for kind in &ALL_KINDS {
        let why = kind.unavailable(Some(&hello));
        if kind.needs().is_empty() {
            assert_eq!(why, None);
        } else {
            let why = why.expect("a write needs more than read");
            assert!(why.contains("needs a2a:"), "{why}");
        }
    }
}

#[test]
fn delete_needs_admin_while_rename_needs_manage() {
    let hello = HelloReply::for_caller(&[Scope::Read, Scope::Manage], false);
    assert_eq!(MenuKind::Rename.unavailable(Some(&hello)), None);
    let why = MenuKind::Delete
        .unavailable(Some(&hello))
        .expect("delete is admin");
    assert!(why.contains("a2a:admin"), "{why}");
}

#[test]
fn an_older_crab_greys_out_what_it_does_not_declare() {
    let mut hello = HelloReply::for_caller(Scope::ALL, false);
    hello
        .ops
        .retain(|known| known.name != <sessions::MergeTopics as Op>::NAME);
    let why = MenuKind::DisableTopics
        .unavailable(Some(&hello))
        .expect("not declared");
    assert!(why.contains("older"), "{why}");
    assert_eq!(MenuKind::EnableTopics.unavailable(Some(&hello)), None);
}
