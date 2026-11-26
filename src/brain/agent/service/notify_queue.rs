//! Durable parking for undelivered pushes (#111).
//!
//! Every chokepoint that parks a push in memory (`PARKED` for
//! restart-recovery reports, `pending_reactions` for Telegram mid-turn
//! follow-ups) is wiped by a process kill. The `notify_queue` table is the
//! durable twin: each in-memory park also writes one row, and each consume
//! site clears the matching rows on delivery — the same posture the
//! tombstone persistence (#73) established.
//!
//! Best-effort by design, both directions:
//! - A failed persist costs a loud error log, the push rides the in-memory
//!   queue alone (exactly the pre-#111 behavior).
//! - A failed clear costs a stale row that boot redelivery replays — a
//!   DUPLICATE after restart, never a lost message.
//!
//! `None` before the DB is initialized (early startup, tests) simply means
//! durable parking is skipped, mirroring [`super::restart_recovery`]'s
//! tombstone posture. Entry points are runtime-agnostic: outside a live
//! tokio runtime (sync tests, pre-boot) the work is skipped with a debug
//! log rather than panicking — a sync context cannot await the DB anyway,
//! and the in-memory queue still holds the push.

use super::types::QueuedUserMessage;
use crate::db::NotifyQueueRepository;
use uuid::Uuid;

/// One day, the unit the reap ceiling is expressed in. Age is a schema-free
/// stand-in for a redelivery count, which would have needed a new column —
/// and the shipped table is deliberately unchanged.
const STALE_ROW_SECS: i64 = 24 * 60 * 60;

/// How long a row may survive in `notify_queue` before it is reaped.
///
/// Three days of boots is long past the point where a route is coming back:
/// each boot re-offers the row, so a row this old has been declined by every
/// start since it was parked. Only [`reap_stale_after_redelivery`] reads it,
/// and only after the current boot has made its own offer.
pub(crate) const MAX_ROW_AGE_SECS: i64 = 3 * STALE_ROW_SECS; // 72h (259,200s)

/// Age past which a redelivered push is marked as a delayed delivery (#2926).
///
/// The redelivery pass is age-blind on purpose: dropping an old row would lose
/// a push for a session that was simply asleep, which this module forbids. But
/// an unmarked row that is hours old lands in a conversation that has moved on
/// and reads as the answer to whatever the user just sent. Marking keeps the
/// never-lose contract and removes the misreading.
pub(crate) const STALE_NOTICE_SECS: i64 = 60 * 60;

/// Prefix a redelivered context with its queue age once it is older than
/// [`STALE_NOTICE_SECS`] (#2926).
///
/// Only `context_text` is marked. `display_text`, `origin` and `bg_meta` stay
/// untouched so the echo rendering and the receipt path are unaffected.
pub(crate) fn mark_delayed(context_text: &str, age_secs: i64) -> String {
    if age_secs <= STALE_NOTICE_SECS {
        return context_text.to_string();
    }
    let hours = age_secs / 3600;
    let minutes = (age_secs % 3600) / 60;
    format!(
        "[DELAYED PUSH] queued {hours}h{minutes:02}m ago, before the current \
         conversation. This is not a reply to anything just received.\n\n{context_text}"
    )
}

/// The marker `mark_delayed` opens its header with (#3175).
///
/// The rest of the header carries the queue age, so it cannot be a fixed
/// string: the age changes on every boot, which is exactly why the header has
/// to be recognised by its opening and cut at the blank line that ends it.
pub(crate) const DELAYED_MARK_PREFIX: &str = "[DELAYED PUSH]";

/// The busy-lane framing prepended to a push that lands while its target is
/// mid-turn (fork #13). It lives here, beside the durable-queue text
/// normalisation it has to survive, because the two are one contract.
pub(crate) const BUSY_LANE_WRAPPER: &str =
    "[queued while you were working — re-anchor to your current task after reading this]\n\n";

