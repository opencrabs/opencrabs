//! Telegram command parity with the TUI (#1155 follow-up).
//!
//! Three separate defects are pinned here:
//!
//! 1. **Owner resolution.** `allowed_users` is a list whose first entry is
//!    often the wildcard `"*"`. Every owner-menu path used to take
//!    `.first()` and parse it, so a `["*", "<id>"]` config resolved to no
//!    owner at all and the Telegram `/` picker was left with `/start`.
//! 2. **Missing menu chips.** `/restart`, `/exit` and `/rename` were handled
//!    by the dispatcher but never published, so they were undiscoverable.
//! 3. **Missing handlers / silent TUI-only gaps.** `/approve` and `/skills`
//!    had no channel handler, and TUI-only commands (`/theme`, `/whisper`,
//!    `/rebuild`, `/onboard*`) fell through to "unknown command" — which
//!    reads like a bug rather than a surface that does not exist in chat.

use crate::channels::commands::{ChannelCommand, format_help, handle_command, is_tui_only_command};
use crate::channels::telegram::agent::collect_command_catalog;
use crate::channels::telegram::menu_scope::owner_user_id;
use crate::tests::agent_service_mocks::create_test_service_full;

// ── owner resolution ───────────────────────────────────────────────────────

// `owner_user_id(allowed, bot_owner)` delegates to
// `config::owner::resolve_owner_id`, so the menu publishes the same owner the
// command-level owner gate accepts. `bot_owner` decides first; the allow list
// is the positional fallback — and the *first parseable* entry wins there,
// because the shipped convention allows the wildcard `"*"` first in the list.

#[test]
fn owner_user_id_skips_a_leading_wildcard() {
    // The regression: `.first().parse()` returned None for exactly this shape,
    // which silently disabled every owner menu.
    let allowed = vec!["*".to_string(), "1608111860".to_string()];
    assert_eq!(owner_user_id(&allowed, &[]), Some(1_608_111_860));
}

#[test]
fn owner_user_id_finds_the_id_after_junk_entries() {
    let allowed = vec![
        String::new(),
        "@someone".to_string(),
        "not-a-number".to_string(),
        "  42  ".to_string(),
    ];
    assert_eq!(
        owner_user_id(&allowed, &[]),
        Some(42),
        "non-numeric entries must be skipped, not abort the lookup"
    );
}

#[test]
fn owner_user_id_is_none_without_any_numeric_entry() {
    assert_eq!(owner_user_id(&[], &[]), None);
    assert_eq!(owner_user_id(&["*".to_string()], &[]), None);
    assert_eq!(
        owner_user_id(&["*".to_string(), "@bot".to_string()], &[]),
        None
    );
}

#[test]
fn owner_user_id_prefers_the_first_numeric_entry() {
    let allowed = vec!["*".to_string(), "1".to_string(), "2".to_string()];
    assert_eq!(owner_user_id(&allowed, &[]), Some(1));
}

#[test]
fn owner_user_id_honours_an_explicit_bot_owner_over_the_allow_list() {
    // Same precedence as `config::owner::is_owner`: an explicit bot_owner
    // list decides, even when it disagrees with the allow list's first id.
    let allowed = vec!["*".to_string(), "1".to_string()];
    let bot_owner = vec!["2".to_string()];
    assert_eq!(owner_user_id(&allowed, &bot_owner), Some(2));
}

#[test]
fn owner_user_id_skips_a_wildcard_in_the_bot_owner_list() {
    let bot_owner = vec!["*".to_string(), "1608111860".to_string()];
    assert_eq!(owner_user_id(&[], &bot_owner), Some(1_608_111_860));
}

// ── menu catalog ───────────────────────────────────────────────────────────

#[test]
fn catalog_publishes_the_previously_undiscoverable_commands() {
    let catalog = collect_command_catalog();
    let names: Vec<&str> = catalog.iter().map(|c| c.command.as_str()).collect();

    // /restart, /exit and /rename were handled for ages with no chip; /approve
    // and /skills gained handlers in the same change that added the chips.
    for expected in ["restart", "exit", "rename", "approve", "skills"] {
        assert!(
            names.contains(&expected),
            "`/{expected}` must appear in the Telegram menu; catalog = {names:?}"
        );
    }
}

#[test]
fn catalog_has_no_duplicate_command_names() {
    let catalog = collect_command_catalog();
    let mut seen = std::collections::HashSet::new();
    for c in &catalog {
        assert!(
            seen.insert(c.command.clone()),
            "duplicate menu chip `{}` — Telegram renders both",
            c.command
        );
    }
}

