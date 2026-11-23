//! Tests for the window module, kept beside it instead of inline in `main.rs`.
//!
//! The GUI is a separate binary crate, so these files are siblings of the
//! modules they exercise and reach internals through `crate::` paths. They
//! live here rather than in `src/tests/` because that tree belongs to the
//! library, which cannot see the window at all.

mod composer_orb_test;
// The bang operator's built-in editor (#3163), read as source text.
mod editor_dialog_test;
mod layout_guard_test;
mod main_test;
mod plan_strip_test;
mod pool_door_test;
mod prompt_files_test;
mod sidebar_action_test;
mod voice_note_row_test;
mod window_test;
