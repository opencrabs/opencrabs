//! A `/models` provider tile must switch on the FIRST tap when that provider
//! has exactly one model — there is nothing to pick.
//!
//! v0.5.15 always answered `data=provider:<name>` with a second screen listing
//! that provider's models, and the switch only happened on the second tap
//! (`data=model:<provider>|<model>`). For a provider with one model that
//! screen held a single button already marked ✓, because it was the active
//! model — so the tap looked dead. The built-in `opencrabs` provider has
//! exactly one model, which is how a user found this.
//!
//! The fix routes one-model providers straight to the model-switch path.
//! [`single_model_choice`] is the single decision point, pinned here: one
//! model means the tile is the choice, zero or several means the list is a
//! real choice and must still be rendered.

use crate::channels::commands::single_model_choice;

#[test]
fn one_model_tile_is_the_choice() {
    let models = vec!["deepseek-v4.1-flash".to_string()];
    assert_eq!(
        single_model_choice(&models),
        Some("deepseek-v4.1-flash"),
        "a one-model provider must switch on the tile tap"
    );
}

#[test]
fn empty_list_has_no_choice_to_make() {
    // Callers render the "no models available" answer for this case; the
    // helper must not claim a model out of nothing.
    let models: Vec<String> = Vec::new();
    assert_eq!(single_model_choice(&models), None);
}

#[test]
fn multi_model_provider_still_renders_the_list() {
    let models = vec![
        "deepseek-v4.1-flash".to_string(),
        "deepseek-v4.1-pro".to_string(),
    ];
    assert_eq!(
        single_model_choice(&models),
        None,
        "several models are a real choice — the second screen must stay"
    );
}

#[test]
fn order_does_not_change_the_answer() {
    // The helper inspects the list length, not the active model: the current
    // model is inserted at index 0 by `models_for_provider`, so a one-model
    // list is one entry whichever position it came from.
    let models = vec!["only-model".to_string()];
    assert_eq!(single_model_choice(&models), Some("only-model"));
}
