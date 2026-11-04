//! What `/clear` tells the user, and when (#1585).
//!
//! The wording lives here so the command handler only decides which line
//! applies, the same split `compact_notice` keeps for `/compact`.

use crate::brain::agent::service::clear::ClearReceipt;

/// `/clear` typed while a turn is running. The running turn would persist
/// its answer after the marker and the cut would land mid-work, so the
/// user is asked to cancel first.
pub fn busy() -> String {
    "A turn is running. Cancel it first (Esc twice or /stop), then run /clear.".to_string()
}

/// No session to clear.
pub fn no_session() -> String {
    "No active session to clear.".to_string()
}

/// The marker is in; the footer has been reset to the baseline.
pub fn cleared(receipt: &ClearReceipt) -> String {
    receipt.user_line()
}

/// The marker could not be written. Never silent: the user typed a command
/// and must learn that the context is unchanged.
pub fn failed(err: &dyn std::fmt::Display) -> String {
    format!("/clear did nothing, the context is unchanged: {err}")
}
