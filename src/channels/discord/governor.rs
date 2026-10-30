//! Outbound write governor for Discord (#1910).
//!
//! Telegram ships an explicit product-level policy layer
//! ([`crate::channels::telegram::governor`]); WhatsApp ships the simple shape
//! of one ([`crate::channels::whatsapp::rate_limit`], production-proven since
//! #1407). Discord had neither: the only protection against an edit storm was
//! serenity's built-in per-bucket ratelimiter, which is transport hygiene -
//! it queues the request, it does not know that this request is the fourth
//! re-render of a bubble nobody has read yet. Silent, too: a 429 here logged
//! as a generic failure with nothing separating "the API told us to slow
//! down" from "the message was deleted under us".
//!
//! Two surfaces make this acute: the tool-group bubble and the 4 s flow
//! ticker (#1843) that keeps rewriting its clock. A turn with eight tool
//! calls and a long-running command is one message edited every few seconds,
//! all of it cosmetic, all of it competing with the content that has to land
//! at settle. The governor's job is to make that competition ordered:
//!
//! - **Content is delay-never-drop** ([`Surface::Send`], [`Surface::Final`]).
//!   A ceiling breach holds the write until the budget refills, exactly like
//!   Telegram's G3 sends (#297). Past [`Limits::content_max_hold`] it fails
//!   open and lets the write go anyway: a late answer beats a lost one, and
//!   serenity's limiter plus a real 429 are still downstream.
//! - **Chrome yields** ([`Surface::Chrome`]). The ticker's clock line and the
//!   waiting-line refresh carry no information a later edit will not restate
//!   (every re-render paints the FULL current state), so a chrome write is
//!   DROPPED the moment it is refused: inside the spacing window, over the
//!   ceiling, or while the channel is parked. It never waits. That is
//!   Telegram's G2 drop ladder reduced to the one class Discord actually
//!   has.
//! - **A 429 is evidence, and the only real budget we are ever given.**
//!   Discord publishes no fixed per-route numbers and its docs forbid
//!   hardcoding them, so when a response names a `retry_after` window THIS
//!   layer honors it: the channel parks until the window closes and the
//!   ticker stops editing while it is parked. The config numbers are safety
//!   ceilings around that, nothing more.
//!
//! Like the WhatsApp limiter, all the math is synchronous and I/O-free in
//! [`admit`] / [`note_429`], with the clock injected as `now: Instant`, so
//! tests advance time by hand instead of sleeping. [`Governor::gate`] is the
//! thin async shell the call sites use.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Config;

/// Process-wide virtual clock offset, `cfg(test)` only. The governor reads
/// [`now`] everywhere instead of `Instant::now()`, so a test can advance time
/// by hand and watch a park expire or a bucket refill without sleeping. Same
/// trick as Telegram's `gate_now`, same reason: the suite must not depend on
/// wall-clock luck.
#[cfg(test)]
static CLOCK_OFFSET_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The clock every governor path reads: the monotonic instant in a normal
/// build, the offset one under `cfg(test)`.
#[cfg(not(test))]
fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
fn now() -> Instant {
    use std::sync::atomic::Ordering;
    let off = CLOCK_OFFSET_MS.load(Ordering::Relaxed);
    Instant::now() + Duration::from_millis(off)
}

/// Which kind of write is asking. The class decides what a dry budget does to
/// it; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// A brand-new message: the answer, a notice, a voice reply.
    Send,
    /// The last word on a turn: the settled tool-group line, a final edit.
    Final,
    /// A live re-render of state already shown: the flow ticker's clock, the
    /// waiting-line refresh. Safe to drop; the next paint restates it.
    Chrome,
}

impl Surface {
    /// True when a breach must never lose the payload.
    pub(crate) fn is_content(self) -> bool {
        matches!(self, Surface::Send | Surface::Final)
    }
}

/// What [`admit`] decided for one write.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Take the write now: the budget was charged.
    Admit,
    /// Content: sleep `wait`, then re-admit. Chrome never waits, it drops.
    Hold(Duration),
    /// Chrome on a dry or parked budget: discard this re-render. Nothing was
    /// charged and nothing is lost - the next tick paints the same state.
    Drop,
    /// The hold ceiling was reached and the write goes out anyway, uncharged.
    /// The gate logs this one: a content write that outran its own hold budget
    /// is the signal that the ceiling or the API is misbehaving, and it is
    /// rare enough to be worth a line every time.
    FailOpen,
}

