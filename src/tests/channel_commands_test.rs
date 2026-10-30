//! Tests for channel commands: format_number, format_help, provider helpers,
//! and user command matching.

use crate::brain::commands::UserCommand;
use crate::channels::commands::{
    ChannelCommand, format_help, format_number, match_user_command_inner, normalize_provider_name,
    provider_display_name, provider_names_match,
};
use crate::tests::agent_service_mocks::create_test_service_full;

// ── format_number ─────────────────────────────────────────────────────

#[test]
fn format_number_small() {
    assert_eq!(format_number(0), "0");
    assert_eq!(format_number(1), "1");
    assert_eq!(format_number(999), "999");
}

#[test]
fn format_number_thousands() {
    assert_eq!(format_number(1_000), "1.0K");
    assert_eq!(format_number(1_500), "1.5K");
    assert_eq!(format_number(999_999), "1000.0K");
}

#[test]
fn format_number_millions() {
    assert_eq!(format_number(1_000_000), "1.0M");
    assert_eq!(format_number(2_500_000), "2.5M");
    assert_eq!(format_number(123_456_789), "123.5M");
}

// ── format_help ───────────────────────────────────────────────────────

#[test]
fn format_help_shows_the_running_version() {
    // #696: the channel /help must surface the build version so users on any
    // channel can see which version they're on.
    let help = format_help();
    assert!(
        help.contains(&format!("OpenCrabs v{}", env!("CARGO_PKG_VERSION"))),
        "help must show the running version: {help}"
    );
}

#[test]
fn format_help_contains_all_commands() {
    let help = format_help();
    for cmd in [
        "/cd",
        "/evolve",
        "/exit",
        "/help",
        "/models",
        "/new",
        "/restart",
        "/sessions",
        "/stop",
        "/usage",
    ] {
        assert!(help.contains(cmd), "help text missing {}", cmd);
    }
}

#[test]
fn format_help_most_used_commands_first() {
    let help = format_help();
    let builtin_section = help.split("Custom Commands").next().unwrap_or(&help);
    let commands: Vec<&str> = builtin_section
        .lines()
        .filter_map(|line| {
            // Handle both bare backtick lines and markdown table rows:
            //   bare:  `/new` description
            //   table: | `/new` | description |
            let content = line.trim();
            let after_pipe = content.strip_prefix('|').unwrap_or(content);
            let trimmed = after_pipe.trim().strip_prefix('`')?;
            let cmd = trimmed.split('`').next()?;
            if cmd.starts_with('/') {
                Some(cmd.split_whitespace().next().unwrap_or(cmd))
            } else {
                None
            }
        })
        .collect();
    // Most-used commands (/new, /cd, /sessions, /stop) should be at the top
    assert_eq!(commands.first(), Some(&"/new"), "/new should be first");
    assert_eq!(commands.get(1), Some(&"/cd"), "/cd should be second");
    assert_eq!(
        commands.get(2),
        Some(&"/sessions"),
        "/sessions should be third"
    );
    assert_eq!(commands.get(3), Some(&"/stop"), "/stop should be fourth");
    // Remaining commands should be alphabetical
    let rest: Vec<&str> = commands[4..].to_vec();
    let mut sorted_rest = rest.clone();
    sorted_rest.sort();
    assert_eq!(
        rest, sorted_rest,
        "commands after /new, /cd, /sessions, /stop should be alphabetical"
    );
}

// ── provider_display_name ─────────────────────────────────────────────

#[test]
fn provider_display_name_known() {
    assert_eq!(provider_display_name("anthropic"), "Anthropic");
    assert_eq!(provider_display_name("openai"), "OpenAI");
    assert_eq!(provider_display_name("github"), "GitHub Copilot");
    assert_eq!(provider_display_name("openrouter"), "OpenRouter");
    assert_eq!(provider_display_name("minimax"), "MiniMax");
    assert_eq!(provider_display_name("gemini"), "Gemini");
}

#[test]
fn provider_aliases_normalize_and_match() {
    assert_eq!(normalize_provider_name("GitHub Copilot"), "github");
    assert_eq!(normalize_provider_name("Google Gemini"), "gemini");
    assert_eq!(
        normalize_provider_name("custom(DeepSeek)"),
        "custom:deepseek"
    );
    assert!(provider_names_match("custom:deepseek", "deepseek"));
}

#[test]
fn provider_display_name_custom() {
    assert_eq!(provider_display_name("custom:deepseek"), "deepseek");
    assert_eq!(provider_display_name("custom:local-llm"), "local-llm");
}

#[test]
fn provider_display_name_unknown() {
    assert_eq!(provider_display_name("mystery"), "mystery");
}

// ── match_user_command_inner ──────────────────────────────────────────

