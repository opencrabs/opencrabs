// Witness for #2151: a headless install could never mint the built-in
// provider's token.
//
// `ensure_install` was only ever reached from the TUI (`provision_first_run`,
// and the wizard). `opencrabs run`, `daemon` and the cron scheduler built
// their provider from a config whose `[providers.opencrabs] api_key` was empty,
// `try_create_opencrabs` returned `Ok(None)`, and the command died with "No
// provider configured" on a home that had no TTY to fix it from.
//
// The fix is `opencrabs_acquire::provision_builtin_provider`, wired into the
// commands that build a provider. These tests cover the two things that can go
// wrong without a network: provisioning a home that did not need it (hijacking
// a configured install), and writing the token somewhere the factory cannot
// see it (keys.toml updated, config in hand still empty).
//
// No test here calls `provision_builtin_provider` itself: it writes config.toml
// and keys.toml, and a test must never touch the live home.

use crate::brain::provider::opencrabs_acquire::{apply_install, needs_builtin_provision};
use crate::config::{Config, ProviderConfig};

/// A provider section with just the two fields this decision reads.
fn section(enabled: bool, api_key: Option<&str>) -> ProviderConfig {
    ProviderConfig {
        enabled,
        api_key: api_key.map(str::to_string),
        ..ProviderConfig::default()
    }
}

/// A config carrying exactly the opencrabs section given, nothing else.
fn config_with_builtin(builtin: Option<ProviderConfig>) -> Config {
    let mut config = Config::default();
    config.providers.opencrabs = builtin;
    config
}

fn active_provider(config: &Config) -> String {
    config.providers.active_provider_and_model().0
}

#[test]
fn a_fresh_home_needs_the_built_in_provider() {
    // What the documented example leaves behind: the section exists, shipped
    // disabled, with no token, and nothing else is configured.
    let config = config_with_builtin(Some(section(false, None)));

    assert_eq!(
        active_provider(&config),
        "none",
        "a tokenless built-in section must read as no provider; the registry \
         marks opencrabs requires_api_key"
    );
    assert!(
        needs_builtin_provision(&config),
        "a fresh home has no provider and no token, which is exactly the \
         install #2151 left dead on a headless start"
    );
}

#[test]
fn a_keyless_enabled_section_still_needs_one() {
    // The reported deployment: config.toml sets `enabled = true` and
    // `default_provider = "opencrabs"` but keys.toml has no token. Enabled is
    // not enough, and the gate must not mistake it for a working provider.
    let config = config_with_builtin(Some(section(true, Some(""))));

    // The registry walk only asks whether `api_key` is `Some`, and an empty
    // string is `Some`, so it answers "opencrabs" for an install that cannot
    // make a request. That is why the gate switches the section off and asks
    // about the rest, instead of trusting this answer.
    assert_eq!(
        active_provider(&config),
        "opencrabs",
        "documents the trap: active_provider_and_model does not filter empty keys"
    );
    assert!(
        needs_builtin_provision(&config),
        "an enabled section with an empty token cannot serve a request"
    );
}

#[test]
fn a_configured_provider_is_never_hijacked() {
    // A user who set up their own provider and never ran onboarding (or whose
    // built-in section is still disabled) must not have opencrabs switched on
    // underneath them.
    let mut config = config_with_builtin(Some(section(false, None)));
    config.providers.anthropic = Some(section(true, Some("sk-ant-test")));

    assert_eq!(active_provider(&config), "anthropic");
    assert!(
        !needs_builtin_provision(&config),
        "a working provider means this install is not the one #2151 describes; \
         provisioning here would replace a deliberate choice"
    );
}

#[test]
fn a_stored_token_needs_no_provisioning() {
    let config = config_with_builtin(Some(section(true, Some("issued-token"))));

    assert_eq!(active_provider(&config), "opencrabs");
    assert!(
        !needs_builtin_provision(&config),
        "a stored install is reused as-is; a second launch must not call the \
         proxy again"
    );
}

#[test]
fn a_config_without_the_section_is_left_alone() {
    // A hand-written config that never mentions opencrabs is not a broken
    // install, it is someone else's setup.
    let config = config_with_builtin(None);

    assert!(
        !needs_builtin_provision(&config),
        "provisioning a section the user does not have would invent config"
    );
}

#[test]
fn applying_an_install_makes_the_token_visible_to_the_factory() {
    // `ensure_install` writes keys.toml, but the caller already holds a Config
    // it loaded before that write, and `try_create_opencrabs` reads the token
    // from the config. Without the in-memory apply the command keeps failing
    // with a valid token sitting on disk.
    let mut config = config_with_builtin(Some(section(false, None)));
    assert_eq!(active_provider(&config), "none");

    apply_install(&mut config, "issued-token");

    let builtin = config
        .providers
        .opencrabs
        .as_ref()
        .expect("the section was there before the apply");
    assert!(
        builtin.enabled,
        "the section must be enabled to be selected"
    );
    assert_eq!(
        builtin.api_key.as_deref(),
        Some("issued-token"),
        "the factory reads this field, not keys.toml"
    );
    assert_eq!(
        active_provider(&config),
        "opencrabs",
        "after the apply the config reports the built-in provider, which is \
         what the factory needs to build one"
    );
}
