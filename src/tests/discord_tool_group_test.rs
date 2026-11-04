//! Tests for the collapsible Discord tool group (#380): render modes,
//! toggle/preservation semantics, and retention pruning — mirroring the
//! Slack port's contracts.

use crate::channels::background_work::FlowOutcome;
use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, SettledStatus, render_components, render_content, render_embeds,
};
use std::time::{Duration, Instant};

fn entries(n: usize, done: bool) -> Vec<GroupEntry> {
    (0..n)
        .map(|i| GroupEntry {
            call_id: String::new(),
            task_id: None,
            name: format!("tool{i}"),
            context: format!(" (arg{i})"),
            status: if done { Some(true) } else { None },
        })
        .collect()
}

fn group(n: usize, done: bool, expanded: bool) -> GroupState {
    GroupState {
        page: 0,
        entries: entries(n, done),
        expanded,
        notes: Vec::new(),
        started_at: Instant::now(),
        settled: None,
    }
}

#[test]
fn collapsed_shows_summary_expanded_lists_tools() {
    let collapsed = render_content(&group(3, false, false));
    assert!(collapsed.contains("3 tool calls"));
    assert!(collapsed.contains("running"));
    assert!(!collapsed.contains("tool0"));

    // Expanded rows live in the card embed, not the message text (#1880).
    let expanded = render_content(&group(3, true, true));
    assert!(!expanded.contains("tool0"));
    let card = serde_json::to_string(&render_embeds(&group(3, true, true))).unwrap();
    assert!(card.contains("tool0") && card.contains("tool2"));

    let single = render_content(&group(1, false, false));
    assert!(single.contains("tool0"));
    assert!(!single.contains("tool call"));
}

#[test]
fn toggle_button_only_for_multi_tool_groups() {
    assert!(render_components(&group(1, false, false), 7).is_empty());
    assert_eq!(render_components(&group(2, false, false), 7).len(), 1);
}

#[tokio::test]
async fn toggle_flips_and_updates_preserve_expansion() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, group(2, false, false)).await;
    let toggled = state.toggle_tool_group(111).await.expect("exists");
    assert!(toggled.expanded);
    // Progress updates (built collapsed) must preserve the user's choice.
    let stored = state.upsert_tool_group(111, group(2, true, false)).await;
    assert!(stored.expanded);
    assert!(state.toggle_tool_group(999).await.is_none());
}

#[tokio::test]
async fn retention_prunes_oldest_groups() {
    let state = DiscordState::new();
    for i in 0..25u64 {
        state.upsert_tool_group(i, group(2, true, false)).await;
    }
    // Retention is a memory bound. A click on an evicted card legitimately
    // restores it from SQLite whenever a pool is present (#1880), so assert on
    // the memory map rather than on the toggle, which depends on whether some
    // other test in the binary has opened a database.
    let ids = state.memory_group_ids().await;
    assert_eq!(ids.len(), 20);
    assert!(!ids.contains(&0));
    assert!(ids.contains(&24));
    assert!(state.toggle_tool_group(24).await.is_some());
}

#[test]
fn live_summary_carries_the_rolling_clock_settled_freezes_it() {
    let live = render_content(&group(3, false, false));
    assert!(live.contains("🕒"));

    let mut done_group = group(2, true, false);
    done_group.settled = Some(SettledStatus {
        elapsed: Duration::from_secs(90),
        ctx: Some("ctx: 84K/200K 42%".into()),
        waiting: None,
        outcome: None,
    });
    let settled = render_content(&done_group);
    assert!(settled.contains("⏱️ 1:30"));
    assert!(settled.contains("ctx: 84K/200K 42%"));
    assert!(!settled.contains("🕒"));
}

#[tokio::test]
async fn settle_freezes_elapsed_and_stamps_ctx() {
    let state = DiscordState::new();
    let mut g = group(2, true, false);
    g.started_at = Instant::now() - Duration::from_secs(90);
    state.upsert_tool_group(77, g).await;
    let stamped = state
        .settle_tool_group(77, Some("ctx: 1K/2K 50%".into()), None, None)
        .await
        .expect("group exists");
    let s = stamped.settled.as_ref().expect("stamped at settle");
    assert_eq!(s.elapsed.as_secs(), 90);
    let done = render_content(&stamped);
    assert!(done.contains("ctx: 1K/2K 50%"));
    assert!(done.contains("⏱️ 1:30"));
}

#[tokio::test]
async fn upsert_preserves_started_at_and_settled() {
    let state = DiscordState::new();
    let mut g = group(1, false, false);
    g.started_at = Instant::now() - Duration::from_secs(30);
    state.upsert_tool_group(88, g).await;
    state
        .settle_tool_group(88, Some("ctx: A".into()), None, None)
        .await;
    // A late progress update must not restart the clock or clear the stamp.
    let stored = state.upsert_tool_group(88, group(1, true, false)).await;
    assert!(stored.started_at.elapsed().as_secs() >= 29);
    assert!(stored.settled.is_some());
}

