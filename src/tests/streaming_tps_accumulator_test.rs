//! Pin the live-tok/s accumulator behavior added 2026-05-28.
//!
//! Pre-fix: footer divided streaming_output_tokens by
//! `processing_started_at.elapsed()` (wall-clock). A 30-second tool exec
//! made a 200 tok/s burst decay to "8 tok/s" mid-turn — the rate was
//! counting idle.
//!
//! Post-fix: the accumulator only counts time during which token deltas
//! are actively arriving. Gaps longer than the tracker's
//! `IDLE_GAP_SECS` (1.0s) close the prior window and open a new one
//! when the next token arrives. The footer's `last_tps` keeps showing
//! the previous turn's finalized rate during idle until the next turn
//! produces its first token.
//!
//! Tests drive the tracker with synthetic `Instant`s instead of real
//! sleeps so they're deterministic and fast.

use crate::tui::app::StreamingTpsTracker;
use std::time::{Duration, Instant};

#[test]
fn fresh_tracker_has_zero_active_time() {
    let t = StreamingTpsTracker::default();
    let now = Instant::now();
    assert_eq!(t.active_secs_now(now), 0.0);
    assert_eq!(t.last_tps, None);
}

#[test]
fn first_advance_opens_window_with_zero_elapsed() {
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    assert_eq!(
        t.active_secs_now(t0),
        0.0,
        "fresh window with no prior token has zero active time"
    );
}

#[test]
fn consecutive_advances_within_threshold_extend_same_window() {
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + Duration::from_millis(50));
    let t2 = t0 + Duration::from_millis(100);
    t.advance(t2);
    // Three events spanning 100ms — single window, active = 100ms.
    assert!(
        (t.active_secs_now(t2) - 0.1).abs() < 1e-6,
        "expected 100ms active window, got {:.6}s",
        t.active_secs_now(t2)
    );
}

#[test]
fn gap_longer_than_threshold_closes_window_and_opens_new_one() {
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + Duration::from_millis(50)); // first window: 50ms active
    let after_gap = t0 + Duration::from_millis(50 + 1100); // > 1s idle
    t.advance(after_gap);

    // The 1100ms idle gap must NOT be counted. Total active should be
    // just the prior 50ms window (now closed) + 0ms of the new window.
    let total = t.active_secs_now(after_gap);
    assert!(
        (total - 0.05).abs() < 1e-6,
        "idle gap must be excluded; got total active {total:.6}s (would be ~1.15s if regression)"
    );
}

#[test]
fn three_windows_separated_by_gaps_each_count() {
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    // Window 1: 0-30ms active, 1.5s idle
    t.advance(t0);
    t.advance(t0 + Duration::from_millis(30));
    // Window 2: starts at 1530ms, 50ms active, 2s idle
    let w2_start = t0 + Duration::from_millis(1530);
    t.advance(w2_start);
    t.advance(w2_start + Duration::from_millis(50));
    // Window 3: starts at ~3580ms, 70ms active
    let w3_start = w2_start + Duration::from_millis(50 + 2000);
    t.advance(w3_start);
    let w3_end = w3_start + Duration::from_millis(70);
    t.advance(w3_end);

    let total = t.active_secs_now(w3_end);
    let expected = 0.030 + 0.050 + 0.070; // 150ms total active
    assert!(
        (total - expected).abs() < 1e-6,
        "three windows must sum: expected {expected:.6}s, got {total:.6}s"
    );
}

#[test]
fn finalize_stashes_rate_and_resets_accumulator() {
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    // 400ms window: above the MIN_ACTIVE_SECS floor, so the rate is credible.
    t.advance(t0 + Duration::from_millis(400));
    // None = use local estimate (the path tested here).
    t.finalize(100, None);
    let tps = t.last_tps.expect("finalize must stash a rate");
    assert!(
        (tps - 250.0).abs() < 1e-3,
        "100 tokens / 0.4s = 250 tok/s, got {tps:.3}"
    );
    // Reset state for next turn.
    assert_eq!(t.active_secs_now(t0 + Duration::from_millis(500)), 0.0);
}

