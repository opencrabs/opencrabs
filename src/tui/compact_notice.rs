//! What `/compact` tells the user, and when (#1375).
//!
//! The command dispatches a trigger to the agent and prints a note. The note
//! used to be printed BEFORE the send, and the send result was dropped: a
//! `/compact` that never left the TUI reported exactly like one that did.
//! It also read as a live indicator ("Compacting context... 62%") while
//! being frozen text from the moment the command was typed, disagreeing with
//! the footer within seconds. These two functions own the wording so the
//! call site only decides which one applies.

/// The note printed once the trigger is actually away. Says it is a
/// starting note, and names the footer as the live figure, because a
/// compaction can legitimately take minutes while a fallback chain walks.
pub fn requested(usage_pct: f64) -> String {
    format!(
        "Compaction requested at {:.0}% context. This line does not update: \
         the footer ctx counter is the live figure, and a slow provider or a \
         fallback chain can take minutes.",
        usage_pct
    )
}

/// The error shown when the trigger could not be dispatched. Never silent:
/// the user typed a command and must learn that nothing is running.
pub fn dispatch_failed(err: &dyn std::fmt::Display) -> String {
    format!("/compact was not dispatched, nothing is compacting: {err}")
}
