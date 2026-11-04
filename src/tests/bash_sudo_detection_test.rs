//! sudo detection and the password dialog (#2157).
//!
//! The dialog used to open for every command whose text began with `sudo `,
//! even when sudo had nothing to ask for. On a surface that waits for an
//! answer, that is a dead turn: the prompt never appears, the callback never
//! returns, and the command is still unsent two minutes later. Reported from
//! the TUI with a same-process comparison: TUI dead at 17:07:38, Telegram in
//! the same PID fine one second later.
//!
//! Two rules come out of it, and both are asserted here:
//!   1. The dialog opens only when sudo will really prompt. `-n` says it will
//!      not, and so does a host where `sudo -n -v` exits 0.
//!   2. Detection is positional, not a prefix test, so a chained invocation is
//!      found while a heredoc body or a quoted string is not.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::bash::{
    BashTool, SudoPlatform, find_sudo_invocations, inject_sudo_password_flags,
    looks_like_sudo_password_failure, sudo_access_hint, sudo_access_hint_cached, sudo_account_name,
    sudo_passwordless_state,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

/// Seed the per-session probe cache, so a test never depends on whether the
/// machine running it happens to have a cached sudo credential.
fn seed_probe(session: Uuid, passwordless: bool) {
    sudo_passwordless_state()
        .write()
        .expect("sudo probe cache is poisoned")
        .insert(session, passwordless);
}

/// A context whose sudo callback counts its invocations. Returning `Ok(None)`
/// would cancel the command, so a count of zero is the only reading that says
/// the dialog never opened.
fn counting_context(session: Uuid) -> (ToolExecutionContext, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut context = ToolExecutionContext::new(session).with_auto_approve(true);
    context.sudo_callback = Some(Arc::new(move |_command: String| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }));
    (context, calls)
}

async fn run(command: &str, context: &ToolExecutionContext) {
    let input = serde_json::json!({ "command": command });
    // The command itself may fail (no password, no tty). The assertion is about
    // the dialog, not the exit status.
    let _ = BashTool.execute(input, context).await;
}

#[tokio::test]
async fn sudo_passwordless_probe_skips_the_dialog_for_plain_sudo() {
    let session = Uuid::new_v4();
    seed_probe(session, true);
    let (context, calls) = counting_context(session);

    run("sudo true", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "sudo -n -v exited 0, so there is nothing to ask for and the dialog must stay shut"
    );
}

#[tokio::test]
async fn sudo_passwordless_short_flag_n_never_opens_the_dialog() {
    let session = Uuid::new_v4();
    // Explicitly NOT passwordless: the flag alone has to be enough.
    seed_probe(session, false);
    let (context, calls) = counting_context(session);

    run("sudo -n true", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "`sudo -n` fails instead of prompting, so a dialog cannot help it"
    );
}

#[tokio::test]
async fn sudo_passwordless_short_flag_n_inside_a_cluster_never_opens_the_dialog() {
    let session = Uuid::new_v4();
    seed_probe(session, false);
    let (context, calls) = counting_context(session);

    run("sudo -Hn true", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "`-Hn` carries -n in the cluster, so sudo will not prompt"
    );
}

#[tokio::test]
async fn sudo_passwordless_chained_invocation_is_detected() {
    let session = Uuid::new_v4();
    seed_probe(session, true);
    let (context, calls) = counting_context(session);

    run("cd /tmp && sudo true", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a chained sudo is still a sudo, and with a passwordless host it asks nothing"
    );
}

#[tokio::test]
async fn sudo_passwordless_heredoc_body_never_opens_the_dialog() {
    let session = Uuid::new_v4();
    seed_probe(session, true);
    let (context, calls) = counting_context(session);

    run("cat <<'EOF'\nsudo true\nEOF", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the sudo inside a heredoc body is data, not a command"
    );
}

#[tokio::test]
async fn sudo_passwordless_prompting_sudo_still_opens_the_dialog() {
    let session = Uuid::new_v4();
    // Nothing passwordless and no -n: the dialog is the only way through.
    seed_probe(session, false);
    let (context, calls) = counting_context(session);

    run("sudo true", &context).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a sudo that will prompt must still reach the user"
    );
}

// ── Detection ────────────────────────────────────────────────────────
// Pure functions, no process spawned. Every test name carries the
// `sudo_passwordless` prefix so the issue's single filter covers them.

#[test]
fn sudo_passwordless_detects_a_tab_after_the_word() {
    let found = find_sudo_invocations("sudo\ttrue");
    assert_eq!(found.len(), 1, "a tab is still a word boundary");
    assert_eq!(found[0].insert_at, 4);
}

#[test]
fn sudo_passwordless_detects_a_leading_tab() {
    let found = find_sudo_invocations("\tsudo true");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].insert_at, 5);
}

#[test]
fn sudo_passwordless_detects_after_a_separator() {
    let command = "cd /tmp && sudo rm -f x";
    let found = find_sudo_invocations(command);
    assert_eq!(found.len(), 1);
    assert_eq!(&command[..found[0].insert_at], "cd /tmp && sudo");
}

#[test]
fn sudo_passwordless_detects_behind_a_wrapper() {
    assert_eq!(find_sudo_invocations("nohup sudo true").len(), 1);
    assert_eq!(find_sudo_invocations("env FOO=1 sudo true").len(), 1);
    assert_eq!(find_sudo_invocations("timeout 30 sudo true").len(), 1);
}

#[test]
fn sudo_passwordless_ignores_a_longer_word() {
    assert!(
        find_sudo_invocations("sudoku --help").is_empty(),
        "`sudoku` is not `sudo`, and the prefix test could not tell them apart"
    );
}

