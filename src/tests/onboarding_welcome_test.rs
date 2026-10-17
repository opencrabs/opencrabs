//! Tests for the first-time onboarding welcome message.
//!
//! Covers the `is_first_time` flag on `OnboardingWizard` and the
//! `WELCOME_MESSAGE` constant used in `dialogs.rs`.

use crate::tui::onboarding::{
    OnboardingWizard, SetupGap, WELCOME_MESSAGE, WELCOME_QUICK_START_MESSAGE, WizardMode,
    quick_start_welcome, welcome_message_for,
};

// ── OnboardingWizard::is_first_time ─────────────────────────────

#[test]
fn wizard_is_first_time_defaults_to_false() {
    let w = OnboardingWizard::new();
    assert!(!w.is_first_time);
}

#[test]
fn wizard_is_first_time_can_be_set_true() {
    let mut w = OnboardingWizard::new();
    w.is_first_time = true;
    assert!(w.is_first_time);
}

#[test]
fn wizard_is_first_time_can_be_toggled_back() {
    let mut w = OnboardingWizard::new();
    w.is_first_time = true;
    assert!(w.is_first_time);
    w.is_first_time = false;
    assert!(!w.is_first_time);
}

// ── WELCOME_MESSAGE constant ────────────────────────────────────

#[test]
fn welcome_message_is_non_empty() {
    assert!(!WELCOME_MESSAGE.is_empty());
}

#[test]
fn welcome_message_contains_cron_manage() {
    assert!(
        WELCOME_MESSAGE.to_lowercase().contains("cron_manage"),
        "welcome message should mention cron_manage for task scheduling"
    );
}

#[test]
fn welcome_message_contains_heartbeat() {
    assert!(
        WELCOME_MESSAGE.to_lowercase().contains("heartbeat"),
        "welcome message should mention heartbeat"
    );
}

#[test]
fn welcome_message_contains_onboarding_complete() {
    assert!(
        WELCOME_MESSAGE
            .to_lowercase()
            .contains("onboarding complete"),
        "welcome message should contain 'onboarding complete'"
    );
}

#[test]
fn welcome_message_mentions_personality() {
    assert!(
        WELCOME_MESSAGE.contains("SOUL.md") && WELCOME_MESSAGE.contains("USER.md"),
        "welcome message should reference SOUL.md and USER.md for personality"
    );
}

#[test]
fn welcome_message_is_system_prompt() {
    assert!(
        WELCOME_MESSAGE.starts_with("[SYSTEM:"),
        "welcome message should start with [SYSTEM: to be hidden from user display"
    );
}

// ── welcome_message_for (quick start vs advanced) ───────────────

#[test]
fn quick_start_gets_the_interview_prompt_for_the_session_model() {
    assert_eq!(
        welcome_message_for(WizardMode::QuickStart, "zai", "glm-5.3-flash", &[]),
        quick_start_welcome("zai", "glm-5.3-flash", &[])
    );
}

#[test]
fn advanced_keeps_the_existing_welcome() {
    assert_eq!(
        welcome_message_for(WizardMode::Advanced, "zai", "glm-5.3-flash", &[]),
        WELCOME_MESSAGE
    );
}

#[test]
fn quick_start_welcome_fills_the_session_provider_and_model() {
    let p = quick_start_welcome("opencrabs", "cb/deepseek-v4.1-flash", &[]);
    assert!(p.contains("the opencrabs provider with model cb/deepseek-v4.1-flash"));
    assert!(p.contains("brain is online on cb/deepseek-v4.1-flash"));
    assert!(!p.contains("{provider}"));
    assert!(!p.contains("{model}"));
}

#[test]
fn quick_start_welcome_follows_the_session_not_a_hardcoded_model() {
    let p = quick_start_welcome("anthropic", "claude-sonnet-5-5", &[]);
    assert!(p.contains("claude-sonnet-5-5"));
    assert!(!p.contains("cb/deepseek-v4.1-flash"));
    assert!(!p.contains("deepseek-v4.1-flash"));
}

#[test]
fn welcome_prompts_have_no_em_dash() {
    assert!(!WELCOME_MESSAGE.contains('\u{2014}'));
    assert!(!quick_start_welcome("opencrabs", "cb/deepseek-v4.1-flash", &[]).contains('\u{2014}'));
}

#[test]
fn quick_start_prompt_asks_about_the_user_and_fills_user_md() {
    let p = WELCOME_QUICK_START_MESSAGE;
    assert!(p.starts_with("[SYSTEM:"));
    assert!(p.contains("USER.md"));
    assert!(p.contains("write_opencrabs_file"));
    assert!(p.contains("operation=append"));
}

#[test]
fn quick_start_prompt_leaves_soul_alone_unless_asked() {
    assert!(WELCOME_QUICK_START_MESSAGE.contains("Do not rewrite SOUL.md"));
}

#[test]
fn quick_start_prompt_asks_only_about_gaps() {
    let p = WELCOME_QUICK_START_MESSAGE;
    assert!(p.contains("{gaps}"));
    assert!(p.contains("never ask about it"));
    assert!(!p.contains("skipped the profile questions"));
}

#[test]
fn quick_start_welcome_lists_the_remaining_gaps() {
    let p = quick_start_welcome(
        "opencrabs",
        "cb/deepseek-v4.1-flash",
        &[SetupGap::Name, SetupGap::ImageGeneration],
    );
    assert!(p.contains("Still missing: name (USER.md), image generation."));
    assert!(!p.contains("{gaps}"));
}

#[test]
fn quick_start_welcome_says_when_nothing_is_missing() {
    let p = quick_start_welcome("opencrabs", "cb/deepseek-v4.1-flash", &[]);
    assert!(p.contains("Still missing: nothing, everything is set."));
}

#[test]
fn quick_start_welcome_points_to_models_and_advanced() {
    let p = quick_start_welcome("opencrabs", "cb/deepseek-v4.1-flash", &[]);
    assert!(p.contains("/models"));
    assert!(p.contains("/onboard"));
    assert!(p.contains("Advanced"));
}
