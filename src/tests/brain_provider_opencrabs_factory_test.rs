use crate::brain::provider::factory::create_provider_by_name;
use crate::config::types::io::{KeysFile, merge_provider_keys};
use crate::config::{Config, ProviderConfig, ProviderConfigs};

fn opencrabs_config(api_key: Option<&str>) -> Config {
    let providers = ProviderConfigs {
        opencrabs: Some(ProviderConfig {
            enabled: true,
            api_key: api_key.map(str::to_string),
            ..Default::default()
        }),
        ..Default::default()
    };
    Config {
        providers,
        ..Default::default()
    }
}

#[tokio::test]
async fn by_name_opencrabs_with_install_token() {
    let config = opencrabs_config(Some("tok-install-123"));
    let provider = create_provider_by_name(&config, "opencrabs")
        .await
        .expect("opencrabs should create with an install token");
    assert_eq!(provider.name(), "opencrabs");
}

#[tokio::test]
async fn by_name_opencrabs_without_token_is_not_created() {
    let config = opencrabs_config(None);
    let result = create_provider_by_name(&config, "opencrabs").await;
    assert!(
        result.is_err(),
        "without an install token the provider must not be created"
    );
}

#[test]
fn keys_merge_copies_the_install_token_into_the_provider() {
    let keys = ProviderConfigs {
        opencrabs: Some(ProviderConfig {
            api_key: Some("tok-install-123".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let merged = merge_provider_keys(ProviderConfigs::default(), keys);
    let entry = merged.opencrabs.expect("opencrabs entry created");
    assert_eq!(entry.api_key.as_deref(), Some("tok-install-123"));
}

#[test]
fn keys_file_with_install_id_still_parses() {
    // The store keeps install_id beside api_key. It is not a ProviderConfig
    // field, so it must be ignored rather than break keys.toml parsing.
    let raw = r#"
[providers.opencrabs]
install_id = "abcd1234abcd1234abcd1234abcd1234"
api_key = "tok-install-123"
"#;
    let file: KeysFile = toml::from_str(raw).expect("keys.toml with install_id parses");
    let section = file.providers.opencrabs.expect("section present");
    assert_eq!(section.api_key.as_deref(), Some("tok-install-123"));
}

/// The built-in endpoint is free and fixed at 200k, so a `context_window` in
/// the section is inert: it can neither raise the budget above the window the
/// endpoint really serves nor lower it below what it offers (#2066).
#[tokio::test]
async fn built_in_context_window_is_pinned_to_200k() {
    for configured in [None, Some(2_000_000), Some(64_000)] {
        let mut config = opencrabs_config(Some("tok-install-123"));
        config
            .providers
            .opencrabs
            .as_mut()
            .expect("section present")
            .context_window = configured;
        let provider = create_provider_by_name(&config, "opencrabs")
            .await
            .expect("opencrabs should create with an install token");
        assert_eq!(
            provider.configured_context_window(),
            Some(200_000),
            "context_window = {configured:?} must not move the built-in window"
        );
    }
}

/// A custom section pointed at the built-in host reaches the same free
/// endpoint, so the fixed window has to hold there too. The section is named
/// `proxycopy` on purpose: no provider-name rule covers that name, so only the
/// host check can be doing the work (#2067).
#[tokio::test]
async fn custom_section_on_the_built_in_host_is_capped_to_200k() {
    let config = custom_config("proxycopy", "https://inference.opencrabs.com/v1", 2_000_000);
    let provider = create_provider_by_name(&config, "custom.proxycopy")
        .await
        .expect("custom section on the built-in host should build");
    assert_eq!(provider.configured_context_window(), Some(200_000));
}

/// The cap is scoped to the built-in host: every other endpoint keeps the
/// value the user set.
#[tokio::test]
async fn custom_section_on_another_host_keeps_its_context_window() {
    let config = custom_config("otherhost", "https://api.example.com/v1", 2_000_000);
    let provider = create_provider_by_name(&config, "custom.otherhost")
        .await
        .expect("custom section on another host should build");
    assert_eq!(provider.configured_context_window(), Some(2_000_000));
}

fn custom_config(name: &str, base_url: &str, context_window: u32) -> Config {
    let mut custom = std::collections::BTreeMap::new();
    custom.insert(
        name.to_string(),
        ProviderConfig {
            enabled: true,
            base_url: Some(base_url.to_string()),
            context_window: Some(context_window),
            ..Default::default()
        },
    );
    let providers = ProviderConfigs {
        custom: Some(custom),
        ..Default::default()
    };
    Config {
        providers,
        ..Default::default()
    }
}
