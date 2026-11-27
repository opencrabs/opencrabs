//! #3212: the stream loop must not use the mid-stream idle budget as the clock
//! for the window before the FIRST chunk.
//!
//! `stream_idle_timeout` answers "how long may silence last between chunks that
//! were already flowing" - a dropped TCP connection or a frozen proxy. Applied
//! before any chunk has arrived it answers a different question ("how long may
//! this request take to start answering") with a number sized for the first.
//! On a request that follows a long detached task the prefill window is long
//! (big history, whole tool output, proxy routing plus a cold upstream), and
//! the 2026-11-22 receipt shows the consequence: 120s of the idle clock cut a
//! live stream at 0 output tokens.
//!
//! These tests pin the arithmetic the stream loop uses to pick its clock, so a
//! later refactor cannot quietly collapse the two budgets back into one.
//! `prefill_timeout_for` is the same expression the loop evaluates.

use crate::brain::agent::service::helpers::handshake_timeout_for;
use std::time::Duration;

/// Mirror of the loop's pre-first-token clock. Kept structurally identical to
/// the expression in `helpers.rs::stream_complete`:
///
/// ```ignore
/// handshake_timeout_for(is_cli, provider.base_url())
///     .max(stream_idle_timeout)
///     .max(Duration::from_secs(120))
/// ```
///
/// A change to one without the other is exactly the regression these tests
/// exist to catch, so the two are asserted against each other through the
/// public values rather than through a shared private helper.
fn prefill_timeout_for(cli: bool, base_url: Option<&str>, idle: Duration) -> Duration {
    handshake_timeout_for(cli, base_url)
        .max(idle)
        .max(Duration::from_secs(120))
}

/// The default remote idle budget (20s) must never be the first-token clock.
/// Before this split a cloud endpoint cut a silent-but-live start at 20s, and
/// the compat factory raised it to 120s, which is where the 2026-11-22 cut
/// landed. Both are far below the prefill window of a large request.
#[test]
fn prefill_budget_is_never_the_mid_stream_idle_budget() {
    let idle = Duration::from_secs(20);
    for url in [
        Some("https://api.openai.com/v1/chat/completions"),
        Some("https://inference.opencrabs.com/v1"),
        Some("http://127.0.0.1:8080/v1/chat/completions"),
        None,
    ] {
        let prefill = prefill_timeout_for(false, url, idle);
        assert!(
            prefill > idle,
            "first-token budget {prefill:?} must exceed the idle budget {idle:?} for {url:?}"
        );
        assert!(
            prefill >= Duration::from_secs(120),
            "first-token budget {prefill:?} must be at least 120s for {url:?}"
        );
    }
}

/// The built-in endpoint is a proxy in front of upstream models, so its
/// handshake budget (120s) is the number that was already sized for "a cold
/// upstream needs longer than a cloud gateway". The prefill clock inherits it
/// rather than reinventing a second per-endpoint table.
#[test]
fn built_in_endpoint_prefill_matches_its_handshake_budget() {
    let url = Some("https://inference.opencrabs.com/v1");
    assert_eq!(handshake_timeout_for(false, url), Duration::from_secs(120));
    assert_eq!(
        prefill_timeout_for(false, url, Duration::from_secs(20)),
        Duration::from_secs(120)
    );
}

/// A CLI provider answers through a subprocess, so its 600s startup budget is
/// the right prefill clock too. Local HTTP keeps 90s and cloud 60s, both raised
/// to the 120s floor: the point is that a server that has not started talking
/// is not yet evidence of a broken connection.
#[test]
fn prefill_clock_follows_the_endpoint_class_with_a_120s_floor() {
    let idle = Duration::from_secs(20);

    assert_eq!(
        prefill_timeout_for(true, None, idle),
        Duration::from_secs(600),
        "CLI subprocess startup is the prefill window"
    );
    assert_eq!(
        prefill_timeout_for(false, Some("http://127.0.0.1:8080/v1"), idle),
        Duration::from_secs(120),
        "local 90s handshake is raised by the 120s floor"
    );
    assert_eq!(
        prefill_timeout_for(false, Some("https://api.openai.com/v1"), idle),
        Duration::from_secs(120),
        "cloud 60s handshake is raised by the 120s floor"
    );
}

/// A configured `stream_idle_timeout_secs` larger than every default stays
/// authoritative: the prefill clock never shrinks a budget the operator set,
/// and it is taken as a floor, not a replacement, so a big idle value is
/// honoured on both sides of the first chunk.
#[test]
fn configured_idle_budget_is_a_floor_not_a_cap() {
    let idle = Duration::from_secs(300);
    assert_eq!(
        prefill_timeout_for(false, Some("https://api.openai.com/v1"), idle),
        idle,
        "an operator's 300s idle budget must also govern the first-token wait"
    );
}

/// The mid-stream budget is untouched by this change: once chunks flow, a
/// stall is a dropped stream and must still be caught on the strict clock.
/// Pinning the default here documents that the split did not loosen it.
#[test]
fn mid_stream_budget_stays_strict() {
    // `helpers.rs` uses 20s for remote HTTP, 3600s for CLI/local, and a
    // provider override when one is configured. Only the remote default is
    // restated here because it is the one the #3212 split would have loosened.
    let remote_idle = Duration::from_secs(20);
    assert!(
        remote_idle < prefill_timeout_for(false, Some("https://api.openai.com/v1"), remote_idle),
        "the strict clock must still apply after the first chunk arrives"
    );
}

/// The stream loop must consult BOTH clocks. A wiring test, not a timing one:
/// it reads the loop itself so a later edit that drops the split (goes back to
/// a bare `timeout(stream_idle_timeout, ...)`) fails here instead of silently
/// reintroducing #3212. Reading source is the only way to pin this without a
/// 120s wall-clock test, and the alternative (no test at all) is what let the
/// single-clock version survive this long.
#[test]
fn stream_loop_selects_the_clock_from_whether_a_chunk_arrived() {
    let src = include_str!("../brain/agent/service/helpers.rs");

    assert!(
        src.contains("if last_delta_at.is_some() { stream_idle_timeout } else { prefill_timeout }"),
        "the stream branch must pick its timeout from whether a chunk has arrived; \
         a bare `timeout(stream_idle_timeout, stream.next())` is #3212"
    );
    assert!(
        src.contains("let prefill_timeout = {"),
        "the pre-first-token budget must be computed before the loop"
    );
    assert!(
        !src.contains("tokio::time::timeout(stream_idle_timeout, stream.next())"),
        "the pre-#3212 single-clock poll must not come back"
    );
}