#[test]
fn catalog_stays_within_the_telegram_limits() {
    let catalog = collect_command_catalog();
    assert!(
        catalog.len() <= 100,
        "Telegram rejects more than 100 commands, got {}",
        catalog.len()
    );
    for c in &catalog {
        assert!(
            !c.command.is_empty() && c.command.len() <= 32,
            "invalid command name length: `{}`",
            c.command
        );
        assert!(
            c.command
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
            "command `{}` violates Telegram's [a-z0-9_] rule",
            c.command
        );
    }
}

// ── /approve ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn approve_is_denied_for_non_owner() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/approve", sid, &agent, &svc, false, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => assert!(
            msg.to_lowercase().contains("owner"),
            "denial should mention owner-only, got: {msg}"
        ),
        _ => panic!("non-owner /approve must be denied"),
    }
}

#[tokio::test]
async fn approve_reports_the_current_policy_for_the_owner() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/approve", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::Approve(text) => {
            assert!(
                text.to_lowercase().contains("approval policy"),
                "bare /approve must report the policy, got: {text}"
            );
            // The three accepted values must be discoverable from the reply.
            for value in ["approve-only", "auto-session", "auto-always"] {
                assert!(text.contains(value), "reply must list `{value}`: {text}");
            }
        }
        _ => panic!("owner /approve must answer with the current policy"),
    }
}

#[test]
fn approve_rejects_an_unknown_policy_without_writing_config() {
    // Pure string path: the write branch is only reached for the three valid
    // values, so this assertion cannot touch the live config.
    let text = crate::channels::commands::handle_approve_policy("yolo-mode");
    assert!(
        text.contains("Invalid policy"),
        "unknown values must be rejected, got: {text}"
    );
    assert!(
        text.contains("auto-always"),
        "the rejection must name the valid values: {text}"
    );
}

// ── /skills ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn skills_is_denied_for_non_owner() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/skills", sid, &agent, &svc, false, None).await;
    assert!(
        matches!(cmd, ChannelCommand::UnknownCommand(_)),
        "non-owner /skills must be denied"
    );
}

#[tokio::test]
async fn skills_lists_slash_names_for_the_owner() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/skills", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::Skills(text) => {
            assert!(!text.is_empty(), "/skills must always answer with text");
            // Built-in skills ship with the binary, so at least one `/name`
            // must be enumerated regardless of the user's own skills dir.
            assert!(
                text.contains('`') && text.contains('/'),
                "skills listing must show invokeable slashes, got: {text}"
            );
        }
        _ => panic!("owner /skills must return the skill listing"),
    }
}

// ── TUI-only commands ──────────────────────────────────────────────────────

#[test]
fn tui_only_detection_covers_the_real_commands_only() {
    for cmd in [
        "/theme",
        "/theme set dracula",
        "/whisper",
        "/rebuild",
        "/onboard",
        "/onboard:voice",
    ] {
        assert!(
            is_tui_only_command(cmd),
            "`{cmd}` must be recognised as TUI-only"
        );
    }
    for cmd in ["/models", "/themer", "/theme-ish", "/onboarding-notes"] {
        assert!(
            !is_tui_only_command(cmd),
            "`{cmd}` must NOT be swallowed by the TUI-only arm"
        );
    }
}

#[tokio::test]
async fn tui_only_commands_explain_themselves_in_chat() {
    let (agent, svc, sid) = create_test_service_full().await;
    for input in ["/theme", "/whisper", "/rebuild", "/onboard:voice"] {
        let cmd = handle_command(input, sid, &agent, &svc, true, None).await;
        match cmd {
            ChannelCommand::TuiOnly(text) => {
                assert!(
                    text.contains("TUI"),
                    "`{input}` must say it is TUI-only, got: {text}"
                );
                assert!(
                    !text.to_lowercase().contains("unknown command"),
                    "`{input}` must not be reported as unknown: {text}"
                );
            }
            _ => panic!("`{input}` must return the TUI-only explanation"),
        }
    }
}

#[tokio::test]
async fn tui_only_explanations_are_not_owner_gated() {
    // These replies carry no privileged information, and a non-owner hitting
    // one should get the explanation rather than a lock emoji.
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/whisper", sid, &agent, &svc, false, None).await;
    assert!(matches!(cmd, ChannelCommand::TuiOnly(_)));
}

// ── /help ──────────────────────────────────────────────────────────────────

#[test]
fn help_documents_the_newly_reachable_commands() {
    let help = format_help();
    for cmd in ["/approve", "/skills"] {
        assert!(help.contains(cmd), "/help must document `{cmd}`: {help}");
    }
}
