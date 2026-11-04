//! `/models <provider/model>` in the TUI switches directly (#2064). A bare
//! `/models` still opens the picker, and the direct-switch arm must come before
//! the setup-dialog arm, which also claims `/models`.

use crate::cli::session_set_model::direct_switch_arg;

#[test]
fn a_pair_after_models_is_the_direct_switch_argument() {
    assert_eq!(
        direct_switch_arg("/models anthropic/claude-sonnet-4-20250514"),
        Some("anthropic/claude-sonnet-4-20250514")
    );
}

#[test]
fn the_all_scope_suffix_stays_with_the_argument() {
    assert_eq!(
        direct_switch_arg("/models openai/gpt-5 all"),
        Some("openai/gpt-5 all")
    );
}

#[test]
fn a_bare_models_opens_the_picker() {
    assert_eq!(direct_switch_arg("/models"), None);
    assert_eq!(direct_switch_arg("/models   "), None);
    assert_eq!(direct_switch_arg("  /models  "), None);
}

#[test]
fn other_commands_are_not_direct_switches() {
    assert_eq!(direct_switch_arg("/model openai/gpt-5"), None);
    assert_eq!(direct_switch_arg("/modelsx openai/gpt-5"), None);
    assert_eq!(direct_switch_arg("/channels whatsapp"), None);
}

#[test]
fn the_direct_switch_arm_precedes_the_setup_dialog_arm() {
    let src = include_str!("../tui/app/messaging.rs");
    let direct = src
        .find("direct_switch_arg(input).is_some()")
        .expect("direct-switch arm present");
    let setup = src
        .find("deep_link::is_setup_command(s)")
        .expect("setup-dialog arm present");
    assert!(
        direct < setup,
        "the direct switch must match before the setup dialog claims /models"
    );
}