/// Effective knobs, read from `[discord.governor]` at each gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Master switch. Off means every gate answers [`Decision::Admit`] and
    /// records nothing: pre-#1910 behaviour, one config line away.
    pub enabled: bool,
    /// Rolling per-channel write ceiling (all surfaces together) and its
    /// burst capacity. `ceiling == 0` disables the ceiling; `burst` is then
    /// irrelevant.
    pub ceiling_per_min: u32,
    pub burst: u32,
    /// Minimum spacing between two admitted chrome writes on one channel.
    /// Chrome never waits: inside this window the re-render is dropped, which
    /// is the same self-heal Telegram's G2 ladder relies on - the next paint
    /// restates whatever this one would have said.
    pub chrome_spacing: Duration,
    /// Longest a CONTENT write may be held before it fails open.
    pub content_max_hold: Duration,
    /// Fallback park window for a 429 that named no `retry_after`.
    pub pause_fallback: Duration,
    /// Safety ceiling on any park window, named or fallback.
    pub pause_ceiling: Duration,
}

impl Limits {
    /// Snapshot the live config. Mirrors Telegram's
    /// [`crate::channels::telegram::governor`] `Limits::from_config`: durations
    /// built from the ms/s knobs, ceilings clamped so a zero cannot divide.
    fn from_config() -> Self {
        let g = &Config::current().channels.discord.governor;
        Self {
            enabled: g.enabled,
            ceiling_per_min: g.writes_per_minute,
            burst: g.burst.max(1),
            chrome_spacing: Duration::from_millis(g.chrome_min_spacing_ms),
            // Content is delay-never-drop (#297); the hold ceiling is the
            // ticker's own orphan horizon, one tick shorter than the 30-minute
            // stop so a held write can still land before the turn dies.
            content_max_hold: Duration::from_secs(25 * 60),
            pause_fallback: Duration::from_secs(g.pause_secs.max(1)),
            pause_ceiling: Duration::from_secs(g.pause_secs.max(1) * 4),
        }
    }
}

/// Per-channel budget state. One entry per channel id, created on first
/// write; the whole map lives under one lock because every path drops the
/// guard before it awaits (see [`Governor::gate`]).
#[derive(Debug)]
pub(crate) struct ChannelBudget {
    /// Token bucket. Starts full so a turn's opening burst is free.
    tokens: f64,
    /// Last refill instant; `None` until the first refill runs.
    last_refill: Option<Instant>,
    /// When the last chrome write was admitted: the spacing clock.
    last_chrome: Option<Instant>,
    /// Until when this channel is parked by a 429.
    pause_until: Option<Instant>,
}

impl Default for ChannelBudget {
    fn default() -> Self {
        Self {
            tokens: f64::INFINITY,
            last_refill: None,
            last_chrome: None,
            pause_until: None,
        }
    }
}

impl ChannelBudget {
    /// Refill the bucket up to `burst` at `ceiling_per_min` tokens/minute and
    /// clear an expired park. Idempotent, so callers run it before every
    /// read. A disabled ceiling leaves the bucket infinite: nothing paces.
    fn refresh(&mut self, l: &Limits, now: Instant) {
        if l.ceiling_per_min == 0 {
            self.tokens = f64::INFINITY;
            self.last_refill = Some(now);
        } else {
            let capacity = l.burst.max(1) as f64;
            let rate = l.ceiling_per_min as f64 / 60.0;
            self.tokens = match self.last_refill {
                Some(last) => {
                    let dt = now.saturating_duration_since(last).as_secs_f64();
                    (self.tokens + dt * rate).min(capacity)
                }
                None => capacity,
            };
            self.last_refill = Some(now);
        }
        if self.pause_until.is_some_and(|until| until <= now) {
            self.pause_until = None;
        }
    }

    /// True while a 429 park is in force. The flow ticker consults this
    /// through [`Governor::paused`] and skips its edit rather than fighting
    /// the window serenity already told us about.
    fn is_paused(&self, now: Instant) -> bool {
        self.pause_until.is_some_and(|until| until > now)
    }
}

