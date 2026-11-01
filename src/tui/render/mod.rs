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

// The turn-header grammar (#759) and the token label are pure formatting the
// desktop client renders too. Served from here so the gui cannot fork them
// into a second dialect that drifts the moment one side changes.
// `format_turn_spinner_meta` is the live head's suffix builder: the gui
// status line spells its clock and counter exactly as the TUI does.
pub use chat::{
    TurnSummary, format_step_count, format_turn_header, format_turn_spinner_meta,
    is_deliverable_report,
};
pub use utils::format_token_count_raw;
// The GUI's running-task strip is drawn with the TUI's own formatter and label
// width, so the two surfaces cannot disagree about what a running task looks
// like (#2322).
pub use background::{LABEL_CHARS, format_background_tasks};
// #2237: the wording and width maths for the mid-turn queue row live in
// `input`, which stays a private module. Re-export just what the tests pin.
// `#[cfg(test)]` because nothing in the library reads them through this path:
// `render_queue` calls the helper directly, so an ungated re-export is a dead
// import for every non-test build and trips `-D warnings`.
#[cfg(test)]
pub(crate) use input::queue_line_parts;
