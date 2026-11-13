//! #2285: the compaction summary written to the shared daily memory file.
//!
//! The continuation document is prompted to open with an imperative directive
//! ("## 0. IMMEDIATE TASK", "Do NOT deviate"), and the daily file is read by
//! every session in the profile. Writing it verbatim made one session's task
//! look like another session's instruction. These pin the three guarantees:
//! only durable sections survive, one entry is capped, and a second compaction
//! in the same session replaces its earlier entry instead of stacking.

use crate::brain::agent::service::context::{
    cap_entry_body, compaction_memory_entry, reinjectable_summary, replace_session_entry,
    strip_session_local_sections,
};
use uuid::Uuid;

const SUMMARY: &str = "\
# Compaction continuation document

## 0. IMMEDIATE TASK (CRITICAL)
**Obligation status: OPEN**
CONTINUE THIS TASK: finish the spreadsheet. Do NOT deviate to another topic.

## 1. Chronological Analysis
- the user asked for an audit

## 2. Files Modified
- src/foo.rs

## 3. User Preferences & Constraints
- terse, lowercase

## 4. Errors & Corrections
- E0027 fixed by dropping the pattern fields

## 5. All User Messages
1. do the thing

## 6. Pending Tasks
- task 11

## 7. Recovery Playbook
1. read the gate log

## 8. Next Step
read the gate log and report

## 9. Continuation Message
carry on

## PLAN STATE (harness)
[PLAN MODE - injected by the harness, not from the user]
";

#[test]
fn only_the_durable_sections_survive_the_strip() {
    let out = strip_session_local_sections(SUMMARY);

    for kept in [
        "## 1. Chronological Analysis",
        "## 2. Files Modified",
        "## 3. User Preferences & Constraints",
        "## 4. Errors & Corrections",
        "## 5. All User Messages",
    ] {
        assert!(out.contains(kept), "durable section dropped: {kept}");
    }

    for dropped in [
        "IMMEDIATE TASK",
        "Obligation status",
        "CONTINUE THIS TASK",
        "## 6. Pending Tasks",
        "## 7. Recovery Playbook",
        "## 8. Next Step",
        "## 9. Continuation Message",
        "PLAN STATE",
    ] {
        assert!(
            !out.contains(dropped),
            "session-local text reached the daily file: {dropped}"
        );
    }
}

#[test]
fn a_heading_inside_a_fence_is_content_not_a_section() {
    let summary = "\
## 1. Chronological Analysis
The summary template looks like this:

```
## 0. IMMEDIATE TASK
**Obligation status: OPEN**
```

## 8. Next Step
resume
";
    let out = strip_session_local_sections(summary);

    assert!(
        out.contains("## 0. IMMEDIATE TASK"),
        "a heading inside a fence is quoted content and must survive"
    );
    assert!(
        !out.contains("## 8. Next Step"),
        "the real Next Step section still goes"
    );
}

#[test]
fn the_context_manifest_block_goes_with_its_fence() {
    let summary = "\
## 1. Chronological Analysis
- did the work

```context-manifest
active_skills:
- opencrabs-dev
required_tools:
- gh
```

## 2. Files Modified
- src/foo.rs
";
    let out = strip_session_local_sections(summary);

    assert!(!out.contains("active_skills"), "manifest body survived");
    assert!(
        !out.contains("```context-manifest"),
        "manifest fence survived"
    );
    assert!(out.contains("## 2. Files Modified"));
}

#[test]
fn one_entry_is_capped_and_says_it_was_cut() {
    let huge = "a".repeat(20_000);
    let entry = compaction_memory_entry(Uuid::nil(), "2026-11-13 09:00:00", &huge);

    assert!(
        entry.len() <= 8 * 1024,
        "entry exceeded the cap: {} bytes",
        entry.len()
    );
    assert!(
        entry.contains("[truncated at the daily-memory entry cap]"),
        "a cut entry must say so"
    );
}

#[test]
fn the_cap_lands_on_a_character_boundary() {
    // Two bytes per character, so a byte budget that ignores boundaries would
    // panic or produce invalid UTF-8 here.
    let body = "é".repeat(10_000);
    let capped = cap_entry_body(&body, 4_001);

    assert!(
        capped.len() <= 4_001,
        "capped body is {} bytes",
        capped.len()
    );
    assert!(capped.ends_with("[truncated at the daily-memory entry cap]"));
}

