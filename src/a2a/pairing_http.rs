//! Public pairing endpoint for the A2A gateway.
//!
//! `POST /a2a/pair {"code","label"}` exchanges a one-time code (minted by
//! `opencrabs connect` on the host) for a scoped session token. This route
//! is deliberately UNAUTHENTICATED — the code is the credential — so it is
//! hardened three ways:
//!
//! 1. Fixed-window per-client rate limit (in-memory, [`PAIR_RATE_LIMIT`]
//!    per minute) in front of any store access.
//! 2. The store's own global fail-streak lockout (10 misses = 15 min),
//!    surfaced as 429 + `Retry-After`.
//! 3. Single-use codes with a 5-minute TTL; every failure reads the same
//!    (invalid / expired / used), so failures leak nothing.
//!
//! Responses are plain JSON — never a redirect, so a code or token can't be
//! bounced through a third party. Client identity for the rate limit is a
//! trust rule, not a header read: forwarded IPs (`CF-Connecting-IP`,
//! `X-Forwarded-For`) are honored only when the connection itself comes
//! from loopback — a local reverse proxy (the cloudflared tunnel)
//! speaking on behalf of a remote client, the one case where the real
//! client IP exists only in a header. Any other peer is the client
//! itself, and headers it forged are ignored: rotating fake `XFF` values
//! otherwise mints a fresh rate-limit bucket per request. The limit runs
//! as a `from_fn` layer ahead of the route's body extraction, so even
//! malformed-JSON rejections pay the toll (axum's `Json` extractor would
//! reject those before any handler code, making 400-spam unthrottled);
//! the bucket map is swept of rolled windows once it hits its cap, so a
//! spoofing scan cannot grow it forever.
//!
//! The store call is a small blocking file read inside an async handler;
//! at A2A request rates (agent-to-agent, not eyeball traffic) that is the
//! right trade against dragging in an async runtime for file I/O.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::a2a::server::A2aState;
use crate::remote::now_secs;
use crate::remote::pairing::VerifyError;

/// Pairing attempts allowed per client key per 1-minute fixed window.
pub const PAIR_RATE_LIMIT: u32 = 10;

/// Keys tracked before the map is swept of rolled windows. Generous for an
/// agent gateway; the point is bounded memory under spoofing scans.
pub const MAX_TRACKED_KEYS: usize = 1024;

/// In-memory rate-limit state: client key → (window minute, hit count).
/// Per-`A2aState` (not a global static) so tests never share buckets.
pub type PairHits = Arc<Mutex<HashMap<String, (u64, u32)>>>;

pub fn new_pair_hits() -> PairHits {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Debug, Deserialize)]
pub struct PairRequest {
    pub code: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub token: String,
    pub scopes: Vec<String>,
    pub expires_at: u64,
}

/// Fixed-window rate-limit bump. `Ok(remaining)` when allowed,
/// `Err(retry_after_secs)` when the window budget is spent. Pure over the
/// map + clock values so tests drive it without sleeping.
fn bump(map: &mut HashMap<String, (u64, u32)>, key: &str, now_secs: u64) -> Result<u32, u64> {
    let now_min = now_secs / 60;
    if map.len() >= MAX_TRACKED_KEYS {
        // A rolled fixed window can never come back: sweep it. Without
        // this, a spoofed-header scan grows the map one key per request,
        // forever.
        map.retain(|_, (m, _)| *m == now_min);
    }
    let entry = map.entry(key.to_string()).or_insert((now_min, 0));
    if entry.0 != now_min {
        *entry = (now_min, 0);
    }
    entry.1 += 1;
    if entry.1 > PAIR_RATE_LIMIT {
        Err(60 - (now_secs % 60))
    } else {
        Ok(PAIR_RATE_LIMIT - entry.1)
    }
}

/// Client bucket key, by trust: forwarded headers only from a loopback
/// peer (a local reverse proxy speaking for a remote client — the
/// Cloudflare tunnel — the one case where the real client IP exists only
/// in a header); the socket peer for everyone else, since a non-loopback
/// client's `X-Forwarded-For` is self-asserted and exists only to mint
/// fresh rate-limit buckets. `None` (no address available: unit tests,
/// exotic servers) keeps the header path rather than guessing.
pub fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    match peer.map(|p| p.ip()) {
        Some(ip) if ip.is_loopback() => forwarded_key(headers),
        Some(ip) => format!("peer:{ip}"),
        None => forwarded_key(headers),
    }
}

/// Edge IP header first, then first proxy hop, else `"direct"`.
fn forwarded_key(headers: &HeaderMap) -> String {
    if let Some(v) = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        && !v.trim().is_empty()
    {
        return v.trim().to_string();
    }
    if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let first = v.split(',').next().unwrap_or("").trim();
        if !first.is_empty() {
            return first.to_string();
        }
    }
    "direct".to_string()
}