/// Pure admission for one write. No I/O, no clock reads: `now` is injected
/// and `held` is what the caller has already spent waiting, so a test can
/// drive a whole hold ladder in one line.
///
/// Charges the bucket only on [`Decision::Admit`] (and on [`Decision::FailOpen`]
/// it charges nothing, because the write bypassed the budget by policy).
pub(crate) fn admit(
    b: &mut ChannelBudget,
    l: &Limits,
    surface: Surface,
    held: Duration,
    now: Instant,
) -> Decision {
    if !l.enabled {
        return Decision::Admit;
    }
    b.refresh(l, now);

    // A parked channel is a parked channel: content waits out the window,
    // chrome has nothing useful to say into it and is discarded.
    if b.is_paused(now) {
        let until = b.pause_until.unwrap_or(now);
        let wait = until.saturating_duration_since(now);
        return if surface.is_content() {
            if held + wait > l.content_max_hold {
                Decision::FailOpen
            } else {
                Decision::Hold(wait)
            }
        } else {
            Decision::Drop
        };
    }

    // Chrome spacing: two clock re-renders inside one window is one too many.
    if surface == Surface::Chrome
        && !l.chrome_spacing.is_zero()
        && let Some(last) = b.last_chrome
        && now.saturating_duration_since(last) < l.chrome_spacing
    {
        return Decision::Drop;
    }

    // The ceiling. Chrome drops on dry; content holds for the refill and
    // fails open past the hold ceiling.
    if l.ceiling_per_min > 0 && b.tokens < 1.0 {
        let need = Duration::from_secs_f64((1.0 - b.tokens) * 60.0 / l.ceiling_per_min as f64);
        if !surface.is_content() {
            return Decision::Drop;
        }
        if held + need > l.content_max_hold {
            tracing::warn!(
                "Discord: governor write ceiling held a content write past its ceiling, failing open"
            );
            return Decision::FailOpen;
        }
        return Decision::Hold(need);
    }

    if l.ceiling_per_min > 0 {
        b.tokens -= 1.0;
    }
    if surface == Surface::Chrome {
        b.last_chrome = Some(now);
    }
    Decision::Admit
}

/// Park `until` if it is later than what is already armed, and return the
/// window that took effect measured from `now`. Latest-wins: a 429 naming
/// 30 s must not be shortened by an earlier one that named 2 s.
fn arm_park(b: &mut ChannelBudget, until: Instant, now: Instant) -> Duration {
    match b.pause_until {
        Some(existing) if existing >= until => existing.saturating_duration_since(now),
        _ => {
            b.pause_until = Some(until);
            until.saturating_duration_since(now)
        }
    }
}

/// Learn from a 429. `retry_after` is the window the RESPONSE named, when the
/// response named one; `None` means "we know it was a 429, we do not know how
/// long", and the fallback ceiling applies. Either way the number is clamped
/// to [`Limits::pause_ceiling`]: a window measured in hours parks the channel
/// for the ceiling and then hands the problem back to the API, because a
/// governor that stalls a channel forever is a worse outage than the one it
/// prevents. Returns the effective pause.
pub(crate) fn note_429(
    b: &mut ChannelBudget,
    l: &Limits,
    retry_after: Option<Duration>,
    now: Instant,
) -> Duration {
    let requested = retry_after.unwrap_or(l.pause_fallback);
    let clamped = requested.min(l.pause_ceiling);
    // Return what the channel is ACTUALLY parked for, not what this one
    // response asked for: a shorter ask on a budget already parked longer
    // must not report a window the budget does not honor.
    arm_park(b, now + clamped, now)
}

/// The channel-wide governor: one budget per channel id behind a single
/// `std::sync::Mutex`. Every path takes the guard, decides, and DROPS it
/// before any await - holding it across a sleep would stall every outbound
/// Discord write in the process.
#[derive(Default)]
pub(crate) struct Governor {
    channels: Mutex<HashMap<u64, ChannelBudget>>,
}

impl Governor {
    /// Admit, hold, drop or fail open one write on `channel`. Content sleeps
    /// out holds and re-asks; chrome never waits. `held` accumulates so the
    /// fail-open ceiling is honest even across several hold rounds.
    pub(crate) async fn gate(&self, channel: u64, surface: Surface) {
        let mut held = Duration::ZERO;
        loop {
            let decision = {
                let limits = Limits::from_config();
                let mut channels = self.channels.lock().expect("discord governor lock");
                admit(
                    channels.entry(channel).or_default(),
                    &limits,
                    surface,
                    held,
                    now(),
                )
            };
            match decision {
                Decision::Admit | Decision::Drop => return,
                Decision::FailOpen => {
                    tracing::warn!(
                        "Discord: governor held a {surface:?} write past its ceiling on \
                         channel {channel} and let it through unpaced (#1910)"
                    );
                    return;
                }
                Decision::Hold(wait) => {
                    // The caller cannot tell a drop from an admit: chrome
                    // paths that must not be held are gated up front with
                    // [`Governor::chrome_admits`], so a Hold here is always a
                    // content write, and content waits.
                    tokio::time::sleep(wait).await;
                    held += wait;
                }
            }
        }
    }

