//! Spacing-floor suite (#1927) — the cross-surface, per-chat minimum interval
//! between ANY two admissions, plus the per-second admission instrumentation
//! that lets a 429 be attributed to the pacer or to the server.
//!
//! Two layers, tested separately:
//!
//! - **Pure ring math** — [`governor::Recent`] and [`governor::spacing_wait`]
//!   are functions of an injected `Instant`, so the arithmetic is pinned
//!   without tokio, a registry or a clock seam.
//! - **Gate wiring** — G2/G3/G4 drive the REAL gates under the paused runtime
//!   and the virtual clock (`ts::advance`), asserting who is floored, who is
//!   exempt, and that `spacing_floor_ms = 0` restores the pre-#1927 behaviour.

use std::time::{Duration, Instant};

use teloxide::Bot;
use teloxide::types::{ChatId, MessageId};

use crate::channels::telegram::governor;
use crate::channels::telegram::governor::test_support as ts;
use crate::config::Config;

/// Replace the process-wide config mirror with `config.toml.example` plus
/// per-test `rate_limiter` mutations. Parsed fresh per test so knob changes
/// cannot leak sideways; the [`ts::registry_guard`] serializes the swap.
macro_rules! rl_config {
    ($($field:ident : $value:expr),* $(,)?) => {{
        let mut cfg: Config = toml::from_str(include_str!("../../config.toml.example"))
            .expect("embedded config.toml.example must parse");
        $(cfg.channels.telegram.rate_limiter.$field = $value;)*
        Config::set_current(cfg);
    }};
}

// ---------------------------------------------------------------------------
// Ring math
// ---------------------------------------------------------------------------

#[test]
fn recent_ring_counts_within_window_and_per_surface() {
    let t0 = Instant::now();
    let mut r = governor::Recent::default();
    r.push(t0, governor::SURFACE_TYPING);
    r.push(t0 + Duration::from_millis(200), governor::SURFACE_EDITS);
    r.push(t0 + Duration::from_millis(400), governor::SURFACE_TYPING);
    let now = t0 + Duration::from_millis(500);

    assert_eq!(r.count_within(now, Duration::from_secs(1)), 3);
    // Only the 400 ms sample falls inside a 150 ms window ending at 500 ms.
    assert_eq!(r.count_within(now, Duration::from_millis(150)), 1);
    assert_eq!(
        r.count_surface(now, Duration::from_secs(1), governor::SURFACE_TYPING),
        2
    );
    assert_eq!(
        r.count_surface(now, Duration::from_secs(1), governor::SURFACE_EDITS),
        1
    );
    assert_eq!(
        r.count_surface(now, Duration::from_secs(1), governor::SURFACE_SENDS),
        0
    );
    // The gap is measured to the NEWEST sample of any surface.
    assert_eq!(r.gap_ms(now), Some(100));
}

#[test]
fn recent_ring_is_bounded_and_drops_the_oldest_sample() {
    let t0 = Instant::now();
    let mut r = governor::Recent::default();
    // One typing sample first, then overflow the ring past its cap.
    r.push(t0, governor::SURFACE_TYPING);
    for i in 1..=governor::RECENT_CAP {
        r.push(
            t0 + Duration::from_millis(i as u64),
            governor::SURFACE_EDITS,
        );
    }
    let now = t0 + Duration::from_secs(10);
    assert_eq!(
        r.count_within(now, Duration::from_secs(60)),
        governor::RECENT_CAP,
        "the ring never grows past RECENT_CAP"
    );
    assert_eq!(
        r.count_surface(now, Duration::from_secs(60), governor::SURFACE_TYPING),
        0,
        "the oldest sample fell off the front"
    );
    assert_eq!(
        r.count_surface(now, Duration::from_secs(60), governor::SURFACE_EDITS),
        governor::RECENT_CAP
    );
}

#[test]
fn spacing_wait_returns_the_floor_remainder() {
    let t0 = Instant::now();
    let floor = Duration::from_millis(1000);
    let mut r = governor::Recent::default();

    // Empty ring: nothing has been admitted, so there is nothing to wait for.
    assert_eq!(governor::spacing_wait(&r, t0, floor), Duration::ZERO);

    r.push(t0, governor::SURFACE_SENDS);
    // 400 ms after the admission the chat still owes 600 ms.
    assert_eq!(
        governor::spacing_wait(&r, t0 + Duration::from_millis(400), floor),
        Duration::from_millis(600)
    );
    // At the floor the debt is settled, and it stays settled past it.
    assert_eq!(
        governor::spacing_wait(&r, t0 + Duration::from_millis(1000), floor),
        Duration::ZERO
    );
    assert_eq!(
        governor::spacing_wait(&r, t0 + Duration::from_secs(5), floor),
        Duration::ZERO
    );
}

#[test]
fn spacing_wait_is_zero_when_the_floor_is_disabled() {
    let t0 = Instant::now();
    let mut r = governor::Recent::default();
    r.push(t0, governor::SURFACE_SENDS);
    // `spacing_floor_ms = 0` means "no floor", not "wait forever".
    assert_eq!(
        governor::spacing_wait(&r, t0, Duration::ZERO),
        Duration::ZERO
    );
}

