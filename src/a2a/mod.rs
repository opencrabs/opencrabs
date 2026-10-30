//! A2A (Agent-to-Agent) Protocol implementation for OpenCrabs.
//!
//! Implements the A2A Protocol RC v1.0 specification:
//! - Agent Card discovery (`.well-known/agent.json`)
//! - JSON-RPC 2.0 task API (`message/send`, `tasks/get`, `tasks/cancel`)
//! - HTTP gateway server (axum)
//! - Multi-agent debate protocol (Bee Colony)

pub mod agent_card;
pub mod debate;
pub mod handler;
pub mod pairing_http;
pub mod persistence;
pub mod routes_http;
pub mod server;
pub mod types;

#[cfg(test)]
pub mod test_helpers;

pub use debate::run_debate;
pub use server::start_server;
