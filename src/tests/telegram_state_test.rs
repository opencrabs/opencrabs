//! `TelegramState` registry and fold-dedupe tests (#61).
//!
//! Moved out of an inline `#[cfg(test)] mod tests` at the foot of
//! `src/channels/telegram/state.rs`, which #1422 added against the house rule
//! that tests live under `src/tests/`. Test logic is unchanged; only the module
//! path moved, plus the two dedupe constants gained `pub(crate)` so they stay
//! reachable from here.

use crate::channels::telegram::flow::StreamingState;
use crate::channels::telegram::state::{LiveFlowHandle, TelegramState};
use std::sync::Arc;
use uuid::Uuid;

/// Minimal StreamingState with one OPEN roll block — enough for
/// registry lifecycle tests; field set mirrors the turn-site literal
/// in handler.rs (house pattern, src/tests/telegram_stream_loop_resume_test.rs).
fn open_roll() -> std::sync::Arc<std::sync::Mutex<StreamingState>> {
    Arc::new(std::sync::Mutex::new(StreamingState {
        is_dm: true,
        pending_suggestions: None,
        pending_trailer: None,
        delivered_files: Vec::new(),
        media_intermediates: Vec::new(),
        msg_id: None,
        thinking: String::new(),
        tool_msgs: Vec::new(),
        display_queue: Vec::new(),
        open_group_msg_id: Some(teloxide::types::MessageId(1)),
        rich_transport_failures: 0,
        flow_entries: Vec::new(),
        flow_status: None,
        flow_rich: false,
        response: String::new(),
        final_bubble: None,
        dirty: false,
        recreate: false,
        header_preview: None,
        compacting: false,
        sections: Default::default(),
        retained_goal: None,
        applied_plan_kb: Default::default(),
        tool_round_count: 0,
        tools_started_at: None,
        turn_started_at: std::time::Instant::now(),
        flow_outcome: None,
        bg_indicator: None,
        bg_count: None,
        subagent_counts: Default::default(),
        sent_intermediates: Vec::new(),
        intermediate_bubbles: Vec::new(),
        voice_msg_ids: Vec::new(),
        processing: true,
        is_cli: false,
    }))
}

fn handle_for(streaming: &std::sync::Arc<std::sync::Mutex<StreamingState>>) -> LiveFlowHandle {
    LiveFlowHandle {
        streaming: std::sync::Arc::clone(streaming),
    }
}

/// Register → lookup returns the same coordinates; explicit
/// unregister (cfg(test) path) clears; dropping an ALREADY-spent guard
/// afterwards is a no-op, not a panic.
#[test]
fn live_flow_registration_roundtrip() {
    let state = std::sync::Arc::new(TelegramState::new());
    let sid = Uuid::new_v4();
    assert!(state.live_flow(sid).is_none(), "fresh state: no live flow");
    let roll = open_roll();
    let guard = state.register_live_flow(sid, handle_for(&roll));
    let h = state.live_flow(sid).expect("registered handle visible");
    assert!(
        std::sync::Arc::ptr_eq(&h.streaming, &roll),
        "registered handle carries this turn's streaming Arc"
    );
    state.unregister_live_flow(sid);
    assert!(state.live_flow(sid).is_none(), "explicit unregister clears");
    drop(guard); // stale guard vs empty map: no panic, no resurrect
    assert!(state.live_flow(sid).is_none());
}

/// The RAII contract: scope exit — normal, early return, or panic
/// unwind — unregisters. This pins the drop path.
#[test]
fn live_flow_guard_drop_unregisters() {
    let state = std::sync::Arc::new(TelegramState::new());
    let sid = Uuid::new_v4();
    {
        let _g = state.register_live_flow(sid, handle_for(&open_roll()));
        assert!(state.live_flow(sid).is_some(), "live during the turn");
    }
    assert!(state.live_flow(sid).is_none(), "guard drop must unregister");
}

