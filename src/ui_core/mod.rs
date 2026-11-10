//! Terminal-neutral UI core shared by every frontend (TUI today, GPUI GUI later).
//!
//! Owns pure UI logic: data loading, view models, state machines, and
//! key-neutral input decisions. Nothing in this module may depend on
//! ratatui, crossterm, or any other terminal-specific crate, so a GUI
//! binary can link it as-is.

pub mod actions;
pub mod bang;
// `card` is the shared card shape and reads `gpui` types, so it only
// compiles with the feature that provides them (#2510). Every consumer is
// in `src/gui/`.
#[cfg(feature = "gui")]
pub mod card;
pub mod channel_status;
pub mod commands;
pub mod composer;
pub mod copy_select;
pub mod docs;
pub mod help_keys;
pub mod input_history;
pub mod keys;
pub mod links;
pub mod message_select;
pub mod model_layout;
pub mod model_selector;
pub mod palette;
pub mod pane_tree;
pub mod plan_history;
pub mod plan_index;
pub mod query;
pub mod rail;
pub mod rate;
pub mod scroll;
pub mod session_preview;
pub mod split;
pub mod topic_bar;
pub mod usage;
