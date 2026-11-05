//! Client-side route actions: resolve the best URL for a target, race a
//! host's routes, and harvest announced routes from a remote gateway.
//!
//! The split from [`super::route_store`]: the store is data (persist,
//! merge, order); this module is behavior over that data (probe, record,
//! learn). Both the `a2a_send` tool and the `opencrabs routes` CLI are
//! thin shells over these three functions.

use std::time::Duration;

use anyhow::Result;

use super::now_secs;
use super::route_store::{RaceOutcome, RouteStore, order, race_best};
use super::routes::AnnouncedRoute;

/// Resolve the best URL for a target the route store knows about.
///
/// Guarantees: with no store, no host owning `base_url`, or a single-route
/// host, this returns the input untouched and probes nothing — pre-Phase-2
/// behavior, bit for bit. With 2+ routes it races them (ordered, so the
/// likely winner is probed first), records every completed outcome, and
/// returns the winner. A total race failure also returns the input: the
/// downstream call then produces the honest error.
pub async fn resolve_best_url(store: &RouteStore, base_url: &str) -> String {
    let now = now_secs();
    let Ok(found) = store.find_host_by_url(base_url) else {
        return base_url.to_string();
    };
    let Some((_host_id, host)) = found else {
        return base_url.to_string();
    };
    if host.routes.len() < 2 {
        return base_url.to_string();
    }
    let ordered = order(&host.routes, now);
    let (winner, outcomes) = race_best(&ordered, now).await;
    record_outcomes(store, &outcomes, now);
    winner.unwrap_or_else(|| base_url.to_string())
}

/// Persist race outcomes: successes smooth the EWMA and clear streaks,
/// connect-failures bump the windowed streak. Aborted routes are absent
/// from `outcomes` and record nothing (cancellation is neutrality).
pub fn record_outcomes(store: &RouteStore, outcomes: &[RaceOutcome], now: u64) {
    for o in outcomes {
        let r = if o.ok {
            store.record_success(&o.url, o.latency_ms, now)
        } else {
            store.record_failure(&o.url, now)
        };
        if let Err(e) = r {
            tracing::warn!("route store update failed for {}: {e:#}", o.url);
        }
    }
}

/// Race one known host's routes (CLI diagnostic + shared race path).
pub async fn race_host(
    store: &RouteStore,
    host_id: &str,
) -> Result<(Option<String>, Vec<RaceOutcome>)> {
    let now = now_secs();
    let Some(host) = store.host(host_id)? else {
        anyhow::bail!("no such host in the route store: {host_id}");
    };
    if host.routes.is_empty() {
        anyhow::bail!("host {host_id} has no routes");
    }
    let ordered = order(&host.routes, now);
    let (winner, outcomes) = race_best(&ordered, now).await;
    record_outcomes(store, &outcomes, now);
    Ok((winner, outcomes))
}

/// Fetch `GET /a2a/routes` from a remote gateway and merge its announcement
/// into the store. Keyless or anonymous remotes return no host_id and are
/// silently skipped (announcement is auth-gated by design). Returns
/// `(host_id, learned_count)` when something was merged.
pub async fn harvest(
    store: &RouteStore,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Option<(String, usize)>> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/a2a/routes", base_url.trim_end_matches('/')))
        .timeout(Duration::from_secs(5));
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body: serde_json::Value = resp.json().await?;
    let Some(host_id) = body
        .get("host_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let routes: Vec<AnnouncedRoute> =
        serde_json::from_value(body.get("routes").cloned().unwrap_or_default()).unwrap_or_default();
    if routes.is_empty() {
        return Ok(None);
    }
    store.learn(host_id, base_url, &routes, now_secs())?;
    Ok(Some((host_id.to_string(), routes.len())))
}
