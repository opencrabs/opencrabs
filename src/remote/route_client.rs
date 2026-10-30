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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::route_store::RouteSource;
    use crate::remote::routes::RouteKind;

    fn store() -> (tempfile::TempDir, RouteStore) {
        let dir = tempfile::tempdir().unwrap();
        let st = RouteStore::new(dir.path().join("routes.json"));
        (dir, st)
    }

    /// Minimal HTTP responder: fixed body for any GET, optional delay.
    async fn mock_server(body: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(body.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}"), handle)
    }

    async fn dead_url() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}")
    }

    #[tokio::test]
    async fn resolve_without_store_or_host_is_identity() {
        let (_d, st) = store();
        assert_eq!(
            resolve_best_url(&st, "http://unknown:1").await,
            "http://unknown:1"
        );
    }

    #[tokio::test]
    async fn resolve_single_route_never_probes() {
        let (_d, st) = store();
        let dead = dead_url().await;
        st.upsert_configured("h1", "lab", &dead, RouteKind::Lan, now_secs())
            .unwrap();
        // single (dead) route: returned as-is, no race, no failure recorded
        assert_eq!(resolve_best_url(&st, &dead).await, dead);
        let host = st.host("h1").unwrap().unwrap();
        assert_eq!(host.routes[0].failures, 0);
    }

    #[tokio::test]
    async fn resolve_races_and_records() {
        let (_d, st) = store();
        let (live, guard) = mock_server(r#"{"status":"ok"}"#.to_string()).await;
        let dead = dead_url().await;
        let now = now_secs();
        st.upsert_configured("h1", "lab", &live, RouteKind::Lan, now)
            .unwrap();
        st.upsert_configured("h1", "lab", &dead, RouteKind::Lan, now)
            .unwrap();

        let best = resolve_best_url(&st, &dead).await; // enter via either URL
        guard.abort();
        assert_eq!(best, live);
        let host = st.host("h1").unwrap().unwrap();
        let live_r = host.routes.iter().find(|r| r.url == live).unwrap();
        let dead_r = host.routes.iter().find(|r| r.url == dead).unwrap();
        assert!(live_r.ewma_ms > 0.0, "winner EWMA recorded");
        assert!(dead_r.failures >= 1, "loser failure recorded");
    }

    #[tokio::test]
    async fn race_host_rejects_unknown() {
        let (_d, st) = store();
        assert!(race_host(&st, "nope").await.is_err());
    }

    #[tokio::test]
    async fn harvest_learns_announced_routes() {
        let (_d, st) = store();
        let body = serde_json::json!({
            "host_id": "h-remote-1",
            "routes": [
                {"url": "http://192.168.9.9:18790", "kind": "lan"},
                {"url": "http://100.64.1.2:18790", "kind": "tailnet"},
            ],
            "announced_at": 123
        })
        .to_string();
        let (url, guard) = mock_server(body).await;
        let learned = harvest(&st, &url, Some("k")).await.unwrap();
        guard.abort();
        let (host_id, n) = learned.expect("harvest merged");
        assert_eq!(host_id, "h-remote-1");
        assert_eq!(n, 2);
        let host = st.host("h-remote-1").unwrap().unwrap();
        assert_eq!(host.routes.len(), 2);
        assert!(host.routes.iter().all(|r| r.source == RouteSource::Learned));
    }

    #[tokio::test]
    async fn harvest_skips_keyless_suppression() {
        let (_d, st) = store();
        let body =
            serde_json::json!({"host_id": null, "routes": [], "note": "suppressed"}).to_string();
        let (url, guard) = mock_server(body).await;
        let out = harvest(&st, &url, None).await.unwrap();
        guard.abort();
        assert!(out.is_none());
        assert!(st.load().unwrap().hosts.is_empty());
    }
}
