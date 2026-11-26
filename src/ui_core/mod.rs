//! Terminal-neutral UI core shared by every frontend (TUI today, GPUI GUI later).
//!
//! Owns pure UI logic: data loading, view models, state machines, and
//! key-neutral input decisions. Nothing in this module may depend on
//! ratatui, crossterm, or any other terminal-specific crate, so a GUI
//! binary can link it as-is.

pub mod actions;
pub mod bang;
// The arithmetic a query becomes when it is a calculation rather than a
// search: pure, no toolkit, so both frontends answer the same keystrokes.
pub mod calc;
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
// The text behind the built-in editor the bang operator opens (#3163).
pub mod editor_buffer;
pub mod help_keys;
// The line under the composer that says what to do next, shared so the
// terminal and the desktop app cannot recommend different things (#2279).
pub mod hints;
pub mod input_history;
pub mod keys;
pub mod links;
pub mod local_document;
pub mod message_reader;
pub mod message_select;
// The cmd/ctrl+1..9 jump: pure id arithmetic so the terminal and the desktop
// window pick the same nth topic or session (#3166).
pub mod model_layout;
pub mod model_selector;
pub mod numbered_jump;
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
pub mod text_field;
pub mod topic_bar;
pub mod usage;
// Which window controls a window draws itself, and what each maps to
// (#2813). The drawing needs a window and lives in the GUI binary; the
// decision reads only `gpui` types, so it is gated on the same feature as
// `card`.
#[cfg(feature = "gui")]
pub mod window_controls;
// Where a window's frame lands when it opens (#3082). Same gating: the frame
// is arithmetic over `gpui` rectangles, and the platform call that supplies the
// work area lives in the GUI binary.
#[cfg(feature = "gui")]
pub mod window_geometry;
