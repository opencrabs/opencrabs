//! Bash/Shell Command Execution Tool
//!
//! Allows executing shell commands in the system.

use super::error::{Result, ToolError, expand_tilde};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::utils::long_command::Detach;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

// ── TOML blocklist (runtime-configurable safety rules) ─────────────
// Loaded once from ~/.opencrabs/safety/bash_blocklist.toml on first use.
// Adds patterns on top of the hardcoded blocklist. The hardcoded rules
// are the immovable floor; TOML adds runtime-configurable patterns.

#[derive(Debug, Deserialize)]
pub(crate) struct TomlBlocklist {
    #[allow(dead_code)]
    pub(crate) meta: Option<toml::Value>,
    #[serde(default)]
    pub(crate) rules: Vec<TomlBlockRule>,
    #[serde(default)]
    pub(crate) overrides: Vec<TomlOverride>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TomlBlockRule {
    #[allow(dead_code)]
    pub(crate) id: String,
    #[allow(dead_code)]
    pub(crate) category: String,
    pub(crate) severity: String,
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TomlOverride {
    #[allow(dead_code)]
    pub(crate) id: String,
    #[allow(dead_code)]
    pub(crate) reason: String,
    pub(crate) patterns: Vec<String>,
}

/// The TOML blocklist, reloaded when the file changes on disk.
///
/// Was a `OnceLock`, which read the file once per process and made the
/// "editable at runtime, no rebuild" promise false: a new pattern needed a
/// restart. Keyed on mtime + length now, so an edit takes effect on the next
/// bash call (#851). The hardcoded blocklist is always active; this only adds.
fn toml_blocklist() -> Option<std::sync::Arc<TomlBlocklist>> {
    static BLOCKLIST: OnceLock<Option<super::toml_hot_reload::HotToml<TomlBlocklist>>> =
        OnceLock::new();
    BLOCKLIST
        .get_or_init(|| {
            super::toml_hot_reload::safety_path("bash_blocklist.toml")
                .map(|p| super::toml_hot_reload::HotToml::new(p, "TOML blocklist"))
        })
        .as_ref()?
        .get()
}

/// Check the command against the TOML blocklist. Returns `Some(reason)`
/// if blocked, `None` if allowed. Overrides are checked first: if any
/// override pattern matches, the command is allowed even if a rule also
/// matches. Rules use simple substring matching on normalized (lowercase,
/// whitespace-collapsed) input.
fn check_toml_blocklist(command: &str) -> Option<String> {
    let blocklist = toml_blocklist()?;
    evaluate_blocklist(&blocklist, command)
}

/// Pure match of `command` against an already-loaded blocklist.
///
/// Split from the loader so the precedence rules can be tested without a home
/// directory or a file on disk. Overrides are checked first: an override match
/// allows the command even when a rule also matches. It can only relax TOML
/// rules — the hardcoded blocklist runs before this is ever reached.
///
/// Matching is substring on normalized (lowercased, whitespace-collapsed)
/// input, so a pattern is exactly as precise as the string chosen for it.
pub(crate) fn evaluate_blocklist(blocklist: &TomlBlocklist, command: &str) -> Option<String> {
    let normalized: String = command
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .to_lowercase();

    for ov in &blocklist.overrides {
        if ov
            .patterns
            .iter()
            .any(|p| normalized.contains(&p.to_lowercase()))
        {
            return None;
        }
    }

    for rule in &blocklist.rules {
        if rule.severity != "block" || rule.patterns.is_empty() {
            continue;
        }
        if rule
            .patterns
            .iter()
            .any(|p| normalized.contains(&p.to_lowercase()))
        {
            return Some(format!("{} [runtime rule: {}]", rule.reason, rule.id));
        }
    }

    None
}

/// Detach a child from the controlling terminal before `exec`.
///
/// Without this, programs that bypass `stdin` and open `/dev/tty` directly
/// (ssh password prompt, sudo's getpass fallback, gnu readline) steal the
/// parent's TTY — when the parent is the OpenCrabs TUI, that means the
/// child reads the user's keystrokes, switches the terminal into no-echo
/// mode, and emits escape sequences into the rendered chat (cursor keys
/// turn into `[[[[[`). Restart fixes it because the TTY mode is reset.
///
/// `setsid()` puts the child in a brand-new session with no controlling
/// TTY, so `open("/dev/tty")` inside the child returns `ENXIO` and the
/// program either errors out cleanly or falls back to a non-TTY path
/// (e.g. SSH consults `SSH_ASKPASS`). The TUI is never touched.
#[cfg(unix)]
fn detach_session_pre_exec(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach_session_pre_exec(_cmd: &mut Command) {
    // No-op on Windows — the TTY-bleed problem doesn't apply (different
    // console model) and pre_exec is a Unix-only API.
}

/// Inject session identity and context environment variables into the command.
fn apply_context_env(cmd: &mut Command, context: &ToolExecutionContext) {
    for (k, v) in &context.env_vars {
        cmd.env(k, v);
    }
    cmd.env("OPENCRABS_SESSION_ID", context.session_id.to_string());
}

/// First-token of a shell command, normalized for SSH detection.
fn first_token(cmd: &str) -> &str {
    cmd.split([';', '|', '&'])
        .next()
        .unwrap_or(cmd)
        .split_whitespace()
        .find(|tok| !tok.contains('='))
        .unwrap_or("")
}

/// Pretty-print the SSH/scp/rsync target so the password dialog has a
/// recognisable hostname. Returns `None` for non-SSH commands and for
/// any SSH invocation that already carries a non-interactive auth
/// hint (`-o BatchMode=yes`, `-o PreferredAuthentications=publickey`)
/// — in those cases we let the command fail naturally rather than
/// prompting.
pub(crate) fn parse_ssh_invocation(command: &str) -> Option<String> {
    let cmd = command.trim();
    let first = first_token(cmd);
    let is_ssh_like = matches!(first, "ssh" | "scp" | "sftp" | "rsync");
    if !is_ssh_like {
        return None;
    }

    // ssh-keygen / ssh-keyscan / ssh-add never prompt for a remote
    // password — skip them.
    if first == "ssh" {
        let second = cmd.split_whitespace().nth(1).unwrap_or("");
        if second.is_empty() || second.starts_with('-') {
            // ok
        } else if second.contains('@') || !second.contains('-') {
            // looks like a host
        }
    }

    let lower = cmd.to_lowercase();
    if lower.contains("batchmode=yes")
        || lower.contains("preferredauthentications=publickey")
        || lower.contains("passwordauthentication=no")
    {
        return None;
    }

    // Best-effort host extraction: first token that contains '@' or
    // looks like host:path (scp/rsync) and isn't a flag.
    for tok in cmd.split_whitespace().skip(1) {
        if tok.starts_with('-') {
            continue;
        }
        if tok.contains('@') {
            return Some(tok.to_string());
        }
        if tok.contains(':') && !tok.starts_with('/') {
            return Some(tok.to_string());
        }
    }
    // Fallback: just the bare hostname after `ssh`
    if first == "ssh" {
        for tok in cmd.split_whitespace().skip(1) {
            if !tok.starts_with('-') {
                return Some(tok.to_string());
            }
        }
    }
    Some(format!("(unknown {} target)", first))
}

/// Inject `-o BatchMode=yes -o ConnectTimeout=15` after the leading
/// `ssh`/`scp`/`sftp`/`rsync` token so we can probe whether key auth
/// works without ever blocking on a TTY prompt. Returns the rewritten
/// command, leaving the rest of the args untouched.
fn inject_batch_mode(command: &str) -> String {
    let trimmed_start = command.trim_start();
    let leading_ws = &command[..command.len() - trimmed_start.len()];

    // Find the end of the first token (the binary name).
    let first_end = trimmed_start
        .find(char::is_whitespace)
        .unwrap_or(trimmed_start.len());
    let (head, tail) = trimmed_start.split_at(first_end);

    let probe_opts = " -o BatchMode=yes -o ConnectTimeout=15 -o StrictHostKeyChecking=accept-new";
    format!("{}{}{}{}", leading_ws, head, probe_opts, tail)
}

/// Heuristic: did this stderr come from an SSH/scp/rsync auth failure
/// (vs. a network or hostname error)? We only retry the password flow
/// for genuine auth-rejected cases.
fn ssh_stderr_is_auth_failure(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("permission denied")
        || s.contains("password:")
        || s.contains("host key verification failed")
        || s.contains("publickey,password")
        || s.contains("publickey,keyboard-interactive")
        || s.contains("no supported authentication methods")
        || (s.contains("ssh:") && s.contains("authentication"))
}

/// Bash execution tool
pub struct BashTool;

#[derive(Debug, Deserialize, Serialize)]
struct BashInput {
    /// Command to execute
    command: String,

    /// Optional working directory (overrides context)
    #[serde(skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,

    /// Optional timeout in seconds (overrides context default)
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,

    /// Optional flag to run detached in background (true) or force inline (false)
    #[serde(skip_serializing_if = "Option::is_none")]
    background: Option<bool>,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Returns stdout, stderr, and exit code. \
         stdin is /dev/null — interactive commands (git add -p, git rebase -i, \
         bare editors/pagers/TUIs like vim/less/top, bare REPLs like python/node) \
         will not work; the non-interactive forms of those same tools are fine \
         (`less FILE`, `top -b -n 1`, `man TOPIC`, `tmux -V`, `node --version`). \
         That check is advisory rather than a boundary — it matches the command \
         word as written, so an absolute path such as /usr/bin/less FILE is not \
         intercepted. Use non-interactive flags (-A, -m, --no-edit) or pipe \
         input via heredoc/echo. \
         Each call is a fresh shell — `cd` does not persist across calls; chain with \
         `&&` or use `git -C <path> <cmd>` for cross-directory work. Use carefully \
         as this can modify system state. \
         \n\nSHELL DIALECT: command strings run under a real `bash` when the host has \
         one (probed once at first use), and fall back to POSIX `sh` — dash on \
         Debian/Ubuntu — where it does not. Bash-only syntax (`$'…'` ANSI-C quoting, \
         `[[ ]]`, arrays, `<<<`, `${PIPESTATUS}`, `set -o pipefail`) is therefore the \
         normal case but is NOT guaranteed: on a host with no bash those constructs \
         either error or, worse, silently misparse. If a command must work everywhere, \
         write it in POSIX. The dialect in force is logged once per process. \
         \n\nGITHUB OPERATIONS: use the `gh` CLI via this tool for \
         everything GitHub — issues, PRs, releases, comments, file \
         fetches, repo / code search, workflow runs, checks. When \
         installed, `gh` returns structured JSON (--json flag) and \
         respects --jq for filtering. Check availability first with \
         `gh --version` if unsure, since it is NOT preinstalled on stock \
         Windows or macOS. Without it, `http_request` reads the public \
         REST API fine, but anything that writes (opening an issue, \
         commenting, merging) needs the token `gh` would have carried, so \
         report that `gh` is missing instead of failing quietly. Never reach \
         for `browser_navigate` to inspect or act on a GitHub URL. \
         Examples: `gh pr view 123 --json title,body,comments`, \
         `gh issue list --label bug --json number,title`, \
         `gh api repos/OWNER/REPO/commits/SHA/check-runs`."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional: Working directory for command execution"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional: Timeout in seconds (default 120, max 600). Use higher values for builds."
                },
                "background": {
                    "type": "boolean",
                    "description": "Optional: Set to true to run the command in the background (detached), or false to force inline execution. If omitted, uses automatic heuristic detection."
                }
            },
            "required": ["command"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecuteShell,
            ToolCapability::SystemModification,
            ToolCapability::Network,
        ]
    }

