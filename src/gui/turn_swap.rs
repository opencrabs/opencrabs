//! Parking and restoring a turn when the screen changes conversation.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// How long a finished turn's reload keeps retrying while its rows are still
/// being written.
///
/// The first version bounded this with `SETTLE_ATTEMPTS` (eight), which the
/// poll cadence turned into roughly half a second. A 100 KB assistant row
/// under WAL can take longer: the 2026-11-08 23:38:29 log shows retries 1 to
/// 8 ending in "transcript never caught up", after which the live card stayed
/// pinned beside the persisted turn until the session was reopened. A
/// deadline bounds the wait without assuming how long one write takes.
pub(crate) const SETTLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// One more read this long after the budget expired, so a write that landed
/// late still releases the live card instead of leaving it pinned.
pub(crate) const SETTLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// How many late re-reads one turn may spend after the budget expired. The
/// second covers a write that landed between the first recheck and its own
/// read; past that, the log line is the honest answer.
pub(crate) const SETTLE_RECHECKS: u8 = 2;

/// Park the turn on screen under `from` and bring back the one `to` owns.
///
/// The window paints one conversation at a time, but every session may be
/// running a turn of its own. State moves with the session instead of the
/// window: the outgoing turn is stored whole and the incoming one restored, so
/// neither the spinner nor the counters jump to a conversation that never sent
/// anything (#2616). A session with nothing in flight gets an empty slot,
/// which is what makes a switch to an idle conversation paint as idle.
///
/// A card that has ended is parked too (#2811). Gating the park on `busy` alone
/// dropped every turn the moment it stopped running, so a turn the user had
/// just stopped came back as an empty slot and the stop, the stream and the
/// verdict went with it. What decides the park is whether the card holds
/// anything the ledger cannot draw yet, which is `!live.is_empty()`; `busy`
/// rides along so the restored card knows whether anything is still moving.
///
/// The whole [`chat_leaf::Turn`] moves now, not six fields of it, so the
/// reload handshake's deadline and late re-checks travel with the turn they
/// belong to instead of waiting on screen for the next conversation (#2805).
pub(crate) fn swap_visible_turn(
    stashed: &mut std::collections::HashMap<Uuid, chat_leaf::Turn>,
    from: Option<Uuid>,
    to: Uuid,
    turn: &mut chat_leaf::Turn,
) {
    if from == Some(to) {
        return;
    }
    if let Some(from) = from
        && (turn.busy || !turn.live.is_empty())
    {
        stashed.insert(from, std::mem::take(turn));
    }
    match stashed.remove(&to) {
        // The flag travels with the card (#2811): a stopped turn comes back
        // stopped, not "running again".
        Some(restored) => *turn = restored,
        None => *turn = chat_leaf::Turn::default(),
    }
}

/// Whether a reload holds the turn it was waiting for (#2705).
///
/// The comparison is against the session's row total as the ledger reports it,
/// never against the windowed turn count: past `transcript::WINDOW` rows the
/// window stops growing, so a turn count could never reach an expectation
/// taken from it and the metrics of every later turn were thrown away.
pub(crate) fn settle_landed(expect: Option<usize>, total: i64) -> bool {
    expect.is_none_or(|want| total.max(0) as usize >= want)
}
