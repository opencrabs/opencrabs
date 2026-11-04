//! Editor handoff (#1744).
//!
//! The bang operator pipes a child's stdout/stderr through a buffer
//! (`tokio::process::Command::output()`), so a full-screen editor like `vi`
//! never gets a real tty — it prints "Output is not to a terminal" and dumps
//! raw escape sequences into the chat.
//!
//! This module gives the editor the *actual* terminal instead: it hands the
//! tty over to an allowlisted editor (or the user's `$EDITOR`/`$VISUAL`),
//! waits for the child to exit, then re-establishes TUI ownership. The pure helpers (`handoff_target`,
//! `resolve_editor`, `decode_wait_status`) carry no side effects so they can
//! be unit-tested without a real terminal.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::events::{EventHandler, TuiEvent};
use super::runner::{self, TuiTerminal};

/// Bang commands whose argv[0] matches one of these are routed to the
/// terminal-handoff path instead of pipe-capture. Matching is on the
/// *basename*, so `/usr/bin/nvim` hands off while `myvi-helper` still does
/// not (the comparison stays exact; a substring match would exec the wrong
/// thing).
///
/// The set covers what people actually run (#2173): `nvim` and `emacsclient`
/// were missing, and `helix`/`hx`/`micro` are the common non-vi editors.
pub(crate) const EDITOR_ALLOWLIST: &[&str] = &[
    "vi",
    "vim",
    "nvim",
    "nano",
    "emacs",
    "emacsclient",
    "helix",
    "hx",
    "micro",
];

/// The last path component of `token`, so `/usr/bin/nvim` and `./vim` compare
/// equal to `nvim` and `vim`.
fn basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

/// Return the editor name if `cmd`'s argv[0] is one, else None.
///
/// A command hands off when its basename is in [`EDITOR_ALLOWLIST`], or when
/// it matches the basename of `env_editor` (`$EDITOR`/`$VISUAL`): the user
/// chose that program, so honouring it adds no attack surface. The env path
/// is a name comparison, not a blanket pass, so `!myvi-helper` still pipes.
pub(crate) fn handoff_target(cmd: &str, env_editor: Option<&str>) -> Option<String> {
    let argv0 = basename(cmd.split_whitespace().next()?);
    if let Some(ed) = EDITOR_ALLOWLIST.iter().copied().find(|ed| *ed == argv0) {
        return Some(ed.to_string());
    }
    let env_base = basename(env_editor?.split_whitespace().next()?);
    (!env_base.is_empty() && env_base == argv0).then(|| env_base.to_string())
}

/// Split a bang command into `(program, args)`. Intentionally minimal:
/// whitespace-split, no shell quoting (the allowlist restricts `program` to a
/// handful of editors, and their file arguments rarely need embedded spaces).
pub(crate) fn split_argv(cmd: &str) -> Option<(String, Vec<String>)> {
    let mut it = cmd.split_whitespace();
    let program = it.next()?.to_string();
    let args = it.map(str::to_string).collect();
    Some((program, args))
}

/// Resolve an editor binary via `path_dirs` (an explicit `$PATH`-shaped string
/// so the lookup is testable) falling back to the ambient `$PATH`. Returns the
/// absolute path, or None when the binary isn't installed.
pub(crate) fn resolve_editor(name: &str, path_dirs: Option<&str>, cwd: &Path) -> Option<PathBuf> {
    let effective;
    let path: Option<&str> = match path_dirs {
        Some(p) => Some(p),
        None => {
            effective = std::env::var("PATH").unwrap_or_default();
            Some(effective.as_str())
        }
    };
    which::which_in(name, path, cwd).ok()
}

/// Outcome of a `waitpid` status word.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
    /// Exited normally with this code.
    Exited(i32),
    /// Killed by this signal.
    Signaled(i32),
    /// Stopped by this signal (Ctrl+Z / SIGTSTP) — treated as exit by the
    /// handoff so the TUI never hangs waiting on a suspended child.
    Stopped(i32),
}

/// Decode a raw wait status. The POSIX encoding (shared by Linux and macOS):
/// low 7 bits == 0 → exited; == 0x7f → stopped; otherwise → signaled.
pub(crate) fn decode_wait_status(status: i32) -> WaitOutcome {
    if status & 0x7f == 0 {
        WaitOutcome::Exited((status >> 8) & 0xff)
    } else if status & 0x7f == 0x7f {
        WaitOutcome::Stopped((status >> 8) & 0xff)
    } else {
        WaitOutcome::Signaled(status & 0x7f)
    }
}

/// What the handoff did, for the caller to surface as a system message.
#[derive(Debug)]
pub(crate) struct EditorResult {
    pub text: String,
}