    fn requires_approval(&self) -> bool {
        true // Shell execution always requires approval
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: BashInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;

        if input.command.trim().is_empty() {
            return Err(ToolError::InvalidInput(
                "Command cannot be empty".to_string(),
            ));
        }

        // Hard blocklist — these commands are NEVER allowed, even if the user
        // accidentally approves them. This is a last line of defense against
        // catastrophic, irreversible operations.
        if let Some(reason) = check_blocked_command(&input.command) {
            return Err(ToolError::InvalidInput(format!(
                "Blocked: {}. This command is on the hard blocklist and cannot be executed.",
                reason
            )));
        }

        // Runtime TOML blocklist — additional patterns loaded from
        // ~/.opencrabs/safety/bash_blocklist.toml. Editable at runtime
        // with approval gate. Checked AFTER the hardcoded rules so the
        // compiled floor always applies. Overrides in the TOML can allow
        // commands that would otherwise match a TOML rule (but NOT the
        // hardcoded rules).
        if let Some(reason) = check_toml_blocklist(&input.command) {
            return Err(ToolError::InvalidInput(format!(
                "Blocked: {}. This command matches a runtime blocklist rule.",
                reason
            )));
        }

        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: BashInput = serde_json::from_value(input)?;

        // #2123: no counter-waiting while this session has a detached task in
        // flight. Such a task resumes the session by itself on completion, so
        // a `sleep` only burns time that belongs to other work. Checked here
        // and not in `validate_input`, because only the execution context can
        // see the background manager. With nothing detached the command runs
        // as ordinary shell, which is what `sleep` is normally for.
        if let Some(mgr) = context.background_manager.as_ref()
            && mgr.running_for(context.session_id) > 0
        {
            let labels: Vec<String> = mgr
                .running_tasks(context.session_id)
                .into_iter()
                .map(|task| task.label)
                .collect();
            if let Some(msg) = sleep_refusal(&input.command, &labels) {
                return Ok(ToolResult::error(msg));
            }
        }

        // Determine working directory
        let working_dir = if let Some(ref dir) = input.working_dir {
            // Expand a leading `~` before the exists-check: PathBuf::from("~")
            // is a relative path named `~` that never exists (#1165).
            let expanded = expand_tilde(dir);
            if dir.starts_with('~') {
                tracing::info!("bash working_dir expanded: {dir} -> {}", expanded.display());
            }
            expanded
        } else {
            context.working_dir()
        };

        // Verify working directory exists
        if !working_dir.exists() {
            let msg = format!(
                "Working directory does not exist: {}",
                working_dir.display()
            );
            record_bash_outcome(
                context.session_id,
                input.command.clone(),
                true,
                Some(msg.clone()),
            );
            return Ok(ToolResult::error(msg));
        }

        // Layer 3: short-circuit if the agent just ran this exact
        // command and it failed. Stops the same-command-twice loop
        // dead with a strong "stop retrying" message instead of
        // letting the underlying error fire a second time.
        if let Some(msg) = check_recent_failure(context.session_id, &input.command) {
            record_bash_block(context.session_id, &input.command);
            return Ok(ToolResult::error(msg));
        }

        // Layer 1: refuse interactive commands. With stdin=/dev/null
        // (the post-2026-04-23 stdin-detach fix) most of these exit
        // cleanly on EOF — looks like success, did nothing — and the
        // agent gets stuck retrying. Surface the failure clearly with
        // a non-interactive alternative so the loop breaks on attempt 1.
        if let Some(hint) = check_interactive_command(&input.command) {
            record_bash_outcome(
                context.session_id,
                input.command.clone(),
                true,
                Some(hint.to_string()),
            );
            return Ok(ToolResult::error(hint.to_string()));
        }

        // Prepare command for the current platform (one shared source of
        // truth — the same pair every detached/child spawn uses)
        let (shell, shell_arg) = crate::utils::shell::shell_pair();

        // Determine timeout: use input override if provided, else context default, cap at 600s
        let effective_timeout = input.timeout_secs.unwrap_or(context.timeout_secs).min(600);

        // Genuinely long tasks (builds, test runs, CI waits, renders) run
        // DETACHED and resume the session on completion instead of churning
        // toward the 600s cap (#722). Only when a surface wired the background
        // manager; sudo needs the inline password flow, so it's excluded.
        // Anything else falls through and runs inline exactly as before.
        // #1195: pure workers (allow_nested=false) lose detachment too -
        // their long commands run inline so nothing outlives their verdict.
        // Positional detection, not a prefix test: `cd /tmp && sudo foo` and
        // `sudo<TAB>foo` are sudo invocations too, and they used to fall
        // through to the inline path with no password flow at all (#2157).
        let sudo_invocations = find_sudo_invocations(&input.command);
        let is_sudo = !sudo_invocations.is_empty();
        let nesting_ok = context
            .subagent_manager
            .as_ref()
            .map(|m| m.nesting_allowed_for_session(context.session_id))
            .unwrap_or(true);

        // Check explicit background override or heuristic auto-detection
        match input.background {
            Some(false) => {
                tracing::debug!(
                    target: "background_task",
                    "Forced inline execution (background: false) for session {}",
                    context.session_id
                );
            }
            Some(true) => {
                if is_sudo {
                    return Ok(ToolResult::error(
                        "Cannot run sudo commands in the background (password prompt requires inline execution)"
                            .to_string(),
                    ));
                }
                if !nesting_ok {
                    return Ok(ToolResult::error(
                        "Background execution is disabled in pure worker sessions (allow_nested=false)"
                            .to_string(),
                    ));
                }
                let Some(ref mgr) = context.background_manager else {
                    return Ok(ToolResult::error(
                        "Background execution is unavailable on this surface (no background task manager)"
                            .to_string(),
                    ));
                };

                let label =
                    crate::brain::agent::service::background_tasks::short_label(&input.command);
                tracing::info!(
                    target: "background_task",
                    "Explicitly detaching '{label}' for session {} (background: true)",
                    context.session_id
                );
                mgr.clone().spawn_command(
                    context.session_id,
                    context.working_directory.clone(),
                    label.clone(),
                    input.command.clone(),
                    context.progress_callback.clone(),
                );
                return Ok(ToolResult::success(format!(
                    "Started in the background: {label}\n\nThis is a long-running task, so it \
                     is running detached. It reports back on its own: the completion is \
                     delivered to this session when it finishes, and the agent turn reports \
                     the outcome. The task row also settles in place on surfaces that draw \
                     rows, and the full output is in the task status file that tasks_list \
                     reads. No need to poll or wait. Do other independent work meanwhile if \
                     there is any."
                )));
            }
            None => {
                if let Some(ref mgr) = context.background_manager
                    && !is_sudo
                    && nesting_ok
                {
                    match crate::utils::long_command::classify(&input.command) {
                        Detach::Yes { marker } => {
                            let label = crate::brain::agent::service::background_tasks::short_label(
                                &input.command,
                            );
                            tracing::info!(
                                target: "background_task",
                                "Detaching '{label}' for session {}: '{marker}' starts a command",
                                context.session_id
                            );
                            mgr.clone().spawn_command(
                                context.session_id,
                                context.working_directory.clone(),
                                label.clone(),
                                input.command.clone(),
                                context.progress_callback.clone(),
                            );
                            return Ok(ToolResult::success(format!(
                                "Started in the background: {label}\n\nThis is a long-running task, so it \
                                 is running detached. It reports back on its own: the completion is \
                                 delivered to this session when it finishes, and the agent turn reports \
                                 the outcome. The task row also settles in place on surfaces that draw \
                                 rows, and the full output is in the task status file that tasks_list \
                                 reads. No need to poll or wait. Do other independent work meanwhile if \
                                 there is any."
                            )));
                        }
                        // Worth a line: the command looks long to a reader, and the
                        // reason it ran inline is not visible anywhere else.
                        Detach::Mentioned { marker } => tracing::debug!(
                            target: "background_task",
                            "Running inline: '{marker}' appears only as data (heredoc body or quoted \
                             argument), not as a command"
                        ),
                        Detach::No => {}
                    }
                }
            }
        }

        // Request the sudo password only when sudo will really prompt (#2157).
        // Three things make the dialog pointless, and each one used to open it
        // anyway: the invocation already carries `-n` (sudo fails instead of
        // asking), a passwordless rule covers the user, or the credential is
        // still cached. A dialog over a prompt that never appears leaves the
        // turn waiting until the surface times out, which is the dead turn in
        // the report. All three checks are cheap; the wait is not.
        let sudo_password = if sudo_invocations.iter().any(|i| !i.never_prompts) {
            if sudo_needs_no_password(context.session_id).await {
                None
            } else if let Some(ref callback) = context.sudo_callback {
                match callback(input.command.clone()).await {
                    Ok(Some(password)) => Some(password),
                    Ok(None) => return Ok(ToolResult::error("Sudo cancelled by user".to_string())),
                    Err(e) => return Ok(ToolResult::error(format!("Sudo prompt failed: {}", e))),
                }
            } else {
                None // No callback — run normally (will fail if password needed)
            }
        } else {
            None
        };

        // Execute command with timeout — use piped stdin for sudo password
        // RTK rewrite tracking (hoisted from normal-execution branch)
        #[cfg(feature = "rtk")]
        let mut rtk_was_rewritten = false;

        let output = if let Some(password) = sudo_password {
            // Ask sudo to read the password from stdin and keep its own prompt
            // off the screen. The flags go right after the `sudo` word that was
            // detected, so a heredoc body or a quoted string that merely
            // contains `sudo ` is left alone (#2157).
            let sudo_cmd = inject_sudo_password_flags(&input.command, &sudo_invocations);

            // Tree-kill anchor: capture the child pid so the timeout arm can
            // taskkill /T the whole tree, not just the shell process (H-02).
            let sudo_child_pid = std::sync::atomic::AtomicU32::new(0);
            let command_future = async {
                use crate::utils::shell::PushShellCommand;
                let mut cmd = Command::new(shell);
                // Reap the child if this future is dropped. A timeout drops it, and
                // tokio leaves the process running unless told otherwise (#1046).
                cmd.kill_on_drop(true);
                // Verbatim command line: .arg()'s MSVC escaping corrupts quoted
                // commands once cmd.exe re-parses them (H-05).
                cmd.push_shell_command(shell_arg, &sudo_cmd)
                    .current_dir(&working_dir)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                apply_context_env(&mut cmd, context);
                detach_session_pre_exec(&mut cmd);
                let mut child = cmd.spawn()?;
                sudo_child_pid.store(
                    child.id().unwrap_or(0),
                    std::sync::atomic::Ordering::Relaxed,
                );

                // Write password to stdin and close it
                if let Some(mut stdin) = child.stdin.take() {
                    if let Err(e) = stdin.write_all(format!("{}\n", password).as_bytes()).await {
                        tracing::warn!(error = %e, "failed to write password to bash stdin");
                    }
                    drop(stdin);
                }

                child.wait_with_output().await
            };

            match timeout(Duration::from_secs(effective_timeout), command_future).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    return Ok(ToolResult::error(format!(
                        "Command execution failed: {}",
                        e
                    )));
                }
                Err(_) => {
                    // kill_on_drop reaped the shell; the grandchildren (the
                    // actual work processes) survive it — sweep the tree (H-02).
                    crate::utils::shell::kill_process_tree(
                        sudo_child_pid.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    return Err(ToolError::Timeout(effective_timeout));
                }
            }
        } else if let Some(ssh_target) = parse_ssh_invocation(&input.command) {
            // SSH-like commands need special handling: pre_exec/setsid means
            // the child has no TTY, so an interactive password prompt would
            // exit immediately with "no controlling terminal". We probe with
            // BatchMode=yes first (key-only auth) and fall back to the
            // password callback + SSH_ASKPASS if the probe rejects auth.
            let probe_cmd = inject_batch_mode(&input.command);
            // Tree-kill anchor for the timeout arm (H-02).
            let probe_child_pid = std::sync::atomic::AtomicU32::new(0);
            let probe_future = async {
                use crate::utils::shell::PushShellCommand;
                let mut cmd = Command::new(shell);
                // Reap the child if this future is dropped. A timeout drops it, and
                // tokio leaves the process running unless told otherwise (#1046).
                cmd.kill_on_drop(true);
                // Verbatim command line (H-05).
                cmd.push_shell_command(shell_arg, &probe_cmd)
                    .current_dir(&working_dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                apply_context_env(&mut cmd, context);
                detach_session_pre_exec(&mut cmd);
                let child = cmd.spawn()?;
                probe_child_pid.store(
                    child.id().unwrap_or(0),
                    std::sync::atomic::Ordering::Relaxed,
                );
                child.wait_with_output().await
            };

            let probe_output =
                match timeout(Duration::from_secs(effective_timeout), probe_future).await {
                    Ok(Ok(o)) => o,
                    Ok(Err(e)) => {
                        return Ok(ToolResult::error(format!(
                            "Command execution failed: {}",
                            e
                        )));
                    }
                    Err(_) => {
                        // Sweep the orphaned process tree (H-02).
                        crate::utils::shell::kill_process_tree(
                            probe_child_pid.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        return Err(ToolError::Timeout(effective_timeout));
                    }
                };

            let probe_stderr = String::from_utf8_lossy(&probe_output.stderr).to_string();
            let probe_succeeded = probe_output.status.success();
            let auth_failed = !probe_succeeded && ssh_stderr_is_auth_failure(&probe_stderr);

            if probe_succeeded || !auth_failed {
                probe_output
            } else if let Some(ref callback) = context.ssh_callback {
                // Auth failed and a password callback is wired — request the
                // password from the user, then retry with SSH_ASKPASS pointing
                // at a script that emits the password to stdout.
                let prompt = format!(
                    "{} ({})",
                    ssh_target,
                    input.command.split_whitespace().next().unwrap_or("ssh")
                );
                let password = match callback(prompt).await {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        return Ok(ToolResult::error(
                            "SSH password cancelled by user".to_string(),
                        ));
                    }
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "SSH password prompt failed: {}",
                            e
                        )));
                    }
                };

                let askpass = match SshAskpass::new(&password) {
                    Ok(a) => a,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "Failed to set up SSH_ASKPASS: {}",
                            e
                        )));
                    }
                };

                // Tree-kill anchor for the timeout arm (H-02).
                let retry_child_pid = std::sync::atomic::AtomicU32::new(0);
                let retry_future = async {
                    use crate::utils::shell::PushShellCommand;
                    let mut cmd = Command::new(shell);
                    // Reap the child if this future is dropped. A timeout drops it, and
                    // tokio leaves the process running unless told otherwise (#1046).
                    cmd.kill_on_drop(true);
                    // Verbatim command line (H-05).
                    cmd.push_shell_command(shell_arg, &input.command)
                        .current_dir(&working_dir)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .env("SSH_ASKPASS", askpass.script_path())
                        .env("SSH_ASKPASS_REQUIRE", "force")
                        // SSH_ASKPASS_REQUIRE=force on modern OpenSSH ignores
                        // DISPLAY, but older builds (Debian 11, macOS preinstalled)
                        // still gate on it being non-empty. Keep both happy.
                        .env("DISPLAY", ":0");
                    apply_context_env(&mut cmd, context);
                    detach_session_pre_exec(&mut cmd);
                    let child = cmd.spawn()?;
                    retry_child_pid.store(
                        child.id().unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    child.wait_with_output().await
                };

                match timeout(Duration::from_secs(effective_timeout), retry_future).await {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        return Ok(ToolResult::error(format!("SSH retry failed: {}", e)));
                    }
                    Err(_) => {
                        // Sweep the orphaned process tree (H-02).
                        crate::utils::shell::kill_process_tree(
                            retry_child_pid.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        return Err(ToolError::Timeout(effective_timeout));
                    }
                }
            } else {
                // No password callback wired (channel session) — return the
                // probe output as-is so the agent sees the auth-failure
                // stderr and can either retry with explicit `-i <key>` or
                // ask the user out of band.
                probe_output
            }
        } else {
            // Normal execution (no sudo password needed).
            // stdin is set to /dev/null to detach from the parent TTY — when
            // the TUI is running with mouse capture enabled, leaving stdin
            // inherited lets subshells (pipes, `read`, `cat`) swallow
            // mouse-report escape sequences off the terminal and emit them
            // on stdout, where they land in the tool-output buffer and
            // leak into the rendered TUI message.
            // setsid() additionally puts the child in a fresh session
            // with no controlling TTY — programs that bypass stdin and
            // open /dev/tty directly (ssh prompt, sudo getpass) cannot
            // steal the user's keyboard or corrupt the TUI.

            // RTK rewriting: if the rtk feature is enabled and RTK is available,
            // try to rewrite the command to save tokens (60-90% reduction).
            #[cfg(feature = "rtk")]
            let execution_command = if crate::rtk::is_rtk_available().await {
                match crate::rtk::rewrite_command(&input.command).await {
                    Some(result) => {
                        tracing::debug!(
                            "RTK rewrote command: '{}' -> '{}'",
                            input.command,
                            result.rewritten_command
                        );
                        rtk_was_rewritten = result.was_rewritten;
                        result.rewritten_command
                    }
                    None => input.command.clone(),
                }
            } else {
                input.command.clone()
            };

            #[cfg(not(feature = "rtk"))]
            let execution_command = input.command.clone();

            // Tree-kill anchor for the timeout arm (H-02).
            let exec_child_pid = std::sync::atomic::AtomicU32::new(0);
            let command_future = async {
                use crate::utils::shell::PushShellCommand;
                let mut cmd = Command::new(shell);
                // Reap the child if this future is dropped. A timeout drops it, and
                // tokio leaves the process running unless told otherwise (#1046).
                cmd.kill_on_drop(true);
                // Verbatim command line (H-05). This is the main execution path:
                // every quoted command the model sends flows through here.
                cmd.push_shell_command(shell_arg, &execution_command)
                    .current_dir(&working_dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                apply_context_env(&mut cmd, context);
                detach_session_pre_exec(&mut cmd);
                let child = cmd.spawn()?;
                exec_child_pid.store(
                    child.id().unwrap_or(0),
                    std::sync::atomic::Ordering::Relaxed,
                );
                child.wait_with_output().await
            };

            match timeout(Duration::from_secs(effective_timeout), command_future).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    return Ok(ToolResult::error(format!(
                        "Command execution failed: {}",
                        e
                    )));
                }
                Err(_) => {
                    // kill_on_drop reaped cmd.exe; cargo/cmake/clang-style
                    // grandchildren survive it and keep holding file locks
                    // (target/, .cargo cache) — sweep the tree (H-02).
                    crate::utils::shell::kill_process_tree(
                        exec_child_pid.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    return Err(ToolError::Timeout(effective_timeout));
                }
            }
        };

        // Convert output to strings
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // RTK metrics tracking: record that the command was rewritten.
        // We can't measure "original" tokens without running the command twice,
        // so we record the filtered output size. RTK's own `gain` command tracks
        // the actual savings vs what the unfiltered output would have been.
        #[cfg(feature = "rtk")]
        {
            if rtk_was_rewritten {
                let filtered_output = format!("{}\n{}", stdout, stderr);
                let filtered_tokens = filtered_output.split_whitespace().count();

                let savings = crate::rtk::TokenSavings {
                    command: input.command.clone(),
                    rewritten_command: format!("rtk {}", input.command),
                    original_tokens: 0,
                    filtered_tokens,
                    tokens_saved: 0,
                    savings_percent: 0.0,
                    timestamp: chrono::Utc::now(),
                };

                crate::rtk::global_tracker().record_savings(savings).await;

                tracing::info!(
                    "RTK rewrote command ({} filtered tokens): {}",
                    filtered_tokens,
                    input.command
                );
            }
        }

        // Build output message
        let mut result_text = String::new();

        if !stdout.is_empty() {
            result_text.push_str("STDOUT:\n");
            result_text.push_str(&stdout);
        }

        if !stderr.is_empty() {
            if !result_text.is_empty() {
                result_text.push_str("\n\n");
            }
            result_text.push_str("STDERR:\n");
            result_text.push_str(&stderr);
        }

        if result_text.is_empty() {
            result_text = "(no output)".to_string();
        }

        // grep/rg exit 1 with nothing on stderr means "no line matched" — a
        // successful search with zero results, not a command failure (real
        // errors surface as exit 2+ and/or stderr output). Treating that as a
        // failure inflated the bash failure rate with search noise (#663).
        let search_no_match = is_search_no_match(&input.command, exit_code, &stderr);
        let success = output.status.success() || search_no_match;

        if search_no_match && result_text == "(no output)" {
            result_text = "(no matches found)".to_string();
        }

        // A surface with no dialog (a channel, the daemon) cannot answer sudo's
        // prompt, so the command dies on "a password is required" and the turn
        // ends with nowhere to go. Name the one-time fix in the error instead of
        // leaving the user to guess (#2157).
        if is_sudo
            && !success
            && context.sudo_callback.is_none()
            && looks_like_sudo_password_failure(&stderr)
        {
            result_text.push_str("\n\n");
            result_text.push_str(sudo_access_hint_cached());
        }

        // Record the outcome so a follow-up call with the exact same
        // command can be short-circuited by Layer 3 instead of re-running.
        let error_snippet = if !success {
            Some(result_text.chars().take(300).collect::<String>())
        } else {
            None
        };
        record_bash_outcome(
            context.session_id,
            input.command.clone(),
            !success,
            error_snippet,
        );

        let result = if success {
            ToolResult::success(result_text)
        } else {
            ToolResult {
                success: false,
                output: result_text,
                error: Some(format!("Command exited with code {}", exit_code)),
                metadata: std::collections::HashMap::new(),
                images: Vec::new(),
            }
        };

        Ok(result
            .with_metadata("exit_code".to_string(), exit_code.to_string())
            .with_metadata("working_dir".to_string(), working_dir.display().to_string()))
    }
}

