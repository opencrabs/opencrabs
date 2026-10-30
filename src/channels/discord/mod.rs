//! Discord Integration
//!
//! Runs a Discord bot alongside the TUI, forwarding messages from
//! allowlisted users to the AgentService and replying with responses.
//!
//! Layout: [`state`] holds the shared `DiscordState` struct and its
//! constructor; each concern has its own impl module beside it
//! (`approval`, `cancel`, `connection`, `pending_interactions`,
//! `sessions`, and the tool-group methods in `tool_group`); `agent` runs
//! the gateway, `handler` routes inbound messages, `interactions` /
//! `reactions` / `suggest_options` / `typing` handle their UI surfaces,
//! `poll` validates the native-poll spec (#1848), and `resume` re-delivers
//! background results. This file is declarations
//! only — no function definitions live here (CONTRIBUTING.md).

pub(crate) mod access;
mod agent;
mod approval;
mod cancel;
pub(crate) mod commands;
mod connection;
pub(crate) mod cowork;
pub(crate) mod governor;
pub(crate) mod group_bubble;
pub(crate) mod handler;
pub(crate) mod interactions;
pub(crate) mod options_host;
pub(crate) mod outbox;
pub(crate) mod pending_interactions;
pub(crate) mod plan_card;
pub(crate) mod poll;
pub(crate) mod reactions;
pub(crate) mod resume;
mod sessions;
mod state;
pub(crate) mod suggest_options;
pub(crate) mod table_convert;
pub(crate) mod tool_card;
pub(crate) mod tool_card_store;
pub(crate) mod tool_group;
pub(crate) mod trace_answer;
pub(crate) mod tracked_turn;
pub(crate) mod turns;
pub(crate) mod typing;

pub use agent::DiscordAgent;
pub use state::DiscordState;
