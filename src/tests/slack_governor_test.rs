//! Slack outbound write governor (#2012) - the parity tests for
//! `channels/slack/governor.rs`.
//!
//! Same class of coverage as `discord_governor_test.rs` (#1910) with Slack's
//! shapes: the conversation key is a channel-id STRING, the 429 is
//! `SlackClientError::RateLimitError` carrying a parsed `Retry-After`
//! (slack-morphism 2.26), and the chrome writers are the step-group bubble and
//! its 4 s flow ticker rather than a Discord embed.
//!
//! Four of the tests drive the PURE `admit` / `note_429` functions: every
//! branch of the policy (drop, hold, charge, latest-wins park) is integer
//! arithmetic on `Instant`s, so an injected `now` proves it in one line with no
//! sleeping and no live socket. The remaining tests exercise the async
//! [`Governor::gate`] loop against the virtual clock in
//! `governor::test_support`, because that loop - sleep, re-ask, and the
//! park-expiry that ends it - is exactly where an async bug hides. The last
//! test pins the wiring: a Slack chat write that leaves the process without
//! paying the budget is the regression this issue is about.

use std::sync::Arc;
use std::time::Duration;

use slack_morphism::errors::SlackClientError;
use slack_morphism::errors::SlackRateLimitError;

use crate::channels::slack::governor::test_support;
use crate::channels::slack::governor::{
    ChannelBudget, Decision, Governor, Limits, Surface, admit, note_429,
};
use crate::config::Config;

/// Default-ish limits with round numbers, so the arithmetic below is readable
/// in the assertions instead of in a comment.
fn limits(ceiling_per_min: u32, burst: u32, chrome_ms: u64) -> Limits {
    Limits {
        enabled: true,
        ceiling_per_min,
        burst,
        chrome_spacing: Duration::from_millis(chrome_ms),
        content_max_hold: Duration::from_secs(60),
        pause_fallback: Duration::from_secs(5),
        pause_ceiling: Duration::from_secs(20),
    }
}

/// The claim #2012 is built on: a live re-render of state already on screen is
/// worth nothing once it is refused. Two clock paints inside the spacing window
/// is one too many, and the second one must be `Drop`, never `Hold` - holding
/// the ticker is how a cosmetic writer ends up queued behind everything.
#[test]
fn chrome_is_dropped_inside_the_spacing_window() {
    let l = limits(0, 0, 1_000);
    let now = std::time::Instant::now();
    let mut b = ChannelBudget::default();
    assert_eq!(
        admit(&mut b, &l, Surface::Chrome, Duration::ZERO, now),
        Decision::Admit,
        "the first paint of a fresh budget goes out"
    );
    assert_eq!(
        admit(
            &mut b,
            &l,
            Surface::Chrome,
            Duration::ZERO,
            now + Duration::from_millis(500)
        ),
        Decision::Drop,
        "a second paint inside the spacing window is a wasted edit: drop it, \
         the next tick restates the same group"
    );
    assert_eq!(
        admit(
            &mut b,
            &l,
            Surface::Chrome,
            Duration::ZERO,
            now + Duration::from_millis(1_500)
        ),
        Decision::Admit,
        "outside the window the clock paints again"
    );
    assert!(
        l.ceiling_per_min == 0 && b.tokens.is_infinite(),
        "with the ceiling off nothing is charged, which is why a spacing drop \
         cannot be blamed on the budget: Chrome yields on spacing alone"
    );
}

/// A content write and a chrome write hit the SAME parked budget and must get
/// OPPOSITE answers: content holds out the window, chrome drops. If chrome
/// ever learned to queue, the ticker's backlog would eat the budget the settle
/// edit needs - the exact starvation this layer exists to prevent.
#[test]
fn parked_conversation_holds_content_and_refuses_chrome() {
    let l = limits(0, 0, 0);
    let now = std::time::Instant::now();
    let mut b = ChannelBudget::default();
    let pause = note_429(&mut b, &l, Some(Duration::from_secs(2)), now);
    assert_eq!(pause, Duration::from_secs(2), "the park is armed");
    assert_eq!(
        admit(&mut b, &l, Surface::Final, Duration::ZERO, now),
        Decision::Hold(Duration::from_secs(2)),
        "content waits out the window Slack named"
    );
    assert_eq!(
        admit(&mut b, &l, Surface::Chrome, Duration::ZERO, now),
        Decision::Drop,
        "chrome has nothing useful to say into a parked conversation"
    );
    assert_eq!(
        admit(
            &mut b,
            &l,
            Surface::Send,
            Duration::ZERO,
            now + Duration::from_secs(3)
        ),
        Decision::Admit,
        "and when the park expires content goes: it waited, it did not lose \
         the payload"
    );
}

