//! Proactive per-peer flood governors for Telegram forum chats (#1211).
//!
//! Telegram-side FLOOD_WAIT 429s grow daily on multi-session forum
//! deployments (391 chat-action 429s on Aug 25 alone, worst second 36 failed
//! typing attempts): a forum supergroup is ONE peer (`chat_id`) regardless of
//! topic, the typing indicator re-fires every 4 s per active turn, and the
//! flow loop re-renders chrome every 1.5 s tick. The existing handling is
//! reactive only ([`super::rate_limit`] — parse `Retry after`, sleep): nothing
//! prevents hitting the limit, and retries consume more of the same bucket,
//! amplifying into storms.
//!
//! This module enforces three independent governors keyed by `chat_id`,
//! layered BESIDE the reactive backstop, which stays as the last line of
//! defence:
//!
//! - **G1 typing** ([`admit_chat_action`]): token bucket ~1 call / 3 s per
//!   forum peer, burst capacity 8. Concurrent sessions in one topic collapse
//!   into that single refresh because `sendChatAction` is thread-scoped and
//!   stateless. Under pressure a caller holds briefly for refill instead of
//!   hammering (hold-and-release); a refresh held past
//!   [`Limits::typing_max_hold`] is dropped — the indicator is cosmetic and
//!   the next tick re-fires it.
//! - **G2 edits** ([`edit_admission`] + [`EditClass`]): token bucket
//!   ~18/min per forum peer. On an empty bucket the priority drop ladder
//!   applies — clock → brain preview → intermediary flow updates → status
//!   line — because each dropped class self-heals: every flow refresh
//!   re-renders FULL current state, so the next admitted edit carries the
//!   dropped one's content. **Finals are never dropped**: settle renders and
//!   plan-card refreshes queue latest-wins per message id ([`PendingFinal`])
//!   and a drainer flushes them as tokens refill.
//! - **G3 sends** ([`pace_send`]): paced ~1/s per chat under an ~18/min group
//!   ceiling with a small burst absorb. Sends delay rather than drop (#297),
//!   so pacing FAILS OPEN after [`SEND_MAX_HOLD`] instead of ever discarding.
//!
//! Scope is forums-only initially. A negative chat id (groups) becomes a
//! governed peer once any call is observed carrying a topic id — positive ids
//! are DMs and are never touched, so DM-only surfaces (draft streaming et al.)
//! pass through unchanged. Direct user-command replies stay outside G3 by
//! design (≤1 message per human action); the reactive backstop covers them.
//!
//! Telemetry: per-forum counters for admissions, ladder drops per class and
//! throttle milliseconds, summarized by one periodic INFO line
//! ([`summary_loop`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use teloxide::Bot;
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, MessageId, ParseMode};

use crate::config::Config;

/// Virtual-clock offset used ONLY by `cfg(test)` builds (`gate_now`). Tests
/// advance it by hand to simulate refill spacing, hold budgets and drain
/// ticks — mocked passage of time, zero real sleeps anywhere (#1211 test
/// contract). Production builds never read it.
#[cfg(test)]
static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);

/// Wall-clock seam for ALL gate math (bucket takes, hold accounting, drain
/// cadence). Production reads the real clock unchanged; test builds add the
/// monotonic [`CLOCK_OFFSET_MS`] offset so time passage is fully scripted.
#[cfg(not(test))]
pub(crate) fn gate_now() -> Instant {
    Instant::now()
}

/// [`gate_now`] — test build.
#[cfg(test)]
pub(crate) fn gate_now() -> Instant {
    let off = CLOCK_OFFSET_MS.load(Ordering::Relaxed);
    Instant::now()
        .checked_add(Duration::from_millis(off))
        .expect("virtual clock offset overflow")
}

/// Longest a SEND may be held by G3 pacing before failing open.
///
/// Sends carry real content and are delay-never-drop (#297), so the pacer has
/// no drop path at all — but an unbounded hold would pile caller tasks up
/// without limit under a pathological config or a flood-banned chat. Failing
/// open hands the send to the reactive backstop ([`super::rate_limit`]),
/// which owns oversized windows from there (#1064).
const SEND_MAX_HOLD: Duration = Duration::from_secs(30);

/// Retry budget for one queued final edit before it is abandoned with a warn.
///
/// Finals are never dropped AT ADMISSION, but a payload Telegram permanently
/// refuses (message deleted, chat gone) must not wedge the queue forever:
/// each drain attempt consumes one unit of this budget, and the reactive
/// error paths plus the next differing-content refresh take over afterwards.
const FINAL_MAX_ATTEMPTS: u32 = 8;

/// Drain-loop spacing. One queued final per tick keeps drained edits roughly
/// on the edit bucket's cadence without a dedicated wakeup channel.
const DRAIN_TICK: Duration = Duration::from_millis(400);

/// Process-wide token bucket pacer for all Telegram requests across all chats/topics.
/// Enforces a ~25 req/s global ceiling with burst capacity of 25.
///
/// A `std::sync::Mutex` taken from async code: correct only because every
/// path drops the guard before it awaits. Never hold this across an `.await`
/// — a blocking guard parked on a suspended future stalls every outbound
/// Telegram send in the process, which is the one lock where that is fatal.
static GLOBAL_PACER: Mutex<Option<Bucket>> = Mutex::new(None);

/// Global pacer parameters.
const GLOBAL_CAPACITY: u32 = 25;
const GLOBAL_REFILL_PER_SEC: f64 = 25.0;

/// Max duration a request may be held by the global pacer before failing open.
const GLOBAL_MAX_HOLD: Duration = Duration::from_secs(5);

/// Acquire a permit from the global rate limiter and wait out any active global 429 cooldown.
///
/// Returns `true` if a permit was cleanly acquired or successfully waited for,
/// or `false` if the hold exceeded `GLOBAL_MAX_HOLD` and failed open to prevent deadlock.
pub(crate) async fn acquire_global_permit() -> bool {
    // 1. First, respect any active global 429 cooldown lock
    super::rate_limit::wait_global_cooldown().await;

    // 2. Proactive global token bucket pacing
    let mut total_held = Duration::ZERO;
    loop {
        let now = gate_now();
        let wait_res = {
            let mut lock = GLOBAL_PACER.lock().unwrap_or_else(|e| e.into_inner());
            let bucket = lock.get_or_insert_with(|| {
                let mut b = Bucket::new(GLOBAL_CAPACITY, GLOBAL_REFILL_PER_SEC);
                b.last_refill = now;
                b
            });
            // Upstream's bucket has no interactive reserve floor, so `take`
            // is the floor-free consume that fork `take_any` provided.
            bucket.take(now)
        };

        match wait_res {
            Ok(()) => {
                return true;
            }
            Err(delay) => {
                total_held += delay;
                if total_held > GLOBAL_MAX_HOLD {
                    tracing::warn!(
                        "Telegram: Global pacer held for {:?} exceeding 5s limit — failing open to avoid hanging turns",
                        total_held
                    );
                    return false;
                }

                #[cfg(test)]
                test_support::advance(delay.as_millis() as u64);

                tokio::time::sleep(delay).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config snapshot
// ---------------------------------------------------------------------------

/// Live knob snapshot for one gate evaluation, from
/// `[channels.telegram.rate_limiter]`. Read fresh per call so config.toml
/// edits land without a restart (the same contract as `Config::current`).
struct Limits {
    enabled: bool,
    /// G1 refill spacing (one typing action per this much wall time).
    typing_interval: Duration,
    /// G1 burst capacity.
    typing_burst: u32,
    /// G1 longest hold before a typing refresh is dropped.
    typing_max_hold: Duration,
    /// G2 refill rate in tokens (edits) per second.
    edit_rate_per_sec: f64,
    /// G2 burst capacity.
    edit_burst: u32,
    /// G3 minimum spacing between sends.
    send_interval: Duration,
    /// G3 minute-ceiling capacity (refills fully over 60 s).
    send_minute_ceiling: u32,
    /// G3 burst capacity.
    send_burst: u32,
    /// G4 refill rate in tokens (rich calls) per second.
    rich_rate_per_sec: f64,
    /// G4 burst capacity.
    rich_burst: u32,
    /// G5 cross-surface minimum spacing between any two admissions to one
    /// chat (#1927). `Duration::ZERO` disables the floor — deliberately NOT
    /// clamped to a minimum, unlike every knob above.
    spacing_floor: Duration,
    /// Spacing of the telemetry summary INFO line.
    summary_log_period: Duration,
}

impl Limits {
    fn from_config() -> Self {
        let rl = &Config::current().channels.telegram.rate_limiter;
        let ceiling = rl.sends_ceiling_per_minute.max(1);
        Self {
            enabled: rl.enabled,
            typing_interval: Duration::from_secs(rl.typing_min_interval_secs.max(1)),
            typing_burst: rl.typing_burst.max(1),
            typing_max_hold: Duration::from_secs(rl.typing_max_hold_secs),
            edit_rate_per_sec: (rl.edits_per_minute.max(1) as f64) / 60.0,
            edit_burst: rl.edit_burst.max(1),
            send_interval: Duration::from_millis(rl.send_min_interval_millis.max(50)),
            send_minute_ceiling: ceiling,
            send_burst: rl.sends_burst.max(1),
            rich_rate_per_sec: (rl.rich_per_minute.max(1) as f64) / 60.0,
            rich_burst: rl.rich_burst.max(1),
            // No `.max(1)` here on purpose: 0 ms is the documented way to
            // disable the floor (#1927), so the clamp the other knobs carry
            // would silently turn "off" into "1 ms".
            spacing_floor: Duration::from_millis(rl.spacing_floor_ms),
            summary_log_period: Duration::from_secs(rl.summary_log_secs.max(30)),
        }
    }
}

// ---------------------------------------------------------------------------
// Surface policy descriptors (#635)
// ---------------------------------------------------------------------------

// Everything in this section is consumed by `pace_engine` (the next step).
// It lands as a frozen descriptor first so it can be reviewed against
// `src/docs/governor-policy-matrix.md` without a single call site moving; the
// `allow(dead_code)` markers come off when the engine reads every field.
//
// Source of truth: `src/docs/governor-policy-matrix.md`.
// A cell that changes here is a behavioural change and must be named as one.

/// Whether a surface consults the shared cross-surface spacing floor (#676).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SpacingPolicy {
    /// G1 (typing): no spacing check at all.
    Exempt,
    /// G3 (sends): the floor delays the request; the gate waits, or fails open.
    Wait,
    /// G2/G4: droppable chrome is discarded on a short gap, content is not.
    DropDroppable,
}

/// What happens to droppable chrome (clock / brain-preview / intermediary /
/// status edits) when the surface cannot admit it right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ChromeOnDry {
    /// Discard it — G2 and G4 both drop droppable chrome on a dry bucket.
    Drop,
    /// The surface has no droppable class at all (G1 typing, G3 sends).
    NotApplicable,
}

/// What happens to content (G2 finals, G3 sends, G4 rich content) when the
/// surface cannot admit it right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ContentOnDry {
    /// Discard it — G1's typing refresh has no queue and no wait.
    Drop,
    /// Queue it latest-wins, superseding whatever was queued before (G2).
    Queue,
    /// Hold until a token refills (G3, G4).
    Wait,
}

/// Which budget bounds a surface's hold. The number itself is config-derived
/// and lives in [`Limits`]; this names which knob supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum HoldBudget {
    /// No wait loop at all — the gate answers in one shot (G2, G4).
    None,
    /// [`Limits::typing_max_hold`] (G1).
    Typing,
    /// The [`SEND_MAX_HOLD`] constant (G3).
    Send,
}