    /// Non-blocking chrome check for the re-render paths: true when this
    /// tick's edit may go out. A false means "skip the edit", which is exactly
    /// the drop the caller would have made anyway, and it keeps the ticker
    /// honest about not sleeping into a 429 window.
    pub(crate) fn chrome_admits(&self, channel: u64) -> bool {
        let limits = Limits::from_config();
        let mut channels = self.channels.lock().expect("discord governor lock");
        matches!(
            admit(
                channels.entry(channel).or_default(),
                &limits,
                Surface::Chrome,
                Duration::ZERO,
                now(),
            ),
            Decision::Admit
        )
    }

    /// Record that a write on `channel` came back rate-limited. `retry_after`
    /// is whatever the response named, if anything. Logs one structured line
    /// per pause, which is the observability the issue asked for: before this
    /// a 429 was indistinguishable from any other failure in the log.
    pub(crate) fn note_rate_limited(&self, channel: u64, retry_after: Option<Duration>) {
        let limits = Limits::from_config();
        let pause = {
            let mut channels = self.channels.lock().expect("discord governor lock");
            note_429(
                channels.entry(channel).or_default(),
                &limits,
                retry_after,
                now(),
            )
        };
        tracing::warn!(
            channel = channel,
            pause_ms = pause.as_millis() as u64,
            named = retry_after.map(|d| d.as_millis() as u64),
            "Discord: 429 on channel {channel}, parking outbound writes for {} ms{}",
            pause.as_millis(),
            match retry_after {
                Some(d) => format!(" (API named {d:?}, clamped to the ceiling)"),
                None => " (response named no retry_after, fallback window)".to_string(),
            }
        );
    }

    /// True when the error is a Discord 429. Serenity surfaces the status code
    /// on an unsuccessful request; anything else (deleted message, bad token,
    /// network) is not a rate limit and must not park the channel.
    pub(crate) fn is_rate_limited(err: &serenity::Error) -> bool {
        matches!(
            err,
            serenity::Error::Http(http)
                if http.status_code().is_some_and(|code| code.as_u16() == 429)
        )
    }

    /// The `retry_after` a response named, in seconds, if we can read it.
    /// Serenity 0.12.5 puts the code and message on the error but NOT the
    /// numeric `retry_after` (see `http/error.rs`: `DiscordJsonError` has
    /// `code`/`message`/`errors` only), so this scans the rendered text for a
    /// `retry_after <n>` / `retry after <n>` mention and answers `None`
    /// otherwise, which sends the caller to the fallback window. The
    /// response's own rate-limit headers are what serenity's limiter already
    /// consumes; this layer only records what it was told.
    pub(crate) fn retry_after(err: &serenity::Error) -> Option<Duration> {
        let text = err.to_string().to_lowercase();
        let at = text
            .find("retry_after")
            .or_else(|| text.find("retry after"))?;
        let rest = &text[at..];
        let digits = rest.find(char::is_numeric)?;
        let tail = &rest[digits..];
        let end = tail
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(tail.len());
        let secs: f64 = tail[..end].parse().ok()?;
        if secs > 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        }
    }
}

/// Park the channel when the failure is a 429 and nothing else. Every helper
/// below runs this on its error path, which is why no call site has to remember
/// it: learning the window is the whole point of having this layer.
pub(crate) fn note_if_429(governor: &Governor, channel: u64, err: Option<&serenity::Error>) {
    if let Some(e) = err.filter(|e| Governor::is_rate_limited(e)) {
        governor.note_rate_limited(channel, Governor::retry_after(e));
    }
}

/// One content write out to `channel`: pay the budget first, then send, then
/// learn from a 429. These helpers are the sanctioned way a Discord outbound
/// write leaves this channel, so the policy cannot be skipped by forgetting to
/// call it, which is exactly what happened before #1910. The flow ticker is
/// the one deliberate exception: it checks its own budget up front so a
/// refusal can skip the rest of the tick, and it must not pay twice.
// serenity's `Error` is 128+ bytes and is not ours to box; the repo
// allows this lint everywhere a write returns serenity's own Result.
#[allow(clippy::result_large_err)]
pub(crate) async fn send_content(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    surface: Surface,
    builder: serenity::builder::CreateMessage,
) -> Result<serenity::model::channel::Message, serenity::Error> {
    governor.gate(channel.get(), surface).await;
    let res = channel.send_message(http, builder).await;
    note_if_429(governor, channel.get(), res.as_ref().err());
    res
}

