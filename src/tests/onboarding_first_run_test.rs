//! Zero setup: a fresh install opens chat on the built-in provider, with the
//! documented config files on disk and the agent greeting the user (#2079).
//!
//! v0.5.10 shipped the opposite. Two separate defects sat behind the same
//! symptom (`New Chat · none / none`, no `config.toml`):
//!
//! 1. `initialize_sync` sent a first run to the wizard. Zero setup means the
//!    built-in provider is live before the user types anything, so the wizard
//!    has nothing to ask on a first run; it is the opt-in path, reached with
//!    `--onboard` / `/onboard`. Nothing provisioned the built-in provider
//!    outside the wizard's completion flow, so a first run that skipped the
//!    wizard never fetched a token and had no provider to greet anyone with.
//! 2. Seeding was gated on an API key already being in the environment, so a
//!    genuinely fresh home finished startup with neither `config.toml` nor
//!    `keys.toml`.
//!
//! These tests scope the config home with `with_home_override` rather than
//! pointing `$HOME` at a tempdir, which is process-global and would let a
//! parallel test resolve into this one's directory (#912).

use crate::config::profile::with_home_override;
use crate::config::seed;
use crate::config::{Config, ProviderConfig};
use crate::tui::app::{LaunchPlan, plan_launch};
use crate::tui::onboarding::OnboardingWizard;
use crate::tui::provider_selector::index_of_provider;

fn enabled() -> ProviderConfig {
    ProviderConfig {
        enabled: true,
        ..Default::default()
    }
}

/// Run `f` against a config home that exists only for this test.
fn in_throwaway_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let home = tempfile::TempDir::new().expect("tempdir");
    let path = home.path().to_path_buf();
    with_home_override(path.clone(), || f(&path))
}

#[test]
fn a_fresh_install_lands_in_chat_and_knows_to_provision() {
    assert_eq!(
        plan_launch(false, true),
        LaunchPlan::Chat { first_run: true },
        "zero setup means the first screen is chat on the built-in provider; \
         the wizard is opt-in, so a first run must never land on it"
    );
}

#[test]
fn an_existing_install_lands_in_chat_without_provisioning() {
    assert_eq!(
        plan_launch(false, false),
        LaunchPlan::Chat { first_run: false },
        "a second launch must not re-run the first-run provisioning"
    );
}

#[test]
fn onboard_is_the_only_way_into_the_wizard() {
    for first_time in [true, false] {
        assert_eq!(
            plan_launch(true, first_time),
            LaunchPlan::Wizard,
            "--onboard must open the wizard (first_time={first_time})"
        );
    }
}

#[test]
fn a_blank_home_gets_both_documented_files() {
    in_throwaway_home(|home| {
        assert!(!home.join("config.toml").exists());
        assert!(!home.join("keys.toml").exists());

        seed::ensure_config_seeded();
        seed::ensure_keys_seeded();

        assert!(
            home.join("config.toml").exists(),
            "a fresh home must get config.toml, the file every setting is \
             edited in"
        );
        assert!(
            home.join("keys.toml").exists(),
            "a fresh home must get keys.toml, where the install token lands"
        );

        // The provider sections are the point of the example: without them
        // there is nothing to hand-edit when a provider setting needs changing.
        let config = std::fs::read_to_string(home.join("config.toml")).expect("config.toml");
        for section in ["[providers.opencrabs]", "[providers.anthropic]"] {
            assert!(
                config.contains(section),
                "seeded config.toml is missing {section}"
            );
        }
    });
}

#[test]
fn the_seeded_built_in_provider_starts_disabled() {
    in_throwaway_home(|_home| {
        seed::ensure_config_seeded();
        let config = Config::load().expect("seeded config loads");
        let opencrabs = config
            .providers
            .opencrabs
            .as_ref()
            .expect("the example carries a built-in provider section");
        assert!(
            !opencrabs.enabled,
            "the example ships every section disabled; the first run is what \
             turns the built-in one on, so this fails if that ever changes"
        );
        assert_eq!(
            opencrabs.default_model.as_deref(),
            Some("deepseek-v4.1-flash"),
            "the built-in provider serves one model"
        );
    });
}

#[test]
fn enabling_the_built_in_provider_makes_it_the_active_pair() {
    in_throwaway_home(|_home| {
        seed::ensure_config_seeded();
        seed::ensure_keys_seeded();
        Config::write_key("providers.opencrabs", "enabled", "true").expect("enable");
        Config::write_keys_key("providers.opencrabs", "api_key", "install-token").expect("token");

        let config = Config::load().expect("config loads");
        assert_eq!(
            config.providers.active_provider_and_model(),
            ("opencrabs".to_string(), "deepseek-v4.1-flash".to_string()),
            "with the section on and a token present, the built-in provider is \
             the pair the session runs on"
        );
    });
}

#[test]
fn the_wizard_opens_on_the_built_in_provider_from_config() {
    let mut config = Config::default();
    config.providers.opencrabs = Some(enabled());
    let wizard = OnboardingWizard::from_config(&config);
    assert_eq!(
        wizard.ps.selected_provider,
        index_of_provider("opencrabs").expect("opencrabs is a provider"),
        "an enabled built-in section must open /onboard on it, not fall \
         through to the custom-provider fallback"
    );
}

#[test]
fn a_config_that_enables_nothing_leaves_the_selection_at_the_default() {
    let wizard = OnboardingWizard::from_config(&Config::default());
    assert_eq!(
        wizard.ps.selected_provider, 0,
        "from_config must answer from the config it was handed, never from \
         the live config.toml on this machine (#2077)"
    );
    assert!(
        wizard.ps.api_key_input.is_empty(),
        "a config with no key must not inherit a key marker from disk"
    );
}