/// Whether a non-zero bash exit is a benign "search found no matches" rather
/// than a real failure. `grep`/`rg` exit 1 (with nothing on stderr) when no
/// line matches; that is a successful search with zero results, not an error
/// (exit 2+ / stderr output is a genuine error). Counting these as failures
/// inflated the bash failure rate with noise (#663).
///
/// On Windows the same contract holds for `findstr`: exit 1 with empty
/// stderr is "no lines matched", and counting it as a tool failure made
/// every no-match findstr a phantom bash failure in the ledger.
pub(crate) fn is_search_no_match(command: &str, exit_code: i32, stderr: &str) -> bool {
    if exit_code != 1 || !stderr.trim().is_empty() {
        return false;
    }
    // Check each pipe / chain segment's leading command token.
    command.split(['|', ';', '&']).any(|segment| {
        let seg = segment.trim();
        let first = seg.split_whitespace().next().unwrap_or("");
        matches!(first, "grep" | "egrep" | "fgrep" | "rg" | "findstr")
            || seg.starts_with("git grep")
    })
}

/// Hard blocklist check for dangerous commands.
///
/// Returns `Some(reason)` if the command matches a blocked pattern,
/// `None` if the command is allowed to proceed (still requires approval).
///
/// This is intentionally conservative — it blocks patterns that are
/// almost never legitimate in an AI agent context and would cause
/// catastrophic, irreversible damage if executed.
/// Normalize an `rm` target token for blocklist comparison: strip surrounding
/// quotes and map the (already-lowercased) `$HOME` / `${HOME}` env vars to `~`,
/// so `"$HOME"`, `$home`, and `~` all compare equal.
fn normalize_rm_target(tok: &str) -> String {
    let t = tok.trim_matches(|c| c == '"' || c == '\'');
    if let Some(rest) = t
        .strip_prefix("${home}")
        .or_else(|| t.strip_prefix("$home"))
    {
        format!("~{rest}")
    } else {
        t.to_string()
    }
}

