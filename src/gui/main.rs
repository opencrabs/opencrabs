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
mod dashboard_screens;
mod dashboard_sessions;
mod dashboard_settings;
#[cfg(test)]
mod dashboard_source;
mod dashboard_topics;
mod dashboard_turns;
mod editor;
mod fonts;
mod help_view;
mod hint;
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
mod rail_activity;
// The rail is an entity of its own (#2933): its rows, chips, badges and
// filter menu are built once per frame it is handed, and a frame that matches
// the last one replays the prepaint instead of rebuilding every listener.
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
mod window_input;

use crate::composer_view::{
    command_popup_view, composer_hint_for, emoji_popup_view, options_strip, queue_strip,
};
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
use opencrabs::ui_core::card::{CARD_RADIUS, COMPOSER_RADIUS, glass_fill};
use opencrabs::ui_core::palette::{
    self as find, Channel, PaletteAction, PaletteItem, PaletteKind, PaletteState,
};
// The docs pages Seeker can open, built once in the shared core (#2461).
use opencrabs::ui_core::docs;
use opencrabs::ui_core::help_keys;

use futures::channel::oneshot;

use gpui::{
    AnyElement, AnyView, App, Application, Bounds, ClickEvent, ClipboardItem, Context, Div,
    DragMoveEvent, Entity, ExternalPaths, FocusHandle, Image, KeyDownEvent, KeystrokeEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
    ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Stateful, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, point, prelude::*, px, rems, rgb,
    rgba, size,
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
use opencrabs::ui_core::window_controls::{PLATFORM_DRAWS_CAPTION, effective_decorations};

/// Which pane the right side draws. Chat is the default screen, the usage
/// ledger is opened with ctrl+u, and the conversation's tracked files with
/// ctrl+g, because the owner asked for exactly that split: an open
/// conversation must never be labelled or treated as usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    Chat,
    Usage,
    Files,
    /// The model picker: every pair the terminal offers, the live one marked.
    Models,
    /// The onboarding wizard, full screen: the same steps, the same writes.
    Onboard,
    /// Mission Control: the inbox, analytics, activity and schedule panels,
    /// the window's copy of the terminal's own screen (#2426).
    MissionControl,
    /// The skills list: every skill the terminal can invoke, from the same
    /// loader, runnable from the window (#2466).
    Skills,
    /// The terminal's project organizer, backed by the same service (#2669).
    Projects,
    /// The /cd directory picker (#2527): the terminal's own dialog, so a
    /// bare `/cd` opens a screen instead of answering with a text reply
    /// that also lights the thinking spinner.
    DirectoryPicker,
}

/// A divider drag in flight (#2472, #2476): which split, which divider in it,
/// the axis it moves along, and where the pointer was on the last move.
#[derive(Clone, Debug)]
struct DividerDrag {
    /// The split's path from the root, the way [`PaneTree::at`] addresses it.
    path: Vec<usize>,
    /// Which divider of that split: the one between children `divider` and
    /// `divider + 1`.
    divider: usize,
    /// Whether the split stacks its children, which fixes the drag axis.
    stacked: bool,
    /// Where the pointer was along that axis on the last move, in px.
    last: f32,
}

/// A pane picked up by its header (#2683). The drag carries the session that
/// owns the pane and nothing else: where the pane lands is decided by the drop
/// target under the pointer, not by the drag path it took.
#[derive(Clone)]
struct PaneDrag {
    session: Uuid,
}

/// What follows the pointer while a pane is dragged (#2683): the pane's own
/// title on a card, so the drag reads as the pane it will land as rather than
/// as a nameless chip.
struct PaneGhost {
    title: SharedString,
}

impl Render for PaneGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(CARD_RADIUS))
            .bg(rgb(palette::current().panel_hi))
            .border_1()
            .border_color(rgb(palette::current().accent))
            .text_xs()
            .text_color(rgb(palette::current().text))
            .child(self.title.clone())
    }
}

/// The rail's default width, and the floor it may be dragged to. At the
/// floor the splitter closes the rail instead of leaving a sliver: a 3-rem
/// column shows titles cut to nothing, which is worse than no column.
const RAIL_DEFAULT_REM: f32 = 17.0;
const RAIL_MIN_REM: f32 = 11.0;
const RAIL_MAX_REM: f32 = 32.0;

/// How many destinations the forward picker lists at once (#2608). The rail
/// can hold hundreds of sessions; the picker is a bar, not a list, so it shows
/// the head of the match and the query narrows from there.
const FORWARD_PICKER_ROWS: usize = 6;

/// A message actions menu never stays parked over history (#2862).
const MSG_MENU_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a row's copy icon shows a check before it goes back to the copy
/// mark (#2750). Short enough to be a receipt, long enough to be seen.
const COPIED_ICON_TTL: std::time::Duration = std::time::Duration::from_millis(1200);

/// Only the timer belonging to the menu still on screen may close it.
fn msg_menu_expiry_due(open: bool, current_generation: u64, timer_generation: u64) -> bool {
    open && current_generation == timer_generation
}

/// Whether a rail preview refresh can actually change a line.
///
/// The first version asked `rail.streaming_count()`, which counts only
/// sessions streaming with no pane of their own. A turn in the open
/// conversation never joins that set (its progress is handled inline) and a
/// parked turn returns from `fold_pane_progress` before the set is touched, so
/// the count read zero for the whole of a normal turn: the badge spun while
/// the preview line, the cost and the row order stayed frozen until `Done`.
/// Ask about the work instead of the set.
fn previews_can_move(busy: bool, stashed_live: bool, streaming: usize) -> bool {
    busy || stashed_live || streaming > 0
}

/// Holds a rail preview read's in-flight slot, and gives it back on drop.
///
/// A detached task whose view has gone away still unwinds through this, so the
/// slot can never be wedged by a cancelled read (#2817).
struct PreviewSlot(std::rc::Rc<std::cell::Cell<usize>>);