#[test]
fn a_second_compaction_replaces_the_sessions_earlier_entry() {
    let session = Uuid::nil();
    let first = compaction_memory_entry(session, "2026-11-13 08:00:00", SUMMARY);
    let second = compaction_memory_entry(session, "2026-11-13 09:30:00", SUMMARY);

    let file = replace_session_entry("", session, &first);
    let file = replace_session_entry(&file, session, &second);

    assert_eq!(
        file.matches("## Auto-Compaction Summary").count(),
        1,
        "one session stacked two entries into the same day file"
    );
    assert!(file.contains("09:30:00"), "the newer entry must win");
    assert!(!file.contains("08:00:00"), "the older entry must be gone");
}

#[test]
fn another_session_gets_its_own_entry() {
    let a = Uuid::nil();
    let b = Uuid::from_u128(1);
    let first = compaction_memory_entry(a, "2026-11-13 08:00:00", SUMMARY);
    let second = compaction_memory_entry(b, "2026-11-13 08:10:00", SUMMARY);

    let file = replace_session_entry("", a, &first);
    let file = replace_session_entry(&file, b, &second);

    assert_eq!(
        file.matches("## Auto-Compaction Summary").count(),
        2,
        "two sessions must keep one entry each"
    );
}

#[test]
fn an_entry_with_no_markers_is_left_alone() {
    // Files written before this change carry bare entries. Appending must not
    // rewrite or drop them; they age out with the day file.
    let session = Uuid::nil();
    let legacy = "# 2026-11-13\n\n## Auto-Compaction Summary (2026-11-13 07:00:00)\n\nold body";
    let entry = compaction_memory_entry(session, "2026-11-13 09:00:00", SUMMARY);

    let file = replace_session_entry(legacy, session, &entry);

    assert!(file.contains("old body"), "legacy entry was dropped");
    assert_eq!(file.matches("## Auto-Compaction Summary").count(), 2);
}

// #2883: the same document, on its way back into CONTEXT rather than into the
// daily file. A stored summary is replayed verbatim on every later turn, so its
// execution sections were still reading as live orders days after the work had
// finished. The reporter's session spent a turn re-doing a job from 9 Nov.

const BANNER: &str = "[CONTEXT COMPACTION — The conversation was automatically compacted. \
                      Below is a structured summary of everything before this point.]";

fn stored_summary() -> String {
    format!("{BANNER}\n\n{SUMMARY}")
}

fn then_and_now() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let then = chrono::DateTime::from_timestamp(1_760_000_000, 0).expect("valid timestamp");
    let now = then + chrono::Duration::days(4);
    (then, now)
}

#[test]
fn a_replayed_summary_drops_the_directive_it_was_written_with() {
    let (then, now) = then_and_now();
    let out = reinjectable_summary(&stored_summary(), then, now);

    for directive in ["IMMEDIATE TASK", "CONTINUE THIS TASK", "PLAN STATE"] {
        assert!(
            !out.contains(directive),
            "a stored order survived re-injection: {directive}"
        );
    }
    assert!(
        out.contains("## 1. Chronological Analysis"),
        "durable half lost"
    );
}

#[test]
fn the_replayed_summary_keeps_its_boundary_banner() {
    // `messages_from_last_compaction` anchors the reload on this prefix. A
    // rewrite that ate it would delete the boundary along with the directive.
    let (then, now) = then_and_now();
    let out = reinjectable_summary(&stored_summary(), then, now);

    assert!(out.starts_with("[CONTEXT COMPACTION"), "banner lost: {out}");
}

#[test]
fn the_replayed_summary_says_how_old_it_is() {
    let (then, now) = then_and_now();
    let out = reinjectable_summary(&stored_summary(), then, now);

    assert!(out.contains("summary written"), "no age stamp: {out}");
    assert!(out.contains("4 days ago"), "age not stated in words: {out}");
}

#[test]
fn a_message_that_is_not_a_summary_is_left_alone() {
    let (then, now) = then_and_now();
    let plain = "## 0. IMMEDIATE TASK\nnot a compaction marker at all";

    assert_eq!(reinjectable_summary(plain, then, now), plain);
}