/// Hand the terminal to a full-screen editor and block until it exits.
///
/// Steps: stop the event reader (so it can't race the child for stdin),
/// restore the real terminal, `exec` the editor with inherited stdio, reap it
/// with a non-blocking `waitpid` loop (a stop is coerced into SIGCONT+SIGTERM
/// so a mid-edit Ctrl+Z can't hang the TUI), then re-init the terminal and
/// restart the reader. `listener_handle` is re-bound to the fresh reader task.
pub(crate) async fn run_editor_handoff(
    terminal: &mut TuiTerminal,
    event_sender: mpsc::UnboundedSender<TuiEvent>,
    listener_handle: &mut JoinHandle<()>,
    cmd: &str,
    cwd: &Path,
) -> EditorResult {
    // 1. Resolve the editor binary BEFORE touching the terminal, so a missing
    //    editor leaves the TUI completely undisturbed.
    let (program, args) = match split_argv(cmd) {
        Some(pair) => pair,
        None => {
            return EditorResult {
                text: "empty editor command".to_string(),
            };
        }
    };
    let resolved = match resolve_editor(&program, None, cwd) {
        Some(p) => p,
        None => {
            return EditorResult {
                text: format!("{program} not found in PATH — editor handoff aborted."),
            };
        }
    };

    // 2. Stop the event reader so it cannot consume the editor's keystrokes.
    listener_handle.abort();

    // 3. Hand the real tty over to the child.
    runner::force_restore_terminal();

    // 4. Spawn the editor with all three stdio streams inherited.
    let spawn = std::process::Command::new(&resolved)
        .args(&args)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();

    let child = match spawn {
        Ok(c) => c,
        Err(e) => {
            let r = finish_handoff(terminal, event_sender, listener_handle);
            return EditorResult {
                text: if let Err(e2) = r {
                    format!("failed to start {program}: {e}; terminal restore failed: {e2}")
                } else {
                    format!("failed to start {program}: {e}")
                },
            };
        }
    };

    // 5. Reap the child ourselves with WUNTRACED so a stop is observable.
    //    std::Child::drop on unix does not reap; we own the pid now.
    let pid = child.id() as libc::pid_t;
    std::mem::forget(child);
    reap_editor(pid).await;

    // 6. Re-establish TUI ownership and the event reader.
    let result = finish_handoff(terminal, event_sender, listener_handle);
    let text = format!("{} exited.", program);
    match result {
        Ok(()) => EditorResult { text },
        Err(e) => EditorResult {
            text: format!("{text} (terminal re-init failed: {e})"),
        },
    }
}

/// Non-blocking reap loop with stop coercion. Returns once the child has
/// exited. A `Stopped` outcome sends SIGCONT, then SIGTERM with a bounded
/// grace that escalates to SIGKILL, so the TUI never blocks on a suspended
/// or SIGTERM-deferring editor.
/// `pub(crate)` for testing: a real stopped child must not hang this reap.
pub(crate) async fn reap_editor(pid: libc::pid_t) {
    let mut status: libc::c_int = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
        if r == pid {
            match decode_wait_status(status) {
                WaitOutcome::Stopped(_sig) => {
                    // Coerce a mid-edit Ctrl+Z into a full exit: SIGCONT so a
                    // pending signal can land, then SIGTERM for a polite
                    // death. A shell waiting on a foreground job DEFERS
                    // SIGTERM (this exact child hung the original reap), so
                    // the polite path gets a bounded grace and then escalates
                    // to SIGKILL — a stubborn child must never hang the TUI.
                    unsafe {
                        libc::kill(pid, libc::SIGCONT);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                    if !reap_exit(pid, std::time::Duration::from_secs(1)).await {
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                        }
                        reap_exit(pid, std::time::Duration::from_secs(2)).await;
                    }
                    return;
                }
                // Exited or signaled — both mean the child is gone.
                _ => return,
            }
        }
        // r == 0 → still running; r < 0 → error (treat as gone to avoid spin).
        if r < 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Plain exit-only reap (no WUNTRACED) with a bounded wait, used after a
/// coerced stop. Returns true when the child is reaped (or already gone),
/// false when it outlived `timeout`.
async fn reap_exit(pid: libc::pid_t, timeout: std::time::Duration) -> bool {
    let mut status: libc::c_int = 0;
    let start = tokio::time::Instant::now();
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid || r < 0 {
            return true; // reaped (or error — either way, gone)
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Re-init the terminal and restart the event reader, re-binding
/// `listener_handle` to the fresh task.
fn finish_handoff(
    terminal: &mut TuiTerminal,
    event_sender: mpsc::UnboundedSender<TuiEvent>,
    listener_handle: &mut JoinHandle<()>,
) -> anyhow::Result<()> {
    runner::reinit_terminal_for_editor(terminal)?;
    *listener_handle = EventHandler::start_terminal_listener(event_sender);
    Ok(())
}
