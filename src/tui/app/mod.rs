//! App Module — TUI application state and logic.

pub(crate) mod answer_placement;
pub(crate) mod background_session;
pub(crate) mod clickable;
pub(crate) mod clipboard_route;
pub(crate) mod dialog_keys;
pub(crate) mod dialogs;
pub(crate) mod dropped_path;
pub(crate) mod duplicate_submit;
pub(crate) mod help_catalog;
pub(crate) mod input;
/// Reused by the gui transcript: the ledger scanner is the TUI message
/// grammar, and the gui mirrors the TUI instead of forking it.
pub mod ledger_scan;
pub(crate) mod messaging;
pub mod mission_control;
pub(crate) mod mouse_frag;
pub mod profiles_dialog;
/// Reused by the gui transcript for thinking/narration parsing, same reason
/// as `ledger_scan` above.
pub mod reasoning_split;
pub mod skills_dialog;
pub(crate) mod state;

pub use background_session::{BackgroundSessionState, SessionStateMut};
pub use state::*;

// Re-export sibling modules so sub-modules can use `super::events`, etc.
pub(crate) use super::events;
pub(crate) use super::onboarding;
