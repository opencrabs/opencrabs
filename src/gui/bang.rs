//! The window side of the `!` operator (#2437): runs the command off the
//! window thread and reports the output. Parsing is in `ui_core::bang`.

use opencrabs::ui_core::bang::{is_editor, output_text};

/// Run a bang command off the window thread and report its output as a
/// `BangOutput` event. The command runs in the launch directory, as the terminal's
/// does, through the platform shell.
pub fn spawn(cmd: String, sink: crate::agent_bridge::EventSink) {
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-bang".into())
        .spawn(move || {
            let text = if is_editor(&cmd) {
                open_in_terminal(&cmd)
            } else {
                run_shell(&cmd)
            };
            if let Err(e) = sink.send(crate::agent_bridge::TurnEvent::BangOutput(text)) {
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