impl Drop for PreviewSlot {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// The place the footer names: the open conversation's working directory and
/// the branch checked out in it (#3036).
///
/// The window used to print the directory the process was launched from, which
/// never moves, so a `/cd` changed where the agent works and left the footer
/// naming the old project. The branch was read once at startup for the same
/// reason. Both are resolved from the session now, and the branch is kept here
/// because reading it runs `git`: it is re-read only when the directory
/// changes, never on every frame.
struct FooterPlace {
    /// The conversation it was read for. `None` means no conversation is open,
    /// so the row falls back to the launch directory.
    session: Option<Uuid>,
    dir: std::path::PathBuf,
    branch: Option<String>,
}

/// The window root. Every string on screen is real data or the real error.
struct Dashboard {
    /// Which pane the right side draws: chat by default, usage on ctrl+u.
    screen: Screen,
    /// The screen the previous frame drew (#2698). Coming back to the chat is
    /// a return, not a resume: every panel that closes hands the reader the
    /// newest row instead of the history they left behind.
    last_screen: Screen,
    state: LoadState,
    /// Which card is focused, whether the breakdown is open: the shared
    /// state the TUI's own handler mutates, driven by the same `decide`.
    dash: DashboardState,
    focus: FocusHandle,
    /// Where the next frame should put focus. Every screen transition sets it,
    /// so the caret is never left on an element that stopped drawing and a
    /// screen key always has a named receiver.
    refocus: Option<FocusTarget>,
    /// The owner focus was last set for. A frame where it changed applies the
    /// new target, so an opener that forgets to ask still cannot leave the
    /// composer answering under the field that just took the keyboard.
    last_owner: Option<KeyboardOwner>,
    /// The Seeker card. The query, the ranking and the armed row live in
    /// `ui_core::palette`; this is whether the card is up, and what it was
    /// given to find when it opened (#2458).
    finder: Option<PaletteState>,
    /// Which channels the config says are live, read once when the card opens,
    /// so the hint can tell an unconfigured channel from a working one without
    /// touching the disk on every keystroke (#2462).
    finder_channels: Vec<Channel>,
    /// The channel chip (#2452): `Some(line)` draws a quiet notice at the
    /// bottom of the rail that the configured channels have no live owner,
    /// with a Start daemon action. `None` means either everything is being
    /// served or nothing is configured; the probe refreshes every few
    /// seconds, off the UI thread.
    channel_chip: Option<SharedString>,
    /// The configured channel names, read from the config ONCE when the
    /// window opens, on the UI thread. The 5s probe never calls Config:
    /// on a GPUI background worker the config build overflows the small
    /// stack and killed the window with SIGILL (2026-11-03 crash reports).
    chip_channel_names: Vec<String>,
    /// The Start daemon click is in flight: a second click while launchctl
    /// or systemctl is running would stack two installs.
    daemon_starting: bool,
    /// What the last Start daemon attempt failed with, so the chip can say
    /// why nothing changed after the click.
    daemon_error: Option<SharedString>,
    /// Whether the card's caret is on this half of the blink. The card is a
    /// drawn div, not an input widget, so nothing blinks for it unless a
    /// timer says so (#2491).
    finder_caret: bool,
    /// When the Seeker field last took a key. The caret blinks for
    /// [`CARET_IDLE_AFTER`] after that instant and then holds off, the way
    /// Spotlight drops its caret when typing stops (#2531).
    finder_activity: std::time::Instant,
    /// Latest shaped text and bounds for Seeker mouse hit testing (#2534).
    finder_paint: Entity<command_palette::FinderPaint>,
    /// A left-button drag is extending the field's selection.
    finder_selecting: bool,
    /// The Seeker's own take (#3044). The card records in place: its glass
    /// bubble replaces the card while the device is open, and the words land
    /// in the field where the person was already typing.
    seeker_capture: Option<voice_input::Capture>,
    seeker_transcription: Option<voice_input::Transcription>,
    /// The high-resolution procedural glass drawn while this take is live.
    seeker_orb: Option<seeker_orb::OrbAnimation>,
    /// The live microphone meter that drives the glass motion.
    seeker_voice_levels: Vec<f32>,
    /// The recording was asked to stop and its WAV is being built.
    seeker_voice_finishing: bool,
    /// Whether the words this take produces should be run as a search the
    /// moment they land, or only put in the field for editing (#3044). The
    /// chord sends; clicking the glass hands the words back.
    seeker_voice_send: bool,
    /// Whether the Seeker's voice loop is already ticking. Same one-shot shape
    /// as the composer's: one loop per take, never one per frame.
    seeker_voice_looping: bool,
    /// Why the last take failed, said in the card. The card is what the person
    /// is looking at, so an error drawn behind it would be an error nobody
    /// reads.
    seeker_voice_error: Option<SharedString>,
    /// Monotonic tag on the latest period fetch; only its result is applied,
    /// so a slow fetch landing after a newer one cannot paint stale data.
    pending: u64,
    /// The session-files browser (ctrl+g): rows for the open conversation and
    /// the session they were fetched for, so a slow fetch landing after a
    /// rail click paints nothing.
    files: Vec<agent_bridge::SessionFile>,
    files_for: Option<Uuid>,
    files_err: Option<SharedString>,
    /// Mission Control's snapshot: the four services read once, so a frame
    /// never touches SQLite or the disk (#2426).
    mc: Option<mission_control::Snapshot>,
    /// Why the last read failed, when it did. The screen says so instead of
    /// drawing an empty instance as if nothing were waiting on the user.
    mc_err: Option<SharedString>,
    /// A read is in flight. The screen shows the last snapshot and this flag
    /// rather than clearing to a blank pane.
    mc_loading: bool,
    /// The analytics window the snapshot was read for. It starts at the
    /// terminal's default (a month, `tui::app::mission_control`) so both
    /// clients open on the same numbers.
    mc_window: mission_control::TimeWindow,
    /// Which panel the keyboard is on, which row, and whether the detail is
    /// open. The rules live in `mission_control::Nav` so they are tested
    /// without a window.
    mc_nav: mission_control::Nav,
    /// The log viewer while `L` has it open. It is the terminal's own state
    /// type, so the window reads a log with the same parser and the same
    /// filters (#1528).
    mc_logs: Option<LogViewerState>,
    /// The result of the last apply or reject, said on the screen so the
    /// decision is not silent. Cleared when the screen is reopened.
    mc_notice: Option<mission_control::Notice>,
    /// The window's rem size in px. The zoom keys move it and every
    /// rem-based length in the theme follows.
    rem: f32,
    /// Ledger path resolved once at startup, before the window opens.
    /// Period fetches reuse it; they never re-read the config, because
    /// tiny-stack background threads must not run `Config::load`.
    db_path: std::path::PathBuf,
    /// The session rail's live state.
    sessions: sessions::SessionsState,
    /// Which rail rows have work in flight, and which plans wait (#2636).
    /// Memory only: the open turn and each split leaf already keep their own
    /// `busy`, so this holds the one thing nothing else records, a session
    /// streaming with no pane of its own.
    rail: rail_activity::RailActivity,
    /// The id to plan-file index behind each row's badge, with its mtime cache.
    rail_index: rail_activity::PlanIndex,
    /// The plans the owner has already opened, so their rings stop (#2636).
    rail_seen: rail_activity::SeenPlans,
    /// When the rail's live previews were last re-read, so a stream of chunks
    /// costs at most one query a second instead of one per token.
    preview_refreshed_at: Option<std::time::Instant>,
    /// How many preview reads are in flight, and whether another was asked for
    /// while one ran. A busy ledger made a read outlast the one-second tick,
    /// so the next tick stacked a second thread and a second connection on the
    /// same 1.2 GB file (#2817). One read at a time, and the last ask wins.
    previews_in_flight: std::rc::Rc<std::cell::Cell<usize>>,
    previews_dirty: std::rc::Rc<std::cell::Cell<bool>>,
    /// Monotonic generation of the ctrl+f deep search (#2720). Each keystroke
    /// bumps it and spawns a debounced content query that carries the value;
    /// a result only lands when its generation still matches, so a slow query
    /// for "nee" can never paint over the hits for "needle".
    rail_find_gen: u64,
    /// The last ledger signature the rail was refetched from (#2881). A turn
    /// that arrives on a messaging channel produces no window event, so nothing
    /// on the UI side asks the rail to reload and it kept drawing the list it
    /// loaded at startup. The probe compares this pair and pays for a reload
    /// only when the ledger actually moved.
    rail_ledger_sig: Option<(i64, i64)>,
    /// Seeker deep search (#2721): content hits per trimmed query, cached so
    /// re-opening the same query costs no second read. The card's rows are
    /// rebuilt from this map when a debounced result lands.
    seeker_find: HashMap<String, Vec<opencrabs::db::repository::ContentSearchRow>>,
    /// Monotonic generation of the Seeker deep search, the rail's trick again:
    /// a result only rebuilds the card when its generation still matches.
    seeker_find_gen: u64,
    /// The trimmed query the deep search was last scheduled for, so caret
    /// moves and arrow navigation never rebuild the catalog (#2721).
    seeker_scheduled_query: String,
    /// Parents whose topic chips are expanded past the first three, with the
    /// instant each one opened. A chip strip shows three plus a "+N" pill by
    /// default; tapping it reveals the rest, and the collapse pill folds them
    /// back (Telegram topic style).
    ///
    /// The instant is the disclosure's own clock (#3020): an expanded strip is
    /// a picker, not a setting, so it folds itself after a bounded idle period
    /// instead of staying open until the window restarts.
    rail_chips_expanded: std::collections::HashMap<Uuid, std::time::Instant>,
    /// The conversations whose turn is running a voice message (#3044). A turn
    /// started from a recording is answered with speech once it settles, the
    /// way a channel answers a voice note, and this is how the settle knows
    /// which turns those are: the transcript is ordinary text by the time it
    /// reaches the bridge, and nothing downstream can tell it apart from typing.
    voice_turns: std::collections::HashSet<Uuid>,
    /// The last voice state the window painted (#3054): active playback and
    /// waveform loads. The poll compares one snapshot, so position changes
    /// and completed background decodes both reach the visible previews.
    voice_paint: voice_play::PaintState,
    /// Rail width in rem, moved by the splitter's drag. Never below
    /// `RAIL_MIN_REM`: dragging past it closes the rail instead, and ctrl+s
    /// brings it back at the last width.
    rail_width: f32,
    /// Whether the rail is drawn at all.
    rail_open: bool,
    /// Whether the sidebar holds the keyboard. Tab moves it; while it does,
    /// the arrows walk its rows, and the composer keeps them otherwise (#2417).
    rail_focused: bool,
    /// Poll ticks since the window opened (#2899). The poll runs at 20 Hz and
    /// most of what it checks only changes what a repaint would draw, so the
    /// answers that cost a file read or a bridge lock are resolved on the
    /// repaint cadence and reused in between.
    poll_tick: u64,
    /// The last answer `rail_animating` gave, reused between the polls that
    /// resolve it.
    rail_anim_live: bool,
    /// When the settled-task pass last ran (#2899). It clones every plan and
    /// resolves their settled ids, so it runs on its own clock rather than
    /// once per frame.
    chime_last: Option<std::time::Instant>,
    /// This frame's status row per leaf (#2899). Building it reads the bridge
    /// for the session's provider pair and its plan progress, and every pane
    /// used to resolve its own twice: once in the stamp the walk builds and
    /// again in its own draw. Resolved once per frame, for every leaf, before
    /// either of them reads it.
    frame_footers: HashMap<Uuid, SharedString>,
    /// The detached-command line per leaf for this frame (#2899). Same reason:
    /// the label takes the background manager's lock and formats a string.
    frame_tasks: HashMap<Uuid, SharedString>,
    /// When `frame_tasks` was last rebuilt. The label ticks in whole seconds,
    /// so a second rebuild inside the same second would take the lock and
    /// allocate the same strings for text that cannot have changed (#2805).
    frame_tasks_at: Option<std::time::Instant>,
    /// A newer release, when the update check found one (#2406).
    update_available: Option<String>,
    /// The update dialog: None when closed, Some(false) asks whether to update,
    /// Some(true) asks again before /evolve runs.
    update_prompt: Option<bool>,
    /// The Help Center filter typed after `/`; None while it is not open (#2407).
    help_filter: Option<String>,
    /// The commands the Help Center prints: slash names, loaded skills and the
    /// user's own entries (#3044). Read once when the reference opens, because
    /// two of the three sources walk the disk; a per-frame read would be a
    /// directory walk and a parse on every repaint.
    help_commands: CommandLists,
    /// Drag-to-copy state shared by the chat's rows (#2383).
    copy_drag: Rc<CopyDrag>,
    /// The window's panes as a tree (#2472). One leaf is the unsplit window,
    /// so the plain chat is the base case rather than a special one, and a
    /// nested split is just another node. `None` until a conversation opens.
    panes: Option<PaneTree>,
    /// The state every pane but the chat keeps, keyed by session (#2472). Each
    /// leaf is its own entity, so it repaints and is read on its own (#2805).
    pane_states: HashMap<Uuid, Entity<PaneView>>,
    /// A pane switched to another conversation, kept whole under the session it
    /// was showing (#2854). A switch used to drop the entity, and with it the
    /// live turn, the tool blocks, the thinking flag and the cancel token, so a
    /// turn that was still running came back as an empty pane. The window has
    /// parked its own turn this way since #2616; this is the same idea for a
    /// leaf. Keyed by the session the view was showing, so switching back
    /// restores exactly what left.
    parked_panes: HashMap<Uuid, Entity<PaneView>>,
    /// The session holding the caret and answering `ctrl+x`; `None` is the
    /// window's own chat (#2474, #2475).
    pane_focus: Option<Uuid>,
    /// Where each split begins and ends along both axes, in px, keyed by the
    /// split's path from the root, written by the body itself every frame
    /// (#2472, #2476). Mapping pointer travel onto a share needs the span it
    /// travels across, and an element is never told its own children's bounds
    /// any other way.
    split_areas: Rc<std::cell::RefCell<HashMap<Vec<usize>, [f32; 4]>>>,
    /// A divider drag in flight: the split's path, which divider, where the
    /// pointer was along the drag axis in px, and the share when it began.
    /// The moves land on the window root the way the rail splitter's do, so
    /// the pointer can leave the handle (#2472, #2476).
    split_drag: Option<DividerDrag>,
    /// The pane a pending pick splits; the sidebar fills it (#2472).
    split_target: Option<Uuid>,
    /// Where a dragged pane would land (#2683): the pane under the pointer and
    /// which half of it. Live only while a pane is in flight, so a stale value
    /// can never outline a pane nobody is dragging.
    pane_drop: Option<(Uuid, DropHalf)>,
    /// The layout a split is waiting on a session for; the sidebar picks it (#2438).
    split_pick: Option<Layout>,
    /// A split's new session is being created, so the created row fills the pane.
    split_creating: bool,
    /// The row whose message menu is open (#2613). One at a time, like the
    /// rail's: opening another closes the first.
    msg_menu: Option<Uuid>,
    /// The row that menu was opened on, captured at the press (#2827).
    ///
    /// The menu's actions used to read the row back out of the window's own
    /// transcript, which meant a row living in a split leaf could not be
    /// copied, answered or forwarded: it was never in that transcript. The
    /// press already has the row, so it is carried instead of looked up.
    msg_menu_row: Option<Message>,
    /// Which leaf that row lives in (#2827). `None` is the window's own chat;
    /// `Some` is the split leaf of that session, whose composer an answer
    /// belongs in.
    msg_menu_leaf: Option<Uuid>,
    /// True when the captured row is an exact, unsaved live entry (#2925).
    /// React and Select stay off it because there is no persisted row id.
    msg_menu_live: bool,
    /// The row open for an in-place rewrite in the window's chat (#2826).
    /// A split leaf holds its own on its `PaneView`.
    edit_row: Option<chat_leaf::InlineEdit>,
    /// Where that row's right press landed, in window coordinates (#2690).
    /// The card opens under the pointer instead of pinned to the row's top
    /// corner, so a long message cannot put it half a screen away.
    msg_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// Which menu's expiry timer is current. Opening another menu invalidates
    /// the old timer, so it cannot close the new one early (#2862).
    msg_menu_generation: u64,
    /// The row whose copy icon is showing its check (#2750). One at a time:
    /// copying another row moves the mark rather than lighting two.
    copied_row: Option<Uuid>,
    /// Which copy's timer is current, the same generation rule the menu's
    /// expiry follows (#2750).
    copied_generation: u64,
    /// The row whose crab pack is open (#2825). One card at a time, so it
    /// closes the message menu that opened it.
    react_menu: Option<Uuid>,
    /// Where that pack opened, carried over from the menu that asked for it.
    react_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// Select mode (#2613). The picks live in the model, keyed by message id,
    /// so a scroll or a refresh never drops one; `is_active` is what the
    /// selection bar answers to.
    msg_select: Selection<Uuid>,
    /// A forward waiting for its destination (#2608): the rows picked when
    /// Forward was pressed. `Some` swaps the composer for the session picker.
    forward_pick: Option<Vec<Message>>,
    /// Which leaf opened that picker (#2827). `None` is the window's chat;
    /// `Some` puts the picker in the split leaf whose row was forwarded.
    forward_leaf: Option<Uuid>,
    /// The destination picker's query, typed into the bar itself.
    forward_query: String,
    /// The destination the picker points at, so the arrows walk it and Enter
    /// takes it. `None` falls back to the first match.
    forward_cursor: Option<Uuid>,
    /// The icon picked for the topic being named; None means the default (#2414).
    topic_draft_icon: Option<String>,
    /// Custom icon images read once from the store, keyed by stored value
    /// (#2790). The paint clones an Arc, never reads a file; the map
    /// rebuilds when the topic icons load and after an image is picked.
    icon_images: HashMap<String, Arc<Image>>,
    /// The Help Center body's scroll position, so the arrows can move it (#2407).
    help_scroll: ScrollHandle,
    /// A splitter drag is in flight. The move events land on the window root,
    /// not the 4px handle, so the pointer can leave the handle without ending
    /// the drag.
    dragging_rail: bool,
    /// The pointer x and rail width when the drag began, in pixels.
    drag_from: Option<(f32, f32)>,
    /// When the last external file-drag move was seen. gpui turns a Finder
    /// drag into a synthetic drag of `ExternalPaths` but keeps `active_drag`
    /// private, so freshness is how a drag that leaves the window stops
    /// counting: the poll tick retires the flag a few frames after the moves
    /// stop, and the drop clears it outright.
    drag_seen: Option<std::time::Instant>,
    /// Where the right press that opened the rail's context menu landed, in
    /// window coordinates. The card opens just past the rail at that
    /// height instead of over the row it belongs to (#2742).
    rail_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// The turn running in a session that has no leaf of its own (#2718), so
    /// its rail card can say what it is doing instead of showing the last
    /// message the ledger holds. Kept apart from the panes and the stash on
    /// purpose: this is the only record of a turn nobody is looking at, and
    /// the entry is dropped the moment that turn settles.
    rail_live: HashMap<Uuid, transcript::LiveTurn>,
    /// A leftward slide in flight on a rail row: the session and the pointer
    /// x where the drag began. Past the threshold the row's menu opens.
    rail_swipe: Option<(Uuid, f32)>,
    /// The row whose menu a slide just opened. gpui fires a click on mouse-up
    /// whenever the pointer is still over the element, movement or not, so
    /// without this the release that ends a slide also opens the chat the
    /// slide was meant to act on.
    rail_slide_opened: Option<Uuid>,
    /// The conversation a delete is waiting on. Every delete path arms this
    /// and only the dialog's own button erases, so no gesture can cost a
    /// conversation.
    confirm_delete: Option<Uuid>,
    /// The conversation whose topics are about to be folded in and switched
    /// off (#2863). Arming this is the first click; the dialog's own button is
    /// the second, because the fold cannot be undone.
    confirm_disable_topics: Option<Uuid>,
    /// The Group topics picker: the parent row being grouped under, the
    /// candidate sessions (id, title, date), the multi-selection, the keyboard
    /// cursor, a guard sentence on refusal, and a busy flag while attaching.
    group_target: Option<Uuid>,
    group_candidates: Vec<(Uuid, String, String)>,
    group_selected: std::collections::HashSet<Uuid>,
    group_cursor: usize,
    group_error: Option<String>,
    group_busy: bool,
    /// Whether the picker has already shown its fold warning (#2864). The
    /// owner asked for a refusal that can be forced: the first Enter names
    /// what will happen to the selected families, the second one goes ahead.
    group_warned: bool,
    /// What has been typed into the picker's field (#2864). Narrowing happens
    /// against this, never against the list in place, so clearing the field
    /// brings every candidate back.
    group_filter: String,
    /// Whether the key reference is open (#2320). The overlay reads the same
    /// table the terminal's `/help` prints, so the two cannot disagree.
    help_open: bool,
    /// The file a click on a transcript tile opened (#2395). One at a time:
    /// the overlay is modal, so a second preview replaces the first instead of
    /// stacking.
    preview: Option<preview::Preview>,
    /// How far the previewed image is zoomed, 1.0 at open. Reset on every open
    /// so a fresh click never inherits the last picture's magnification.
    preview_zoom: f32,
    /// What a clip's overlay can show: its poster frame, length and size,
    /// probed once on the click that opened it. Never gathered while painting,
    /// because both facts come from a subprocess and a render that shells out
    /// is a render that stutters.
    preview_video: Option<preview::VideoFacts>,
    /// Markdown and plain text load off the UI thread and carry their own
    /// scroll position. A generation rejects a response for an older file.
    preview_document: Option<preview_document::DocumentState>,
    preview_document_generation: u64,
    /// The chat's `...` menu, as the session it acts on (#2480).
    ///
    /// It carries the id, never a bare flag: a right press on a topic tab
    /// opens the same menu for a session that is not the one on screen.
    topic_menu: Option<uuid::Uuid>,
    /// Where that right press landed, in window coordinates (#2690). The card
    /// opens under the pointer instead of at the column's top corner, which in
    /// a split was not even the pane whose tab was pressed.
    topic_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// The topic name being typed, while the name prompt is open (#2303).
    /// A real text field, so the prompt carries a caret and a selection and
    /// answers the composer's editing chords (#2892). A bare `String` put the
    /// caret at the end and gave `cmd-a`, `alt+left` and `cmd+x` nothing to
    /// act on, so a rename could only be typed or backspaced.
    topic_draft: Option<TextField>,
    /// The pick the window has to explain before it goes on (#2605): the
    /// seeker's topic pick named a topic, and a topic cannot hold topics, so
    /// this carries that row's title for the warning instead of opening a
    /// prompt that would nest one under another.
    topic_warning: Option<String>,
    /// A pick that named a conversation already open in a pane (#2819). The
    /// plain open path used to drag it into the chat leaf and close the pane
    /// it came from, so a pick could destroy a split the user built. This
    /// carries that row's title for the card that says so instead.
    pane_warning: Option<String>,
    /// Which side of the chat the topic row sits on (#2402).
    topic_edge: topic_tabs::Edge,
    /// Whether the chat's topic row shows its whole family (#2949). The row
    /// folds past `BAR_TOPICS_SHOWN` and prints a pill; the pill was a label,
    /// so the folded topics could only be reached from the rail. A press
    /// expands the row, which then wraps and scrolls, and the same pill folds
    /// it back.
    chat_topics_expanded: bool,
    /// The topic being renamed, while the prompt is open for a rename (#2304).
    /// None means the prompt names a new topic.
    topic_rename: Option<uuid::Uuid>,
    /// The read-only info panel for a topic, while it is open (#2304).
    topic_info: Option<sessions::TopicInfo>,
    /// The theme modal's accent, as hue/saturation/value so a drag keeps its
    /// precision instead of round-tripping through 8-bit channels (#2619).
    themes_hue: f32,
    themes_sat: f32,
    themes_val: f32,
    /// Whether the theme modal is open. It is an overlay, not a screen: the
    /// window behind it keeps showing whatever it showed, so the choice is
    /// made against real content (#2619).
    themes_open: bool,
    /// The card the keyboard is standing on: 0 orgn, 1 light, 2 custom, 3 auto.
    themes_card: usize,
    /// The whole-window transparency in force (#2922). Zero keeps the native
    /// window opaque; a positive value composites the root with the desktop.
    window_transparency: u8,
    /// The native compositing mode last applied by the renderer. Tracking it
    /// avoids sending the same platform mutation on every frame.
    window_background_transparent: bool,
    /// Which accent control the pointer is dragging, if any.
    themes_drag: Option<accent::Drag>,
    /// Whether the pointer is dragging the whole-window transparency track.
    themes_transparency_drag: bool,
    /// The wheel's box in window coordinates, recorded at paint time so a
    /// press can be turned into a hue (#2619).
    themes_wheel: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// The saturation and brightness tracks' boxes, recorded the same way.
    themes_sat_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    themes_val_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// The whole-window transparency track, recorded the same way (#2922).
    themes_transparency_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// Whether the font list under the cards is open (#2786). Closed by
    /// default, because the list is the machine's whole family catalog and the
    /// modal is small.
    fonts_open: bool,
    /// Whether the installed families have been read. The enumeration walks
    /// CoreText on macOS and keeps no cache, so it runs once per process.
    fonts_loaded: bool,
    /// The settings modal (#2878): the values are read fresh from the loaded
    /// config on open, one row per `settings_view::SPECS` entry, followed by
    /// the credential rows. `settings_secret` is the one armed credential
    /// edit: its row index and the typed buffer, which is only ever painted
    /// as bullets.
    settings_open: bool,
    settings_values: Vec<settings_view::RowValue>,
    settings_creds: Vec<settings_view::CredSpec>,
    settings_cursor: usize,
    /// Which rail category the settings modal paints (#2890).
    settings_section: settings_view::SettingsSection,
    settings_scroll: ScrollHandle,
    settings_status: Option<String>,
    settings_secret: Option<(usize, String)>,
    /// The /cd picker's browse state (#2527): the directory on screen, its
    /// subdirectories read at open and on every descend, the row the
    /// keyboard stands on, and whether dotfile directories are listed.
    cd_dir: std::path::PathBuf,
    cd_entries: Vec<std::path::PathBuf>,
    cd_selected: usize,
    cd_show_hidden: bool,
    /// The session the open picker is about. Pinned when the screen opens,
    /// because a split window's caret leaf is not the chat leaf: reading
    /// `chat_session` at either end sent the change to the conversation the
    /// window happens to hold while the reader was looking at a pane, and a
    /// switch made while the picker was open moved the target out from under
    /// them.
    cd_session: Option<Uuid>,
    /// The model picker's rows, built when the screen opens so a slow frame
    /// never rebuilds the catalogue.
    models_rows: Vec<models::Row>,
    /// The live search text. Selection indexes the filtered rows, never the
    /// unfiltered catalogue underneath them.
    models_query: String,
    /// The row the arrows stand on.
    models_index: usize,
    /// The provider the models screen's list marks. A click sets it and moves
    /// the model cursor to that provider's first visible model.
    models_provider: Option<String>,
    /// The session that opened the picker. A split pane owns its own model,
    /// so the picker must apply and answer in this session, not the window.
    models_session: Option<Uuid>,
    /// The session whose switch is in flight. A second Enter while this is
    /// set is ignored, so one pick cannot write the same confirmation twice.
    models_switching: Option<Uuid>,
    /// Shared request lifecycle and pending providers. Static rows are only a
    /// fallback; every configured provider gets a live discovery request.
    models_fetch: Option<std::sync::mpsc::Receiver<(u64, model_fetch::FetchOutcome)>>,
    models_fetch_lifecycle: opencrabs::ui_core::model_selector::FetchLifecycle,
    models_pending: Vec<FetchRequest>,
    models_fetching: bool,
    models_fetch_error: Option<String>,
    /// The skills screen's rows, read from the terminal's own loader when the
    /// screen opens so the list can never drift from `/skills` in the TUI,
    /// and the row the arrows stand on.
    skills_all: Vec<opencrabs::brain::skills::Skill>,
    skills_rows: Vec<opencrabs::brain::skills::Skill>,
    skills_query: String,
    skills_index: usize,
    /// Project rows and navigation, loaded by the shared service on demand.
    projects: projects::ProjectsState,
    /// The onboarding wizard while its screen is open. Built on first open:
    /// constructing it reads the live config, which is not something to do on
    /// every window start when most sessions never onboard.
    onboard: Option<onboard::Onboard>,
    /// The plan checklist strip above the editor, re-read when its file moves.
    plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per selection rather than once per frame.
    plan_for: Option<Uuid>,
    /// Panes whose plan strip the user expanded (#2624). A pane opens at the
    /// density its pane count and width allow, and a click on the card trades
    /// the conversation's room for the checklist, per pane, so expanding one
    /// leaf never grows the card in the others.
    plan_expanded: std::collections::HashSet<Uuid>,
    /// Sessions whose task list is open. At rest it is always the one line:
    /// a session is here only after a click or a Down from the composer, and
    /// leaves when it has nothing running, so the next task starts collapsed.
    tasks_open: std::collections::HashSet<Uuid>,
    /// The session whose task list holds the keyboard, after Down from its
    /// composer. The row the keys stand on is that session's `task_open`.
    task_focus: Option<Uuid>,
    /// The task whose output is showing, per session.
    task_open: HashMap<Uuid, Uuid>,
    /// The last output read for an open task, with when it was read and the
    /// file it came from once that file was found. Re-read on a one-second
    /// tick, the same cadence the strip's elapsed time moves at.
    task_tails: HashMap<Uuid, TaskTail>,
    /// The task opened into its full view, with Enter on its row or the
    /// row's open door. It outlives the task: the view stays on the last
    /// output once the task ends, until the reader closes it.
    task_detail: Option<task_detail::TaskDetail>,
    /// The scroll of that view's output.
    task_detail_scroll: ScrollHandle,
    /// Bumped on every open, so the repaint loop of a view that was closed
    /// and reopened ends instead of doubling up.
    task_detail_generation: u64,
    /// Plan tasks whose completion already rang (#2691). One set for the
    /// whole window, shared by the window strip and every pane strip, so a
    /// plan drawn in two places rings once. The terminal keeps the same
    /// single set (`tui::app::state::announced_task_completions`).
    announced_task_completions: std::collections::HashSet<Uuid>,
    /// Until when each session's rail plan bar stays flashing orange, per
    /// session, keyed by the plan's own session id (#2701 follow-up).
    ///
    /// The deadline is written on the exact event that rings the chime, so the
    /// sound and the blink are two surfaces of one fact: a task just settled.
    /// It is memory only, like the rail's streaming set: a window restart has
    /// nothing to flash about.
    plan_flash_until: HashMap<Uuid, std::time::Instant>,
    /// The conversation on screen: its transcript, its viewport and whether
    /// that viewport follows the newest row (#2805). One value, so a frame
    /// that touches the chat stops being a frame that rebuilds the window.
    chat_state: chat_leaf::ChatState,
    /// The session `chat_state` holds, mirrored on the window because the
    /// rail, the split fill, the plan strip and the model picker decide with
    /// it on every frame. Written in exactly one place, `set_chat_session`, so
    /// it cannot drift from the owner it mirrors.
    chat_session: Option<Uuid>,
    /// The one middle-click auto-scroll gesture in this window (#2964). Its
    /// target owns the scroll state; this value owns only pointer and timing.
    middle_scroll: Option<middle_scroll::MiddleScroll>,
    /// Whether GPUI already has the next auto-scroll frame queued. This stays
    /// set through cancellation until that callback arrives, preventing a new
    /// gesture from starting a second animation loop beside the old one.
    middle_scroll_frame_pending: bool,
    /// When the first Ctrl+C armed the quit window (#2308). `None` means no
    /// press is waiting, and the next one only clears and arms again.
    ctrl_c_pending: Option<std::time::Instant>,
    /// The keystroke interceptor that hands Ctrl+C to this window before the
    /// composer's own copy chord can take it (#2308). Held here because
    /// dropping the subscription would silently unregister it.
    ctrl_c_interceptor: Option<gpui::Subscription>,
    /// The rail's own scroll handle, so a cursor moved by the arrows can
    /// ask the next frame to bring its row into view (#2351).
    rail_scroll: ScrollHandle,
    /// The rail itself, drawn as a cached view so a repaint it has no stake in
    /// replays its last prepaint instead of rebuilding every row (#2933).
    rail_view: Entity<rail_view::RailView>,
    /// The composer and the queue: the field the user types in, the drafts kept
    /// per conversation, the messages waiting behind the turn, and the flags
    /// the hint and the spinner read. One value, so the chat leaf inherits one
    /// field instead of seven (#2805).
    composer: chat_leaf::Composer,
    /// The submitted history every composer in this window walks. One handle
    /// cloned into the window's field and every split pane, so a message sent
    /// in one is recalled in all of them, in real time, and the terminal reads
    /// the same list off the same file.
    history: composer::SharedHistory,
    /// Agent-side state (task 5). Events from the bridge's bootstrap and
    /// turn threads land in the receiver; the poll tick drains it.
    agent_rx: std::sync::mpsc::Receiver<agent_bridge::TurnEvent>,
    /// Reloads the watcher thread hands the window: a config.toml, keys.toml or
    /// commands.toml save, already loaded and published to `Config::current()`
    /// by the terminal's own reload pass. Drained on the poll tick, which
    /// re-derives what the window cached at boot (`dashboard_config`).
    config_reload: std::sync::mpsc::Receiver<config_reload::Reload>,
    sink: agent_bridge::EventSink,
    /// Set once bootstrap reports the service live. Turns run on the pane's
    /// open conversation (`chat_state.transcript.session`), never on a session
    /// of the bridge's own, so the answer always lands where the question was
    /// typed.
    agent_ready: bool,
    /// The turn in flight: the card, whether it is still running, the handle
    /// that stops it, the prompt size the provider reported and the reload
    /// handshake waiting for its rows. One value, so a switch moves the turn
    /// whole and the chat leaf inherits one field instead of five (#2805).
    turn: chat_leaf::Turn,
    /// Turns running in conversations the window is not painting (#2616).
    /// Keyed by the session that owns each one, so a switch parks the turn on
    /// screen and restores the destination's, and the window never has to
    /// guess which conversation a card belongs to.
    stashed: std::collections::HashMap<Uuid, chat_leaf::Turn>,
    /// The open conversation's context window: the compaction budget the
    /// service actually enforces for it, not a hardcoded 200K.
    ctx_max: u32,
    /// What each settled turn cost and how fast it answered (#2324), plus the
    /// numbers waiting for their rows to land. One owner, like the handshake
    /// above: both are read from the same reload.
    metrics: chat_leaf::Metrics,
    /// The footer's place, resolved from the open conversation rather than
    /// from the process (#3036): the session it was read for, the directory
    /// that session works in, and the branch checked out there. The window
    /// used to print the launch directory and a branch read once at startup,
    /// so a `/cd` moved the session and left the footer naming the old place.
    footer_dir: Option<FooterPlace>,
}

/// Apply sidebar visibility without letting a closed rail retain keyboard
/// focus or a hidden filter. Width belongs to the window layout and is not
/// part of this transition.
///
/// Opening is a request to walk the list, so the rail takes the keyboard on
/// the way in and the cursor stands on the open conversation (#2665). It used
/// to arrive unfocused, which left up and down dead until a Tab.
fn set_rail_open_state(
    open: bool,
    rail_open: &mut bool,
    rail_focused: &mut bool,
    sessions: &mut sessions::SessionsState,
) {
    *rail_open = open;
    if open {
        *rail_focused = true;
        sessions.cursor = sessions.cursor.or(sessions.selected);
    } else {
        *rail_focused = false;
        sessions.filter.clear();
        sessions.filter_active = false;
        // The filter menu clears with the rail (#2719): a reopened rail
        // starts from the default rule, never a stale chip from last time.
        sessions.rail_filter = sessions::RailFilter::default();
        // The open chip panel folds with the rail too (#2787).
        sessions.rail_filter_open = false;
        // The deep hits clear with the rail too (#2720): a reopened rail
        // starts unsearched, never showing snippets from the last query.
        sessions.content_matches.clear();
    }
}

/// Poll ticks between repaints a live turn asks for. The poll runs at 20 Hz;
/// the only things moving in that branch are a spinner and a clock, and every
/// one of those repaints walks every pane's transcript (#2688). Five frames a
/// second keeps the animation alive at a quarter of the rebuilds.
const BUSY_REPAINT_TICKS: u64 = 4;

/// True on the poll tick that paints a live turn's animation. Pure, so the
/// cadence stays a rule rather than an accident of where the counter is bumped.
pub(crate) fn busy_repaint_due(tick: u64) -> bool {
    tick.is_multiple_of(BUSY_REPAINT_TICKS)
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Advance from whatever repaint came in. The animation receipt is
        // cleared only by its own callback, not by an unrelated repaint.
        self.tick_middle_scroll(cx);
        // Native alpha and the root's opacity are two halves of the same
        // setting (#2922). Change the platform mode only when it flips; the
        // opacity itself is cheap to paint on every frame.
        let transparent = self.window_transparency > 0;
        if transparent != self.window_background_transparent {
            window.set_background_appearance(if transparent {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Opaque
            });
            self.window_background_transparent = transparent;
        }
        // The machine's font catalog is read once, on the first paint (#2786):
        // `all_font_names` walks CoreText on macOS and keeps no cache of its
        // own, so asking it per frame would re-enumerate every installed
        // family on every paint.
        if !self.fonts_loaded {
            self.fonts_loaded = true;
            fonts::remember_installed(cx.text_system().all_font_names());
        }
        // Focus follows the keyboard owner, decided here because this is the
        // only place holding a window to focus through. A transition that
        // forgot to ask would leave the composer answering under an open
        // field, which is the bug class #2455 came from, so the owner is
        // reconciled every frame instead of trusted from each call site.
        //
        // A task list whose last task ended lets go first, so the keyboard is
        // back on the composer in the same frame the card disappears.
        self.prune_task_state();
        let owner = keyboard_owner(&self.open_overlays(), self.screen);
        if self.last_owner != Some(owner) {
            // A receipt in the daily log, the same shape as `pane focus ->`
            // (#2549): a switch that never moves the owner is exactly the
            // case the reconciler below cannot repair (#2741).
            logging::debug(&format!("keyboard owner -> {owner:?}"));
            self.last_owner = Some(owner);
            self.refocus = Some(focus_target(owner));
        }
        // Applied before the tree is built, so the frame that shows the new
        // screen is the frame that can be typed into.
        if let Some(target) = self.refocus.take() {
            match target {
                FocusTarget::Composer => {
                    // The keyboard follows the pane, not the window (#2610):
                    // a rotated caret that left focus on the chat leaf typed
                    // into a field the split was not drawing.
                    let composer = self.composer_entity(cx);
                    let handle = composer.read(cx).focus_handle.clone();
                    // `before` is true when gpui turned the request into a
                    // no-op, because it returns early on the handle that is
                    // already focused. `now` names who holds the keys after
                    // the call, so "the caret never came back" is answerable
                    // from the log instead of guessed from a screenshot
                    // (#2741).
                    let before = handle.is_focused(window);
                    window.focus(&handle);
                    logging::debug(&format!(
                        "caret -> composer before={before} now={}",
                        self.caret_owner_name(window, cx)
                    ));
                }
                FocusTarget::Root => {
                    let before = self.focus.is_focused(window);
                    window.focus(&self.focus);
                    logging::debug(&format!(
                        "caret -> root before={before} now={}",
                        self.caret_owner_name(window, cx)
                    ));
                }
            }
        }
        // A press on plain chrome takes the caret without moving the owner:
        // the root tracks focus, so every element that does not sits under it
        // and the press lands there, while the derived owner is still the
        // composer. The transition above cannot see that, so the drift is read
        // off the window instead and repaired before the user types (#2741).
        if owner == KeyboardOwner::Composer {
            let composer = self.composer_entity(cx);
            let drifted = !composer.read(cx).focus_handle.is_focused(window)
                && window.focused(cx).is_none_or(|handle| handle == self.focus);
            if drifted {
                let handle = composer.read(cx).focus_handle.clone();
                window.focus(&handle);
                logging::debug(&format!(
                    "caret -> composer repaired now={}",
                    self.caret_owner_name(window, cx)
                ));
            }
        } else {
            // The mirror of the repair above, and the half that was missing
            // (#2767): an overlay or a screen took the keys while the composer
            // kept the caret, so everything typed into the prompt also landed
            // in the message field and Enter submitted it. A field the overlay
            // owns is not this drift; only the composer is.
            let composer = self.composer_entity(cx);
            let leaked = composer.read(cx).focus_handle.is_focused(window);
            if leaked {
                window.focus(&self.focus);
                logging::debug(&format!(
                    "caret -> root repaired now={}",
                    self.caret_owner_name(window, cx)
                ));
            }
        }
        // Coming back to the conversation is a return, not a resume: a panel
        // that closes lands on the newest row instead of the history the
        // reader left behind, the same rule the terminal applies when a
        // dialog hands the chat back (#2698).
        if self.screen == Screen::Chat && self.last_screen != Screen::Chat {
            self.rearm_transcript_follow();
        }
        self.last_screen = self.screen;
        // Keep every transcript on its newest row (#2685). Run before the
        // tree is built so the snap lands in this frame's layout, and judged
        // from each viewport's own offset so a reader parked in history is
        // never yanked back down.
        if !self.middle_scroll_controls_chat() {
            self.follow_chat();
        }
        for session in self.panes.as_ref().map(|t| t.leaves()).unwrap_or_default() {
            if !self.middle_scroll_controls_pane(session) {
                self.follow_pane(session, cx);
            }
        }
        // The rail is an overlay only while it holds the keys. A path that
        // focuses the composer directly (a drop, a pasted image, the
        // paperclip's deferred request) never asks `refocus_screen`, so the
        // claim is repaired here: an armed rail over a focused input would
        // steal the caret back on the next transition (#2665).
        if self.rail_focused
            && self.rail_open
            && self
                .composer_entity(cx)
                .read(cx)
                .focus_handle
                .is_focused(window)
        {
            self.rail_focused = false;
        }
        // The plan strip follows the open conversation. Where a plan lives
        // depends on what the session is bound to, so the lookup is a DB read
        // and runs once per selection, off the paint path; the strip itself
        // then only stats the file.
        if self.plan_for != self.chat_session {
            self.plan_for = self.chat_session;
            match self.chat_session {
                Some(sid) => {
                    self.plan.set_path(None);
                    // The lookup reads the DB through the pool, and deadpool
                    // needs a Tokio reactor; the gpui executor is not one, so
                    // this never runs inline (smoke-panic receipt in the gate
                    // log, 2026-10-31). Same pattern as every bridge call.
                    agent_bridge::spawn_plan_path(self.db_path.clone(), sid, self.sink.clone());
                }
                None => self.plan.set_path(None),
            }
        }
        self.plan.refresh();
        // A task that just settled rings once (#2691). This is the window's
        // tick: the strip was re-read by mtime just above, and the pane
        // strips are re-read again when the split body is built below, so a
        // pane that settles between frames is caught on the next one.
        self.chime_settled_plan_tasks(cx);

        // The expanded rail is a cached view of its own (#2933). The closed
        // rail never resolves that frame: it keeps only the logo and reopen
        // button, so no session maps are built behind the compact strip.
        let rail = if self.rail_open || self.screen == Screen::Onboard {
            self.rail_element(cx)
        } else {
            self.collapsed_rail_element(cx)
        };

        // One label drives the in-window chrome and the OS titlebar: the old
        // static "usage" title sat over an open conversation. The owner
        // wanted the usage screen's whole identity on the one line the
        // chrome already draws: `OpenCrabs Usage · All Time · v0.5.15`, the
        // middle word tracking the period switch (t/w/m/a).
        let label = match self.screen {
            Screen::Usage => format!(
                "OpenCrabs Usage \u{b7} {} \u{b7} v{}",
                period_word(self.dash.period),
                env!("CARGO_PKG_VERSION")
            ),
            Screen::Files => "OpenCrabs \u{b7} files".to_string(),
            Screen::Models => "OpenCrabs \u{b7} models".to_string(),
            Screen::Skills => "OpenCrabs \u{b7} skills".to_string(),
            Screen::Projects => "OpenCrabs \u{b7} projects".to_string(),
            Screen::DirectoryPicker => "OpenCrabs \u{b7} directory".to_string(),
            Screen::Onboard => "OpenCrabs \u{b7} onboarding".to_string(),
            Screen::MissionControl => "OpenCrabs \u{b7} mission control".to_string(),
            Screen::Chat => format!(
                "OpenCrabs \u{b7} {}",
                self.session_title().unwrap_or_else(|| "chat".to_string())
            ),
        };
        window.set_window_title(&label);
        // The header is its own surface: a panel one step above the chat and a
        // divider under it, so the title does not read as part of the history.
        let chrome = window_controls::drag_handle(div())
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(rgb(palette::current().panel))
            .border_b_1()
            .border_color(rgb(palette::current().panel_hi))
            // The header is the window's own bar on Linux, where the
            // compositor draws none (#2813), so a press on it moves the
            // window instead of selecting text. On the chat screen there is no
            // header (#2596): the rail's brand row is the handle there (#3014).
            .child(icon::banner_logo())
            .child(div().text_lg().child(SharedString::from(label)));

        // One click listener per turn anchor, minted here because only the
        // view's own `cx` can build them; the transcript takes them by value.
        let mut fold_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for anchor in self.chat_state.transcript.anchors() {
            let key = anchor.clone();
            let handler = cx.listener(move |this, _ev, _window, cx| {
                this.toggle_turn(&key, cx);
            });
            fold_handlers.insert(anchor, std::rc::Rc::new(handler));
        }
        // The live card's own click, minted here for the same reason as the
        // folds: only this `cx` can build one, and the card that is on screen
        // is the one a click must fold (#2442).
        let live_click: transcript::ClickHandler =
            std::rc::Rc::new(cx.listener(|this, ev: &gpui::ClickEvent, _window, cx| {
                // A release that ended a drag is the end of a copy, not a
                // request to fold. gpui fires a click on every release with no
                // movement threshold, and the row's own release handler clears
                // the selection before this listener runs, so the positions are
                // the only evidence left: without this, copying a line out of
                // the card also closed the card the line was in (#2678).
                if let gpui::ClickEvent::Mouse(click) = ev
                    && (click.up.position - click.down.position).magnitude() > 2.0
                {
                    return;
                }
                this.chat_state.transcript.live_expanded =
                    !this.chat_state.transcript.live_expanded;
                cx.notify();
            }));
        // The marker above the transcript's drawn window loads the next one
        // (#2805). Minted here because the transcript state lives on this view.
        let older_click: transcript::ClickHandler =
            std::rc::Rc::new(cx.listener(|this, _ev: &gpui::ClickEvent, _window, cx| {
                if this.chat_state.transcript.load_older() {
                    cx.notify();
                }
            }));
        // One click listener per attachment tile, minted here for the same
        // reason as the folds: only this `cx` can build one, and each tile has
        // to open its own file (#2395).
        let mut attachment_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for key in transcript::attachment_paths_with_live(
            &self.chat_state.transcript,
            // The window holds exactly the visible conversation's turn: it was
            // parked and handed back by `swap_visible_turn`, so a live card
            // here always belongs to the session on screen (#2616).
            &self.turn.live,
        ) {
            let path = std::path::PathBuf::from(key.clone());
            let handler = cx.listener(move |this, _ev, _window, cx| {
                this.open_preview(path.clone(), cx);
            });
            attachment_handlers.insert(key, std::rc::Rc::new(handler));
        }

        // One hook per stored row (#2613): a right press opens that row's
        // menu, and while select mode is on a left click toggles it. Minted
        // per frame like the tiles above, because only this `cx` can build a
        // listener and each row has to answer for itself.
        let msg_order: Vec<Uuid> = self.chat_state.transcript.message_ids();
        let select_on = self.msg_select.is_active();
        let msg_picked: std::collections::HashSet<Uuid> = msg_order
            .iter()
            .copied()
            .filter(|id| self.msg_select.contains(id))
            .collect();
        let mut message_click: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        let mut message_press: std::collections::HashMap<Uuid, transcript::PressHandler> =
            std::collections::HashMap::new();
        // The copy behind one answer's hover button (#2750). Only answers get
        // one: the reader's own rows already carry a pencil, and two buttons in
        // one corner would fight for the same pixel.
        let mut message_copy: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        // The row itself goes with the press (#2827), so the menu's actions
        // read the message that was right-clicked rather than looking an id up
        // in whatever transcript happens to be on screen by then.
        let msg_rows: Vec<Message> = msg_order
            .iter()
            .filter_map(|id| {
                self.chat_state
                    .transcript
                    .rows
                    .iter()
                    .find(|m| m.id == *id)
                    .cloned()
            })
            .collect();
        for row in msg_rows {
            let id = row.id;
            if select_on {
                let handler = cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    this.msg_toggle(id, ev.modifiers().shift, cx);
                });
                message_click.insert(id, std::rc::Rc::new(handler));
            } else if row.role == "user"
                && opencrabs::ui_core::message_reader::opens_in_reader(&row.content)
            {
                // A prompt long enough to stop reading well in the column opens
                // in the reader on a click (#3053). Nothing is cut to reach
                // that: the row still draws every line, and the reader is the
                // comfortable place to read it, not the only place.
                let title = opencrabs::ui_core::message_reader::title(&row.content);
                let content = row.content.clone();
                let handler = cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    // A drag is a copy gesture, not a request to open (#2678).
                    if let gpui::ClickEvent::Mouse(click) = ev
                        && (click.up.position - click.down.position).magnitude() > 2.0
                    {
                        return;
                    }
                    this.open_message_preview(title.clone(), content.clone(), cx);
                });
                message_click.insert(id, std::rc::Rc::new(handler));
            }
            if row.role == "assistant" {
                let content = row.content.clone();
                let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.msg_copy_markdown(id, content.clone(), cx);
                });
                message_copy.insert(id, std::rc::Rc::new(handler));
            }
            let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.msg_menu_open(row.clone(), None, ev.position, cx);
            });
            message_press.insert(id, std::rc::Rc::new(handler));
        }
        // Exact live entries get their own captured rows (#2925). Keying by the
        // row they draw keeps the handler attached as later lines arrive, and
        // it covers the reader's own rows too: the question and every message
        // the agent took mid-turn are drawn from the turn, not from `rows`.
        let mut live_press: std::collections::HashMap<
            transcript::LiveRow,
            transcript::PressHandler,
        > = std::collections::HashMap::new();
        // The pencil behind those same rows (#2925). A turn in flight is
        // exactly when the reader wants to take back what he just said, so the
        // one row on screen with no pencil would be his own.
        let mut live_edit: std::collections::HashMap<
            transcript::LiveSlot,
            transcript::ClickHandler,
        > = std::collections::HashMap::new();
        if let Some(session) = self.chat_session {
            for (target, row) in transcript::live_action_rows(&self.turn.live, session) {
                if let transcript::LiveRow::User(slot) = target {
                    let text = row.content.clone();
                    let edit = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.msg_edit_begin_live(slot, text.clone(), cx);
                    });
                    live_edit.insert(slot, std::rc::Rc::new(edit));
                }
                let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.live_menu_open(row.clone(), None, ev.position, cx);
                });
                live_press.insert(target, std::rc::Rc::new(handler));
            }
        }
        // One toggle per chip on screen (#2825). Minted only for the pictures
        // the loaded rows actually carry, so a row with no reactions costs
        // nothing here. The pairs are collected first for the same reason the
        // row ids are: a listener is built from `cx`, not from the state.
        let reaction_pairs: Vec<(Uuid, String)> = self
            .chat_state
            .transcript
            .reactions
            .iter()
            .flat_map(|(id, list)| list.iter().map(move |(name, _)| (*id, name.clone())))
            .collect();
        let mut reaction_click: std::collections::HashMap<
            (Uuid, String),
            transcript::ClickHandler,
        > = std::collections::HashMap::new();
        for (id, name) in reaction_pairs {
            let picked = name.clone();
            let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                // A chip on the window's own row reacts on that row, whatever
                // surface the last row menu happened to be opened in (#2827).
                this.msg_menu_leaf = None;
                this.msg_menu_row = None;
                this.msg_react_pick(id, picked.clone(), cx);
            });
            reaction_click.insert((id, name), std::rc::Rc::new(handler));
        }
        // One edit per stored user row (#2826): the pencil opens a field in
        // that row's own card, with a check that sends the rewrite and a cross
        // that closes it. Answers are left out, so the map holds only the rows
        // a rewrite makes sense for.
        let edit_rows: Vec<(Uuid, i32)> = self
            .chat_state
            .transcript
            .rows
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| (m.id, m.sequence))
            .collect();
        let mut msg_edit: std::collections::HashMap<Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for (id, sequence) in edit_rows {
            let handler = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.msg_edit_begin(id, sequence, cx);
            });
            msg_edit.insert(id, std::rc::Rc::new(handler));
        }
        // A left press anywhere in the transcript dismisses the message menu
        // (#2613): the press is elsewhere, so the menu goes with it, the same
        // rule the rail applies to its own menu (#2354). The card and the rows
        // stop this from firing on their own presses.
        let msg_dismiss: transcript::PressHandler =
            Rc::new(cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                let closed_menu = this.msg_menu.take().is_some();
                let closed_picker = this.react_menu.take().is_some();
                if closed_picker {
                    this.react_menu_at = None;
                }
                if closed_menu || closed_picker {
                    this.msg_menu_row = None;
                    this.msg_menu_leaf = None;
                    cx.notify();
                }
            }));
        // A wheel tick over the conversation is the reader's own hand on it
        // (#2698): GPUI moves the viewport, and this only says whether the
        // follow goes with it. A tick up releases it, and the frame loop
        // re-arms it the moment the reader is back at the bottom.
        let wheel: transcript::ScrollHandler =
            Rc::new(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                // The card is pinned to a window point, not to its row, so a
                // scroll leaves it floating over a row it no longer names
                // (#2690). The wheel closes it.
                if this.msg_menu.take().is_some() {
                    this.msg_menu_row = None;
                    this.msg_menu_leaf = None;
                    cx.notify();
                }
                if this.react_menu.take().is_some() {
                    this.react_menu_at = None;
                    cx.notify();
                }
                if !wheel_releases_follow(wheel_dy(&ev.delta)) {
                    return;
                }
                this.release_transcript_follow();
                cx.notify();
            }));
        let middle_click: transcript::PressHandler =
            Rc::new(cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                if let Some(session) = this.chat_session {
                    this.middle_scroll_down(
                        middle_scroll::MiddleScrollTarget::Chat(session),
                        ev.position,
                        cx,
                    );
                }
            }));

        // The transcript plans its attachment tiles from the width the pane
        // actually has, read from the live viewport rather than a constant, so
        // a row can never be planned wider than the box it is drawn into
        // (#2395). The usage screen loses its side panel from the same number.
        let pane_width = attachment_view::content_width(
            f32::from(window.viewport_size().width),
            self.visible_rail_width() * self.rem,
        );
        let picker_layout = model_layout::model_layout(
            pane_width,
            self.rem,
            BASE_REM,
            self.screen == Screen::Onboard,
        );

        // The chat's share of the pane area comes from the tree: a leaf under
        // stacked splits spans the full width, and a leaf beside a sibling
        // takes its share of it (#2472, #2473). A row is still never planned
        // wider than its box. The measure is read here rather than inside the
        // chat arm because the conversation and the chrome under it have to
        // agree on one width (#2588).
        let chat_share = self
            .panes
            .as_ref()
            .zip(self.chat_session)
            .and_then(|(tree, id)| tree.leaf_width_share(id))
            .unwrap_or(1.0);
        let primary_width = if chat_share < 1.0 {
            split_pane_width(pane_width, false, chat_share)
        } else {
            pane_width
        };
        // A single pane draws the conversation at a readable measure and
        // centres it; a split window is exempt and keeps every pane at the span
        // the tree hands it (#2586). The count is resolved once: it also
        // decides whether the window footer row draws at all (#2641).
        let pane_count = self.panes.as_ref().map_or(1, |tree| tree.leaves().len());
        let split_window = pane_count > 1;
        let chat_measure_px = chat_measure(primary_width, split_window);
        // Only a pane wider than the measure is wrapped, and a split pane is
        // never wrapped: its measure is already its own width.
        let centred_chat = chat_measure_px < primary_width;

        let (middle, footer): (AnyElement, AnyElement) = match self.screen {
            // The theme modal is an overlay over the conversation, not a
            // screen of its own (#2619): the chat draws as usual and the
            // modal is attached at the root, above the panes.
            Screen::Chat => {
                // The chat is the primary pane, and a split draws its session
                // beside or below it (#2438). Two panes share the content
                // width, less the gap_2 that separates them, so a row is
                // never planned wider than the box it is painted into.
                // The chat's share of the pane area comes from the tree: a
                // leaf under stacked splits spans the full width, and a leaf
                // beside a sibling takes its share of it (#2472, #2473). A
                // row is still never planned wider than its box.
                let measure = chat_measure_px;
                // Minted before the view borrows the transcript (#2826): the
                // two clicks are listeners, so they need `self` mutably.
                let inline_edit = self.inline_edit_hooks(cx);
                let primary = transcript::view(
                    &self.chat_state.transcript,
                    &self.chat_state.scroll,
                    // The visible conversation's turn, never another's: the
                    // window keeps exactly one and swaps them on a switch
                    // (#2616).
                    &self.turn.live,
                    self.turn.busy,
                    measure,
                    f32::from(window.rem_size()),
                    fold_handlers,
                    transcript::ViewHooks {
                        attachments: attachment_handlers,
                        drag: Some(self.copy_drag.clone()),
                        wheel: Some(wheel),
                        middle_click: Some(middle_click),
                        ctx_used: self.turn.ctx_tokens,
                        ctx_max: self.ctx_max,
                        live_click: Some(live_click.clone()),
                        message_click: message_click.clone(),
                        message_press: message_press.clone(),
                        live_press,
                        live_edit,
                        selection: select_on.then(|| msg_picked.clone()),
                        menu_dismiss: Some(msg_dismiss.clone()),
                        older: Some(older_click.clone()),
                        reactions: self.chat_state.transcript.reactions.clone(),
                        reaction_click: reaction_click.clone(),
                        edit: msg_edit.clone(),
                        // The field a rewrite draws in its own card (#2826),
                        // minted here because its two clicks are listeners.
                        edit_row: inline_edit,
                        // The copy on an answer's hover (#2750) and the row
                        // whose check is up, so a copy confirms itself without
                        // moving the row.
                        copy: message_copy.clone(),
                        copied: self.copied_row(),
                        // Whether this transcript is keeping its own bottom
                        // (#3027). Only the reader's place can say whether a
                        // row that runs past the box is a frame to correct or
                        // a place they chose to be.
                        following: self.chat_state.follow.is_following(),
                    },
                );
                // The conversation takes the measure and the column centres it
                // (#2586). The cap goes on the transcript itself instead of on
                // a wrapper element: that wrapper was the one node a single
                // pane had and a split never did, and it is what the black
                // chat came from (#2981). Nothing here adds an element to the
                // surface's tree, it only gives the surface its width. The
                // width is set while the view is still its own type, because
                // an erased element takes no style.
                let primary = if centred_chat {
                    primary.w(px(measure)).into_any_element()
                } else {
                    primary.into_any_element()
                };
                // The panes come from the tree (#2472): one leaf is the
                // window as it was, and each split adds a divider and the
                // panes its children hold.
                let body = self.panes_body(primary, pane_width, true, cx);
                (
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        // A capped surface is centred by its parent, so the
                        // slack falls to both margins and the chrome under it
                        // keeps the same axis. A split never reaches this: its
                        // measure is its own width.
                        .when(centred_chat, |d| d.items_center())
                        .child(body)
                        .into_any_element(),
                    if window_footer_yields(pane_count) {
                        // A split draws one footer per pane (#2593), so the
                        // window row stands down and yields its height, the way
                        // the terminal's status bar does (#2575, #2641). Two
                        // rows printed the model twice, and the window one
                        // could only ever name the chat's pair while the pane
                        // above it named another.
                        div().into_any_element()
                    } else {
                        // Key hints left, identity right. The ctx budget used
                        // to sit at the right edge too, and it was the same
                        // number the tool block's own footer already carries:
                        // read twice, on every turn (#2631).
                        // Who is answering, from where, on what branch, and how
                        // fast: the same fields the TUI's footer carries.
                        // The open conversation's own pair, not the window's
                        // bootstrap provider: a pick pins the model per
                        // session, so the global slot never moves and this row
                        // kept naming the pair it started with (#2634).
                        let (provider, model) = match agent_bridge::bridge() {
                            Some(b) => match self.chat_session {
                                Some(sid) => {
                                    let (provider, model) =
                                        b.service.provider_pair_for_session(sid);
                                    let label =
                                        model.rsplit('/').next().unwrap_or(&model).to_string();
                                    (provider, label)
                                }
                                None => (b.service.provider_name(), b.service.provider_model()),
                            },
                            None => ("-".to_string(), "-".to_string()),
                        };
                        // The session name leads, as in the TUI footer. The row
                        // across the top that used to carry it was removed
                        // (#2596), and in a split this footer stands down while
                        // each pane's border names its own conversation, so the
                        // name is not printed twice anywhere (#2578).
                        // The place the footer names is the open
                        // conversation's own, never the directory the process
                        // was started in (#3036): the launch directory never
                        // moves, so a `/cd` could change where the agent works
                        // and leave this row pointing at the old project. The
                        // branch is read from that same directory, and only
                        // when it changes, because `git` is a subprocess.
                        let (cwd, branch) = self.footer_place();
                        let identity = format!(
                            "{} · {}",
                            self.session_title().unwrap_or_else(|| "chat".to_string()),
                            status_identity(
                                &provider,
                                &model,
                                &cwd,
                                &branch,
                                self.turn.live.tokens_per_sec(),
                            )
                        );
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .w_full()
                            // The single-session dock owns this row now: it is
                            // the bottom half of the composer card, under the
                            // hairline. It needs its own padding because the
                            // chat column's `gap_3` no longer separates it
                            // from the input, and a bare text row would sit
                            // on the card's rounded bottom edge. The split
                            // arm above yields an empty row, so a pane footer
                            // keeps the chrome it already had.
                            .px_3()
                            .py_2()
                            // The Seeker opens with a chord nothing on
                            // screen named (#2747). The hint is permanent and
                            // clickable: a hover tooltip is invisible to
                            // exactly the people who need it, and the chord
                            // printed matches the platform the window runs on.
                            .child(
                                div()
                                    .id("seeker-hint")
                                    .flex()
                                    .flex_none()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .rounded_sm()
                                    .px_1()
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_muted))
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                    .on_click(cx.listener(
                                        |this, _ev: &gpui::ClickEvent, _window, cx| {
                                            this.open_finder(cx);
                                        },
                                    ))
                                    .child(SharedString::from(
                                        opencrabs::ui_core::palette::seeker_chord_label(),
                                    ))
                                    .child("· seek"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_muted))
                                    .child("ctrl+s · sidebar"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_faint))
                                    .child(SharedString::from(identity)),
                            )
                            .into_any_element()
                    },
                )
            }
            Screen::DirectoryPicker => {
                // The terminal's /cd dialog, as a screen (#2527): the path on
                // top, the rows under it, one footer line naming the keys.
                // Flat surfaces, the same styling law the rest of the window
                // follows (#2528).
                let shown = palette::current();
                let last = self.cd_entries.len().saturating_sub(1);
                let current = self.cd_selected.min(last);
                let mut list = div().flex().flex_col().gap_1();
                for (index, path) in self.cd_entries.iter().enumerate() {
                    let name = if path.ends_with("..") {
                        "..".to_string()
                    } else {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?")
                            .to_string()
                    };
                    let selected = index == current;
                    let mut row = div()
                        .id(SharedString::from(format!("cd-row-{index}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_2()
                        .py_1();
                    if selected {
                        row = row.bg(rgb(shown.panel_hi));
                    }
                    let row = row
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.cd_selected = index;
                            this.cd_descend(index, cx);
                        }))
                        .child(text(name, Size::Sm, shown.text))
                        .child(text(
                            if path.ends_with("..") { "up" } else { "open" },
                            Size::Xs,
                            shown.text_muted,
                        ));
                    list = list.child(row);
                }
                let scroller = div()
                    .id("cd-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list);
                let header = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(text(
                        SharedString::from(self.cd_dir.display().to_string()),
                        Size::Sm,
                        shown.text,
                    ))
                    .child(text(
                        if self.cd_show_hidden {
                            "hidden on"
                        } else {
                            "hidden off"
                        },
                        Size::Xs,
                        shown.text_muted,
                    ));
                let mut panel = div()
                    .id("cd-panel")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(rgb(shown.panel_hi))
                    .bg(glass_fill(shown.panel))
                    .child(header)
                    .child(scroller);
                if self.cd_entries.is_empty() {
                    panel = panel.child(text("no subdirectories", Size::Sm, shown.text_muted));
                }
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        format!("{}/{}", current + 1, self.cd_entries.len()),
                    ))
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        "up/down \u{b7} walk \u{b7} enter \u{b7} open \u{b7} . \u{b7} hidden \u{b7} tab/space \u{b7} select here \u{b7} esc \u{b7} back",
                    ));
                (panel.into_any_element(), footer.into_any_element())
            }
            Screen::Usage => {
                let body: Div = if self.dash.cache_detail_open {
                    let cache = match &self.state {
                        LoadState::Loaded(vm) => vm.cache.as_ref(),
                        LoadState::Failed(_) => None,
                    };
                    breakdown::panel(cache)
                } else {
                    let content = match &self.state {
                        LoadState::Loaded(vm) => usage_view::dashboard(vm, self.dash.focused_card),
                        LoadState::Failed(err) => div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .text_color(rgb(palette::current().red))
                                    .child("Usage data failed to load"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(palette::current().text_muted))
                                    .child(err.clone()),
                            ),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(self.usage_period_switcher(cx))
                        .child(content)
                };
                // The scrollable middle: an overflow-scroll element's minimum size
                // is zero, so `flex_1` bounds it to the space left over and the
                // content inside scrolls. The footer sits outside it, so the key
                // hints never scroll away.
                let scroller = div()
                    .id("usage-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body);
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(breakdown::footer(&self.dash))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("alt+space · seek help")
                            .child("ctrl+s · sidebar"),
                    );
                (scroller.into_any_element(), footer.into_any_element())
            }
            Screen::Files => {
                // One row per tracked file, the same door the TUI opens with
                // `f` on its sessions browser. Empty without any, errored
                // with the reason, never a mock row.
                let scroller = div()
                    .id("files-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.files_view());
                let count = self.files.len();
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(format!(
                                "{count} file{} tracked",
                                if count == 1 { "" } else { "s" }
                            ))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("alt+space · seek help")
                            .child("ctrl+s · sidebar"),
                    );
                (scroller.into_any_element(), footer.into_any_element())
            }
            // The shared modal paints Projects once, above the window chrome.
            Screen::Projects => (div().flex_1().into_any_element(), div().into_any_element()),
            Screen::MissionControl => {
                let shown = palette::current();
                // The cards reflow with the width the pane actually has, read
                // from the live window so a resize lands on the same frame
                // (#2459). The rail is subtracted only when it is drawn.
                let viewport = f32::from(window.viewport_size().width);
                let rail = self.visible_rail_width() * self.rem;
                let cols = mission_control::grid_columns(viewport - rail, self.rem);
                // The cards draw the snapshot the four services produced on
                // entry; the frame itself never reads SQLite (#2426).
                let body = mission_control_body(
                    self.mc.as_ref(),
                    &self.mc_nav,
                    self.mc_err.as_ref(),
                    self.mc_loading,
                    shown,
                    cols,
                );
                // A decision is never silent: the tool's own sentence sits
                // above the panels, green when it landed and red when it did
                // not, until the screen is left.
                let body = match self.mc_notice.clone() {
                    Some(notice) => {
                        let color = if notice.ok { shown.green } else { shown.red };
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(
                                div()
                                    .px_4()
                                    .py_1()
                                    .text_xs()
                                    .text_color(rgb(color))
                                    .child(notice.text),
                            )
                            .child(body)
                            .into_any_element()
                    }
                    None => body.into_any_element(),
                };
                // The terminal's footer, in its own words: the keys the screen
                // answers, so a hand that knows the terminal is already home.
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_1()
                    .child(text("Tab/h/l switch panel", Size::Xs, shown.text_muted))
                    .child(text("↑↓ navigate", Size::Xs, shown.text_muted))
                    .child(text("Enter detail", Size::Xs, shown.text_muted))
                    .child(text("a apply", Size::Xs, shown.text_muted))
                    .child(text("r reject", Size::Xs, shown.text_muted))
                    .child(text("D/W/M/A filter", Size::Xs, shown.text_muted))
                    .child(text("L logs", Size::Xs, shown.text_muted))
                    .child(text("Esc close", Size::Xs, shown.text_muted));
                (body.into_any_element(), footer.into_any_element())
            }
            Screen::Onboard => match self.onboard.as_mut() {
                None => {
                    // Unreachable through the door: `open_onboard` builds the
                    // wizard before it shows the screen. A window in this state
                    // says so instead of drawing a blank pane.
                    let shown = palette::current();
                    (
                        div()
                            .flex_1()
                            .p_6()
                            .child(text(
                                "The wizard is closed. Type /onboard to open it.",
                                Size::Sm,
                                shown.text_muted,
                            ))
                            .into_any_element(),
                        div().into_any_element(),
                    )
                }
                Some(ob) => {
                    let shown = palette::current();
                    let mut model_footer = None;
                    let (number, total) = ob.progress();
                    // The rail is the flow the current mode walks, with the
                    // step the user stands on marked. Channel sub-steps are
                    // reached from Channels and are not stops.
                    let mut rail = div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(picker_layout.onboarding_rail_width))
                        .flex_none()
                        .min_w_0();
                    if picker_layout.onboarding_rail_width > 0.0 {
                        rail = rail.pr_5();
                    } else {
                        rail = rail.hidden();
                    }
                    for (n, title, here) in ob.rail() {
                        rail = rail.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(text(
                                    format!("{n:>2}"),
                                    Size::Xs,
                                    if here { shown.amber } else { shown.text_faint },
                                ))
                                .child(
                                    text(
                                        title,
                                        Size::Sm,
                                        if here {
                                            shown.text_strong
                                        } else {
                                            shown.text_muted
                                        },
                                    )
                                    .flex_1()
                                    .min_w_0()
                                    .truncate(),
                                ),
                        );
                    }
                    let mut column = div().flex().flex_col().gap_3().flex_1().min_w_0().min_h_0();
                    column = column.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(text(
                                format!("step {number} of {total}"),
                                Size::Xs,
                                shown.text_faint,
                            ))
                            .child(text(ob.title(), Size::Xl, shown.text_title)),
                    );

                    if ob.step() == onboard::OnboardingStep::ProviderAuth {
                        // The pair this step is pointed at, above the list: the
                        // one line that answers "what will this window answer
                        // with" without reading two lists.
                        column = column.child(text(
                            format!("{} · {}", ob.provider_id(), ob.model()),
                            Size::Xs,
                            shown.text_faint,
                        ));
                        // The provider list, the help the terminal shows under
                        // it, then the fields that provider needs. Every row is
                        // a real click: nothing here is a label pretending.
                        // A window over the list, not the whole list. The
                        // terminal windows this exact list (`visible_window`)
                        // because every provider painted pushed the fields
                        // below it off screen. The arrows walk all of them and
                        // the counts say what is out of view.
                        let selected = ob.wizard.ps.selected_provider;
                        let (provider_rows, hidden_above, hidden_below) = ob.provider_rows(14);
                        let mut list = div().flex().flex_col().gap_1().w_full();
                        if hidden_above > 0 {
                            list = list.child(text(
                                format!("\u{2191} {hidden_above} more"),
                                Size::Xs,
                                shown.text_faint,
                            ));
                        }
                        for (index, provider_name) in provider_rows {
                            let here = index == selected;
                            let mut row = div()
                                .id(SharedString::from(format!("onboard-provider-{index}")))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .child(text(
                                    if here { "\u{25cf}" } else { " " },
                                    Size::Sm,
                                    if here { shown.amber } else { shown.text_muted },
                                ))
                                .child(text(
                                    provider_name,
                                    Size::Sm,
                                    if here { shown.text_strong } else { shown.text },
                                ));
                            if here {
                                row = row.bg(rgb(shown.panel_hi));
                            }
                            list = list.child(row.on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    if let Some(ob) = this.onboard.as_mut() {
                                        ob.select_provider(index);
                                        ob.wizard.ps.model_filter.clear();
                                    }
                                    this.models_query.clear();
                                    this.models_index = 0;
                                    this.refresh_onboard_models();
                                    cx.notify();
                                },
                            )));
                        }
                        column = column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(text("provider", Size::Xs, shown.text_faint))
                                .child(div().id("onboard-providers").w_full().child(list).child(
                                    if hidden_below > 0 {
                                        text(
                                            format!("\u{2193} {hidden_below} more below"),
                                            Size::Xs,
                                            shown.text_faint,
                                        )
                                    } else {
                                        text("", Size::Xs, shown.text_faint)
                                    },
                                )),
                        );

                        let mut help = div().flex().flex_col().gap_1();
                        for line in ob.help_lines() {
                            help = help.child(text(*line, Size::Xs, shown.text_muted));
                        }
                        column = column.child(help);

                        if let Some(key_field) = ob.key_field() {
                            let focused = ob.field == key_field;
                            let saved = ob.wizard.ps.api_key_input
                                == opencrabs::tui::onboarding::EXISTING_KEY_SENTINEL;
                            let value = if ob.wizard.ps.api_key_input.is_empty() {
                                "paste the key".to_string()
                            } else if saved {
                                "a key is already saved \u{2014} type to replace it".to_string()
                            } else {
                                ob.wizard.ps.api_key_input.clone()
                            };
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text(
                                        ob.key_label().to_string(),
                                        Size::Xs,
                                        shown.text_faint,
                                    ))
                                    .child(
                                        div()
                                            .id("onboard-key")
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .px_3()
                                            .py_2()
                                            .rounded(px(6.0))
                                            .bg(rgb(if focused {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .border_1()
                                            .border_color(rgb(if focused {
                                                shown.focus
                                            } else {
                                                shown.track
                                            }))
                                            .child(text(
                                                value,
                                                Size::Sm,
                                                if ob.wizard.ps.api_key_input.is_empty() || saved {
                                                    shown.text_faint
                                                } else {
                                                    shown.text
                                                },
                                            ))
                                            .child(text(
                                                if focused { "\u{258f}" } else { "" },
                                                Size::Sm,
                                                shown.amber,
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.field = key_field;
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            );
                        }

                        if ob.wizard.ps.is_custom() {
                            let base = 2 + usize::from(ob.key_needed());
                            let fields = [
                                ("name", ob.wizard.ps.custom_name.clone(), base),
                                ("base url", ob.wizard.ps.base_url.clone(), base + 1),
                                ("model", ob.wizard.ps.custom_model.clone(), base + 2),
                            ];
                            for (label, value, field) in fields {
                                let focused = ob.field == field;
                                column = column.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(text(label, Size::Xs, shown.text_faint))
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "onboard-custom-{field}"
                                                )))
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .px_3()
                                                .py_2()
                                                .rounded(px(6.0))
                                                .bg(rgb(if focused {
                                                    shown.panel_hi
                                                } else {
                                                    shown.panel
                                                }))
                                                .border_1()
                                                .border_color(rgb(if focused {
                                                    shown.focus
                                                } else {
                                                    shown.track
                                                }))
                                                .child(text(
                                                    if value.is_empty() {
                                                        "\u{2014}".to_string()
                                                    } else {
                                                        value
                                                    },
                                                    Size::Sm,
                                                    if focused {
                                                        shown.text
                                                    } else {
                                                        shown.text_normal
                                                    },
                                                ))
                                                .child(text(
                                                    if focused { "\u{258f}" } else { "" },
                                                    Size::Sm,
                                                    shown.amber,
                                                ))
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        if let Some(ob) = this.onboard.as_mut() {
                                                            ob.field = field;
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                );
                            }
                        }

                        let active_provider = if ob.wizard.ps.is_custom() {
                            ob.wizard.ps.custom_name.clone()
                        } else {
                            ob.provider_id().to_string()
                        };
                        let rendered = model_picker::render(
                            cx,
                            model_picker::ModelPickerView {
                                rows: &self.models_rows,
                                query: &self.models_query,
                                selected: self.models_index,
                                fetching: self.models_fetching,
                                pending: self.models_pending.len(),
                                error: self.models_fetch_error.as_deref(),
                                active_provider: &active_provider,
                                onboarding: true,
                                search_focused: ob.field == ob.search_field(),
                                max_models: 14,
                                row_layout: picker_layout.model_row,
                                provider_label_width: picker_layout.provider_label_width,
                                show_provider_labels: true,
                            },
                        );
                        model_footer = Some(rendered.footer);
                        column = column.child(rendered.body);
                    } else if ob.step() == onboard::OnboardingStep::ModeSelect {
                        // The first choice the terminal asks: how much of the
                        // flow to walk. Same two options, same step counts, so
                        // the promise here matches the progress counter.
                        let current = ob.mode();
                        let mut choices = div().flex().flex_col().gap_2();
                        for (mode, title, note) in [
                            (
                                onboard::WizardMode::QuickStart,
                                "QuickStart",
                                format!(
                                    "sensible defaults, {} steps",
                                    onboard::OnboardingStep::quick_total()
                                ),
                            ),
                            (
                                onboard::WizardMode::Advanced,
                                "Advanced",
                                format!(
                                    "full control, all {} steps",
                                    onboard::OnboardingStep::total()
                                ),
                            ),
                        ] {
                            let here = current == mode;
                            // The arrows land here first on a fresh install, so
                            // the row they are on has to be visible (#2432).
                            let focused =
                                ob.focused_choice() == Some(onboard::ChoiceRow::Mode(mode));
                            choices = choices.child(
                                div()
                                    .id(SharedString::from(format!("onboard-mode-{title}")))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                    .border_1()
                                    .border_color(rgb(if focused {
                                        shown.amber
                                    } else if here {
                                        shown.focus
                                    } else {
                                        shown.track
                                    }))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused {
                                            shown.amber
                                        } else {
                                            shown.text_faint
                                        },
                                    ))
                                    .child(text(
                                        if here { "\u{25cf}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if here { shown.amber } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                title,
                                                Size::Sm,
                                                if here { shown.text_strong } else { shown.text },
                                            ))
                                            .child(text(note, Size::Xs, shown.text_muted)),
                                    )
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.set_mode(mode);
                                        }
                                        cx.notify();
                                    })),
                            );
                        }
                        column = column.child(choices);
                    } else if ob.step() == opencrabs::tui::onboarding::OnboardingStep::HealthCheck {
                        // The terminal spreads these checks over ticks so a
                        // Pending row is visible for one frame. The window
                        // resolves them on the click and paints the verdicts,
                        // which is the part anyone came for.
                        let results = ob.health().to_vec();
                        let complete = ob.health_complete();
                        let passed = ob.all_health_passed();
                        let mut rows = div().flex().flex_col().gap_1();
                        if results.is_empty() {
                            rows = rows.child(text(
                                "The checks have not run yet.",
                                Size::Sm,
                                shown.text_muted,
                            ));
                        }
                        for (label, status) in results {
                            let (mark, colour, detail) = match status {
                                opencrabs::tui::onboarding::HealthStatus::Pending => {
                                    ("\u{25cb}", shown.text_faint, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Running => {
                                    ("\u{25cb}", shown.amber, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Pass => {
                                    ("\u{2713}", shown.green, None)
                                }
                                opencrabs::tui::onboarding::HealthStatus::Fail(reason) => {
                                    ("\u{2717}", shown.red, Some(reason))
                                }
                            };
                            let mut row = div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .child(text(mark, Size::Sm, colour))
                                        .child(text(label, Size::Sm, shown.text)),
                                );
                            if let Some(reason) = detail {
                                row = row.child(text(reason, Size::Xs, shown.text_muted));
                            }
                            rows = rows.child(row);
                        }
                        column = column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(text("checks", Size::Xs, shown.text_faint))
                                .child(rows),
                        );
                        // The terminal ends the list with a verdict, so a
                        // clean run reads as one sentence and not as six
                        // marks the user has to add up.
                        if complete {
                            let (verdict, colour) = if passed {
                                ("All checks passed!", shown.green)
                            } else {
                                ("Some checks failed.", shown.red)
                            };
                            column = column.child(text(verdict, Size::Sm, colour));
                        }
                        column = column.child(
                            div()
                                .id("onboard-health-run")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel_hi))
                                .child(text(
                                    if complete {
                                        "run the checks again"
                                    } else {
                                        "run the checks"
                                    },
                                    Size::Sm,
                                    shown.text_strong,
                                ))
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    if let Some(ob) = this.onboard.as_mut() {
                                        ob.run_health();
                                    }
                                    cx.notify();
                                })),
                        );
                    } else {
                        if ob.is_channel_step() {
                            // The setup steps, verbatim from the terminal, so
                            // a window user is told where to create the bot
                            // instead of hunting for it.
                            let help_lines = ob.channel_help();
                            if !help_lines.is_empty() {
                                let mut help = div().flex().flex_col().gap_1();
                                for line in help_lines {
                                    help = help.child(text(*line, Size::Xs, shown.text_muted));
                                }
                                column = column.child(help);
                            }
                            // The terminal's own channel list. Toggling a row
                            // here is the same flip the terminal makes, and the
                            // fields each enabled channel asks for are painted
                            // underneath from the shared slot table.
                            let mut list = div().flex().flex_col().gap_1();
                            for (index, (name, description, on)) in
                                ob.channels().into_iter().enumerate()
                            {
                                // The row the keyboard stands on is the row
                                // Space flips, so it has to be visible.
                                let focused = ob.field == index;
                                let mut row = div()
                                    .id(SharedString::from(format!("onboard-channel-{index}")))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused {
                                            shown.amber
                                        } else {
                                            shown.text_faint
                                        },
                                    ))
                                    .child(text(
                                        if on { "\u{2713}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if on { shown.green } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                name,
                                                Size::Sm,
                                                if on { shown.text_strong } else { shown.text },
                                            ))
                                            .child(text(description, Size::Xs, shown.text_muted)),
                                    );
                                if focused {
                                    row = row.bg(rgb(shown.panel_hi));
                                }
                                list = list.child(row.on_click(cx.listener(
                                    move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.field = index;
                                            ob.toggle_channel(index);
                                        }
                                        cx.notify();
                                    },
                                )));
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("channels", Size::Xs, shown.text_faint))
                                    .child(list),
                            );
                        }

                        if ob.step() == onboard::OnboardingStep::VoiceSetup {
                            // The terminal asks this step as a radio choice first
                            // and only then shows the fields that choice needs. Same
                            // options, same labels, same order.
                            let mut rows = div().flex().flex_col().gap_1();
                            for (provider, label, here) in ob.stt_options() {
                                // The row the arrows and Space act on, so it has
                                // to be visible: caret when focused, filled dot
                                // when picked (#2432).
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Stt(provider));
                                rows = rows.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-stt-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if here { "\u{25cf}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if here { shown.amber } else { shown.text_faint },
                                        ))
                                        .child(text(
                                            label,
                                            Size::Sm,
                                            if here { shown.text_strong } else { shown.text },
                                        ))
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if let Some(ob) = this.onboard.as_mut() {
                                                ob.set_stt_provider(provider);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("transcription", Size::Xs, shown.text_faint))
                                    .child(rows),
                            );

                            let mut rows = div().flex().flex_col().gap_1();
                            for (provider, label, here) in ob.tts_options() {
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Tts(provider));
                                rows = rows.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-tts-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if here { "\u{25cf}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if here { shown.amber } else { shown.text_faint },
                                        ))
                                        .child(text(
                                            label,
                                            Size::Sm,
                                            if here { shown.text_strong } else { shown.text },
                                        ))
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if let Some(ob) = this.onboard.as_mut() {
                                                ob.set_tts_provider(provider);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                            column = column.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text("speech", Size::Xs, shown.text_faint))
                                    .child(rows),
                            );

                            // OpenAI TTS is ten voices behind one radio list
                            // in the terminal, so the window paints the same list
                            // instead of asking anyone to type a voice name blind.
                            if ob.wizard.tts_provider
                                == opencrabs::tui::onboarding::TtsProvider::OpenAi
                            {
                                let mut voices = div().flex().flex_col().gap_1();
                                for (voice, here) in ob.tts_voice_options() {
                                    voices = voices.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "onboard-tts-voice-{voice}"
                                            )))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .py_1()
                                            .rounded(px(6.0))
                                            .bg(rgb(if here {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .child(text(
                                                if here { "\u{25cf}" } else { "\u{25cb}" },
                                                Size::Sm,
                                                if here { shown.amber } else { shown.text_faint },
                                            ))
                                            .child(text(
                                                voice,
                                                Size::Sm,
                                                if here { shown.text_strong } else { shown.text },
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.set_tts_voice(voice);
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    );
                                }
                                column = column.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(text("voice", Size::Xs, shown.text_faint))
                                        .child(voices),
                                );
                            }

                            for note in ob.voice_notes() {
                                column = column.child(text(note, Size::Xs, shown.text_muted));
                            }
                        }

                        if ob.step() == onboard::OnboardingStep::ImageSetup {
                            // The terminal names the house provider above the
                            // switches, so the window says the same thing.
                            column = column.child(text(
                                "provider \u{b7} google \u{b7} nano banana",
                                Size::Xs,
                                shown.text_faint,
                            ));
                            // Two switches the terminal asks for on this step: read
                            // images sent to a channel, and generate images on
                            // request. Both are stored as their own config keys.
                            for (label, note, on, target) in [
                                (
                                    "understand images",
                                    "look at a picture sent in a chat",
                                    ob.image_vision(),
                                    onboard::ImageToggle::Vision,
                                ),
                                (
                                    "generate images",
                                    "draw a picture from a prompt",
                                    ob.image_generation(),
                                    onboard::ImageToggle::Generation,
                                ),
                            ] {
                                // The switch the arrows and Space act on (#2432).
                                let focused =
                                    ob.focused_choice() == Some(onboard::ChoiceRow::Image(target));
                                column = column.child(
                                    div()
                                        .id(SharedString::from(format!("onboard-image-{label}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(if focused {
                                            shown.amber
                                        } else {
                                            shown.track
                                        }))
                                        .bg(rgb(if focused { shown.panel_hi } else { shown.panel }))
                                        .child(text(
                                            if focused { "\u{25b8}" } else { " " },
                                            Size::Xs,
                                            if focused {
                                                shown.amber
                                            } else {
                                                shown.text_faint
                                            },
                                        ))
                                        .child(text(
                                            if on { "\u{2713}" } else { "\u{25cb}" },
                                            Size::Sm,
                                            if on { shown.green } else { shown.text_faint },
                                        ))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(text(label, Size::Sm, shown.text))
                                                .child(text(note, Size::Xs, shown.text_muted)),
                                        )
                                        .on_click(cx.listener(move |_this, _ev, _window, cx| {
                                            if let Some(ob) = _this.onboard.as_mut() {
                                                ob.toggle_image(target);
                                            }
                                            cx.notify();
                                        })),
                                );
                            }
                        }

                        if ob.step() == opencrabs::tui::onboarding::OnboardingStep::Daemon {
                            let on = ob.install_daemon();
                            let focused = ob.focused_choice() == Some(onboard::ChoiceRow::Daemon);
                            column = column.child(
                                div()
                                    .id("onboard-daemon")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(rgb(if focused {
                                        shown.amber
                                    } else {
                                        shown.track
                                    }))
                                    .bg(rgb(if focused { shown.panel_hi } else { shown.panel }))
                                    .child(text(
                                        if focused { "\u{25b8}" } else { " " },
                                        Size::Xs,
                                        if focused { shown.amber } else { shown.text_faint },
                                    ))
                                    .child(text(
                                        if on { "\u{2713}" } else { "\u{25cb}" },
                                        Size::Sm,
                                        if on { shown.green } else { shown.text_faint },
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(text(
                                                format!("install as {}", ob.daemon_platform()),
                                                Size::Sm,
                                                shown.text,
                                            ))
                                            .child(text(
                                                "so OpenCrabs keeps answering with no window or terminal open",
                                                Size::Xs,
                                                shown.text_muted,
                                            )),
                                    )
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        if let Some(ob) = this.onboard.as_mut() {
                                            ob.toggle_daemon();
                                        }
                                        cx.notify();
                                    })),
                            );
                        }

                        // Text rows come from the wizard's own slot table, so a
                        // row's label and the string it edits cannot drift.
                        let mut fields = div().flex().flex_col().gap_2();
                        for (label, target) in ob.slots() {
                            let focused = ob.focused_text() == Some(target);
                            let value = ob.text_mut(target).clone();
                            let saved = value == opencrabs::tui::onboarding::EXISTING_KEY_SENTINEL;
                            let (painted, faint) = if saved {
                                (
                                    "a key is already saved, type to replace it".to_string(),
                                    true,
                                )
                            } else if value.is_empty() {
                                // An empty generation model is not "nothing":
                                // the step falls back to a specific model, so
                                // the row names it instead of playing blank.
                                match target {
                                    onboard::TextTarget::ImageModel => (
                                        format!(
                                            "{} (default)",
                                            onboard::Onboard::default_image_model()
                                        ),
                                        true,
                                    ),
                                    _ => ("type here".to_string(), true),
                                }
                            } else {
                                (value, false)
                            };
                            fields = fields.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(text(label, Size::Xs, shown.text_faint))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "onboard-field-{label}"
                                            )))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .px_3()
                                            .py_2()
                                            .rounded(px(6.0))
                                            .bg(rgb(if focused {
                                                shown.panel_hi
                                            } else {
                                                shown.panel
                                            }))
                                            .border_1()
                                            .border_color(rgb(if focused {
                                                shown.focus
                                            } else {
                                                shown.track
                                            }))
                                            .child(text(
                                                painted,
                                                Size::Sm,
                                                if faint { shown.text_faint } else { shown.text },
                                            ))
                                            .child(text(
                                                if focused { "\u{258f}" } else { "" },
                                                Size::Sm,
                                                shown.amber,
                                            ))
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    if let Some(ob) = this.onboard.as_mut() {
                                                        ob.focus_text(target);
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            );
                        }
                        column = column.child(fields);
                    }

                    if ob.step() == opencrabs::tui::onboarding::OnboardingStep::BrainSetup {
                        // The step does work before it can move, so it says
                        // where that work stands instead of looking like a
                        // text field that does nothing on Enter.
                        let (mark, colour, note) = if ob.brain_generating() {
                            ("\u{25cb}", shown.amber, "writing your brain files")
                        } else if ob.brain_generated() {
                            ("\u{2713}", shown.green, "your brain files are written")
                        } else if let Some(why) = ob.brain_error() {
                            ("\u{2717}", shown.red, why)
                        } else {
                            (
                                "\u{25cb}",
                                shown.text_faint,
                                "enter writes them from the words above",
                            )
                        };
                        column = column.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .child(text(mark, Size::Sm, colour))
                                .child(text(note.to_string(), Size::Sm, colour)),
                        );
                    }

                    if let Some(err) = ob.error() {
                        column = column.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(rgb(shown.panel))
                                .border_1()
                                .border_color(rgb(shown.red))
                                .child(text(err.to_string(), Size::Sm, shown.red)),
                        );
                    }

                    let primary_hint = if ob.is_channel_step() {
                        // Space is the key that flips a channel here, so the
                        // hint names it instead of "save".
                        "space \u{b7} toggle \u{b7} enter \u{b7} continue \u{b7} esc \u{b7} back"
                    } else if ob.step() == opencrabs::tui::onboarding::OnboardingStep::BrainSetup {
                        "enter \u{b7} write my brain files \u{b7} esc \u{b7} back"
                    } else if ob.wizard.quick_jump {
                        "enter \u{b7} save \u{b7} esc \u{b7} close"
                    } else {
                        "enter \u{b7} continue \u{b7} esc \u{b7} back"
                    };
                    let horizontal_hints =
                        matches!(picker_layout.footer, model_layout::FooterLayout::Row);
                    let mut footer = div().flex().w_full().gap_2().min_w_0();
                    footer = match picker_layout.footer {
                        model_layout::FooterLayout::Row => {
                            footer.flex_row().items_center().justify_between()
                        }
                        model_layout::FooterLayout::Column => footer.flex_col().items_start(),
                    };
                    let footer = footer
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_muted))
                                .min_w_0()
                                .whitespace_normal()
                                .when(horizontal_hints, |d| d.flex_1())
                                .when(!horizontal_hints, |d| d.w_full())
                                .child(primary_hint),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_muted))
                                .min_w_0()
                                .whitespace_normal()
                                .when(horizontal_hints, |d| d.flex_none())
                                .when(!horizontal_hints, |d| d.w_full())
                                .child("tab \u{b7} next field \u{b7} arrows \u{b7} walk"),
                        );
                    let footer = if let Some(model_footer) = model_footer {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w_full()
                            .min_w_0()
                            .child(model_footer)
                            .child(footer)
                    } else {
                        footer
                    };
                    (
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(rail)
                            .child(
                                div()
                                    .id("onboard-body")
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(column),
                            )
                            .into_any_element(),
                        footer.into_any_element(),
                    )
                }
            },
            // The window-level modal owns this surface. Keeping the old full
            // screen here as well duplicated its controls and element IDs.
            Screen::Models => (div().flex_1().into_any_element(), div().into_any_element()),
            Screen::Skills => {
                // The same list `/skills` opens in the terminal, from the same
                // loader. A skill is run by its row: Enter or a click submits
                // the prompt the slash would have submitted (#2466).
                let shown = palette::current();
                let last = self.skills_rows.len().saturating_sub(1);
                let current = self.skills_index.min(last);
                let mut list = div().flex().flex_col().gap_1();
                for (index, skill) in self.skills_rows.iter().enumerate() {
                    let selected = index == current;
                    let name = skill.slash_name.clone();
                    let description = skill.description.clone();
                    let tag = skills_view::source_label(skill);
                    let mut head = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(text(name, Size::Sm, shown.text))
                        .child(text(tag, Size::Xs, shown.text_muted));
                    if skill.review_gate {
                        // The gate is visible before it runs: a skill that will
                        // stop for approval says so on its row.
                        head = head.child(text("review gate", Size::Xs, shown.amber));
                    }
                    let mut entry = div()
                        .id(SharedString::from(format!("skill-row-{index}")))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded(px(6.0))
                        .child(head)
                        .child(text(description, Size::Xs, shown.text_muted));
                    if selected {
                        entry = entry.bg(rgb(shown.panel_hi));
                    }
                    list =
                        list.child(entry.on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.skills_index = index;
                            this.run_skill(index, cx);
                        })));
                }
                let scroller = div()
                    .id("skills-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list);
                let count = self.skills_rows.len();
                // The typed filter is shown, so a short list never looks like a
                // loader failure: the row says what is being narrowed by.
                let counted = if self.skills_query.is_empty() {
                    format!("{count} skill{}", if count == 1 { "" } else { "s" })
                } else {
                    format!(
                        "{count} of {} skills \u{b7} filter: {}",
                        self.skills_all.len(),
                        self.skills_query
                    )
                };
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(shown.text_muted))
                            .child(SharedString::from(counted)),
                    )
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        "enter \u{b7} run \u{b7} type \u{b7} filter \u{b7} esc \u{b7} back",
                    ));
                (scroller.into_any_element(), footer.into_any_element())
            }
        };

        // The input box and the live turn panel belong to the chat screen.
        // An editor parked under the usage ledger is the mixing the owner
        // called out, so usage draws the footer hint and nothing else.
        // The bar sits above the transcript, at the top of the chat (#2394).
        let mut topic_row: Option<(topic_tabs::Edge, AnyElement)> = None;
        // Topics live in the chat: the `...` button sits above the
        // conversation for a session that has them on (#2303).
        let is_topic = self
            .chat_session
            .is_some_and(|id| self.sessions.parents.contains_key(&id));
        // A split draws the strip inside each pane (#2687): one window row
        // above a wall of panes named the chat's topics over conversations
        // that had none, and sat outside the box it described.
        let split_draws_its_own = self
            .panes
            .as_ref()
            .is_some_and(|tree| tree.leaves().len() > 1);
        if self.screen == Screen::Chat
            && !split_draws_its_own
            && (self.open_chat_topics_on() || is_topic)
        {
            // The tabs (#2400): the family's parent, then its topics. The
            // leading button moves the whole row to another side (#2402).
            // The `...` menu is NOT here any more (#2480): it lives at the
            // conversation's top right, over the chat it acts on.
            let (tabs, pill) = self.topic_tabs(None, cx);
            let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.cycle_topic_edge(cx);
            });
            let strip = topic_strip(
                "window",
                self.topic_edge,
                toggle_edge,
                tabs,
                self.chat_topics_expanded,
                pill,
            );
            topic_row = Some((self.topic_edge, strip.into_any_element()));
        }
        // The dock owns the chrome, so the field must not paint a second card
        // inside it. A split pane renders the same editor bare and keeps its own
        // card, which is why this is a flag on the field and not a deleted style
        // (#2987). It is recomputed on every frame rather than latched: the
        // window's composer IS the focused pane's editor once the chat splits
        // (`dashboard_panes.rs`), so a sticky flag would strip the box off the
        // pane the moment it took the chat over.
        self.composer.editor.update(cx, |editor, _cx| {
            editor.docked = self.screen == Screen::Chat && !self.window_is_split();
        });
        let mut right = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            // Less top and bottom room than the old 24px on every side: the
            // header lines up with the sessions title, and the input is not
            // floating in empty space above the window edge.
            .px_6()
            .pt_3()
            .pb_4();
        // The chat screen draws no title row (#2596). The conversation's
        // name already prints in the footer, so a second copy across the top
        // only cost a band of the window to repeat it. Every other screen
        // keeps the chrome it has always had.
        if self.screen != Screen::Chat {
            right = right.child(chrome);
        }
        // The row takes the side it was saved on: above the transcript,
        // under the input, or beside the chat (#2402).
        let (mut topic_top, mut topic_bottom, mut topic_side) = (None, None, None);
        if let Some((edge, strip)) = topic_row {
            match edge {
                topic_tabs::Edge::Top => topic_top = Some(strip),
                topic_tabs::Edge::Bottom => topic_bottom = Some(strip),
                topic_tabs::Edge::Left | topic_tabs::Edge::Right => {
                    topic_side = Some((edge, strip))
                }
            }
        }
        if let Some(bar) = topic_top {
            // The topic bar is a row of the chat's own stack, so it takes the
            // same measure as the text under it (#2590). It is drawn above the
            // screen match, which is why the wrapper is a named function and
            // not a closure: the same two values have to reach it here.
            right = right.child(central(bar, centred_chat, chat_measure_px));
        }
        right = right.child(middle);
        right = match self.screen {
            Screen::Chat => {
                // The running checklist sits between the conversation and the
                // input, the same place the TUI's plan card holds. The key
                // hints go back UNDER the input where they were: the owner
                // called the move above the plan card out.
                let mut col = right;
                // The strips that ride between the conversation and the input
                // share one box that is allowed to shrink and clip (#2980).
                // Each of them is `flex_none` on its own, so together they
                // could ask for more height than the window has, and the stack
                // was painted past the bottom edge, under the composer and off
                // the screen. Inside this box they give up the room instead,
                // and the box cuts what does not fit. The composer and the key
                // hints stay outside it: neither may ever be clipped.
                let mut above = div()
                    .flex()
                    .flex_col()
                    .flex_shrink()
                    .min_h_0()
                    .overflow_hidden()
                    .gap_3();
                let mut above_used = false;
                // In a split every pane carries its own plan, queue and
                // composer (#2530), so the window-level copies stand down:
                // drawing both would give the chat two composers. An unsplit
                // window keeps exactly the layout it has always had, measure
                // wrapper included (#2594).
                if !self.window_is_split() {
                    let open = self.chat_session;
                    let actions = open.map(|session| self.plan_actions(session, cx));
                    self.plan.refresh();
                    let card = self
                        .plan
                        .editing_view(actions.as_ref())
                        .or_else(|| self.plan.view());
                    if let Some(plan_view) = card {
                        above = above.child(central(
                            plan_view.into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                        above_used = true;
                    }
                    let waiting = self.queued_for_strip();
                    if !waiting.is_empty() {
                        above = above.child(central(
                            queue_strip(&waiting, self.composer.queue_expanded)
                                .id("queue-strip-chat")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut Self, _, _, cx| {
                                        if this.queued_for_strip().len() > 2 {
                                            this.composer.queue_expanded =
                                                !this.composer.queue_expanded;
                                            cx.notify();
                                        }
                                    }),
                                )
                                .into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                        above_used = true;
                    }
                    // The follow-up offer sits where the queue does, directly
                    // above the composer it sends into (#2708).
                    if let Some(sid) = self.chat_session {
                        let offers = agent_bridge::options_snapshot(sid);
                        if !offers.is_empty() {
                            let rows = self.option_rows(sid, offers, cx);
                            above = above.child(central(
                                options_strip(&rows).into_any_element(),
                                centred_chat,
                                chat_measure_px,
                            ));
                            above_used = true;
                        }
                    }
                    // A detached command sits directly above the editor: the same
                    // place the TUI puts it (on the input box's top border), which
                    // is where the eye goes while waiting.
                    if let Some(label) = self.running_tasks_label()
                        && let Some(sid) = self.chat_session
                        && let Some(area) = self.task_area(sid, &label, cx)
                    {
                        above = above.child(central(area, centred_chat, chat_measure_px));
                        above_used = true;
                    }
                    // The strips take their room from the conversation before
                    // the composer does: the box goes in above the input, and
                    // an empty box adds no row at all.
                    if above_used {
                        col = col.child(above);
                    }
                    // The unsplit chat uses the same composer swap as a split
                    // leaf (#2924). Without this arm the forward state changed
                    // but its destination picker was never rendered here.
                    // The bottom topic row sits immediately ABOVE the dock,
                    // in Telegram's forum arrangement. It is never between
                    // the composer and the footer, because the footer now
                    // lives inside the same card as the composer.
                    if let Some(strip) = topic_bottom {
                        col = col.child(central(strip, centred_chat, chat_measure_px));
                    }
                    let composer_replacement =
                        if self.forward_pick.is_some() && self.forward_leaf.is_none() {
                            Some(self.forward_picker(cx))
                        } else if self.msg_select.is_active() {
                            Some(self.select_bar(cx))
                        } else {
                            None
                        };
                    let composer: AnyElement = if let Some(view) = composer_replacement {
                        // The forward picker and the select bar stand in for the
                        // input, and they keep the dock so the key hints stay put
                        // instead of jumping to a separate row (#2924).
                        view
                    } else {
                        // The slash-command list sits over the input while a name is
                        // not being spelled, the terminal's own placement. The name
                        // prompt itself is the overlay below (#2398). The list floats
                        // over the composer, so it lines up with it: same measure,
                        // still the child directly above the editor, so the order it
                        // paints in does not change (#2590).
                        if self.topic_draft.is_none() {
                            let popup = self.composer.editor.read(cx).command_popup();
                            if let Some((names, selected)) = popup {
                                col = col.child(central(
                                    command_popup_view(&names, selected).into_any_element(),
                                    centred_chat,
                                    chat_measure_px,
                                ));
                            } else if let Some((items, selected)) =
                                self.composer.editor.read(cx).emoji_popup()
                            {
                                col = col.child(central(
                                    emoji_popup_view(&items, selected).into_any_element(),
                                    centred_chat,
                                    chat_measure_px,
                                ));
                            }
                        }
                        self.composer.editor.clone().into_any_element()
                    };
                    // The dock owns the composer: surface, border, radius and
                    // the hairline above the key hints. The composer itself
                    // paints none of them any more, so a single session reads
                    // as one card instead of a floating pill parked above a
                    // separate footer row (#2987). The column has no gap: the only
                    // separation inside the card is the 1px hairline.
                    col = col.child(central(
                        div()
                            .id("composer-dock")
                            .flex()
                            .flex_col()
                            .gap_0()
                            .bg(glass_fill(palette::current().panel))
                            .border_1()
                            .border_color(rgb(palette::current().panel_hi))
                            .rounded(px(COMPOSER_RADIUS))
                            .overflow_hidden()
                            .child(composer)
                            .child(
                                div()
                                    .w_full()
                                    .h(px(1.))
                                    .bg(rgb(palette::current().panel_hi)),
                            )
                            .child(footer.into_any_element())
                            .into_any_element(),
                        centred_chat,
                        chat_measure_px,
                    ));
                }
                col
            }
            Screen::Usage => right.child(footer),
            Screen::Files => right.child(footer),
            Screen::Models => right.child(footer),
            Screen::Skills => right.child(footer),
            Screen::Projects => right.child(footer),
            Screen::DirectoryPicker => right.child(footer),
            Screen::Onboard => right.child(footer),
            Screen::MissionControl => right.child(footer),
        };

        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
            // Every child, including the transcript and every overlay, gets
            // the same opacity. The native window accepts alpha above (#2922).
            .opacity(gui_theme::window_opacity(self.window_transparency))
            .bg(rgb(palette::current().bg))
            .text_color(rgb(palette::current().text))
            // One family for the window (#2786): `font_family` sets the family
            // of this element and its children, so the composer, the
            // transcript and every bare label inherit the pick instead of each
            // one naming a face. The transcript's own `TextStyle`s name it
            // explicitly, because `TextStyle::default()` fills the field with
            // the platform font and would pin it.
            .font_family(fonts::current_ui())
            .track_focus(&self.focus)
            // Capture exits even when a focused editor would consume the key
            // before the dashboard's bubble handler sees it (#2964).
            .capture_key_down(cx.listener(|this, _ev: &KeyDownEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            .on_key_down(cx.listener(Self::on_key))
            // Capture sees the second middle-click before the marker's
            // full-window hitbox, and every other press exits too (#2964).
            .capture_any_mouse_down(cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            // A context menu closes when its row is left alone: any left
            // press in the window dismisses it, the rail rows do the same
            // in their own press listener first (#2354).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                    // A press anywhere is a press on the conversation unless a
                    // key put the rail in front (#2798). Arming from the
                    // pointer's x made every uncommitted press in the column
                    // take the keyboard, so the composer needed an Esc before
                    // it would type again.
                    this.rail_hands_back();
                    if this.sessions.rail_menu.take().is_some() {
                        cx.notify();
                    }
                    if this.topic_menu.take().is_some() {
                        cx.notify();
                    }
                    // A message card is pinned to a window point now, so it is
                    // not the transcript's to dismiss any more: every card
                    // closes on the same press, wherever it landed (#2690).
                    if this.msg_menu.take().is_some() {
                        this.msg_menu_row = None;
                        this.msg_menu_leaf = None;
                        cx.notify();
                    }
                    this.close_composer_menus(cx);
                }),
            )
            // While the marker owns the hit test, the transcript never sees
            // the wheel. The root still does, so the wheel exits cleanly.
            .on_scroll_wheel(cx.listener(|this, _ev: &ScrollWheelEvent, _window, cx| {
                this.cancel_middle_scroll(cx);
            }))
            // The drag lives on the root so it survives the pointer leaving
            // the handle: a 4px column cannot track a fast drag on its own.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                this.move_middle_scroll(ev.position, cx);
                if this.dragging_rail
                    && let Some((from_x, from_w)) = this.drag_from
                {
                    let dx = f32::from(ev.position.x) - from_x;
                    let next = (from_w + dx) / this.rem;
                    if next < RAIL_MIN_REM {
                        // Dragged past the floor: close it, the same gesture
                        // the owner asked for instead of leaving a sliver. The
                        // close goes through the one transition, so a hidden
                        // filter, an open chip panel and the keyboard focus do
                        // not outlive a rail that is no longer drawn (#2845).
                        // A drag and ctrl+s now close the same way.
                        this.set_rail_open(false, cx);
                        this.dragging_rail = false;
                        this.drag_from = None;
                    } else {
                        this.rail_width = next.min(RAIL_MAX_REM);
                    }
                    cx.notify();
                }
                // The divider drag, mapped onto the pane span recorded at paint
                // time. The floor lives in split::share_after_drag, so a fast
                // drag narrows a pane but never shuts it (#2476).
                if let Some(drag) = this.split_drag.clone() {
                    let area = this.split_areas.borrow().get(&drag.path).copied();
                    if let Some([x0, y0, x1, y1]) = area {
                        let span = if drag.stacked { y1 - y0 } else { x1 - x0 };
                        let now = if drag.stacked {
                            f32::from(ev.position.y)
                        } else {
                            f32::from(ev.position.x)
                        };
                        let travelled = now - drag.last;
                        if travelled != 0.0 {
                            if let Some(tree) = this.panes.as_mut() {
                                tree.drag_divider(&drag.path, drag.divider, travelled, span);
                            }
                            if let Some(active) = this.split_drag.as_mut() {
                                active.last = now;
                            }
                            cx.notify();
                        }
                    }
                }
                // A rail row slid left far enough: open its menu. The
                // threshold keeps a wobbling click from revealing anything.
                if let Some((id, from_x)) = this.rail_swipe
                    && sessions::slide_reveals(from_x, f32::from(ev.position.x))
                    && this.sessions.rail_menu != Some(id)
                {
                    this.sessions.rail_menu = Some(id);
                    this.rail_menu_at = Some(ev.position);
                    this.rail_slide_opened = Some(id);
                    cx.notify();
                }
            }))
            // Light the target while a Finder drag is over the window. The
            // moves are what carry the flag; the drop consumes it.
            .on_drag_move::<ExternalPaths>(cx.listener(
                |this, _ev: &DragMoveEvent<ExternalPaths>, _window, cx| {
                    this.drag_seen = Some(std::time::Instant::now());
                    cx.notify();
                },
            ))
            // The drop itself: every path that is a real file becomes a chip
            // in the input, through the same attach path the paperclip uses.
            .on_drop::<ExternalPaths>(cx.listener(|this, ev: &ExternalPaths, window, cx| {
                let paths = ev.paths().to_vec();
                this.drag_seen = None;
                // A dropped file belongs to the composer the keyboard owns,
                // which in a split is the focused pane's own field (#2610).
                let composer = this.composer_entity(cx);
                let added = composer.update(cx, |editor, cx| {
                    let added = editor.attach_paths(paths);
                    if added > 0 {
                        cx.notify();
                        // A drop is a request to type, so the caret goes
                        // to the input rather than leaving the window with
                        // no focused element.
                        editor.focus_handle.focus(window);
                    }
                    added
                });
                if added > 0 {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _window, _cx| {
                    this.dragging_rail = false;
                    this.drag_from = None;
                    this.rail_swipe = None;
                    // A pane drag ends here whether or not it landed anywhere
                    // (#2683): the band belongs to the drag, so it goes with it.
                    this.pane_drop = None;
                    // A divider dragged to a new size is a layout the user
                    // chose, so the release is when it gets written (#2675).
                    if this.split_drag.take().is_some() {
                        this.persist_layout();
                    }
                }),
            )
            // A release ends the slide even when the element under the pointer
            // swallows the bubble. A topic chip, the "+N" pill and the collapse
            // pill all stop propagation in their click listeners, so the
            // handler above never ran for them: the armed swipe outlived the
            // click and the next pointer move tripped `slide_reveals`, opening
            // the card's menu on a plain left click (#2911). Capture runs root
            // first, so no child can swallow this one.
            .capture_any_mouse_up(cx.listener(|this, _ev: &MouseUpEvent, _window, _cx| {
                this.rail_swipe = None;
            }));
        if self.rail_open {
            let width = self.rail_width;
            let grab = cx.listener(move |this, _ev: &MouseDownEvent, window, _cx| {
                this.dragging_rail = true;
                this.drag_from = Some((f32::from(window.mouse_position().x), width * this.rem));
            });
            root = root.child(rail).child(splitter(grab));
        } else if self.screen != Screen::Onboard {
            root = root.child(rail);
        }
        // The panes go in before every overlay. gpui paints children in the
        // order they are added, so a pane added after an overlay covers it:
        // the key reference then drew under the chat and under the rail.
        // A side strip rides the conversation's measure (#2948). The top and
        // bottom strips are wrapped in `central` (#2590), but a side strip is a
        // root sibling of the pane column, so it never got that wrapper: it sat
        // against the window edge while the text was centred at the measure,
        // and the whole centring slack opened between the two. The strip and
        // the column are centred as one block instead, so the topics sit beside
        // the conversation they belong to and the slack falls outside both.
        match topic_side {
            Some((edge, strip)) => {
                // The column pads itself with `px_6` on each side, so its own
                // width is the measure plus 3rem. Capping it there puts its
                // children back on the axis they always centred on.
                let pane_px = chat_measure_px + 3.0 * self.rem;
                let pane = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .when(centred_chat, |d| d.max_w(px(pane_px)))
                    .child(right);
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .justify_center();
                row = if edge == topic_tabs::Edge::Left {
                    row.child(strip).child(pane)
                } else {
                    row.child(pane).child(strip)
                };
                root = root.child(row);
            }
            None => root = root.child(right),
        }
        // One dialog for every delete path, painted above both panes. The
        // conversation is only named so the user knows what they are about
        // to lose; nothing is erased until the confirm button is clicked.
        if let Some(id) = self.confirm_delete {
            let title = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            let confirm = cx.listener(move |this, _ev, _window, cx| {
                this.confirm_delete = None;
                this.refocus_screen();
                this.delete_session(id, cx);
            });
            let cancel = cx.listener(|this, _ev, _window, cx| {
                this.confirm_delete = None;
                this.refocus_screen();
                cx.notify();
            });
            root = root.child(confirm_delete_overlay(title, confirm, cancel));
        }
        // Turning topics off folds the family into the conversation (#2863).
        // Same shape as the delete dialog: the menu arms it, the dialog's own
        // button is what writes, and the topic count is named so the fold is
        // never a surprise.
        if let Some(id) = self.confirm_disable_topics {
            let title = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            // Live topics only: `parents` already drops archived rows, so
            // this is the number the fold will actually move.
            let topics = self
                .sessions
                .parents
                .values()
                .filter(|parent| **parent == id)
                .count();
            let confirm = cx.listener(move |this, _ev, _window, cx| {
                this.confirm_disable_topics = None;
                this.refocus_screen();
                this.disable_topics(id, cx);
            });
            let cancel = cx.listener(|this, _ev, _window, cx| {
                this.confirm_disable_topics = None;
                this.refocus_screen();
                cx.notify();
            });
            root = root.child(confirm_disable_topics_overlay(
                title, topics, confirm, cancel,
            ));
        }
        // The Group topics picker: a multi select card painted above both
        // panes. Rows toggle on click, the keyboard cursor and Space mirror
        // the same selection, and only the group button writes anything.
        if self.group_target.is_some() {
            let parent_title = self
                .group_target
                .and_then(|id| self.sessions.items.iter().find(|s| s.id == id))
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            // The rows the field lets through (#2864). Built fresh each
            // render so a keystroke narrows the list without touching the
            // selection underneath it.
            let visible = self.group_visible();
            let mut list = div().id("group-pick-list").flex().flex_col().gap_0p5();
            for (idx, (sid, title, date)) in visible.iter().enumerate() {
                let sid = *sid;
                let checked = self.group_selected.contains(&sid);
                let cursored = idx == self.group_cursor;
                // A row that still carries topics is offered, and says how
                // many: grouping it folds them into it first, which is the
                // only way a family becomes a topic at depth one (#2864).
                let topic_count = match self
                    .sessions
                    .parents
                    .values()
                    .filter(|parent| **parent == sid)
                    .count()
                {
                    0 => None,
                    n => Some(n),
                };
                let toggle = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    this.group_cursor = idx;
                    if !this.group_selected.insert(sid) {
                        this.group_selected.remove(&sid);
                    }
                    // A new selection re-asks the fold question (#2864).
                    this.group_warned = false;
                    cx.notify();
                });
                let mark = div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(rems(1.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(rgb(palette::current().text_muted))
                    .when(checked, |d| {
                        d.bg(rgb(palette::current().accent))
                            .border_color(rgb(palette::current().accent))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette::current().panel))
                                    .child(SharedString::from("\u{2713}")),
                            )
                    });
                let row = div()
                    .id(SharedString::from(format!("group-pick-row-{sid}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .when(cursored, |d| d.bg(rgb(palette::current().selection)))
                    .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                    .on_click(toggle)
                    .child(mark)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(palette::current().text))
                            .truncate()
                            .child(SharedString::from(title.clone())),
                    )
                    .when_some(topic_count, |d, count| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette::current().text_muted))
                                .child(SharedString::from(if count == 1 {
                                    "1 topic".to_string()
                                } else {
                                    format!("{count} topics")
                                })),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(date.clone())),
                    );
                list = list.child(row);
            }
            // A blank list is never left unexplained (#2864): an empty field
            // with nothing to offer and a search that matched nothing read
            // the same before, and neither said which one it was.
            if visible.is_empty() {
                list = list.child(
                    div()
                        .id("group-pick-empty")
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(if self.group_filter.is_empty() {
                            "no session is available to group".to_string()
                        } else {
                            "no session matches that".to_string()
                        })),
                );
            }
            let scroller = div()
                .id("group-pick-body")
                .max_h(rems(20.0))
                .overflow_y_scroll()
                .child(list);
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.close_group_topics();
                cx.notify();
            });
            let confirm = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.submit_group_topics(cx);
            });
            let count = self.group_selected.len();
            let card = div()
                .id("group-pick")
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(28.0))
                .p_4()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(glass_fill(palette::current().panel))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from(format!(
                            "Group topics under {parent_title}"
                        ))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(
                            "Pick sessions to attach as topics. Type to filter, Space \
                             toggles, Enter groups.",
                        )),
                )
                // The field (#2864). It shows what was typed instead of
                // holding a caret: the dialog owns the keyboard, so the
                // characters land here from the key handler, not from a
                // focusable text input.
                .child(
                    div()
                        .id("group-pick-filter")
                        .px_2()
                        .py_1()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(palette::current().panel_hi))
                        .bg(rgb(palette::current().panel_hi))
                        .text_sm()
                        .text_color(if self.group_filter.is_empty() {
                            rgb(palette::current().text_faint)
                        } else {
                            rgb(palette::current().text)
                        })
                        .child(SharedString::from(if self.group_filter.is_empty() {
                            "type to filter...".to_string()
                        } else {
                            self.group_filter.clone()
                        })),
                )
                // A selected family keeps its own topics, and grouping it
                // folds them in first (#2864). The first attempt names that
                // instead of writing; Enter again goes ahead.
                .when(self.group_warned, |d| {
                    d.child(
                        div()
                            .id("group-pick-warning")
                            .text_xs()
                            .text_color(rgb(palette::current().amber))
                            .child(SharedString::from(
                                "The selected sessions have topics of their \
                                 own. Grouping folds those into them first, \
                                 and that cannot be undone. Enter again to go \
                                 ahead, or Esc to leave them as they are.",
                            )),
                    )
                })
                .when_some(self.group_error.clone(), |d, e| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().red))
                            .child(SharedString::from(e)),
                    )
                })
                .child(scroller)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("group-pick-cancel")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().text_normal))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .child(SharedString::from("cancel"))
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("group-pick-yes")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().accent))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(palette::current().accent))
                                .hover(|d| d.bg(rgb(palette::current().selection)))
                                .child(SharedString::from(if self.group_busy {
                                    "grouping...".to_string()
                                } else if self.group_warned {
                                    format!("fold and group ({count})")
                                } else {
                                    format!("group ({count})")
                                }))
                                .on_click(confirm),
                        ),
                );
            let scrim = div()
                .id("group-pick-scrim")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .bg(rgba(0x0a0a0acc))
                .child(card);
            root = root.child(scrim);
        }
        // A pick that named a topic says why no prompt opened (#2605). The
        // card is the delete dialog's shape with one answer, so a stopped
        // pick can never be mistaken for a progress notice.
        if let Some(title) = self.topic_warning.clone() {
            let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_warning = None;
                this.refocus_screen();
                cx.notify();
            });
            let body = format!(
                "\"{title}\" is a topic, not a conversation. A topic cannot hold topics, so pick the conversation it sits under, then name the new topic."
            );
            root = root.child(warning_overlay(&body, dismiss));
        }
        // A pick that named a conversation already open in a pane says so
        // instead of moving it (#2819). One card at a time, so the topic
        // warning keeps the slot when both are somehow armed.
        if self.topic_warning.is_none()
            && let Some(title) = self.pane_warning.clone()
        {
            let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pane_warning = None;
                this.refocus_screen();
                cx.notify();
            });
            let body = format!(
                "\"{title}\" is already open in another pane. Close that pane first, or pick a different conversation."
            );
            root = root.child(warning_overlay(&body, dismiss));
        }
        // A preview sits over both panes, and over everything else on the
        // window: it is where the user went to look at something, and the
        // transcript behind it must not take a click meant for it (#2395).
        if let Some(open) = self.preview.clone() {
            let close = cx.listener(|this, _ev, _window, cx| {
                // The other half of the pair: an open with no close after it
                // stayed on screen, an open and a close in the same gesture is
                // the overlay dismissing itself (#2395).
                logging::info("preview: closed");
                this.close_preview();
                this.refocus_screen();
                cx.notify();
            });
            let window_size = window.viewport_size();
            let viewport = (f32::from(window_size.width), f32::from(window_size.height));
            let document_height = (viewport.1 - 300.0).clamp(120.0, 620.0);
            let body = self.preview_body(&open, document_height, cx);
            let detail =
                preview::detail_line(&open, self.preview_zoom, self.preview_video.as_ref());
            let document_open = self.preview_document.is_some();
            root = root.child(preview::overlay(
                &open,
                detail,
                body,
                document_open,
                viewport,
                close,
            ));
        }
        // The key reference, above the panes and above the delete dialog: it
        // is read-only, so nothing behind it can be armed while it is up.
        if self.help_open {
            let view = HelpView {
                version: opencrabs::VERSION,
                update: self.update_available.clone(),
                filter: self.help_filter.clone(),
                scroll: self.help_scroll.clone(),
                narrow: f32::from(window.viewport_size().width) < 760.0,
                commands: &self.help_commands,
            };
            root = root.child(help_screen(&view));
        }
        // One task's full output, over everything in the chat: it is where
        // the reader went to look, so nothing behind it takes a click.
        if self.task_detail.is_some() {
            let size = window.viewport_size();
            let viewport = (f32::from(size.width), f32::from(size.height));
            if let Some(modal) = self.task_detail_overlay(viewport, cx) {
                root = root.child(modal);
            }
        }
        // The topic info panel sits above the panes, like the key reference (#2304).
        if let Some(info) = self.topic_info.clone() {
            let close = cx.listener(|this, _ev, _window, cx| {
                this.topic_info = None;
                cx.notify();
            });
            root = root.child(topic_info_overlay(&info, close));
        }
        // While a split waits for its session the chat dims, and a card says what
        // to do. The sidebar is left bright so its rows can be picked (#2438).
        if let Some(layout) = self.split_pick {
            let rail_px = px(self.visible_rail_width() * self.rem);
            root = root.child(split_pick_overlay(layout, rail_px));
        }
        // The name prompt takes the whole window: the scrim darkens everything
        // else, and a click outside the panel cancels, as Esc does (#2398).
        if let Some(draft) = self.topic_draft.clone() {
            // The heading follows what the prompt will rename: a child session
            // is a topic, a root one is a conversation. Reading it from the
            // pending rename alone labelled every conversation rename as a
            // topic edit (#2668).
            let label = match self.topic_rename {
                Some(id) if self.sessions.parents.contains_key(&id) => "Rename topic",
                Some(_) => "Rename conversation",
                None => "New topic",
            };
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_draft = None;
                this.topic_rename = None;
                cx.notify();
            });
            let icon = self
                .topic_draft_icon
                .clone()
                .unwrap_or_else(|| DEFAULT_TOPIC_ICON.to_string());
            // The pack ships in two families, so the prompt lists them one
            // after another instead of one 98-cell wall (#2824, #2897). A pick
            // is a picture, not a bullet (#2768): at the old 1.5rem the badges
            // were specks and picking one was guesswork. They render at 4.0rem
            // (#2789), and the 256x256 sources stay crisp at 2x.
            let mut families: Vec<(&'static str, Vec<Stateful<Div>>)> = topic_icons::TOPIC_FAMILIES
                .iter()
                .map(|&(caption, prefix)| {
                    let cells: Vec<Stateful<Div>> = topic_icons::family_icons(prefix)
                        .map(|(name, bytes)| {
                            let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                                this.topic_draft_icon = Some(topic_icons::store_name(name));
                                cx.notify();
                            });
                            div()
                                .id(SharedString::from(format!("topic-icon-{name}")))
                                .p_1()
                                .rounded(px(6.0))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .on_click(pick)
                                .child(topic_icons::picture(bytes, 4.0))
                        })
                        .collect();
                    (caption, cells)
                })
                .collect();
            // One extra cell opens the platform file picker for a custom
            // image, Telegram style (#2790). The tile matches the embedded
            // picks' geometry so the grid keeps its rhythm, and it keeps its
            // own caption so the + never trails a 50-icon family unseen
            // (#2824).
            let add_image = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pick_icon_image(cx);
            });
            families.push((
                "Your image",
                vec![
                    div()
                        .id("topic-icon-add-image")
                        .p_1()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(add_image)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(rems(4.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(rgb(palette::current().panel_hi))
                                .text_color(rgb(palette::current().text_muted))
                                .text_size(rems(1.6))
                                .child("+"),
                        ),
                ],
            ));
            // The badge previews a custom pick from the same cache the rail
            // and tabs draw from (#2790).
            let custom_picture = self.custom_icon_picture(&icon, 3.2);
            // The session image itself opens the picker, the same handler
            // the add-image cell uses (#2790).
            let pick_image = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pick_icon_image(cx);
            });
            root = root.child(topic_prompt_overlay(
                label,
                &draft,
                &icon,
                families,
                custom_picture,
                pick_image,
                cancel,
            ));
        }
        // The update dialog (#2406): two stages, and /evolve runs only after the second yes.
        if let Some(second) = self.update_prompt {
            let version = self.update_available.clone().unwrap_or_default();
            let confirm = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                if second {
                    this.update_prompt = None;
                    this.run_command(
                        commands::Outcome::Run(commands::GuiAction::Evolve),
                        this.chat_session,
                        cx,
                    );
                } else {
                    this.update_prompt = Some(true);
                }
                cx.notify();
            });
            let cancel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.update_prompt = None;
                cx.notify();
            });
            root = root.child(update_prompt_overlay(&version, second, confirm, cancel));
        }
        // An open Mission Control record sits above the screen it belongs to, the
        // way the preview does: Esc or a click on the scrim closes it (#2426).
        if self.screen == Screen::MissionControl && self.mc_nav.detail {
            let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.mc_nav.detail = false;
                cx.notify();
            });
            root = root.child(mission_control_detail(
                self.mc.as_ref(),
                &self.mc_nav,
                close,
            ));
        }
        // The log reader covers the screen while it is open, like the
        // terminal's, so no key lands on a panel nobody can see.
        if self.screen == Screen::MissionControl && self.mc_logs.is_some() {
            let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.mc_logs = None;
                cx.notify();
            });
            if let Some(viewer) = self.mc_logs.as_mut() {
                root = root.child(mission_control_logs(viewer, close));
            }
        }
        // The drop target: a Finder drag tints the whole window so the target
        // is never guessed. It paints above both panes and vanishes the
        // moment the drag ends.
        if self.file_drag_active() {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded(px(CARD_RADIUS))
                    .border_2()
                    .border_color(rgb(palette::current().accent))
                    .bg(rgba(0x101010dd))
                    .child(div().text_xl().child("📎"))
                    .child(div().text_lg().child("Drop to attach"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child("files and images ride with your next message"),
                    ),
            );
        }
        // The Seeker card paints above every other surface, because it is
        // the one thing that takes every key while it is up (#2458). One
        // handler per printed row, built here where the context lives.
        if let Some(state) = self.finder.as_ref() {
            let (start, printed) = command_palette::window_range(state);
            let handlers: Vec<command_palette::PickHandler> = (0..printed)
                .map(|ix| -> command_palette::PickHandler {
                    Box::new(
                        cx.listener(move |this: &mut Self, _ev: &ClickEvent, _window, cx| {
                            this.pick_finder_row(start + ix, cx);
                        }),
                    )
                })
                .collect();
            let entity = cx.entity().downgrade();
            let chip: command_palette::ChipHandler =
                Box::new(move |want: Option<PaletteKind>, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| this.set_finder_kind_filter(want, cx));
                });
            let wheel = cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let lines = match ev.delta {
                    ScrollDelta::Lines(lines) => lines.y,
                    // A pixel delta is a trackpad: a hundredth of a notch
                    // per pixel keeps a flick from jumping whole pages.
                    ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / 100.0,
                };
                let rows = if lines > 0.0 {
                    lines.ceil() as i32
                } else if lines < 0.0 {
                    lines.floor() as i32
                } else {
                    0
                };
                this.finder_scroll(rows, cx);
            });
            let idle_revealed = state.idle_ready(std::time::Instant::now());
            root = root.child(command_palette::overlay(
                command_palette::FinderOverlayState {
                    state,
                    idle_revealed,
                    caret_on: self.finder_caret,
                    view: cx.entity().downgrade(),
                    paint: self.finder_paint.clone(),
                    voice: self.seeker_voice_view(cx),
                },
                handlers,
                chip,
                wheel,
                find::channel_hint(state.query(), &self.finder_channels),
                self.seeker_voice_error.clone(),
            ));
        }
        // The window controls are the frame's, not a screen's (#2813): drawn
        // over the top-right corner so no screen's layout owns them, and before
        // the menus and modals below so a scrim that owns the pointer also owns
        // the buttons. Windows has no caption to inherit, so the same cluster
        // is drawn there and the platform runs its clicks (#2966).
        let decorations =
            effective_decorations(window.window_decorations(), PLATFORM_DRAWS_CAPTION);
        #[cfg(target_os = "windows")]
        let on_control: Option<window_controls::Handler> = None;
        #[cfg(not(target_os = "windows"))]
        let on_control: Option<window_controls::Handler> =
            Some(Rc::new(|control, _ev, window, cx| match control {
                window_controls::Control::Minimize => window.minimize_window(),
                window_controls::Control::Maximize => window.zoom_window(),
                window_controls::Control::Close => cx.quit(),
            }));
        if let Some(cluster) = window_controls::cluster(decorations, on_control) {
            root = root.child(cluster);
        }
        // Both context menus live here, at the window root, never inside the
        // row or column that asked for them (#2690). Their cards are placed at
        // the press point in window coordinates, and a parent's own frame would
        // translate those coordinates to its corner. Deferred, so they paint
        // over the cards and the conversation they open above.
        if let Some(menu) = self.chat_menu(cx) {
            root = root.child(gpui::deferred(menu).with_priority(2));
        }
        if let Some(id) = self.sessions.rail_menu
            && let Some(menu) = self.rail_menu_popover_for(id, cx)
        {
            root = root.child(gpui::deferred(menu).with_priority(2));
        }
        // The message menu is a window-level card too (#2690): it used to be a
        // child of its row, so a long message pushed it far from the press and
        // a split pane's frame moved it again.
        if self.msg_menu.is_some()
            && let Some(at) = self.msg_menu_at
        {
            let items = self.msg_menu_items(cx);
            let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.msg_menu_close(cx);
            });
            root = root.child(
                gpui::deferred(transcript::message_menu_popover(&items, at, dismiss))
                    .with_priority(2),
            );
        }
        // The crab pack is a window-level card for the same reason (#2825): it
        // opens at the point the row's menu did, and a row's own frame would
        // move that point.
        if self.react_menu.is_some()
            && let Some(at) = self.react_menu_at
        {
            let cells = self.react_cells(cx);
            let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                this.msg_menu_close(cx);
            });
            root = root.child(
                gpui::deferred(transcript::react_picker_popover(&cells, at, dismiss))
                    .with_priority(2),
            );
        }
        // These window-level cards share Theme's scrim and pointer ownership.
        // Their screen state remains the compatibility keyboard owner.
        if self.screen == Screen::Models {
            root = root.child(self.models_modal(cx));
        }
        if self.screen == Screen::Projects {
            root = root.child(self.projects_modal(cx));
        }
        // The theme modal is attached last, so it paints over every other
        // surface: while it is open it is what the user is looking at, and the
        // scrim is what the pointer meets (#2619).
        if self.themes_open {
            root = root.child(self.themes_modal(cx));
        }
        // The settings modal follows the same rule (#2878).
        if self.settings_open {
            root = root.child(self.settings_modal(cx));
        }
        // The auto-scroll marker is the final surface while active, and it
        // owns the pointer: gpui gates every mouse listener on the element
        // being hovered, and this overlay blocks the hit test for everything
        // beneath it, so a listener left on the root never runs while the
        // marker is up (#2964). The gesture's three listeners ride the one
        // element the pointer is over, and stopping propagation in the
        // capture phase keeps the press from reaching the rows underneath.
        if let Some(gesture) = self.middle_scroll.as_ref() {
            root = root.child(
                middle_scroll::overlay(gesture)
                    .capture_any_mouse_down(cx.listener(
                        |this, _ev: &MouseDownEvent, _window, cx| {
                            this.cancel_middle_scroll(cx);
                            cx.stop_propagation();
                        },
                    ))
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                        this.move_middle_scroll(ev.position, cx);
                    }))
                    .on_scroll_wheel(cx.listener(|this, _ev: &ScrollWheelEvent, _window, cx| {
                        this.cancel_middle_scroll(cx);
                        cx.stop_propagation();
                    })),
            );
        }
        // A stationary pointer still scrolls. Entering the dead zone lets the
        // one outstanding animation callback drain out.
        self.schedule_middle_scroll_frame(window, cx);
        root
    }
}