/// What an over-budget hold does to a surface's counters — the matrix's
/// `fail-open` cell, named by its effect. Only G3 admits anyway: a send is
/// delay-never-drop (#297), so its pacer has no drop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum HoldExceeded {
    /// G1: the refresh is dropped once `typing_max_hold` elapses.
    DropTyping,
    /// G3: the send goes out anyway, still counted as admitted.
    AdmitSend,
    /// G2/G4: no cap exists, so this arm is unreachable by construction.
    Unreachable,
}

/// How a surface treats an active global 429 cooldown / permit refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum PermitPolicy {
    /// G1/G2: the global permit is not consulted at all.
    Ignore,
    /// G3: log and proceed — a send is delay-never-drop (#297).
    WarnAndProceed,
    /// G4: refuse, and let the caller defer.
    Refuse,
}

/// Why a surface discarded a payload. One variant per drop path, so a caller
/// can increment the counter its own surface already keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DropReason {
    /// The cross-surface spacing floor was not met (droppable chrome only).
    SpacingFloor,
    /// The surface's own bucket was dry.
    BucketDry,
    /// The surface's hold budget elapsed; the gate refuses rather than waits.
    HoldExceeded,
}

/// The engine's verdict for one admission attempt.
///
/// The four gates return four different shapes today (`bool`, `bool`, `()`,
/// `()`). One enum means a gate cannot invent a fifth outcome, and a caller
/// that ignores the reason stops compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum PaceOutcome {
    /// The request may proceed now.
    Admit,
    /// The engine declined to run the surface at all: the peer is ungoverned
    /// (no topic-scoped call has marked it yet). Each gate's ungoverned answer
    /// is its own — `true` for the `bool` gates, `Now` for rich.
    Bypass,
    /// The request was discarded; the reason names the path that did it.
    Drop(DropReason),
    /// The request was queued latest-wins for a later tick (G2 finals only).
    Queue,
    /// The request must wait this long before it may retry.
    Defer(Duration),
    /// The hold budget elapsed and the policy says admit anyway (G3 only). The
    /// payload is the remainder the gate could not buy — telemetry, not a wait.
    FailOpen(Duration),
}

/// One row of the governor policy matrix — the frozen contract for #635.
///
/// Every field is a column of that table. The four consts below are the four
/// gates, and `pace_engine` reads them instead of the four hand-written
/// skeletons that let the gates diverge in the first place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct SurfacePolicy {
    /// The gate this policy describes, as its function name.
    pub(crate) gate: &'static str,
    /// The event-time ring surface pushed on admission.
    pub(crate) ring: &'static str,
    /// Whether the shared spacing floor applies, and how.
    pub(crate) spacing: SpacingPolicy,
    /// Faith of droppable chrome when the bucket is dry.
    pub(crate) chrome_on_dry: ChromeOnDry,
    /// Faith of content when the bucket is dry.
    pub(crate) content_on_dry: ContentOnDry,
    /// Which budget bounds the hold.
    pub(crate) hold: HoldBudget,
    /// What an over-budget hold does to this surface's counters.
    pub(crate) hold_exceeded: HoldExceeded,
    /// How the global permit / cooldown is treated.
    pub(crate) permit: PermitPolicy,
    /// Whether the gate records `forum_seen` when a thread id is present.
    /// G2 and G3 never do — they read the flag but do not set it.
    pub(crate) sets_forum_seen: bool,
}

#[allow(dead_code)]
impl SurfacePolicy {
    /// G1 — `sendChatAction` (typing). Floor-exempt (#676), drops its refresh
    /// once `typing_max_hold` elapses, never fails open.
    pub(crate) const TYPING: Self = Self {
        gate: "admit_chat_action",
        ring: SURFACE_TYPING,
        spacing: SpacingPolicy::Exempt,
        chrome_on_dry: ChromeOnDry::NotApplicable,
        content_on_dry: ContentOnDry::Drop,
        hold: HoldBudget::Typing,
        hold_exceeded: HoldExceeded::DropTyping,
        permit: PermitPolicy::Ignore,
        sets_forum_seen: true,
    };

    /// G2 — `editMessageText` / rich edits. No wait loop: content queues
    /// latest-wins, droppable chrome is discarded.
    pub(crate) const EDITS: Self = Self {
        gate: "edit_admission",
        ring: SURFACE_EDITS,
        spacing: SpacingPolicy::DropDroppable,
        chrome_on_dry: ChromeOnDry::Drop,
        content_on_dry: ContentOnDry::Queue,
        hold: HoldBudget::None,
        hold_exceeded: HoldExceeded::Unreachable,
        permit: PermitPolicy::Ignore,
        sets_forum_seen: false,
    };

    /// G3 — `sendMessage`. Two buckets AND-ed, floor waited, global permit
    /// logged and ignored, and the only surface that fails open.
    pub(crate) const SENDS: Self = Self {
        gate: "pace_send",
        ring: SURFACE_SENDS,
        spacing: SpacingPolicy::Wait,
        chrome_on_dry: ChromeOnDry::NotApplicable,
        content_on_dry: ContentOnDry::Wait,
        hold: HoldBudget::Send,
        hold_exceeded: HoldExceeded::AdmitSend,
        permit: PermitPolicy::WarnAndProceed,
        sets_forum_seen: false,
    };

    /// G4 — `sendRichMessage` and rich edits. Floor applies to finals only
    /// (#757), droppable chrome is discarded, content waits, never fails open.
    pub(crate) const RICH: Self = Self {
        gate: "pace_rich",
        ring: SURFACE_RICH,
        spacing: SpacingPolicy::DropDroppable,
        chrome_on_dry: ChromeOnDry::Drop,
        content_on_dry: ContentOnDry::Wait,
        hold: HoldBudget::None,
        hold_exceeded: HoldExceeded::Unreachable,
        permit: PermitPolicy::Refuse,
        sets_forum_seen: true,
    };
}

// ---------------------------------------------------------------------------
// The engine (#635)
// ---------------------------------------------------------------------------

/// Which surface's hold counter a gate's elapsed hold is attributed to
/// (#635). The three counters stay separate in the summary because they
/// measure three different budgets — merging them would hide which surface
/// spent the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldSurface {
    /// G1 — the wait against `typing_max_hold`.
    Typing,
    /// G3 — the wait against `SEND_MAX_HOLD`.
    Sends,
    /// G4 — the wait for a rich token.
    Rich,
}

/// Attribute hold time spent by a gate back to its peer's counters, keyed by
/// the surface that spent it (#635). One helper replaces the three
/// byte-identical `fold_throttle_ms` / `fold_send_ms` / `fold_rich_ms` copies,
/// which differed only in the counter they bumped. Best effort: a peer entry
/// vanishing between hold and fold loses one sample.
fn fold_hold_ms(chat_id: i64, waited: Duration, surface: HoldSurface) {
    if waited.is_zero() {
        return;
    }
    let ms = waited.as_millis() as u64;
    if ms == 0 {
        return;
    }
    if let Some(peer) = peers_guard().get_mut(&chat_id) {
        match surface {
            HoldSurface::Typing => peer.counters.throttled_typing_ms += ms,
            HoldSurface::Sends => peer.counters.throttled_send_ms += ms,
            HoldSurface::Rich => peer.counters.throttled_rich_ms += ms,
        }
    }
}