/// Chrome on a DRY budget drops too, and dropping charges nothing: the next
/// content write must still find the same (empty) bucket rather than a bucket
/// the discarded paint quietly drained.
#[test]
fn chrome_write_dropped_on_a_dry_budget_uncharged() {
    let l = limits(60, 2, 0);
    let now = std::time::Instant::now();
    let mut b = ChannelBudget::default();
    assert_eq!(
        admit(&mut b, &l, Surface::Send, Duration::ZERO, now),
        Decision::Admit
    );
    assert_eq!(
        admit(&mut b, &l, Surface::Send, Duration::ZERO, now),
        Decision::Admit
    );
    assert_eq!(
        admit(&mut b, &l, Surface::Chrome, Duration::ZERO, now),
        Decision::Drop,
        "two of a burst of 2 are spent, so the ceiling is reached: chrome \
         yields instead of queueing"
    );
    assert!(
        matches!(
            admit(&mut b, &l, Surface::Final, Duration::ZERO, now),
            Decision::Hold(_)
        ),
        "content at the same dry instant is HELD, not dropped, on the same \
         budget - same bucket, different answer, and the refused chrome paid \
         nothing toward the refill content is waiting on"
    );
}

/// The named-vs-fallback and clamp branches of a 429. Slack's `Retry-After`
/// governs when it names one; a window measured in hours parks for the
/// ceiling, not for the request.
#[test]
fn a_429_parks_for_the_named_window_and_clamps_a_silly_one() {
    let l = limits(50, 10, 1_000);
    let now = std::time::Instant::now();

    let mut b = ChannelBudget::default();
    let pause = note_429(&mut b, &l, Some(Duration::from_secs(2)), now);
    assert_eq!(
        pause,
        Duration::from_secs(2),
        "named window honored exactly"
    );

    let mut b = ChannelBudget::default();
    let pause = note_429(&mut b, &l, None, now);
    assert_eq!(pause, Duration::from_secs(5), "unnamed 429 falls back");

    // Latest-wins across two windows. Slack names seconds on a tier breach and
    // minutes on a real one; 15 s sits inside this helper's 20 s ceiling, so the
    // assertion measures the ARMED park rather than the clamp, which is the next
    // case's job.
    let mut b = ChannelBudget::default();
    note_429(&mut b, &l, Some(Duration::from_secs(15)), now);
    let pause = note_429(&mut b, &l, Some(Duration::from_secs(2)), now);
    assert_eq!(
        pause,
        Duration::from_secs(15),
        "latest-wins is not shortening-wins: the earlier longer park stands"
    );

    let mut b = ChannelBudget::default();
    let pause = note_429(&mut b, &l, Some(Duration::from_secs(3600)), now);
    assert_eq!(
        pause,
        Duration::from_secs(20),
        "a one-hour window parks for the ceiling and then hands the problem \
         back to the API: stalling a conversation forever is a worse outage \
         than the one it prevents"
    );
}

/// A 429 must park only on a 429. Slack's error type splits rate limits into
/// their own variant, and the connector parses `Retry-After` into it - the
/// classification here is what keeps a `message_not_found` on a settle edit
/// from silencing the channel for five seconds.
#[test]
fn only_a_real_429_is_read_as_a_rate_limit() {
    let err = SlackClientError::RateLimitError(SlackRateLimitError {
        retry_after: Some(Duration::from_secs(7)),
        code: None,
        warnings: None,
        http_response_body: None,
    });
    assert!(
        Governor::is_rate_limited(&err),
        "SlackClientError::RateLimitError is the 429 shape in slack-morphism 2.26"
    );
    assert_eq!(
        Governor::retry_after(&err),
        Some(Duration::from_secs(7)),
        "and the window it names is the window we honor"
    );

    let unnamed = SlackClientError::RateLimitError(SlackRateLimitError {
        retry_after: None,
        code: None,
        warnings: None,
        http_response_body: None,
    });
    assert!(
        Governor::is_rate_limited(&unnamed),
        "a rate limit with no header is still a rate limit"
    );
    assert_eq!(
        Governor::retry_after(&unnamed),
        None,
        "and answers no window, which sends the caller to the fallback: the \
         absence of a number must not be read as zero"
    );

    let zero = SlackClientError::RateLimitError(SlackRateLimitError {
        retry_after: Some(Duration::ZERO),
        code: None,
        warnings: None,
        http_response_body: None,
    });
    assert_eq!(
        Governor::retry_after(&zero),
        None,
        "a zero Retry-After is not a window: it would park for an instant and \
         hammer again, so it reads as no window and the fallback governs"
    );
}

