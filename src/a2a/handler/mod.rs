//! JSON-RPC 2.0 handler for A2A protocol operations.
//!
//! Dispatches JSON-RPC methods:
//! - `message/send`   → create task + process message via AgentService
//! - `session/notify` → post a notification into a live session's queue (#23)
//! - `tasks/get`      → retrieve task by ID
//! - `tasks/cancel`   → cancel a running task
//!
//! [`dispatch`] routes, [`stores`] holds the shared task maps, and each
//! method has its own module. This file is declarations only — no function
//! definitions live here (CONTRIBUTING.md).

mod dispatch;
pub(crate) mod notify;
mod send;
mod stores;
pub mod stream;
pub(crate) mod tasks;

pub use dispatch::dispatch;
// Only the handler tests drive the send path directly; dispatch reaches it as
// `send::handle_send_message`. Re-exporting it unconditionally is an unused
// import in the plain library build, which `-D warnings` refuses (#2874).
#[cfg(test)]
pub(crate) use send::handle_send_message;
pub use stores::{CancelStore, TaskStore, new_cancel_store, new_task_store};