/// The one admission engine behind all four governor gates (#635).
///
/// Each gate used to carry its own copy of this skeleton — lock the peer,
/// honour `forum_seen`, take the surface's bucket(s), decide admit/drop/hold —
/// and the copies drifted: #757 was `pace_send` and `pace_rich` disagreeing
/// about the spacing floor, in code that was otherwise the same seven stages.
/// Here the stages live once and every difference between gates is a column of
/// [`SurfacePolicy`].
///
/// `admit` runs UNDER the peers lock and owns the surface's own buckets and
/// counters; it returns the raw verdict. The engine applies the shared spacing
/// floor and the hold budget, which are the two decisions that must not be
/// re-expressed per gate: a gate that re-derives its own hold cap is exactly
/// the divergence this removes. `waited_so_far` is the hold the caller has
/// already absorbed this attempt, which is what the budget is measured
/// against.
///
/// [`PaceOutcome::Bypass`] is the engine's own answer, never `admit`'s: the
/// surface was not run at all because the peer is still ungoverned.
fn pace_engine<F>(
    policy: &SurfacePolicy,
    lim: &Limits,
    chat_id: i64,
    thread_id: Option<i32>,
    waited_so_far: Duration,
    admit: F,
) -> PaceOutcome
where
    F: FnOnce(&mut Peer, Instant, Duration) -> PaceOutcome,
{
    let mut map = peers_guard();
    let peer = map.entry(chat_id).or_default();
    if policy.sets_forum_seen && thread_id.is_some() {
        peer.forum_seen = true;
    }
    if !peer.forum_seen {
        return PaceOutcome::Bypass;
    }
    let now = gate_now();
    let spacing_hold = match policy.spacing {
        SpacingPolicy::Exempt => Duration::ZERO,
        SpacingPolicy::Wait | SpacingPolicy::DropDroppable => {
            spacing_wait(&peer.recent, now, lim.spacing_floor)
        }
    };
    let verdict = admit(peer, now, spacing_hold);
    if let PaceOutcome::Defer(wait) = verdict {
        // The hold budget is the ONE place a `Defer` can become terminal.
        // `HoldBudget::None` is no cap at all, not "no waiting": G4 waits
        // until a token refills, however long that takes, and its
        // `hold_exceeded` cell is unreachable by construction.
        let over = match policy.hold {
            HoldBudget::None => false,
            HoldBudget::Typing => waited_so_far + wait > lim.typing_max_hold,
            HoldBudget::Send => waited_so_far + wait > SEND_MAX_HOLD,
        };
        if !over {
            return PaceOutcome::Defer(wait);
        }
        // An over-budget hold is still an event on the surface's OWN counters,
        // and the surface — not the engine — knows which ones. It is recorded
        // HERE, under the lock the gate already holds and with the instant the
        // gate itself computed: re-locking in the caller would take the mutex
        // twice and drift `now`, and the ring push is exactly what the next
        // request's spacing floor measures its gap against. Getting this wrong
        // is not cosmetic — a fail-open send that goes out uncounted leaves
        // `admitted_sends` short of the traffic that actually reached Telegram.
        match policy.hold_exceeded {
            HoldExceeded::DropTyping => {
                peer.counters.dropped_typing += 1;
                PaceOutcome::Drop(DropReason::HoldExceeded)
            }
            HoldExceeded::AdmitSend => {
                peer.counters.admitted_sends += 1;
                peer.recent.push(now, SURFACE_SENDS);
                PaceOutcome::FailOpen(wait)
            }
            // No surface reaches here: G2 never defers, and G4 has no cap to
            // exceed. Fail closed if the table and the engine ever disagree.
            HoldExceeded::Unreachable => PaceOutcome::Drop(DropReason::HoldExceeded),
        }
    } else {
        verdict
    }
}

// ---------------------------------------------------------------------------
// Token bucket
// ---------------------------------------------------------------------------

/// Minimal continuous-refill token bucket. Pure with respect to the injected
/// `now`, so the admission math is unit-testable without tokio or config.
pub(crate) struct Bucket {
    pub(crate) tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
    /// Deadline of a 429 pause armed on this bucket (#635), if any.
    ///
    /// A pause is not a spend: the bucket is FROZEN, not emptied. Refill stops
    /// for the window and resumes where it left off, so a chat that took a 429
    /// is not additionally punished with a cold bucket once the window closes.
    /// Kept on the bucket rather than on [`Peer`] so all four gates honour it
    /// through the one `take`/`next_token_in` path they already share.
    pause_until: Option<Instant>,
}

impl Bucket {
    pub(crate) fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            tokens: f64::from(capacity),
            capacity: f64::from(capacity),
            refill_per_sec,
            last_refill: Instant::now(),
            pause_until: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        if let Some(until) = self.pause_until {
            if until > now {
                // Frozen: a paused bucket accrues nothing, so the window is
                // dead time rather than a windfall handed back on expiry.
                return;
            }
            // The pause has just elapsed. Start the refill clock AT the
            // deadline, not at the pre-pause `last_refill`: otherwise the
            // whole window counts as elapsed and the bucket refills to
            // capacity the instant it unfreezes, which is the burst the pause
            // exists to prevent.
            self.last_refill = until;
            self.pause_until = None;
        }
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.last_refill = now;
        let gained = elapsed.as_secs_f64() * self.refill_per_sec;
        self.tokens = (self.tokens + gained).min(self.capacity);
    }

    /// Consume one token, or report how long until the next refills.
    pub(crate) fn take(&mut self, now: Instant) -> Result<(), Duration> {
        let wait = self.next_token_in(now);
        if wait.is_zero() {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(wait)
        }
    }

    /// Peek without consuming: when the next token becomes available — which,
    /// while a 429 pause is in force, is when the pause lifts (#635). Folding
    /// the pause in here rather than in `take` is what makes it reach all four
    /// gates: `pace_send` and `pace_rich` decide on `next_token_in` and only
    /// then call `take`, so a pause visible only to `take` would never stop
    /// them.
    fn next_token_in(&mut self, now: Instant) -> Duration {
        self.refill(now);
        let pause = self.pause_remaining(now);
        let refill = if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.refill_per_sec)
        };
        refill.max(pause)
    }

    /// Time left on the pause, without mutating it.
    fn pause_remaining(&self, now: Instant) -> Duration {
        match self.pause_until {
            Some(until) if until > now => until.duration_since(now),
            _ => Duration::ZERO,
        }
    }

    /// The pause in force for this bucket, clearing an expired one on read.
    ///
    /// The read side of the per-chat pause (#635): [`Peer::paused_until`] asks
    /// every bucket and the peer reports the newest live deadline.
    pub(crate) fn paused_until(&mut self, now: Instant) -> Option<Instant> {
        match self.pause_until {
            Some(until) if until > now => Some(until),
            Some(_) => {
                self.pause_until = None;
                None
            }
            None => None,
        }
    }

    /// Arm a 429 pause on this bucket, never shortening a longer one.
    pub(crate) fn pause_arm(&mut self, until: Instant) {
        self.pause_until = Some(match self.pause_until {
            Some(existing) if existing > until => existing,
            _ => until,
        });
    }
}

/// Initialize or reshape a lazily-built bucket slot. A config change that
/// moves capacity or rate rebuilds the bucket fresh (burst restored) rather
/// than silently keeping the old shape for the life of the process.
pub(crate) fn ensure_bucket(
    slot: &mut Option<Bucket>,
    capacity: u32,
    refill_per_sec: f64,
) -> &mut Bucket {
    let stale = match slot {
        Some(b) => {
            b.capacity != f64::from(capacity)
                || (b.refill_per_sec - refill_per_sec).abs() > f64::EPSILON
        }
        None => true,
    };
    if stale {
        // A live 429 pause is not part of the bucket's SHAPE. Rebuilding for a
        // capacity/rate change (or replacing a `note_429_pause` placeholder)
        // must not hand the chat its tokens back while it is still being
        // throttled (#635).
        let carried = slot.as_ref().and_then(|b| b.pause_until);
        let mut fresh = Bucket::new(capacity, refill_per_sec);
        fresh.pause_until = carried;
        *slot = Some(fresh);
    }
    slot.as_mut().expect("bucket was just built")
}

// ---------------------------------------------------------------------------
// G5 — cross-surface spacing floor (#1927)
// ---------------------------------------------------------------------------

/// How many recent admissions the event-time ring keeps per peer.
///
/// 64 samples at the measured ~43 admits/min is ~90 s of history — long enough
/// to span the 60 s window the group limit is quoted in, and ~24 B per entry
/// across a handful of peers is immaterial against the daemon's cgroup.
pub(crate) const RECENT_CAP: usize = 64;

/// Surface labels for the event-time ring. `&'static str` so recording an
/// admission allocates nothing.
pub(crate) const SURFACE_TYPING: &str = "typing";
pub(crate) const SURFACE_EDITS: &str = "edits";
pub(crate) const SURFACE_SENDS: &str = "sends";
pub(crate) const SURFACE_RICH: &str = "rich";

/// Sliding window over the last [`RECENT_CAP`] admissions of one peer.
///
/// The per-surface buckets answer "is THIS surface over its own budget?"; the
/// floor needs the complementary question — "was ANYTHING admitted to this
/// chat in the last second?" — because Telegram's per-chat rule is sub-minute
/// and surface-blind. Entries carry [`gate_now`] instants, so the ring is a
/// pure function of the same clock seam the buckets use and is unit-testable
/// without tokio.
#[derive(Default)]
pub(crate) struct Recent {
    t: VecDeque<(Instant, &'static str)>,
}

impl Recent {
    /// Record one admission. Oldest entries fall off past the cap, so the
    /// 60 s window stays exact only while the peer admits <64/60 s (measured
    /// p50 is 43/min); under a sustained flood the window saturates and the
    /// floor reads conservative, never permissive.
    ///
    /// `pub(crate)` so the spacing-floor suite can script an exact ring
    /// without driving a gate; the gates are its only production callers.
    pub(crate) fn push(&mut self, now: Instant, surface: &'static str) {
        if self.t.len() == RECENT_CAP {
            self.t.pop_front();
        }
        self.t.push_back((now, surface));
    }

    /// Admissions of ANY surface inside `window` ending at `now`.
    pub(crate) fn count_within(&self, now: Instant, window: Duration) -> usize {
        self.t
            .iter()
            .filter(|(t, _)| now.saturating_duration_since(*t) < window)
            .count()
    }

    /// Admissions of ONE surface inside `window` ending at `now`.
    pub(crate) fn count_surface(&self, now: Instant, window: Duration, surface: &str) -> usize {
        self.t
            .iter()
            .filter(|(t, s)| *s == surface && now.saturating_duration_since(*t) < window)
            .count()
    }

    /// Milliseconds since the most recent admission of any surface, or `None`
    /// on a peer that has not admitted yet.
    pub(crate) fn gap_ms(&self, now: Instant) -> Option<u128> {
        self.t
            .back()
            .map(|(t, _)| now.saturating_duration_since(*t).as_millis())
    }