#[tokio::test]
async fn resettle_with_no_ctx_keeps_the_stamped_budget() {
    let state = DiscordState::new();
    state.upsert_tool_group(99, group(1, true, false)).await;
    state
        .settle_tool_group(99, Some("ctx: B".into()), None, None)
        .await;
    let again = state
        .settle_tool_group(99, None, None, None)
        .await
        .expect("group exists");
    assert_eq!(
        again
            .settled
            .as_ref()
            .expect("still stamped")
            .ctx
            .as_deref(),
        Some("ctx: B")
    );
}

// #1987: the waiting state, the settled outcome stamp, the flip, and the registry.

#[test]
fn waiting_verb_replaces_the_check_and_keeps_the_frozen_clock() {
    let mut g = group(2, true, false);
    g.settled = Some(SettledStatus {
        elapsed: Duration::from_secs(60),
        ctx: Some("ctx: 84K/200K 42%".into()),
        waiting: Some("waiting for 2 background tasks".into()),
        outcome: None,
    });
    let line = render_content(&g);
    assert!(line.contains("⏳ waiting for 2 background tasks"));
    assert!(!line.contains("✅"));
    assert!(!line.contains("🕒"));
    assert!(line.contains("⏱️ 1:00"));
}

#[test]
fn cancelled_outcome_settles_the_group_with_a_cross() {
    let mut g = group(2, true, false);
    g.settled = Some(SettledStatus {
        elapsed: Duration::from_secs(12),
        ctx: None,
        waiting: None,
        outcome: Some(FlowOutcome::Cancelled),
    });
    let line = render_content(&g);
    assert!(line.contains("❌ Cancelled"));
    assert!(!line.contains("✅"));
    assert!(!line.contains("🕒"));
}

#[tokio::test]
async fn settle_stamps_waiting_then_the_failed_outcome() {
    let state = DiscordState::new();
    state.upsert_tool_group(120, group(2, true, false)).await;
    let stamped = state
        .settle_tool_group(
            120,
            None,
            Some("waiting for 1 background task".into()),
            None,
        )
        .await
        .expect("group exists");
    let s = stamped.settled.as_ref().expect("stamped at settle");
    assert_eq!(s.waiting.as_deref(), Some("waiting for 1 background task"));
    assert!(s.outcome.is_none());
    state.upsert_tool_group(121, group(1, true, false)).await;
    let dead = state
        .settle_tool_group(121, None, None, Some(FlowOutcome::Failed))
        .await
        .expect("group exists");
    assert_eq!(
        dead.settled.as_ref().expect("stamped").outcome,
        Some(FlowOutcome::Failed)
    );
    assert!(render_content(&dead).contains("❌ Failed"));
}

#[tokio::test]
async fn refresh_waiting_line_narrows_then_flips_to_finished() {
    let state = DiscordState::new();
    let mut g = group(2, true, false);
    g.started_at = Instant::now() - Duration::from_secs(60);
    state.upsert_tool_group(130, g).await;
    let settled = state
        .settle_tool_group(
            130,
            None,
            Some("waiting for 3 background tasks".into()),
            None,
        )
        .await
        .expect("group exists");
    assert!(render_content(&settled).contains("⏳ waiting for 3 background tasks"));
    // A completion narrows the verb; the frozen clock must survive the edit.
    let narrowed = state
        .refresh_waiting_line(130, Some("waiting for 1 background task".into()))
        .await
        .expect("group exists");
    let line = render_content(&narrowed);
    assert!(line.contains("⏳ waiting for 1 background task"));
    assert!(line.contains("⏱️ 1:00"));
    // Drained: the line flips to the plain finished check, no hourglass left.
    let done = state
        .refresh_waiting_line(130, None)
        .await
        .expect("group exists");
    let line = render_content(&done);
    assert!(line.contains("✅"));
    assert!(!line.contains("⏳"));
    // An unsettled group has no waiting line to refresh...
    state.upsert_tool_group(131, group(1, false, false)).await;
    assert!(state.refresh_waiting_line(131, None).await.is_none());
    // ...and an aged-out message id reports None so the caller can drop its
    // registration instead of pinning the slot forever.
    assert!(state.refresh_waiting_line(9999, None).await.is_none());
}

#[tokio::test]
async fn waiting_group_registry_roundtrips() {
    let state = DiscordState::new();
    let session = uuid::Uuid::new_v4();
    assert!(state.waiting_group_for(session).await.is_none());
    state.register_waiting_group(session, 4242).await;
    assert_eq!(state.waiting_group_for(session).await, Some(4242));
    // A newer waiting settle overwrites the stale registration.
    state.register_waiting_group(session, 5151).await;
    assert_eq!(state.waiting_group_for(session).await, Some(5151));
    assert_eq!(state.clear_waiting_group(session).await, Some(5151));
    assert!(state.waiting_group_for(session).await.is_none());
}
