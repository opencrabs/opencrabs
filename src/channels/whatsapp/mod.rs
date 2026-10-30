//! WhatsApp Integration
//!
//! Runs a WhatsApp Web client alongside the TUI, forwarding messages from
//! allowlisted phone numbers to the AgentService and replying with responses.
//!
//! Layout: [`state`] holds the shared `WhatsAppState` struct and its
//! constructor; each concern has its own impl module beside it
//! (`approval`, `cancel`, `connection`, `followups`, `onboarding_events`,
//! `pairing`, `photos`, `sessions`); `agent` runs the client event loop,
//! `handler` routes inbound messages, `resume` re-delivers background
//! results and `store` persists the session. This file is declarations
//! only — no function definitions live here (CONTRIBUTING.md).

pub(crate) mod agent;
mod approval;
pub(crate) mod blocklist;
pub(crate) mod broadcast;
mod cancel;
mod connection;
pub(crate) mod ephemeral;
pub(crate) mod flow_message;
pub(crate) mod flow_render;
mod followups;
pub(crate) mod handler;
pub(crate) mod history;
pub(crate) mod identity;
pub(crate) mod inbound;
pub(crate) mod interactive;
pub(crate) mod lid_migration;
pub(crate) mod media_retry;
pub(crate) mod newsletter;
mod onboarding_events;
pub(crate) mod outbox;
mod pairing;
mod photos;
pub(crate) mod poll;
pub(crate) mod rate_limit;
pub(crate) mod reaction;
pub(crate) mod recent;
pub(crate) mod resume;
mod sessions;
mod state;
pub(crate) mod store;
pub(crate) mod stream;
pub(crate) mod turns;
pub(crate) mod voice_note;

pub use agent::WhatsAppAgent;
pub use approval::WaApproval;
pub use state::WhatsAppState;