// ---------------------------------------------------------------------------
// Gate wiring (real Governor, virtual clock, swapped config)
// ---------------------------------------------------------------------------

/// Install a config mirror with the Slack governor knobs this suite expects,
/// then put the previous mirror back. The guard serializes against every other
/// test here, and the restore keeps the full-suite gate honest:
/// `Config::current()` is one process-wide mirror. Callers must hold
/// `telegram_cooldown_lock::guard()` from BEFORE this macro to after the
/// restore: the mirror is shared with the Telegram and Discord governor suites,
/// and an unserialized swap window rewrites their bucket math mid-body (#2012).
/// Shaped like `discord_governor_test.rs`'s macro (#1910) with the lock pulled
/// outside the swap, which is what the #2012 local gate receipts ask for.
macro_rules! slack_governor_config {
    ($($field:ident : $value:expr),* $(,)?) => {{
        let _guard = test_support::registry_guard().await;
        test_support::reset();
        let prev = Config::current();
        let mut cfg: Config = toml::from_str(include_str!("../../config.toml.example"))
            .expect("embedded config.toml.example must parse");
        $(cfg.channels.slack.governor.$field = $value;)*
        Config::set_current(cfg);
        (prev, _guard)
    }};
}

/// The async seam on the virtual clock: two clock paints inside the spacing
/// window is one too many, the second is refused WITHOUT sleeping (that is the
/// whole point of chrome), and the next tick restates it.
#[tokio::test]
async fn chrome_paint_is_refused_inside_the_spacing_window() {
    // The cross-suite lock is taken BEFORE the macro swaps the mirror:
    // `Config::current()` is one process-wide value and the Telegram governor
    // re-reads it on every admission, so a swap window that is not serialized
    // against the Telegram gate bodies flips their bucket math mid-assertion.
    // Receipt: #2012 local gate, `--lib governor` runs failing
    // `governor_gates_test` edit-ladder bodies that pass in isolation. The
    // lock also covers `test_support::advance` across this body (#1854).
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    let (prev, _guard) = slack_governor_config!(chrome_min_spacing_ms: 1_000u64);
    let gov = Governor::default();
    let conv = "C0SPACING";
    assert!(
        gov.gate(conv, Surface::Chrome).await,
        "the first paint of a turn must go out"
    );
    test_support::advance(500);
    assert!(
        !gov.gate(conv, Surface::Chrome).await,
        "a second paint inside the window is dropped, not queued: the caller \
         gets `false` and skips the edit"
    );
    test_support::advance(600);
    assert!(
        gov.gate(conv, Surface::Chrome).await,
        "and the next tick restates it"
    );
    if let Ok(cfg) = Arc::try_unwrap(prev) {
        Config::set_current(cfg);
    }
}