/// The single command-safety floor for every execution tool (OC-06).
///
/// `bash` documents its hard blocklist as applying even under approval, but
/// `execute_code`, dynamic shell executors, and cron-embedded shell ran their
/// commands without it, so `rm -rf ~` (and fork bombs, `/etc/shadow` reads, …)
/// through those tools was a complete bypass of the "immovable floor". This
/// runs the hardcoded blocklist and the runtime TOML blocklist, the same pair
/// `bash` itself uses, and is what those other tools now call. Returns the
/// reason a command is refused, or None when it is allowed.
pub(crate) fn assert_command_allowed(command: &str) -> Option<String> {
    if let Some(reason) = check_blocked_command(command) {
        return Some(reason.to_string());
    }
    check_toml_blocklist(command)
}

pub(crate) fn check_blocked_command(command: &str) -> Option<&'static str> {
    check_blocked_inner(command, 0)
}

/// Strip ONE matching pair of surrounding quotes from a string. Used to peel
/// the code argument out of `bash -c '<code>'` / `echo '<code>' | sh` before
/// re-checking it. Leaves inner quotes alone.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['\'', '"'] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Decode a base64 token (standard alphabet) into a UTF-8 string, or `None`.
/// Lets the blocklist see through `echo <b64> | base64 -d | bash`.
fn try_base64_decode(tok: &str) -> Option<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(tok.trim())
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Pull out any command strings that `command` would hand to a shell
/// interpreter, so the blocklist can re-check them. Covers the common ways a
/// catastrophic command gets smuggled past a literal scan:
///
/// - `bash -c '<code>'`, `sh -lc "<code>"`, `zsh -c …`  (the `-c` payload)
/// - `eval '<code>'`                                     (the eval payload)
/// - `echo '<code>' | bash`, `printf … | sh`            (pipe to a shell)
/// - `echo <b64> | base64 -d | bash`                    (base64 then shell)
///
/// Returns the decoded inner command(s); the caller recurses on each.
fn extract_interpreter_payloads(command: &str) -> Vec<String> {
    const SHELLS: [&str; 6] = ["bash", "sh", "zsh", "dash", "ash", "ksh"];
    let basename = |t: &str| -> String {
        t.trim_start_matches("./")
            .rsplit('/')
            .next()
            .unwrap_or(t)
            .to_lowercase()
    };
    let mut out: Vec<String> = Vec::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();

    // `<shell> … -c <code>` — the `-c` value (and everything after it) is code.
    for i in 0..tokens.len() {
        if !SHELLS.contains(&basename(tokens[i]).as_str()) {
            continue;
        }
        for j in (i + 1)..tokens.len() {
            let tk = tokens[j];
            // `-c`, or a short cluster ending in c (`-lc`, `-ic`); NOT `--norc`.
            let is_c =
                tk == "-c" || (tk.starts_with('-') && !tk.starts_with("--") && tk.ends_with('c'));
            if is_c {
                out.push(unquote(&tokens[(j + 1)..].join(" ")).to_string());
                break;
            }
            if !tk.starts_with('-') {
                break; // a positional (script name) before -c => not the -c form
            }
        }
    }

    // `eval <code>` — everything after `eval` is code.
    if let Some(idx) = tokens.iter().position(|&t| basename(t) == "eval") {
        let payload = tokens[(idx + 1)..].join(" ");
        if !payload.is_empty() {
            out.push(unquote(&payload).to_string());
        }
    }

    // `<producer> | … | <shell>` — text piped into a shell is executed. Pull the
    // literal that `echo`/`printf` emits (base64-decoded if the pipe decodes it).
    if command.contains('|') {
        let stages: Vec<&str> = command.split('|').collect();
        let feeds_shell = stages.iter().any(|s| {
            s.split_whitespace()
                .next()
                .is_some_and(|w| SHELLS.contains(&basename(w).as_str()))
        });
        if feeds_shell {
            let has_b64 = stages.iter().any(|s| {
                let l = s.to_lowercase();
                l.contains("base64") && (l.contains(" -d") || l.contains("--decode"))
            });
            for s in &stages {
                let st = s.trim();
                let first = st.split_whitespace().next().unwrap_or("");
                if matches!(first, "echo" | "printf") {
                    let arg = unquote(st.split_once(char::is_whitespace).map_or("", |(_, a)| a));
                    if has_b64 {
                        if let Some(dec) = try_base64_decode(arg) {
                            out.push(dec);
                        }
                    } else {
                        out.push(arg.to_string());
                    }
                }
            }
        }
    }

    out
}

