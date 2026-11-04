//! The Discord flow ticker's decision surface (#1843): the clock must
//! re-render on a throttle, not freeze between tool events. The loop itself
//! is a network task (message edits against a live session), so the tests
//! pin the snapshot contract it decides on, plus lexical sentinels for the
//! throttle (4 s, never Telegram's 1500 ms), the cap, and the spawn site.

use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{GroupEntry, GroupState};
use std::time::Instant;

fn live_group() -> GroupState {
    GroupState {
        page: 0,
        entries: (0..2)
            .map(|i| GroupEntry {
                call_id: String::new(),
                task_id: None,
                name: format!("tool{i}"),
                context: format!(" (arg{i})"),
                status: None,
            })
            .collect(),
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        settled: None,
    }
}

#[tokio::test]
async fn snapshot_reports_a_live_group_so_the_ticker_ticks() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, live_group()).await;
    let snap = state.tool_group_snapshot(111).await;
    assert!(snap.is_some(), "live group must be visible to the ticker");
    assert!(
        snap.unwrap().settled.is_none(),
        "group is live: the ticker must keep ticking"
    );
}

#[tokio::test]
async fn snapshot_reports_settled_so_the_ticker_stops() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, live_group()).await;
    state.settle_tool_group(111, None, None, None).await;
    let snap = state
        .tool_group_snapshot(111)
        .await
        .expect("settled group is still stored");
    assert!(
        snap.settled.is_some(),
        "settled must stay visible to the ticker: it is the stop condition (#1843)"
    );
}

#[tokio::test]
async fn snapshot_missing_group_is_none() {
    let state = DiscordState::new();
    assert!(
        state.tool_group_snapshot(404).await.is_none(),
        "a pruned or never-born group stops the ticker via None"
    );
}

/// The throttle is the rate-limit posture: Discord's docs do not publish a
/// fixed per-route edit budget and explicitly forbid hardcoding one, and
/// serenity's built-in ratelimiter is the safety layer. The 4 s interval is
/// ours, chosen for parity with the Slack ticker (#1807). Telegram runs
/// 1500 ms; that number must never migrate here.
#[test]
fn ticker_throttle_is_rate_limit_safe() {
    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        handler.contains(
            "const FLOW_TICKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);"
        ),
        "the ticker interval is 4 s, parity with Slack (#1807)"
    );
    assert!(
        !handler.contains("from_millis(1500"),
        "Telegram's 1500 ms tick must never migrate to Discord"
    );
    assert!(
        handler.contains(
            "const FLOW_TICKER_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);"
        ),
        "orphaned ticks die at the 30 min cap: no immortal tasks"
    );
    assert!(
        handler.contains("should not be hard coded"),
        "the docs citation must stay: the budget is serenity's limiter, not a magic number"
    );
}

/// The ticker must spawn before the turn is dispatched. Since #1845 the
/// group is born at turn start (before the spawn), so the ticker finds it
/// immediately; the spawn still precedes the agent turn itself.
#[test]
fn ticker_spawns_before_the_turn_dispatch() {
    let handler = include_str!("../channels/discord/handler.rs");
    let spawn = handler
        .find("Spawns after the turn-start")
        .expect("spawn site comment present");
    let dispatch = handler
        .find("send_message_with_tools_and_display")
        .expect("turn dispatch present");
    assert!(
        spawn < dispatch,
        "spawn_flow_ticker must run before the agent turn starts"
    );
}
