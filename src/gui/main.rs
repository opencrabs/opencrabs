//! OpenCrabs desktop client: the same sessions, chat and usage as the
//! terminal client, drawn with GPUI.
//!
//! Built only with `--features gui` (`required-features` on the `[[bin]]`), so
//! the TUI build and the CI gate never link gpui.
//!
//! The window has two panes: the session rail (every real session, most
//! recent first) and the right pane, which is the chat by default, with the
//! newest conversation already open. ctrl+u swaps the right pane to the real
//! usage ledger and Esc (or ctrl+u again) returns to the conversation; the two
//! screens are never mixed. There is no sample data anywhere: when a read
//! cannot happen, the pane says why.
//!
//! Keys go through the neutral layer where a TUI analogue exists: a gpui
//! `KeyDownEvent` is translated into a `UiKey` at this edge and the usage
//! decision is `ui_core::usage::input::decide`, the same function the TUI
//! runs. The footer's hint labels come from the same `advertises` rule set.
//! GUI-only concerns live outside that vocabulary: the zoom keys, the scroll
//! containers, and rail clicks.
//!
//! Long shell commands detach instead of blocking the pane. A running one gets
//! its own row in the live turn and a `⏳` strip above the input, both drawn
//! from the same formatter the TUI uses. When it finishes, the row settles by
//! task id and the conversation resumes with the output (#2322). A completion
//! resumes the session that started the task, whether or not its conversation
//! is the one on screen (#2704).

mod accent;
mod agent_bridge;
mod app;
mod attachment_view;
mod bang;
mod boot;
mod breakdown;
mod channel_chip;
mod chat_leaf;
mod command_palette;
mod commands;
mod composer_view;
mod config_reload;
mod dashboard_commands;
mod dashboard_config;
mod dashboard_finder;
mod dashboard_keys;
mod dashboard_messages;
mod dashboard_models;
mod dashboard_nav;
mod dashboard_panes;
mod dashboard_projects;
mod dashboard_rail;
mod dashboard_render;
mod dashboard_screens;
mod dashboard_sessions;
mod dashboard_settings;
#[cfg(test)]
mod dashboard_source;
mod dashboard_state;
mod dashboard_topics;
mod dashboard_turns;
mod editor;
// The built-in editor the bang operator opens (#3163).
mod editor_dialog;
mod fonts;
mod help_view;
mod hint;
mod hint_dismiss;
mod icon;
mod launch;
mod layout_store;
mod live_fold;
mod logging;
mod markdown;
mod menu;
mod middle_scroll;
mod mission_control;
mod mission_control_view;
mod modal_shell;
mod model_fetch;
mod model_picker;
mod models;
mod models_modal;
mod onboard;
mod overlays;
mod palette;
mod pane_chrome;
mod pane_focus;
mod pane_layout;
mod pane_view;
mod plan_strip;
mod platform_icons;
mod preview;
mod preview_document;
mod projects;
mod projects_screen;
mod projects_view;
mod prompt_files;
mod rail_activity;
// The rail is an entity of its own (#2933): its rows, chips, badges and
// filter menu are built once per frame it is handed, and a frame that matches
// the last one replays the prepaint instead of rebuilding every listener.
mod composer_orb;
mod rail_view;
mod screen_keys;
mod seeker_orb;
mod sessions;
mod settings_modal;
mod settings_view;
#[cfg(test)]
mod settings_view_test;
mod skills_view;
mod status;
mod task_detail;
mod task_list;
mod task_strip;
mod telegram_topics;
#[cfg(test)]
mod tests;
mod text_commands;
mod theme;
mod themes_view;
mod thumbs;
mod topic_bar_view;
mod topic_icons;
mod topic_tabs;
mod transcript;
mod turn_swap;
mod usage_load;
mod usage_view;
mod voice_input;
mod voice_media;
mod voice_play;
mod voice_preview;
mod window_controls;
mod window_geometry;
mod window_input;

