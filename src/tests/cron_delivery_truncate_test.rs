//! Regression tests for cron delivery payload sizing.
//!
//! #2252: delivery cut every report at 4000 bytes, so a 5150-byte report lost
//! its conclusion, marker included, even though the Telegram outbox chunks at
//! 4096 and Discord and Slack chunk at 2000 and 3500. The per-channel chunkers
//! own the real limits; `prepare_for_delivery` only enforces a runaway guard.
//!
//! #1919 stays pinned here: the guard's cut must land on a UTF-8 char boundary.
//! The original panic was `end byte index 4000 is not a char boundary; it is
//! inside '✅' (bytes 3999..4002)`, from the `HeyIolo Morning Recap` job.

use crate::cron::scheduler::prepare_for_delivery;

const GUARD: usize = 100_000;
const MARKER: &str = "...\n\n(truncated at 100000 bytes, full output in session)";

/// The live receipt from #2252: 5150 chars of report, 4082 chars delivered.
#[test]
fn report_over_the_old_limit_is_delivered_whole() {
    let mut s = String::from("## Verdict\n\n");
    while s.len() < 5150 {
        s.push_str("ping 192.168.0.1 timed out again\n");
    }
    assert!(
        s.len() > 4000,
        "the old 4000-byte cut would have dropped this report's tail"
    );
    let out = prepare_for_delivery(&s);
    assert_eq!(out, s, "a 5 KB report must reach the chunker intact");
    assert!(
        !out.contains("truncated"),
        "no truncation marker on a report under the runaway guard"
    );
}

#[test]
fn emoji_straddling_the_guard_truncates_without_panic() {
    let mut s = "a".repeat(GUARD - 1);
    s.push('✅'); // bytes GUARD-1..GUARD+2: index GUARD falls mid-char
    s.push_str(&" tail".repeat(100));
    assert!(s.len() > GUARD);
    let out = prepare_for_delivery(&s);
    let kept = out.strip_suffix(MARKER).expect("truncation marker present");
    assert_eq!(
        kept.len(),
        GUARD - 1,
        "cut backs off to the char boundary, never half an emoji"
    );
    assert_eq!(kept, "a".repeat(GUARD - 1));
}

#[test]
fn content_at_or_under_the_guard_is_untouched() {
    let short = "morning recap ✅";
    assert_eq!(prepare_for_delivery(short), short);
    let exact = "c".repeat(GUARD);
    assert_eq!(prepare_for_delivery(&exact), exact);
}
