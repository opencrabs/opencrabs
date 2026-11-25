//! The gate at the head of `run_command` (`dashboard_commands::refused`) and
//! the backend's own answer it asks first (`Backend::lacks`).
//!
//! The palette greys a command out; this is what keeps the same command,
//! typed by hand, from running anyway. A remote crab is a `RemoteCrab` on a
//! port nothing listens on: nothing here dials it.

use std::path::PathBuf;
use std::sync::Arc;

use opencrabs::backend::Scope;
use opencrabs::backend::cap::Cap;
use opencrabs::backend::hello::HelloReply;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::commands::{GuiAction, Outcome, parse_command};
use crate::dashboard_commands::refused;
use crate::text_commands::TextCommand;

fn remote() -> Backend {
    Backend::Remote(Arc::new(RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    )))
}

fn local() -> Backend {
    Backend::local(PathBuf::from("unused.db"))
}

fn action(line: &str) -> GuiAction {
    match parse_command(line) {
        Some(Outcome::Run(action)) | Some(Outcome::RunWith(action, _)) => action,
        other => panic!("{line} did not run in the window: {other:?}"),
    }
}

#[test]
fn a_remote_crab_lacks_what_acts_on_this_machine() {
    let backend = remote();
    for cap in [Cap::Setup, Cap::Logs, Cap::OpenTerminal] {
        let why = backend.lacks(cap).unwrap_or_else(|| panic!("{cap:?}"));
        assert!(why.contains("this machine"), "{cap:?}: {why}");
    }
    assert_eq!(backend.lacks(Cap::OpenExternal), None);
}

#[test]
fn a_local_crab_lacks_nothing() {
    let backend = local();
    for cap in Cap::ALL {
        assert_eq!(backend.lacks(*cap), None, "{cap:?}");
    }
}

#[test]
fn onboard_typed_by_hand_on_a_remote_crab_leaves_a_note_and_does_not_run() {
    // No handshake yet: the capability is the backend's answer, so the gate
    // does not fail open while `hello` is in flight.
    let line = refused(action("/onboard"), &remote(), None).expect("refused");
    assert!(line.starts_with("/onboard: "), "{line}");
    assert!(line.contains("this machine"), "{line}");

    // Every door into the wizard is the same refusal, named by its own door.
    for door in ["/channels", "/voice", "/daemon", "/workspace"] {
        let line = refused(action(door), &remote(), None).expect(door);
        assert!(line.starts_with(&format!("{door}: ")), "{line}");
    }
}

#[test]
fn onboard_on_a_local_crab_runs_as_it_always_has() {
    let hello = HelloReply::for_caller(Scope::ALL, true);
    assert_eq!(refused(action("/onboard"), &local(), None), None);
    assert_eq!(refused(action("/onboard"), &local(), Some(&hello)), None);
}

#[test]
fn an_operation_waits_for_the_handshake_to_refuse() {
    let cd = GuiAction::Text(TextCommand::Cd);
    // Before `hello` the crab's own 403 is the refusal, as before.
    assert_eq!(refused(cd, &remote(), None), None);
    // With a handshake that withholds the scope, the command says why.
    let hello = HelloReply::for_caller(&[Scope::Read], false);
    let line = refused(cd, &remote(), Some(&hello)).expect("needs admin");
    assert!(line.starts_with("/cd: "), "{line}");
    assert!(line.contains("a2a:admin"), "{line}");
    // `/new` is named by the table's own spelling.
    let line = refused(GuiAction::NewSession, &remote(), Some(&hello)).expect("needs manage");
    assert!(line.starts_with("/new: "), "{line}");
}

#[test]
fn a_command_that_needs_nothing_is_never_refused() {
    let hello = HelloReply::for_caller(&[], false);
    assert_eq!(
        refused(GuiAction::OpenThemes, &remote(), Some(&hello)),
        None
    );
    assert_eq!(refused(GuiAction::OpenThemes, &remote(), None), None);
}
