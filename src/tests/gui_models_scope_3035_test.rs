//! #3035: the model picker carries a scope, and all-sessions asks first.
//!
//! The engine has taken a trailing ` all` since #468, and the chat channels
//! have wrapped it in a count-then-confirm since #2042. The window had
//! neither: the only way to reach scope-all was to type a suffix nothing
//! advertised, and that path wrote on one Enter with no count.
//!
//! These are source pins, the same shape as the layout-boot pins (#2689): the
//! wiring under test lives in the gpui dashboard, which has no headless
//! harness here, so the pins hold the route rather than the paint.

const MODELS: &str = include_str!("../gui/dashboard_models.rs");
const MODAL: &str = include_str!("../gui/models_modal.rs");
const TURNS: &str = include_str!("../gui/dashboard_turns.rs");

/// Enter in the picker, and a click on a single-model provider, both go
/// through the scope route. A direct `switch_model` from either would skip
/// the confirmation.
#[test]
fn every_picker_pick_routes_through_the_scope_route() {
    assert!(
        MODELS.contains("self.pick_model(pair, cx)"),
        "Enter must route through pick_model"
    );
    assert!(
        MODAL.contains("this.pick_model(pair, cx)"),
        "a provider click must route through pick_model too"
    );
    assert!(
        MODELS.contains("fn pick_model(&mut self, pair: String"),
        "pick_model is the one route a pick takes"
    );
}

/// All-sessions is counted before it is written: the number is what the
/// prompt names, and the prompt is the only place the write is approved.
#[test]
fn all_sessions_is_counted_before_anything_is_written() {
    assert!(
        MODELS.contains("agent_bridge::spawn_scope_count"),
        "the count runs off the UI thread"
    );
    assert!(
        TURNS.contains("TurnEvent::ModelScopeCounted"),
        "the count comes back as an event"
    );
    assert!(
        TURNS.contains("self.models_apply_confirm = Some((pair, count))"),
        "a count above zero waits for the second tap"
    );
    assert!(
        MODELS.contains("self.models_apply_confirm = None;\n                    self.switch_model"),
        "confirming clears the pending state before it writes"
    );
}

/// The suffix the engine understands is added at exactly one place: the
/// confirmation. Anywhere else and a bulk write could happen unannounced.
#[test]
fn the_all_suffix_is_added_only_after_the_confirm() {
    assert_eq!(
        MODELS.matches("format!(\"{pair} all\")").count(),
        1,
        "the suffix belongs to the confirmed path only"
    );
    assert!(
        MODELS.contains("agent_bridge::spawn_switch_model"),
        "the confirmed path still goes through the engine's switch"
    );
}

/// The prompt text is the channel helper, so the window and Telegram cannot
/// describe the same bulk write differently.
#[test]
fn the_prompt_is_the_shared_channel_text() {
    assert!(
        MODELS.contains("opencrabs::channels::commands::apply_all_prompt_text"),
        "the count prompt comes from the shared helper"
    );
}

/// The scope is visible without prior knowledge: a chip in the header's
/// action slot, spelled out in the subtitle, and a key that flips it.
#[test]
fn the_scope_is_on_screen_and_reachable() {
    assert!(
        MODAL.contains("\"models-scope\""),
        "the header carries a scope control"
    );
    assert!(
        MODAL.contains("actions: Some(scope_chip.into_any_element())"),
        "the control uses the slot the modal already reserves"
    );
    assert!(
        MODAL.contains("all sessions"),
        "the subtitle names the state"
    );
    assert!(
        MODELS.contains("\"tab\" if !onboarding =>"),
        "a keystroke flips the scope"
    );
}

/// Scope-all never survives a reopening: every opening starts on the session
/// the picker was opened for.
#[test]
fn opening_the_picker_resets_the_scope() {
    assert!(
        MODELS.contains("self.models_scope_all = false;"),
        "the picker opens on this-session scope"
    );
}

/// The typed path is not a side door (#3168): a trailing ` all` typed in the
/// composer is parsed before anything runs, and a scope-all argument counts
/// through the same route the picker takes. A plain pair keeps the bare
/// switch it has always had.
#[test]
fn the_typed_argument_routes_through_the_scope_route_too() {
    assert!(
        MODELS.contains("strip_suffix(\" all\")"),
        "the typed argument is parsed for the scope suffix before anything runs"
    );
    assert!(
        MODELS.contains("self.begin_scope_all(pair, cx)"),
        "a scope-all typed argument counts before it writes"
    );
    assert!(
        MODELS.matches("self.begin_scope_all").count() == 2,
        "the picker and the typed path are the only two callers"
    );
}

/// The ask cannot live in a footer the typed command never opened: when the
/// picker is not on screen, the count prompt lands in the conversation, and
/// it is the shared channel text, not a window-private sentence.
#[test]
fn the_typed_ask_lands_in_the_conversation() {
    assert!(
        TURNS.contains("self.screen != Screen::Models"),
        "the in-chat ask only fires when no footer can show it"
    );
    assert!(
        TURNS.contains("opencrabs::channels::commands::apply_all_prompt_text"),
        "the in-chat ask reads the same text the channels use"
    );
    assert!(
        TURNS.contains("enter apply to all  esc cancel"),
        "the ask names the keys that answer it"
    );
}