fn make_cmd(name: &str, action: &str, prompt: &str) -> UserCommand {
    UserCommand {
        name: name.to_string(),
        description: String::new(),
        action: action.to_string(),
        prompt: prompt.to_string(),
    }
}

fn variant_name(cmd: &ChannelCommand) -> &'static str {
    match cmd {
        ChannelCommand::Compact => "Compact",
        ChannelCommand::ClearContext => "ClearContext",
        ChannelCommand::Help(_) => "Help",
        ChannelCommand::Usage(_) => "Usage",
        ChannelCommand::MissionControl(_) => "MissionControl",
        ChannelCommand::Models(_) => "Models",
        ChannelCommand::ModelSwitched(_) => "ModelSwitched",
        ChannelCommand::NewSession => "NewSession",
        ChannelCommand::Sessions(_) => "Sessions",
        ChannelCommand::Stop => "Stop",
        ChannelCommand::StopTarget(_) => "StopTarget",
        ChannelCommand::UserPrompt(_) => "UserPrompt",
        ChannelCommand::PlanModeWithQuery(_) => "PlanModeWithQuery",
        ChannelCommand::UserSystem(_) => "UserSystem",
        ChannelCommand::Doctor => "Doctor",
        ChannelCommand::Restart => "Restart",
        ChannelCommand::Exit => "Exit",
        ChannelCommand::Evolve => "Evolve",
        ChannelCommand::Rtk(_) => "Rtk",
        ChannelCommand::Rename(_) => "Rename",
        ChannelCommand::ChangeDir(_) => "ChangeDir",
        ChannelCommand::Architecture(_) => "Architecture",
        ChannelCommand::Attach(_) => "Attach",
        ChannelCommand::Audit(_) => "Audit",
        ChannelCommand::Profiles(_) => "Profiles",
        ChannelCommand::RespondTo(_) => "RespondTo",
        ChannelCommand::Redact(_) => "Redact",
        ChannelCommand::UnknownCommand(_) => "UnknownCommand",
        ChannelCommand::NotACommand => "NotACommand",
        ChannelCommand::PlanMode(_) => "PlanMode",
        ChannelCommand::ShowPlan(_) => "ShowPlan",
        ChannelCommand::ExecutePlan => "ExecutePlan",
        ChannelCommand::DiscardPlan => "DiscardPlan",
    }
}

#[test]
fn user_command_prompt_no_args() {
    let cmds = vec![make_cmd(
        "/credits",
        "prompt",
        "Check my OpenRouter credits",
    )];
    match match_user_command_inner("/credits", &cmds, &[]) {
        ChannelCommand::UserPrompt(p) => assert_eq!(p, "Check my OpenRouter credits"),
        other => panic!("expected UserPrompt, got {:?}", variant_name(&other)),
    }
}

#[test]
fn dash_command_matches_underscore_form_from_telegram_menu() {
    // Telegram's command menu only allows `[a-z0-9_]`, so `/x-engage` is
    // registered (and tapped) as `/x_engage`. Tapping the menu entry must hit
    // the same definition as typing the original dash form (issue #196).
    let cmds = vec![make_cmd("/x-engage", "prompt", "Engage on X")];
    match match_user_command_inner("/x_engage", &cmds, &[]) {
        ChannelCommand::UserPrompt(p) => assert_eq!(p, "Engage on X"),
        other => panic!("expected UserPrompt, got {:?}", variant_name(&other)),
    }
    // The original dash form (and args) still works when typed directly.
    match match_user_command_inner("/x-engage now", &cmds, &[]) {
        ChannelCommand::UserPrompt(p) => assert_eq!(p, "Engage on X now"),
        other => panic!("expected UserPrompt, got {:?}", variant_name(&other)),
    }
}

#[test]
fn user_command_prompt_with_args() {
    let cmds = vec![make_cmd("/deploy", "prompt", "Deploy the service")];
    match match_user_command_inner("/deploy staging --dry-run", &cmds, &[]) {
        ChannelCommand::UserPrompt(p) => assert_eq!(p, "Deploy the service staging --dry-run"),
        other => panic!("expected UserPrompt, got {:?}", variant_name(&other)),
    }
}

#[test]
fn user_command_system_action() {
    let cmds = vec![make_cmd("/info", "system", "OpenCrabs v0.2")];
    match match_user_command_inner("/info", &cmds, &[]) {
        ChannelCommand::UserSystem(t) => assert_eq!(t, "OpenCrabs v0.2"),
        other => panic!("expected UserSystem, got {:?}", variant_name(&other)),
    }
}

