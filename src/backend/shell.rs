//! Running a shell command on the crab.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::ui_core::bang::output_text;
use crate::utils::shell::PushShellCommand;

use super::{BackendError, Ctx, Op, Scope};

/// The most of each stream (stdout, stderr) a [`ShellRun`] answers with. The
/// rest is read and dropped, so the command never stalls on a full pipe, and
/// the answer says it was cut.
pub const OUTPUT_CAP: usize = 64 * 1024;

/// How long a [`ShellRun`] may take before the crab kills it.
pub const TIME_LIMIT: Duration = Duration::from_secs(120);

/// The window's `!cmd` (#2437), run on the crab through the platform shell in
/// the crab's launch directory, the way the terminal runs it. The answer is
/// the chat's text for it: the command line, then its output.
///
/// Admin: this is a shell on the crab's machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellRun {
    /// The command after the `!`.
    pub cmd: String,
}

impl Op for ShellRun {
    const NAME: &'static str = "shell.run";
    const SCOPE: Scope = Scope::Admin;
    type Reply = String;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let cwd = crate::utils::cwd::launch_cwd();
        Ok(run_capped(&self.cmd, &cwd, OUTPUT_CAP, TIME_LIMIT).await)
    }
}

/// Run `cmd` in `cwd` through the platform shell and word its output for the
/// chat. Each stream keeps at most `cap` bytes; past `limit` the process is
/// killed and the answer says so.
pub async fn run_capped(cmd: &str, cwd: &Path, cap: usize, limit: Duration) -> String {
    let (shell, shell_arg) = crate::utils::shell::shell_pair();
    let spawned = tokio::process::Command::new(shell)
        .push_shell_command(shell_arg, cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return format!("$ {cmd}\ncould not start: {e}"),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let finished = tokio::time::timeout(limit, async {
        tokio::join!(capped(stdout, cap), capped(stderr, cap), child.wait())
    })
    .await;
    match finished {
        Ok(((out, out_cut), (err, err_cut), status)) => {
            let success = status.map(|s| s.success()).unwrap_or(false);
            let mut text = format!("$ {cmd}\n{}", output_text(&out, &err, success));
            if out_cut || err_cut {
                text.push_str(&format!("\n(output cut at {} KiB)", cap / 1024));
            }
            text
        }
        Err(_) => {
            let _ = child.kill().await;
            format!(
                "$ {cmd}\ntimed out after {} s: the command was killed",
                limit.as_secs()
            )
        }
    }
}

/// Read `stream` to its end, keeping the first `cap` bytes. Whether anything
/// past them was dropped comes back beside them.
async fn capped<R: AsyncRead + Unpin>(stream: Option<R>, cap: usize) -> (Vec<u8>, bool) {
    let Some(mut stream) = stream else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut cut = false;
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if n > room {
                    cut = true;
                }
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    (kept, cut)
}

ops! { ShellRun }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[tokio::test]
    async fn shell_run_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            ShellRun {
                cmd: "echo parity".into(),
            }
        );
        assert_eq!(answer["Ok"], serde_json::json!("$ echo parity\nparity"));
    }

    #[tokio::test]
    async fn shell_run_needs_admin() {
        // A shell on the crab's machine: nothing less than admin runs it.
        let gateway = Gateway::start().await;
        forbidden!(
            gateway,
            ShellRun {
                cmd: "echo refused".into(),
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_under_the_cap_reads_as_it_always_did() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = run_capped("echo out; echo err 1>&2", dir.path(), 1024, TIME_LIMIT).await;
        assert_eq!(text, "$ echo out; echo err 1>&2\nout\nerr");
        let text = run_capped("true", dir.path(), 1024, TIME_LIMIT).await;
        assert_eq!(text, "$ true\n(no output)");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_past_the_cap_is_cut_and_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cmd = "head -c 5000 /dev/zero | tr '\\0' x";
        let text = run_capped(cmd, dir.path(), 2048, TIME_LIMIT).await;
        let body = text
            .strip_prefix(&format!("$ {cmd}\n"))
            .expect("the command line leads");
        let (kept, note) = body.split_once('\n').expect("a note after the output");
        assert_eq!(kept, "x".repeat(2048));
        assert_eq!(note, "(output cut at 2 KiB)");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_past_the_limit_is_killed_and_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = std::time::Instant::now();
        let text = run_capped("sleep 30", dir.path(), 1024, Duration::from_secs(1)).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the limit stopped the wait"
        );
        assert_eq!(text, "$ sleep 30\ntimed out after 1 s: the command was killed");
    }
}