    /// Snapshot the ring for telemetry (#1927). The per-second counts are the
    /// instrument that lets a 429 be attributed: a refusal minute whose prior
    /// second admitted >=1 request is a real over-rate signal, while one whose
    /// prior second admitted nothing points at the server, not the pacer.
    pub(crate) fn profile(&self, now: Instant) -> RecentProfile {
        let minute = Duration::from_secs(60);
        RecentProfile {
            last_1s: self.count_within(now, Duration::from_secs(1)),
            last_5s: self.count_within(now, Duration::from_secs(5)),
            last_60s: self.count_within(now, minute),
            typing: self.count_surface(now, minute, SURFACE_TYPING),
            edits: self.count_surface(now, minute, SURFACE_EDITS),
            sends: self.count_surface(now, minute, SURFACE_SENDS),
            rich: self.count_surface(now, minute, SURFACE_RICH),
            gap_ms: self.gap_ms(now),
        }
    }
}

/// One peer's admission profile (#1927), derived from [`Recent`]. Cumulative
/// [`Counters`] answer "how much since boot"; this answers "how much just
/// now", which is the only shape a sub-minute rate rule can be judged by.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RecentProfile {
    pub(crate) last_1s: usize,
    pub(crate) last_5s: usize,
    pub(crate) last_60s: usize,
    pub(crate) typing: usize,
    pub(crate) edits: usize,
    pub(crate) sends: usize,
    pub(crate) rich: usize,
    pub(crate) gap_ms: Option<u128>,
}

impl RecentProfile {
    /// One-line rendering shared by the periodic summary and the 429 log.
    pub(crate) fn render(&self) -> String {
        format!(
            "window{{1s={},5s={},60s={}}} by_surface{{typing={},edits={},sends={},rich={}}} gap_ms={}",
            self.last_1s,
            self.last_5s,
            self.last_60s,
            self.typing,
            self.edits,
            self.sends,
            self.rich,
            self.gap_ms
                .map(|g| g.to_string())
                .unwrap_or_else(|| "none".to_string()),
        )
    }
}

/// Per-second admission profile for one peer — or every governed peer when
/// `chat` is `None` — rendered for a log line (#1927).
///
/// Reads the peer registry and nothing else, so a caller may take it before
/// acquiring any other lock. A peer that has never admitted renders zeros;
/// with no peers at all the result is `"none"`.
pub(crate) fn recent_profile(chat: Option<i64>) -> String {
    let now = gate_now();
    let map = peers().lock().unwrap_or_else(|e| e.into_inner());
    let mut parts: Vec<String> = Vec::new();
    for (id, peer) in map.iter() {
        if chat.is_some_and(|want| *id != want) {
            continue;
        }
        parts.push(format!("chat={id} {}", peer.recent.profile(now).render()));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("; ")
    }
}

/// How long this chat must still wait before its next request may be admitted
/// (#1927). [`Duration::ZERO`] means the floor is satisfied, or disabled.
///
/// ONE computation for every gate: the floor is cross-surface, so it reads the
/// ring rather than any surface's bucket, and it is charged AHEAD of the
/// per-surface budget so a request the floor sheds never spends a token a
/// later legitimate one needs.
pub(crate) fn spacing_wait(recent: &Recent, now: Instant, floor: Duration) -> Duration {
    if floor.is_zero() {
        return Duration::ZERO;
    }
    match recent.gap_ms(now) {
        // A gap at or beyond the floor means the chat has already waited long
        // enough. `try_from` guards the u128 -> u64 narrowing rather than
        // casting, and `saturating_sub` collapses both that case and a
        // pathologically large gap to zero, i.e. no hold.
        Some(gap) => {
            let gap_ms = u64::try_from(gap).unwrap_or(u64::MAX);
            floor.saturating_sub(Duration::from_millis(gap_ms))
        }
        None => Duration::ZERO,
    }
}

// ---------------------------------------------------------------------------
// Per-peer state
// ---------------------------------------------------------------------------

/// Dialect of the queued final payload (#229). Distinguishes HTML (classic/rich)
/// from Markdown (rich plan cards), ensuring media-empty markdown edits do not
/// get sent to Telegram as HTML and corrupt whitespace / formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub(crate) enum FinalDialect {
    #[default]
    Html,
    Markdown,
}

/// A final edit Telegram refused transiently, held latest-wins per message id
/// until the drainer can land it.
#[derive(Clone)]
struct PendingFinal {
    bot: Bot,
    /// HTML for rich/classic finals; MARKDOWN when `dialect` is `FinalDialect::Markdown`
    /// (the markdown+media edit dialect shares this field — see `run_final_edit`).
    html: String,
    /// Rich-API edit when true, classic HTML `editMessageText` otherwise.
    rich: bool,
    /// Dialect of the final payload (#229).
    dialect: FinalDialect,
    /// Media entries (rendered mermaid diagrams) for the markdown+media edit
    /// dialect (#134 family). Empty for every other final shape.
    media: Vec<super::rich::mermaid::MediaEntry>,
    /// Optional reply markup (inline keyboard) preserved across queue drain (#155).
    reply_markup: Option<serde_json::Value>,
    attempts: u32,
}

/// Counters behind the periodic summary line. Field-per-class instead of a
/// map so the summary formatting cannot silently miss a newly named class.
#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) admitted_typing: u64,
    pub(crate) admitted_edits: u64,
    pub(crate) admitted_sends: u64,
    pub(crate) admitted_rich: u64,
    pub(crate) dropped_typing: u64,
    /// Admissions shed by the cross-surface spacing floor (#1927). Distinct
    /// from the ladder drops above: those are per-surface budget exhaustion,
    /// this one is the chat-wide sub-minute rule refusing the next request
    /// regardless of which surface asked.
    pub(crate) dropped_spacing: u64,
    pub(crate) dropped_clock: u64,
    pub(crate) dropped_brain_preview: u64,
    pub(crate) dropped_intermediary: u64,
    pub(crate) dropped_status: u64,
    pub(crate) queued_finals: u64,
    pub(crate) superseded_finals: u64,
    pub(crate) delivered_finals: u64,
    pub(crate) failed_finals: u64,
    pub(crate) throttled_typing_ms: u64,
    pub(crate) throttled_send_ms: u64,
    pub(crate) throttled_rich_ms: u64,
    /// 429 pauses armed on this peer's buckets (#635). Counted on the peer
    /// rather than in `rate_limit` because the pause is per-chat state: the
    /// process-wide deadline is one number shared by every chat, while this
    /// says how often THIS chat was the one that hit the wall.
    pub(crate) pause_armed_429: u64,
}

impl Counters {
    /// Record a ladder drop. The Final arm stays empty ON PURPOSE: finals are
    /// never dropped here — admission queues them instead (#1211).
    pub(crate) fn note_drop(&mut self, class: EditClass) {
        match class {
            EditClass::Clock => self.dropped_clock += 1,
            EditClass::BrainPreview => self.dropped_brain_preview += 1,
            EditClass::Intermediary => self.dropped_intermediary += 1,
            EditClass::Status => self.dropped_status += 1,
            EditClass::Final | EditClass::Interactive => {}
        }
    }

    /// Record one admission shed by the cross-surface spacing floor (#1927).
    pub(crate) fn note_spacing_drop(&mut self) {
        self.dropped_spacing += 1;
    }

    /// True while nothing at all was counted — quiet forums stay silent in
    /// the periodic summary instead of logging zero-lines forever.
    fn all_zero(&self) -> bool {
        self.admitted_typing == 0
            && self.admitted_edits == 0
            && self.admitted_sends == 0
            && self.dropped_typing == 0
            && self.dropped_spacing == 0
            && self.dropped_clock == 0
            && self.dropped_brain_preview == 0
            && self.dropped_intermediary == 0
            && self.dropped_status == 0
            && self.queued_finals == 0
            && self.superseded_finals == 0
            && self.delivered_finals == 0
            && self.failed_finals == 0
            && self.throttled_typing_ms == 0
            && self.throttled_send_ms == 0
            && self.pause_armed_429 == 0
    }
}

/// Everything the governors track for one forum peer.
#[derive(Default)]
struct Peer {
    /// Flips true the first time ANY call for this chat is observed carrying
    /// a topic id. Until then the peer passes through ungoverned: forums-only
    /// rollout, DMs (positive ids) never governed at all.
    ///
    /// Contract (#1708): the topic id must be FORUM-CERTIFIED — resolved
    /// through the session topic pipeline, never a raw incoming reply-chain
    /// id (ordinary groups hand those out and they are not forum evidence).
    /// Enforced by `telegram_thread_gate_scan_test`.
    forum_seen: bool,
    typing: Option<Bucket>,
    edits: Option<Bucket>,
    sends_sec: Option<Bucket>,
    sends_min: Option<Bucket>,
    rich: Option<Bucket>,
    /// Queued finals keyed by message id — insert-replace IS latest-wins.
    finals: HashMap<i32, PendingFinal>,
    /// A drainer task is currently running for this peer.
    draining: bool,
    counters: Counters,
    /// Last [`RECENT_CAP`] admissions, any surface (#1927). The G5 floor and
    /// the per-second telemetry both read this; it is deliberately NOT part
    /// of [`Counters`], which stays a cumulative tally.
    recent: Recent,
}

impl Peer {
    /// The newest 429 pause in force across this peer's surfaces, if any (#635).
    ///
    /// Every bucket carries the same deadline — [`note_429_pause`] arms them
    /// all — so this is a max, not a merge: whichever bucket was armed for
    /// longest speaks for the chat. Reading the pause out of the buckets is
    /// what makes "is this chat paused" and "may this surface spend" the same
    /// question instead of two that can disagree.
    fn paused_until(&mut self, now: Instant) -> Option<Instant> {
        let mut newest: Option<Instant> = None;
        for slot in [
            &mut self.typing,
            &mut self.edits,
            &mut self.sends_sec,
            &mut self.sends_min,
            &mut self.rich,
        ] {
            if let Some(until) = slot.as_mut().and_then(|b| b.paused_until(now)) {
                newest = Some(newest.map_or(until, |n: Instant| n.max(until)));
            }
        }
        newest
    }
}

fn peers() -> &'static Mutex<HashMap<i64, Peer>> {
    static PEERS: OnceLock<Mutex<HashMap<i64, Peer>>> = OnceLock::new();
    PEERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lock the peers map, recovering from a poisoned mutex.
