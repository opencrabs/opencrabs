//! Tests for the window module, kept beside it instead of inline in `main.rs`.
//!
//! The GUI is a separate binary crate, so these files are siblings of the
//! modules they exercise and reach internals through `crate::` paths. They
//! live here rather than in `src/tests/` because that tree belongs to the
//! library, which cannot see the window at all.

mod backend_remote_test;
mod blob_cache_test;
mod command_gate_test;
mod composer_orb_test;
mod composer_prompt_test;
mod crab_switch_reload_test;
mod crab_window_test;
mod dashboard_turns_test;
// The bang operator's built-in editor (#3163), read as source text.
mod editor_dialog_test;
mod instance_glyph_test;
mod instances_test;
mod instances_wiring_test;
mod layout_guard_test;
mod local_actions_test;
mod localized_paths_test;
mod main_test;
mod menu_test;
mod mission_control_gate_test;
mod models_picker_test;
mod plan_strip_test;
mod pool_door_test;
mod prompt_files_test;
// A remote crab asking before it runs a tool (C8).
mod remote_approval_test;
mod remote_watch_wiring_test;
mod session_restore_test;
mod share_address_test;
mod sidebar_action_test;
mod topic_icon_files_test;
mod transcript_tile_files_test;
mod usage_load_test;
mod voice_crab_test;
mod voice_note_row_test;
mod window_test;
