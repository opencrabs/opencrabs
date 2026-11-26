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

/// The banner every compaction summary is authored with. The full-context
/// marker and the delta-segment continuation share the prefix, so one literal
/// here covers both and the render paths stop repeating it.
pub const COMPACTION_BANNER: &str = "[CONTEXT COMPACTION";

/// Split compaction scaffold out of a row that carries it EMBEDDED.
///
/// A summary row starts with the banner, so the render paths skip it whole.
/// The banner can also ride INSIDE a row the user did author: text copied out
/// of the transcript (a reply quoting the scaffold) and pasted into the
/// composer lands after the typed words. Enter queued it while the turn was
/// busy, and the tool-boundary drain persisted it as one continuation row, so
/// the bubble painted the whole summary after two typed sentences (2026-11-26,
/// session 00d3619d: a 1498-char transcript copy glued onto a 106-char
/// complaint, byte-exact against the copy receipt in opencrabs-gui.log).
///
/// Everything from the banner to the end of the row is scaffold. The summary
/// is injected as a whole message, so nothing user-authored ever follows it,
/// and the head is the user's own words. `None` when the row carries no
/// banner, so callers keep the text untouched.
pub fn strip_embedded_compaction(content: &str) -> Option<String> {
    // split_once, not a byte index: `find` plus `content[..idx]` is a
    // clippy::string_slice site, and the byte-slice ratchet in CI fails the
    // push on any new one (scripts/byte-slice-baseline.txt). split_once hands
    // back the head without an index, and it is the same first-match
    // semantics the find had.
    let (head, _) = content.split_once(COMPACTION_BANNER)?;
    let head = head.trim_end();
    Some(format!("{head}\n\n[compaction summary hidden]"))
}

#[cfg(test)]
mod embedded_compaction_tests {
    use super::{COMPACTION_BANNER, strip_embedded_compaction};

    #[test]
    fn an_embedded_banner_is_cut_with_its_tail() {
        // The incident shape: typed words, one space, then the whole pasted
        // summary. The words survive, the scaffold and anything after it do
        // not.
        let row = "what the actual fuck [CONTEXT COMPACTION — DELTA SEGMENT. More\nscaffold lines\nand more";
        let stripped = strip_embedded_compaction(row).expect("the banner is embedded");
        assert_eq!(
            stripped,
            "what the actual fuck\n\n[compaction summary hidden]"
        );
        assert!(!stripped.contains("scaffold lines"));
    }

    #[test]
    fn a_row_without_the_banner_is_untouched() {
        assert_eq!(strip_embedded_compaction("plain words"), None);
        assert_eq!(strip_embedded_compaction(""), None);
        // A near miss is not a banner: the user's text stays theirs.
        assert_eq!(
            strip_embedded_compaction("we were talking about compaction"),
            None
        );
    }

    #[test]
    fn the_banner_constant_matches_both_authored_markers() {
        // Full-context and delta-segment summaries share the prefix. If the
        // builder in brain::agent::context ever changes its wording, this test
        // is the tripwire.
        let full = "[CONTEXT COMPACTION — The conversation was automatically compacted";
        let delta = "[CONTEXT COMPACTION — DELTA SEGMENT. This block summarises";
        for marker in [full, delta] {
            assert!(marker.starts_with(COMPACTION_BANNER));
            assert!(strip_embedded_compaction(marker).is_some());
        }
    }
}
