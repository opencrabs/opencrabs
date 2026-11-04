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
pub(crate) mod ledger_scan;
pub(crate) mod messaging;
pub mod mission_control;
pub(crate) mod mouse_frag;
pub mod profiles_dialog;
pub(crate) mod reasoning_split;
pub mod skills_dialog;
pub(crate) mod state;

pub use background_session::{BackgroundSessionState, SessionStateMut};
pub use state::*;

// Re-export sibling modules so sub-modules can use `super::events`, etc.
pub(crate) use super::events;
pub(crate) use super::onboarding;
