//! Remote-access primitives for OpenCrabs: scoped pairing, session tokens,
//! managed cloudflared tunnels, and multi-route management.
//!
//! Adapted from t3code's T3 Connect for a single-owner, relay-free
//! deployment: the host daemon authenticates clients it paired with, and
//! clients reach it over whatever routes exist (LAN, tailnet, tunnel)
//! without a broker in the middle. Design reference:
//! `research/remote-phase1-plan.md` in the OpenCrabs home.

pub mod pairing;
pub mod route_client;
pub mod route_store;
pub mod routes;
pub mod scopes;
pub mod tunnel;

/// Unix seconds. Shared clock read for pairing + tunnel state files.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