/// Depth-bounded core of [`check_blocked_command`]. Runs the literal scan on
/// `command`, then recurses into any code it would feed to a shell interpreter
/// so the same gate applies to `bash -c '<blocked>'`, `echo '<blocked>' | sh`,
/// `eval …`, and base64-wrapped variants. Depth is capped so a maliciously
/// self-nesting command can't spin the checker.
fn check_blocked_inner(command: &str, depth: usize) -> Option<&'static str> {
    if depth > 4 {
        return Some("excessively nested interpreter invocation");
    }

    // Normalize: collapse whitespace, lowercase for pattern matching
    let normalized: String = command
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .to_lowercase();

    // ── Recursive filesystem destruction ──────────────────────────
    // `rm` (optionally `sudo`) with a recursive flag targeting root/home/cwd.
    // Split into command segments first (so `echo x; rm -rf ~`, `... && rm
    // -rf ~`, and `rm -rf ~;echo` are all caught), then per-segment this is
    // robust to flag ORDER (-rf, -fr, -r -f), LONG flags (--recursive --force),
    // QUOTED targets ("$HOME"), and the $HOME/${HOME} env vars.
    let segmented = normalized
        .replace("&&", "\n")
        .replace("||", "\n")
        .replace(['|', ';', '&'], "\n");
    for segment in segmented.split('\n') {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        let sudo = tokens.first() == Some(&"sudo");
        let rm_idx = if sudo { 1 } else { 0 };
        if tokens.get(rm_idx) != Some(&"rm") {
            continue;
        }
        let mut recursive = false;
        let mut targets: Vec<String> = Vec::new();
        for &tok in tokens.iter().skip(rm_idx + 1) {
            if tok == "--recursive" {
                recursive = true;
            } else if tok.starts_with("--") {
                // other long flag (--force, --verbose, …) — ignore
            } else if tok.starts_with('-') {
                // short flag cluster: -rf, -fr, -r, -rfv … — recursive is what
                // makes a delete catastrophic.
                if tok.contains('r') {
                    recursive = true;
                }
            } else {
                targets.push(normalize_rm_target(tok));
            }
        }
        if recursive {
            for t in &targets {
                if matches!(t.as_str(), "/" | "/*" | "~" | "~/" | "~/*") {
                    return Some("recursive delete on root or home directory");
                }
                if sudo && matches!(t.as_str(), "." | "./" | "./*" | ".." | "../" | "../*") {
                    return Some("sudo recursive delete on current or parent directory");
                }
            }
        }
    }

    // ── Disk/partition destruction ────────────────────────────────
    if normalized.contains("mkfs")
        || normalized.contains("dd if=") && normalized.contains("of=/dev")
    {
        return Some("disk formatting or raw device write");
    }

    // ── Fork bombs ───────────────────────────────────────────────
    if normalized.contains(":(){ :|:& };:") || normalized.contains("./$0|./$0&") {
        return Some("fork bomb");
    }

    // ── /dev/sda or /dev/nvme direct writes ──────────────────────
    if (normalized.contains("> /dev/sd") || normalized.contains("> /dev/nvme"))
        && !normalized.contains("/dev/stderr")
        && !normalized.contains("/dev/stdout")
    {
        return Some("direct write to block device");
    }

    // ── chmod 777 on system dirs ─────────────────────────────────
    if normalized.contains("chmod")
        && normalized.contains("777")
        && normalized.contains("-r")
        && (normalized.contains(" /") && !normalized.contains(" /tmp"))
    {
        return Some("recursive chmod 777 on system directory");
    }

    // ── Overwrite system files ───────────────────────────────────
    if normalized.contains("> /etc/passwd")
        || normalized.contains("> /etc/shadow")
        || normalized.contains("> /etc/sudoers")
    {
        return Some("overwrite critical system file");
    }

    // ── Kernel/system destruction ────────────────────────────────
    if normalized.contains("echo") && normalized.contains("> /proc/") {
        return Some("write to /proc filesystem");
    }
    if normalized.contains("> /dev/null < /dev/sda")
        || normalized.contains("cat /dev/urandom > /dev/sd")
    {
        return Some("device destruction via /dev");
    }

    // ── Network exfiltration of sensitive files ──────────────────
    if (normalized.contains("curl") || normalized.contains("wget") || normalized.contains("nc "))
        && (normalized.contains("/etc/shadow")
            || normalized.contains("/etc/passwd")
            || normalized.contains("id_rsa")
            || normalized.contains(".ssh/"))
    {
        return Some("network exfiltration of sensitive files");
    }

    // ── Crypto mining / known malware patterns ───────────────────
    if normalized.contains("xmrig")
        || normalized.contains("minerd")
        || normalized.contains("cryptonight")
        || normalized.contains("stratum+tcp")
    {
        return Some("cryptocurrency mining");
    }

    // ── iptables flush (locks out remote access) ─────────────────
    if normalized.contains("iptables -f") && normalized.contains("drop") {
        return Some("firewall flush with default DROP (can lock out remote access)");
    }

    // ── Interpreter indirection ──────────────────────────────────
    // A literal scan can't see a blocked command hidden inside a string handed
    // to a shell (`bash -c '…'`, `echo '…' | sh`, `eval …`, base64-then-shell).
    // Pull those payloads back out and re-run the SAME gate on each.
    for payload in extract_interpreter_payloads(command) {
        if let Some(reason) = check_blocked_inner(&payload, depth + 1) {
            return Some(reason);
        }
    }

    None
}

/// Maximum number of recent bash commands tracked per session for the
/// retry-loop guard. Five is enough to catch tight back-to-back loops
/// (the actual symptom — same command twice in a row), small enough
/// that a legitimate retry after enough other work won't false-fire.
pub(crate) const RECENT_BASH_WINDOW: usize = 5;

/// Maximum duration a failed outcome blocks identical retries (#195).
/// Tight loops happen seconds apart. Hours later, the environment may
/// have recovered, so old failures age out.
pub(crate) const RECENT_BASH_TTL: Duration = Duration::from_secs(600);

/// Maximum consecutive times a failed command can be short-circuited
/// before yielding one real execution (#195).
pub(crate) const RECENT_BASH_MAX_BLOCKS: u32 = 3;

#[derive(Debug, Clone)]
pub(crate) struct RecentBashOutcome {
    pub command: String,
    pub failed: bool,
    pub error_snippet: Option<String>,
    /// When this outcome was recorded (#195). A block does NOT refresh this —
    /// refreshing it on every block would re-create the latch the TTL exists to break.
    pub recorded_at: Instant,
    /// Number of times this failure entry has short-circuited a retry (#195).
    pub blocks: u32,
}

/// Per-session ring buffer of the most recent bash commands and their
/// outcomes. In-memory only; cleared on process restart. Keyed by
/// `session_id` so different conversations never see each other's
/// retry history.
pub(crate) fn recent_bash_state() -> &'static RwLock<HashMap<Uuid, VecDeque<RecentBashOutcome>>> {
    static STATE: OnceLock<RwLock<HashMap<Uuid, VecDeque<RecentBashOutcome>>>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Layer 3 of the retry-loop guard: if the agent ran this exact
/// command in the last `RECENT_BASH_WINDOW` calls and it failed,
/// short-circuit with a stronger "you already tried this" message
/// that quotes the prior error. Returns `None` when there's no
/// matching prior failure (the command should be allowed to run).
pub(crate) fn check_recent_failure(session_id: Uuid, cmd: &str) -> Option<String> {
    let normalized = cmd.trim();
    let state = recent_bash_state().read().ok()?;
    let buf = state.get(&session_id)?;
    for prev in buf.iter() {
        if prev.failed
            && prev.command.trim() == normalized
            && prev.recorded_at.elapsed() < RECENT_BASH_TTL
            && prev.blocks < RECENT_BASH_MAX_BLOCKS
        {
            let snippet = prev
                .error_snippet
                .as_deref()
                .unwrap_or("(no detail captured)");
            return Some(format!(
                "You already ran this exact command in the last {} bash calls and it failed. \
                 {}\n\nPrevious error:\n{}",
                RECENT_BASH_WINDOW,
                crate::brain::agent::service::nudge::variation_directive(),
                snippet
            ));
        }
    }
    None
}

/// Record the outcome of a bash call so a follow-up matching call can
/// be intercepted by `check_recent_failure`. Move-to-front semantics:
/// repeating the same command bumps the existing entry rather than
/// adding a duplicate, so the buffer always reflects the latest
/// outcome per unique command string.
pub(crate) fn record_bash_outcome(
    session_id: Uuid,
    command: String,
    failed: bool,
    error_snippet: Option<String>,
) {
    record_bash_outcome_at(session_id, command, failed, error_snippet, Instant::now());
}

/// Variant of `record_bash_outcome` that accepts an explicit timestamp (#195).
/// Used in production with `Instant::now()`, and in unit tests to inject
/// stale entries without sleeping.
pub(crate) fn record_bash_outcome_at(
    session_id: Uuid,
    command: String,
    failed: bool,
    error_snippet: Option<String>,
    recorded_at: Instant,
) {
    let Ok(mut state) = recent_bash_state().write() else {
        return;
    };
    let buf = state.entry(session_id).or_default();
    let normalized = command.trim().to_string();
    buf.retain(|o| o.command.trim() != normalized);
    buf.push_back(RecentBashOutcome {
        command,
        failed,
        error_snippet,
        recorded_at,
        blocks: 0,
    });
    while buf.len() > RECENT_BASH_WINDOW {
        buf.pop_front();
    }
}

/// Increment the block counter for a failed command after it is short-circuited (#195).
/// Deliberately does NOT update `recorded_at` — doing so would refresh the TTL and
/// resurrect the infinite latch defect.
pub(crate) fn record_bash_block(session_id: Uuid, cmd: &str) {
    let Ok(mut state) = recent_bash_state().write() else {
        return;
    };
    let Some(buf) = state.get_mut(&session_id) else {
        return;
    };
    let normalized = cmd.trim();
    for o in buf.iter_mut() {
        if o.command.trim() == normalized {
            o.blocks = o.blocks.saturating_add(1);
        }
    }
}

/// Forget the recorded bash outcomes for a session (#2211).
///
/// The guard's premise is "same command, unchanged world". Two things break
/// that premise: a new user turn, and a successful non-bash tool call. When
/// `write_file` creates the file a previous `cat` could not find, the recorded
/// error no longer describes what the command would do, and the user's next
/// explicit request for it has to run. The loop the guard exists to stop is
/// back-to-back retries inside one turn, and that stays blocked because bash
/// calls never reach this function.
pub(crate) fn note_environment_changed(session_id: Uuid) {
    let Ok(mut state) = recent_bash_state().write() else {
        return;
    };
    state.remove(&session_id);
}

