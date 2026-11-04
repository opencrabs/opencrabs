//! Terminal User Interface
//!
//! Provides an interactive terminal interface for the AI orchestration agent using Ratatui.

pub mod app;
pub(crate) mod capture;
pub(crate) mod clear_notice;
pub(crate) mod compact_notice;
/// Unix-only (#1744, #1755): the handoff reaps with libc `waitpid(WUNTRACED)`
/// and coerces stops with SIGCONT/SIGTERM/SIGKILL, none of which the windows
/// libc crate carries. On Windows the module compiles out and bang commands
/// keep the pipe-capture path.
#[cfg(unix)]
pub mod editor;
pub mod error;
pub mod events;
/// Detached process launcher (#2170, #2172): spawns with nulled stdio, reaps
/// on a background thread and reports a non-zero exit, so the draw loop never
/// blocks and a failed open is not reported as a success.
pub(crate) mod launcher;
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
pub mod theme_catalog;

// Enhanced rendering modules
pub mod highlight;
pub mod markdown;

pub mod components;

// Re-exports
pub use app::{App, DisplayMessage};
pub use events::{AppMode, EventHandler, TuiEvent};
pub use runner::run;