///
/// The gates never hold a lock across an `.await`, so a panic that poisons the
/// mutex leaves the map consistent; recovering the guard keeps a single
/// poisoned peer from wedging every other chat's pacing. One helper so the
/// recovery is written once instead of at each of the lock sites.
fn peers_guard() -> MutexGuard<'static, HashMap<i64, Peer>> {
    peers().lock().unwrap_or_else(|e| e.into_inner())
}

/// Longest per-chat 429 pause any surface may be armed for (#635).
///
/// The process-wide deadline already caps its inline wait at
/// [`super::rate_limit::MAX_INLINE_RATE_LIMIT_WAIT`]; this is the per-chat
/// twin, and it is deliberately longer (45s against 30s) because the two answer
/// different questions. The inline cap bounds how long ONE call parks the agent
/// turn; a pause only stops the chat's own buckets from handing out tokens, and
/// every caller still has its own hold budget above it. Past this the chat is
/// flood-banned rather than throttled, and the reactive ladder — not the pacer
/// — owns the outcome (#1064).
const MAX_429_PAUSE: Duration = Duration::from_secs(45);

/// Arm the per-chat 429 pause on every surface bucket (#635).
///
/// The process-wide cooldown answers "is the bot being throttled"; this answers
/// "is THIS chat being throttled". Both are needed and neither replaces the
/// other: the global deadline is one instant shared by every chat, so a single
/// chat's 429 stalls the whole process, while a chat that keeps firing through
/// its own window is what re-arms the penalty in the first place.
///
/// Always arms the global deadline too — one entry point for "a 429 was learned
/// here", so a caller cannot record the chat and forget the process. DMs
/// (positive ids) are skipped: they are ungoverned by construction, so a pause
/// on one would be state no gate ever reads.
///
/// This does NOT set `forum_seen`. A 429 carries no certified topic id, and the
/// #1708 contract is that only the session topic pipeline may certify a peer as
/// governed. The consequence is bounded: a pause armed before the chat's first
/// admitted call is inert until that call sets the flag, and every path that
/// can learn a 429 has already been admitted at least once.
pub(crate) fn note_429_pause(chat_id: i64, wait: Duration) {
    super::rate_limit::record_global_429(wait);
    if chat_id >= 0 {
        return;
    }
    let wait = wait.min(MAX_429_PAUSE);
    if wait.is_zero() {
        return;
    }
    let until = gate_now() + wait + super::rate_limit::RETRY_MARGIN;
    let mut map = peers_guard();
    let peer = map.entry(chat_id).or_default();
    // Buckets are built if absent: a chat can take its first 429 before any
    // gate has admitted for it (a raw send path, a queued final), and the pause
    // has to be in place by the time the gates do start admitting.
    // `ensure_bucket` carries a live deadline across a shape rebuild, so a
    // placeholder's pause survives the first real admission.
    for slot in [
        &mut peer.typing,
        &mut peer.edits,
        &mut peer.sends_sec,
        &mut peer.sends_min,
        &mut peer.rich,
    ] {
        let bucket = slot.get_or_insert_with(|| Bucket::new(1, 1.0));
        bucket.pause_arm(until);
    }
    peer.counters.pause_armed_429 += 1;
}

/// The cooldown in force for one chat at one instant (#635).
///
/// Before this, "should this chat hold back right now" was answered in three
/// places: the process-wide deadline, the per-chat pause armed by a 429 on the
/// chat's own traffic, and the cosmetic fast-paths — which read the global flag
/// directly and so could never see a per-chat pause. One struct, one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cooldown {
    /// The process-wide 429 deadline, when one is in force.
    pub(crate) global_until: Option<Instant>,
    /// The newest per-chat pause, when one is in force.
    pub(crate) chat_until: Option<Instant>,
}

impl Cooldown {
    /// Whether the PROCESS-WIDE deadline is what holds this chat.
    #[allow(dead_code)]
    pub(crate) fn is_global(self) -> bool {
        self.global_until.is_some()
    }

    /// Whether anything at all holds this chat — the process-wide deadline or
    /// the chat's own pause.
    pub(crate) fn is_any(self) -> bool {
        self.global_until.is_some() || self.chat_until.is_some()
    }
}

/// The one read of "is this chat cooled down right now" (#635).
///
/// `now` is passed in rather than taken here so both halves are judged against
/// the SAME instant — the caller's, not two reads of a clock that can move
/// between them.
pub(crate) fn chat_paused_until(chat_id: i64, now: Instant) -> Cooldown {
    let global_until = match super::rate_limit::global_cooldown_deadline() {
        Some(deadline) if deadline > now => Some(deadline),
        _ => None,
    };
    let chat_until = peers_guard()
        .get_mut(&chat_id)
        .and_then(|peer| peer.paused_until(now));
    Cooldown {
        global_until,
        chat_until,
    }
}

/// Format one peer's summary line. Pure so the field coverage is pinned by a
/// test: adding a counter without extending this format fails the test.
pub(crate) fn format_summary(
    chat_id: i64,
    c: &Counters,
    finals_pending: usize,
    recent: Option<&RecentProfile>,
) -> Option<String> {
    if c.all_zero() && finals_pending == 0 {
        return None;
    }
    // The per-second block is appended LAST so the cumulative groups above
    // keep the exact shape their pinning test asserts.
    let recent_block = recent
        .map(|p| format!(" {}", p.render()))
        .unwrap_or_default();
    Some(format!(
        "Telegram rate-limiter chat={chat_id}: \
         admitted{{typing={},edits={},sends={},rich={}}} \
         dropped{{clock={},brain_preview={},intermediary={},status={},typing={},spacing={}}} \
         finals{{queued={},superseded={},delivered={},failed={},pending={}}} \
         throttled_ms{{typing={},send={},rich={}}} \
         pause{{pause_armed={}}}{recent_block}",
        c.admitted_typing,
        c.admitted_edits,
        c.admitted_sends,
        c.admitted_rich,
        c.dropped_clock,
        c.dropped_brain_preview,
        c.dropped_intermediary,
        c.dropped_status,
        c.dropped_typing,
        c.dropped_spacing,
        c.queued_finals,
        c.superseded_finals,
        c.delivered_finals,
        c.failed_finals,
        finals_pending,
        c.throttled_typing_ms,
        c.throttled_send_ms,
        c.throttled_rich_ms,
        c.pause_armed_429,
    ))
}

/// One summary INFO line per active forum every `[rate_limiter].
/// summary_log_secs`. Spawned lazily by the first gated call.
#[cfg(not(test))]
async fn summary_loop() {
    loop {
        let period = Limits::from_config().summary_log_period;
        tokio::time::sleep(period).await;
        let lines: Vec<String> = {
            let map = peers_guard();
            map.iter()
                .filter_map(|(chat_id, peer)| {
                    let profile = peer.recent.profile(gate_now());
                    format_summary(*chat_id, &peer.counters, peer.finals.len(), Some(&profile))
                })
                .collect()
        };
        for line in lines {
            tracing::info!("{line}");
        }
    }
}

fn ensure_summary_task() {
    // Tests run under tokio's paused clock: the periodic summary sleep would
    // auto-advance instantly and busy-spin the registry lock between virtual
    // refills. The summary format is pinned by its own unit test instead.
    #[cfg(test)]
    {
        // Read the knob rather than discarding the whole struct, so the field
        // is exercised in this cfg too instead of reading as dead.
        let _ = Limits::from_config().summary_log_period;
    }
    #[cfg(not(test))]
    {
        static STARTED: OnceLock<()> = OnceLock::new();
        STARTED.get_or_init(|| {
            tokio::spawn(summary_loop());
        });
    }
}

// ---------------------------------------------------------------------------
// G1 — typing
// ---------------------------------------------------------------------------