#[test]
fn edit_class_droppability_matrix() {
    // The floor is a drop path, so it may only shed what the ladder may shed.
    assert!(governor::EditClass::Clock.is_droppable());
    assert!(governor::EditClass::BrainPreview.is_droppable());
    assert!(governor::EditClass::Intermediary.is_droppable());
    assert!(governor::EditClass::Status.is_droppable());
    // Finals queue instead of dropping; interactive edits are user-initiated.
    assert!(!governor::EditClass::Final.is_droppable());
    assert!(!governor::EditClass::Interactive.is_droppable());
}

/// #1927: the floor defaults to the documented ~1/s per-chat interval, and the
/// key is OPTIONAL in config. Every existing `config.toml` predates it and the
/// template ships it commented out, so a missing `serde(default)` would fail
/// the upgrade at PARSE time rather than merely defaulting — which is what the
/// template parse below pins.
#[test]
fn spacing_floor_defaults_to_one_second_and_the_template_still_parses() {
    assert_eq!(
        crate::config::RateLimiterConfig::default().spacing_floor_ms,
        1000,
        "the documented ~1/s per-chat floor"
    );

    let parsed: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("embedded config.toml.example must still parse with the floor commented out");
    assert_eq!(
        parsed.channels.telegram.rate_limiter.spacing_floor_ms, 1000,
        "an ABSENT key must fall back to the default, not fail the parse"
    );
}

// ---------------------------------------------------------------------------
// Telemetry shape
// ---------------------------------------------------------------------------

#[test]
fn summary_line_reports_spacing_drops_without_disturbing_earlier_groups() {
    let c = governor::Counters {
        dropped_spacing: 3,
        ..Default::default()
    };
    let line = governor::format_summary(-100123, &c, 0, None)
        .expect("non-zero counters must render a line");

    assert!(line.contains("chat=-100123"), "{line}");
    assert!(
        line.contains(
            "dropped{clock=0,brain_preview=0,intermediary=0,status=0,typing=0,spacing=3}"
        ),
        "the floor counter rides the dropped group: {line}"
    );
    assert!(
        line.contains("throttled_ms{typing=0,send=0,rich=0}"),
        "{line}"
    );
    // No profile passed => no per-second block, so the cumulative groups keep
    // the exact shape the pinning test in governor_internals_test asserts.
    assert!(!line.contains("window{"), "{line}");
}

#[test]
fn recent_profile_renders_windows_surfaces_and_gap() {
    let t0 = Instant::now();
    let mut r = governor::Recent::default();
    r.push(t0, governor::SURFACE_TYPING);
    r.push(t0 + Duration::from_millis(100), governor::SURFACE_RICH);
    let now = t0 + Duration::from_millis(300);

    let p = r.profile(now);
    assert_eq!(p.last_1s, 2);
    assert_eq!(p.last_5s, 2);
    assert_eq!(p.last_60s, 2);
    assert_eq!(p.typing, 1);
    assert_eq!(p.rich, 1);
    assert_eq!(p.edits, 0);
    assert_eq!(p.sends, 0);
    assert_eq!(p.gap_ms, Some(200));

    let line = p.render();
    assert!(line.contains("window{1s=2,5s=2,60s=2}"), "{line}");
    assert!(
        line.contains("by_surface{typing=1,edits=0,sends=0,rich=1}"),
        "{line}"
    );
    assert!(line.contains("gap_ms=200"), "{line}");
}

#[tokio::test(start_paused = true)]
async fn recent_profile_reports_none_without_peers() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(0);
    assert_eq!(governor::recent_profile(None), "none");
}

#[tokio::test(start_paused = true)]
async fn recent_profile_renders_a_governed_peer() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(0);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1927);
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    let line = governor::recent_profile(Some(chat.0));
    assert!(line.contains(&format!("chat={}", chat.0)), "{line}");
    assert!(line.contains("window{1s=1,5s=1,60s=1}"), "{line}");
    assert!(
        line.contains("by_surface{typing=1,edits=0,sends=0,rich=0}"),
        "{line}"
    );
}

// ---------------------------------------------------------------------------
// Gate wiring
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn g1_typing_is_exempt_from_the_floor_but_feeds_the_ring() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1928);

    // Two back-to-back typing refreshes: the second is well inside the floor
    // window and must still be admitted — typing is cosmetic chrome the floor
    // does not gate. It does count as an admission, so the ring grows even
    // though the floor shed nothing.
    assert!(governor::admit_chat_action(chat, Some(42)).await);
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    let snap = ts::snapshot(chat).expect("peer registered by the typing gate");
    assert_eq!(snap.typing_admitted, 2);
    assert_eq!(snap.dropped_spacing, 0, "the floor never sheds typing");
}