use crate::composer_view::{command_popup_view, emoji_popup_view, options_strip, queue_strip};
use crate::dashboard_panes::{DividerDrag, PaneDrag, PaneGhost, splitter};
use crate::dashboard_rail::{RAIL_DEFAULT_REM, RAIL_MAX_REM, RAIL_MIN_REM};
use crate::dashboard_state::{Dashboard, FooterPlace, Screen, SendOrigin};
use crate::help_view::{CommandLists, HelpView, help_screen};
use crate::mission_control_view::{
    mission_control_body, mission_control_detail, mission_control_logs,
};
use crate::overlays::{
    DEFAULT_TOPIC_ICON, TopicPick, cd_entries_for, confirm_delete_overlay,
    confirm_disable_topics_overlay, split_pick_overlay, topic_info_overlay, topic_pick,
    topic_prompt_overlay, update_prompt_overlay, warning_overlay,
};
use crate::pane_chrome::{
    central, chat_measure, pane_composer_dim, rotation_chord, split_chord, split_pane_width,
    window_footer_yields,
};
use crate::pane_layout::{PanesCtx, pane_element};
use crate::pane_view::PaneView;
use crate::screen_keys::{
    ChordMods, FocusTarget, KeyboardOwner, OverlayId, ScreenAction, focus_target, keyboard_owner,
    screen_chord,
};
use crate::status::status_identity;
use crate::theme::{BASE_REM, Size, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP, text};
use crate::topic_bar_view::{overflow_chip, topic_label, topic_strip, topic_tab};

// The window's boot, input, launch and fold helpers live beside this file
// (#3060), imported here so the sibling modules keep reaching them through
// the crate root.

use crate::boot::{LoadState, boot_config, ledger_path};
use crate::channel_chip::{
    channel_chip_line, channel_chip_line_from, configured_channels, configured_channels_from,
};
use crate::launch::{
    Launch, launch_intent, pick_initial_session, print_launch_help, remember_open_session,
};
use crate::live_fold::{fold_live, rail_activity_line};
use crate::pane_focus::resolve_split_focus;
use crate::rail_activity::busy_repaint_due;
use crate::task_strip::{StripTap, TaskTail, task_strip};
use crate::turn_swap::{
    SETTLE_BUDGET, SETTLE_RECHECK, SETTLE_RECHECKS, settle_landed, swap_visible_turn,
};
use crate::usage_load::{load_view, period_word};
use crate::window_input::{
    CARET_IDLE_AFTER, CtrlCAction, ctrl_c_action, translate_key, wheel_dy, zoom_step_for,
};

use opencrabs::db::models::Message;
use opencrabs::ui_core::actions;
use opencrabs::ui_core::channel_status;
use opencrabs::ui_core::composer;
use opencrabs::ui_core::message_select::{MessageAction, Selection};
use opencrabs::ui_core::model_layout;
use opencrabs::ui_core::model_selector::{FetchRequest, ProviderSelectorState};
use opencrabs::ui_core::pane_tree::{DropHalf, PaneTree};
use opencrabs::ui_core::rail;
use opencrabs::ui_core::scroll::{Follow, page_landing, scrolled_from_top, wheel_releases_follow};
use opencrabs::ui_core::split::{self, Layout};
use opencrabs::ui_core::text_field::{self, TextField};
use opencrabs::ui_core::topic_bar;
use transcript::CopyDrag;
// The terminal's own command table, shared rather than copied.
use opencrabs::ui_core::commands as slash;
// The Seeker engine: ranking, keywords and state, shared with the terminal
// so both clients print the same rows in the same order (#2458). Named `find`
// because `palette` here is the window's colour table.
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::palette::{
    self as find, Channel, PaletteAction, PaletteItem, PaletteKind, PaletteState,
};
// The docs pages Seeker can open, built once in the shared core (#2461).
use opencrabs::ui_core::docs;
use opencrabs::ui_core::help_keys;

use futures::channel::oneshot;

use gpui::{
    AnyElement, AnyView, App, Application, ClickEvent, ClipboardItem, Context, Div, DragMoveEvent,
    Entity, ExternalPaths, FocusHandle, Image, KeyDownEvent, KeystrokeEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, SharedString, Stateful, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowOptions, div, point, prelude::*, px, rems, rgb, rgba,
};
// The decoration request is Linux-only (#2813): every other platform keeps the
// server-side decorations its window manager already draws.
#[cfg(target_os = "linux")]
use gpui::WindowDecorations;
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::brain::tools::suggest_options::SuggestionItem;
use opencrabs::utils::drop_landing::human_size;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use uuid::Uuid;

use opencrabs::config::Config;
use opencrabs::config::GuiTheme;
use opencrabs::config::gui_font;
use opencrabs::config::gui_theme;
use opencrabs::tui::app::mission_control::log_viewer::LogViewerState;
use opencrabs::ui_core::keys::UiKey;
use opencrabs::ui_core::usage::data::Period;
use opencrabs::ui_core::usage::input::{self, DashboardState, UsageKeyOutcome};
use opencrabs::ui_core::usage::loader::usage_view;
use opencrabs::ui_core::usage::view_model::UsageViewModel;
use opencrabs::ui_core::window_controls::{
    TITLEBAR_APPEARS_TRANSPARENT, effective_decorations, platform_draws_caption,
};

fn main() {
    app::run();
}