/// The drag handle between the rail and the chat pane. The hit target stays
/// wide, but only its centered child paints a hairline.
fn splitter(
    grab: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    div()
        .id("rail-splitter")
        .w(px(4.0))
        .h_full()
        .flex_none()
        .flex()
        .justify_center()
        .cursor_col_resize()
        .child(
            div()
                .w(px(1.0))
                .h_full()
                .bg(topic_tabs::separator_color())
                .hover(|d| d.bg(rgb(palette::current().accent))),
        )
        .on_mouse_down(MouseButton::Left, grab)
}

fn main() {
    // Before the log, the ledger and every connector (#2841). A version check
    // must never become a second instance on the same bot token.
    match launch_intent(std::env::args().skip(1)) {
        Launch::Version => {
            println!("opencrabs-gui {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Launch::Help => {
            print_launch_help();
            return;
        }
        Launch::Refused(flag) => {
            eprintln!("opencrabs-gui: unknown option '{flag}'");
            eprintln!("Try 'opencrabs-gui --help' for the flags this binary answers.");
            std::process::exit(2);
        }
        Launch::Window => {}
    }

    // The terminal binary installs the rustls provider before anything opens a
    // TLS connection (`src/main.rs`), and the window serves the same channels,
    // so it installs it too. Without it the Slack connector panics while it
    // tries to pick a provider from the crate features, and that task dies on
    // every boot while Telegram, which does not use rustls, keeps working
    // (#2452, #2607).
    #[cfg(feature = "slack")]
    let _ = rustls::crypto::ring::default_provider().install_default();

    // First, so a panic during the first fetch still lands in the trail.
    logging::init();

    // The window owns the channel layer (`services::surface` starts and stops
    // Telegram, WhatsApp, Slack and the rest), but this bin installed no
    // `tracing` subscriber, so every `tracing::` event in that layer was
    // dropped: a channel that never started, a token-lock refusal, and a
    // poller that died after boot all left no line in any file (#2933). The
    // terminal binary installs this at `src/main.rs`; the window needs the
    // same one because it is the process the channels actually run in.
    //
    // The daily file is gated by `agent.debug_logs`, seeded on boot below and
    // re-applied by `services::surface` on every config reload. While that
    // toggle is off the writer stays lazy and creates no file, so this costs
    // nothing until someone asks for the trail.
    let tracing_guard = opencrabs::logging::init_logging(
        opencrabs::logging::LogConfig::new().with_log_dir(opencrabs::logging::log_dir()),
    );
    if let Err(e) = &tracing_guard {
        logging::error(&format!("tracing subscriber not installed: {e}"));
    }
    // A banner click arrives as a protocol launch (#2751): the shell starts
    // this binary with the activation URI in argv, so the click is queued here,
    // before any window exists, and the poll tick opens it once there is one to
    // open it in. On Windows the scheme is registered first, so a click that
    // arrives before this point has a handler next time.
    #[cfg(target_os = "windows")]
    opencrabs::utils::desktop_notify::ensure_activation_handler();
    opencrabs::utils::desktop_notify::post_activation_from_args(std::env::args().skip(1));

    // The ledger path is resolved once, here, on the main thread: every
    // later fetch reuses it instead of re-running `Config::load` on a
    // tiny-stack background thread.
    let db_path = ledger_path();
    // Agent bootstrap (task 5) starts before the window: provider, tool
    // registry, brain files and a fresh session build on a 4 MB thread and
    // report through the channel the Dashboard drains on its poll tick.
    let (agent_sink, agent_rx) = std::sync::mpsc::channel::<agent_bridge::TurnEvent>();
    match boot_config() {
        Ok(config) => {
            // Before the bootstrap takes the config: the keystroke trail is
            // switched on from config.toml, not only from the environment.
            logging::set_debug_from_config(config.debug.gui_debug_logs);
            // The tracing trail has its own switch (`agent.debug_logs`, the
            // same key the terminal honours). Seeded here so a window launched
            // with it on records channel events from the first boot instead of
            // waiting for the first config reload. This binary takes no
            // `--debug` flag, so nothing is forced.
            opencrabs::logging::apply_debug_logs(false, config.agent.debug_logs);
            agent_bridge::spawn_bootstrap(config, agent_sink.clone());
            agent_bridge::spawn_update_checks(agent_sink.clone());
        }
        Err(e) => {
            let _ = agent_sink.send(agent_bridge::TurnEvent::BootstrapFailed(e));
        }
    }
    let state = futures::executor::block_on(load_view(db_path.clone(), Period::AllTime));
    let session_list = futures::executor::block_on(sessions::load_sessions(db_path.clone()));
    // The pinned set is a sidecar table, so it is its own read. A failure
    // leaves the rail unpinned rather than hiding the conversations.
    let favorites: std::collections::HashSet<uuid::Uuid> =
        futures::executor::block_on(sessions::load_favorites(db_path.clone()))
            .unwrap_or_else(|e| {
                logging::error(&format!("favorites load: {e}"));
                Vec::new()
            })
            .into_iter()
            .collect();
    // Topics are opt-in per session, read as their own set (#2302).
    let topics: std::collections::HashSet<uuid::Uuid> =
        futures::executor::block_on(sessions::load_topics(db_path.clone()))
            .unwrap_or_else(|e| {
                logging::error(&format!("topics load: {e}"));
                Vec::new()
            })
            .into_iter()
            .collect();
    // Closed topics are dimmed and refuse turns, so their set is read up front (#2305).
    let closed: std::collections::HashSet<uuid::Uuid> =
        futures::executor::block_on(sessions::load_closed(db_path.clone()))
            .unwrap_or_else(|e| {
                logging::error(&format!("closed load: {e}"));
                Vec::new()
            })
            .into_iter()
            .collect();
    // Which topic sits under which session, so the rail can group them (#2303).
    let parents: std::collections::HashMap<uuid::Uuid, uuid::Uuid> =
        futures::executor::block_on(sessions::load_topic_parents(db_path.clone()))
            .unwrap_or_else(|e| {
                logging::error(&format!("topic parents load: {e}"));
                Vec::new()
            })
            .into_iter()
            .collect();
    // Each topic's own icon, read with the rest of the topic state (#2480).
    // The map used to start empty and only fill after a close or a create, so
    // every tab drew its name alone until the owner touched a topic: the
    // "topic icons vanished" bug. The rows are already in the database; they
    // just were never read on the way in.
    let topic_icons: std::collections::HashMap<uuid::Uuid, String> =
        futures::executor::block_on(sessions::load_topic_icons(db_path.clone()))
            .unwrap_or_else(|e| {
                logging::error(&format!("topic icons load: {e}"));
                Vec::new()
            })
            .into_iter()
            .collect();
    let sessions_state = match session_list {
        Ok(items) => sessions::SessionsState {
            items,
            favorites: favorites.clone(),
            topics: topics.clone(),
            parents: parents.clone(),
            closed: closed.clone(),
            topic_icons: topic_icons.clone(),
            topic_titles: HashMap::new(),
            previews: HashMap::new(),
            selected: None,
            cursor: None,
            rail_menu: None,
            error: None,
            filter: String::new(),
            filter_active: false,
            rail_filter: sessions::RailFilter::default(),
            rail_filter_open: false,
            plan_progress: HashMap::new(),
            content_matches: HashMap::new(),
        },
        Err(e) => sessions::SessionsState {
            items: Vec::new(),
            favorites,
            topics,
            parents,
            closed,
            topic_icons,
            topic_titles: HashMap::new(),
            previews: HashMap::new(),
            selected: None,
            cursor: None,
            rail_menu: None,
            error: Some(e.into()),
            filter: String::new(),
            filter_active: false,
            rail_filter: sessions::RailFilter::default(),
            rail_filter_open: false,
            plan_progress: HashMap::new(),
            content_matches: HashMap::new(),
        },
    };
    // The split the last run closed with (#2675). Read here, on the main
    // thread, and put back once the conversation is open: the chat's leaf has
    // to exist before the tree can agree with it. Nothing is drawn from it
    // until then, so a stale or unreadable file costs one log line.
    let saved_layout = layout_store::load(&layout_store::path());
    // Chat is the default screen: the conversation the user was in opens on
    // launch, and a first run opens the newest. The usage ledger is something
    // the user asks for (ctrl+u); the two never share the pane, so a
    // conversation can never render as "usage" (#2444).
    // A saved layout names the boot conversation itself (#2689): last_session
    // drifts to whatever surface spoke last, so it is rarely one of the panes,
    // and letting it pick sent every restart back to the plain chat. The
    // terminal has always restored its tree this way; the window now agrees.
    let initial_session = saved_layout
        .as_ref()
        .and_then(|saved| {
            let known: std::collections::HashSet<Uuid> =
                sessions_state.items.iter().map(|item| item.id).collect();
            layout_store::boot_session(saved, &known)
        })
        .or_else(|| {
            pick_initial_session(
                &sessions_state.items,
                opencrabs::utils::last_session::read(),
            )
        });
    let initial_transparency =
        gui_theme::clamp_window_transparency(Config::current().gui.transparency);
    Application::new().run(move |cx: &mut App| {
        // Dock icon first: a bare binary has no bundle, so macOS would show
        // its generic "exec" icon for the whole session otherwise.
        icon::apply_dock_icon();
        editor::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(980.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Below this the single-column grid still fits every card's
                // 420px minimum, so nothing clips on load.
                window_min_size: Some(size(px(480.0), px(320.0))),
                titlebar: Some(TitlebarOptions {
                    title: Some("OpenCrabs".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                // The root paints every surface at the matching opacity. The
                // native window must accept alpha too, or the opaque backing
                // would hide the desktop behind it (#2922).
                window_background: if initial_transparency > 0 {
                    WindowBackgroundAppearance::Transparent
                } else {
                    WindowBackgroundAppearance::Opaque
                },
                // A Wayland compositor that does not implement
                // `zxdg_decoration_manager_v1` (mutter is one) never answers a
                // decoration request, so asking for server-side decorations
                // yields a window with no chrome at all: no title bar and no
                // minimise, maximise or close button (#2813). Client-side is
                // the only request that guarantees the controls exist, and the
                // window draws them itself in `window_controls`.
                #[cfg(target_os = "linux")]
                window_decorations: Some(WindowDecorations::Client),
                #[cfg(not(target_os = "linux"))]
                window_decorations: None,
                ..Default::default()
            },
            |window, cx| {
                let focus = cx.focus_handle();
                // The window's history, loaded once and shared with every
                // composer this window builds, split panes included.
                let history_path = composer::default_history_path();
                let history = composer::new_shared_history(composer::load_history(&history_path));
                let editor = cx.new({
                    let history = history.clone();
                    let history_path = history_path.clone();
                    move |cx| editor::Editor::new_with_history(history, history_path, cx)
                });
                // A drag-copied range lands on the clipboard inside the
                // transcript, which cannot see the composer. The dashboard owns
                // both halves, so it is the one that turns a landed copy into
                // the composer's own notice (#2587).
                let copy_drag = Rc::new(CopyDrag::default());
                {
                    let editor = editor.downgrade();
                    copy_drag.set_copied_listener(Rc::new(move |cx: &mut App| {
                        let _ = editor.update(cx, |editor, cx| editor.show_copied(cx));
                    }));
                }
                // The dashboard keeps its own handle on the same list.
                let dashboard_history = history.clone();
                let view = cx.new(move |cx: &mut Context<Dashboard>| {
                    // Enter on the editor is a send: routed into the agent
                    // turn path on the Dashboard itself.
                    cx.subscribe(&editor, |this, _editor, event, cx| match event {
                        editor::EditorEvent::Sent(text) => {
                            this.send_message(text.clone(), cx);
                        }
                        // A chip's thumbnail opens the same overlay a
                        // transcript tile opens: one preview, one code path.
                        editor::EditorEvent::Preview(path) => {
                            this.open_preview(path.clone(), cx);
                        }
                        // Down off the end of the field reaches the tasks
                        // running under it, when there are any.
                        editor::EditorEvent::Below => {
                            if let Some(session) = this.chat_session {
                                this.focus_tasks(session, cx);
                            }
                        }
                    })
                    .detach();
                    // The poll tick: drains bridge events onto the UI thread
                    // at a fixed cadence. Exits when the view dies.
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(agent_bridge::POLL_INTERVAL)
                                .await;
                            if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                    // The channel chip's slow probe (#2452): one lock-dir read
                    // every few seconds, all of it off the UI thread. Exits
                    // with the view like every other loop here. The names are
                    // read from the config ONCE, here on the UI thread; the
                    // worker never calls Config::load (SIGILL, 2026-11-03).
                    let chip_names: Vec<String> = {
                        let names: Vec<String> = configured_channels()
                            .iter()
                            .map(|c| c.name().to_string())
                            .collect();
                        logging::info(&format!(
                            "channel chip probe up: {} configured channel(s)",
                            names.len()
                        ));
                        names
                    };
                    let probe_names = chip_names.clone();
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(5))
                                .await;
                            // Each tick's future owns a copy: spawned work
                            // must be 'static and may outlive the tick.
                            let names = probe_names.clone();
                            let line = cx
                                .background_executor()
                                .spawn(async move { channel_chip_line_from(&names) })
                                .await;
                            if this
                                .update(cx, |this, cx| {
                                    if this.channel_chip != line {
                                        logging::info(&format!(
                                            "channel chip -> {}",
                                            line.as_ref().map(|s| s.as_ref()).unwrap_or("hidden")
                                        ));
                                        this.channel_chip = line;
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                    // The rail's ledger probe (#2881). A turn that arrives on a
                    // messaging channel writes to the ledger without producing
                    // any window event, and every existing `refresh_rail`
                    // caller was something only this window could cause. One
                    // aggregate per tick, with the full refetch paid only when
                    // that aggregate moved, keeps this clear of the per-frame
                    // ledger reads #2817 removed.
                    let rail_db = db_path.clone();
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(3))
                                .await;
                            let sig = sessions::rail_ledger_signature(rail_db.clone()).await;
                            if this
                                .update(cx, |this, cx| {
                                    // `None` means the read failed, not that the
                                    // ledger is empty: keep the last signature
                                    // and try again on the next tick.
                                    if sig.is_some() && sig != this.rail_ledger_sig {
                                        this.rail_ledger_sig = sig;
                                        this.refresh_rail(cx);
                                    }
                                    // An expanded chip strip is temporary
                                    // (#3020), and this probe is the slow
                                    // cadence the window already pays for, so
                                    // the fold rides it rather than a second
                                    // timer.
                                    this.rail_expire_chips(std::time::Instant::now(), cx);
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                    // The rail is an entity of its own (#2933). Its scroll
                    // handle is shared with the window, which still owns the
                    // keyboard walk, and it holds a weak handle back so a rail
                    // that outlives the window simply stops asking.
                    let rail_scroll = ScrollHandle::new();
                    let rail_owner = cx.entity().downgrade();
                    let rail_view = cx.new({
                        let rail_scroll = rail_scroll.clone();
                        move |_| rail_view::RailView::new(rail_owner, rail_scroll)
                    });
                    Dashboard {
                        screen: Screen::Chat,
                        last_screen: Screen::Chat,
                        state,
                        dash: DashboardState::default(),
                        focus: focus.clone(),
                        // The chat opens with the composer holding focus, so its
                        // caret shows and typing needs no click first (#2433).
                        refocus: Some(focus_target(keyboard_owner(&[], Screen::Chat))),
                        last_owner: None,
                        finder: None,
                        finder_channels: Vec::new(),
                        finder_caret: true,
                        finder_activity: std::time::Instant::now(),
                        finder_paint: cx.new(|_| command_palette::FinderPaint::default()),
                        finder_selecting: false,
                        seeker_capture: None,
                        seeker_transcription: None,
                        seeker_orb: None,
                        seeker_voice_levels: Vec::new(),
                        seeker_voice_finishing: false,
                        seeker_voice_send: false,
                        seeker_voice_looping: false,
                        seeker_voice_error: None,
                        chip_channel_names: chip_names,
                        channel_chip: channel_chip_line(),
                        daemon_starting: false,
                        daemon_error: None,
                        pending: 0,
                        files: Vec::new(),
                        files_for: None,
                        files_err: None,
                        mc: None,
                        mc_err: None,
                        mc_loading: false,
                        mc_window: mission_control::TimeWindow::Month,
                        mc_nav: mission_control::Nav::default(),
                        mc_logs: None,
                        mc_notice: None,
                        rem: BASE_REM,
                        db_path: db_path.clone(),
                        sessions: sessions_state,
                        rail: rail_activity::RailActivity::default(),
                        rail_index: rail_activity::PlanIndex::default(),
                        rail_seen: rail_activity::SeenPlans::load(Some(
                            opencrabs::config::opencrabs_home().join("rail_seen.json"),
                        )),
                        preview_refreshed_at: None,
                        previews_in_flight: std::rc::Rc::new(std::cell::Cell::new(0)),
                        previews_dirty: std::rc::Rc::new(std::cell::Cell::new(false)),
                        rail_find_gen: 0,
                        rail_ledger_sig: None,
                        seeker_find: HashMap::new(),
                        seeker_find_gen: 0,
                        seeker_scheduled_query: String::new(),
                        rail_chips_expanded: std::collections::HashMap::new(),
                        voice_turns: std::collections::HashSet::new(),
                        voice_paint: voice_play::PaintState::default(),
                        rail_width: RAIL_DEFAULT_REM,
                        rail_open: true,
                        rail_focused: false,
                        poll_tick: 0,
                        rail_anim_live: false,
                        chime_last: None,
                        frame_footers: HashMap::new(),
                        frame_tasks: HashMap::new(),
                        frame_tasks_at: None,
                        update_available: None,
                        update_prompt: None,
                        help_filter: None,
                        help_commands: CommandLists::empty(),
                        panes: None,
                        pane_states: HashMap::new(),
                        parked_panes: HashMap::new(),
                        pane_focus: None,
                        split_areas: Rc::new(std::cell::RefCell::new(HashMap::new())),
                        split_drag: None,
                        split_target: None,
                        pane_drop: None,
                        copy_drag,
                        split_pick: None,
                        split_creating: false,
                        msg_menu: None,
                        msg_menu_row: None,
                        msg_menu_leaf: None,
                        msg_menu_live: false,
                        edit_row: None,
                        msg_menu_at: None,
                        msg_menu_generation: 0,
                        copied_row: None,
                        copied_generation: 0,
                        react_menu: None,
                        react_menu_at: None,
                        msg_select: Selection::new(),
                        forward_pick: None,
                        forward_leaf: None,
                        forward_query: String::new(),
                        forward_cursor: None,
                        topic_draft_icon: None,
                        icon_images: HashMap::new(),
                        help_scroll: ScrollHandle::new(),
                        dragging_rail: false,
                        drag_from: None,
                        drag_seen: None,
                        rail_menu_at: None,
                        rail_live: HashMap::new(),
                        rail_swipe: None,
                        rail_slide_opened: None,
                        confirm_delete: None,
                        confirm_disable_topics: None,
                        group_target: None,
                        group_candidates: Vec::new(),
                        group_selected: std::collections::HashSet::new(),
                        group_cursor: 0,
                        group_error: None,
                        group_busy: false,
                        group_warned: false,
                        group_filter: String::new(),
                        preview: None,
                        preview_zoom: 1.0,
                        preview_video: None,
                        preview_document: None,
                        preview_document_generation: 0,
                        help_open: false,
                        topic_menu: None,
                        topic_menu_at: None,
                        topic_draft: None,
                        topic_warning: None,
                        pane_warning: None,
                        topic_edge: topic_tabs::Edge::from_config(
                            Config::load()
                                .ok()
                                .and_then(|c| c.gui.topic_tabs)
                                .as_deref(),
                        ),
                        chat_topics_expanded: false,
                        topic_rename: None,
                        topic_info: None,
                        themes_hue: 24.47,
                        themes_sat: 0.91,
                        themes_val: 0.84,
                        themes_open: false,
                        themes_card: 3,
                        window_transparency: initial_transparency,
                        window_background_transparent: initial_transparency > 0,
                        themes_drag: None,
                        themes_transparency_drag: false,
                        themes_wheel: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        themes_sat_track: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        themes_val_track: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        themes_transparency_track: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        fonts_open: false,
                        fonts_loaded: false,
                        settings_open: false,
                        settings_values: Vec::new(),
                        settings_creds: Vec::new(),
                        settings_cursor: 0,
                        settings_section: settings_view::SettingsSection::General,
                        settings_scroll: ScrollHandle::new(),
                        settings_status: None,
                        settings_secret: None,
                        cd_dir: std::env::current_dir().unwrap_or_default(),
                        cd_entries: Vec::new(),
                        cd_selected: 0,
                        cd_show_hidden: false,
                        cd_session: None,
                        models_rows: Vec::new(),
                        models_query: String::new(),
                        models_index: 0,
                        models_provider: None,
                        models_session: None,
                        models_switching: None,
                        models_fetch: None,
                        models_fetch_lifecycle:
                            opencrabs::ui_core::model_selector::FetchLifecycle::default(),
                        models_pending: Vec::new(),
                        models_fetching: false,
                        models_fetch_error: None,
                        skills_all: Vec::new(),
                        skills_rows: Vec::new(),
                        skills_query: String::new(),
                        skills_index: 0,
                        projects: projects::ProjectsState::default(),
                        onboard: None,
                        plan: plan_strip::PlanStripState::default(),
                        plan_for: None,
                        plan_expanded: std::collections::HashSet::new(),
                        tasks_open: std::collections::HashSet::new(),
                        task_focus: None,
                        task_open: HashMap::new(),
                        task_tails: HashMap::new(),
                        task_detail: None,
                        task_detail_scroll: ScrollHandle::new(),
                        task_detail_generation: 0,
                        announced_task_completions: std::collections::HashSet::new(),
                        plan_flash_until: HashMap::new(),
                        chat_state: chat_leaf::ChatState {
                            transcript: transcript::TranscriptState {
                                session: initial_session,
                                ..Default::default()
                            },
                            scroll: ScrollHandle::new(),
                            follow: Follow::new(),
                        },
                        chat_session: initial_session,
                        middle_scroll: None,
                        middle_scroll_frame_pending: false,
                        ctrl_c_pending: None,
                        // Registered by `main` once the window exists: it needs
                        // a handle on this view to judge the press (#2308).
                        ctrl_c_interceptor: None,
                        rail_scroll,
                        rail_view,
                        composer: chat_leaf::Composer::new(editor),
                        history: dashboard_history,
                        agent_rx,
                        config_reload: config_reload::spawn(),
                        sink: agent_sink,
                        agent_ready: false,
                        turn: chat_leaf::Turn::default(),
                        stashed: HashMap::new(),
                        ctx_max: 0,
                        metrics: chat_leaf::Metrics::default(),
                        footer_dir: None,
                    }
                });
                // Local transcript targets open in this window's preview;
                // external URLs keep the system handler. The weak backlink
                // avoids a dashboard/copy-state ownership cycle.
                {
                    let dashboard = view.downgrade();
                    view.update(cx, |this, _| {
                        this.copy_drag
                            .set_open_target_listener(Rc::new(move |target, cx| {
                                if dashboard
                                    .update(cx, |this, cx| this.open_transcript_target(target, cx))
                                    .is_err()
                                {
                                    logging::info("transcript link: window is gone");
                                }
                            }));
                    });
                }
                // Custom icon images are read once at startup, not per paint
                // (#2790): the stored values were loaded with the rest of the
                // topic state above.
                view.update(cx, |this, _| this.refresh_icon_images());
                // Ctrl+C has to reach the window even while the composer holds
                // the keyboard, and the editor binds the key to its own copy
                // action (#2587): a keymap match stops every listener behind
                // it, so the quit ladder is judged in an interceptor, which
                // runs before bindings resolve and can answer the press itself
                // (#2308). A composer selection keeps its copy and the press
                // falls through untouched.
                {
                    let weak = view.downgrade();
                    let kept = cx.intercept_keystrokes(
                        move |ev: &KeystrokeEvent, window: &mut Window, cx: &mut App| {
                            let chord = ev.keystroke.modifiers.control
                                && !ev.keystroke.modifiers.platform
                                && !ev.keystroke.modifiers.alt
                                && ev.keystroke.key == "c";
                            if !chord {
                                return;
                            }
                            let falls_to_copy = weak
                                .read_with(cx, |this, cx| {
                                    // The selection that keeps its copy lives in
                                    // the field the caret is on. A split draws a
                                    // real editor per leaf, so the window's
                                    // editor only answers for the chat leaf, and
                                    // asking it meant a selection in a pane never
                                    // copied and the quit ladder ran instead
                                    // (#2885).
                                    let composer = this.composer_entity(cx);
                                    let editor = composer.read(cx);
                                    this.screen == Screen::Chat
                                        && editor.focus_handle.is_focused(window)
                                        && editor.has_selection()
                                })
                                .unwrap_or(false);
                            if falls_to_copy {
                                return;
                            }
                            // Interceptors run ahead of the bindings and a
                            // stopped keystroke never reaches a listener, so
                            // the press is answered here instead of in the
                            // view's own handler (#2308).
                            cx.stop_propagation();
                            let _ = weak.update(cx, |this, cx| this.ctrl_c_press(cx));
                        },
                    );
                    view.update(cx, |this, _| this.ctrl_c_interceptor = Some(kept));
                }
                // The palette follows the system appearance from the first
                // frame, and again whenever the system switches (#2341).
                palette::follow_appearance(window.appearance());
                view.update(cx, |_this, cx| {
                    cx.observe_window_appearance(window, |_this, window, cx| {
                        palette::follow_appearance(window.appearance());
                        cx.notify();
                    })
                    .detach();
                });
                // The editor starts focused, like the TUI's input box: the
                // user opens the window and types straight into it.
                window.focus(&view.read(cx).composer.editor.read(cx).focus_handle);
                // Open the newest conversation before the first paint (chat
                // is the default). The fetch runs off the UI thread, like
                // every other DB read in this window.
                if let Some(id) = initial_session {
                    view.update(cx, |this, cx| this.open_session(id, cx));
                }
                // Then the panes the last run left behind, in the same slots
                // at the same sizes, with the caret on the same pane (#2675).
                // The TUI has always done this from its own file; the window
                // had no layout file at all until now.
                if saved_layout.is_some() {
                    view.update(cx, |this, cx| this.restore_layout(saved_layout, cx));
                }
                view
            },
        )
        .expect("failed to open the gpui window");
        // gpui registers its view class with the window, and that class has no
        // handler for the middle button's drag, so the gesture is installed
        // onto it here (#2964).
        middle_scroll::install_drag_bridge();
        // The window exists: only now may the GUI claim the profile and preempt
        // the running TUI/daemon. Claiming before open_window meant a window
        // that failed to open left the user with no surface at all (#2784).
        agent_bridge::window_is_up();
        cx.activate(true);
    });
}

/// Who asked for a send.
///
/// Window chrome owns the keyboard, so a composer that sends while an overlay
/// is up is answering keys nobody gave it (#2767). A turn the window minted
/// itself is the opposite case: the plan card's Approve tap and a command's
/// own turn come from a click, not from a field. They were refused all the
/// same, because the gate read the window's chrome and the chrome has no
/// session in it: a plan waiting on approval sat there with its tap landing
/// nowhere, while every other session's composer stayed dead behind the same
/// overlay until the window was restarted (#2795). The origin travels with the
/// send so the gate can tell the two apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SendOrigin {
    /// Typed into a composer: the overlay gate applies.
    Composer,
    /// Minted by the window for a named session: chrome cannot refuse it.
    Window,
}
