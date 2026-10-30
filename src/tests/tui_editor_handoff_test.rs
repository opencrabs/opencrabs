//! Editor handoff (#1744): `!vi`/`!nano` bang commands must take over the real
//! terminal instead of being pipe-captured, and Ctrl+Z mid-edit must not hang
//! the TUI. Covers the pure decision helpers (allowlist, argv split, PATH
//! resolution, wait-status decode) and pins the bang-branch routing shape.

use std::path::{Path, PathBuf};

use crate::tui::editor::{
    EDITOR_ALLOWLIST, WaitOutcome, decode_wait_status, handoff_target, resolve_editor, split_argv,
};

#[test]
fn editor_allowlist_covers_the_editors_people_run() {
    assert_eq!(
        EDITOR_ALLOWLIST,
        &[
            "vi",
            "vim",
            "nvim",
            "nano",
            "emacs",
            "emacsclient",
            "helix",
            "hx",
            "micro"
        ]
    );
}

#[test]
fn handoff_target_matches_bare_editor_names() {
    assert_eq!(handoff_target("vi", None).as_deref(), Some("vi"));
    assert_eq!(handoff_target("vim", None).as_deref(), Some("vim"));
    assert_eq!(handoff_target("nano", None).as_deref(), Some("nano"));
    assert_eq!(handoff_target("emacs", None).as_deref(), Some("emacs"));
}

#[test]
fn handoff_target_matches_editors_with_arguments() {
    assert_eq!(handoff_target("vi notes.md", None).as_deref(), Some("vi"));
    assert_eq!(
        handoff_target("vim src/main.rs +10", None).as_deref(),
        Some("vim")
    );
    assert_eq!(handoff_target("nano TODO", None).as_deref(), Some("nano"));
    assert_eq!(
        handoff_target("emacs -nw file.txt", None).as_deref(),
        Some("emacs")
    );
}

#[test]
fn handoff_target_rejects_lookalikes_and_non_editors() {
    // Substring lookalike: exec-ing this would run the wrong thing, and it
    // proves we match argv[0] exactly, not `contains`.
    assert_eq!(handoff_target("myvi-helper", None), None);
    assert_eq!(handoff_target("vim-tiny notes.md", None), None);
    assert_eq!(handoff_target("echo vi", None), None);
    assert_eq!(handoff_target("ls", None), None);
    // A lookalike whose basename is not an editor still pipes.
    assert_eq!(handoff_target("./myvi-helper", None), None);
    // Empty/whitespace commands are not handoffs.
    assert_eq!(handoff_target("", None), None);
    assert_eq!(handoff_target("   ", None), None);
}

#[test]
fn split_argv_program_and_args() {
    assert_eq!(
        split_argv("vi notes.md"),
        Some(("vi".to_string(), vec!["notes.md".to_string()]))
    );
    assert_eq!(split_argv("nano"), Some(("nano".to_string(), vec![])));
    assert_eq!(
        split_argv("  vi  a  b  "),
        Some(("vi".to_string(), vec!["a".into(), "b".into()]))
    );
    assert_eq!(split_argv(""), None);
    assert_eq!(split_argv("   "), None);
}

/// Drop a fake executable into a temp dir and point `path_dirs` at it —
/// the lookup is hermetic, never depends on what's installed on the runner.
fn fake_path_with(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| {
            let p = dir.join(name);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").expect("write fake binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod fake binary");
            }
            p
        })
        .collect()
}

#[test]
fn resolve_editor_finds_executables_on_the_given_path() {
    let dir = tempfile::tempdir().unwrap();
    fake_path_with(dir.path(), &["vi", "nano"]);
    let path_dirs = dir.path().to_string_lossy().to_string();

    assert_eq!(
        resolve_editor("vi", Some(&path_dirs), dir.path()),
        Some(dir.path().join("vi"))
    );
    assert_eq!(
        resolve_editor("nano", Some(&path_dirs), dir.path()),
        Some(dir.path().join("nano"))
    );
    // Not installed on this PATH → None; the handoff aborts before touching
    // the terminal and says so in the chat pane.
    assert_eq!(resolve_editor("emacs", Some(&path_dirs), dir.path()), None);
    assert_eq!(
        resolve_editor("nonexistent-editor-xyz", Some(&path_dirs), dir.path()),
        None
    );
}

#[test]
fn resolve_editor_ignores_non_executable_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vi"), "not executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("vi"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let path_dirs = dir.path().to_string_lossy().to_string();
        assert_eq!(resolve_editor("vi", Some(&path_dirs), dir.path()), None);
    }
}

#[test]
fn resolve_editor_rejects_path_dir_that_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_editor("vi", Some("/nonexistent-dir-for-1744"), dir.path()),
        None
    );
}

#[test]
fn wait_status_decode_covers_all_three_posix_cases() {
    // Synthesised per the POSIX wait-status encoding documented in
    // waitpid(2) — identical layout on Linux and macOS:
    //   exited(code) → (code << 8) | 0
    //   stopped(sig) → (sig << 8) | 0x7f
    //   signaled(sig) → sig (low 7 bits non-zero, not 0x7f)
    assert_eq!(decode_wait_status(0x0000), WaitOutcome::Exited(0));
    assert_eq!(decode_wait_status(0x0100), WaitOutcome::Exited(1));
    assert_eq!(decode_wait_status(0x147F), WaitOutcome::Stopped(20)); // SIGTSTP (Ctrl+Z)
    assert_eq!(decode_wait_status(0x0009), WaitOutcome::Signaled(9)); // SIGKILL
    assert_eq!(decode_wait_status(0x000F), WaitOutcome::Signaled(15)); // SIGTERM
}

