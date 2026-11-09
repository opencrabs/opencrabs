//! Terminal User Interface
//!
//! Provides an interactive terminal interface for the AI orchestration agent using Ratatui.

pub mod app;
pub(crate) mod capture;
/// Shared by every frontend: the window prints the same `/clear` line the
/// terminal does.
pub mod clear_notice;
/// Shared by every frontend, same reason as `clear_notice`.
pub mod compact_notice;
/// Unix-only (#1744, #1755): the handoff reaps with libc `waitpid(WUNTRACED)`
/// and coerces stops with SIGCONT/SIGTERM/SIGKILL, none of which the windows
/// libc crate carries. On Windows the module compiles out and bang commands
/// keep the pipe-capture path.
#[cfg(unix)]
pub mod editor;
pub mod error;
pub mod events;
/// One source of truth for the input box geometry: visual rows, the byte
/// under a screen cell, and the text between two cells. Click to place the
/// caret, drag to copy and the selection highlight all read it, so they
/// cannot disagree.
pub mod input_layout;
/// Detached process launcher (#2170, #2172): spawns with nulled stdio, reaps
/// on a background thread and reports a non-zero exit, so the draw loop never
/// blocks and a failed open is not reported as a success.
pub(crate) mod launcher;
#[cfg(test)]
pub(crate) mod model_order;
pub mod onboarding;
pub mod onboarding_layout;
pub mod onboarding_render;
pub mod pane;
pub mod plan;
pub mod provider_selector;
pub mod remote_upload;
pub mod render;
pub mod runner;
/// The session-list filter (plan chip + cron/RSI visibility) shared by the
/// dialog's keys and its render (#2723).
pub mod session_filter;
pub mod theme_catalog;

// Enhanced rendering modules
pub mod highlight;
pub mod markdown;

pub mod components;

// Re-exports
pub use app::{App, DisplayMessage};
pub use events::{AppMode, EventHandler, TuiEvent};
pub use runner::run;
