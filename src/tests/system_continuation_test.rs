//! #2756: the seed prompts the harness sends to itself must never reach the
//! transcript.
//!
//! This file used to assert `starts_with("[System:")` inline, which is the
//! defect rather than a test of it: it pinned ONE spelling while every plan
//! approval seed is written `[SYSTEM:` (uppercase), so 145 approval prompts
//! and 52 compaction requests sat in the chat history as user rows and the
//! suite stayed green. The rule now lives in
//! `crate::utils::is_internal_system_message`, and these tests drive it.

use crate::utils::is_internal_system_message;

#[test]
fn the_plan_seed_is_internal() {
    // The exact prompt `/execute` hands the model (`plan_mode.rs`). This is
    // the one that leaked: uppercase, and the old check only knew the title
    // case spelling.
    let seed = "[SYSTEM: PLAN APPROVED] The user approved the checklist. Start task 1.";
    assert!(is_internal_system_message(seed));
}

#[test]
fn the_restart_seed_is_internal() {
    let seed = "[System: You just rebuilt yourself from source and restarted]";
    assert!(is_internal_system_message(seed));
}

#[test]
fn the_compaction_seed_is_internal() {
    let seed = "[SYSTEM: Compact context now. Summarize this conversation for continuity.]";
    assert!(is_internal_system_message(seed));
}

#[test]
fn a_typed_message_is_not_internal() {
    assert!(!is_internal_system_message("Hello, how are you?"));
    assert!(!is_internal_system_message(""));
    // Someone who types the marker themselves wrote their own message.
    assert!(!is_internal_system_message("[system: something I typed]"));
    assert!(!is_internal_system_message("[Syste"));
    // A compaction SUMMARY is a different marker with its own filter; the
    // seed filter must not swallow the record of the summary.
    assert!(!is_internal_system_message("[CONTEXT COMPACTION"));
}

#[test]
fn the_marker_must_open_the_message() {
    assert!(!is_internal_system_message(
        "note to self [SYSTEM: PLAN APPROVED]"
    ));
}

/// The three layers a seed can leak through, pinned on the source that has to
/// use the one helper: the tool loop's persistence check, the window's
/// transcript builder, and the terminal's send path.
#[test]
fn every_layer_asks_the_shared_helper() {
    for (file, src) in [
        (
            "tool_loop.rs",
            include_str!("../brain/agent/service/tool_loop.rs"),
        ),
        ("transcript.rs", include_str!("../gui/transcript.rs")),
        ("messaging.rs", include_str!("../tui/app/messaging.rs")),
    ] {
        let shipped = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            shipped.contains("is_internal_system_message("),
            "{file} must classify seeds through the shared helper, not a local prefix (#2756)"
        );
        assert!(
            !shipped.contains("starts_with(\"[System:\")"),
            "{file} still carries a one-spelling prefix check, which is the bug (#2756)"
        );
    }
}
