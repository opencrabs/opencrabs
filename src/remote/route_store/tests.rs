//! Route store tests: staleness/timeout policy, EWMA, failure windows,
//! ordering, learn-merge/prune, promotion, persistence, URL lookup, and
//! live races against mock listeners on ephemeral loopback ports.

use std::time::Duration;

use super::*;

const NOW: u64 = 1_800_000_000;

fn store() -> (tempfile::TempDir, RouteStore) {
    let dir = tempfile::tempdir().unwrap();
    let st = RouteStore::new(dir.path().join("routes.json"));
    (dir, st)
}

fn entry(url: &str, source: RouteSource) -> RouteEntry {
    RouteEntry::new(url.to_string(), RouteKind::Lan, source, NOW)
}

fn announced(url: &str, kind: RouteKind) -> AnnouncedRoute {
    AnnouncedRoute {
        url: url.to_string(),
        kind,
    }
}

/// Mock A2A health endpoint on an ephemeral port; optional response delay.
async fn mock_health(delay: Duration) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let body = br#"{"status":"ok"}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

/// A URL whose port was just released: connect refuses promptly.
async fn dead_url() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    format!("http://{a}")
}

// ── staleness + ordering (pure) ────────────────────────────────────────────

#[test]
fn stale_routes_get_the_short_leash() {
    let mut e = entry("http://a", RouteSource::Learned);
    assert_eq!(e.probe_timeout(NOW), FRESH_TIMEOUT);
    e.failures = 1;
    e.last_fail = NOW - 10; // inside the window
    assert!(e.stale(NOW));
    assert_eq!(e.probe_timeout(NOW), STALE_TIMEOUT);
    e.last_fail = NOW - FAILURE_WINDOW_SECS - 1; // outside the window
    assert!(!e.stale(NOW));
    assert_eq!(e.probe_timeout(NOW), FRESH_TIMEOUT);
}

#[test]
fn ordering_puts_stale_last_then_fastest_first() {
    let mut slow = entry("http://slow", RouteSource::Learned);
    slow.ewma_ms = 200.0;
    let fast = {
        let mut e = entry("http://fast", RouteSource::Learned);
        e.ewma_ms = 20.0;
        e
    };
    let unmeasured = entry("http://fresh", RouteSource::Learned);
    let mut broken = entry("http://broken", RouteSource::Learned);
    broken.failures = 2;
    broken.last_fail = NOW - 5;
    broken.ewma_ms = 1.0; // fast history, bad standing → still last

    let ordered = order(
        &[
            slow.clone(),
            broken.clone(),
            unmeasured.clone(),
            fast.clone(),
        ],
        NOW,
    );
    let urls: Vec<_> = ordered.iter().map(|r| r.url.as_str()).collect();
    // measured-good (20ms) → measured-slower (200ms) → never-measured
    // (assumed 1000ms: unknown sorts behind known-decent) → stale last.
    assert_eq!(
        urls,
        vec![
            "http://fast",
            "http://slow",
            "http://fresh",
            "http://broken"
        ]
    );
}

// ── store mutations ────────────────────────────────────────────────────────

#[test]
fn success_sets_then_smooths_ewma_and_clears_failures() {
    let (_d, st) = store();
    st.upsert_configured("h1", "lab", "http://a", RouteKind::Lan, NOW)
        .unwrap();
    st.record_failure("http://a", NOW).unwrap();
    st.record_success("http://a", 100.0, NOW + 1).unwrap();
    let host = st.host("h1").unwrap().unwrap();
    let r = &host.routes[0];
    assert_eq!(r.ewma_ms, 100.0, "first measurement sets EWMA");
    assert_eq!(r.failures, 0, "success clears the streak");
    st.record_success("http://a", 200.0, NOW + 2).unwrap();
    let r = &st.host("h1").unwrap().unwrap().routes[0];
    let expected = EWMA_ALPHA * 200.0 + (1.0 - EWMA_ALPHA) * 100.0;
    assert!(
        (r.ewma_ms - expected).abs() < 1e-9,
        "EWMA blend: {}",
        r.ewma_ms
    );
}

#[test]
fn failure_streaks_are_windowed() {
    let (_d, st) = store();
    st.upsert_configured("h1", "lab", "http://a", RouteKind::Lan, NOW)
        .unwrap();
    st.record_failure("http://a", NOW).unwrap();
    st.record_failure("http://a", NOW + 1).unwrap();
    st.record_failure("http://a", NOW + 2).unwrap();
    assert_eq!(st.host("h1").unwrap().unwrap().routes[0].failures, 3);
    // a failure long after the window starts a fresh streak
    st.record_failure("http://a", NOW + 2 + FAILURE_WINDOW_SECS + 1)
        .unwrap();
    assert_eq!(st.host("h1").unwrap().unwrap().routes[0].failures, 1);
}