/// The text a durable row must carry for a push (#439/#366).
///
/// The row holds the push's OWN text: the busy-lane framing is presentation
/// applied when the push is handed to a turn, never part of the push's
/// identity. Two measured reasons (2026-09-30):
///
/// 1. The framing was re-applied on every re-offer, so the stored text grew by
///    one wrapper per boot — two live rows read `wrappers=2`.
/// 2. `clear_matching` is content-exact, so a clear issued with the
///    pre-framing text could never match a framed row. The row then survived
///    the very delivery that should have retired it, and the next boot
///    re-delivered it: one duplicate per boot, forever.
pub(crate) fn durable_context_text(context_text: &str) -> String {
    // Both framings, until neither applies. A push can pick up more than one:
    // a redelivery marks a row that may already carry the busy-lane wrapper,
    // and the two can arrive in either order (#3175).
    let mut rest = context_text;
    loop {
        let before = rest;
        rest = strip_delayed_mark(strip_busy_wrapper(rest));
        if rest == before {
            return rest.to_string();
        }
    }
}

/// Strip the delayed-push header a boot redelivery carries (#3175).
///
/// [`mark_delayed`] prepends the header to the in-memory copy only, so the
/// identity `persist` wrote is the text underneath it. Recognised by its
/// opening and cut at the blank line that closes the header, because the age
/// inside changes on every boot and a fixed-string strip cannot match it.
///
/// Repeated, like the busy-lane strip: a row re-offered across several boots
/// can be marked more than once. A text that opens with the marker but carries
/// no blank line is left alone rather than emptied: eating the whole push
/// would retire a row for a delivery that still has its own text to match.
pub(crate) fn strip_delayed_mark(text: &str) -> &str {
    let mut rest = text;
    while let Some(tail) = rest.strip_prefix(DELAYED_MARK_PREFIX) {
        // split_once, not `tail.find` plus `&tail[idx + 2..]`: the byte
        // index is a clippy::string_slice site, and the byte-slice ratchet in
        // CI fails the push on a new one (scripts/byte-slice-baseline.txt).
        match tail.split_once("\n\n") {
            Some((_, after)) => rest = after,
            None => break,
        }
    }
    rest
}

/// Strip every leading busy-lane framing wrapper.
///
/// Repeated, not single-pass: rows written before the framing was excluded
/// from the durable text can carry two or more, and leaving one behind would
/// defeat the content-exact match this exists to restore.
pub(crate) fn strip_busy_wrapper(text: &str) -> &str {
    let mut rest = text;
    while let Some(stripped) = rest.strip_prefix(BUSY_LANE_WRAPPER) {
        rest = stripped;
    }
    rest
}

/// Prepend the busy-lane framing, normalising to EXACTLY one.
///
/// The postcondition is "at most once", so input that already carries the
/// framing must come back with ONE wrapper, not with its original count. A
/// plain `starts_with` guard is not enough: it passes 0 and 1 through
/// correctly but leaves two or more untouched, and rows written before the
/// framing was excluded from the durable text do carry two or more (measured
/// live 2026-09-30: `wrappers=2`). A push re-offered at boot, or an item
/// re-queued after a lost turn race, must neither keep nor accumulate a
/// second one.
pub(crate) fn wrap_busy_once(context_text: &str) -> String {
    format!("{BUSY_LANE_WRAPPER}{}", strip_busy_wrapper(context_text))
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}
fn repo() -> Option<NotifyQueueRepository> {
    crate::db::global_pool().map(|p| NotifyQueueRepository::new(p.clone()))
}

/// Runtime-agnostic fire-and-forget: run the future on the tokio runtime
/// when one is live, log-and-drop when none is (sync tests, pre-boot).
fn spawn_if_runtime<F>(future: F, what: &'static str)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(future);
        }
        Err(_) => {
            tracing::debug!(
                target: "background_task",
                "No tokio runtime live: skipping {what} (durable parking unavailable)"
            );
        }
    }
}