/// End-to-end on one real 429: the conversation it parks gets no chrome while
/// parked and its content write goes the moment the window closes, while a
/// neighbour keeps flowing. This is the product of #2012, and none of it is
/// visible from the pure tests: the park, the per-conversation isolation and
/// the delay-never-drop answer all live in the async layer.
#[tokio::test]
async fn a_429_parks_its_conversation_and_no_other() {
    // Same discipline as the test above: lock first, swap inside the window (#2012).
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    let (prev, _guard) = slack_governor_config!(pause_secs: 5u64);
    let gov = Governor::default();
    let parked = "C0PARKED";
    let neighbour = "C0NEIGHBOUR";
    // The response named 3 s; that is what we honor, and it is shorter than the
    // fallback, which is the point of reading `Retry-After` at all.
    gov.note_rate_limited(parked, Some(Duration::from_secs(3)));
    assert!(
        !gov.gate(parked, Surface::Chrome).await,
        "the ticker must not repaint a conversation Slack just told us to back off from"
    );
    assert!(
        gov.gate(neighbour, Surface::Chrome).await,
        "the park is per conversation: one slow channel cannot silence the workspace"
    );
    test_support::advance(3_100);
    assert!(
        gov.gate(parked, Surface::Final).await,
        "when the named window closes the settle edit goes out: it waited, it \
         did not lose the payload"
    );
    if let Ok(cfg) = Arc::try_unwrap(prev) {
        Config::set_current(cfg);
    }
}

/// The escape hatch has to be inert at the ASYNC layer too, not just in the
/// pure math: `enabled = false` must never park a caller in `gate`, chrome
/// included, or the one documented way back to pre-#2012 behaviour is a lie.
#[tokio::test]
async fn a_disabled_governor_hands_every_surface_straight_through() {
    // The swap itself is the shared-state write; hold the cross-suite lock
    // across the whole window even though this body never moves the clock.
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    let (prev, _guard) = slack_governor_config!(enabled: false);
    let gov = Governor::default();
    for surface in [Surface::Send, Surface::Final, Surface::Chrome] {
        assert!(
            gov.gate("C0OFF", surface).await,
            "a disabled gate admits {surface:?} instead of deciding"
        );
    }
    if let Ok(cfg) = Arc::try_unwrap(prev) {
        Config::set_current(cfg);
    }
}

/// The wiring #2012 exists to install: no Slack chat write may leave the
/// process except through `GatedWrites`, and the cosmetic re-renders must ask
/// for the CHROME surface. A future PR that adds a `chat_post_message` call
/// fails here, not in production after the retry storm it was meant to
/// prevent. The pin is source text because the gate has no socket to test
/// against and no live API to hammer.
#[test]
fn slack_chat_writes_only_leave_through_the_governor() {
    let handler = include_str!("../channels/slack/handler.rs");
    let flow = include_str!("../channels/slack/flow_progress.rs");
    let resume = include_str!("../channels/slack/resume.rs");
    let resume_turn = include_str!("../channels/slack/resume_turn.rs");
    let suggest = include_str!("../channels/slack/suggest_options.rs");
    let reactions = include_str!("../channels/slack/reactions.rs");

    // The flow progress callback writes into the tool group, so it lives in
    // its own module and counts toward the same coverage as handler.rs.
    for (name, src) in [
        ("handler.rs", handler),
        ("flow_progress.rs", flow),
        ("suggest_options.rs", suggest),
        ("reactions.rs", reactions),
    ] {
        assert!(
            !src.contains(".chat_post_message("),
            "{name}: an ungated chat.postMessage (#2012)"
        );
        assert!(
            !src.contains(".chat_update("),
            "{name}: an ungated chat.update (#2012)"
        );
        assert!(
            src.contains("use super::governor::GatedWrites;"),
            "{name}: writes go through the trait, so the trait must be in scope"
        );
    }

    // The resume producers only delegate to the turn; they must not write
    // to Slack around the gate.
    for (name, src) in [("resume.rs", resume), ("resume_turn.rs", resume_turn)] {
        assert!(
            !src.contains(".chat_post_message("),
            "{name}: an ungated chat.postMessage (#2012)"
        );
        assert!(
            !src.contains(".chat_update("),
            "{name}: an ungated chat.update (#2012)"
        );
    }

    let count = |needle: &str| handler.matches(needle).count() + flow.matches(needle).count();
    assert_eq!(
        count("session.post("),
        32,
        "every new-message site pays the budget: the count is the coverage"
    );
    assert_eq!(
        count("session.update("),
        7,
        "content edits (settle stamps, flipped waiting lines, approval \
         labels, the user's toggle) stay content: 7 of them"
    );
    assert_eq!(
        count("session.update_chrome("),
        5,
        "exactly five sites re-render state already on screen: the ticker's \
         clock, the step-group append, the two tool-status edits and the \
         detached-task row edit (#2149). More means a real answer got \
         reclassified as cosmetic"
    );
}
