//! Authenticated route announcement: `GET /a2a/routes` plus the auth-gated
//! `routes` block on `/a2a/health`.
//!
//! Privacy invariant: routes are announced only when `[a2a].api_key` is
//! configured AND the caller presented a valid bearer with `a2a:read`.
//! Health itself stays public — the tunnel exposes it to the open internet,
//! and LAN topology is not for anonymous eyes. A keyless gateway suppresses
//! announcements entirely: keyless means anyone who can reach the port is
//! "authenticated", which is exactly who must NOT learn the LAN layout.

use axum::Extension;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::a2a::server::{A2aState, GrantedScopes};
use crate::remote::now_secs;
use crate::remote::routes::{
    AnnouncedRoute, collect_interfaces, host_id, select_advertised_routes,
};
use crate::remote::scopes::{SCOPE_READ, allows};
use crate::remote::tunnel;

/// Gather what this host can announce: persistent host_id + lan/tailnet
/// routes from live interfaces + the tunnel URL when one is running.
pub fn announced_routes(state: &A2aState) -> anyhow::Result<(String, Vec<AnnouncedRoute>)> {
    let state_dir = state.remote_state_dir.as_path();
    let ifaces = collect_interfaces();
    // A dead tunnel's state self-cleans inside status(); announce only live ones.
    let tunnel_url = tunnel::status(state_dir)?.map(|t| t.url);
    let routes = select_advertised_routes(&ifaces, state.port, tunnel_url.as_deref());
    Ok((host_id(state_dir)?, routes))
}

/// GET /a2a/routes — canonical announcement for route-manager clients.
/// Lives on the protected router, so a valid bearer already cleared
/// `require_bearer`; the read-scope check here is the second half.
pub async fn handle_routes(
    State(state): State<A2aState>,
    Extension(granted): Extension<GrantedScopes>,
) -> Response {
    if state.api_key.is_none() {
        // Keyless posture: suppression is policy, not an error.
        return Json(serde_json::json!({
            "host_id": null,
            "routes": [],
            "note": "route announcement requires [a2a].api_key to be set",
        }))
        .into_response();
    }
    if !allows(&granted.0, SCOPE_READ) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": format!("insufficient scope: {SCOPE_READ} required")
            })),
        )
            .into_response();
    }
    match announced_routes(&state) {
        Ok((hid, routes)) => Json(serde_json::json!({
            "host_id": hid,
            "routes": routes,
            "announced_at": now_secs(),
        }))
        .into_response(),
        Err(e) => {
            tracing::error!("route announcement failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "route announcement failed" })),
            )
                .into_response()
        }
    }
}