/// Persist an undelivered push so a restart cannot lose it (#111).
///
/// Called at the chokepoints that park a push in memory. Best-effort: a
/// failure is logged loudly and the push rides the in-memory queue alone.
pub(crate) fn persist(session_id: Uuid, msg: &QueuedUserMessage) {
    let Some(repo) = repo() else {
        return;
    };
    // The row carries the push's OWN text, never the busy-lane framing: see
    // `durable_context_text`. Without this the framing stacks once per boot and
    // the content-exact clear can never match the row it must retire.
    let (context_text, display_text) = (
        durable_context_text(&msg.context_text),
        msg.display_text.clone(),
    );
    let (origin, bg_meta) = (msg.origin, msg.bg_meta.clone());
    spawn_if_runtime(
        async move {
            if let Err(e) = repo
                .record(
                    Uuid::new_v4(),
                    session_id,
                    &context_text,
                    &display_text,
                    origin,
                    bg_meta.as_ref(),
                )
                .await
            {
                tracing::error!(
                    target: "background_task",
                    "Could not persist undelivered push for session {session_id}: it rides \
                     the in-memory queue alone and the next restart will lose it: {e:#}"
                );
            }
        },
        "notify_queue persist",
    );
}

/// Re-offer every persisted push from a previous process (#111).
///
/// Runs from [`super::restart_recovery::recover`] alongside the tombstone
/// redelivery. A row whose push lands on a live route is cleared; one that
/// parks again keeps its row and keeps surviving restarts until the park is
/// genuinely delivered (the consume sites clear it). Returns how many rows
/// were re-offered.
pub(crate) async fn redeliver_persisted() -> usize {
    let Some(repo) = repo() else {
        return 0;
    };
    // Reap rows whose session no longer exists FIRST (#111 follow-up, Part B):
    // no channel can ever claim them, so no consume site can ever clear them,
    // and re-offering them would only re-park a push nobody can receive.
    match repo.clear_dead_sessions().await {
        Ok(reaped) if !reaped.is_empty() => {
            tracing::info!(
                target: "background_task",
                "Boot notify queue: reaped={} row(s) whose session no longer exists",
                reaped.len()
            );
            for row in reaped {
                let age_h = now_unix().saturating_sub(row.created_at) / 3600;
                let preview: String = row
                    .context_text
                    .replace(['\n', '\r'], " ")
                    .trim()
                    .chars()
                    .take(120)
                    .collect();
                tracing::error!(
                    target: "background_task",
                    "Notify queue reaper: dropped dead-session push {} for session {} (age {}h, origin={:?}): {}",
                    row.id,
                    row.session_id,
                    age_h,
                    row.origin,
                    preview
                );
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(
            target: "background_task",
            "Could not reap notify-queue rows for dead sessions: {e:#}"
        ),
    }

    let rows = match repo.all().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(
                target: "background_task",
                "Could not read persisted notify-queue rows: {e:#}"
            );
            return 0;
        }
    };
    let mut count = 0usize;
    for row in rows {
        // A row that survived a long downtime is delivered, never dropped, but
        // it says how old it is so it cannot read as a reply to newer traffic
        // (#2926).
        let age_secs = now_unix().saturating_sub(row.created_at);
        // Origin and bg_meta are preserved: the echo rendering and the
        // receipt path depend on them, and the row IS the message.
        let msg = QueuedUserMessage {
            context_text: mark_delayed(&row.context_text, age_secs),
            display_text: row.display_text.clone(),
            origin: row.origin,
            bg_meta: row.bg_meta.clone(),
        };
        if super::restart_recovery::deliver_or_park(row.session_id, msg)
            && let Err(e) = repo.clear(row.id).await
        {
            // Worst case the push is delivered twice, never zero times.
            tracing::error!(
                target: "background_task",
                "Delivered persisted push {} but could not clear its row; it may be \
                 re-delivered after the next restart: {e:#}",
                row.id
            );
        }
        count += 1;
    }

    // Only NOW reap by age (#182). A row is "unclaimed" because the pass
    // above just offered it and `deliver_or_park` re-parked it; anything a
    // live route could take was delivered and cleared moments ago. Reaping
    // before the offer would delete pushes for perfectly live sessions after
    // any long downtime (a sleeping laptop, a machine off over a holiday),
    // which is the one outcome this module's contract forbids.
    reap_stale_after_redelivery(&repo).await;

    count
}

