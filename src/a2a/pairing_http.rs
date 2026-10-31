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
//! bounced through a third party. Client identity for the rate limit comes
//! from `CF-Connecting-IP` (set by the Cloudflare edge on tunnel traffic),
//! else the first `X-Forwarded-For` hop, else a shared `"direct"` bucket —
//! fine for a single-owner gateway on a private tailnet/LAN.
//!
//! The store call is a small blocking file read inside an async handler;
//! at A2A request rates (agent-to-agent, not eyeball traffic) that is the
//! right trade against dragging in an async runtime for file I/O.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::a2a::server::A2aState;
use crate::remote::now_secs;
use crate::remote::pairing::VerifyError;

/// Pairing attempts allowed per client key per 1-minute fixed window.
pub const PAIR_RATE_LIMIT: u32 = 10;

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

/// Client bucket key: edge IP header first, then first proxy hop, else
/// `"direct"`.
pub fn client_key(headers: &HeaderMap) -> String {
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

/// POST /a2a/pair — exchange a one-time code for a scoped session token.
pub async fn handle_pair(
    State(state): State<A2aState>,
    headers: HeaderMap,
    Json(body): Json<PairRequest>,
) -> Response {
    let now = now_secs();
    let key = client_key(&headers);

    {
        // A poisoned lock (a panicked holder) must not wedge pairing forever.
        let mut map = state.pair_hits.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(retry_after) = bump(&mut map, &key, now) {
            return rate_limited(retry_after, "too many pairing attempts, slow down");
        }
    }

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
        assert_eq!(client_key(&h), "9.9.9.9");
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        assert_eq!(client_key(&h), "1.2.3.4");
    }

    #[test]
    fn client_key_falls_back_to_direct() {
        assert_eq!(client_key(&HeaderMap::new()), "direct");
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "  ".parse().unwrap());
        assert_eq!(client_key(&h), "direct");
    }
}