#[tokio::test(start_paused = true)]
async fn g2_floor_sheds_droppable_chrome_but_queues_finals() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1929);
    let bot = Bot::new("TESTTOKEN");
    // One admission puts ~1000 ms of floor debt on the chat.
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    // The edit bucket is still full, yet the floor alone sheds the chrome edit
    // — proof the floor is charged AHEAD of the per-surface budget.
    assert!(
        !governor::edit_admission(
            &bot,
            chat,
            MessageId(1),
            governor::EditClass::Clock,
            "<b>tick</b>".into(),
            false,
        )
        .await,
        "a Clock edit inside the floor window must be shed"
    );

    // Drain the edit bucket, then a Final inside the same floor window still
    // gets in line rather than being shed.
    ts::burn_bucket(chat, ts::BucketKind::Edits, 10, 0.3);
    assert!(
        !governor::edit_admission(
            &bot,
            chat,
            MessageId(2),
            governor::EditClass::Final,
            "<b>settle</b>".into(),
            false,
        )
        .await
    );

    let snap = ts::snapshot(chat).expect("peer registered");
    assert_eq!(
        snap.dropped_spacing, 1,
        "only the chrome edit is a floor drop"
    );
    assert_eq!(snap.queued_finals, 1, "the final is queued, never shed");
    assert_eq!(snap.edits_admitted, 0, "neither edit spent a bucket token");
}

#[tokio::test(start_paused = true)]
async fn g3_send_waits_out_the_floor_remainder() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1930);
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    governor::pace_send(chat).await;

    let snap = ts::snapshot(chat).expect("peer registered");
    assert_eq!(snap.admitted_sends, 1, "the send went out");
    assert!(
        (900..=1010).contains(&snap.throttled_send_ms),
        "the send waited out the ~1000 ms floor remainder, got {} ms",
        snap.throttled_send_ms
    );
}

#[tokio::test(start_paused = true)]
async fn g4_rich_edit_waits_out_the_floor_remainder() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1931);
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    governor::pace_rich(chat, Some(42)).await;

    let snap = ts::snapshot(chat).expect("peer registered");
    assert_eq!(snap.admitted_rich, 1, "the rich edit went out");
    assert!(
        (900..=1010).contains(&snap.throttled_rich_ms),
        "the rich edit waited out the ~1000 ms floor remainder, got {} ms",
        snap.throttled_rich_ms
    );
}

#[tokio::test(start_paused = true)]
async fn zero_floor_restores_pre_1927_spacing() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true, spacing_floor_ms: 0);
    let chat = ChatId(-900_1932);
    let bot = Bot::new("TESTTOKEN");
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    // A chrome edit right on the heels of an admission is admitted again.
    assert!(
        governor::edit_admission(
            &bot,
            chat,
            MessageId(1),
            governor::EditClass::Clock,
            "<b>tick</b>".into(),
            false,
        )
        .await,
        "with the floor disabled the chrome edit is no longer shed"
    );
    // And a send is no longer held.
    governor::pace_send(chat).await;

    let snap = ts::snapshot(chat).expect("peer registered");
    assert_eq!(snap.dropped_spacing, 0);
    assert_eq!(snap.edits_admitted, 1);
    assert_eq!(snap.admitted_sends, 1);
    assert_eq!(snap.throttled_send_ms, 0, "no floor => no send hold");
}

// ---------------------------------------------------------------------------
// Host scheduling delay must never reach the hold ledger (#2076)
// ---------------------------------------------------------------------------

/// A pacer parked on its `Defer` must record the scripted delay, never the
/// host's scheduling delay.
///
/// The ledger is a wall-clock difference taken across `tokio::time::sleep`,
/// and under the paused runtime the sleeper is resumed by a real worker
/// thread, so real time leaks into a value the band assertions pin to the
/// millisecond. In a full-suite run (#2076) that leak was 17 ms and pushed
/// `throttled_rich_ms` to 1017 ms against a `900..=1010` band. Here the leak
/// is made deterministic instead of load-dependent: real wall time is burned
/// while the pacer sits parked on its sleep, with the virtual clock untouched.
#[tokio::test(start_paused = true)]
async fn a_slow_resumption_does_not_inflate_the_hold_ledger() {
    let _guard = ts::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    ts::reset(1_000);
    rl_config!(enabled: true);
    let chat = ChatId(-900_1933);
    assert!(governor::admit_chat_action(chat, Some(42)).await);

    // Park the pacer on its floor `Defer`. The global permit path takes a
    // token from a fresh bucket without suspending, so the first poll reaches
    // the sleep; the precondition below proves it is parked there.
    let pacer = tokio::spawn(async move { governor::pace_rich(chat, Some(42)).await });
    tokio::task::yield_now().await;
    let parked = ts::snapshot(chat).expect("peer registered");
    assert_eq!(
        parked.admitted_rich, 0,
        "the pacer must still be waiting out the floor, otherwise this test \
         burns real time outside the sleep and proves nothing"
    );

    // Real wall time the virtual clock never sees.
    std::thread::sleep(Duration::from_millis(120));
    pacer.await.expect("the pacer must not panic");

    let snap = ts::snapshot(chat).expect("peer registered");
    assert_eq!(snap.admitted_rich, 1, "the rich edit went out");
    assert!(
        (900..=1010).contains(&snap.throttled_rich_ms),
        "120 ms of host scheduling delay must not reach the hold ledger, got {} ms",
        snap.throttled_rich_ms
    );
}