/// Drop rows that survived the redelivery pass and are past the ceiling age.
///
/// Called at the END of [`redeliver_persisted`] so every row it sees has
/// already been offered to a live route and re-parked. Sessions that still
/// exist but have no channel route (headless/cron sessions, archived worker
/// topics) park pushes forever; past [`MAX_ROW_AGE_SECS`] the row is dropped
/// so the table does not grow without bound. Each drop is logged loudly: this
/// is the one place in the module where a push is genuinely lost, so it never
/// happens quietly.
async fn reap_stale_after_redelivery(repo: &NotifyQueueRepository) {
    let now = now_unix();
    let cutoff = now.saturating_sub(MAX_ROW_AGE_SECS);
    let max_age_hours = MAX_ROW_AGE_SECS / 3600;
    match repo.reap_stale_unclaimed(cutoff).await {
        Ok(reaped) if !reaped.is_empty() => {
            tracing::info!(
                target: "background_task",
                "Boot notify queue: reaped {} stale unclaimed push(es) older than {}h",
                reaped.len(),
                max_age_hours
            );
            for row in reaped {
                let age_h = now.saturating_sub(row.created_at) / 3600;
                let clean_text = row.context_text.replace(['\n', '\r'], " ");
                let preview: String = clean_text.trim().chars().take(120).collect();
                tracing::error!(
                    target: "background_task",
                    "Notify queue reaper: dropped undeliverable push {} for unclaimed session {} (age {}h > {}h, origin={:?}): {}",
                    row.id,
                    row.session_id,
                    age_h,
                    max_age_hours,
                    row.origin,
                    preview
                );
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(
            target: "background_task",
            "Could not reap stale unclaimed notify-queue rows: {e:#}"
        ),
    }
}

/// How long a post-delivery clear keeps looking for the row it must retire
/// (#439/#366, shape A).
///
/// The ordering half of the fix, and it exists because `route` CANNOT be
/// awaited: [`MessageEnqueueCallback`](super::types::MessageEnqueueCallback)
/// is `Arc<dyn Fn(Uuid, QueuedUserMessage)>` returning `()`, and
/// `build_enqueue_callback` defers the whole delivery — including its
/// `enqueue_item` → `persist` INSERT — into a `tokio::spawn` whose first act
/// is a bounded transport wait. So a clear issued at the hand-off reaches the
/// pool BEFORE the write it is retiring, which is the measured loop: one fresh
/// row per boot, one duplicate per boot, forever.
///
/// The clear therefore WAITS for its row rather than racing it, for a window
/// derived from the very budget that defers the write.
///
/// The route spends up to `ROUTE_GRACE` waiting for its transport BEFORE it
/// reaches the streaming branch that persists. A pre-fix measurement put the
/// twin 20-40 s after a boot for exactly that reason: a window shorter than
/// the grace expires before the row it is chasing exists, which is why this
/// is derived rather than fixed.
const CLEAR_MATCH_MARGIN: std::time::Duration = std::time::Duration::from_secs(5);
const CLEAR_MATCH_WINDOW: std::time::Duration = std::time::Duration::from_secs(
    super::restart_recovery::ROUTE_GRACE.as_secs() + CLEAR_MATCH_MARGIN.as_secs(),
);
/// Poll interval, matched to the transport-readiness poll so a wake follows
/// its connect on the cadence it already did.
const CLEAR_MATCH_STEP: std::time::Duration = std::time::Duration::from_millis(250);

/// Clear the durable twin of a push that was just delivered.
///
/// Ordering (shape A): the delete must not run before the write that replaces
/// it, so this re-reads for [`CLEAR_MATCH_WINDOW`] and retires EVERY matching
/// row it sees — the original row at the first attempt, and the deferred
/// INSERT of the delivering route when it lands a moment later. Stopping at
/// the first success would leave exactly that second row behind.
///
/// Match is FRAMING-BLIND (see [`durable_context_text`]) so a row written
/// before the #439 normalisation, still carrying the busy-lane framing, is
/// retired rather than left immortal until the age reaper, and so is a
/// delivery carrying the delayed-push header a boot redelivery adds (#3175).
/// Both sides are normalised, because a row may carry a framing the key does
/// not. Content-exact otherwise: any OTHER undelivered push for the same
/// session survives.
///
/// A clear that retires nothing is LOUD (M4): a chokepoint that returns a
/// silent success cannot corroborate its own failure. The two shapes are kept
/// apart in the log — a session with no rows left is benign (another consume
/// site already retired it); a session whose rows exist and none match is the
/// defect signal.
pub(crate) fn clear_on_delivery(session_id: Uuid, msg: &QueuedUserMessage) {
    let Some(repo) = repo() else {
        return;
    };
    // The row carries the push's OWN text (#439), and the in-memory item may
    // carry the busy-lane framing: normalise to the identity `persist` wrote.
    let context_text = durable_context_text(&msg.context_text);
    let display_text = msg.display_text.clone();
    spawn_if_runtime(
        async move {
            let retired = match retire_rows_until(
                &repo,
                session_id,
                &context_text,
                &display_text,
                CLEAR_MATCH_WINDOW,
                CLEAR_MATCH_STEP,
            )
            .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        target: "background_task",
                        "Could not clear delivered push from the durable notify queue for \
                         session {session_id}: next boot may redeliver it (a duplicate, \
                         never a loss): {e:#}"
                    );
                    return;
                }
            };
            if retired > 0 {
                return;
            }
            // Nothing retired: benign if the session has no rows at all
            // (a sibling consume site already took it), a defect if rows are
            // there and this push's key matched none of them.
            match repo.all().await {
                Ok(rows) if !rows.iter().any(|r| r.session_id == session_id) => {
                    tracing::debug!(
                        target: "background_task",
                        "Delivered push for session {session_id} had no durable row (already \
                         retired by a sibling consume site)"
                    );
                }
                Ok(rows) => {
                    let count = rows.iter().filter(|r| r.session_id == session_id).count();
                    tracing::warn!(
                        target: "background_task",
                        "Delivered push for session {session_id} matched NO durable row after \
                         {:?}, yet {count} row(s) are held for that session — the clear key does \
                         not match what was persisted; those rows survive to the next boot \
                         redelivery (a duplicate, never a loss)",
                        CLEAR_MATCH_WINDOW
                    );
                }
                Err(e) => tracing::warn!(
                    target: "background_task",
                    "Delivered push for session {session_id} retired no durable row and the \
                     queue could not be read to say why: {e:#}"
                ),
            }
        },
        "notify_queue clear",
    );
}