#[test]
fn user_command_unknown_returns_unknown_command() {
    let cmds = vec![make_cmd("/credits", "prompt", "Check credits")];
    match match_user_command_inner("/unknown", &cmds, &[]) {
        ChannelCommand::UnknownCommand(msg) => assert!(msg.contains("/unknown")),
        other => panic!("expected UnknownCommand, got {:?}", variant_name(&other)),
    }
}

#[test]
fn user_command_empty_list_returns_unknown_command() {
    match match_user_command_inner("/anything", &[], &[]) {
        ChannelCommand::UnknownCommand(msg) => assert!(msg.contains("/anything")),
        other => panic!("expected UnknownCommand, got {:?}", variant_name(&other)),
    }
}

#[test]
fn user_command_default_action_is_prompt() {
    let cmds = vec![make_cmd("/test", "whatever", "test prompt")];
    assert!(matches!(
        match_user_command_inner("/test", &cmds, &[]),
        ChannelCommand::UserPrompt(_)
    ));
}

// ── format_providers (session-aware) ──────────────────────────────────

#[tokio::test]
async fn models_command_shows_session_provider_after_override() {
    // When a session has a provider override, /models should show the session's
    // provider, not the global one. This tests the fix for issue #543.
    use crate::tests::agent_service_mocks::MockProviderWithModel;
    use std::sync::Arc;

    let (agent, session_svc, session_id) = create_test_service_full().await;
    // Create a different provider for the session
    let openai_provider = Arc::new(MockProviderWithModel::new("openai", "gpt-4"));
    // Swap to openai for this session
    agent.swap_provider_for_session(session_id, openai_provider, "gpt-4");
    // Now call /models command
    let cmd = crate::channels::commands::handle_command(
        "/models",
        session_id,
        &agent,
        &session_svc,
        true, // is_owner
        None,
    )
    .await;
    // Should return Models variant with session's provider
    match cmd {
        ChannelCommand::Models(response) => {
            assert_eq!(response.current_provider, "openai");
            assert_eq!(response.current_model, "gpt-4");
            assert!(response.text.contains("openai"));
            assert!(response.text.contains("gpt-4"));
        }
        other => panic!("expected Models command, got {:?}", variant_name(&other)),
    }
}

#[tokio::test]
async fn models_command_shows_global_provider_when_no_session_override() {
    // When a session has no provider override, /models should show the global/default provider.
    let (agent, session_svc, session_id) = create_test_service_full().await;
    // Don't swap provider - use the default mock provider
    let cmd = crate::channels::commands::handle_command(
        "/models",
        session_id,
        &agent,
        &session_svc,
        true, // is_owner
        None,
    )
    .await;
    // Should return Models variant with default provider
    match cmd {
        ChannelCommand::Models(response) => {
            assert_eq!(response.current_provider, "mock");
            assert_eq!(response.current_model, "mock-model");
            assert!(response.text.contains("mock"));
            assert!(response.text.contains("mock-model"));
        }
        other => panic!("expected Models command, got {:?}", variant_name(&other)),
    }
}

#[tokio::test]
async fn plan_with_query_enters_plan_mode_and_carries_the_query() {
    // `/plan <query>` arms Plan mode and returns the trailing text as the
    // planning intent so the agent drafts the design in one step (#579).
    use crate::config::profile::{home_for_profile, with_profile_home_async};
    let profile = format!("plan-query-test-{}", uuid::Uuid::new_v4());
    with_profile_home_async(Some(&profile), async {
        let (agent, session_svc, session_id) = create_test_service_full().await;
        let cmd = crate::channels::commands::handle_command(
            "/plan audit the last 2 closed issues",
            session_id,
            &agent,
            &session_svc,
            true, // is_owner
            None,
        )
        .await;
        match cmd {
            ChannelCommand::PlanModeWithQuery(q) => {
                assert_eq!(q, "audit the last 2 closed issues");
            }
            other => panic!("expected PlanModeWithQuery, got {:?}", variant_name(&other)),
        }
    })
    .await;
    let _ = std::fs::remove_dir_all(home_for_profile(Some(&profile)));
}

#[tokio::test]
async fn bare_plan_still_enters_plan_mode() {
    use crate::config::profile::{home_for_profile, with_profile_home_async};
    let profile = format!("plan-bare-test-{}", uuid::Uuid::new_v4());
    with_profile_home_async(Some(&profile), async {
        let (agent, session_svc, session_id) = create_test_service_full().await;
        let cmd = crate::channels::commands::handle_command(
            "/plan",
            session_id,
            &agent,
            &session_svc,
            true,
            None,
        )
        .await;
        assert!(
            matches!(cmd, ChannelCommand::PlanMode(_)),
            "bare /plan enters Plan mode and prompts for a description, got {:?}",
            variant_name(&cmd)
        );
    })
    .await;
    let _ = std::fs::remove_dir_all(home_for_profile(Some(&profile)));
}