/// The same for an edit of an existing message. Used by the settle stamps,
/// which are the last thing a flow bubble will ever say and therefore content,
/// not chrome.
// serenity's `Error` is 128+ bytes and is not ours to box; the repo
// allows this lint everywhere a write returns serenity's own Result.
#[allow(clippy::result_large_err)]
pub(crate) async fn edit_content(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    message: serenity::model::id::MessageId,
    surface: Surface,
    edit: serenity::builder::EditMessage,
) -> Result<serenity::model::channel::Message, serenity::Error> {
    governor.gate(channel.get(), surface).await;
    let res = channel.edit_message(http, message, edit).await;
    note_if_429(governor, channel.get(), res.as_ref().err());
    res
}

/// A plain-text send, the shape most call sites actually use. Same contract as
/// [`send_content`]: pay the budget, write, learn from a 429. Exists so a
/// caller that only has a string does not have to build a `CreateMessage`.
// serenity's `Error` is 128+ bytes and is not ours to box; the repo
// allows this lint everywhere a write returns serenity's own Result.
#[allow(clippy::result_large_err)]
pub(crate) async fn say(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    surface: Surface,
    content: impl std::fmt::Display,
) -> Result<serenity::model::channel::Message, serenity::Error> {
    governor.gate(channel.get(), surface).await;
    let res = channel.say(http, content.to_string()).await;
    note_if_429(governor, channel.get(), res.as_ref().err());
    res
}

/// [`say`], plus a row in `channel_messages` (#2196).
///
/// Only the answer-delivery sites and the `discord_send` tool use this. The
/// plain [`say`] stays the path for tool-group bubbles and flow-ticker
/// repaints: rendered group markdown belongs on screen, not in the transcript
/// the next turn's group-context preamble reads back.
// serenity's `Error` is 128+ bytes and is not ours to box; the repo
// allows this lint everywhere a write returns serenity's own Result.
#[allow(clippy::result_large_err)]
pub(crate) async fn say_recorded(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    surface: Surface,
    content: impl std::fmt::Display,
) -> Result<serenity::model::channel::Message, serenity::Error> {
    let body = content.to_string();
    let res = say(governor, channel, http, surface, &body).await;
    if let Ok(msg) = &res {
        super::outbox::record_sent(None, msg, &body).await;
    }
    res
}

/// [`send_content`], plus a row in `channel_messages` (#2196).
///
/// `content` is the text the builder carries, passed separately because
/// `CreateMessage` does not expose it back. An attachment-only send records
/// nothing, which matches the empty-content skip in the recorder.
// serenity's `Error` is 128+ bytes and is not ours to box; the repo
// allows this lint everywhere a write returns serenity's own Result.
#[allow(clippy::result_large_err)]
pub(crate) async fn send_content_recorded(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    surface: Surface,
    builder: serenity::builder::CreateMessage,
    content: &str,
) -> Result<serenity::model::channel::Message, serenity::Error> {
    let res = send_content(governor, channel, http, surface, builder).await;
    if let Ok(msg) = &res {
        super::outbox::record_sent(None, msg, content).await;
    }
    res
}

/// A chrome re-render. `None` means the governor dropped this paint: the
/// caller skips the edit and changes nothing else, because the next tick
/// restates the same content. An admitted paint reports its API result like
/// any other write.
pub(crate) async fn edit_chrome(
    governor: &Governor,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    message: serenity::model::id::MessageId,
    edit: serenity::builder::EditMessage,
) -> Option<Result<serenity::model::channel::Message, serenity::Error>> {
    if !governor.chrome_admits(channel.get()) {
        tracing::debug!(
            "Discord: governor dropped chrome edit on channel {}",
            channel.get()
        );
        return None;
    }
    let res = channel.edit_message(http, message, edit).await;
    note_if_429(governor, channel.get(), res.as_ref().err());
    Some(res)
}

// ---------------------------------------------------------------------------
// Test support (cfg(test)) - shared with src/tests/discord_governor_test.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Serialize every test that swaps the process-wide config mirror or
    /// moves the virtual clock. Both are process singletons by design (one
    /// config per instance, one monotonic reference), so tests that touch
    /// them take this guard first. Same contract as Telegram's
    /// `governor::test_support::registry_guard`.
    /// Async on purpose: the guard is held across `gate().await`, so a
    /// `std::sync` guard would make the test future non-`Send`.
    pub(crate) async fn registry_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        GUARD.lock().await
    }

    /// Pin the virtual clock at `offset_ms` past the real instant.
    pub(crate) fn reset(offset_ms: u64) {
        CLOCK_OFFSET_MS.store(offset_ms, Ordering::Relaxed);
    }

    /// Advance the virtual clock: refills, spacing windows and parks observe
    /// the jump on their next `now()` read. No test ever sleeps to make time
    /// pass.
    pub(crate) fn advance(ms: u64) {
        CLOCK_OFFSET_MS.fetch_add(ms, Ordering::Relaxed);
    }
}