/// Retire this push's durable rows, WAITING for the delivering route's write.
///
/// The ordering half of shape A (#439/#366), split out from
/// [`clear_on_delivery`] so the contract is testable against an in-memory pool
/// rather than the process-global one. It keeps reading for the whole `window`
/// and retires every match it sees: the row that was already there, and the
/// deferred INSERT of the delivery that replaced it. A single pass that
/// stopped at the first success would leave exactly that second row behind,
/// which is the row that minted the next boot's duplicate.
pub(crate) async fn retire_rows_until(
    repo: &NotifyQueueRepository,
    session_id: Uuid,
    context_text: &str,
    display_text: &str,
    window: std::time::Duration,
    step: std::time::Duration,
) -> anyhow::Result<usize> {
    let deadline = tokio::time::Instant::now() + window;
    let mut retired = 0usize;
    loop {
        retired += clear_matching_rows(repo, session_id, context_text, display_text).await?;
        if tokio::time::Instant::now() >= deadline {
            return Ok(retired);
        }
        tokio::time::sleep(step).await;
    }
}

/// Retire every row of this session whose identity is this push's own.
///
/// Read-then-delete rather than one exact-content `DELETE`: the match has to
/// be wrapper-blind (a pre-#439 row still carries the framing, and stripping
/// is not expressible in SQL), and a per-row delete keeps the blast radius to
/// the rows that actually match. The table is small by construction — its own
/// 72h reap exists to keep it so.
async fn clear_matching_rows(
    repo: &NotifyQueueRepository,
    session_id: Uuid,
    context_text: &str,
    display_text: &str,
) -> anyhow::Result<usize> {
    let mut retired = 0usize;
    // Both sides, not only the key (#3175): a row written by a build that
    // stored the delayed header would otherwise never match its own delivery,
    // and that row would re-answer on every boot until the age reaper took it.
    let key = durable_context_text(context_text);
    for row in repo.all().await? {
        if row.session_id != session_id || row.display_text != display_text {
            continue;
        }
        if durable_context_text(&row.context_text) != key {
            continue;
        }
        repo.clear(row.id).await?;
        retired += 1;
    }
    Ok(retired)
}
