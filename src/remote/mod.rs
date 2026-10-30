//! Remote-access primitives for OpenCrabs: scoped pairing, session tokens,
//! managed cloudflared tunnels, and multi-route management.
//!
//! Adapted from t3code's T3 Connect for a single-owner, relay-free
//! deployment: the host daemon authenticates clients it paired with, and
//! clients reach it over whatever routes exist (LAN, tailnet, tunnel)
//! without a broker in the middle. Design reference:
//! `research/remote-phase1-plan.md` in the OpenCrabs home.

pub mod scopes;
