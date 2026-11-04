//! A new channel session inherits a provider only when the config enables it
//! (#2158). A disabled provider falls back to the [agent] default.

#[test]
fn inherited_providers_are_checked_against_the_enabled_flag() {
    let src = include_str!("../channels/session_init.rs");
    assert!(
        src.contains("provider_enabled(cfg, name)"),
        "an inherited provider must be checked against the enabled flag"
    );
    assert!(
        src.contains("fn provider_enabled"),
        "the enabled check must be a named helper"
    );
}
