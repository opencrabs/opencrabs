//! Sentinel tests for the typed-not-matched-filter → model-name escape
//! hatch on built-in providers.
//!
//! Bug context: built-in providers (`AuthField::Model`) only accepted
//! the model name via filter+pick on the suggestion list. The list is
//! either fetched live from `/v1/models` (Anthropic / OpenAI / etc.)
//! or hardcoded (MiniMax has no /models endpoint, so the wizard ships
//! a static list of MiniMax-M2.7 / M2.5 / M2.1). When the user typed
//! a model name that wasn't in the list (e.g. `MiniMax-M3` — a real
//! release the bundled wizard doesn't know about yet),
//! `selected_model_name()` fell back to "first item in the list" and
//! silently committed `MiniMax-M2.7` instead. The custom-provider
//! path at input.rs:818-823 already handled this branch correctly;
//! the built-in path didn't.
//!
//! Fix:
//!   1. `selected_model_name()` returns the typed filter text when
//!      the filter is non-empty and matches zero entries.
//!   2. The `AuthField::Model` Enter handler pushes the typed text
//!      into `ps.models` and points `selected_model` at it, mirroring
//!      the custom-provider populated-mode commit so the model name
//!      survives across subsequent wizard steps that clear the filter.
//!   3. The wizard render shows the typed text as a "press Enter to
//!      use" line when there's no match, so the feature is visible.
//!
//! These tests pin the helper-level behaviour. The Enter-handler
//! mutation and render text are exercised via include_str! source
//! sentinels because driving the full TUI input loop in a unit test
//! needs a real terminal — overkill for a single-branch fix.

use crate::tui::provider_selector::ProviderSelectorState;

fn state() -> ProviderSelectorState {
    ProviderSelectorState {
        selected_provider: 0,
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
fn empty_filter_falls_back_to_first_model() {
    // Default behaviour: no filter, no typed text → first suggestion
    // wins. This is the path users on a fresh provider take when they
    // accept the highlighted default.
    let mut s = state();
    s.models = vec!["MiniMax-M2.7".to_string(), "MiniMax-M2.5".to_string()];
    s.selected_model = 0;
    s.model_filter = String::new();
    assert_eq!(s.selected_model_name(), "MiniMax-M2.7");
}

#[test]
fn typed_filter_matching_a_suggestion_picks_filtered_index() {
    // Filter matches → pick from filtered list. The user typed enough
    // to narrow to one suggestion and Enter commits that one.
    let mut s = state();
    s.models = vec![
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.5".to_string(),
        "MiniMax-M2.1".to_string(),
    ];
    s.model_filter = "M2.5".to_string();
    s.selected_model = 0;
    assert_eq!(s.selected_model_name(), "MiniMax-M2.5");
}

#[test]
fn typed_filter_with_no_match_becomes_the_model_name() {
    // The #141-style symptom: user typed a model name that's not in
    // the suggestion list. Before the fix this fell back to "first
    // item" silently. After the fix the typed text IS the model name.
    let mut s = state();
    s.models = vec![
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.5".to_string(),
        "MiniMax-M2.1".to_string(),
    ];
    s.model_filter = "MiniMax-M3".to_string();
    s.selected_model = 0;
    assert_eq!(
        s.selected_model_name(),
        "MiniMax-M3",
        "typed text must become the model name when no suggestion matches"
    );
}

#[test]
fn typed_filter_is_trimmed_before_use() {
    // Leading / trailing whitespace from paste or sloppy typing
    // shouldn't end up in config.toml.
    let mut s = state();
    s.models = vec!["MiniMax-M2.7".to_string()];
    s.model_filter = "  MiniMax-M3  ".to_string();
    s.selected_model = 0;
    assert_eq!(s.selected_model_name(), "MiniMax-M3");
}

#[test]
fn whitespace_only_filter_falls_back_to_first_model() {
    // A filter that's just spaces is treated as empty — the user
    // didn't actually type anything meaningful, default behaviour.
    let mut s = state();
    s.models = vec!["MiniMax-M2.7".to_string()];
    s.model_filter = "   ".to_string();
    s.selected_model = 0;
    assert_eq!(s.selected_model_name(), "MiniMax-M2.7");
}

#[test]
fn empty_model_list_with_typed_filter_returns_typed() {
    // Edge case: provider hasn't returned a list yet (fetching) AND
    // user typed a name. The typed name should still survive.
    let mut s = state();
    s.models = Vec::new();
    s.model_filter = "MiniMax-M3".to_string();
    s.selected_model = 0;
    assert_eq!(s.selected_model_name(), "MiniMax-M3");
}

#[test]
fn empty_everything_returns_empty_string() {
    // Brand new state — no suggestions, no typed text. Return empty
    // string so the config write skips `default_model` (config.rs:397
    // only writes when non-empty).
    let s = state();
    assert_eq!(s.selected_model_name(), "");
}

// ── Source-level sentinels for the Enter handler + render ──

// The model-step Enter commit lives in navigation.rs since #2782 moved it
// onto the shared `commit_model_step` path; input.rs only routes Enter there.
const NAV_SRC: &str = include_str!("../tui/onboarding/navigation.rs");
const RENDER_SRC: &str = include_str!("../tui/onboarding_render.rs");

#[test]
fn auth_field_model_enter_commits_typed_text_into_models_list() {
    // The Enter handler must push the typed text into `ps.models`
    // and point `selected_model` at it, otherwise subsequent
    // wizard steps that clear the filter (input.rs:617, 762, 779)
    // would erase the typed name before `apply_config` reads it.
    let pattern = "if filtered_count == 0 && !filter.is_empty()";
    assert!(
        NAV_SRC.contains(pattern),
        "commit_model_step (navigation.rs) must check filtered_count == 0 \
         AND non-empty filter to trigger the typed-as-name commit"
    );
    assert!(
        NAV_SRC.contains("self.ps.models.push(filter.clone());"),
        "Enter handler must push the typed text into models so it \
         survives filter resets in later wizard steps"
    );
}

#[test]
fn render_shows_typed_text_as_custom_when_no_match() {
    // Without the visual hint the user sees "no models match" and
    // assumes they need a binary update. The "press Enter to use"
    // surface is what makes the typed-as-name feature discoverable.
    assert!(
        RENDER_SRC.contains("custom — press Enter to use"),
        "render must surface the typed text as 'press Enter to use' \
         when filter has zero matches — otherwise the typed-as-name \
         escape hatch is invisible"
    );
}