#[test]
fn non_editor_bangs_stay_on_the_pipe_path() {
    // The routing predicate: only allowlisted argv[0] values park a handoff.
    // Everything else must fall through to the existing piped bang branch —
    // nothing is parked, nothing is aborted.
    assert!(handoff_target("ls -la", None).is_none());
    assert!(handoff_target("git status", None).is_none());
    assert!(handoff_target("echo vi > /dev/null", None).is_none());
}

#[test]
fn bang_branch_routes_allowlist_into_the_parking_field() {
    // #1744 routing shape: the bang branch checks the allowlist and parks
    // `(cmd, origin_session)` for the runner loop; the runner consumes it,
    // performs the handoff, and re-enables mouse capture afterwards.
    let src = include_str!("../tui/app/input.rs");
    assert!(
        src.contains("crate::tui::editor::handoff_target(&shell_cmd, env_editor.as_deref())")
            && src.contains("self.pending_editor_handoff = Some((shell_cmd, origin_session));"),
        "bang branch must park handoff_target matches as (cmd, origin_session) for the runner loop"
    );
    let runner = include_str!("../tui/runner.rs");
    assert!(
        runner.contains("app.pending_editor_handoff.take()")
            && runner.contains("mouse_capture_applied = true;"),
        "runner loop must consume the parked handoff and resync mouse capture after it"
    );
}

#[tokio::test]
async fn a_stopped_child_is_coerced_to_exit_instead_of_hanging_the_reap() {
    // Acceptance criterion 3 of #1744 at the real-process seam: a child that
    // ends up group-stopped (exactly the state Ctrl+Z puts vim in) must not
    // wedge reap_editor. It gets SIGCONT, a polite SIGTERM with a bounded
    // grace, and escalation to SIGKILL — the child here (a shell waiting on
    // a foreground job) defers SIGTERM outright, which is exactly why the
    // grace must be bounded and end in KILL.
    // SIGSTOP stands in for SIGTSTP here because the wait-status class is
    // identical and SIGSTOP is unmaskable; a non-interactive /bin/sh ignores
    // SIGTSTP outright (verified 2026-09-26: `kill -TSTP $$` under bash 3.2
    // on macOS leaves the process sleeping, never stopped).
    use std::process::Stdio;

    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("kill -STOP $$; sleep 30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as libc::pid_t;
    std::mem::forget(child);

    let done = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::tui::editor::reap_editor(pid),
    )
    .await;
    assert!(
        done.is_ok(),
        "reap_editor hung on a stopped child (#1744 regression)"
    );
}

#[test]
fn editor_module_is_unix_gated_at_the_mod_declaration() {
    // #1755: libc waitpid/kill don't exist on the windows libc crate, so the
    // whole editor module compiles out on Windows and bang commands keep the
    // pipe-capture path. The Windows runner only builds (never runs tests),
    // so the gate is pinned by source scan.
    let decl = include_str!("../tui/mod.rs");
    assert!(
        decl.contains("#[cfg(unix)]\npub mod editor;"),
        "the editor module must stay cfg(unix): its reap path is libc-based (#1755)"
    );
}

/// #2173: the editors people actually run must hand off, not dump escape
/// sequences into the chat. `nvim`, `emacsclient`, `helix`, `hx` and `micro`
/// were all pipe-captured before this.
#[test]
fn handoff_target_covers_modern_editors() {
    assert_eq!(handoff_target("nvim", None).as_deref(), Some("nvim"));
    assert_eq!(
        handoff_target("emacsclient -nw x.rs", None).as_deref(),
        Some("emacsclient")
    );
    assert_eq!(
        handoff_target("helix notes.md", None).as_deref(),
        Some("helix")
    );
    assert_eq!(handoff_target("hx", None).as_deref(), Some("hx"));
    assert_eq!(
        handoff_target("micro config.toml", None).as_deref(),
        Some("micro")
    );
}

/// #2173: an absolute or relative path to an allowlisted editor hands off.
/// The match is on the basename, so `/usr/bin/nvim` and `./vim` work while a
/// lookalike basename still pipes.
#[test]
fn handoff_target_matches_on_the_basename() {
    assert_eq!(
        handoff_target("/usr/bin/nvim file.rs", None).as_deref(),
        Some("nvim")
    );
    assert_eq!(handoff_target("./vim", None).as_deref(), Some("vim"));
    assert_eq!(handoff_target("vimrc-helper", None), None);
    assert_eq!(handoff_target("./myvi-helper", None), None);
}

/// #2173: $EDITOR/$VISUAL is honoured by name comparison, so an editor we do
/// not list still gets the tty when it is the user's own choice. A command
/// that is NOT the env editor keeps piping.
#[test]
fn handoff_target_honours_env_editor() {
    assert_eq!(handoff_target("kak", Some("kak")).as_deref(), Some("kak"));
    assert_eq!(
        handoff_target("/opt/homebrew/bin/kak file", Some("kak -n")).as_deref(),
        Some("kak")
    );
    assert_eq!(handoff_target("myvi-helper", Some("micro")), None);
    assert_eq!(handoff_target("ls", Some("kak")), None);
}
