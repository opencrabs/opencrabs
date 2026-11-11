//! Pin the one-model skip in the `/models` wizard (#2782).
//!
//! A provider that offers exactly one model has no decision on the model
//! screen, so the screen is skipped and the model committed. The skip must
//! never fire for a provider that still owes an API key: advancing there
//! would commit a provider with no credentials and no way back to the field,
//! and it must never fire for a typed filter, which is the user searching a
//! list they can already see.

use crate::tui::onboarding::{
    AuthField, EXISTING_KEY_SENTINEL, OnboardingStep, OnboardingWizard, WizardAction,
};
use crate::ui_core::model_selector::{CUSTOM_PROVIDER_IDX, index_of_provider};

#[allow(clippy::field_reassign_with_default)]
fn wizard_on_model_step(provider: &str, models: &[&str], key_present: bool) -> OnboardingWizard {
    let mut w = OnboardingWizard::default();
    // quick_jump is how `/models` opens the wizard. It also keeps `next_step`
    // out of the config writer: the step does not change, so nothing is saved.
    w.quick_jump = true;
    w.step = OnboardingStep::ProviderAuth;
    w.auth_field = AuthField::Model;
    w.ps.selected_provider = index_of_provider(provider).expect("provider is in the catalogue");
    w.ps.models = models.iter().map(|m| m.to_string()).collect();
    w.ps.selected_model = 0;
    w.ps.api_key_input = if key_present {
        EXISTING_KEY_SENTINEL.to_string()
    } else {
        String::new()
    };
    w
}

// ─── The decision ─────────────────────────────────────────────────────

#[test]
fn the_built_in_provider_offers_exactly_one_model() {
    let mut w = OnboardingWizard::default();
    w.ps.selected_provider = index_of_provider("opencrabs").expect("built-in provider exists");
    assert_eq!(
        w.ps.sole_model_choice().as_deref(),
        Some("deepseek-v4.1-flash"),
        "`/models opencrabs` has one model to land on, so the picker must not ask"
    );
}

#[test]
fn several_models_keep_the_screen() {
    let w = wizard_on_model_step("anthropic", &["a", "b"], true);
    assert_eq!(w.ps.sole_model_choice(), None);
}

#[test]
fn zero_models_keep_the_screen() {
    let w = wizard_on_model_step("anthropic", &[], true);
    assert_eq!(
        w.ps.sole_model_choice(),
        None,
        "an empty catalogue has nothing to commit, so the screen must stay"
    );
}

#[test]
fn a_provider_that_owes_a_key_keeps_the_screen() {
    let w = wizard_on_model_step("anthropic", &["only-model"], false);
    assert_eq!(
        w.ps.sole_model_choice(),
        None,
        "skipping here would commit a provider with no credentials"
    );
}

#[test]
fn a_typed_filter_keeps_the_screen() {
    let mut w = wizard_on_model_step("anthropic", &["only-model"], true);
    w.ps.model_filter = "only".to_string();
    assert_eq!(
        w.ps.sole_model_choice(),
        None,
        "a typed filter is the user narrowing a list, not a screen to skip"
    );
}

#[test]
fn a_custom_provider_keeps_its_model_field() {
    let mut w = wizard_on_model_step("anthropic", &["only-model"], true);
    w.ps.selected_provider = CUSTOM_PROVIDER_IDX;
    assert_eq!(
        w.ps.sole_model_choice(),
        None,
        "custom providers type their model name, there is no catalogue to trust"
    );
}

// ─── The advance ──────────────────────────────────────────────────────

#[test]
fn one_model_advances_past_the_screen() {
    let mut w = wizard_on_model_step("anthropic", &["only-model"], true);
    assert_eq!(w.advance_past_sole_model(), Some(WizardAction::None));
    assert!(
        w.quick_jump_done,
        "the wizard must leave the model step on its own"
    );
}

#[test]
fn no_advance_when_the_provider_owes_a_key() {
    let mut w = wizard_on_model_step("anthropic", &["only-model"], false);
    assert_eq!(w.advance_past_sole_model(), None);
    assert!(!w.quick_jump_done, "the wizard must stay on the model step");
    assert_eq!(
        w.auth_field,
        AuthField::Model,
        "the field must not move while the key is missing"
    );
}

#[test]
fn no_advance_when_several_models_are_offered() {
    let mut w = wizard_on_model_step("anthropic", &["a", "b"], true);
    assert_eq!(w.advance_past_sole_model(), None);
    assert!(!w.quick_jump_done);
}

#[test]
fn no_advance_off_the_model_step() {
    let mut w = wizard_on_model_step("anthropic", &["only-model"], true);
    w.auth_field = AuthField::ApiKey;
    assert_eq!(
        w.advance_past_sole_model(),
        None,
        "only the model step is skipped; every other field stays put"
    );
    assert!(!w.quick_jump_done);
}