/// G1 gate for `sendChatAction`. Returns true when the caller must fire the
/// action now; false when the refresh was collapsed away (cosmetic loss, the
/// next tick re-fires). Hold-and-release under pressure: a caller whose wait
/// still fits the hold budget sleeps for the refill window instead of
/// retry-spinning into the same bucket, which is what amplified 429 storms.
pub(crate) async fn admit_chat_action(chat: ChatId, thread_id: Option<i32>) -> bool {
    // Fast-path: a 429 in force — process-wide, or armed on this chat's own
    // traffic — drops cosmetic typing refreshes immediately (#635). Read
    // through the one `Cooldown` accessor so a per-chat pause counts here
    // exactly as it does inside the engine.
    if chat_paused_until(chat.0, gate_now()).is_any() {
        return false;
    }

    let chat_id = chat.0;
    // Positive ids are DMs — untouched by construction, no matter what a
    // future call site passes. Cheap exit before touching config.
    if chat_id >= 0 {
        return true;
    }
    let lim = Limits::from_config();
    if !lim.enabled {
        return true;
    }
    ensure_summary_task();
    let mut waited = Duration::ZERO;
    loop {
        let decision = pace_engine(
            &SurfacePolicy::TYPING,
            &lim,
            chat_id,
            thread_id,
            waited,
            |peer, now, _spacing_hold| {
                // #676 — typing is EXEMPT from the spacing floor. Measured: zero
                // typing refusals across 8 days of logs while typing sits pinned
                // at its own ~20/min ceiling, so Telegram does not meter it like
                // a message (owner ruling 2026-09-08: the limits are per-surface,
                // and the offending arm is the evidence). Two tests also pin this
                // bucket — not spacing — as the gate governing the shared typing
                // budget, which a floor would break.
                let bucket = ensure_bucket(
                    &mut peer.typing,
                    lim.typing_burst,
                    1.0 / lim.typing_interval.as_secs_f64(),
                );
                match bucket.take(now) {
                    Ok(()) => {
                        peer.counters.admitted_typing += 1;
                        peer.recent.push(now, SURFACE_TYPING);
                        PaceOutcome::Admit
                    }
                    // The hold budget (`typing_max_hold`) is applied by the
                    // engine, which is the one place that knows how long this
                    // attempt has already held.
                    Err(wait) => PaceOutcome::Defer(wait),
                }
            },
        );
        match decision {
            // `Bypass` is the engine's ungoverned-peer answer, and a typing
            // refresh for an ungoverned chat fires exactly as it always did.
            PaceOutcome::Bypass | PaceOutcome::Admit => {
                fold_hold_ms(chat_id, waited, HoldSurface::Typing);
                return true;
            }
            PaceOutcome::Drop(_) => {
                tracing::debug!(
                    "Telegram rate-limiter: typing refresh dropped for chat={chat_id} after \
                     holding {waited:?} (hold cap {}s)",
                    lim.typing_max_hold.as_secs()
                );
                fold_hold_ms(chat_id, waited, HoldSurface::Typing);
                return false;
            }
            PaceOutcome::Defer(wait) => {
                #[cfg(not(test))]
                let start = gate_now();
                tokio::time::sleep(wait).await;
                // Under tokio's paused runtime the sleep above returns
                // instantly, so the virtual clock has to absorb it or the
                // hold accounting never moves and this loop cannot reach its
                // bound. A test build then credits the scripted delay exactly
                // instead of measuring the sleep: the sleeper is resumed by a
                // real worker thread, so a wall-clock difference taken across
                // it carries host scheduling jitter into a ledger the band
                // assertions pin to the millisecond (#2076).
                #[cfg(test)]
                test_support::advance(wait.as_millis() as u64);
                #[cfg(test)]
                let elapsed = wait;
                #[cfg(not(test))]
                let elapsed = gate_now().duration_since(start);
                waited += elapsed;
            }
            // G1 has neither a queue nor a fail-open arm; the policy table says
            // so. Reaching here would mean the table and the gate disagree.
            PaceOutcome::Queue | PaceOutcome::FailOpen(_) => {
                fold_hold_ms(chat_id, waited, HoldSurface::Typing);
                return true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// G2 — edits
// ---------------------------------------------------------------------------

/// Which flow-surface edit is asking to go out. Discriminant order IS the
/// drop ladder from #1211: on an empty edit bucket, lower ranks drop first
/// because they self-heal on the next full-state refresh; finals queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditClass {
    /// Elapsed-clock chrome ticks — pure cosmetics, re-rendered next tick.
    Clock,
    /// Streaming/thinking preview churn (the ▋ cursor placeholder).
    BrainPreview,
    /// Mid-turn tool/narration appends folded into the processing log.
    Intermediary,
    /// Live status render of the open flow block (open/refresh on activity).
    Status,
    /// Settle renders and plan-card refreshes — NEVER dropped, queued
    /// latest-wins per message id until the edit bucket refills.
    Final,
    /// User-initiated interactive UI edits (e.g. menu navigation in /cd, /models)
    /// or tap consequences — NEVER dropped, NEVER queued. Admitted directly
    /// or via reserved floor, falling back to direct send or reactive retry.
    Interactive,
}

impl EditClass {
    /// Ladder rank: higher drops later. Kept explicit so a test pins the
    /// ordering the issue locks in, even if variants reorder.
    pub(crate) fn drop_rank(self) -> u8 {
        match self {
            EditClass::Clock => 0,
            EditClass::BrainPreview => 1,
            EditClass::Intermediary => 2,
            EditClass::Status => 3,
            EditClass::Final => 4,
            EditClass::Interactive => 5,
        }
    }

    /// True for the chrome classes the ladder sheds when budget runs out
    /// (#1927). The two never-dropped classes are `Final` (queues instead)
    /// and `Interactive` (user-initiated, admitted directly) — so the
    /// cross-surface floor, which is also a drop path, may only shed the rest.
    pub(crate) fn is_droppable(self) -> bool {
        !matches!(self, EditClass::Final | EditClass::Interactive)
    }
}

/// G2 gate for `editMessageText`. Returns true when the caller must perform
/// its edit now; false when the governor handled it (dropped chrome, or the
/// payload was queued as a final). `html`/`rich` describe the payload so a
/// queued final can be executed verbatim by the drainer without the caller
/// staying alive.
pub(crate) async fn edit_admission(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    class: EditClass,
    html: String,
    rich: bool,
) -> bool {
    edit_admission_media_kb(
        bot,
        chat_id,
        msg_id,
        class,
        html,
        rich,
        Vec::new(),
        None,
        FinalDialect::Html,
    )
    .await
}

/// Media-and-keyboard-bearing variant of [`edit_admission`] (#134, #155, #229):
/// preserves rendered-mermaid [`super::rich::mermaid::MediaEntry`]s, optional `reply_markup`, and the
/// payload [`FinalDialect`] across queued final drains so rate-governed plan card
/// and rich edits do not strip media, lose keyboards, or corrupt formatting dialects.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn edit_admission_media_kb(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    class: EditClass,
    html: String,
    rich: bool,
    media: Vec<super::rich::mermaid::MediaEntry>,
    reply_markup: Option<serde_json::Value>,
    dialect: FinalDialect,
) -> bool {
    // Fast-path: a 429 in force — process-wide, or armed on this chat's own
    // traffic — drops intermediate cosmetic edits immediately (#635), read
    // through the same `Cooldown` accessor the typing fast-path uses.
    if chat_paused_until(chat_id.0, gate_now()).is_any()
        && class != EditClass::Final
        && class != EditClass::Interactive
    {
        return false;
    }

    // DMs untouched (positive ids), matching the G1 scope guard.
    if chat_id.0 >= 0 {
        return true;
    }
    let lim = Limits::from_config();
    if !lim.enabled {
        return true;
    }
    ensure_summary_task();
    // One pass, no loop: G2 answers in one shot. `Duration::ZERO` is the hold
    // already absorbed, which is always nothing for a surface that never waits.
    let admission = pace_engine(
        &SurfacePolicy::EDITS,
        &lim,
        chat_id.0,
        None,
        Duration::ZERO,
        |peer, now, spacing_hold| {
            // #676 — the spacing floor governs DROPPABLE chrome only: an
            // in-interval cosmetic tick is refused before it can spend a token,
            // while content falls through to the arms below untouched. Content
            // is NEVER floored — it is already paced by the bucket.
            let droppable = class.is_droppable();
            if droppable && !spacing_hold.is_zero() {
                peer.counters.note_spacing_drop();
                return PaceOutcome::Drop(DropReason::SpacingFloor);
            }
            let bucket = ensure_bucket(&mut peer.edits, lim.edit_burst, lim.edit_rate_per_sec);
            if bucket.take(now).is_ok() {
                peer.counters.admitted_edits += 1;
                // A rich-API edit is metered by G4, not here; recording it as
                // an EDITS admission would double-count it in the ring.
                if !rich {
                    peer.recent.push(now, SURFACE_EDITS);
                }
                PaceOutcome::Admit
            } else if class == EditClass::Interactive {
                // Interactive UI edits never drop and never queue; pass through immediately
                if !rich {
                    peer.recent.push(now, SURFACE_EDITS);
                }
                PaceOutcome::Admit
            } else if class == EditClass::Final {
                let superseded = peer
                    .finals
                    .insert(
                        msg_id.0,
                        PendingFinal {
                            bot: bot.clone(),
                            html,
                            rich,
                            dialect,
                            media,
                            reply_markup,
                            attempts: 0,
                        },
                    )
                    .is_some();
                if superseded {
                    peer.counters.superseded_finals += 1;
                }
                peer.counters.queued_finals += 1;
                PaceOutcome::Queue
            } else {
                peer.counters.note_drop(class);
                PaceOutcome::Drop(DropReason::BucketDry)
            }
        },
    );
    match admission {
        // `Bypass` is the ungoverned peer: edits rarely carry a topic id, so a
        // peer reached here without any topic-scoped call yet (e.g. right after
        // restart) stays ungoverned until G1 or a topic-scoped call marks it.
        PaceOutcome::Bypass | PaceOutcome::Admit => true,
        PaceOutcome::Drop(_) => {
            tracing::debug!(
                "Telegram rate-limiter: {:?} edit (rank {}) dropped for chat={} msg={} — \
                 next full-state refresh carries it",
                class,
                class.drop_rank(),
                chat_id.0,
                msg_id.0
            );
            false
        }
        PaceOutcome::Queue => {
            tracing::debug!(
                "Telegram rate-limiter: final edit queued latest-wins for chat={} msg={}",
                chat_id.0,
                msg_id.0
            );
            ensure_drain(chat_id.0);
            false
        }
        // G2 answers in one shot: no wait loop, no fail-open arm.
        PaceOutcome::Defer(_) | PaceOutcome::FailOpen(_) => true,
    }
}

/// Take ownership of one queued final when the edit bucket allows it.
/// Returns `None` on "nothing to do this tick" (no peer, empty queue, or no
/// edit budget yet).
fn take_due_final(chat_id: i64, lim: &Limits) -> Option<(i32, PendingFinal)> {
    let mut map = peers_guard();
    let peer = map.get_mut(&chat_id)?;
    if peer.finals.is_empty() {
        peer.draining = false;
        return None;
    }
    let bucket = ensure_bucket(&mut peer.edits, lim.edit_burst, lim.edit_rate_per_sec);
    if bucket.take(gate_now()).is_err() {
        return None;
    }
    let key = peer.finals.keys().next().copied()?;
    peer.finals.remove(&key).map(|pending| (key, pending))
}

/// Flush queued finals for one peer as the edit bucket refills. Exits when
/// the queue runs dry (clearing `draining` so a later enqueue respawns it).
async fn drain_finals(chat_id: i64) {
    loop {
        tokio::time::sleep(DRAIN_TICK).await;
        // Same clock divergence as the gate loops: under the paused runtime
        // this returns instantly, so without absorbing it the edit bucket
        // never refills and a queued final is never drained.
        #[cfg(test)]
        test_support::advance(DRAIN_TICK.as_millis() as u64);

        let lim = Limits::from_config();
        let Some(job) = take_due_final(chat_id, &lim) else {
            // Queue drained (draining flag cleared inside) or no budget yet.
            let empty = peers_guard()
                .get(&chat_id)
                .is_none_or(|p| p.finals.is_empty());
            if empty {
                return;
            }
            continue;
        };
        deliver_final(chat_id, job.0, job.1).await;
    }
}

/// Respawn the drainer for a peer exactly when a first final appears and no
/// drainer is running; the task itself retires when the queue drains.
fn ensure_drain(chat_id: i64) {
    let should_spawn = {
        let mut map = peers_guard();
        let peer = map.entry(chat_id).or_default();
        if peer.draining || peer.finals.is_empty() {
            false
        } else {
            peer.draining = true;
            true
        }
    };
    if should_spawn {
        tokio::spawn(drain_finals(chat_id));
    }
}

/// Errors that mean a queued final will NEVER land, no matter how often the
/// drainer retries: the target is gone or the content is already there.
pub(crate) fn is_permanent_edit_error(error: &str) -> bool {
    error.contains("message to edit not found")
        || error.contains("message is not modified")
        || error.contains("message can't be edited")
        || error.contains("MESSAGE_ID_INVALID")
        || error.contains("chat not found")
}

enum Verdict {
    Delivered,
    Abandoned(String),
    Retried,
}

/// Execute one queued final outside the registry lock, then record the
/// outcome. Transient failures requeue with their attempt budget decremented;
/// permanent ones abandon with a warn so a dead target cannot wedge the
/// queue forever (finals are never dropped at ADMISSION — wire failures are
/// a different failure mode with their own telemetry).
async fn deliver_final(chat_id: i64, msg_id: i32, mut pending: PendingFinal) {
    let result = run_final_edit(chat_id, msg_id, &pending).await;
    let retry_after = match &result {
        Err(e) => super::rate_limit::parse_retry_after(e),
        Ok(()) => None,
    };
    // A queued final that came back 429 is the same signal as any other, and it
    // is about THIS chat as much as the process: arm both before the per-peer
    // requeue maths, so this chat's own buckets stop spending and other chats
    // pause instead of walking into the same throttle (#635).
    if let Some(wait) = retry_after {
        note_429_pause(chat_id, wait);
    }
    let verdict = {
        let mut map = peers_guard();
        let Some(peer) = map.get_mut(&chat_id) else {
            // The peer vanished between pop and verdict: no counter will ever
            // move for this final. Still fire the settle hook so a parked
            // converge loop wakes to observe the 0/0/0 shape instead of
            // waiting out its watchdog (#28 mode 4).
            #[cfg(test)]
            test_support::notify_settled();
            return;
        };
        match result {
            Ok(()) => {
                peer.counters.delivered_finals += 1;
                Verdict::Delivered
            }
            Err(e) => {
                pending.attempts += 1;
                if pending.attempts >= FINAL_MAX_ATTEMPTS || is_permanent_edit_error(&e) {
                    peer.counters.failed_finals += 1;
                    Verdict::Abandoned(e)
                } else {
                    peer.finals.insert(msg_id, pending);
                    Verdict::Retried
                }
            }
        }
    };
    match verdict {
        Verdict::Delivered => {
            tracing::debug!(
                "Telegram rate-limiter: queued final delivered chat={chat_id} msg={msg_id}"
            );
        }
        Verdict::Abandoned(e) => {
            tracing::warn!(
                "Telegram rate-limiter: queued final abandoned after {FINAL_MAX_ATTEMPTS} \
                 attempts chat={chat_id} msg={msg_id}: {e}"
            );
        }
        Verdict::Retried => {
            // Respect a 429 window if that is what bounced us, capped inline
            // like every other send path (#1064), then let the tick cadence
            // handle plain transients.
            if let Some(window) = retry_after {
                let (wait, _) = super::rate_limit::clamp_inline_wait(window);
                tokio::time::sleep(wait).await;
                // Under tokio's paused runtime the sleep above returns
                // instantly, so the virtual clock has to absorb it or the
                // hold accounting never moves and this loop cannot reach its
                // bound. Production reads the same real clock for both.
                #[cfg(test)]
                test_support::advance(wait.as_millis() as u64);
            }
        }
    }
    // Test hook (#28 mode 4): every verdict exit ends the silence — wake a
    // parked converge loop. Precedent: the `test_support::advance` hooks in
    // this file for the paused-clock sleeps.
    #[cfg(test)]
    test_support::notify_settled();
}

/// The wire shapes a queued final can take, mirroring the call sites that
/// produced the payload: rich HTML, classic HTML edit, or the markdown+media
/// rich edit (plan card, #134, #229), where `html` carries the raw markdown body.
async fn run_final_edit(chat_id: i64, msg_id: i32, pending: &PendingFinal) -> Result<(), String> {
    match (pending.rich, pending.dialect) {
        (true, FinalDialect::Markdown) => super::rich::api::edit_rich_markdown_media(
            pending.bot.api_url().as_str(),
            pending.bot.token(),
            chat_id,
            msg_id,
            &pending.html,
            &pending.media,
            pending.reply_markup.as_ref(),
            "turn",
            "-",
        )
        .await
        .map_err(|e| e.to_string()),
        (true, FinalDialect::Html) => super::rich::api::edit_rich_html(
            pending.bot.api_url().as_str(),
            pending.bot.token(),
            chat_id,
            msg_id,
            &pending.html,
            pending.reply_markup.as_ref(),
            "turn",
            "-",
        )
        .await
        .map_err(|e| e.to_string()),
        (false, _) => {
            use teloxide::payloads::EditMessageTextSetters;
            let mut req = pending
                .bot
                .edit_message_text(ChatId(chat_id), MessageId(msg_id), &pending.html)
                .parse_mode(ParseMode::Html);
            if let Some(inline_kb) = pending.reply_markup.as_ref().and_then(|kb| {
                serde_json::from_value::<teloxide::types::InlineKeyboardMarkup>(kb.clone()).ok()
            }) {
                req = req.reply_markup(inline_kb);
            }
            req.await.map(|_| ()).map_err(|e| e.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// G3 — send pacing
// ---------------------------------------------------------------------------

/// G3 gate ahead of full-message sends. Two AND-ed buckets: ~1/s spacing and
/// an ~18/min group ceiling (both configurable). Holds the caller just long
/// enough to buy a token pair, never drops, and fails open past
/// [`SEND_MAX_HOLD`] so a pathological configuration degrades to today's
/// behavior instead of stalling turns.
pub(crate) async fn pace_send(chat: ChatId) {
    let chat_id = chat.0;
    // DMs (positive ids) untouched, cheap exit first.
    if chat_id >= 0 {
        return;
    }
    // Proactive global limiter & global 429 lock. Above the per-chat gate on
    // purpose: `limits.enabled` is a per-chat pacing policy, while the global
    // permit is the process-wide ceiling and the shared 429 cooldown. Below
    // the gate, switching the per-chat pacer off would also delete the safety
    // floor and let every chat keep firing straight through a live cooldown.
    acquire_global_permit().await;

    let lim = Limits::from_config();
    if !lim.enabled {
        return;
    }

    let mut waited = Duration::ZERO;
    loop {
        let verdict = pace_engine(
            &SurfacePolicy::SENDS,
            &lim,
            chat_id,
            None,
            waited,
            |peer, now, spacing_hold| {
                // #676 - a plain send is content, so the spacing floor is
                // WAITED out rather than dropped; the loop's existing
                // SEND_MAX_HOLD arm bounds it exactly like a bucket hold. The
                // wait is the REMAINDER of the interval, not the whole floor.
                let sec = ensure_bucket(
                    &mut peer.sends_sec,
                    lim.send_burst,
                    1.0 / lim.send_interval.as_secs_f64(),
                );
                let min = ensure_bucket(
                    &mut peer.sends_min,
                    lim.send_minute_ceiling,
                    f64::from(lim.send_minute_ceiling) / 60.0,
                );
                let need = sec
                    .next_token_in(now)
                    .max(min.next_token_in(now))
                    .max(spacing_hold);
                if need.is_zero() {
                    let _ = sec.take(now);
                    let _ = min.take(now);
                    peer.counters.admitted_sends += 1;
                    peer.recent.push(now, SURFACE_SENDS);
                    PaceOutcome::Admit
                } else {
                    // `SEND_MAX_HOLD` is applied by the engine, which owns the
                    // fail-open cell: past the cap this becomes `FailOpen(need)`
                    // and the send goes out anyway (#297).
                    PaceOutcome::Defer(need)
                }
            },
        );
        match verdict {
            PaceOutcome::Bypass => return,
            PaceOutcome::Admit => {
                fold_hold_ms(chat_id, waited, HoldSurface::Sends);
                return;
            }
            PaceOutcome::FailOpen(next) => {
                tracing::warn!(
                    "Telegram rate-limiter: send pacing held {waited:?} for chat={chat_id}, \
                     still {} from a token — failing open to the reactive backstop",
                    next.as_secs_f64()
                );
                fold_hold_ms(chat_id, waited, HoldSurface::Sends);
                return;
            }
            PaceOutcome::Defer(delay) => {
                #[cfg(not(test))]
                let start = gate_now();
                tokio::time::sleep(delay).await;
                // Under tokio's paused runtime the sleep above returns
                // instantly, so the virtual clock has to absorb it or the
                // hold accounting never moves and this loop cannot reach its
                // bound. A test build then credits the scripted delay exactly
                // instead of measuring the sleep: the sleeper is resumed by a
                // real worker thread, so a wall-clock difference taken across
                // it carries host scheduling jitter into a ledger the band
                // assertions pin to the millisecond (#2076).
                #[cfg(test)]
                test_support::advance(delay.as_millis() as u64);
                #[cfg(test)]
                let elapsed = delay;
                #[cfg(not(test))]
                let elapsed = gate_now().duration_since(start);
                waited += elapsed;
            }
            // G3 is delay-never-drop and has no queue.
            PaceOutcome::Drop(_) | PaceOutcome::Queue => {
                fold_hold_ms(chat_id, waited, HoldSurface::Sends);
                return;
            }
        }
    }
}

/// G4 gate ahead of rich-endpoint calls (`sendRichMessage` and its edit
/// sibling), which sit in their own method family with their own budget.
///
/// G1/G2/G3 govern `sendChatAction`, `editMessageText` and `sendMessage`.
/// Telegram meters the rich endpoint separately, so none of those buckets
/// sees its traffic — a deployment can therefore take zero typing 429s while
/// the rich endpoint 429s hundreds of times a day. Measured on one: 260 real
/// 429s in a day against 7,891 rich edits and 399 rich sends, each costing a
/// 17-24 s `retry_after`, roughly 87 minutes of stalled flow rendering. Those
/// 429s cluster in the p99 minutes (40-46 calls) while the median minute runs
/// 18, which is what the 30/min default is sized against.
///
/// Holds rather than drops, like [`pace_send`]: a rich call is content, and
/// the reactive `wait_out` backstop still owns anything that gets through.
/// Unlike [`pace_send`], `pace_rich` never fails open past [`SEND_MAX_HOLD`]:
/// rich calls wait until a token is refilled so they never fire unadmitted into
/// Telegram to trigger server-level 429 lockouts across the chat.
pub(crate) async fn pace_rich(chat: ChatId, thread_id: Option<i32>) {
    let chat_id = chat.0;
    // DMs (positive ids) untouched, cheap exit first.
    if chat_id >= 0 {
        return;
    }
    // Proactive global limiter & global 429 lock. Above the per-chat gate on
    // purpose: `limits.enabled` is a per-chat pacing policy, while the global
    // permit is the process-wide ceiling and the shared 429 cooldown. Below
    // the gate, switching the per-chat pacer off would also delete the safety
    // floor and let every chat keep firing straight through a live cooldown.
    acquire_global_permit().await;

    let lim = Limits::from_config();
    if !lim.enabled {
        return;
    }

    ensure_summary_task();
    let mut waited = Duration::ZERO;
    loop {
        let verdict = pace_engine(
            &SurfacePolicy::RICH,
            &lim,
            chat_id,
            thread_id,
            waited,
            |peer, now, spacing_hold| {
                // The rich path carries the topic itself, so it can establish
                // forums-only rollout on its own rather than waiting for a typing
                // gate to have run first for this chat. Contract (#1708): this id
                // is session-certified upstream; a raw reply-chain id from an
                // ordinary group must never reach here (scan-test enforced).
                let bucket = ensure_bucket(&mut peer.rich, lim.rich_burst, lim.rich_rate_per_sec);
                // The cross-surface floor joins the rich bucket as a second,
                // AND-ed constraint: a rich edit WAITS out the floor remainder
                // rather than firing inside it (landed g4 rich-floor test).
                let need = bucket.next_token_in(now).max(spacing_hold);
                if need.is_zero() {
                    let _ = bucket.take(now);
                    peer.counters.admitted_rich += 1;
                    peer.recent.push(now, SURFACE_RICH);
                    PaceOutcome::Admit
                } else {
                    // `HoldBudget::None` on the rich row is no CAP, not no
                    // waiting: the engine passes this `Defer` straight back and
                    // the loop below sleeps it, so rich content waits until a
                    // token refills and never fires unadmitted.
                    PaceOutcome::Defer(need)
                }
            },
        );
        match verdict {
            // `Bypass` is the ungoverned peer, `Admit` a bought token: both mean
            // the rich call may go out now.
            PaceOutcome::Bypass | PaceOutcome::Admit => {
                fold_hold_ms(chat_id, waited, HoldSurface::Rich);
                return;
            }
            PaceOutcome::Defer(delay) => {
                #[cfg(not(test))]
                let start = gate_now();
                tokio::time::sleep(delay).await;
                // Under tokio's paused runtime the sleep above returns
                // instantly, so the virtual clock has to absorb it or the
                // hold accounting never moves and this loop cannot reach its
                // bound. A test build then credits the scripted delay exactly
                // instead of measuring the sleep: the sleeper is resumed by a
                // real worker thread, so a wall-clock difference taken across
                // it carries host scheduling jitter into a ledger the band
                // assertions pin to the millisecond (#2076).
                #[cfg(test)]
                test_support::advance(delay.as_millis() as u64);
                #[cfg(test)]
                let elapsed = delay;
                #[cfg(not(test))]
                let elapsed = gate_now().duration_since(start);
                waited += elapsed;
            }
            // G4 is delay-never-drop and has no fail-open cap: none of these
            // arms is reachable from the rich row, so end the call like a wait.
            PaceOutcome::Drop(_) | PaceOutcome::Queue | PaceOutcome::FailOpen(_) => {
                fold_hold_ms(chat_id, waited, HoldSurface::Rich);
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test support (cfg(test)) — shared with src/tests/governor_gates_test.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    use std::sync::atomic::Ordering;

    /// Serialize every test that touches the global peer registry, the
    /// virtual clock or the process-wide config mirror. The gates are built
    /// on process singletons by design (one bot token per instance), so the
    /// tests that exercise them take this guard first.
    pub(crate) async fn registry_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        GUARD.lock().await
    }

    /// Wipe all peer state and pin the virtual clock at `offset_ms`.
    pub(crate) fn reset(offset_ms: u64) {
        peers_guard().clear();
        *GLOBAL_PACER.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::channels::telegram::rate_limit::reset_global_cooldown();
        CLOCK_OFFSET_MS.store(offset_ms, Ordering::Relaxed);
    }

    /// Advance the virtual clock: bucket refills, hold budgets and drain
    /// cadence observe the jump on their next `gate_now` evaluation. No real
    /// sleeping anywhere — mocked passage of time per the #1211 test brief.
    pub(crate) fn advance(ms: u64) {
        CLOCK_OFFSET_MS.fetch_add(ms, Ordering::Relaxed);
    }

    /// Process-wide settle notifier for the drainer wire tests (#28 mode 4).
    /// `deliver_final` fires [`notify_settled`] on EVERY exit (delivered,
    /// abandoned, retried, and the peer-vanished early return); a converge
    /// loop parks on `settle_notifier().notified()` instead of pumping
    /// virtual-time sleeps. With no tokio timer left pending in the test
    /// task, the paused runtime performs a REAL reactor park, so the
    /// in-flight mockito response lands in real time and the drainer's
    /// verdict wakes the loop — the wait is event-driven, not a lottery.
    pub(crate) fn settle_notifier() -> &'static tokio::sync::Notify {
        static SETTLED: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
        SETTLED.get_or_init(tokio::sync::Notify::new)
    }

    /// Wake any converge loop parked on [`settle_notifier`]. `notify_one`
    /// permit semantics: a notify with no waiter stores one permit that the
    /// next `notified()` consumes — at worst one spurious re-check (the loop
    /// always re-reads the settle counters before parking again), never a
    /// lost wakeup.
    pub(crate) fn notify_settled() {
        settle_notifier().notify_one();
    }

    /// Mark a chat as an already-seen forum peer WITHOUT consuming budget,
    /// so a test controls exactly how many tokens each bucket starts with.
    pub(crate) fn mark_forum(chat: ChatId) {
        let chat_id = chat.0;
        peers_guard().entry(chat_id).or_default().forum_seen = true;
    }

    /// Empty one of a peer's buckets through the SAME refill/take math the
    /// gates use (`kind`: "typing" | "edits" | "sends_sec" | "sends_min").
    pub(crate) fn burn_bucket(chat: ChatId, kind: BucketKind, capacity: u32, rate_per_sec: f64) {
        let chat_id = chat.0;
        let mut map = peers_guard();
        let peer = map.entry(chat_id).or_default();
        peer.forum_seen = true;
        let slot = match kind {
            BucketKind::Typing => &mut peer.typing,
            BucketKind::Edits => &mut peer.edits,
            BucketKind::SendsSec => &mut peer.sends_sec,
            BucketKind::SendsMin => &mut peer.sends_min,
            BucketKind::Rich => &mut peer.rich,
        };
        let bucket = ensure_bucket(slot, capacity, rate_per_sec);
        for _ in 0..capacity {
            let _ = bucket.take(gate_now());
        }
    }

    /// Which of a peer's buckets [`burn_bucket`] empties.
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum BucketKind {
        Typing,
        Edits,
        SendsSec,
        SendsMin,
        Rich,
    }

    /// Point-in-time copy of everything a test can assert on for one peer.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(crate) struct Snap {
        pub forum_seen: bool,
        pub typing_admitted: u64,
        pub typing_dropped: u64,
        pub edits_admitted: u64,
        pub dropped_clock: u64,
        pub dropped_brain_preview: u64,
        pub dropped_intermediary: u64,
        pub dropped_status: u64,
        /// Admissions shed by the cross-surface spacing floor (#1927).
        pub dropped_spacing: u64,
        pub queued_finals: u64,
        pub superseded_finals: u64,
        pub delivered_finals: u64,
        pub failed_finals: u64,
        pub throttled_typing_ms: u64,
        pub admitted_rich: u64,
        pub admitted_sends: u64,
        pub throttled_rich_ms: u64,
        pub throttled_send_ms: u64,
        pub finals_pending: usize,
        pub pause_armed_429: u64,
    }

    /// Read [`Snap`] for `chat_id`; `None` when no gate has touched the peer.
    pub(crate) fn snapshot(chat: ChatId) -> Option<Snap> {
        let chat_id = chat.0;
        let map = peers_guard();
        map.get(&chat_id).map(|p| Snap {
            forum_seen: p.forum_seen,
            typing_admitted: p.counters.admitted_typing,
            typing_dropped: p.counters.dropped_typing,
            edits_admitted: p.counters.admitted_edits,
            dropped_clock: p.counters.dropped_clock,
            dropped_brain_preview: p.counters.dropped_brain_preview,
            dropped_intermediary: p.counters.dropped_intermediary,
            dropped_status: p.counters.dropped_status,
            dropped_spacing: p.counters.dropped_spacing,
            queued_finals: p.counters.queued_finals,
            superseded_finals: p.counters.superseded_finals,
            delivered_finals: p.counters.delivered_finals,
            failed_finals: p.counters.failed_finals,
            throttled_typing_ms: p.counters.throttled_typing_ms,
            admitted_rich: p.counters.admitted_rich,
            admitted_sends: p.counters.admitted_sends,
            throttled_rich_ms: p.counters.throttled_rich_ms,
            throttled_send_ms: p.counters.throttled_send_ms,
            finals_pending: p.finals.len(),
            pause_armed_429: p.counters.pause_armed_429,
        })
    }
}
