//! Mission Control rendering — top-level entry + per-panel renderers.
//!
//! See `src/brain/mission_control/` for the matching data services and
//! `src/tui/app/mission_control/` for the input/state layer. Each panel
//! is rendered by its own submodule so the file boundaries match the
//! visual ones — adding a 4th panel is a new file, not a diff inside
//! a 1k-line `dispatch.rs`.

mod activity_panel;
mod analytics_panel;
mod detail_popup;
mod dispatch;
mod inbox_panel;
mod layout;
mod log_viewer;
mod schedule_panel;
mod theme;

pub use dispatch::draw;

#[cfg(test)]
pub(crate) use analytics_panel::render as render_analytics;
#[cfg(test)]
pub(crate) use layout::{McLayout, compute_focused};
