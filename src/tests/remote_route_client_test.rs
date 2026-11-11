use std::time::Duration;

use anyhow::Result;

use crate::remote::now_secs;
use crate::remote::route_store::RouteStore;

use crate::remote::route_client::*;
use crate::remote::route_store::RouteSource;
use crate::remote::routes::RouteKind;

fn store() -> (tempfile::TempDir, RouteStore) {
    let dir = tempfile::tempdir().unwrap();
    let st = RouteStore::new(dir.path().join("routes.json"));
    (dir, st)
}

/// Minimal HTTP responder: fixed body for any GET.
///
/// Reads the request head before answering. That is not politeness: a socket
/// closed with unread bytes still sitting in its receive buffer is reset
/// instead of finished, so the client's in-flight exchange dies with a
/// transport error instead of the response it was sent. Leaving the request
/// unread is what made `harvest_*` pass on macOS and fail on the Linux CI leg
/// (the two stacks disagree on what close() does with unread data). The
/// sibling `a2a_transport_cause_test` reads first for the same reason.
async fn mock_server(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Drain the request head. Bounded so a client that never
                // sends the terminator cannot hang the responder.
                let mut req = [0u8; 2048];
                let mut seen = 0usize;
                while seen < req.len() {
                    match sock.read(&mut req[seen..]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            seen += n;
                            if req[..seen].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
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

/// Bounded retry for `harvest` under full-suite CPU starvation (11k+
/// parallel tests can starve the 5s reqwest budget). Production `harvest`
/// stays a single shot; only tests can afford to re-attempt.
async fn harvest_retry(
    st: &RouteStore,
    url: &str,
    key: Option<&str>,
    attempts: u8,
) -> Result<Option<(String, usize)>> {
    let mut last = None;
    for i in 0..attempts {
        match harvest(st, url, key).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("harvest attempt {}/{} failed: {e:#}", i + 1, attempts);
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(300 * (u64::from(i) + 1))).await;
            }
        }
    }
    Err(last.expect("attempts > 0"))
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
    let learned = harvest_retry(&st, &url, Some("k"), 3).await.unwrap();
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
    let body = serde_json::json!({"host_id": null, "routes": [], "note": "suppressed"}).to_string();
    let (url, guard) = mock_server(body).await;
    let out = harvest_retry(&st, &url, None, 3).await.unwrap();
    guard.abort();
    assert!(out.is_none());
    assert!(st.load().unwrap().hosts.is_empty());
}
