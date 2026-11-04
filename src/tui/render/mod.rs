//! TUI Rendering
//!
//! Main rendering logic for the terminal interface. [`frame`] is the
//! per-frame entry point, [`title`] the shared app title bar, and each
//! screen or widget has its own module. This file is declarations only —
//! no function definitions live here (CONTRIBUTING.md).

pub(crate) mod background;
pub(crate) mod chat;
pub(crate) mod chrome;
mod dialogs;
mod frame;

mod help;
pub(crate) mod hints;
mod input;
pub(crate) mod mission_control;
pub(crate) mod notice;
pub(crate) mod palette;
pub(crate) mod panes;
mod plan_overlay;
mod plan_widget;
pub(crate) mod plan_window;
pub(crate) mod profiles_dialog;
mod projects;
pub(crate) mod responsive;
mod session_files;
mod sessions;
pub(crate) mod skills_dialog;
pub(crate) mod theme;
pub(crate) mod theme_picker;
// Boot-apply in tui::runner references presets::by_name at startup.
pub(crate) mod presets;
mod title;
mod tools;
pub(crate) mod user_themes;
pub(crate) mod utils;

// Re-export for sibling modules (e.g. onboarding_render)
pub(in crate::tui) use utils::char_boundary_at_width;

// Re-export for tests
#[cfg(test)]
pub(crate) use chat::reasoning_to_lines;
#[cfg(test)]
pub(crate) use frame::{CHAT_FLOOR, plan_card_height};
#[cfg(test)]
pub(crate) use input::{DropdownFit, dropdown_dimensions, fit_dropdown, truncate_to_chars};
#[cfg(test)]
pub(crate) use tools::collapse_build_output;
#[cfg(test)]
pub(crate) use tools::detached_state_suffix;
#[cfg(test)]
pub(crate) use tools::unescape_display_string;

pub use frame::render;
