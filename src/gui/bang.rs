//! The window side of the `!` operator (#2437): asks the crab to run the
//! command and reports the output. Parsing is in `ui_core::bang`; the run is
//! the `shell.run` operation, so a remote crab runs it on its own machine.

use opencrabs::backend::Op;
use opencrabs::backend::cap::{self, Cap};
use opencrabs::backend::shell::ShellRun;
use opencrabs::ui_core::bang::is_editor;

use crate::agent_bridge::{EventSink, TurnEvent};
use crate::backend_glue::Backend;

/// Run a bang command off the window thread and report its output as a
/// `BangOutput` event. The command runs on the crab the window holds, in that
/// crab's launch directory, through its platform shell.
///
/// An editor needs a terminal on this machine, so it opens here only when the
/// crab is this machine's; a remote crab answers with the reason instead. A
/// crab that refuses the shell (no admin scope) answers with the same reason
/// the greyed out action would give.
pub fn spawn(backend: &Backend, cmd: String, sink: EventSink) {
    if is_editor(&cmd) {
        if let Some(why) = backend.lacks(Cap::OpenTerminal) {
            deliver(&sink, format!("$ {cmd}\n{why}"));
            return;
        }
        let spawned = std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .name("gui-bang".into())
            .spawn(move || deliver(&sink, open_in_terminal(&cmd)));
        if let Err(e) = spawned {
            crate::logging::error(&format!("bang thread: {e}"));
        }
        return;
    }
    let done_sink = sink.clone();
    let line = cmd.clone();
    let spawned = backend.spawn(ShellRun { cmd }, move |reply| {
        let text = match reply {
            Ok(text) => text,
            Err(e) => format!("$ {line}\n{}", cap::notice(ShellRun::NAME, &e)),
        };
        deliver(&done_sink, text);
    });
    if let Err(e) = spawned {
        crate::logging::error(&format!("bang thread: {e}"));
    }
}

fn deliver(sink: &EventSink, text: String) {
    if let Err(e) = sink.send(TurnEvent::BangOutput(text)) {
        crate::logging::error(&format!("bang output not delivered: {e}"));
    }
}

/// An editor needs a real terminal, which the window does not have. On macOS
/// the command opens in Terminal.app in the launch directory; elsewhere the
/// chat says so, and the user runs it in their own terminal.
fn open_in_terminal(cmd: &str) -> String {
    let dir = opencrabs::utils::cwd::launch_cwd();
    if cfg!(target_os = "macos") {
        let shell_line = format!(
            "cd '{}' && {}",
            dir.display().to_string().replace('\'', "'\\''"),
            cmd
        );
        let escaped = shell_line.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("tell application \"Terminal\" to do script \"{escaped}\"");
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status()
        {
            Ok(s) if s.success() => format!("$ {cmd}\nopened in Terminal"),
            Ok(_) => format!("$ {cmd}\nTerminal did not open the editor"),
            Err(e) => format!("$ {cmd}\ncould not reach Terminal: {e}"),
        }
    } else {
        format!(
            "$ {cmd}\neditors need a terminal: run it in {}",
            dir.display()
        )
    }
}
