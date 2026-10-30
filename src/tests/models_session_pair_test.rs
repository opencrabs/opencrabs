//! #2060: `/models` opens on the session's provider and model, and the config
//! fallback only ever picks an enabled provider.
//!
//! #2077: every test here runs under a temporary home. `from_config` reaches
//! `OnboardingWizard::new()` and `reload_config_models()`, and both read
//! `config.toml` from disk, so without the override the answer depends on the
//! machine running the suite. That is what turned CI run 38878570375 red: the
//! runner's own config enabled `claude-cli`, and the selection came out as 8
//! where the fixture asked for the built-in provider at 17.

use crate::config::profile::with_home_override;
use crate::config::{Config, ProviderConfig};
use crate::tui::onboarding::OnboardingWizard;
use crate::tui::provider_selector::{CUSTOM_INSTANCES_START, index_of_provider};
use std::collections::HashMap;

/// Run `f` with `opencrabs_home()` pointed at a throwaway directory, so nothing
/// here can read or write the developer's real `~/.opencrabs`.
fn in_temp_home<T>(f: impl FnOnce() -> T) -> T {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&home).expect("create .opencrabs");
    with_home_override(home, f)
}

fn enabled() -> ProviderConfig {
    ProviderConfig {
        enabled: true,
        ..Default::default()
    }
}

fn custom(base: &str, enabled: bool) -> ProviderConfig {
    ProviderConfig {
        enabled,
        base_url: Some(base.to_string()),
        ..Default::default()
    }
}

#[test]
fn an_enabled_builtin_opencrabs_provider_is_the_one_opened() {
    in_temp_home(|| {
        let mut config = Config::default();
        config.providers.opencrabs = Some(enabled());
        let wizard = OnboardingWizard::from_config(&config);
        assert_eq!(
            wizard.ps.selected_provider,
            index_of_provider("opencrabs").expect("opencrabs is a provider")
        );
    });
}

/// The regression behind #2077: a `config.toml` on disk that enables some other
/// provider must not move the selection. The wizard follows the config it was
/// handed, never the machine it happens to run on.
#[test]
fn a_config_on_disk_never_wins_over_the_config_handed_in() {
    in_temp_home(|| {
        let home = crate::config::opencrabs_home();
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::write(
            home.join("config.toml"),
            "[providers.claude_cli]\nenabled = true\n",
        )
        .expect("write the disk config");

        // Prove the fixture is real before trusting the assertion below. If the
        // section name were wrong, the test would pass for the wrong reason:
        // the leak it guards against could not happen in the first place.
        let from_disk = Config::load().expect("load the temp-home config");
        assert!(
            from_disk
                .providers
                .claude_cli
                .as_ref()
                .is_some_and(|p| p.enabled),
            "fixture must actually enable claude-cli on disk"
        );

        let mut config = Config::default();
        config.providers.opencrabs = Some(enabled());
        let wizard = OnboardingWizard::from_config(&config);
        assert_eq!(
            wizard.ps.selected_provider,
            index_of_provider("opencrabs").expect("opencrabs is a provider"),
            "the wizard must follow the config it was handed, not the one on disk (#2077)"
        );
    });
}

#[test]
fn a_disabled_custom_provider_is_never_selected_by_fallback() {
    in_temp_home(|| {
        let mut config = Config::default();
        let mut customs = HashMap::new();
        customs.insert(
            "baidu-ernie".to_string(),
            custom("https://qianfan.example/v2", false),
        );
        config.providers.custom = Some(customs.into_iter().collect());
        let wizard = OnboardingWizard::from_config(&config);
        assert!(
            wizard.ps.selected_provider < CUSTOM_INSTANCES_START,
            "a disabled custom provider must not be opened; got index {}",
            wizard.ps.selected_provider
        );
    });
}

#[test]
fn the_session_custom_provider_is_selected_by_name() {
    in_temp_home(|| {
        let mut customs = HashMap::new();
        customs.insert("alpha".to_string(), custom("https://a.example/v1", true));
        customs.insert("beta".to_string(), custom("https://b.example/v1", true));
        let mut config = Config::default();
        config.providers.custom = Some(customs.into_iter().collect());

        let mut wizard = OnboardingWizard::from_config(&config);
        let found = wizard.select_session_pair("custom:beta", "gpt-x", &config);
        assert!(found);
        let pos = config
            .providers
            .custom
            .as_ref()
            .unwrap()
            .keys()
            .position(|k| k == "beta")
            .unwrap();
        assert_eq!(wizard.ps.selected_provider, CUSTOM_INSTANCES_START + pos);
        assert_eq!(wizard.ps.custom_name, "beta");
        assert_eq!(wizard.ps.base_url, "https://b.example/v1");
        assert_eq!(wizard.ps.custom_model, "gpt-x");
    });
}

#[test]
fn the_session_builtin_model_is_the_one_selected_even_when_unlisted() {
    in_temp_home(|| {
        let config = Config::default();
        let mut wizard = OnboardingWizard::from_config(&config);
        let found = wizard.select_session_pair("zai", "glm-session-only", &config);
        assert!(found);
        assert_eq!(
            wizard.ps.selected_provider,
            index_of_provider("zai").unwrap()
        );
        let all = wizard.ps.all_model_names();
        assert_eq!(all[wizard.ps.selected_model], "glm-session-only");
    });
}

#[test]
fn an_unknown_session_provider_leaves_the_dialog_alone() {
    in_temp_home(|| {
        let config = Config::default();
        let mut wizard = OnboardingWizard::from_config(&config);
        let before = wizard.ps.selected_provider;
        let found = wizard.select_session_pair("custom:gone", "m", &config);
        assert!(!found);
        assert_eq!(wizard.ps.selected_provider, before);
    });
}