#[test]
fn sudo_passwordless_ignores_a_quoted_mention() {
    assert!(find_sudo_invocations("echo \"sudo rm -rf /\"").is_empty());
    assert!(find_sudo_invocations("grep -rn 'sudo' src/").is_empty());
}

#[test]
fn sudo_passwordless_ignores_a_heredoc_body() {
    assert!(find_sudo_invocations("cat <<'EOF'\nsudo true\nEOF").is_empty());
    // A quoted `<<` opens no body, so the next line is a real command.
    assert_eq!(find_sudo_invocations("echo \"a << b\"\nsudo true").len(), 1);
}

#[test]
fn sudo_passwordless_reads_n_from_the_flag_cluster() {
    assert!(find_sudo_invocations("sudo -n true")[0].never_prompts);
    assert!(find_sudo_invocations("sudo -Hn true")[0].never_prompts);
    assert!(find_sudo_invocations("sudo --non-interactive true")[0].never_prompts);
    assert!(!find_sudo_invocations("sudo -H true")[0].never_prompts);
    assert!(
        !find_sudo_invocations("sudo -u root true")[0].never_prompts,
        "`-u root`: the operand ends sudo's flags, and `root` carries no n"
    );
}

#[test]
fn sudo_passwordless_injects_after_the_detected_word() {
    let command = "cd /tmp && sudo true";
    let injected = inject_sudo_password_flags(command, &find_sudo_invocations(command));
    assert_eq!(injected, "cd /tmp && sudo -S -p \"\" true");
}

#[test]
fn sudo_passwordless_does_not_repeat_flags_already_present() {
    let command = "sudo -S true";
    let injected = inject_sudo_password_flags(command, &find_sudo_invocations(command));
    assert_eq!(injected, "sudo -p \"\" -S true");
}

#[test]
fn sudo_passwordless_never_rewrites_a_heredoc_body() {
    let command = "cat <<'EOF'\nsudo true\nEOF";
    let injected = inject_sudo_password_flags(command, &find_sudo_invocations(command));
    assert_eq!(injected, command, "no invocation, so the text is untouched");
}

#[test]
fn sudo_passwordless_rewrites_every_invocation() {
    let command = "sudo true && sudo false";
    let injected = inject_sudo_password_flags(command, &find_sudo_invocations(command));
    assert_eq!(injected, "sudo -S -p \"\" true && sudo -S -p \"\" false");
}

// ── the hint that ends the dialog for good (#2157) ────────────────────

#[test]
fn sudo_access_hint_unix_names_the_user_and_the_drop_in() {
    let hint = sudo_access_hint(SudoPlatform::Unix, "ada");
    assert!(
        hint.contains("ada ALL=(ALL) NOPASSWD: ALL"),
        "the rule has to cover the named account: {hint}"
    );
    assert!(
        hint.contains("/etc/sudoers.d/opencrabs"),
        "the drop-in path is what makes the rule stick: {hint}"
    );
    assert!(
        hint.contains("chmod 0440 /etc/sudoers.d/opencrabs"),
        "sudo ignores a drop-in that is not 0440: {hint}"
    );
}

#[test]
fn sudo_access_hint_windows_says_there_is_no_sudoers() {
    let hint = sudo_access_hint(SudoPlatform::Windows, "ada");
    let lower = hint.to_lowercase();
    assert!(lower.contains("no sudoers"), "{hint}");
    assert!(
        lower.contains("elevated") || lower.contains("administrator"),
        "the Windows fix is elevation: {hint}"
    );
    assert!(
        !hint.contains("/etc/sudoers"),
        "a Windows hint must not send the user to a Unix path: {hint}"
    );
}

#[test]
fn sudo_access_hint_substitutes_the_real_account() {
    // The name is read from the environment, so the pasted rule names the
    // account it has to cover instead of a placeholder.
    let user = sudo_account_name();
    assert!(!user.is_empty(), "an account name always resolves");
    let hint = sudo_access_hint(SudoPlatform::Unix, &user);
    assert!(
        hint.contains(&format!("{user} ALL=(ALL) NOPASSWD: ALL")),
        "the hint must carry the resolved account: {hint}"
    );
    assert!(
        !hint.contains("{user}"),
        "an unsubstituted placeholder would paste a broken rule: {hint}"
    );
}

#[test]
fn sudo_access_hint_cached_matches_this_host() {
    let hint = sudo_access_hint_cached();
    assert!(!hint.is_empty(), "the cached hint is never empty");
    match SudoPlatform::current() {
        SudoPlatform::Unix => assert!(hint.contains("/etc/sudoers.d/opencrabs"), "{hint}"),
        SudoPlatform::Windows => assert!(hint.to_lowercase().contains("no sudoers"), "{hint}"),
    }
}

#[test]
fn sudo_access_hint_only_fires_for_a_password_wall() {
    // The hint is appended to a failed command's output, so an ordinary failure
    // that merely mentions sudo must not carry it.
    assert!(looks_like_sudo_password_failure(
        "sudo: a password is required"
    ));
    assert!(looks_like_sudo_password_failure(
        "sudo: no tty present and no askpass program specified"
    ));
    assert!(looks_like_sudo_password_failure(
        "sudo: a terminal is required to read the password"
    ));
    assert!(!looks_like_sudo_password_failure(
        "sudo: /etc/sudoers.d/opencrabs: syntax error near line 1"
    ));
    assert!(!looks_like_sudo_password_failure("cat: nope: No such file"));
}
