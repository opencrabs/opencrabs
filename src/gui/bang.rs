//! The window side of the `!` operator (#2437): runs the command off the
//! window thread and reports the output. Parsing is in `ui_core::bang`.

use opencrabs::backend::Op;
use opencrabs::backend::cap::{self, Cap};
use opencrabs::backend::shell::ShellRun;
use opencrabs::ui_core::bang::editor_command;
use uuid::Uuid;

use crate::agent_bridge::{EventSink, TurnEvent};
use crate::backend_glue::Backend;

/// Run a bang command off the window thread and report its output as a
/// `BangOutput` event. The command runs on the crab the window holds, in that
/// crab's launch directory, through its platform shell. A crab that refuses
/// the shell (no admin scope) answers with the same reason the greyed out
/// action would give.
///
/// The session is handed along with the output (#3160). The command runs off
/// the window thread and takes as long as it takes, so the window may well be
/// painting another conversation by the time it answers. Without the id the
/// output had nowhere to go but the chat on screen, which is how a pane's
/// `!cmd` ended up in a neighbour's chat.
///
/// An editor is not run at all (#3163). The window has a buffer of its own,
/// so `!vi` opens a dialog that edits and saves, the way the terminal's own
/// editor behaves. The request goes out as an `EditorRequest` carrying the
/// session id, because the dialog has to open on the pane that asked for it
/// and read a relative path from that conversation's own working directory.
/// The dialog reads and writes this machine's disk, so it opens only when the
/// crab is this machine's; a remote crab answers with the reason instead.
pub fn spawn(backend: &Backend, cmd: String, session: Option<Uuid>, sink: EventSink) {
    if let Some(command) = editor_command(&cmd) {
        if let Some(why) = backend.lacks(Cap::OpenTerminal) {
            deliver(&sink, session, format!("$ {cmd}\n{why}"));
            return;
        }
        let event = TurnEvent::EditorRequest { session, command };
        if let Err(e) = sink.send(event) {
            crate::logging::error(&format!("editor request not delivered: {e}"));
        }
        return;
    }
    let done_sink = sink.clone();
    let line = cmd.clone();
    let spawned = backend.spawn(run_shell(&cmd), move |reply| {
        let text = match reply {
            Ok(text) => text,
            Err(e) => format!("$ {line}\n{}", cap::notice(ShellRun::NAME, &e)),
        };
        deliver(&done_sink, session, text);
    });
    if let Err(e) = spawned {
        crate::logging::error(&format!("bang thread: {e}"));
    }
}

/// The operation that runs `cmd` on the crab: its shell, its launch
/// directory, its output caps.
fn run_shell(cmd: &str) -> ShellRun {
    ShellRun {
        cmd: cmd.to_string(),
    }
}

fn deliver(sink: &EventSink, session: Option<Uuid>, text: String) {
    if let Err(e) = sink.send(TurnEvent::BangOutput { session, text }) {
        crate::logging::error(&format!("bang output not delivered: {e}"));
    }
}
