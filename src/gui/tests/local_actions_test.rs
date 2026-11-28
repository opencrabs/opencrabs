//! What the window still does on this machine by itself, and the gate that
//! keeps it from doing so for a crab elsewhere (P11).
//!
//! The wizard, `/restart` and the agent reload after a saved setup all act on
//! this process: its config, its service, its binary. With a remote crab none
//! of them may run here. A remote crab is a `RemoteCrab` on a port nothing
//! listens on: nothing here dials it.

use std::path::PathBuf;
use std::sync::Arc;

use opencrabs::backend::Scope;
use opencrabs::backend::hello::HelloReply;
use opencrabs::ui_core::commands::GUI_SUPPORTED;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::commands::{GuiAction, Outcome, parse_command};
use crate::dashboard_commands::refused;
use crate::dashboard_models::reloads_here;

const MODELS: &str = include_str!("../dashboard_models.rs");
const BRIDGE: &str = include_str!("../agent_bridge.rs");

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

/// The text of `fn name` in `src`, from its signature to the next function at
/// the same depth. Split on lines, not sliced at byte offsets.
fn fn_body(src: &str, name: &str) -> String {
    let head = format!("fn {name}(");
    let (_, rest) = src
        .split_once(head.as_str())
        .unwrap_or_else(|| panic!("no fn {name} in the source"));
    let mut body = String::new();
    for line in rest.lines() {
        let next_fn = line.starts_with("    pub(crate) fn ")
            || line.starts_with("    fn ")
            || line.starts_with("fn ")
            || line.starts_with("async fn ")
            || line.starts_with("pub fn ")
            || line.starts_with("pub(crate) fn ");
        if !body.is_empty() && next_fn {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

#[test]
fn restart_typed_on_a_remote_crab_leaves_a_note_and_does_not_cycle_this_process() {
    // No handshake yet: the gate is the backend's own answer, so it does not
    // fail open while `hello` is in flight.
    let line = refused(action("/restart"), &remote(), None).expect("refused");
    assert!(line.starts_with("/restart: "), "{line}");
    assert!(line.contains("this machine"), "{line}");
    let hello = HelloReply::for_caller(Scope::ALL, false);
    assert!(refused(action("/restart"), &remote(), Some(&hello)).is_some());
}

#[test]
fn restart_on_a_local_crab_runs_as_it_always_has() {
    let hello = HelloReply::for_caller(Scope::ALL, true);
    assert_eq!(refused(action("/restart"), &local(), None), None);
    assert_eq!(refused(action("/restart"), &local(), Some(&hello)), None);
}

#[test]
fn every_door_into_the_wizard_is_refused_for_a_remote_crab_and_open_locally() {
    let doors: Vec<&str> = GUI_SUPPORTED
        .iter()
        .copied()
        .filter(|name| {
            matches!(
                parse_command(name),
                Some(Outcome::Run(GuiAction::OpenOnboard(_)))
            )
        })
        .collect();
    assert!(doors.contains(&"/onboard"), "{doors:?}");
    for door in doors {
        let line = refused(action(door), &remote(), None).unwrap_or_else(|| panic!("{door}"));
        assert!(line.starts_with(&format!("{door}: ")), "{line}");
        assert_eq!(refused(action(door), &local(), None), None, "{door}");
    }
}

#[test]
fn whisper_is_the_window_s_own_mic_and_is_not_gated() {
    // The mic floats over this machine's screen whichever crab is held.
    assert_eq!(refused(action("/whisper"), &remote(), None), None);
    assert_eq!(refused(action("/whisper"), &local(), None), None);
}

#[test]
fn a_saved_setup_reloads_the_agent_here_only_for_a_local_crab() {
    assert!(reloads_here(&local()));
    assert!(!reloads_here(&remote()));
}

#[test]
fn reload_agent_asks_before_it_reads_this_machine_s_config() {
    let reload = fn_body(MODELS, "reload_agent");
    assert!(reload.contains("reloads_here(&self.backend)"), "{reload}");
    assert!(!reload.contains("Config::load"), "{reload}");
    assert!(!reload.contains("spawn_bootstrap"), "{reload}");
    let local = fn_body(MODELS, "reload_local_agent");
    assert!(local.contains("Config::load"), "{local}");
    assert!(local.contains("spawn_bootstrap"), "{local}");
}

#[test]
fn whisper_launch_is_the_only_spawn_in_the_text_command_runner() {
    let runner = fn_body(BRIDGE, "run_text_command");
    assert!(!runner.contains("Command::new"), "{runner}");
    assert!(runner.contains("launch_whispercrabs(&binary)"), "{runner}");
    let launch = fn_body(BRIDGE, "launch_whispercrabs");
    assert!(
        launch.contains("std::process::Command::new(binary)"),
        "{launch}"
    );
}
