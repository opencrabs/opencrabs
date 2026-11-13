//! Quick-jump saves must not rewrite provider flags the user never touched
//! (#2265).
//!
//! Regression: a `/models` quick-jump (or any wizard run that opened a page
//! that is not ProviderAuth) walked to Complete, and `apply_config` counted
//! that as a provider save. It then swept EVERY `[providers.*].enabled` flag
//! to false and re-enabled the one on the wizard's stale page, so providers
//! the user had deliberately left on were silently switched off.
//!
//! The exclusive enable itself is intentional (resolver ladder, #2262): a run
//! that DID go through the provider step still leaves exactly one provider on.
//! These tests pin both sides.

use crate::config::profile::with_home_override;
use crate::tui::onboarding::{OnboardingStep, OnboardingWizard};

fn temp_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&home).expect("create .opencrabs");
    (dir, home)
}

fn flag(home: &std::path::Path, segments: &[&str], key: &str) -> Option<bool> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = raw.parse().expect("valid TOML");
    let mut table: &dyn toml_edit::TableLike = doc.as_table();
    for seg in segments {
        table = table.get(seg)?.as_table_like()?;
    }
    match table.get(key)? {
        toml_edit::Item::Value(v) => v.as_bool().or_else(|| v.as_str().map(|s| s == "true")),
        _ => None,
    }
}

#[test]
fn quick_jump_complete_without_the_provider_step_preserves_every_flag() {
    let (_temp, home) = temp_home();
    with_home_override(home.clone(), || {
        // Disk truth: two providers are on, set up long before this run.
        crate::config::Config::write_key("providers.openrouter", "enabled", "true").unwrap();
        crate::config::Config::write_key("providers.anthropic", "enabled", "true").unwrap();

        // A quick-jump run that ends on Complete without visiting ProviderAuth.
        let w = OnboardingWizard {
            step: OnboardingStep::Complete,
            quick_jump: true,
            ..OnboardingWizard::default()
        };
        w.apply_config().expect("apply succeeds");

        assert_eq!(
            flag(&home, &["providers", "openrouter"], "enabled"),
            Some(true),
            "a run that never opened the provider page has no business disabling openrouter"
        );
        assert_eq!(
            flag(&home, &["providers", "anthropic"], "enabled"),
            Some(true),
            "same for every other provider the user left on"
        );
    });
}

#[test]
fn a_run_that_went_through_the_provider_step_still_leaves_one_enabled() {
    let (_temp, home) = temp_home();
    with_home_override(home.clone(), || {
        crate::config::Config::write_key("providers.openrouter", "enabled", "true").unwrap();
        crate::config::Config::write_key("providers.anthropic", "enabled", "false").unwrap();

        // Leaving ProviderAuth is what marks the run; the wizard default
        // selection is Anthropic, so that is the one left on.
        let w = OnboardingWizard {
            step: OnboardingStep::Complete,
            quick_jump: true,
            provider_step_touched: true,
            ..OnboardingWizard::default()
        };
        w.apply_config().expect("apply succeeds");

        assert_eq!(
            flag(&home, &["providers", "anthropic"], "enabled"),
            Some(true),
            "the provider the user configured must be enabled"
        );
        assert_eq!(
            flag(&home, &["providers", "openrouter"], "enabled"),
            Some(false),
            "the exclusive enable stays for a real provider save (#2262)"
        );
    });
}

#[test]
fn the_coming_sweep_is_visible_before_it_happens() {
    let (_temp, home) = temp_home();
    with_home_override(home.clone(), || {
        crate::config::Config::write_key("providers.openrouter", "enabled", "true").unwrap();
        crate::config::Config::write_key("providers.anthropic", "enabled", "false").unwrap();

        let w = OnboardingWizard {
            step: OnboardingStep::Complete,
            ..OnboardingWizard::default()
        };
        let doomed = w.providers_to_be_disabled();
        assert_eq!(
            doomed,
            vec!["openrouter".to_string()],
            "the Complete step must be able to name the flags it is about to clear; \
             the selected provider itself never appears"
        );
    });
}