#[cfg(test)]
pub(crate) fn recorded_at_of(session_id: Uuid, cmd: &str) -> Option<Instant> {
    let state = recent_bash_state().read().ok()?;
    let buf = state.get(&session_id)?;
    let normalized = cmd.trim();
    buf.iter()
        .find(|o| o.command.trim() == normalized)
        .map(|o| o.recorded_at)
}

/// Detect commands that require an interactive TTY and return a useful
/// non-interactive alternative. Returns `None` when the command is fine
/// to run as-is, `Some(message)` when it should be rejected up-front.
///
/// The Apr 23 stdin-detach fix made the bash tool well-behaved (no more
/// keystroke theft from the TUI), but it also made interactive commands
/// Split a shell command string into executable segments (`a && b`, `a; b`, `a | b`,
/// multi-line commands), respecting POSIX single quotes (`'...'`), double quotes (`"..."`),
/// and backslash escapes (`\`).
///
/// Within quotes or behind backslashes, `;`, `|`, `&`, and newlines do not delimit segments.
/// This prevents false-positive interactive rejections when patterns or messages contain
/// pipeline or alternation characters, e.g.:
/// `git log --grep="feat: add | fix: remove"` or `echo "git push|git commit"`.
pub(crate) fn split_command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            cur.push(ch);
            continue;
        }

        if in_double {
            if ch == '\\' {
                escaped = true;
                cur.push(ch);
            } else if ch == '"' {
                in_double = false;
                cur.push(ch);
            } else {
                cur.push(ch);
            }
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
                cur.push(ch);
            }
            '\'' => {
                in_single = true;
                cur.push(ch);
            }
            '"' => {
                in_double = true;
                cur.push(ch);
            }
            ';' | '|' | '&' | '\n' => {
                let trimmed = cur.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
                cur.clear();
            }
            _ => {
                cur.push(ch);
            }
        }
    }

    let trimmed = cur.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }

    segments
}

/// Strip any single- or double-quotes from `s`.
/// Used when checking command prefix matches so that e.g. `"git" add -p` or
/// `'git'` commit is still matched.
fn strip_quotes(s: &str) -> String {
    s.chars().filter(|&c| c != '\'' && c != '"').collect()
}

/// A `sudo` invocation found where the shell would actually run it (#2157).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SudoInvocation {
    /// Byte offset just past the `sudo` word, which is where `-S -p ""` belongs.
    pub insert_at: usize,
    /// The invocation's own flags already say "never prompt" (`-n` in a short
    /// cluster, or `--non-interactive`), so no dialog can help it.
    pub never_prompts: bool,
}

/// Wrappers a shell allows in front of the real command. A `sudo` behind any of
/// these still runs as a command, so the dialog has to see it. `timeout` is
/// handled separately because it also takes a duration operand.
const SUDO_PREFIX_WRAPPERS: [&str; 5] = ["command", "exec", "env", "nohup", "time"];

/// The flags `sudo` was invoked with, read from the tokens that follow it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SudoFlags {
    /// Every character of every short cluster, in order: `-Hn` yields `Hn`.
    clusters: String,
    /// The long `--non-interactive` spelling was passed.
    non_interactive: bool,
}

impl SudoFlags {
    fn never_prompts(&self) -> bool {
        self.non_interactive || self.clusters.contains('n')
    }
}

/// Read the flags that follow the `sudo` word at byte offset `after`.
///
/// Stops at the first operand, which is where sudo's own flags end and the
/// wrapped command begins. A segment delimiter (`;`, `|`, `&`) ends them too:
/// `sudo -n; echo hi` gives sudo exactly one flag.
fn sudo_flags(command: &str, after: usize) -> SudoFlags {
    let mut flags = SudoFlags::default();

    for raw in command[after..].split_whitespace() {
        let tok = raw.trim_end_matches([';', '|', '&', ')', '}']);
        let delimited = tok.len() != raw.len();
        if tok == "--" {
            break;
        }
        if !tok.starts_with('-') || tok == "-" {
            break;
        }
        if let Some(long) = tok.strip_prefix("--") {
            if long == "non-interactive" {
                flags.non_interactive = true;
            }
        } else {
            flags.clusters.push_str(&tok[1..]);
        }
        if delimited {
            break;
        }
    }
    flags
}

/// True when `word` is a `VAR=value` assignment rather than the command word.
fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every `sudo` the shell would run in `command`, left to right (#2157).
///
/// Positional, not a substring test. A `sudo` at the start of a segment counts,
/// including after `&&`, `;`, `|`, a newline, or a wrapper (`nohup sudo ...`,
/// `timeout 30 sudo ...`, `FOO=1 sudo ...`). Three shapes that used to be
/// misread do not count: `sudoku --help` (the word is longer), `echo "sudo rm"`
/// (quoted), and a heredoc body that merely contains the word.
///
/// The old test was `trim_start().starts_with("sudo ")`, which missed a tab
/// after the word and every chained invocation, so `cd /tmp && sudo foo` ran
/// with no password path at all and hung on a prompt it could not answer.
pub(crate) fn find_sudo_invocations(command: &str) -> Vec<SudoInvocation> {
    let bodies = crate::utils::long_command::heredoc_body_ranges(command);
    let in_body = |pos: usize| bodies.iter().any(|range| range.contains(&pos));

    let mut found = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut at_command_start = true;
    // `timeout 30 sudo x`: the duration is an operand, not the command.
    let mut skip_operand = false;

    let mut chars = command.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if in_body(idx) {
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '\'' => in_single = true,
            '"' => in_double = true,
            ';' | '|' | '&' | '\n' | '(' | '{' | '`' => at_command_start = true,
            c if c.is_whitespace() => {}
            _ => {
                if !at_command_start {
                    continue;
                }
                // Read the whole word, so `sudoku` never reads as `sudo`.
                let start = idx;
                let mut end = idx + ch.len_utf8();
                while let Some(&(next_idx, next_ch)) = chars.peek() {
                    if next_ch.is_whitespace()
                        || matches!(next_ch, ';' | '|' | '&' | '(' | ')' | '{' | '}' | '`')
                    {
                        break;
                    }
                    chars.next();
                    end = next_idx + next_ch.len_utf8();
                }
                let word = &command[start..end];

                if skip_operand {
                    // The duration `timeout` takes. Flags before it keep us here.
                    if !word.starts_with('-') {
                        skip_operand = false;
                        at_command_start = true;
                    }
                    continue;
                }
                if word == "timeout" {
                    skip_operand = true;
                    at_command_start = true;
                    continue;
                }
                if SUDO_PREFIX_WRAPPERS.contains(&word) || is_env_assignment(word) {
                    // The command we care about is still ahead of this token.
                    at_command_start = true;
                    continue;
                }

                at_command_start = false;
                if word == "sudo" {
                    found.push(SudoInvocation {
                        insert_at: end,
                        never_prompts: sudo_flags(command, end).never_prompts(),
                    });
                }
            }
        }
    }

    found
}

/// Add the flags that let sudo read the password from stdin and keep its own
/// prompt off the screen (#2157).
///
/// Inserted right after each `sudo` word, so a heredoc body or a quoted string
/// that merely contains `sudo ` is never rewritten. The old code called
/// `replacen("sudo ", ...)`, which hit the first occurrence anywhere in the
/// string and could rewrite a body line instead of the command.
pub(crate) fn inject_sudo_password_flags(command: &str, invocations: &[SudoInvocation]) -> String {
    let mut out = command.to_string();

    // Right to left, so each earlier offset still points at the same word.
    for invocation in invocations.iter().rev() {
        let flags = sudo_flags(command, invocation.insert_at);
        let mut add = String::new();
        if !flags.clusters.contains('S') {
            add.push_str(" -S");
        }
        if !flags.clusters.contains('p') {
            add.push_str(" -p \"\"");
        }
        if !add.is_empty() {
            out.insert_str(invocation.insert_at, &add);
        }
    }
    out
}

/// Which sudo model a hint describes (#2157).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SudoPlatform {
    /// Linux and macOS: one sudoers drop-in file per rule.
    Unix,
    /// Windows: no sudoers file at all, elevation or the Windows sudo setting.
    Windows,
}

