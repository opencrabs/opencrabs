//! Internal system triggers: the seed prompts the harness sends to itself.
//!
//! A plan approval, a compaction request and a restart recovery all hand the
//! model a synthetic user message that starts with a bracketed marker. They
//! are context, never conversation: the user did not type them, and showing
//! one paints a wall of instructions where a one-line receipt belongs.
//!
//! Two spellings exist in the tree: `[SYSTEM:` for the plan and compaction
//! seeds, `[System:` for restart recovery and hot-reload handoffs. Both mean
//! the same thing, and a check that matches one of them silently leaks the
//! other. That is exactly what happened (#2756): the persistence check matched
//! the lowercase spelling only while every plan seed is uppercase, so 145
//! approval prompts sat in the chat history as user rows.
//!
//! One function, one rule, used by every site that classifies a message.

/// The two bracketed prefixes every synthetic seed is authored with. Lowercase
/// `[system:` is not a trigger: no code writes it, and a message someone typed
/// that way is theirs.
const INTERNAL_PREFIXES: [&str; 2] = ["[System:", "[SYSTEM:"];

/// Whether this text is a synthetic seed the harness sent to itself rather
/// than a message the user typed.
///
/// Used in three places, which are the three layers a seed can leak through:
/// the persistence check in the tool loop, the window's transcript builder,
/// and the terminal's send-time and reload paths.
pub fn is_internal_system_message(text: &str) -> bool {
    INTERNAL_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::is_internal_system_message;

    #[test]
    fn both_authored_spellings_are_internal() {
        // #2756: the plan and compaction seeds are uppercase, restart
        // recovery is title case. A check that knows one spelling leaks the
        // other into the chat history.
        assert!(is_internal_system_message(
            "[SYSTEM: PLAN APPROVED] The user approved the checklist."
        ));
        assert!(is_internal_system_message(
            "[System: You just rebuilt yourself from source]"
        ));
    }

    #[test]
    fn a_typed_message_is_not_internal() {
        assert!(!is_internal_system_message("Hello, how are you?"));
        assert!(!is_internal_system_message(""));
        // A lowercase bracket is not a spelling anything writes; it is the
        // user's own text and stays theirs.
        assert!(!is_internal_system_message("[system: something I typed]"));
        assert!(!is_internal_system_message("[Syste"));
        // The compaction summary is a different marker, handled by its own
        // filter, and must not be swept up by this one.
        assert!(!is_internal_system_message("[CONTEXT COMPACTION"));
    }

    #[test]
    fn the_prefix_must_be_at_the_start() {
        assert!(!is_internal_system_message(
            "note to self [SYSTEM: PLAN APPROVED]"
        ));
    }
}
