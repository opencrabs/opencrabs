//! The window side of the `!` operator (#2437): runs the command off the
//! window thread and reports the output. Parsing is in `ui_core::bang`.

use opencrabs::ui_core::bang::{editor_command, output_text};
use uuid::Uuid;

/// Run a bang command off the window thread and report its output as a
/// `BangOutput` event. The command runs in the launch directory, as the terminal's
/// does, through the platform shell.
///
/// The session is handed along with the output (#3160). The command runs on
/// its own thread and takes as long as it takes, so the window may well be
/// painting another conversation by the time it answers. Without the id the
/// output had nowhere to go but the chat on screen, which is how a pane's
/// `!cmd` ended up in a neighbour's chat.
///
/// An editor is not run at all (#3163). The window has a buffer of its own,
/// so `!vi` opens a dialog that edits and saves, the way the terminal's own
/// editor behaves. The request goes out as an `EditorRequest` carrying the
/// session id, because the dialog has to open on the pane that asked for it
/// and read a relative path from that conversation's own working directory.
/// The old detour through Terminal.app is gone: the editor never left the
/// machine's window in the first place, so opening another app for it was a
/// worse version of the same thing.
pub fn spawn(cmd: String, session: Option<Uuid>, sink: crate::agent_bridge::EventSink) {
    if let Some(command) = editor_command(&cmd) {
        let event = crate::agent_bridge::TurnEvent::EditorRequest { session, command };
        if let Err(e) = sink.send(event) {
            crate::logging::error(&format!("editor request not delivered: {e}"));
        }
        return;
    }
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-bang".into())
        .spawn(move || {
            let text = run_shell(&cmd);
            if let Err(e) = sink.send(crate::agent_bridge::TurnEvent::BangOutput { session, text })
            {
                crate::logging::error(&format!("bang output not delivered: {e}"));
            }
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("bang thread: {e}"));
    }
}

fn run_shell(cmd: &str) -> String {
    use opencrabs::utils::shell::PushShellCommand;
    let (shell, shell_arg) = opencrabs::utils::shell::shell_pair();
    let cwd = opencrabs::utils::cwd::launch_cwd();
    match std::process::Command::new(shell)
        .push_shell_command(shell_arg, cmd)
        .current_dir(&cwd)
        .output()
    {
        Ok(out) => format!(
            "$ {cmd}\n{}",
            output_text(&out.stdout, &out.stderr, out.status.success())
        ),
        Err(e) => format!("$ {cmd}\ncould not start: {e}"),
    }
}