impl SudoPlatform {
    /// The platform this process runs on.
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// The account name a sudoers rule has to cover: `USER`, else `USERNAME`, else
/// the `%sudo` group.
///
/// Read from the environment rather than forked from `whoami`: this is a hint
/// string, and a process spawn to spell a name the shell already exports is a
/// cost with no answer of its own.
pub(crate) fn sudo_account_name() -> String {
    ["USER", "USERNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "%sudo".to_string())
}

/// How to give this account passwordless sudo, so the dialog stops appearing
/// (#2157).
///
/// One copy of the fix, shown wherever a user can hit the wall: inside the
/// dialog, in the timeout, and in the error a dialog-less surface returns.
/// `platform` is a parameter, not a `cfg!`, so both texts are testable on any
/// host.
pub(crate) fn sudo_access_hint(platform: SudoPlatform, user: &str) -> String {
    match platform {
        SudoPlatform::Unix => format!(
            "To stop this prompt for good, allow {user} passwordless sudo once:\n\
             echo \"{user} ALL=(ALL) NOPASSWD: ALL\" | sudo tee /etc/sudoers.d/opencrabs\n\
             sudo chmod 0440 /etc/sudoers.d/opencrabs"
        ),
        SudoPlatform::Windows => {
            "Windows has no sudoers file. Run OpenCrabs from an elevated terminal \
             (right-click the terminal, Run as administrator), or enable sudo under \
             Settings > System > For developers, then retry."
                .to_string()
        }
    }
}

/// The hint for this host, resolved once.
///
/// The dialog re-renders on every tick while it waits, and building the string
/// per frame would allocate for no new answer.
pub(crate) fn sudo_access_hint_cached() -> &'static str {
    static HINT: OnceLock<String> = OnceLock::new();
    HINT.get_or_init(|| sudo_access_hint(SudoPlatform::current(), &sudo_account_name()))
}

/// True when sudo refused to run because it had no way to ask for a password.
///
/// These are the lines sudo prints when nothing can answer its prompt: no
/// terminal, no askpass helper, no cached credential. Anything else on stderr
/// (a missing file, a bad command) is a different failure and gets no hint.
pub(crate) fn looks_like_sudo_password_failure(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "a password is required",
        "a terminal is required",
        "no tty present",
        "no askpass program specified",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Per-session answer to "will sudo ask for anything?" (#2157).
///
/// Keyed by session like `recent_bash_state`: the answer depends on the host's
/// sudoers rules and the credential timestamp, neither of which changes per
/// turn. `sudo -k` mid-session is the one way to make it stale, and the failure
/// then is sudo's own "a password is required", which the agent can read.
pub(crate) fn sudo_passwordless_state() -> &'static RwLock<HashMap<Uuid, bool>> {
    static STATE: OnceLock<RwLock<HashMap<Uuid, bool>>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// True when sudo has nothing to ask for, so no dialog is needed.
///
/// Probed once per session with `sudo -n -v`, which validates without ever
/// prompting: exit 0 means the credential is cached or a passwordless rule
/// applies. Cached because the probe costs a process spawn and its answer
/// cannot change between turns.
async fn sudo_needs_no_password(session_id: Uuid) -> bool {
    if let Ok(state) = sudo_passwordless_state().read()
        && let Some(known) = state.get(&session_id)
    {
        return *known;
    }

    let answer = probe_sudo_noninteractive().await;
    if let Ok(mut state) = sudo_passwordless_state().write() {
        state.insert(session_id, answer);
    }
    answer
}

/// `sudo -n -v` once: validates the credential without prompting. Any failure
/// to run it (no sudo, a timeout, a non-zero exit) reads as "will prompt",
/// which is the safe direction: the worst case is a dialog over a prompt that
/// would have appeared anyway.
async fn probe_sudo_noninteractive() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        use crate::utils::shell::PushShellCommand;

        let (shell, shell_arg) = crate::utils::shell::shell_pair();
        let mut cmd = Command::new(shell);
        cmd.kill_on_drop(true);
        cmd.push_shell_command(shell_arg, "sudo -n -v")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        matches!(
            timeout(Duration::from_secs(5), cmd.status()).await,
            Ok(Ok(status)) if status.success()
        )
    }
}

/// The `sleep` invocation in `command`, if it makes one (#2123).
///
/// Matches the first word of each command segment, skipping the wrappers a
/// shell allows in front of it (`sudo`, `command`, `exec`, `nohup`, `env`,
/// `timeout <duration>`) plus leading `VAR=value` assignments. Deliberately
/// positional: a substring rule would refuse `grep -rn "sleep" logs/`, which
/// is a legitimate search for this very rule.
pub(crate) fn sleeping_segment(command: &str) -> Option<String> {
    const WRAPPERS: [&str; 5] = ["sudo", "command", "exec", "nohup", "env"];

    for seg in split_command_segments(command) {
        let toks: Vec<&str> = seg.split_whitespace().collect();
        let mut i = 0;
        while i < toks.len() {
            let raw = toks[i];
            // `FOO=bar sleep 1`: the assignment is not the command.
            if !raw.starts_with('-') && raw.contains('=') {
                i += 1;
                continue;
            }
            let word = strip_quotes(raw).to_lowercase();
            if WRAPPERS.contains(&word.as_str()) {
                i += 1;
                continue;
            }
            if word == "timeout" {
                i += 1;
                // The wrapper's own flags, e.g. `-s KILL` / `--signal=KILL`.
                while i < toks.len() && toks[i].starts_with('-') {
                    let flag = toks[i];
                    i += 1;
                    if flag == "-s" || flag == "--signal" {
                        i += 1;
                    }
                }
                i += 1; // the duration, then the wrapped command
                continue;
            }
            // `do sleep 1` / `then sleep 1` inside a loop or conditional body.
            if word == "do" || word == "then" {
                i += 1;
                continue;
            }
            if word == "sleep" {
                return Some(seg);
            }
            break;
        }
    }
    None
}

/// Why a `sleep` is refused right now, or `None` to let it run (#2123).
///
/// Scoped to sessions that actually have a detached task in flight: such a
/// task resumes the session on its own completion, so waiting on a counter
/// only burns time that belongs to other work. With nothing detached the
/// command is ordinary shell and stays available.
///
/// `running` carries the labels, not a count, because the refusal has to say
/// WHAT is being waited on. A bare number leaves the reader unable to tell a
/// two-second test from a ten-minute build.
pub(crate) fn sleep_refusal(command: &str, running: &[String]) -> Option<String> {
    let detail = match running {
        [] => return None,
        [only] => format!("`{only}` is still running"),
        many => {
            let names: Vec<&str> = many.iter().map(String::as_str).collect();
            format!(
                "{} detached tasks are still running ({})",
                many.len(),
                names.join(", ")
            )
        }
    };
    let seg = sleeping_segment(command)?;
    Some(format!(
        "Blocked: `{seg}` waits on a counter, but {detail} for this session. \
         A detached task reports back on its own when it finishes. Do other work, or end the turn and \
         the completion will start the next one."
    ))
}

/// fail *silently*: stdin=/dev/null returns EOF, the program exits with
/// code 0, output looks plausible, and the agent retries the same
/// command thinking it just needs another shot. Cut the loop on attempt
/// 1 with a clear message that names the alternative.
pub(crate) fn check_interactive_command(command: &str) -> Option<&'static str> {
    // Walk each segment of a chain (`a && b`, `a; b`, `a || b`) and
    // check the first token of each — protects against e.g. `cd foo && vim bar`.
    // We use quote-aware segmentation so `;`, `|`, `&` inside quotes or escaped
    // do not split segments (#205).
    let raw_segments = split_command_segments(command);

    for raw_seg in raw_segments {
        let normalized: String = raw_seg
            .split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ")
            .to_lowercase();

        let seg = normalized.as_str();

        // Skip leading env-var assignments (e.g. `EDITOR=vim git commit`)
        let cmd_start = seg
            .split_whitespace()
            .find(|tok| !tok.contains('='))
            .unwrap_or(seg);
        let cmd_seg = &seg[seg.find(cmd_start).unwrap_or(0)..];

        // Strip quotes for command token checks
        let unquoted_seg = strip_quotes(cmd_seg);

        // `git add -p` / `--patch` / `-i` / `--interactive`
        if unquoted_seg.starts_with("git add ")
            && (unquoted_seg.contains(" -p")
                || unquoted_seg.contains(" --patch")
                || unquoted_seg.contains(" -i")
                || unquoted_seg.contains(" --interactive"))
        {
            return Some(
                "`git add -p` / `git add -i` is interactive and won't work — stdin is /dev/null so it exits silently after printing the first hunk. \
                 Stage specific files with `git add <path>` (or `git add -A` for everything), or apply a precomputed patch with `git apply <patch>`. \
                 If you really need hunk-level granularity, use `git diff > /tmp/x.patch`, edit it, then `git apply --cached /tmp/x.patch`.",
            );
        }

        // `git rebase -i` / `--interactive`
        if unquoted_seg.starts_with("git rebase ")
            && (unquoted_seg.contains(" -i") || unquoted_seg.contains(" --interactive"))
        {
            return Some(
                "`git rebase -i` is interactive and won't work — stdin is /dev/null. \
                 For squashing use `git reset --soft <base>` then `git commit -m`; \
                 for non-interactive autosquash run `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <base>`.",
            );
        }

        // `git commit` with no message source — opens an editor.
        // Short-flag combos like `-am` / `-ma` count as a message
        // source; long form `--message=...` too.
        if unquoted_seg.starts_with("git commit")
            && !unquoted_seg.contains(" -m")
            && !unquoted_seg.contains(" -am")
            && !unquoted_seg.contains(" -ma")
            && !unquoted_seg.contains(" --message")
            && !unquoted_seg.contains(" -f ")
            && !unquoted_seg.contains(" --file")
            && !unquoted_seg.contains(" --no-edit")
            && !unquoted_seg.contains(" -c ")
            && !unquoted_seg.contains(" --reuse-message")
            && !unquoted_seg.contains(" -c=")
            && !unquoted_seg.contains(" --fixup")
            && !unquoted_seg.contains(" --squash")
        {
            return Some(
                "`git commit` without a message opens an editor (interactive) — stdin is /dev/null. \
                 Pass `-m \"...\"` directly, or use `--no-edit` when amending.",
            );
        }

        // Standalone editors / pagers / TUI viewers — reject when they're
        // the actual command (not in the middle of a longer pipeline).
        let first_word = unquoted_seg.split_whitespace().next().unwrap_or("");
        // Every arm below reads its arguments from the SAME quote-aware view
        // that produced `first_word`, so one segment is never interpreted two
        // different ways inside a single predicate (#555).
        let args: Vec<&str> = unquoted_seg.split_whitespace().skip(1).collect();

        // Editors have no useful non-interactive output mode. Measured with
        // this tool's stdin/stdout: `vim FILE` and `nano FILE` both fail with
        // escape-sequence noise, and bare `vim`/`ed` exit having done nothing.
        // Only the informational queries produce output, so that is the one
        // shape worth letting through. (Contrast the pagers just below, where
        // a file operand genuinely works.)
        if matches!(
            first_word,
            "vim" | "vi" | "nvim" | "nano" | "emacs" | "pico" | "ed" | "joe" | "micro"
        ) && !has_editor_info_flag(&args)
        {
            return Some(
                "Editors (vim/nvim/nano/emacs/...) need a TTY and cannot run here — \
                 use the `edit_file` or `write_file` tool to modify a file. Informational \
                 queries such as `vim --version` or `nano -h` are allowed.",
            );
        }

        // Pagers with no operand read stdin, which is /dev/null. Measured:
        // bare `less`, `less -R` and `more` all return rc=0 with zero bytes of
        // output — the silent no-op this guard exists to break. With an operand
        // they work (`less FILE` prints the file, `man man` prints 36948 bytes),
        // and `man -w TOPIC` / `less --version` are informational queries that
        // also produce output, so both shapes are allowed.
        if matches!(first_word, "less" | "more" | "most" | "man")
            && !has_operand(&args)
            && !has_info_flag(&args)
        {
            return Some(
                "Pagers with nothing to page (less/more/man) read /dev/null stdin and produce \
                 nothing. Give them a file or topic (`less FILE`, `man TOPIC`), or use `cat`, \
                 `head`, `tail` — e.g. `git log | cat`.",
            );
        }

        // TUI process viewers: measured, bare `top` and bare `htop` fail
        // immediately without a TTY, while `top -b -n 1` and `htop --version`
        // both work. Only the argument-less form is the trap.
        if matches!(first_word, "top" | "htop" | "btop" | "atop" | "iotop") && is_bare(&args) {
            return Some(
                "Interactive process viewers (top/htop/...) need a TTY. Use `top -b -n 1`, \
                 `ps aux` or `ps -ef` for a snapshot, or `ps aux --sort=-%cpu | head`.",
            );
        }
        // REPLs without a script argument, or invoked in explicit interactive
        // mode (#555). The predicate tests the property that matters —
        // interactivity — rather than a flag allowlist: with this tool's stdin
        // detached to /dev/null, a bare invocation and an explicit `-i` both
        // read EOF, exit 0 having done nothing, and stall the loop on a retry.
        // Informational and one-shot flags (`--version`, `--help`, `-p`,
        // `--check`, `-c`, `-e`, `-m`) are genuinely non-interactive: they
        // produce output and terminate, so they pass.
        if matches!(
            first_word,
            "python" | "python3" | "node" | "irb" | "ghci" | "scala"
        ) {
            // `args` is the same quote-aware view that produced `first_word`,
            // hoisted above, so this arm does not re-parse the segment.
            let bare = is_bare(&args);
            let interactive = args.iter().any(|t| *t == "-i" || *t == "--interactive");
            if bare || interactive {
                return Some(
                    "Bare REPLs (python/node/irb/ghci) read /dev/null stdin, exit 0 \
                     having done nothing, and stall the loop. Run a programme instead: \
                     `python3 -c \"...\"` / `node -e \"...\"`, or save to a file and run it.",
                );
            }
        }
        // Database / Redis CLIs without a query. The old tests were substring
        // matches on " -c " / " -e ", so they rejected the joined and long
        // forms (`psql -c"SELECT 1"`, `mysql --execute=...`) even though those
        // are exactly as non-interactive as the spaced ones. Test the property
        // instead: does the invocation supply a command or a script?
        if first_word == "psql"
            && !has_sql_source(&args, "-c", "--command")
            && !has_sql_source(&args, "-f", "--file")
        {
            return Some(
                "`psql` with no command or script opens a REPL — pass `-c \"SQL\"`, \
                 `--command=\"SQL\"`, `-f script.sql` or `--file=script.sql`.",
            );
        }
        if first_word == "mysql" && !has_sql_source(&args, "-e", "--execute") {
            return Some(
                "`mysql` with no statement opens a REPL — pass `-e \"SQL\"` or `--execute=\"SQL\"`.",
            );
        }
        // `redis-cli` opens a REPL whenever no subcommand is given, INCLUDING
        // when connection flags are present (`redis-cli -h HOST`). The old test
        // compared the word count to 1, so it let that case straight through —
        // the arm's own failure mode — while refusing nothing else.
        if first_word == "redis-cli" && !redis_cli_has_command(&args) {
            return Some(
                "`redis-cli` with no subcommand opens a REPL — pass the command directly, \
                 e.g. `redis-cli -h HOST GET key`.",
            );
        }

        // Interactive selectors / TUIs commonly invoked by mistake. Measured:
        // bare `tmux` and bare `screen` both fail immediately, while `tmux -V`,
        // `tmux ls` and `screen -ls` either work or fail with a real diagnostic
        // the agent can act on — so only the argument-less form is refused.
        if matches!(first_word, "fzf" | "gum" | "tmux" | "screen") && is_bare(&args) {
            return Some(
                "Interactive TUI tools (fzf/gum/tmux/screen) need a TTY and cannot be driven \
                 from this tool. Non-interactive forms (`tmux -V`, `screen -ls`) are allowed.",
            );
        }
    }