/// POST /a2a/pair rate-limit layer. Runs AHEAD of the route's body
/// extraction (from_fn wraps the whole route, handler included), so even
/// malformed-JSON 400 rejections pay the toll — axum's `Json` extractor
/// would otherwise reject the request before any handler code, giving
/// unthrottled free 400 spam. Only `POST /a2a/pair` is metered; the
/// health and discovery routes pass through untouched.
pub async fn pair_rate_limit(
    State(hits): State<PairHits>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    if req.uri().path() != "/a2a/pair" || req.method() != "POST" {
        return next.run(req).await;
    }
    let now = now_secs();
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    let key = client_key(req.headers(), peer);
    {
        // A poisoned lock (a panicked holder) must not wedge pairing forever.
        let mut map = hits.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(retry_after) = bump(&mut map, &key, now) {
            return rate_limited(retry_after, "too many pairing attempts, slow down");
        }
    }
    next.run(req).await
}

/// POST /a2a/pair — exchange a one-time code for a scoped session token.
/// Rate limiting happens in [`pair_rate_limit`], before this handler.
pub async fn handle_pair(State(state): State<A2aState>, Json(body): Json<PairRequest>) -> Response {
    match state.pairing.verify_code(&body.code, &body.label) {
        Ok((token, session)) => {
            tracing::info!(label = %session.label, session = %session.id, "A2A pairing: session issued");
            (
                StatusCode::OK,
                Json(PairResponse {
                    token,
                    scopes: session.scopes,
                    expires_at: session.expires_at,
                }),
            )
                .into_response()
        }
        Err(VerifyError::NoMatch) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "invalid, expired, or already-used pairing code"
            })),
        )
            .into_response(),
        Err(VerifyError::Locked { retry_after_secs }) => {
            rate_limited(retry_after_secs, "pairing locked after repeated failures")
        }
        Err(VerifyError::Storage(e)) => {
            tracing::error!("pairing store failure: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "pairing store unavailable" })),
            )
                .into_response()
        }
    }
}

fn rate_limited(retry_after_secs: u64, message: &str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", retry_after_secs.to_string())],
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_allows_up_to_limit_then_blocks() {
        let mut map = HashMap::new();
        for i in 0..u64::from(PAIR_RATE_LIMIT) {
            assert!(bump(&mut map, "1.2.3.4", 1_800_000_000 + i).is_ok());
        }
        let err = bump(&mut map, "1.2.3.4", 1_800_000_010).unwrap_err();
        assert!(
            (1..=60).contains(&err),
            "retry_after within the minute: {err}"
        );
    }

    #[test]
    fn bump_resets_on_window_roll() {
        let mut map = HashMap::new();
        let t0 = 1_800_000_000u64; // some minute
        for _ in 0..PAIR_RATE_LIMIT {
            bump(&mut map, "k", t0).unwrap();
        }
        assert!(bump(&mut map, "k", t0).is_err());
        let next_minute = (t0 / 60 + 1) * 60;
        assert!(
            bump(&mut map, "k", next_minute).is_ok(),
            "new window resets"
        );
    }

    #[test]
    fn bump_keys_are_independent() {
        let mut map = HashMap::new();
        for _ in 0..PAIR_RATE_LIMIT {
            bump(&mut map, "a", 60).unwrap();
        }
        assert!(bump(&mut map, "a", 60).is_err());
        assert!(bump(&mut map, "b", 60).is_ok());
    }

    #[test]
    fn client_key_prefers_edge_header() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "9.9.9.9, 8.8.8.8".parse().unwrap());
        assert_eq!(client_key(&h, None), "9.9.9.9");
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        assert_eq!(client_key(&h, None), "1.2.3.4");
        // Loopback peer = a local reverse proxy: headers are the client's.
        let lo: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert_eq!(client_key(&h, Some(lo)), "1.2.3.4");
    }

    #[test]
    fn client_key_falls_back_to_direct() {
        assert_eq!(client_key(&HeaderMap::new(), None), "direct");
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "  ".parse().unwrap());
        assert_eq!(client_key(&h, None), "direct");
    }

    #[test]
    fn client_key_off_loopback_uses_the_socket_peer_not_forgeable_headers() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "9.9.9.9, 8.8.8.8".parse().unwrap());
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        let peer: SocketAddr = "100.64.1.7:5150".parse().unwrap();
        assert_eq!(client_key(&h, Some(peer)), "peer:100.64.1.7");
    }

    #[test]
    fn bump_sweeps_rolled_windows_once_the_map_hits_the_cap() {
        let mut map = HashMap::new();
        for i in 0..MAX_TRACKED_KEYS {
            map.insert(format!("spoof{i}"), (100, 1));
        }
        // A bump at a later minute sweeps the stale sea before inserting.
        assert!(bump(&mut map, "real", 100 * 60 + 61).is_ok());
        assert!(
            map.len() == 1 && map.contains_key("real"),
            "stale spoof keys evicted, got {} keys",
            map.len()
        );
    }
}