#[test]
fn finalize_with_zero_tokens_does_not_clobber_previous_last_tps() {
    let mut t = StreamingTpsTracker::default();
    t.last_tps = Some(42.0);
    let t0 = Instant::now();
    t.advance(t0);
    t.finalize(0, None);
    assert_eq!(
        t.last_tps,
        Some(42.0),
        "empty turn must not clobber prior persisted rate"
    );
}

#[test]
fn finalize_with_zero_active_secs_does_not_clobber_previous_last_tps() {
    // Same-instant advance + finalize → 0 active secs → divide-by-zero
    // would be infinity. Must skip the update and keep prior rate.
    let mut t = StreamingTpsTracker::default();
    t.last_tps = Some(42.0);
    let t0 = Instant::now();
    t.advance(t0);
    t.finalize(50, None);
    assert_eq!(t.last_tps, Some(42.0));
}

#[test]
fn finalize_uses_authoritative_tps_when_provided() {
    // The whole point of the AgentResponse.tokens_per_second wire-up:
    // when the agent service computed an authoritative tok/s from
    // provider-reported output_tokens divided by summed active
    // streaming time, the TUI must display THAT number, not the
    // tiktoken-estimated local rate. Otherwise users see two
    // different numbers in the footer (local) vs the channel
    // footer (authoritative) for the same turn.
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + std::time::Duration::from_millis(100));
    // Local estimate would be 1000 tok/s (100 tokens / 0.1s).
    // Authoritative says 73 — use it.
    t.finalize(100, Some(73.0));
    assert_eq!(t.last_tps, Some(73.0));
}

#[test]
fn finalize_falls_back_to_local_when_authoritative_is_none() {
    // CLI providers and other non-streaming paths don't measure
    // active streaming time and report tokens_per_second=None. The
    // TUI must still show the local estimate in that case so the
    // footer doesn't go blank.
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + std::time::Duration::from_millis(400));
    t.finalize(100, None);
    let tps = t
        .last_tps
        .expect("local estimate must apply when authoritative is None");
    assert!((tps - 250.0).abs() < 1e-3);
}

#[test]
fn finalize_ignores_non_finite_authoritative() {
    // Guard against the agent service somehow emitting NaN / Infinity
    // (e.g. divide-by-zero on a degenerate turn). Fall back to local.
    let mut t = StreamingTpsTracker::default();
    t.last_tps = Some(42.0);
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + std::time::Duration::from_millis(400));
    t.finalize(100, Some(f64::NAN));
    // Should have fallen back to local (250 tok/s), not NaN.
    let tps = t
        .last_tps
        .expect("NaN authoritative must fall back to local, not blank");
    assert!(
        (tps - 250.0).abs() < 1e-3,
        "got {tps:?}, expected ~250 from local fallback"
    );
}

#[test]
fn idle_ticks_past_last_token_dont_inflate_in_flight_window() {
    // Regression guard: active_secs_now must not extend the open window
    // toward `now`. The active duration is measured between window_start
    // and the LAST token, never beyond.
    let mut t = StreamingTpsTracker::default();
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + Duration::from_millis(40));
    // Simulate 500ms of idle ticks (still within IDLE_GAP_SECS so the
    // window hasn't closed) — active time must stay at 40ms.
    let queried_at = t0 + Duration::from_millis(540);
    assert!(
        (t.active_secs_now(queried_at) - 0.040).abs() < 1e-6,
        "active time must equal last-window span, not extend to now"
    );
}

#[test]
fn finalize_refuses_a_window_too_short_to_be_a_rate() {
    // #2703: a burst-delivered response lands in one sub-second chunk, so the
    // active window is a few milliseconds and the computed rate is a number no
    // model produces (37203 tok/s was observed). The accumulator keeps the
    // previous rate rather than printing the artifact.
    let mut t = StreamingTpsTracker::default();
    t.last_tps = Some(42.0);
    let t0 = Instant::now();
    t.advance(t0);
    t.advance(t0 + Duration::from_millis(8));
    t.finalize(297, None);
    assert_eq!(
        t.last_tps,
        Some(42.0),
        "an 8ms window must not print a rate, however many tokens it claims"
    );
}
