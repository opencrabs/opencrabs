//! Regression: custom-provider live `/v1/models` fetch must survive the
//! provider/model wizard consolidation.
//!
//! Bug context: OpenCrabs used to ship a standalone model-picker dialog
//! (`ModelSelector`) alongside the onboarding wizard. When `/models` was
//! consolidated into `onboard:provider` and the standalone dialog was
//! deleted, the live fetch for custom providers broke in two ways:
//!
//!   1. The `WizardAction::FetchModels` handler in `dialogs.rs` called
//!      `fetch_provider_models(idx, key, zhipu, None)` — hardcoding `None`
//!      for the `base_url` argument. For a custom provider the function
//!      resolves `provider_id` to "" and falls through to the custom
//!      branch, which needs a non-empty `base_url` to hit
//!      `<base_url>/v1/models`. With `None` it always returned an empty
//!      list. The deleted `ModelSelector` had passed `Some(&base_url)`.
//!
//!   2. Clearing the model name on an EXISTING custom provider no longer
//!      refetched. The old dialog refetched on Enter-over-empty-model;
//!      the new `AuthField::CustomModel` free-text handler just advanced
//!      to the next field.
//!
//! The fetch itself makes a network call, so these pin the pure gate
//! (`supports_model_fetch`) plus source-level sentinels for the two call
//! sites — the same approach as `onboarding_custom_model_input_test.rs`.

use crate::tui::provider_selector::{CUSTOM_PROVIDER_IDX, ProviderSelectorState};

fn custom_state() -> ProviderSelectorState {
    ProviderSelectorState {
        selected_provider: CUSTOM_PROVIDER_IDX,
        custom_names: Vec::new(),
        has_existing_key: false,
        api_key_input: String::new(),
        api_key_cursor: 0,
        models: Vec::new(),
        config_models: Vec::new(),
        selected_model: 0,
        model_filter: String::new(),
        models_fetching: false,
        zhipu_endpoint_type: 0,
        xiaomi_endpoint_type: 0,
        moonshot_endpoint_type: 0,
        opencode_endpoint_type: 0,
        moonshot_plan: 0,
        base_url: String::new(),
        custom_model: String::new(),
        custom_name: String::new(),
        editing_custom_key: None,
        context_window: String::new(),
        focused_field: 0,
        showing_providers: false,
        codex_user_code: None,
        codex_device_flow_status: crate::tui::onboarding::CodexDeviceFlowStatus::Idle,
    }
}

#[test]
fn custom_provider_with_base_url_supports_fetch() {
    // The gate for the clear-to-refetch UX: a custom provider that has a
    // base_url set must report it supports live fetching.
    let mut s = custom_state();
    s.base_url = "https://api.example.com".to_string();
    assert!(
        s.supports_model_fetch(),
        "custom provider with a base_url must support live /v1/models fetch"
    );
}

#[test]
fn custom_provider_without_base_url_does_not_support_fetch() {
    // No base_url means there's nowhere to fetch from — the gate must be
    // false so the Enter handler advances instead of looping on an empty
    // fetch.
    let s = custom_state();
    assert!(
        !s.supports_model_fetch(),
        "custom provider with no base_url must NOT claim fetch support"
    );
}

#[test]
fn custom_provider_blank_base_url_does_not_support_fetch() {
    // Whitespace-only base_url is treated as unset.
    let mut s = custom_state();
    s.base_url = "   ".to_string();
    assert!(
        !s.supports_model_fetch(),
        "whitespace-only base_url must be treated as unset"
    );
}

// ── Source-level sentinels for the two call sites ──

const DIALOGS_SRC: &str = include_str!("../tui/app/dialogs.rs");
const INPUT_SRC: &str = include_str!("../tui/onboarding/input.rs");
const REQUEST_SRC: &str = include_str!("../ui_core/model_selector/state.rs");
/// The fetcher lives in its own module now; `mod.rs` only re-exports it.
const FETCH_SRC: &str = include_str!("../ui_core/model_selector/fetch.rs");

#[test]
fn fetch_models_handler_forwards_base_url_not_none() {
    // The handler snapshots the wizard before spawning. The shared fetcher
    // must forward that snapshot's base_url instead of discarding it.
    assert!(
        DIALOGS_SRC.contains("FetchRequest::from_selector(&wizard.ps)"),
        "WizardAction::FetchModels must snapshot the wizard's provider state"
    );
    assert!(
        REQUEST_SRC.contains(
            "base_url: (!selector.base_url.trim().is_empty()).then(|| selector.base_url.clone())"
        ),
        "the request must preserve the unsaved custom provider base_url"
    );
    assert!(
        FETCH_SRC.contains("request.base_url.as_deref()"),
        "the shared fetcher must forward base_url.as_deref() so custom providers \
         reach <base_url>/v1/models"
    );
}

#[test]
fn clearing_custom_model_refetches_on_enter() {
    // Regression #2: Enter over an empty custom_model on a fetch-capable
    // provider must return FetchModels, restoring the old ModelSelector
    // clear-to-refetch behaviour instead of just advancing the field.
    assert!(
        INPUT_SRC.contains("self.ps.custom_model.trim().is_empty()")
            && INPUT_SRC.contains("self.ps.supports_model_fetch()"),
        "the CustomModel Enter handler must guard on empty model + \
         supports_model_fetch before returning FetchModels"
    );
}