/// Turn-overlap window (#61): a successor turn registers before the
/// previous turn's guard drops. The stale guard must NOT evict the
/// successor's entry — Arc::ptr_eq identity is what spares it.
#[test]
fn live_flow_stale_guard_spares_successor() {
    let state = std::sync::Arc::new(TelegramState::new());
    let sid = Uuid::new_v4();
    let roll1 = open_roll();
    let roll2 = open_roll();
    let g1 = state.register_live_flow(sid, handle_for(&roll1));
    let g2 = state.register_live_flow(sid, handle_for(&roll2));
    drop(g1); // stale: its streaming Arc is no longer the registered one
    let h = state
        .live_flow(sid)
        .expect("successor survives the stale guard drop");
    assert!(
        std::sync::Arc::ptr_eq(&h.streaming, &roll2),
        "successor's handle"
    );
    drop(g2);
    assert!(
        state.live_flow(sid).is_none(),
        "successor's own drop clears"
    );
}

/// #61 fold-dedupe: the same fingerprint folds once; a re-delivery is
/// the duplicate; a distinct fingerprint still folds.
#[test]
fn notify_fold_dedup_second_delivery_is_duplicate() {
    let state = TelegramState::new();
    let sid = Uuid::new_v4();
    let now = std::time::Instant::now();
    assert!(!state.note_notify_fold_at(sid, 111, now), "first fold");
    assert!(state.note_notify_fold_at(sid, 111, now), "re-delivery dup");
    assert!(!state.note_notify_fold_at(sid, 222, now), "distinct folds");
    assert!(state.note_notify_fold_at(sid, 222, now), "its re-delivery");
}

/// TTL expiry: an expired fingerprint is foldable again — a genuine
/// re-send hours later is a real announcement, not a duplicate.
#[test]
fn notify_fold_dedup_ttl_expiry_refolds() {
    let state = TelegramState::new();
    let sid = Uuid::new_v4();
    let t0 = std::time::Instant::now();
    assert!(!state.note_notify_fold_at(sid, 7, t0));
    let later = t0 + TelegramState::NOTIFY_FOLD_DEDUP_TTL + std::time::Duration::from_secs(1);
    assert!(
        !state.note_notify_fold_at(sid, 7, later),
        "expired -> foldable"
    );
    assert!(state.note_notify_fold_at(sid, 7, later), "re-recorded");
}

/// Bounded memory: at the cap the oldest fingerprint is still held;
/// one more distinct push evicts it (foldable again) while younger
/// entries survive.
#[test]
fn notify_fold_dedup_cap_prunes_oldest() {
    let state = TelegramState::new();
    let sid = Uuid::new_v4();
    let now = std::time::Instant::now();
    for fp in 0..TelegramState::NOTIFY_FOLD_DEDUP_CAP as u64 {
        assert!(!state.note_notify_fold_at(sid, fp, now));
    }
    assert!(
        state.note_notify_fold_at(sid, 0, now),
        "oldest still held at cap"
    );
    assert!(
        !state.note_notify_fold_at(sid, 9999, now),
        "distinct push evicts the oldest"
    );
    assert!(
        state.note_notify_fold_at(sid, 1, now),
        "younger survives the cap eviction"
    );
    // Re-inserting the evicted fingerprint is allowed — and itself
    // evicts the then-oldest (fp 1), FIFO to the end.
    assert!(
        !state.note_notify_fold_at(sid, 0, now),
        "evicted -> foldable"
    );
    assert!(
        state.note_notify_fold_at(sid, 2, now),
        "the rest of the window survives"
    );
}

/// Dedupe is PER SESSION: two sessions fold the same fingerprint
/// independently.
#[test]
fn notify_fold_dedup_is_per_session() {
    let state = TelegramState::new();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let now = std::time::Instant::now();
    assert!(!state.note_notify_fold_at(a, 5, now));
    assert!(
        !state.note_notify_fold_at(b, 5, now),
        "other session folds the same fp"
    );
    assert!(state.note_notify_fold_at(a, 5, now));
}
