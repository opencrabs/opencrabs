//! A bare provider name in `/models <provider>` must resolve to that
//! provider's only model, and must never guess when there is a choice (#2782).
//!
//! The provider tile already switches on the first tap for a one-model
//! provider (`single_model_choice`). The typed form did not: `/models
//! opencrabs` fell through to "names a provider but no model" and told the
//! user to qualify it, even though the answer was the single model that
//! provider serves. [`bare_provider_pair`] is that decision point, pinned
//! here so zero or several models keep the prompt instead of inventing one.

use crate::channels::commands::bare_provider_pair;

#[test]
fn bare_provider_with_one_model_resolves_it() {
    let offered = vec!["deepseek-v4.1-flash".to_string()];
    assert_eq!(
        bare_provider_pair("opencrabs", &offered),
        Ok(("opencrabs".to_string(), "deepseek-v4.1-flash".to_string())),
        "a provider with one model has nothing to choose"
    );
}

#[test]
fn bare_provider_with_no_models_keeps_the_prompt() {
    // No key, nothing configured, provider failed to load: there is no model
    // to switch to, so the user must still be told to qualify it.
    let offered: Vec<String> = Vec::new();
    let err = bare_provider_pair("opencrabs", &offered).unwrap_err();
    assert!(
        err.contains("names a provider but no model"),
        "unexpected error text: {err}"
    );
}

#[test]
fn bare_provider_with_several_models_keeps_the_prompt() {
    let offered = vec![
        "deepseek-v4.1-flash".to_string(),
        "deepseek-v4.1-pro".to_string(),
    ];
    let err = bare_provider_pair("opencrabs", &offered).unwrap_err();
    assert!(
        err.contains("names a provider but no model"),
        "several models are a real choice and must not be guessed: {err}"
    );
}