    None
}

/// True when the invocation carries no argument after the command word.
///
/// This is the shape that silently does nothing here: with stdin detached to
/// /dev/null the command reads EOF, exits 0 and produces no output, so the
/// agent sees "success" and retries the same command. Measured for bare
/// `less`, `more`, `node`, `python3`, `ed`, `screen` and `htop` (#555).
fn is_bare(args: &[&str]) -> bool {
    args.is_empty()
}

/// True when the invocation is an informational query (`--version`,
/// `--help`). This is the one shape that reliably produces output with no TTY
/// and no operand, so it is the escape hatch the pager and editor arms need.
/// Measured: `less --version` → 285 bytes, `less --help` → 13067,
/// `more --help` → 899, `man --help` → 3261.
///
/// The SHORT `-h` is deliberately NOT accepted here, even though it is help for
/// several of these binaries: `less -h` measured rc=0 with ZERO bytes of output.
/// Accepting it would let a silent no-op through — the exact defect this guard
/// exists to catch — and would be a regression, since the pre-#555 arm refused
/// it. Families whose short form IS safely informational opt in through
/// `has_editor_info_flag`.
fn has_info_flag(args: &[&str]) -> bool {
    args.iter().any(|t| matches!(*t, "--version" | "--help"))
}

/// Editors additionally accept `-h`: measured, `vim -h` → 2369 bytes,
/// `nano -h` → 4365, `ed -h` → 2022, `pico -h` → 4365, all printing usage.
/// The pager family cannot share this, because `less -h` prints nothing at all.
fn has_editor_info_flag(args: &[&str]) -> bool {
    has_info_flag(args) || args.contains(&"-h")
}

/// True when any argument is a non-flag operand — a file path, a manual topic.
/// Distinguishes an invocation that acts on something from one that waits on
/// stdin (which is /dev/null here).
fn has_operand(args: &[&str]) -> bool {
    args.iter().any(|t| !t.starts_with('-'))
}

/// True when `args` supply a command or script through any spelling the client
/// accepts for the flag pair `short`/`long`: `-c SQL`, `-cSQL`,
/// `--command SQL`, `--command=SQL`. The previous `contains(" -c ")` matched
/// only the first of those, so it refused genuinely non-interactive forms
/// (#555).
fn has_sql_source(args: &[&str], short: &str, long: &str) -> bool {
    let long_eq = format!("{}=", long);
    args.iter().any(|t| {
        *t == short
            || *t == long
            || t.starts_with(long_eq.as_str())
            || (t.starts_with(short) && t.len() > short.len())
    })
}

/// redis-cli flags whose value is the NEXT argument, so that value is not
/// mistaken for a subcommand.
fn redis_cli_flag_takes_value(tok: &str) -> bool {
    matches!(
        tok,
        "-h" | "-p"
            | "-a"
            | "-u"
            | "-s"
            | "-n"
            | "--host"
            | "--port"
            | "--pass"
            | "--user"
            | "--dbnum"
            | "--sock"
    )
}

/// True when `args` contain a redis subcommand rather than only connection
/// flags. `redis-cli` opens a REPL with no subcommand, including
/// `redis-cli -h HOST` — the case the old word-count test let straight through.
///
/// Implemented from the documented CLI contract: redis-cli is not installed on
/// this host, so unlike every other arm here this one is NOT measured (#555).
fn redis_cli_has_command(args: &[&str]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let tok = args[i];
        if !tok.starts_with('-') {
            // A bare word is the subcommand: `GET key`, `PING`.
            return true;
        }
        // `--host=HOST` assigns inline; `-h HOST` takes the next argument.
        let head = tok.split('=').next().unwrap_or(tok);
        if redis_cli_flag_takes_value(head) {
            i += if tok.contains('=') { 1 } else { 2 };
        } else if tok.starts_with("--") && !tok.contains('=') {
            // A long option with neither a known arity nor an inline value is
            // command-like and runs to completion — `--scan`, `--version`,
            // `--help`. Treating these as subcommands avoids the false
            // positives that would otherwise reintroduce #555 in this arm.
            return true;
        } else {
            i += 1;
        }
    }
    false
}

/// Wrap `s` in POSIX single quotes so it survives /bin/sh expansion verbatim.
/// `'` inside the input becomes `'\''` (close, escaped quote, reopen).
pub(crate) fn posix_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Tempfile-backed `SSH_ASKPASS` script.
///
/// SSH consults `$SSH_ASKPASS` (with `SSH_ASKPASS_REQUIRE=force`) when it
/// can't open a TTY. The script we point at must be executable and emit
/// the password on stdout. We materialise both files in the process tempdir,
/// chmod 0700, and clean up on `Drop` so the secret never lingers.
struct SshAskpass {
    _password_file: tempfile::NamedTempFile,
    script_file: tempfile::NamedTempFile,
}

impl SshAskpass {
    fn new(password: &str) -> std::io::Result<Self> {
        use std::io::Write;

        // 1. Password file (mode 0600, owner-only).
        let mut pw_file = tempfile::Builder::new()
            .prefix("opencrabs-ssh-pw-")
            .tempfile()?;
        pw_file.write_all(password.as_bytes())?;
        pw_file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(pw_file.path(), perms)?;
        }

        // 2. Askpass script (mode 0700, owner-only executable).
        // `cat` is universally available; the script just dumps the
        // password file. The path goes through POSIX single-quote escaping
        // so $TMPDIR containing quotes/$/backticks can't break out.
        let pw_path = pw_file.path().to_string_lossy().to_string();
        let script_body = format!("#!/bin/sh\nexec cat {}\n", posix_single_quote(&pw_path));
        let mut script_file = tempfile::Builder::new()
            .prefix("opencrabs-ssh-askpass-")
            .suffix(".sh")
            .tempfile()?;
        script_file.write_all(script_body.as_bytes())?;
        script_file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(script_file.path(), perms)?;
        }

        Ok(Self {
            _password_file: pw_file,
            script_file,
        })
    }

    fn script_path(&self) -> &std::path::Path {
        self.script_file.path()
    }
}