#[test]
fn learn_merges_union_and_prunes_only_learned() {
    let (_d, st) = store();
    st.upsert_configured("h1", "lab", "http://configured", RouteKind::Lan, NOW)
        .unwrap();
    st.learn(
        "h1",
        "lab",
        &[
            announced("http://configured", RouteKind::Lan),
            announced("http://lan-new", RouteKind::Lan),
        ],
        NOW,
    )
    .unwrap();
    let host = st.host("h1").unwrap().unwrap();
    assert_eq!(host.routes.len(), 2);
    assert_eq!(host.routes[0].source, RouteSource::Configured);
    assert_eq!(host.routes[1].source, RouteSource::Learned);

    // next announcement drops the learned route → still kept inside 24h
    st.learn(
        "h1",
        "lab",
        &[announced("http://configured", RouteKind::Lan)],
        NOW + 60,
    )
    .unwrap();
    assert_eq!(st.host("h1").unwrap().unwrap().routes.len(), 2);

    // ...but pruned once unseen past the window
    st.learn(
        "h1",
        "lab",
        &[announced("http://configured", RouteKind::Lan)],
        NOW + LEARNED_PRUNE_SECS + 120,
    )
    .unwrap();
    let host = st.host("h1").unwrap().unwrap();
    assert_eq!(host.routes.len(), 1, "stale learned route pruned");
    assert_eq!(
        host.routes[0].url, "http://configured",
        "configured survives"
    );
}

#[test]
fn upsert_promotes_learned_to_configured() {
    let (_d, st) = store();
    st.learn(
        "h1",
        "lab",
        &[announced("http://x", RouteKind::Tailnet)],
        NOW,
    )
    .unwrap();
    assert_eq!(
        st.host("h1").unwrap().unwrap().routes[0].source,
        RouteSource::Learned
    );
    st.upsert_configured("h1", "lab", "http://x/", RouteKind::Tailnet, NOW + 1)
        .unwrap();
    let host = st.host("h1").unwrap().unwrap();
    assert_eq!(
        host.routes.len(),
        1,
        "trailing slash normalizes to the same route"
    );
    assert_eq!(host.routes[0].source, RouteSource::Configured);
}

#[test]
fn find_host_by_url_normalizes() {
    let (_d, st) = store();
    st.upsert_configured("h-abc", "lab", "http://a:1", RouteKind::Lan, NOW)
        .unwrap();
    let (id, host) = st.find_host_by_url("http://a:1/").unwrap().unwrap();
    assert_eq!(id, "h-abc");
    assert_eq!(host.label, "lab");
    assert!(st.find_host_by_url("http://nope").unwrap().is_none());
}

// ── persistence ────────────────────────────────────────────────────────────

#[test]
fn persistence_roundtrip_permissions_and_corruption() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, st) = store();
    st.upsert_configured("h1", "lab", "http://a", RouteKind::Lan, NOW)
        .unwrap();
    let mode = std::fs::metadata(st.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let loaded = st.load().unwrap();
    assert_eq!(loaded.hosts.len(), 1);
    assert_eq!(loaded.hosts["h1"].routes[0].url, "http://a");

    std::fs::write(st.path(), b"{{{ nope").unwrap();
    assert!(st.load().is_err(), "corrupt store errors");
    assert!(
        st.upsert_configured("h1", "lab", "http://a", RouteKind::Lan, NOW)
            .is_err(),
        "and refuses writes rather than wiping"
    );
}

// ── live races (mock listeners) ────────────────────────────────────────────

#[tokio::test]
async fn race_picks_the_live_route_and_reports_the_dead_one() {
    let (live_url, live) = mock_health(Duration::ZERO).await;
    let dead = dead_url().await;
    let routes = vec![
        entry(&dead, RouteSource::Learned),
        entry(&live_url, RouteSource::Learned),
    ];

    let (winner, outcomes) = race_best(&routes, NOW).await;
    live.abort();
    assert_eq!(winner.as_deref(), Some(live_url.as_str()));
    let dead_out = outcomes
        .iter()
        .find(|o| o.url == dead)
        .expect("dead route completed");
    assert!(!dead_out.ok);
    assert!(outcomes.iter().any(|o| o.url == live_url && o.ok));
}

#[tokio::test]
async fn race_prefers_the_faster_healthy_route() {
    let (fast_url, fast) = mock_health(Duration::ZERO).await;
    let (slow_url, slow) = mock_health(Duration::from_millis(600)).await;
    let routes = vec![
        entry(&slow_url, RouteSource::Learned),
        entry(&fast_url, RouteSource::Learned),
    ];
    let (winner, _) = race_best(&routes, NOW).await;
    fast.abort();
    slow.abort();
    assert_eq!(winner.as_deref(), Some(fast_url.as_str()));
}

#[tokio::test]
async fn race_all_dead_has_no_winner() {
    let d1 = dead_url().await;
    let d2 = dead_url().await;
    let routes = vec![
        entry(&d1, RouteSource::Learned),
        entry(&d2, RouteSource::Learned),
    ];
    let (winner, outcomes) = race_best(&routes, NOW).await;
    assert!(winner.is_none());
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| !o.ok));
}

#[tokio::test]
async fn race_stale_route_cannot_stall_resolution() {
    // A stale route points at a black-hole address (TEST-NET-1, unroutable):
    // with the 1s stale leash the race must still finish quickly via the
    // live route, and well under the 3s fresh timeout.
    let (live_url, live) = mock_health(Duration::ZERO).await;
    let mut blackhole = entry("http://192.0.2.1:9", RouteSource::Learned);
    blackhole.failures = 1;
    blackhole.last_fail = NOW - 5; // stale
    assert_eq!(blackhole.probe_timeout(NOW), STALE_TIMEOUT);

    let started = Instant::now();
    let (winner, _) = race_best(&[blackhole, entry(&live_url, RouteSource::Learned)], NOW).await;
    let elapsed = started.elapsed();
    live.abort();
    assert_eq!(winner.as_deref(), Some(live_url.as_str()));
    assert!(
        elapsed < Duration::from_millis(1500),
        "race took {elapsed:?}"
    );
}
