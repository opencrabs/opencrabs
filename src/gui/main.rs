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
mod breakdown;
mod chat_leaf;
mod command_palette;
mod commands;
mod composer_view;
mod dashboard_commands;
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
mod icon;
mod layout_store;
mod logging;
mod markdown;
mod menu;
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
mod pane_layout;
mod pane_view;
mod plan_strip;
mod platform_icons;
mod preview;
mod projects;
mod projects_screen;
mod projects_view;
mod rail_activity;
// The rail is an entity of its own (#2933): its rows, chips, badges and
// filter menu are built once per frame it is handed, and a frame that matches
// the last one replays the prepaint instead of rebuilding every listener.
mod rail_view;
mod screen_keys;
mod sessions;
mod settings_modal;
mod settings_view;
#[cfg(test)]
mod settings_view_test;
mod skills_view;
mod status;
mod telegram_topics;
mod text_commands;
mod theme;
mod themes_view;
mod thumbs;
mod topic_bar_view;
mod topic_icons;
mod topic_tabs;
mod transcript;
mod usage_view;
mod window_controls;

use crate::composer_view::{
    command_popup_view, composer_hint_for, emoji_popup_view, options_strip, queue_strip,
};
use crate::help_view::{HelpView, help_screen};
use crate::mission_control_view::{
    mission_control_body, mission_control_detail, mission_control_logs,
};
use crate::overlays::{
    DEFAULT_TOPIC_ICON, TopicPick, cd_entries_for, confirm_delete_overlay,
    confirm_disable_topics_overlay, split_pick_overlay, topic_info_overlay, topic_pick,
    topic_prompt_overlay, update_prompt_overlay, warning_overlay,
};
use crate::pane_chrome::{
    central, chat_measure, chat_measure_column, pane_composer_dim, rotation_chord, split_chord,
    split_pane_width, window_footer_yields,
};
use crate::pane_layout::{PanesCtx, pane_element};
use crate::pane_view::PaneView;
use crate::screen_keys::{
    ChordMods, FocusTarget, KeyboardOwner, OverlayId, ScreenAction, focus_target, keyboard_owner,
    screen_chord,
};
use crate::status::{git_branch, status_identity, tilde_path};
use crate::theme::{BASE_REM, Size, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP, text};
use crate::topic_bar_view::{overflow_chip, topic_label, topic_strip, topic_tab};
use opencrabs::db::models::Message;
use opencrabs::ui_core::actions;
use opencrabs::ui_core::channel_status;
use opencrabs::ui_core::composer;
use opencrabs::ui_core::message_select::{MessageAction, Selection};
use opencrabs::ui_core::model_layout;
use opencrabs::ui_core::model_selector::{FetchRequest, ProviderSelectorState};
use opencrabs::ui_core::pane_tree::{DropHalf, PaneTree};
use opencrabs::ui_core::rail;
use opencrabs::ui_core::scroll::{Follow, scrolled_from_top, wheel_releases_follow};
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
    AnyElement, AnyView, App, Application, Bounds, ClickEvent, ClipboardItem, Context, Div,
    DragMoveEvent, Entity, ExternalPaths, FocusHandle, Image, KeyDownEvent, KeystrokeEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
    ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Stateful, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowOptions, div, point,
    prelude::*, px, rems, rgb, rgba, size,
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

/// What the window has to draw: the loaded view, or the reason it could not.
enum LoadState {
    Loaded(Box<UsageViewModel>),
    Failed(SharedString),
}

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

/// The ledger path, unchanged: `config.database.path`, with the same
/// default (`~/.opencrabs/opencrabs.db`) when config cannot be read at all.
fn ledger_path() -> std::path::PathBuf {
    Config::load()
        .map(|c| c.database.path)
        .unwrap_or_else(|_| opencrabs::config::opencrabs_home().join("opencrabs.db"))
}

/// The period's display word for the usage chrome line: the owner asked for
/// one line reading `OpenCrabs Usage · All Time · v0.5.15`, the middle word
/// tracking the period switch (t/w/m/a).
fn period_word(p: Period) -> &'static str {
    match p {
        Period::Today => "Today",
        Period::Week => "Weekly",
        Period::Month => "Monthly",
        Period::AllTime => "All Time",
    }
}

/// The config the window boots with, provisioned on a home that has none
/// (#2375).
///
/// A window shipped without this step: it loaded whatever config happened to
/// exist, so a fresh install opened with no provider at all and the first
/// message died on "No provider configured", with no terminal to run the
/// wizard from. The order mirrors the CLI's own startup, and each line is
/// load-bearing:
///
/// 1. Write the documented `config.toml` and `keys.toml` when absent. The
///    built-in provider gets its section from that file, so provisioning
///    before this would have nowhere to write.
/// 2. Re-read the config. The file loaded before seeding carries no provider
///    sections, and `provision_builtin_provider` decides from `enabled`, so
///    the old value would have skipped the token request (#2151).
/// 3. Seed the brain templates, offline-safe and idempotent, so a first-run
///    window has a personality instead of an empty prompt (#1382).
/// 4. Ask the proxy for this install's token. The request needs a reactor,
///    and the main thread has the stack for the current-thread runtime
///    `Config::load` must not run on a gpui worker (see `load_view`).
///
/// Every step is idempotent and only fills a blank home, so a configured
/// install boots exactly as before.
fn boot_config() -> Result<Config, String> {
    if opencrabs::config::seed::ensure_config_seeded() {
        opencrabs::config::seed::ensure_keys_seeded();
        logging::info("first run: documented config and keys written");
    }
    let mut config = Config::load().map_err(|e| format!("config load: {e}"))?;
    opencrabs::config::profile::ensure_brain_seeded();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("first run: no runtime for provisioning: {e}"))?;
    if runtime.block_on(
        opencrabs::brain::provider::opencrabs_acquire::provision_builtin_provider(&mut config),
    ) {
        logging::info("first run: built-in provider token acquired");
    }
    Ok(config)
}

/// Fetch the real view on a thread with a real stack. The GUI is a separate
/// process with no pool of its own, so it uses the loader's by-path door,
/// which opens the ledger read-only and runs no migrations: the daemon and
/// the TUI stay the owners of the schema.
///
/// The db path arrives pre-resolved, read from the config once on the main
/// thread before the window opens. Nothing in here may touch `Config::load`:
/// gpui's background executor polls its async tasks on Apple dispatch-queue
/// worker threads whose stacks are tiny, and `Config::load`'s frame chain
/// (thread-locals, dyld TLV instantiation) blows that stack with a silent
/// SIGILL. Crash receipt 2026-10-30 08:33:39, exception "Thread stack size
/// exceeded", frames `ledger_path -> Config::load -> resolve_profile_home
/// -> _tlv_get_addr`. The tokio runtime and the sqlx work therefore run on
/// this dedicated 4 MB thread; the future only awaits the result channel.
async fn load_view(db_path: std::path::PathBuf, period: Period) -> LoadState {
    let (tx, rx) = oneshot::channel::<LoadState>();
    let spawned = std::thread::Builder::new()
        .name("usage-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => match rt.block_on(usage_view(&db_path, period)) {
                    Ok(vm) => LoadState::Loaded(Box::new(vm)),
                    Err(e) => LoadState::Failed(format!("{e:#}").into()),
                },
                Err(e) => LoadState::Failed(format!("tokio runtime failed: {e}").into()),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(state) => state,
            Err(_) => LoadState::Failed("fetch thread dropped its result".into()),
        },
        Err(e) => LoadState::Failed(format!("fetch thread failed to spawn: {e}").into()),
    }
}

/// Translate a gpui keystroke into the neutral vocabulary the decision logic
/// speaks. Key names here are the macOS platform layer's spellings
/// (`platform/mac/events.rs`): `escape`, `tab` (shift+tab arrives as `tab`
/// with the shift modifier), `enter`. A printable `key_char` becomes
/// `UiKey::Char` unless a command-ish modifier is held, and everything
/// unmodelled falls to `UiKey::Other`, which `decide` always ignores.
fn translate_key(ev: &KeyDownEvent) -> UiKey {
    let ks = &ev.keystroke;
    match ks.key.as_str() {
        "escape" => UiKey::Esc,
        "tab" if ks.modifiers.shift => UiKey::BackTab,
        "tab" => UiKey::Tab,
        "enter" => UiKey::Enter,
        _ => {
            if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
                UiKey::Other
            } else {
                ks.key_char
                    .as_deref()
                    .and_then(|s| s.chars().next())
                    .map(UiKey::Char)
                    .unwrap_or(UiKey::Other)
            }
        }
    }
}

/// One zoom step from the raw key name: `=` or `+` in, `-` or `_` out, `0`
/// back to the base rem, always clamped to the theme's range. The modifier
/// state is irrelevant, so both bare and cmd-chorded keys zoom. `None` for
/// any key that is not a zoom key.
fn zoom_step(key: &str, rem: f32) -> Option<f32> {
    let min = BASE_REM * ZOOM_MIN;
    let max = BASE_REM * ZOOM_MAX;
    match key {
        "=" | "+" => Some((rem * ZOOM_STEP).min(max)),
        "-" | "_" => Some((rem / ZOOM_STEP).max(min)),
        "0" => Some(BASE_REM),
        _ => None,
    }
}

/// The handler's rule: a zoom key acts only with cmd or ctrl held, so a bare
/// minus typed into the composer stays text. The reset rides the same chord.
fn zoom_step_for(key: &str, rem: f32, chord: bool) -> Option<f32> {
    if !chord {
        return None;
    }
    zoom_step(key, rem)
}

/// How long a finished turn's reload keeps retrying while its rows are still
/// being written.
///
/// The first version bounded this with `SETTLE_ATTEMPTS` (eight), which the
/// poll cadence turned into roughly half a second. A 100 KB assistant row
/// under WAL can take longer: the 2026-11-08 23:38:29 log shows retries 1 to
/// 8 ending in "transcript never caught up", after which the live card stayed
/// pinned beside the persisted turn until the session was reopened. A
/// deadline bounds the wait without assuming how long one write takes.
const SETTLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// One more read this long after the budget expired, so a write that landed
/// late still releases the live card instead of leaving it pinned.
const SETTLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// How many late re-reads one turn may spend after the budget expired. The
/// second covers a write that landed between the first recheck and its own
/// read; past that, the log line is the honest answer.
const SETTLE_RECHECKS: u8 = 2;

/// The rail's default width, and the floor it may be dragged to. At the
/// floor the splitter closes the rail instead of leaving a sliver: a 3-rem
/// column shows titles cut to nothing, which is worse than no column.
const RAIL_DEFAULT_REM: f32 = 17.0;
const RAIL_MIN_REM: f32 = 11.0;
const RAIL_MAX_REM: f32 = 32.0;

/// How long the Seeker caret keeps blinking after the last keystroke. Past
/// this the field goes quiet and the caret hides, the way Spotlight's does
/// once typing stops; the next key brings it back (#2531). The window and the
/// terminal share one rule, so this aliases the engine's constant.
const CARET_IDLE_AFTER: std::time::Duration = opencrabs::ui_core::palette::CARET_IDLE_AFTER;

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

/// Which messaging channels the config says are live: enabled and carrying the
/// credential that channel needs. Read once when Seeker opens, so the hint says
/// "not set up" only when that is true (#2462).
fn configured_channels() -> Vec<Channel> {
    let Ok(config) = Config::load() else {
        return Vec::new();
    };
    let channels = &config.channels;
    let filled = |value: &Option<String>| value.as_deref().is_some_and(|text| !text.is_empty());
    let mut live = Vec::new();
    if channels.telegram.enabled && filled(&channels.telegram.token) {
        live.push(Channel::Telegram);
    }
    if channels.discord.enabled && filled(&channels.discord.token) {
        live.push(Channel::Discord);
    }
    if channels.slack.enabled && filled(&channels.slack.token) && filled(&channels.slack.app_token)
    {
        live.push(Channel::Slack);
    }
    // WhatsApp pairs by QR: being enabled is the whole credential.
    if channels.whatsapp.enabled {
        live.push(Channel::WhatsApp);
    }
    if channels.trello.enabled
        && filled(&channels.trello.app_token)
        && filled(&channels.trello.token)
    {
        live.push(Channel::Trello);
    }
    live
}

/// Who owns the channels right now (#2452): a token lock on disk says
/// something claimed them, a live instance lock says that something is
/// still running. Read-only: the GUI never constructs a ChannelManager
/// and never starts one on its own.
fn channel_ownership() -> channel_status::Ownership {
    let locks = opencrabs::config::profile::base_opencrabs_dir().join("locks");
    // Token locks are files named `{channel}_{hash}.lock`; the instance,
    // paths and scheduler locks live in their own subdirectories, so the
    // suffix test on files only sees the channel claims.
    let lock_present = std::fs::read_dir(&locks)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                e.path().is_file() && name.ends_with(".lock") && name.contains('_')
            })
        })
        .unwrap_or(false);
    let profile = opencrabs::config::profile::active_profile().unwrap_or("default");
    let live = opencrabs::config::profile::instance_running_in(&locks.join("instance"), profile);
    channel_status::ownership(lock_present, live)
}

/// The chip line for the current config and lock state: `None` when every
/// configured channel is served or nothing is configured (#2452).
///
/// UI THREAD ONLY: `Config::load` builds the whole config by value and on
/// a GPUI background worker that build overflows the small stack (SIGILL,
/// 2026-11-03). Background callers use `channel_chip_line_from` with the
/// names read here once and passed in.
fn channel_chip_line() -> Option<SharedString> {
    let names: Vec<String> = configured_channels()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    channel_chip_line_from(&names)
}

/// The chip line from a fixed set of channel names: the probe path. Never
/// touches Config, so it is safe on a background worker; the lock and
/// instance checks are small directory reads (#2452).
fn channel_chip_line_from(names: &[String]) -> Option<SharedString> {
    if names.is_empty() {
        return None;
    }
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    channel_status::status_line(&borrowed, &channel_ownership()).map(SharedString::from)
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
    /// Parents whose topic chips are expanded past the first three. A chip
    /// strip shows three plus a "+N" pill by default; tapping it reveals the
    /// rest, and the collapse pill folds them back (Telegram topic style).
    rail_chips_expanded: std::collections::HashSet<Uuid>,
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
    /// The checkout's branch, read once at startup: the footer's own field in
    /// the TUI. `None` outside a git tree, which the footer shows as a dash
    /// rather than a made-up name.
    branch: Option<String>,
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

#[cfg(test)]
mod sidebar_action_tests {
    use super::*;
    use crate::dashboard_source::DASHBOARD;

    /// The body of one function in one of the GUI files, up to the next method.
    /// The source scans below pin decisions a unit test cannot reach without a
    /// window, and they read the body so a mention elsewhere cannot satisfy
    /// them (#2665).
    fn fn_body(src: &'static str, name: &str) -> &'static str {
        item_body(src, &format!("fn {name}("))
    }

    /// The Dashboard's window render. Two impls in this file define `render`,
    /// and only this signature is the one that holds the window (#2741).
    fn dashboard_render(src: &'static str) -> &'static str {
        // Scoped to the window's own `Render` impl first: the rail is a view of
        // its own now (#2933) and its render carries the same signature, so a
        // bare split would read the rail's body instead of the window's.
        item_body(
            item_body(src, "impl Render for Dashboard"),
            "fn render(&mut self, window: &mut Window, cx: &mut Context<Self>)",
        )
    }

    /// The body of one item, from its own definition to its closing brace.
    ///
    /// Two things this gets right that a plain `split` does not: only a real
    /// definition counts, because the scans below quote the names they look
    /// for inside their own strings, and the body ends at the brace that
    /// closes it rather than at the next method, so a top level function in
    /// another module file reads the same as a `Dashboard` method (#2741).
    fn item_body(src: &'static str, needle: &str) -> &'static str {
        let at = src
            .match_indices(needle)
            .map(|(at, _)| at)
            .find(|&at| {
                let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
                let prefix = src[line..at].trim();
                prefix.is_empty() || prefix.starts_with("pub")
            })
            .unwrap_or_else(|| panic!("{needle} not found"));
        let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
        let indent = src[line..at]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let close = format!("\n{}}}", " ".repeat(indent));
        let rest = &src[at..];
        rest.split(&close).next().unwrap_or(rest)
    }

    #[test]
    fn sidebar_actions_gui_open_and_close_are_idempotent() {
        for initially_open in [false, true] {
            let mut open = initially_open;
            let mut focused = initially_open;
            let mut sessions = sessions::SessionsState {
                filter: String::from("needle"),
                filter_active: initially_open,
                ..Default::default()
            };

            set_rail_open_state(true, &mut open, &mut focused, &mut sessions);
            set_rail_open_state(true, &mut open, &mut focused, &mut sessions);
            assert!(open);
            // Opening arms the walk (#2665): the arrows have to move the cards
            // without a Tab first, which is what opening the sidebar means.
            assert!(focused);
            assert_eq!(sessions.filter_active, initially_open);

            set_rail_open_state(false, &mut open, &mut focused, &mut sessions);
            set_rail_open_state(false, &mut open, &mut focused, &mut sessions);
            assert!(!open);
            assert!(!focused);
            assert!(sessions.filter.is_empty());
            assert!(!sessions.filter_active);
        }
    }

    #[test]
    fn opening_the_rail_arms_the_walk_on_the_open_conversation() {
        // #2665: opening the sidebar is asking to move with the arrows, so it
        // must not need a Tab first, and the walk starts on the conversation
        // on screen instead of nowhere.
        let mut open = false;
        let mut focused = false;
        let mut sessions = sessions::SessionsState::default();
        let on_screen = Uuid::new_v4();
        sessions.selected = Some(on_screen);

        set_rail_open_state(true, &mut open, &mut focused, &mut sessions);

        assert!(open, "the rail opens");
        assert!(
            focused,
            "opening arms the keys, the arrows need no Tab first"
        );
        assert_eq!(
            sessions.cursor,
            Some(on_screen),
            "the walk starts where the user is, not on an arbitrary row"
        );
    }

    #[test]
    fn a_committed_row_hands_the_keyboard_to_the_composer() {
        // #2665: a pick ends the walk. The rail stays on screen but stops
        // owning keys, and the focus request is made after that, because
        // `open_overlays` counts the rail only while it is focused.
        let src = DASHBOARD;
        let body = fn_body(src, "open_session_handing_back");
        let (_before, after) = body
            .split_once("if hand_back {")
            .unwrap_or_else(|| panic!("the hand back branch"));
        assert!(
            after.contains("self.rail_focused = false"),
            "a committed row takes the keys off the rail"
        );
        assert!(
            after.contains("self.refocus_screen()"),
            "the focus request follows, so the derivation sees an unarmed rail"
        );
        assert!(
            after.find("self.rail_focused = false") < after.find("self.refocus_screen()"),
            "the rail has to let go before focus is derived"
        );
        // Every commit goes through the wrapper, so the click, Enter, the
        // palette, the find field and a topic tab all hand the keys back.
        assert!(
            fn_body(src, "open_session").contains("open_session_handing_back(id, true, cx)"),
            "a commit is the form that hands the keyboard back"
        );
    }

    #[test]
    fn the_caret_receipt_names_who_took_the_keys() {
        // #2741: the reconciler can ask for the composer and still lose the
        // caret to the next press, and a silent `window.focus` cannot tell
        // those apart. The receipt records the state before the call and the
        // owner after it, so the next build answers it from the daily log.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        assert!(
            render.contains("caret -> composer before={before} now={}"),
            "the composer branch leaves a receipt"
        );
        assert!(
            render.contains("caret -> root before={before} now={}"),
            "the root branch leaves the mirror receipt"
        );
        assert!(
            render.contains("keyboard owner -> {owner:?}"),
            "an owner transition names the owner it moved to"
        );
        assert!(
            render.contains("self.caret_owner_name(window, cx)"),
            "the receipt names the element that holds the keys after the call"
        );
        let helper = fn_body(src, "caret_owner_name");
        for name in ["\"composer\"", "\"root\"", "\"field\"", "\"none\""] {
            assert!(helper.contains(name), "the helper names {name}");
        }
        assert!(
            helper.contains("window.focused(cx)"),
            "the name comes from the window's live focus, not a guess"
        );
    }

    #[test]
    fn the_arrow_walk_previews_without_taking_the_keys() {
        // #2665: the walk opens each row it stands on, so handing the composer
        // the caret there would eat the next arrow. #2819: the row lands in the
        // pane holding the caret, still without the hand back.
        let src = DASHBOARD;
        let keys = fn_body(src, "on_key");
        assert!(
            keys.contains("open_picked_in_focused_pane(id, false, cx)"),
            "the arrows preview the row and keep the rail armed"
        );
    }

    #[test]
    fn a_created_session_and_a_new_split_leave_the_caret_in_the_composer() {
        // #2665: every one of these was clicked from the rail, so each has to
        // take the keys off it before asking for focus again.
        let src = DASHBOARD;
        for name in ["new_session", "choose_split"] {
            let body = fn_body(src, name);
            let (_before, after) = body
                .split_once("self.rail_focused = false")
                .unwrap_or_else(|| panic!("{name} never lets the rail go"));
            assert!(
                after.contains("self.refocus_screen()"),
                "{name} asks for focus after the rail lets go"
            );
        }
    }

    #[test]
    fn a_press_on_the_sidebar_never_takes_the_keyboard() {
        // #2798: the window root armed the rail from the press's own x
        // position, so every press in the column that did not commit a pick
        // (the empty space under the rows, the scrollbar, the header, a
        // filter chip) left the composer without its caret until
        // Esc. A press is not a walk: only a key arms the rail.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        assert!(
            !render.contains("ev.position.x) <= this.rail_width"),
            "no pointer press decides who owns the keys"
        );
        // The window root's key handler is followed by the one press listener
        // it owns, and the drag handler comes after it: that slice is the
        // press, with nothing of the rows' listeners in it.
        let root_press = render
            .split(".on_key_down(cx.listener(Self::on_key))")
            .nth(1)
            .and_then(|rest| rest.split(".on_mouse_move(").next())
            .unwrap_or_else(|| panic!("the root still listens for a press"));
        assert!(
            root_press.contains("this.rail_hands_back();"),
            "a press anywhere hands the keys back to the composer"
        );
        assert!(
            !root_press.contains("rail_focused ="),
            "a press never decides who owns the keys"
        );
        assert!(
            root_press.contains("this.sessions.rail_menu.take()"),
            "the same press still dismisses the rail's menu"
        );
        let helper = fn_body(src, "rail_hands_back");
        assert!(
            helper.contains("self.rail_focused = false"),
            "the rail lets go of the keys"
        );
        assert!(
            helper.contains("self.refocus_screen()"),
            "the press asks for the caret once the rail lets go"
        );
        assert!(
            helper.find("self.rail_focused = false") < helper.find("self.refocus_screen()"),
            "the rail has to let go before focus is derived"
        );
        assert!(
            helper.contains("self.split_pick.is_some()"),
            "a split waiting for its pane keeps the keys its dialog answers"
        );
        // The walk still arms without a press, or the arrows and Tab would
        // have nothing left to open with.
        let open = fn_body(src, "set_rail_open_state");
        assert!(
            open.contains("*rail_focused = true"),
            "opening the rail still arms the walk"
        );
        // Leaving the find field hands the caret back, whichever way it exits.
        assert!(
            fn_body(src, "close_rail_filter").contains("self.refocus_screen()"),
            "closing the filter and the find field asks for the caret"
        );
    }

    #[test]
    fn a_rail_affordance_that_stops_the_press_hands_the_keys_back() {
        // #2798: these three stop the press before it reaches the window
        // root, so the root cannot hand the caret back for them. Each one
        // does it itself, or a press on a rail affordance leaves the composer
        // waiting for an Esc.
        let src = DASHBOARD;
        // The rail asks the window by name now (#2933): each affordance has a
        // method of its own, and the hand back lives in it rather than in the
        // closure that used to be minted inside the window's render.
        for (method, why) in [
            ("rail_row_menu", "a row's right press"),
            ("rail_toggle_pin", "a row's pin"),
            ("rail_chip_menu", "a topic chip's right press"),
        ] {
            let body = fn_body(src, method);
            assert!(
                body.contains("self.rail_hands_back();"),
                "{why} hands the keys back before it opens the card"
            );
        }
        // A row press is not a pick yet, and the caret does not wait for the
        // release to find out what it was.
        let press = fn_body(src, "rail_row_press");
        assert!(
            press.contains("self.rail_hands_back();"),
            "a press on a row hands the keys back whether or not it commits"
        );
    }

    #[test]
    fn an_attached_file_asks_for_the_caret_back() {
        // #2665: the platform picker holds the keyboard while it is open, so
        // attaching is the moment to ask for it back. Deferred to render
        // because only a frame holding a window can focus.
        let src = include_str!("editor.rs");
        let body = src
            .split("fn pick_paths(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("pick_paths"));
        assert!(
            body.contains("attach_paths(paths) > 0"),
            "only a real attach asks for it"
        );
        assert!(
            body.contains("editor.focus_pending = true"),
            "the caret goes back to the field the chips landed in"
        );
        // Cut at the impl's closing brace: `split("impl ")` also matches the
        // `-> impl IntoElement` return type and leaves a 76-byte stub.
        let render = src
            .split("impl Render for Editor")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("the render impl");
        assert!(
            render.contains("window.focus(&self.focus_handle)"),
            "the deferred request needs the window, so it lands in render"
        );
    }

    /// The rename dialog's icon picks render big enough to recognise
    /// (#2789): picker cells at 4.0rem, the chosen badge at 3.2rem inside a
    /// 4.0rem slot. The 512x512 sources stay crisp at 2x.
    #[test]
    fn rename_dialog_icons_render_big_and_crisp() {
        let src = DASHBOARD;
        // Scope to the window render: this test module sits above it in the
        // file, so a bare split would land in the test's own strings.
        let render = dashboard_render(src);
        let picker = render
            .split("topic-icon-{name}")
            .nth(1)
            .expect("picker cell id")
            .split(".collect()")
            .next()
            .expect("picker cell before collect");
        assert!(
            picker.contains("topic_icons::picture(bytes, 4.0)"),
            "a pick renders at 4.0rem, not a speck"
        );
        let overlay = fn_body(include_str!("overlays.rs"), "topic_prompt_overlay");
        assert!(
            overlay.contains("topic_icons::picture(bytes, 3.2)"),
            "the chosen badge picture renders at 3.2rem"
        );
        assert!(
            overlay.contains(".size(rems(4.0))"),
            "the badge slot grows to 4.0rem around it"
        );
    }

    /// A rename keeps the session's icon (#2791): the dialog seeds the draft
    /// from the stored value, and the submit path writes it back only when
    /// the helper says the draft changed it.
    #[test]
    fn rename_seeds_and_persists_the_icon() {
        let src = DASHBOARD;
        let start = fn_body(src, "start_rename");
        assert!(
            start.contains("self.topic_draft_icon = self.sessions.topic_icons.get(&id).cloned()"),
            "the rename dialog opens on the session's own icon"
        );
        let submit = fn_body(src, "submit_topic");
        assert!(
            submit.contains("sessions::icon_write_needed("),
            "the rename branch asks the helper before writing"
        );
        assert!(
            submit.contains("sessions::set_topic_icon(db_path, target, draft_icon)"),
            "the rename branch persists the changed icon"
        );
    }

    /// The session image is the upload trigger (#2790): the badge in the
    /// prompt carries a click that opens the file picker, and the add-image
    /// cell keeps its own. Rename and creation share the overlay, so both
    /// get the trigger.
    #[test]
    fn the_session_image_opens_the_picker() {
        let src = DASHBOARD;
        let overlay = fn_body(include_str!("overlays.rs"), "topic_prompt_overlay");
        assert!(
            overlay.contains(".id(\"topic-prompt-image\")"),
            "the badge is a click target, not a static picture"
        );
        assert!(
            overlay.contains(".on_click(pick_image)"),
            "clicking the session image opens the file picker"
        );
        assert!(
            overlay.contains(".cursor_pointer()"),
            "the badge shows it can be clicked"
        );
        let render = dashboard_render(src);
        let badge_pick = render
            .split("let pick_image = cx.listener")
            .nth(1)
            .expect("the badge listener")
            .split("});")
            .next()
            .expect("listener body");
        assert!(
            badge_pick.contains("this.pick_icon_image(cx)"),
            "the badge opens the same picker the add-image cell opens"
        );
        assert!(
            render.contains(".id(\"topic-icon-add-image\")"),
            "the add-image cell stays in the picker row"
        );
    }

    /// The rail card draws the stored icon, not just custom images (#2790):
    /// the same resolver the topic tab uses, so a pick in the rename dialog
    /// reaches the card. The rail resolves it in the window, which owns the
    /// decoded-image cache, and draws it in its own view (#2933), so the two
    /// halves are read where they now live. Scoped to those regions: this test
    /// module sits inside the same concat, and a bare split would land in the
    /// test's own strings.
    #[test]
    fn rail_card_resolves_the_stored_icon() {
        let src = DASHBOARD;
        let wiring = src
            .split("pub(crate) fn rail_icon(")
            .nth(1)
            .expect("the rail's icon resolver")
            .split("/// The icon picker's")
            .next()
            .expect("the resolver ends at the next method");
        assert!(
            wiring.contains("topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)"),
            "an embedded mark is recognised at the full rail-card media size"
        );
        assert!(
            wiring.contains("self.icon_images.get(stored)"),
            "a custom image comes from the view's decoded cache"
        );
        assert!(
            wiring.contains("topic_icons::IMG_PREFIX"),
            "an image that failed to decode paints nothing rather than its raw path"
        );
        let drawing = src
            .split("icon: row.flags.icon.as_ref()")
            .nth(1)
            .expect("the rail draws the resolved icon")
            .split("let chips = row")
            .next()
            .expect("the icon arm ends at the strip");
        assert!(
            drawing.contains("topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)"),
            "an embedded mark renders at the full rail-card media size"
        );
        assert!(
            drawing.contains("topic_icons::cached_picture(")
                && drawing.contains("sessions::RAIL_MARK_REMS"),
            "a custom image renders from the window's handle at the same size"
        );
        assert!(
            drawing.contains("topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS)"),
            "a legacy glyph keeps its text slot at the same size"
        );
    }

    #[test]
    fn a_topic_switch_hands_the_caret_back() {
        // #2741: a click on a topic chip took the caret and never asked for it
        // back, on both of the paths that do not go through `open_session`:
        // the session already sits in another leaf, or the leaf is renamed.
        let src = DASHBOARD;
        let body = fn_body(src, "open_topic_in_pane");
        assert_eq!(
            body.matches("self.request_composer_caret(cx)").count(),
            2,
            "both branches ask the composer for the caret"
        );
        assert_eq!(
            body.matches("self.rail_focused = false").count(),
            2,
            "a chip in the rail also lets the rail go, the way a row does"
        );
        let caret = fn_body(src, "request_composer_caret");
        assert!(
            caret.contains("editor.focus_pending = true"),
            "the field claims the caret from its own render, after the press"
        );
        assert!(
            caret.contains("self.refocus_screen()"),
            "the derived owner stays the one decision point"
        );
    }

    #[test]
    fn a_pick_lands_in_the_pane_that_holds_the_caret() {
        // #2819: every commit door went through `open_session`, which is the
        // window door and only knows the chat leaf. With a second pane holding
        // the caret the wrong pane changed, and the pane the reader was
        // looking at never moved.
        let src = DASHBOARD;
        let helper = fn_body(src, "open_picked_in_focused_pane");
        assert!(
            helper.contains("self.pane_focus"),
            "the door asks which pane holds the caret"
        );
        assert!(
            helper.contains("tree.contains(*focus)"),
            "a cached caret is only trusted while the tree still holds it"
        );
        let rename = ["tree.rename_leaf(caret, ", "id)"].concat();
        assert!(
            helper.contains(rename.as_str()),
            "the leaf holding the caret is the one that moves"
        );
        let follows = ["self.pane_focus = Some(", "id);"].concat();
        assert!(
            helper.contains(follows.as_str()),
            "the caret follows the conversation into the renamed leaf"
        );
        assert!(
            helper.contains("self.persist_layout()"),
            "the tree the user now sees is the one written down"
        );
        // The rail's two commits and its arrow preview all take this door.
        let keys = fn_body(src, "on_key");
        let commit = ["open_picked_in_focused_pane(id, ", "true, cx)"].concat();
        assert!(
            keys.contains(commit.as_str()),
            "a committed rail row moves the focused pane"
        );
        let preview = ["open_picked_in_focused_pane(id, ", "false, cx)"].concat();
        assert!(
            keys.contains(preview.as_str()),
            "the walk previews in the focused pane without the hand back"
        );
        // The row's click handler moved with the rail (#2933): the view asks
        // the window by name, so the commit is pinned on the method it calls.
        let row_pick = fn_body(src, "rail_row_pick");
        assert!(
            row_pick.contains(commit.as_str()),
            "the row click moves the focused pane"
        );
        let pick = fn_body(src, "pick_or_open");
        assert!(
            pick.contains(commit.as_str()),
            "the palette and the filter land in the focused pane"
        );
    }

    #[test]
    fn a_duplicate_pick_warns_instead_of_swapping() {
        // #2819: picking a conversation that already sits in a pane used to
        // drag it into the chat leaf and close the pane it came from, so one
        // pick could take a split apart for nothing.
        let src = DASHBOARD;
        let helper = fn_body(src, "open_picked_in_focused_pane");
        let (_before, after) = helper
            .split_once("if duplicate {")
            .unwrap_or_else(|| panic!("the duplicate branch"));
        let branch = after
            .split_once("let Some(caret) = caret else {")
            .map(|(branch, _)| branch)
            .unwrap_or_else(|| panic!("the branch ends where the chat leaf path begins"));
        assert!(
            branch.contains("self.warn_pane_duplicate(id, cx)"),
            "a duplicate says so instead of moving anything"
        );
        assert!(
            branch.contains("return false;"),
            "the pick reports that it did not land instead of renaming a leaf"
        );
        assert!(
            !branch.contains(["rename_leaf(caret, ", "id)"].concat().as_str()),
            "no leaf moves on the duplicate path"
        );
        let warn = fn_body(src, "warn_pane_duplicate");
        assert!(
            warn.contains("self.pane_warning = Some(title)"),
            "the card carries the conversation it named"
        );
        let overlays = fn_body(src, "open_overlays");
        assert!(
            overlays.contains("self.pane_warning.is_some()"),
            "the card owns the keyboard while it is up"
        );
        assert!(
            src.contains("is already open in another pane"),
            "the card says why the pick did nothing"
        );
        let keys = fn_body(src, "on_key");
        assert!(
            keys.contains("self.pane_warning = None"),
            "Enter or Esc closes the card"
        );
        let render = dashboard_render(src);
        assert!(
            render.contains("warning_overlay(&body, dismiss)"),
            "the card is the window's existing warning shape"
        );
    }

    #[test]
    fn a_topic_pick_drafts_in_the_pane_holding_the_caret() {
        // #2819: both topic-pick arms opened through `open_session`, which
        // moves the chat leaf, so asking for a new topic on a conversation
        // picked from the palette changed the wrong pane. The prompt resolves
        // its parent from the pane holding the caret, so an arm that carried on
        // after a refused pick would draft under the wrong conversation.
        let src = DASHBOARD;
        let body = fn_body(src, "run_finder_pick");
        let draft = body
            .split_once("TopicPick::Draft => {")
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("the draft arm"))
            .split_once("TopicPick::EnableThenDraft => {")
            .map(|(arm, _)| arm)
            .unwrap_or_else(|| panic!("the draft arm ends at the next arm"));
        let enable = body
            .split_once("TopicPick::EnableThenDraft => {")
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("the enable arm"))
            .split_once("PaletteAction::Favorite =>")
            .map(|(arm, _)| arm)
            .unwrap_or_else(|| panic!("the enable arm ends at the next palette row"));
        for (name, arm) in [("draft", draft), ("enable then draft", enable)] {
            assert!(
                arm.contains("if self.open_picked_in_focused_pane(id, true, cx)"),
                "the {name} arm opens in the pane holding the caret and only carries on when the pick landed"
            );
            assert!(
                !arm.contains("self.open_session(id, cx)"),
                "the {name} arm never moves the chat leaf"
            );
        }
        assert!(
            draft.contains("self.start_topic_draft(cx)"),
            "the draft arm opens the prompt"
        );
        assert!(
            enable.contains("this.start_topic_draft(cx)"),
            "the enable arm opens the prompt after the flag is written"
        );
        // The rail row's own "New topic" item walked the same chat leaf door,
        // so the prompt landed on the wrong conversation there too.
        let menu = fn_body(src, "rail_menu_popover_for");
        let new_topic = menu
            .split_once("MenuKind::NewTopic => {")
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("the row menu's new topic item"))
            .split_once("MenuKind::GroupTopics =>")
            .map(|(item, _)| item)
            .unwrap_or_else(|| panic!("the new topic item ends at the next entry"));
        assert!(
            new_topic.contains("if this.open_picked_in_focused_pane(id, true, cx)"),
            "the row menu opens the conversation in the pane holding the caret"
        );
        assert!(
            !new_topic.contains("this.open_session(id, cx)"),
            "the row menu never moves the chat leaf"
        );
    }

    #[test]
    fn the_chat_leaf_still_takes_the_pick_when_no_pane_has_the_caret() {
        // #2819 keeps the single pane window exactly as it was: with no other
        // leaf holding the caret, the chat leaf is the one that moves, and it
        // keeps whatever hand back the door asked for.
        let src = DASHBOARD;
        let helper = fn_body(src, "open_picked_in_focused_pane");
        let fallback = ["open_session_handing_back(id, ", "hand_back, cx)"].concat();
        assert!(
            helper.contains(fallback.as_str()),
            "the chat leaf path keeps the hand back it was given"
        );
        assert!(
            helper.contains("let Some(caret) = caret else {"),
            "the fallback is the no other pane case, not a guess"
        );
    }

    #[test]
    fn a_committed_row_leaves_the_keys_in_the_chat_leaf() {
        // #2741: the chat leaf adopts the row's conversation, so a split that
        // kept the caret in a neighbouring pane typed into a conversation the
        // click did not open.
        let src = DASHBOARD;
        let body = fn_body(src, "open_session_handing_back");
        let after = body
            .split_once("self.rail_focused = false")
            .map(|(_, rest)| rest)
            .expect("a committed row lets the rail go");
        assert!(
            after.contains("self.pane_focus = None"),
            "the chat leaf is the pane that took the conversation"
        );
        assert!(
            after.contains("self.request_composer_caret(cx)"),
            "the caret follows the conversation"
        );
        assert!(
            after.contains("self.refocus_screen()"),
            "the owner is derived again after the hand back"
        );
    }

    #[test]
    fn the_caret_is_repaired_from_the_window_not_the_owner() {
        // #2741: the owner does not change when a press lands on chrome, so a
        // reconciler that only acts on transitions never fires. The repair
        // reads the window instead.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        assert!(
            render.contains("caret -> composer repaired now={}"),
            "a repaired caret leaves a receipt"
        );
        assert!(
            render.contains("KeyboardOwner::Composer"),
            "only the composer repairs, an overlay owns the keys on purpose"
        );
        assert!(
            render.contains(".is_none_or(|handle| handle == self.focus)"),
            "the root holding the keys is the drift this repairs"
        );
    }

    #[test]
    fn an_overlay_never_leaves_the_caret_in_the_composer() {
        // #2767: the repair ran one way only. With the topic prompt open the
        // owner was the prompt, the composer kept the caret, so the name typed
        // into the field also landed in the message field and Enter submitted
        // the message instead of creating the topic.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        assert!(
            render.contains("caret -> root repaired now={}"),
            "the other direction leaves a receipt too"
        );
        assert!(
            render.contains("let leaked = composer.read(cx).focus_handle.is_focused(window)"),
            "the drift is the composer holding keys it no longer owns"
        );
        assert!(
            render.contains("an overlay or a screen took the keys while the composer"),
            "the repair is a branch with two arms, never one"
        );
    }

    #[test]
    fn a_send_is_refused_while_an_overlay_owns_the_keys() {
        // #2767: the second half. Even with the caret repaired, a send must
        // not reach the agent while a prompt is waiting for its own Enter.
        let src = DASHBOARD;
        let body = fn_body(src, "send_from_pane");
        assert!(
            body.contains("send refused"),
            "a refused send leaves a receipt naming the owner"
        );
        assert!(
            body.contains("self.open_overlays()"),
            "the refusal asks the one owner decision"
        );
    }

    #[test]
    fn sidebar_actions_existing_toggle_uses_the_same_transition() {
        // The toggle moved to the rail module in the split (#2822). Read that
        // file alone: the whole dashboard also contains this test's own needle,
        // and `rsplit` picks the last occurrence, which would be the string
        // rather than the function.
        let source = include_str!("dashboard_rail.rs");
        let body = source
            .split("fn toggle_rail")
            .nth(1)
            .and_then(|tail| tail.split("fn set_rail_open").next())
            .expect("toggle body");
        assert!(body.contains("self.set_rail_open(open, cx)"));
        assert!(!body.contains("self.rail_open = !self.rail_open"));
    }
}

/// How tall one wheel "line" is when the platform reports the tick in lines
/// instead of pixels, the same 20px GPUI's own list uses (#2698).
const WHEEL_LINE_PX: f32 = 20.0;

/// How long a first Ctrl+C stays armed before the second press stops meaning
/// "quit": the terminal's own three seconds (`tui/app/state.rs:3730`).
const QUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// What one Ctrl+C press spends itself on (#2308).
///
/// The terminal has answered this key the same way for years, so the window
/// asks the same question in the same order instead of inventing a second
/// meaning for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtrlCAction {
    /// The reader is scrolled up: put them back at the bottom, keep the window
    /// open.
    SnapBottom,
    /// Throw the draft away and arm the quit window, with the hint.
    ClearAndArm,
    /// A second press inside the window: leave.
    Quit,
}

/// The terminal's Ctrl+C ladder, as a decision (#2308).
///
/// `following` is the transcript's own follow, `input_empty` whether the
/// composer has anything to throw away, `pending` the first press while it is
/// still armed, and `now` the clock that judges it. A composer holding text is
/// a fresh start, never a confirmation: the press throws that text away and
/// arms again, so a keystroke typed between the two presses can never be the
/// thing that closes the window.
fn ctrl_c_action(
    following: bool,
    input_empty: bool,
    pending: Option<std::time::Instant>,
    now: std::time::Instant,
) -> CtrlCAction {
    // A reader scrolled up gets the bottom back and nothing else: the press
    // never costs them the window.
    if !following {
        return CtrlCAction::SnapBottom;
    }
    // The second press quits, and only over an empty composer. A pending
    // stamp in the future means the clock moved backwards: that is no proof
    // of a second press, so it re-arms instead of quitting.
    if input_empty
        && let Some(at) = pending
        && now
            .checked_duration_since(at)
            .is_some_and(|elapsed| elapsed < QUIT_WINDOW)
    {
        return CtrlCAction::Quit;
    }
    CtrlCAction::ClearAndArm
}

/// How far one wheel tick asks a transcript to move, in px (#2698).
///
/// Platforms disagree on the unit and on the axis: a trackpad reports pixels
/// and a mouse may report lines, and either may be horizontal only. Only the
/// vertical part decides whether the reader walked back through history.
fn wheel_dy(delta: &ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(d) => f32::from(d.y),
        ScrollDelta::Lines(d) => d.y * WHEEL_LINE_PX,
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
        self.follow_chat();
        for session in self.panes.as_ref().map(|t| t.leaves()).unwrap_or_default() {
            self.follow_pane(session, cx);
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

        // The rail is a cached view of its own (#2933). The window resolves one
        // frame of plain data and hands it over; the rows, the topic chips, the
        // badges and the filter menu are built there, and a frame that compares
        // equal replays the last prepaint instead of minting every listener
        // again. Built only when something draws it: an element nobody seats is
        // work nobody asked for, the rail is closed most of the time, and with
        // it closed the frame would resolve every map under it and paint
        // nothing (#2805).
        let rail = if self.rail_open || self.screen == Screen::Onboard {
            self.rail_element(cx)
        } else {
            div().into_any_element()
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
        let chrome = div()
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
            // header (#2596), and the compositor's own gesture moves it there.
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |_ev, window, _cx| {
                window.start_window_move()
            })
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
        // Exact live entries get their own captured rows (#2925). Keying by
        // entry index keeps the handler attached as later lines arrive.
        let mut live_press: std::collections::HashMap<usize, transcript::PressHandler> =
            std::collections::HashMap::new();
        if let Some(session) = self.chat_session {
            for (entry, row) in transcript::live_action_rows(&self.turn.live, session) {
                let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.live_menu_open(row.clone(), None, ev.position, cx);
                });
                live_press.insert(entry, std::rc::Rc::new(handler));
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

        // The transcript plans its attachment tiles from the width the pane
        // actually has, read from the live viewport rather than a constant, so
        // a row can never be planned wider than the box it is drawn into
        // (#2395). The usage screen loses its side panel from the same number.
        let pane_width = attachment_view::content_width(
            f32::from(window.viewport_size().width),
            self.rail_width * self.rem,
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
                    fold_handlers,
                    transcript::ViewHooks {
                        attachments: attachment_handlers,
                        drag: Some(self.copy_drag.clone()),
                        wheel: Some(wheel),
                        ctx_used: self.turn.ctx_tokens,
                        ctx_max: self.ctx_max,
                        live_click: Some(live_click.clone()),
                        message_click: message_click.clone(),
                        message_press: message_press.clone(),
                        live_press,
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
                    },
                )
                .into_any_element();
                let primary = if measure < primary_width {
                    chat_measure_column(primary, measure)
                } else {
                    primary
                };
                // The panes come from the tree (#2472): one leaf is the
                // window as it was, and each split adds a divider and the
                // panes its children hold.
                let body = self.panes_body(primary.into_any_element(), pane_width, true, cx);
                (
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
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
                        let identity = format!(
                            "{} · {}",
                            self.session_title().unwrap_or_else(|| "chat".to_string()),
                            status_identity(
                                &provider,
                                &model,
                                &tilde_path(&opencrabs::utils::cwd::launch_cwd()),
                                self.branch.as_deref().unwrap_or("-"),
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
                let rail = if self.rail_open {
                    self.rail_width * self.rem
                } else {
                    0.0
                };
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
            let tabs = self.topic_tabs(None, cx);
            let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.cycle_topic_edge(cx);
            });
            let strip = topic_strip("window", self.topic_edge, toggle_edge, tabs);
            topic_row = Some((self.topic_edge, strip.into_any_element()));
        }
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
                        col = col.child(central(
                            plan_view.into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                    }
                    let waiting = self.queued_for_strip();
                    if !waiting.is_empty() {
                        col = col.child(central(
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
                    }
                    // The follow-up offer sits where the queue does, directly
                    // above the composer it sends into (#2708).
                    if let Some(sid) = self.chat_session {
                        let offers = agent_bridge::options_snapshot(sid);
                        if !offers.is_empty() {
                            let rows = self.option_rows(sid, offers, cx);
                            col = col.child(central(
                                options_strip(&rows).into_any_element(),
                                centred_chat,
                                chat_measure_px,
                            ));
                        }
                    }
                    // A detached command sits directly above the editor: the same
                    // place the TUI puts it (on the input box's top border), which
                    // is where the eye goes while waiting.
                    if let Some(label) = self.running_tasks_label() {
                        col = col.child(central(
                            task_strip(label).into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                    }
                    // The unsplit chat uses the same composer swap as a split
                    // leaf (#2924). Without this arm the forward state changed
                    // but its destination picker was never rendered here.
                    let composer_replacement = if self.forward_pick.is_some()
                        && self.forward_leaf.is_none()
                    {
                        Some(self.forward_picker(cx))
                    } else if self.msg_select.is_active() {
                        Some(self.select_bar(cx))
                    } else {
                        None
                    };
                    if let Some(view) = composer_replacement {
                        col = col.child(central(view, centred_chat, chat_measure_px));
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
                        col = col.child(central(
                            self.composer.editor.clone().into_any_element(),
                            centred_chat,
                            chat_measure_px,
                        ));
                    }
                }
                if let Some(strip) = topic_bottom {
                    col = col.child(central(strip, centred_chat, chat_measure_px));
                }
                col.child(central(footer, centred_chat, chat_measure_px))
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
            .on_key_down(cx.listener(Self::on_key))
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
            // The drag lives on the root so it survives the pointer leaving
            // the handle: a 4px column cannot track a fast drag on its own.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                if this.dragging_rail
                    && let Some((from_x, from_w)) = this.drag_from
                {
                    let dx = f32::from(ev.position.x) - from_x;
                    let next = (from_w + dx) / this.rem;
                    if next < RAIL_MIN_REM {
                        // Dragged past the floor: close it, the same gesture
                        // the owner asked for instead of leaving a sliver.
                        this.rail_open = false;
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
        }
        // The panes go in before every overlay. gpui paints children in the
        // order they are added, so a pane added after an overlay covers it:
        // the key reference then drew under the chat and under the rail.
        match topic_side {
            Some((topic_tabs::Edge::Left, strip)) => root = root.child(strip).child(right),
            Some((_, strip)) => root = root.child(right).child(strip),
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
                this.preview = None;
                this.refocus_screen();
                cx.notify();
            });
            let body = self.preview_body(&open, cx);
            let detail =
                preview::detail_line(&open, self.preview_zoom, self.preview_video.as_ref());
            root = root.child(preview::overlay(&open, detail, body, close));
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
            };
            root = root.child(help_screen(&view));
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
            let rail_px = px(self.rail_width * self.rem);
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
                },
                handlers,
                chip,
                wheel,
                find::channel_hint(state.query(), &self.finder_channels),
            ));
        }
        // The window controls are the frame's, not a screen's (#2813): drawn
        // over the top-right corner so no screen's layout owns them, and before
        // the menus and modals below so a scrim that owns the pointer also owns
        // the buttons.
        if let Some(cluster) =
            window_controls::cluster(window.window_decorations(), |control, _ev, window, cx| {
                match control {
                    window_controls::Control::Minimize => window.minimize_window(),
                    window_controls::Control::Maximize => window.zoom_window(),
                    window_controls::Control::Close => cx.quit(),
                }
            })
        {
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

/// The conversation a launch opens: the one the user was in last time when it
/// is still on the rail, else the newest. Pure, so the choice is testable
/// without a window. A pointer at a session that was deleted (or a first run
/// with no pointer at all) falls back to the rail's order, which is newest
/// first (#2444).
fn pick_initial_session(
    items: &[opencrabs::db::models::Session],
    last: Option<Uuid>,
) -> Option<Uuid> {
    match last {
        Some(id) if items.iter().any(|s| s.id == id) => Some(id),
        _ => items.first().map(|s| s.id),
    }
}

/// Record the conversation the window is showing, so the next launch (this
/// surface or the terminal) reopens it. Split out from `open_session` so the
/// write is exercised on its own, under a scratch home, instead of through a
/// whole Dashboard.
fn remember_open_session(id: Uuid) {
    opencrabs::utils::last_session::write(id);
}

fn main() {
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
                        rail_chips_expanded: std::collections::HashSet::new(),
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
                        ctrl_c_pending: None,
                        // Registered by `main` once the window exists: it needs
                        // a handle on this view to judge the press (#2308).
                        ctrl_c_interceptor: None,
                        rail_scroll,
                        rail_view,
                        composer: chat_leaf::Composer::new(editor),
                        history: dashboard_history,
                        agent_rx,
                        sink: agent_sink,
                        agent_ready: false,
                        turn: chat_leaf::Turn::default(),
                        stashed: HashMap::new(),
                        ctx_max: 0,
                        metrics: chat_leaf::Metrics::default(),
                        branch: git_branch(&opencrabs::utils::cwd::launch_cwd()),
                    }
                });
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
        // The window exists: only now may the GUI claim the profile and preempt
        // the running TUI/daemon. Claiming before open_window meant a window
        // that failed to open left the user with no surface at all (#2784).
        agent_bridge::window_is_up();
        cx.activate(true);
    });
}

/// The card's single line for a turn running with no pane of its own (#2718).
///
/// The pane paints the lead in the accent colour and the tail muted, and a
/// card line has one colour to spend, so the halves are joined with the same
/// separator the terminal uses. Empty means the turn has said nothing yet: the
/// card then keeps its preview instead of blanking a line to show a blank.
///
/// The prose leads and the call follows (#2816). Narration is occasional, not
/// constant: most turns run from call to call and never speak, and on those
/// the call is still the whole line. When a turn does speak, the paragraph is
/// what it concluded and the call is only what it ran, so a card that printed
/// the call alone hid the sentence the chat above it was showing.
fn rail_activity_line(live: &transcript::LiveTurn) -> String {
    // The paragraph the turn last spoke, clipped by the pane's own rule so the
    // two surfaces never show a different slice of the same sentence.
    let narration = live
        .narration()
        .last()
        .map(transcript::collapsed_paragraph)
        .filter(|text| !text.is_empty());
    // `live_status_line` invents "OpenCrabs is thinking..." for a turn that has
    // produced nothing at all, which on a card would out-shout the preview it
    // is replacing. The rail waits for the first real event (#2718), and a
    // paragraph counts as one: a turn whose only output so far is prose still
    // has something to say.
    let idle = !live.thinking
        && live.tools.is_empty()
        && live.chunks == 0
        && live.stream.is_empty()
        && live.reasoning.is_empty()
        && narration.is_none();
    if idle {
        return String::new();
    }
    let (lead, tail) = transcript::live_status_line(live);
    let mut parts: Vec<String> = Vec::new();
    parts.extend(narration);
    // Then what it ran. The call-count and clock tail is appended after both,
    // so the line reads the way the chat does: prose, the call, then the tally.
    match transcript::live_activity(live) {
        Some(call) => parts.push(call),
        // A turn that spoke but has not called anything yet gets no second
        // "thinking" label: the prose is the line.
        None if parts.is_empty() && !lead.is_empty() => parts.push(lead),
        None => {}
    }
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts.join(" \u{b7} ")
}

/// Fold one progress event into a live card.
///
/// Free because two owners draw the same card: a split leaf's pane (#2614) and
/// a turn parked under a conversation the user switched away from (#2616). The
/// fold has to be identical for both or the card would tell a different story
/// depending on where the session happens to be sitting.
fn fold_live(
    live: &mut transcript::LiveTurn,
    progress: opencrabs::brain::agent::ProgressEvent,
    at: std::time::Instant,
) {
    use opencrabs::brain::agent::ProgressEvent;
    match progress {
        ProgressEvent::Thinking => live.thinking = true,
        ProgressEvent::StreamingChunk { text } => {
            live.thinking = false;
            // The arrival stamp, so a batch folded in one loop still measures
            // the time the provider actually spent streaming (#2703).
            live.note_chunk_at(&text, at);
        }
        ProgressEvent::IntermediateText { text, reasoning } => {
            live.thinking = false;
            if let Some(reasoning) = reasoning {
                live.push_reasoning_at(&reasoning, at);
            }
            live.push_narration(&text);
        }
        ProgressEvent::ToolStarted {
            tool_name,
            tool_input,
            ..
        } => {
            live.thinking = false;
            let description =
                opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
            live.push_tool(transcript::ToolCall::running(tool_name, description));
        }
        ProgressEvent::ToolCompleted {
            tool_name, success, ..
        } => {
            let open = live
                .tools
                .iter_mut()
                .rev()
                .find(|c| !c.done && c.name == tool_name && c.task_id.is_none());
            match open {
                Some(call) => {
                    call.done = true;
                    call.success = success;
                }
                None => live.push_tool(transcript::ToolCall {
                    name: tool_name.clone(),
                    description: tool_name,
                    success,
                    done: true,
                    task_id: None,
                }),
            }
        }
        ProgressEvent::DetachedTaskStarted { task_id, label } => {
            live.thinking = false;
            live.push_tool(transcript::ToolCall::detached(task_id, label));
        }
        ProgressEvent::DetachedTaskFinished {
            task_id, success, ..
        } => {
            if let Some(call) = live
                .tools
                .iter_mut()
                .rev()
                .find(|c| c.task_id.as_deref() == Some(task_id.as_str()))
            {
                call.done = true;
                call.success = success;
            }
        }
        ProgressEvent::QueuedUserMessage { text } => live.injected.push(text),
        _ => {}
    }
}

/// Park the turn on screen under `from` and bring back the one `to` owns.
///
/// The window paints one conversation at a time, but every session may be
/// running a turn of its own. State moves with the session instead of the
/// window: the outgoing turn is stored whole and the incoming one restored, so
/// neither the spinner nor the counters jump to a conversation that never sent
/// anything (#2616). A session with nothing in flight gets an empty slot,
/// which is what makes a switch to an idle conversation paint as idle.
///
/// A card that has ended is parked too (#2811). Gating the park on `busy` alone
/// dropped every turn the moment it stopped running, so a turn the user had
/// just stopped came back as an empty slot and the stop, the stream and the
/// verdict went with it. What decides the park is whether the card holds
/// anything the ledger cannot draw yet, which is `!live.is_empty()`; `busy`
/// rides along so the restored card knows whether anything is still moving.
///
/// The whole [`chat_leaf::Turn`] moves now, not six fields of it, so the
/// reload handshake's deadline and late re-checks travel with the turn they
/// belong to instead of waiting on screen for the next conversation (#2805).
fn swap_visible_turn(
    stashed: &mut std::collections::HashMap<Uuid, chat_leaf::Turn>,
    from: Option<Uuid>,
    to: Uuid,
    turn: &mut chat_leaf::Turn,
) {
    if from == Some(to) {
        return;
    }
    if let Some(from) = from
        && (turn.busy || !turn.live.is_empty())
    {
        stashed.insert(from, std::mem::take(turn));
    }
    match stashed.remove(&to) {
        // The flag travels with the card (#2811): a stopped turn comes back
        // stopped, not "running again".
        Some(restored) => *turn = restored,
        None => *turn = chat_leaf::Turn::default(),
    }
}

/// Whether a reload holds the turn it was waiting for (#2705).
///
/// The comparison is against the session's row total as the ledger reports it,
/// never against the windowed turn count: past `transcript::WINDOW` rows the
/// window stops growing, so a turn count could never reach an expectation
/// taken from it and the metrics of every later turn were thrown away.
fn settle_landed(expect: Option<usize>, total: i64) -> bool {
    expect.is_none_or(|want| total.max(0) as usize >= want)
}

/// The running detached command, above the input.
///
/// Same formatter as the TUI (`tui::render::background`), so the two surfaces
/// cannot disagree about what a running task looks like. No border and no
/// percentage width: it sits in the column flow and disappears when idle. It
/// aligns to the chat's right edge and prints in the palette's cyan, which is
/// where the owner reads a background job (#2617).
///
/// The right alignment is not the whole row: the label keeps a gutter and a
/// clip, so in a narrow pane it stops at the pane's padding instead of
/// touching the border, and a long command ellipsizes instead of painting
/// over the neighbouring leaf (#2622).
fn task_strip(text: String) -> Div {
    // #2678: the roster above the composer is the line the user quotes back at
    // the agent ("that job is stuck"), so a drag over it copies it. The strip
    // is one truncated line with no character grid of its own, so there is
    // nothing to select inside it: the gesture is the copy, and it copies the
    // whole label rather than the part that survived truncation.
    let press: Rc<std::cell::Cell<Option<Point<Pixels>>>> = Rc::new(std::cell::Cell::new(None));
    let down = press.clone();
    let up = press.clone();
    let copied = format!("⏳ {text}");
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .w_full()
        .min_w_0()
        .gap_2()
        .px_2()
        .text_xs()
        .text_color(rgb(palette::current().teal))
        .on_mouse_down(MouseButton::Left, move |ev, _window, _app| {
            down.set(Some(ev.position));
        })
        .on_mouse_up(MouseButton::Left, move |ev, _window, app| {
            let Some(from) = up.get() else { return };
            up.set(None);
            if (ev.position - from).magnitude() <= 2.0 {
                return;
            }
            app.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .child(SharedString::from(format!("⏳ {text}"))),
        )
}

/// The leaf that owns the keyboard in a split, `None` meaning the chat leaf.
///
/// `pane_focus` can name a pane the tree no longer holds: a closed leaf, a
/// session moved into the chat, a split that came back with one leaf. Read
/// literally, that id made EVERY leaf take the unfocused branch, so the split
/// drew a dim placeholder in every pane and no frame painted a real field at
/// all. A file dropped then landed in the window's editor, which nothing was
/// drawing, so the drop looked ignored (#2620).
///
/// A single leaf is never a split and the chat owns the keyboard, so the id is
/// not consulted at all. A live id names its own leaf, with the chat leaf
/// reading as `None` the way it does everywhere else. A stale id hands the
/// keyboard back to the chat when the chat is in the tree, and to the first
/// surviving leaf when it is not, so the window always draws one real composer
/// and one real drop target.
fn resolve_split_focus(
    leaves: &[Uuid],
    pane_focus: Option<Uuid>,
    chat: Option<Uuid>,
) -> Option<Uuid> {
    if leaves.len() < 2 {
        return None;
    }
    if let Some(id) = pane_focus.filter(|id| leaves.contains(id)) {
        return (Some(id) != chat).then_some(id);
    }
    if chat.is_some_and(|id| leaves.contains(&id)) {
        None
    } else {
        leaves.first().copied()
    }
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

#[cfg(test)]
mod tests {
    use super::{pick_initial_session, remember_open_session};
    use crate::composer_view::{composer_hint_for, queue_rows};
    use crate::dashboard_source::DASHBOARD;
    use crate::overlays::cd_entries_for;
    use crate::pane_chrome::CHAT_MEASURE_PX;
    use crate::status::help_rows;
    use crate::topic_bar_view::topic_label;
    use opencrabs::brain::mission_control::McAnalytics;

    /// A leaf is its own view now (#2805): its state and the transcript it
    /// draws are scanned here, not in the crate root.
    const PANE_VIEW: &str = include_str!("pane_view.rs");

    /// A live turn repaints on every fourth poll tick, not on every one. The
    /// spinner and the clock are the only things moving in that branch, and at
    /// 20 Hz each repaint rebuilt every pane's transcript (#2688).
    #[test]
    fn a_live_turn_repaints_on_every_fourth_poll_tick() {
        assert!(super::busy_repaint_due(4));
        assert!(super::busy_repaint_due(0));
        assert!(!super::busy_repaint_due(1));
        assert!(!super::busy_repaint_due(2));
        assert!(!super::busy_repaint_due(3));
        assert!(super::busy_repaint_due(8));
    }

    /// The conversation caps at a readable measure in a single pane, and any
    /// pane narrower than the cap draws at its own width (#2586).
    #[test]
    fn chat_measure_caps_a_wide_single_pane_only() {
        assert_eq!(super::chat_measure(2192.0, false), CHAT_MEASURE_PX);
        assert_eq!(
            super::chat_measure(CHAT_MEASURE_PX + 1.0, false),
            CHAT_MEASURE_PX
        );
        assert_eq!(super::chat_measure(CHAT_MEASURE_PX, false), CHAT_MEASURE_PX);
        assert_eq!(super::chat_measure(612.0, false), 612.0);
        assert_eq!(super::chat_measure(0.0, false), 0.0);
    }

    /// A split window is exempt: a pane there takes the full span it is
    /// given, wide or narrow, so the cap can never narrow a pane (#2586).
    #[test]
    fn chat_measure_leaves_a_split_pane_alone() {
        assert_eq!(super::chat_measure(2192.0, true), 2192.0);
        assert_eq!(super::chat_measure(996.0, true), 996.0);
        assert_eq!(super::chat_measure(320.0, true), 320.0);
        assert_eq!(super::chat_measure(CHAT_MEASURE_PX, true), CHAT_MEASURE_PX);
    }

    /// The measure belongs to the single pane and to nothing else (#2586). A
    /// split leaf draws at the span its own pane owns, so the pane renderer is
    /// asserted free of the cap: a split can never be narrowed by it, whatever
    /// later changes do to the single-pane branch.
    #[test]
    fn no_measure_cap_reaches_a_split_pane() {
        let src = DASHBOARD;
        // The leaf's own render, from the pane's frame down to its helpers: a
        // split pane builds its whole box there (#2805).
        let (_, rest) = PANE_VIEW
            .split_once(concat!("impl Render for ", "PaneView {"))
            .expect("the pane draws itself");
        assert!(
            !rest.contains("chat_measure") && !rest.contains("CHAT_MEASURE_PX"),
            "a split pane draws at its own full width, never at the measure"
        );

        let (_, primary) = src
            .split_once("let primary = transcript::view(")
            .expect("primary Chat transcript call exists");
        let (call, _) = primary
            .split_once(".into_any_element();")
            .expect("primary Chat transcript call ends");
        assert!(
            call.contains("measure,"),
            "a single pane draws at the measured width"
        );
        assert!(
            src.contains(
                &[
                    "let chat_measure_px = chat_measure(",
                    "primary_width, split_window);",
                ]
                .concat()
            ),
            "the measure comes from the pane width, exempting a split window"
        );
    }

    /// The primary Chat screen must share its mouse selection state with the
    /// transcript. The split view already did this; Chat silently passed no
    /// state and rendered no selectable rows (#2562).
    #[test]
    fn primary_chat_transcript_enables_drag_copy() {
        let src = DASHBOARD;
        let (_, after) = src
            .split_once("let primary = transcript::view(")
            .expect("primary Chat transcript call exists");
        let (call, _) = after
            .split_once(".into_any_element();")
            .expect("primary Chat transcript call ends");
        assert!(
            call.contains(&["drag: Some(self.copy_", "drag.clone()),"].concat()),
            "primary Chat must pass its shared CopyDrag state"
        );
        assert!(
            !call.contains(&["drag:", " None,"].concat()),
            "primary Chat must not disable transcript selection"
        );
    }

    /// The zoom chords: they need cmd or ctrl, both spellings of plus zoom in,
    /// both of minus zoom out, zero resets to the base rem, and any other key
    /// does nothing even chorded. Each case is one arm of the handler's own
    /// rule, so a later edit cannot drop one silently (#2425).
    #[test]
    fn zoom_keys_need_the_chord_and_cover_in_out_and_reset() {
        use crate::theme::{BASE_REM, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

        let rem = BASE_REM * 1.15;
        // Without the chord these keys stay text in the composer.
        assert_eq!(super::zoom_step_for("=", rem, false), None);
        assert_eq!(super::zoom_step_for("0", rem, false), None);
        // With it, both spellings of each direction act, and zero resets.
        assert_eq!(
            super::zoom_step_for("=", rem, true),
            Some((rem * ZOOM_STEP).min(BASE_REM * ZOOM_MAX))
        );
        assert_eq!(
            super::zoom_step_for("+", rem, true),
            super::zoom_step_for("=", rem, true)
        );
        assert_eq!(
            super::zoom_step_for("-", rem, true),
            Some((rem / ZOOM_STEP).max(BASE_REM * ZOOM_MIN))
        );
        assert_eq!(
            super::zoom_step_for("_", rem, true),
            super::zoom_step_for("-", rem, true)
        );
        assert_eq!(super::zoom_step_for("0", rem, true), Some(BASE_REM));
        // A key that is not a zoom key stays a keystroke, chord or not.
        assert_eq!(super::zoom_step_for("a", rem, true), None);
    }

    #[test]
    fn mission_schedule_duration_matches_the_terminal_format() {
        assert_eq!(crate::mission_control_view::mc_schedule_duration(0), "0s");
        assert_eq!(crate::mission_control_view::mc_schedule_duration(42), "42s");
        assert_eq!(
            crate::mission_control_view::mc_schedule_duration(61),
            "1m 1s"
        );
        assert_eq!(
            crate::mission_control_view::mc_schedule_duration(3661),
            "61m 1s"
        );
    }

    #[test]
    fn mission_top_tool_bar_tracks_relative_usage() {
        assert_eq!(
            crate::mission_control_view::mc_tool_bar(0, 100),
            "░".repeat(12)
        );
        assert_eq!(
            crate::mission_control_view::mc_tool_bar(50, 100),
            format!("{}{}", "█".repeat(6), "░".repeat(6))
        );
        assert_eq!(
            crate::mission_control_view::mc_tool_bar(100, 100),
            "█".repeat(12)
        );
        assert_eq!(
            crate::mission_control_view::mc_tool_bar(200, 100),
            "█".repeat(12)
        );
    }

    #[test]
    fn mission_mc_phantoms_empty_breakdown_has_no_rows() {
        let mut analytics = McAnalytics::default();
        analytics.phantom.total = 1;
        assert!(
            crate::mission_control_view::mc_phantoms_rows(&analytics, palette::current())
                .is_empty(),
            "an empty model breakdown must reach the panel's empty state"
        );

        analytics.phantom.by_model.push(("gpt-5".into(), 1, 0));
        assert_eq!(
            crate::mission_control_view::mc_phantoms_rows(&analytics, palette::current()).len(),
            2,
            "a populated breakdown keeps its totals and model rows"
        );
    }

    /// A tile opens a preview of the file it stands for, Esc closes it, and
    /// the overlay is painted above the panes. Read off the source because a
    /// click and a key need a live window: each of these is one line that a
    /// later edit could drop without any test noticing (#2395).
    #[test]
    fn preview_opens_from_a_tile_and_escape_closes_it() {
        let src = DASHBOARD;
        // Every needle is split in two: `include_str!` puts this test's own
        // text inside `src`, so a whole literal would match itself right here
        // and pin nothing at all.
        assert!(
            src.contains(&["this.open_pre", "view(path.clone(), cx)"].concat()),
            "a tile opens the file it stands for"
        );
        assert!(
            src.contains(&["let open = preview::Pre", "view::of(path);"].concat()),
            "opening decides the kind from the path"
        );
        assert!(
            src.contains(&["if self.preview.ta", "ke().is_some()"].concat()),
            "Esc closes the preview before anything under it"
        );
        assert!(
            src.contains(
                &[
                    "root = root.child(preview::over",
                    "lay(&open, detail, body, close));"
                ]
                .concat()
            ),
            "the preview is painted over both panes"
        );
    }

    /// The seams the attachment fix rests on, pinned by shape so a later
    /// refactor cannot quietly drop one (#2395): a tile click reaches the
    /// preview, the overlay is painted after the panes, and nothing on a
    /// render path shells out to ffmpeg.
    #[test]
    fn the_attachment_seams_stay_where_they_are() {
        let main = DASHBOARD;
        let transcript = include_str!("transcript.rs");
        let preview_src = include_str!("preview.rs");

        // 1. A tile is addressable by the path it stands for, and its click
        //    is the handler the view minted for that path: a picture opens the
        //    file it shows, not whichever tile was planned last.
        assert!(
            transcript.contains(&["format!(\"attach", "ment-{key}\")"].concat()),
            "a tile is addressable by the path it stands for"
        );
        assert!(
            transcript.contains(
                &[
                    ".on_click(move |event, window, cx| ",
                    "handler(event, window, cx))"
                ]
                .concat()
            ),
            "the tile click forwards into that path's handler"
        );
        assert!(
            main.contains(&["this.open_pre", "view(path.clone(), cx)"].concat()),
            "and that handler lands on the preview"
        );
        // A path can appear in more than one message, so a tile clones its
        // handler instead of taking it: taking it would leave the second tile
        // of the same file drawn but dead.
        assert!(
            transcript.contains(&["handlers.get(&key).clon", "ed()"].concat()),
            "every tile of the same path keeps its click"
        );
        assert!(
            transcript.contains(&["pub type ClickHandler = std::rc::", "Rc<dyn Fn"].concat()),
            "one handler per path, cloned per tile"
        );

        // 2. Painting order is z order in gpui: the overlay has to be a child
        //    added after the panes, or the transcript would take the clicks
        //    meant for it.
        let panes = main.find(&["root = root.child(ri", "ght)"].concat());
        let overlay = main.find(&["root = root.child(preview::over", "lay("].concat());
        let help = main.find(&["root = root.child(help_", "screen("].concat());
        let (Some(panes), Some(overlay), Some(help)) = (panes, overlay, help) else {
            panic!(
                "the paint order changed shape: panes={panes:?} overlay={overlay:?} help={help:?}"
            );
        };
        assert!(overlay > panes, "the preview is painted over the panes");
        assert!(
            overlay < help,
            "the key reference still paints above the preview"
        );

        // 3. A poster is extracted on the click, never while drawing. The
        //    transcript has no business with a subprocess, and the overlay body
        //    only ever receives a frame that was already extracted for it.
        assert!(
            !transcript.contains("ffmpeg"),
            "the transcript never extracts a poster frame"
        );
        let render_path = preview_src
            .split(&["pub fn bo", "dy("].concat())
            .nth(1)
            .and_then(|rest| rest.split(&["fn file_", "body("].concat()).next())
            .expect("the overlay body is one function");
        assert!(
            !render_path.contains("ffmpeg") && !render_path.contains("Command::"),
            "the overlay draws the frame it was handed, it never runs a command to get one"
        );
        assert_eq!(
            main.matches(&["VideoFacts::", "probe("].concat()).count(),
            1,
            "a clip is probed on the click and nowhere else"
        );
    }

    /// The theme modal is a centred overlay over the conversation: a scrim
    /// that dismisses, four cards, and no Keep button, because a pick saves at
    /// once. The sidebar, its row helper and its preview history went with the
    /// screen that hosted them (#2619).
    ///
    /// A source scan, because the rule is about wiring: the modal needs a
    /// window and a click to exercise.
    #[test]
    fn theme_modal_is_centred_and_saves_on_the_pick() {
        let src = DASHBOARD;
        let view = include_str!("themes_view.rs");
        // The scrim and the close control belong to the shell now (#2893): it
        // builds both ids from the prefix the modal hands it, so the themes
        // view names the modal and the shell owns what dismisses it.
        let shell = include_str!("modal_shell.rs");
        let scrim = ["format!(\"{prefix}", "-scrim\")"].concat();
        let modal = ["themes_view::", "modal("].concat();
        let card = [
            "themes_view::card(mode, label, hint, self.themes_card == ",
            "index, accent, shown)",
        ]
        .concat();
        let cards = [
            "for (index, &(mode, label, hint)) in themes_view::CARDS",
            ".iter().enumerate()",
        ]
        .concat();
        let close = ["format!(\"{prefix}", "-close\")"].concat();
        assert!(
            view.contains("id: \"theme-modal\""),
            "the modal names itself, and the shell derives the scrim from it"
        );
        assert!(shell.contains(&scrim), "the scrim is what dismisses");
        assert!(
            shell.contains(&close),
            "the header carries a close control, as the README says"
        );
        assert!(src.contains(&modal), "the modal is built in one place");
        assert!(src.contains(&card), "a card is drawn from the pick table");
        assert!(src.contains(&cards), "four cards, and only four");

        // The sidebar is gone, and so is the draft-and-keep flow it carried.
        for gone in [
            [".id(\"themes-", "sidebar\")"].concat(),
            ["themes_view::", "row("].concat(),
            ["themes_view::history_", "row"].concat(),
            [".id(\"themes-", "keep\")"].concat(),
            ["remember_", "preview"].concat(),
        ] {
            assert!(!src.contains(&gone), "leftover: {gone}");
        }
    }

    /// One dialog, one delete path. The bridge that erases the row has exactly
    /// one call site, and that site is the dialog's own confirm handler. A
    /// second call site would mean a gesture can delete a conversation
    /// without ever asking, which is the footgun this dialog exists to close.
    ///
    /// A source scan, because the rule is about wiring: the paths themselves
    /// need a window and a click to exercise.
    #[test]
    fn only_the_confirm_dialog_reaches_the_delete_bridge() {
        let src = DASHBOARD;
        // Built in halves so this test's own source does not contain the
        // needle it is counting: the file it reads includes this module.
        let needle = ["spawn_delete", "_session"].concat();

        let sites: Vec<usize> = src
            .match_indices(needle.as_str())
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            sites.len(),
            1,
            "one delete path only, found {}",
            sites.len()
        );

        let call = sites[0];
        let head = src.get(..call).expect("a call site is a char boundary");
        let owner = head
            .rfind("fn delete_session")
            .expect("the call sits inside delete_session");
        let between = src
            .get(owner..call)
            .expect("a fn header is a char boundary");
        assert!(
            !between.contains("\n    pub(crate) fn "),
            "the bridge must be reached from delete_session itself, not a neighbour"
        );

        // Two arming paths: the row's menu, and ctrl+d on the
        // cursor row (#2352). Both ask the dialog; arming is not deleting.
        // The needles are split so the scan cannot count its own source text.
        let strip_arm = ["this.request", "_delete(id, cx)"].concat();
        let chord_arm = ["self.request", "_delete(target, cx);"].concat();
        assert!(src.contains(&strip_arm), "the row menu must arm the dialog");
        assert!(
            src.contains(&chord_arm),
            "ctrl+d must arm the dialog through the resolved target"
        );
        // A chord that erased on its own would be a second path to the
        // bridge. The dialog's confirm stays the only caller.
        let key_erase = ["this.", "delete_session(id, cx)"].concat();
        assert_eq!(
            src.matches(key_erase.as_str()).count(),
            1,
            "only the dialog's confirm may call delete_session"
        );
        // Esc is the dialog's cancel, and it must answer the dialog before
        // the row menu or the turn it would otherwise abort.
        assert!(
            src.contains("if self.confirm_delete.take().is_some()"),
            "Esc must cancel the dialog first"
        );
    }

    use super::*;

    /// ctrl+d carries a delete, so the chord must never BE the delete: it
    /// resolves one row, then asks the same dialog the row's menu asks
    /// (#2352). The bridge stays behind the dialog's confirm button.
    #[test]
    fn ctrl_d_arms_the_confirm_dialog_not_the_delete() {
        let cursor_row = Uuid::from_u128(11);
        let open_chat = Uuid::from_u128(22);

        let mut state = sessions::SessionsState {
            selected: Some(open_chat),
            ..Default::default()
        };
        assert_eq!(
            state.delete_target(),
            Some(open_chat),
            "with nothing armed the chord targets the open chat"
        );

        state.cursor = Some(cursor_row);
        assert_eq!(
            state.delete_target(),
            Some(cursor_row),
            "the row the keyboard is on wins over the open chat"
        );

        state.cursor = None;
        state.selected = None;
        assert_eq!(
            state.delete_target(),
            None,
            "no row and no open chat means nothing to ask about"
        );

        // The chord is bound on the rail, where arrows already land. A bare
        // `d` would need a rail focus mode the window does not have, so the
        // chord is the only key that may carry a delete.
        let src = DASHBOARD;
        let chord = ["\"d\" if ev.keystroke.modifiers", ".control"].concat();
        assert!(src.contains(&chord), "ctrl+d must be bound on the rail");
    }

    /// #2721: the Seeker's deep search is wired end to end. The item builder
    /// folds the cached content hits into snippet rows, a debounced generation
    /// guarded read fills that cache, and both typing and the editing chords
    /// schedule it only when the query changes. A source scan, because the
    /// spawn needs a live event loop to exercise end to end.
    #[test]
    fn seeker_deep_search_wiring() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("the production half of the file");
        assert!(
            production.contains("fn schedule_seeker_find"),
            "the debounced content read exists"
        );
        assert!(
            production.contains("fn refresh_seeker_items"),
            "the card rebuild hook exists"
        );
        assert!(
            production.contains("state.refresh_items(items)"),
            "the rebuild swaps the open card's catalog"
        );
        assert!(
            production.contains("Some(snippet) if !seeker_query.is_empty()"),
            "a content hit stands its snippet in for the date"
        );
        assert!(
            production.contains(".with_keywords(&seeker_query)"),
            "the query keyword surfaces a content-only session"
        );
        assert_eq!(
            production
                .matches("self.seeker_find_if_changed(cx);")
                .count(),
            2,
            "typing and the editing chords both schedule on a query change"
        );
        assert!(
            production.contains("this.seeker_find_gen == generation"),
            "a stale read never rebuilds the card"
        );
    }

    /// A conversation's turn belongs to that conversation, not to the window.
    ///
    /// #2616: the window keeps ONE live turn, so switching away used to throw
    /// it out and a running turn went dark the moment the user looked at
    /// another chat: no spinner, no tool block, counters frozen. Parking the
    /// turn under its own session and handing it back on the way in is what
    /// makes the card survive a switch.
    #[test]
    fn a_turn_travels_with_its_session_across_a_switch() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();

        let mut stashed: std::collections::HashMap<uuid::Uuid, super::chat_leaf::Turn> =
            std::collections::HashMap::new();

        // A turn running in conversation A, with every piece of state the
        // card needs to look alive. Declared already carrying it, so the test
        // reads as the turn it describes and never as a blank that is filled
        // in later.
        let mut turn = super::chat_leaf::Turn {
            live: super::transcript::LiveTurn::default(),
            busy: true,
            cancel: Some(tokio_util::sync::CancellationToken::new()),
            ctx_tokens: Some(1_234),
            settle: super::chat_leaf::Settle::default(),
        };
        turn.live.begin("ask A".to_string());
        turn.live.thinking = true;
        turn.live.tokens = 42;
        turn.settle.expect = Some(7);
        turn.settle.retries = 2;
        turn.settle.rechecks = 1;
        turn.settle.deadline = Some(std::time::Instant::now());

        super::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

        assert_eq!(
            stashed.len(),
            1,
            "the running turn is parked under its own session, never dropped"
        );
        assert_eq!(stashed[&a].live.user.as_deref(), Some("ask A"));
        assert!(
            stashed[&a].live.thinking,
            "the spinner keeps its state while parked"
        );
        assert_eq!(stashed[&a].live.tokens, 42, "the counter survives a switch");
        assert!(
            stashed[&a].cancel.is_some(),
            "the parked turn keeps its cancel token"
        );
        assert_eq!(stashed[&a].settle.expect, Some(7));
        // The whole handshake travels now, not just the two fields the old
        // parameter list happened to name: the deadline and the late
        // re-checks belong to the turn that armed them (#2805).
        assert_eq!(stashed[&a].settle.retries, 2);
        assert_eq!(stashed[&a].settle.rechecks, 1);
        assert!(
            stashed[&a].settle.deadline.is_some(),
            "the wait's own budget parked with the turn it was measuring"
        );
        assert!(!turn.busy, "the conversation now on screen is idle");
        assert!(turn.live.user.is_none(), "B never inherits A's card");
        assert_eq!(turn.ctx_tokens, None, "nor A's context counter");

        // B runs its own turn, then the user goes back to A.
        turn.live.begin("ask B".to_string());
        turn.busy = true;
        super::swap_visible_turn(&mut stashed, Some(b), a, &mut turn);

        assert_eq!(
            turn.live.user.as_deref(),
            Some("ask A"),
            "A's card comes back, so its tool block and counters keep ticking"
        );
        assert!(turn.live.thinking);
        assert_eq!(turn.live.tokens, 42);
        assert!(turn.busy, "A is still running");
        assert!(turn.cancel.is_some(), "A's cancel token came back with it");
        assert_eq!(turn.settle.expect, Some(7));
        assert_eq!(turn.settle.retries, 2);
        assert_eq!(turn.ctx_tokens, Some(1_234));
        assert_eq!(stashed.len(), 1);
        assert_eq!(
            stashed[&b].live.user.as_deref(),
            Some("ask B"),
            "B's turn was parked on the way out"
        );

        // Opening the conversation already on screen is not a switch: nothing
        // is parked and the running turn is left alone.
        super::swap_visible_turn(&mut stashed, Some(a), a, &mut turn);
        assert_eq!(stashed.len(), 1, "a no-op switch parks nothing");
        assert_eq!(turn.live.user.as_deref(), Some("ask A"));
        assert!(turn.busy);
    }

    /// #2811: stopping a turn clears `busy` immediately, and the park used to
    /// be gated on `busy` alone. A turn the user had just stopped was therefore
    /// dropped on the way out of the session and came back as an empty pane,
    /// which is why the stop looked like it had never happened.
    #[test]
    fn a_stopped_card_survives_a_switch_without_claiming_it_is_running() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let mut stashed: std::collections::HashMap<uuid::Uuid, super::chat_leaf::Turn> =
            std::collections::HashMap::new();
        let mut turn = super::chat_leaf::Turn::default();
        turn.live.begin("ask A".to_string());
        turn.live.stream = "half an answer".to_string();
        turn.live.stopped = true;
        // What the stop already did: nothing is running any more.
        turn.busy = false;

        super::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

        assert_eq!(
            stashed.len(),
            1,
            "the stopped card is parked, never dropped for not running"
        );
        assert!(
            !stashed[&a].busy,
            "it is parked knowing nothing is running, so it cannot spin on return"
        );
        assert!(stashed[&a].live.stopped, "and knowing it was stopped");

        super::swap_visible_turn(&mut stashed, Some(b), a, &mut turn);

        assert_eq!(
            turn.live.stream, "half an answer",
            "the stream the user saw came back with the card"
        );
        assert!(turn.live.stopped, "and so did the fact it was stopped");
        assert!(
            !turn.busy,
            "a restored stop must not claim the turn is running: that is the frozen spinner"
        );
        assert!(stashed.is_empty(), "the card moved back onto the screen");
    }

    /// The park condition is about the card, not the turn: an idle
    /// conversation still parks nothing (#2616), so switching around a profile
    /// cannot accumulate empty entries (#2811).
    #[test]
    fn an_idle_conversation_parks_nothing() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let mut stashed: std::collections::HashMap<uuid::Uuid, super::chat_leaf::Turn> =
            std::collections::HashMap::new();
        let mut turn = super::chat_leaf::Turn::default();

        super::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

        assert!(
            stashed.is_empty(),
            "nothing was on screen, so nothing had to travel"
        );
    }

    /// The window keeps exactly one live turn, so every paint hands it over
    /// directly: no owner check can disagree with what is on screen, because
    /// the swap already did the deciding (#2616).
    #[test]
    fn every_paint_hands_over_the_turn_the_window_holds() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        assert!(
            !production.contains("live_owner"),
            "ownership is the parking map's job now, not a parallel ledger"
        );
        assert!(
            !production.contains("visible_live("),
            "the paint path no longer gate-keeps the card"
        );
        // The transcript takes the visible turn by reference, not as an
        // Option: the window keeps exactly one and swaps it on a switch
        // (#2616). Both remaining handovers belong to the window chat: the
        // attachment-tile scan and the body that paints it. A split pane
        // paints from its own `PaneView::live`, and the third window
        // handover was the Themes preview leaf the centered modal replaced
        // (#2619).
        assert_eq!(
            production.matches("&self.turn.live,").count(),
            2,
            "the attachment scan and the chat body both paint the window's turn"
        );
        assert_eq!(
            production.matches("swap_visible_turn(").count(),
            2,
            "one definition, one caller: opening a conversation"
        );
        // A turn that finishes in a conversation the user left retires its
        // parked card on every terminal path: the success and failure arms,
        // and the cancelled settle a stop produces instead of either (#2657).
        assert_eq!(
            production.matches("self.stashed.remove(&session);").count(),
            3,
            "Done, Failed and Cancelled all drop the parked card"
        );
    }

    /// A turn recovered at boot is a turn like any other, and it is painted on
    /// the conversation that owns it (#2627): on screen it takes the window's
    /// card, off screen it parks under its session (#2616), so switching to it
    /// shows the recovered work instead of an idle pane.
    #[test]
    fn a_resumed_turn_streams_into_its_pane() {
        let src = DASHBOARD;
        let arm = src
            .split("agent_bridge::TurnEvent::TurnResumed {")
            .nth(1)
            .expect("the resumed-turn arm");
        let arm = arm
            .split("agent_bridge::TurnEvent::BackgroundTaskDone")
            .next()
            .expect("the end of the arm");
        assert!(
            arm.contains("self.turn.live.resume(user_message)"),
            "the card names the message the killed process was answering, and says its row is already written"
        );
        assert_eq!(
            arm.matches("resume(user_message)").count(),
            2,
            "both the on-screen card and the parked copy are resumed: each draws a row the ledger already holds"
        );
        assert!(
            !arm.contains("begin(user_message)"),
            "a resumed turn that starts as a fresh one stamps the resume time and paints the question twice (#2740)"
        );
        assert!(
            arm.contains("self.turn.busy = true"),
            "a recovered turn is busy"
        );
        assert!(
            arm.contains("self.turn.cancel = Some(cancel)"),
            "stop has to end a recovered turn too"
        );
        assert!(
            arm.contains("self.stashed.insert("),
            "an off-screen recovery parks under its own session"
        );
        assert!(
            !arm.contains("reload = Some(session)"),
            "a reload landing on a running turn would wipe its card"
        );
    }

    #[test]
    fn the_running_task_strip_sits_right_in_teal() {
        // #2617: the strip reads the same on both surfaces, and the GUI parks
        // it above the composer on the chat's right edge in the palette's
        // cyan. Left-aligned amber was neither.
        let src = DASHBOARD;
        let strip = src
            .split("fn task_strip(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("task strip");
        assert!(strip.contains(".justify_end()"), "the strip sits right");
        assert!(strip.contains(".w_full()"), "the row owns the column width");
        assert!(
            strip.contains("palette::current().teal"),
            "a running job reads cyan"
        );
        assert!(
            !strip.contains("palette::current().amber"),
            "amber is not the running colour"
        );
    }

    #[test]
    fn the_running_task_strip_keeps_a_gutter_in_a_narrow_pane() {
        // #2622: full width is not the whole story. The label keeps a gutter
        // and a clip, so it stops at the pane's padding instead of touching
        // the border, and a long command truncates instead of painting over
        // the neighbouring leaf.
        let src = DASHBOARD;
        let strip = src
            .split("fn task_strip(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("task strip");
        assert!(strip.contains(".px_2()"), "the strip keeps a gutter");
        assert!(strip.contains(".truncate()"), "a long label ellipsizes");
        assert!(strip.contains(".min_w_0()"), "the label may shrink");
    }

    #[test]
    fn the_running_task_strip_copies_its_label_on_a_drag() {
        // #2678: the strip names the job the user wants to quote back ("that
        // command is stuck"). It is one truncated line with no character grid,
        // so the gesture is the copy: a press, then a release far enough away
        // to be a drag, writes the whole label. A plain click stays a click.
        let src = DASHBOARD;
        let strip = src
            .split("fn task_strip(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("task strip");
        assert!(
            strip.contains(".on_mouse_down(MouseButton::Left"),
            "the press is recorded"
        );
        assert!(
            strip.contains(".on_mouse_up(MouseButton::Left"),
            "the release decides whether it was a drag"
        );
        assert!(
            strip.contains("magnitude() <= 2.0"),
            "a release inside the threshold is a click, not a copy"
        );
        assert!(
            strip.contains("write_to_clipboard(ClipboardItem::new_string"),
            "a drag writes the label to the clipboard"
        );
        assert!(
            strip.contains("let copied = format!("),
            "the whole label is copied, not the truncated part"
        );
    }

    #[test]
    fn every_pane_owns_its_drag_to_copy_state() {
        // #2623: a pane transcript was built with `ViewHooks::default()`,
        // which leaves `drag: None`, so dragging over a split pane selected
        // nothing and copied nothing. The window's state cannot be shared:
        // row text is stored by ordinal, and two panes plus the window all
        // count rows from zero.
        let src = DASHBOARD;
        assert!(
            !PANE_VIEW.contains("ViewHooks::default()"),
            "a pane must hand its transcript a drag state"
        );
        // The hooks are the leaf's own now (#2805), built where the leaf
        // draws rather than where the walk used to.
        assert!(
            PANE_VIEW.contains("drag: Some(self.copy_drag.clone())"),
            "the pane's own drag state is wired to its transcript"
        );
        let state = PANE_VIEW
            .split("pub(crate) struct PaneView {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("the pane view");
        assert!(
            state.contains("copy_drag: Rc<CopyDrag>"),
            "each pane carries its own drag state"
        );
        // The pane's composer is the one that acknowledges the copy, because
        // the transcript cannot reach an editor (#2587).
        assert!(
            src.contains("pane_copy.set_copied_listener"),
            "the pane's own composer says the copy landed"
        );
        assert!(
            src.contains("pane_editor.downgrade()"),
            "the listener targets the pane's editor"
        );
    }

    /// The Seeker prints the window-only rows and routes them into the doors
    /// that already exist: the group modal and the split pick (#2712, #2713).
    #[test]
    fn the_seeker_prints_the_window_only_rows_and_routes_them() {
        let code = DASHBOARD
            .split("\nmod tests {")
            .next()
            .expect("the production half of the file");
        assert!(
            code.contains("items.extend(actions::gui_items());"),
            "the window adds the rows that need a window"
        );
        let finder = fn_body(code, "finder_items");
        assert!(
            finder.contains("actions::gui_items()"),
            "the picker carries them"
        );

        // The arm is read from the glyph that follows the needle, never from
        // the needle itself: the needle starts with `PaletteAction::`, so a
        // bare `find` on the tail returns zero and proves nothing.
        let group_pick = "PaletteAction::GroupTopicsOn(id) =>";
        let handler = code.find(group_pick).expect("the group pick has a handler");
        let tail = code
            .get(handler + group_pick.len()..)
            .expect("the arm starts on a glyph");
        let arm = tail.split("PaletteAction::").next().expect("the arm body");
        assert!(
            arm.contains("start_group_topics(Uuid::from_u128(id), cx)"),
            "the pick opens the same modal the rail menu opens"
        );

        let split_row = "PaletteAction::BeginSplit(layout) =>";
        let split = code.find(split_row).expect("the split row has a handler");
        let tail = code
            .get(split + split_row.len()..)
            .expect("the arm starts on a glyph");
        let arm = tail.split("PaletteAction::").next().expect("the arm body");
        assert!(
            arm.contains("self.begin_split(layout, cx)"),
            "the row walks the same split door the chord walks"
        );
    }

    #[test]
    fn a_wheel_tick_up_releases_the_follow_it_does_not_fight_the_scroll() {
        // #2698: the window follows its own bottom until the reader's hand
        // says otherwise. GPUI applies the wheel to the viewport itself, so
        // the listener may only drop the follow; fighting the scroll there
        // would double-apply it. Both surfaces mint their own tick, and each
        // one drops only its own follow.
        // Read the production half only: this test lives in the same file it
        // reads, so its own needles would count as hits and prove nothing.
        let code = DASHBOARD
            .split("\nmod tests {")
            .next()
            .expect("the production half of the file");
        // The pane's own tick lives with the leaf's state now (#2805), so the
        // two halves of #2698 are read where each one lives.
        let view = PANE_VIEW;
        assert!(
            code.contains("wheel_releases_follow(wheel_dy(&ev.delta))"),
            "the wheel is judged by the shared rule, never a second one"
        );
        assert!(
            code.contains("this.release_transcript_follow();"),
            "a tick over the conversation releases the window's follow"
        );
        assert!(
            view.contains("this.release_pane_follow(session, cx);"),
            "a tick over a pane releases that pane's follow, not the window's"
        );
        assert_eq!(
            view.matches("wheel: Some(Rc::new(wheel))").count(),
            1,
            "every pane boxes the tick it mints"
        );
        assert_eq!(
            code.matches("wheel: Some(wheel),").count(),
            1,
            "the conversation hands over the tick it already owns"
        );
    }

    #[test]
    fn a_wheel_tick_reads_the_same_in_pixels_and_in_lines() {
        let pixels = ScrollDelta::Pixels(gpui::point(px(0.0), px(-30.0)));
        assert_eq!(wheel_dy(&pixels), -30.0);
        assert!(!wheel_releases_follow(wheel_dy(&pixels)));
        let lines = ScrollDelta::Lines(gpui::point(0.0, 3.0));
        assert_eq!(wheel_dy(&lines), 3.0 * WHEEL_LINE_PX);
        assert!(wheel_releases_follow(wheel_dy(&lines)));
        // A trackpad tick that only moves sideways scrolls nothing vertically.
        let sideways = ScrollDelta::Pixels(gpui::point(px(-12.0), px(0.0)));
        assert_eq!(wheel_dy(&sideways), 0.0);
        assert!(!wheel_releases_follow(wheel_dy(&sideways)));
    }

    #[test]
    fn page_up_releases_the_follow_and_page_down_gives_it_back_at_the_end() {
        // #2698: the page keys are the same gesture as the wheel, and the
        // terminal binds them the same way. Down re-arms only when the page
        // lands on the end, so a row that lands mid-page cannot drag a reader
        // back down a screen they just left.
        let src = DASHBOARD;
        let branch = src
            .split("matches!(key, \"pageup\" | \"pagedown\")")
            .nth(1)
            .and_then(|rest| rest.split("if key == \"k\"").next())
            .expect("the page key branch");
        assert!(
            branch.contains("let at_end = y >= -max;"),
            "landing on the end is what gives the follow back"
        );
        assert!(
            branch.contains("(None, true) => self.rearm_transcript_follow()"),
            "a page down that reaches the end re-arms the conversation"
        );
        assert!(
            branch.contains("(Some(id), false) => self.release_pane_follow(id, cx)"),
            "a page up releases the pane it scrolled"
        );
    }

    #[test]
    fn a_panel_that_closes_hands_the_chat_back_at_its_newest_row() {
        // #2698: coming back from a panel is a return, not a resume. The
        // screen transition is the one place every panel funnels through, so
        // the re-arm lives there instead of in each close path.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        assert!(
            render.contains("self.screen == Screen::Chat && self.last_screen != Screen::Chat"),
            "the transition into the chat is detected"
        );
        assert!(
            render.contains("self.rearm_transcript_follow();"),
            "and it hands the reader the newest row"
        );
        assert!(
            src.contains("last_screen: Screen::Chat,"),
            "the window starts on the chat, so the first frame is not a return"
        );
        // Sending is the other half of the contract: a send follows its own
        // answer whatever the reader was doing before. Since #2756 the send is
        // two functions, the entry and the one that runs a turn, so the follow
        // is pinned in the half that actually sends.
        assert!(
            fn_body(src, "send_message").contains("self.send_message_showing("),
            "a send reaches the path that runs a turn"
        );
        assert!(
            fn_body(src, "send_message_showing").contains("self.rearm_transcript_follow();"),
            "a send re-arms the follow"
        );
    }

    /// The body of `fn name(` up to the next item at the same indent.
    fn fn_body(src: &'static str, name: &str) -> &'static str {
        item_body(src, &format!("fn {name}("))
    }

    /// The Dashboard's window render. Two impls in this file define `render`,
    /// and only this signature is the one that holds the window (#2741).
    fn dashboard_render(src: &'static str) -> &'static str {
        // Scoped to the window's own `Render` impl first: the rail is a view of
        // its own now (#2933) and its render carries the same signature, so a
        // bare split would read the rail's body instead of the window's.
        item_body(
            item_body(src, "impl Render for Dashboard"),
            "fn render(&mut self, window: &mut Window, cx: &mut Context<Self>)",
        )
    }

    /// The body of one item, from its own definition to its closing brace.
    ///
    /// Two things this gets right that a plain `split` does not: only a real
    /// definition counts, because the scans below quote the names they look
    /// for inside their own strings, and the body ends at the brace that
    /// closes it rather than at the next method, so a top level function in
    /// another module file reads the same as a `Dashboard` method (#2741).
    fn item_body(src: &'static str, needle: &str) -> &'static str {
        let at = src
            .match_indices(needle)
            .map(|(at, _)| at)
            .find(|&at| {
                let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
                let prefix = src[line..at].trim();
                prefix.is_empty() || prefix.starts_with("pub")
            })
            .unwrap_or_else(|| panic!("{needle} not found"));
        let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
        let indent = src[line..at]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let close = format!("\n{}}}", " ".repeat(indent));
        let rest = &src[at..];
        rest.split(&close).next().unwrap_or(rest)
    }

    #[test]
    fn a_running_turn_says_what_it_is_doing_on_its_card() {
        // #2718: the card drew only the last persisted message, so a
        // conversation running its first tool calls claimed to be idle, and a
        // sub-agent working with no pane at all said nothing anywhere.
        let mut live = transcript::LiveTurn::default();
        assert_eq!(
            rail_activity_line(&live),
            "",
            "a turn with nothing to report leaves the preview in its slot"
        );
        live.thinking = true;
        assert_eq!(
            rail_activity_line(&live),
            "OpenCrabs is thinking...",
            "the pane's own wording, not a second one for the rail"
        );
        live.thinking = false;
        live.tools.push(transcript::ToolCall::running(
            "bash".to_string(),
            "cargo test".to_string(),
        ));
        let line = rail_activity_line(&live);
        assert!(
            line.contains("cargo test"),
            "the call in flight is what the card names: {line}"
        );

        let src = DASHBOARD;
        // A session with no leaf keeps its turn here, folded by the same
        // function the panes use, so the two cannot describe it differently.
        let fold = fn_body(src, "fold_pane_progress");
        assert!(
            fold.contains("self.rail_live.get_mut(&session)"),
            "the event is kept, not just a streaming flag"
        );
        assert!(
            fold.contains("fold_live(live, progress, at)"),
            "the pane's own fold gives the card its line"
        );
        // The card takes the activity over the preview while it lasts, and
        // the render supplies it from the row map.
        let sessions = include_str!("sessions.rs");
        let row = fn_body(sessions, "row");
        assert!(
            row.contains("let preview = activity.or(preview);"),
            "the live line takes the preview's own slot"
        );
        assert!(
            row.contains("live_line"),
            "and reads in the accent colour while it is live"
        );
        // The frame the rail is handed, resolved in the rail's own module
        // (#2933): still the same owner lookup, one step away from the paint.
        let frame = src
            .split("flags: rail_view::RailRowFlags {")
            .nth(1)
            .expect("the rail's frame")
            .split("// The channel chip (#2452)")
            .next()
            .expect("the frame ends at the footer");
        assert!(
            frame.contains("activity: self.rail_activity_for(progress_id, cx)"),
            "the frame feeds the card from whichever owner holds the turn"
        );
        let lookup = fn_body(src, "rail_activity_for");
        for source in [
            // The split left the leaf in one map and the parked ones in
            // another, so the lookup asks `pane_view` for both (#2871).
            "self.pane_view(id)",
            "self.stashed.get(&id)",
            // The open session's turn reads through the hoisted state since
            // the chat leaf moved its session out of the window (#2805).
            "self.turn.busy && self.chat_state.session() == Some(id)",
            "self.rail_live.get(&id)",
        ] {
            assert!(
                lookup.contains(source),
                "a turn can be held by any of the four owners: {source}"
            );
        }
        // Every way a turn ends retires the line, or a card would keep
        // claiming work after it stopped.
        let poll = fn_body(src, "poll");
        let retires = poll.matches("self.rail_live.remove(&session);").count();
        assert_eq!(retires, 3, "Done, Failed and Cancelled each clear it");
    }

    /// #2816: the card's live line leads with the paragraph the turn spoke and
    /// names the call after it. Narration is occasional, so the call still owns
    /// the line on the turns that never speak.
    #[test]
    fn a_speaking_turn_puts_its_paragraph_before_the_call() {
        let mut live = transcript::LiveTurn::default();
        live.push_narration("The stamp reads the parent row, not the topics.");
        live.tools.push(transcript::ToolCall::running(
            "bash".to_string(),
            "sed -n 1791p sessions.rs".to_string(),
        ));
        let line = rail_activity_line(&live);
        let prose = line
            .find("The stamp reads the parent row")
            .expect("the paragraph is on the line");
        let call = line
            .find("sed -n 1791p")
            .expect("the call is on the line too");
        assert!(prose < call, "the paragraph leads the call: {line}");
    }

    /// Prose with no call yet is the whole line: a second "thinking" label
    /// beside the sentence the turn just said tells the reader nothing.
    #[test]
    fn a_speaking_turn_with_no_call_yet_drops_the_thinking_placeholder() {
        let mut live = transcript::LiveTurn::default();
        live.push_narration("Reading the rail row now.");
        let line = rail_activity_line(&live);
        assert!(line.contains("Reading the rail row now."), "{line}");
        assert!(!line.contains("thinking"), "no second label: {line}");
    }

    /// Narration alone is enough to put a line on the card, and a turn that has
    /// said nothing at all still leaves the preview in its slot.
    #[test]
    fn narration_alone_is_enough_to_show_a_card_line() {
        let mut live = transcript::LiveTurn::default();
        assert_eq!(rail_activity_line(&live), "");
        live.push_narration("Found the column.");
        assert_eq!(rail_activity_line(&live), "Found the column.");
    }

    #[test]
    fn the_menu_opens_at_the_pointer_that_asked_for_it() {
        // #2690: the press point is the anchor. The card used to be placed
        // beside the rail at the press height, which is not where the click
        // happened: a chip sits mid-row, and in a split the rail edge is not
        // the pane the user was looking at.
        let src = DASHBOARD;
        let builder = fn_body(src, "rail_menu_popover_for");
        assert!(
            builder
                .split_whitespace()
                .collect::<String>()
                .contains("self.rail_menu_at"),
            "the press the row recorded is what places the card"
        );
        assert!(
            !builder.contains("rail_width"),
            "the rail's width no longer decides where the card opens"
        );
        assert!(
            builder.contains("sessions::menu_popover(\"rail-context-menu\", items, at)"),
            "the card goes through the one pointer-anchored popover"
        );
    }

    #[test]
    fn the_rail_menu_is_anchored_at_the_press_through_one_popover() {
        // #2690: the card was absolute inside the row at `right_1` / `top_1`
        // (#2742), then placed beside the rail at the press height. Both
        // answered "where is the row"; the pointer is what the user clicked.
        let menu = include_str!("menu.rs");
        let popover = fn_body(menu, "menu_popover");
        assert!(
            popover.contains("anchored()"),
            "the card is anchored in window coordinates, not absolute in the row"
        );
        assert!(
            popover.contains(".position(at)"),
            "the press point is the anchor it opens from"
        );
        assert!(
            popover.contains(".snap_to_window_with_margin("),
            "a card near the window edge is pulled back inside it"
        );
        let src = DASHBOARD;
        // The window render builds it at the root: a row's own frame would
        // translate the window coordinates to that row's corner.
        let render = dashboard_render(src);
        assert!(
            render.contains("gpui::deferred(menu).with_priority(2)"),
            "the menu is attached at the window root, above the content"
        );
        assert!(
            render.contains("self.rail_menu_popover_for(id, cx)"),
            "the root render is what builds the open row's card"
        );
        // Every entry point records where the press landed: a row, a chip,
        // and the swipe (#2797). Counted inside the render, because this
        // scan's own string holds the same text and a whole-file count would
        // read one more.
        // The swipe still lands here, in the window's own move handler. The
        // row and the chip are built by the rail's view now, so they ask the
        // window by name and it records the point it is handed (#2933).
        let asks = dashboard_render(src)
            .matches("this.rail_menu_at = Some(ev.position);")
            .count();
        assert_eq!(
            asks, 1,
            "the swipe records its press where it happens; the other two ask"
        );
        // Split so this scan cannot count its own text.
        let recorded = ["self.rail_menu_at = Some", "(at);"].concat();
        assert_eq!(
            src.matches(&recorded).count(),
            2,
            "the row and the chip each record the point the view handed them"
        );
        let builder = fn_body(src, "rail_menu_popover_for");
        assert!(
            builder
                .split_whitespace()
                .collect::<String>()
                .contains("self.rail_menu_at"),
            "the card opens at the point the press recorded"
        );
        assert!(
            !builder.contains("rail_width"),
            "and not at the rail's edge, which is not where the click landed"
        );
    }

    #[test]
    fn rail_slide_opens_the_menu_not_a_strip() {
        // #2797: the slide used to open a flex_none strip beside the body,
        // which clipped the title instead of shrinking. It now opens the
        // same rail menu the right-click opens, and the strip's state is
        // gone for good, not parked behind a flag. The needle is split so
        // the scan cannot count its own source text.
        let src = DASHBOARD;
        let dead = ["rail_", "actions"].concat();
        assert!(
            !src.contains(&dead),
            "the strip's state is removed from every site"
        );
        let slide = dashboard_render(src)
            .split("sessions::slide_reveals(from_x")
            .nth(1)
            .expect("the slide handler")
            .split("cx.notify();")
            .next()
            .expect("the slide block");
        assert!(
            slide.contains("&& this.sessions.rail_menu != Some(id)"),
            "a slide on the row whose menu is already open changes nothing"
        );
        assert!(
            slide.contains("this.sessions.rail_menu = Some(id);"),
            "a slide past the threshold opens the row's menu"
        );
        assert!(
            slide.contains("this.rail_menu_at = Some(ev.position);"),
            "the slide anchors the menu where the pointer landed"
        );
        assert!(
            slide.contains("this.rail_slide_opened = Some(id);"),
            "the release that ends a slide must not pick the row"
        );
    }

    #[test]
    fn a_release_clears_the_slide_before_a_chip_can_swallow_it() {
        // #2911: the chip, the "+N" pill and the collapse pill all stop the
        // bubble in their click listeners, so the root's mouse-up never ran for
        // a press on any of them. The armed swipe outlived the click and the
        // next pointer move opened the card's menu on a plain left click.
        // Capture runs root first, so no child can swallow the clear.
        let src = DASHBOARD;
        let render = dashboard_render(src);
        let clear = render
            .split("capture_any_mouse_up")
            .nth(1)
            .expect("the window root clears the swipe in the capture phase")
            .split("});")
            .next()
            .expect("the capture listener");
        assert!(
            clear.contains("this.rail_swipe = None;"),
            "a release disarms the slide before any child can stop the bubble"
        );
        // The three listeners that swallow the bubble are why it has to be
        // capture. If one of them stops doing that, this test re-reads why the
        // clear sits in the capture phase rather than the bubble. They are
        // built by the rail's own view now (#2933), so the scan follows them
        // there: the root's capture still runs before any of them, because
        // capture walks down from the window whatever the child's depth.
        let rail = include_str!("rail_view.rs");
        for action in [
            "dash.rail_chip_pick(",
            "dash.rail_expand_chips(",
            "dash.rail_collapse_chips(",
        ] {
            let listener = rail
                .split("cx.listener(")
                .find(|chunk| chunk.contains(action))
                .unwrap_or_else(|| panic!("{action} is still a listener"));
            assert!(
                listener.contains("cx.stop_propagation();"),
                "{action} is why the release has to be caught on the way down"
            );
        }
    }

    #[test]
    fn a_row_menu_split_grows_from_the_focused_pane() {
        // #2625: the row menu named the session, so opening it as the chat
        // first and then asking again grew the split from the first pane.
        let src = DASHBOARD;
        // Rows and topic chips share one popover builder (#2701), so the
        // split arms live there instead of inside the window render.
        let popover = fn_body(src, "rail_menu_popover_for");
        let menu = popover
            .split("sessions::MenuKind::SplitSide")
            .nth(1)
            .and_then(|rest| rest.split("MenuKind::Delete").next())
            .expect("the split arms");
        assert!(
            menu.contains("this.split_row(id, Layout::Side, cx)"),
            "a side split from a row answers the pick with that row"
        );
        assert!(
            menu.contains("this.split_row(id, Layout::Stack, cx)"),
            "a stack split does the same"
        );
        assert!(
            !menu.contains("open_session(id, cx)"),
            "opening the row first moved the chat leaf and flattened the tree"
        );
        let split_row = fn_body(src, "split_row");
        assert!(
            split_row.contains("self.split_origin()"),
            "the pane being split is the one holding the caret"
        );
        assert!(
            split_row.contains("self.choose_split(id, cx)"),
            "the row already answered the pick"
        );
    }

    #[test]
    fn a_topic_chip_answers_the_split_pick() {
        // #2760: a chip stops propagation, so the parent row's picker never
        // ran and the topic opened in the chat leaf while the split waited
        // for a session nobody was going to pick.
        // Only the render counts for the chip: the whole file holds this
        // test's own quoted names, so a plain scan reads its own strings as
        // the chip's (#2741). The helper is defined outside the render, so the
        // body scan below still needs the whole file.
        let src = DASHBOARD;
        // The chip's listener moved with the rail (#2933) and asks the window
        // by name, so the pick is pinned on the method it calls.
        let chip_pick = fn_body(src, "rail_chip_pick");
        assert!(
            !chip_pick.contains("self.open_session("),
            "the chip must not open the topic directly"
        );
        assert!(
            chip_pick.contains("self.pick_or_open(id, cx)"),
            "the chip answers the picker the way a row does"
        );
        let helper = fn_body(src, "pick_or_open");
        assert!(
            helper.contains("self.split_pick.is_some()"),
            "the helper asks whether a pane is waiting"
        );
        assert!(
            helper.contains("self.choose_split(id, cx)"),
            "and fills it instead of opening the conversation"
        );
    }

    #[test]
    fn the_seeker_and_the_filter_answer_the_split_pick() {
        // #2760: the Seeker's OpenSession arm and the rail filter's Enter both
        // opened directly, so a pick from either left the waiting pane armed.
        let src = DASHBOARD;
        assert!(
            src.contains(
                "PaletteAction::OpenSession(id) => self.pick_or_open(Uuid::from_u128(id), cx)"
            ),
            "a Seeker row answers the picker"
        );
        assert!(
            fn_body(src, "on_key").contains("self.pick_or_open(id, cx)"),
            "the filter's Enter answers it too"
        );
    }

    #[test]
    fn a_split_keeps_the_topics_bar_on_the_pane_that_carries_it() {
        // #2761: every topic pane drew the whole family strip, so a wall of
        // topics repeated one bar per pane and each bar claimed topics that
        // belong to a different pane.
        let src = DASHBOARD;
        assert!(
            src.contains("let carries_topics = leaves.len() == 1 || pane_root == *session;"),
            "only the pane carrying the topics draws the strip"
        );
        assert!(
            src.contains("if carries_topics && self.sessions.is_topic_enabled(pane_root) {"),
            "and the strip is gated on it"
        );
    }

    #[test]
    fn the_split_picker_starts_on_the_pane_being_split() {
        // #2625: the picker's cursor was the chat's row, so with the caret in
        // a right-hand pane the list opened on the wrong session.
        let src = DASHBOARD;
        let body = fn_body(src, "begin_split");
        assert!(
            body.contains("self.sessions.cursor = Some(target);"),
            "the cursor starts on the pane being split"
        );
        assert!(
            !body.contains("self.sessions.cursor = self.chat_session;"),
            "the chat's row is not the pane being split"
        );
    }

    #[test]
    fn begin_split_targets_the_focused_leaf() {
        // #2625: the chord door resolves its target, so a split grows from the
        // leaf holding the caret rather than from whichever leaf the tree
        // happens to reach first. #2757: the resolution lives in
        // `split_origin`, which answers with the caret leaf the RENDER drew
        // instead of refusing on a stale `pane_focus`.
        let src = DASHBOARD;
        let body = fn_body(src, "begin_split");
        assert!(
            body.contains("let Some(target) = self.split_origin() else"),
            "the chord door splits the pane that holds the caret"
        );
        assert!(
            !body.contains("self.pane_focus.or(self.chat_session)"),
            "the resolution stays in `split_origin`, not a second copy here"
        );
        let origin = fn_body(src, "split_origin");
        assert!(
            origin.contains("resolve_split_focus(&leaves, self.pane_focus, chat)"),
            "a stale caret id falls back the way the render falls back"
        );
        assert!(
            origin.contains(".filter(|id| leaves.contains(id))"),
            "a leaf the tree no longer holds is not a split target"
        );
        assert!(
            origin.contains("self.panes = Some(PaneTree::leaf(chat));"),
            "a window with no tree starts from the chat's own leaf"
        );
        assert!(
            body.contains("self.split_target = Some(target);"),
            "the picker answers into the leaf the caret was on"
        );
        // The chord is the door this test pins, so it has to reach the
        // resolution above rather than a path of its own.
        let keys = fn_body(src, "on_key");
        assert!(
            keys.contains("if let Some(layout) = split_chord(ks)"),
            "the split chord is what names the layout"
        );
        assert!(
            keys.contains("self.begin_split(layout, cx)"),
            "the chord opens the pick through `begin_split`"
        );
    }

    #[test]
    fn a_split_pane_draws_the_plans_density_shape() {
        // #2622, #2624: a pane is a fraction of the window, so its plan is
        // the density shape (`plan_strip::pane_view`), never the six-row
        // checklist that clipped mid-word there, and never a shrinkable card
        // that a short pane squeezed out of sight.
        let src = DASHBOARD;
        let body = src
            .split("fn panes_body(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("panes body");
        assert!(
            body.contains("self.plan.pane_view(density, expanded)"),
            "the chat leaf in a split draws the compact strip"
        );
        // A split pane builds its own strip in its own render (#2805), so the
        // density shape is read where that leaf draws.
        assert!(
            PANE_VIEW.contains("self.plan.pane_view(density, expanded)"),
            "every pane draws the compact strip"
        );
        assert!(
            body.contains("plan_strip::pane_density(count, pane_width)"),
            "the shape follows the pane count and the pane's own width"
        );
        assert!(
            PANE_VIEW.contains("plan_strip::pane_density(panes, self.width)"),
            "a pane reads the same shape from its own width"
        );
        assert!(
            body.contains("toggle_pane_plan"),
            "a click expands the checklist for that pane alone"
        );
        assert!(
            !body.contains(".plan.view()"),
            "no split pane draws the full checklist"
        );
    }

    #[test]
    fn a_click_anywhere_in_a_pane_takes_the_keyboard() {
        // #2758: the pane took the keyboard in the BUBBLE phase, and a
        // transcript text row stops the press on its way up, so a click on
        // text never moved the caret and ctrl+| split whatever leaf the
        // window still called focused instead of the pane just clicked.
        let body = PANE_VIEW
            .split_once("let mut node = div()")
            .map(|(_, leaf)| leaf)
            .expect("the leaf builds its own box");
        assert!(
            body.contains(".capture_any_mouse_down(take_keyboard)"),
            "the pane takes the keyboard before a row can stop the press"
        );
        assert!(
            !body.contains(".on_mouse_down(MouseButton::Left, take_keyboard)"),
            "a bubble-phase listener never runs under a transcript row"
        );
    }

    #[test]
    fn the_drop_band_only_follows_the_pane_under_the_pointer() {
        // #2759: gpui hands every `on_drag_move` listener every move while a
        // drag is live, hovered or not, so each pane answered with its own
        // bounds and the last pane painted put the band on itself.
        let src = PANE_VIEW;
        let body = src
            .split(".on_drag_move::<PaneDrag>(")
            .nth(1)
            .and_then(|rest| rest.split(".on_drop::<PaneDrag>(").next())
            .expect("the drag-move listener");
        assert!(
            body.contains("x >= bounds[0] && x <= bounds[2]"),
            "the pane checks the pointer is inside its own bounds"
        );
        assert!(
            body.contains("&& y <= bounds[3];"),
            "the check covers both axes"
        );
        assert!(
            body.contains("if inside {"),
            "only a pane the pointer is over answers with a half"
        );
        assert!(
            body.contains("drop_half(bounds, x, y)"),
            "the half still comes from the same bounds and pointer"
        );
        assert!(
            body.contains(".is_some_and(|(target, _)| target == session)"),
            "a pane clears only the band it owns"
        );
    }

    #[test]
    fn every_pane_keeps_its_strips_at_their_own_height() {
        // #2624: the transcript takes the slack and the strips keep their
        // height. Without this the plan card was squeezed to zero in a short
        // pane, which is why it only ever vanished in a split.
        let body = PANE_VIEW
            .split_once("let mut node = div()")
            .map(|(_, leaf)| leaf)
            .expect("the leaf builds its own box");
        assert!(
            body.contains(".flex_none().min_w_0().child(chrome)"),
            "the chrome stack is not shrinkable"
        );
        assert!(
            body.contains(
                "div()\n                .flex()\n                .flex_col()\n                .flex_1()\n                .min_w_0()\n                .min_h_0()\n                .child(body)"
            ),
            "the body is the child that shrinks, and it can"
        );
    }

    /// The composer's hint names the keys that work in the state on screen, so
    /// the queue's instructions sit under the caret instead of in the card
    /// (#2630).
    #[test]
    fn the_composer_hint_names_the_queue_while_a_turn_runs() {
        assert_eq!(
            composer_hint_for(false, 0).as_ref(),
            "Talk to the crab…",
            "an idle composer keeps its own prompt"
        );
        let running = composer_hint_for(true, 0);
        assert!(
            running.contains("esc") && running.contains("queues"),
            "a running turn says Esc stops it and Enter queues: {running}"
        );
        let queued = composer_hint_for(true, 2);
        assert!(
            queued.contains("queued 2"),
            "the count is on screen: {queued}"
        );
        assert!(
            queued.contains('↑') && queued.contains("esc"),
            "Up edits a queued message and Esc flushes it: {queued}"
        );
    }

    /// A hint is a placeholder, never a control: with anything typed the field
    /// shows the draft and the hint is gone (#2630).
    #[test]
    fn the_hint_never_survives_a_draft() {
        let editor = include_str!("editor.rs");
        assert!(
            editor.contains("let display = if empty { placeholder } else { content };"),
            "the field paints the placeholder only while it is empty"
        );
        assert!(
            editor.contains("hsla_from(palette::current().text_faint)"),
            "the hint is the faint colour, not the text colour"
        );
        assert_eq!(
            composer_hint_for(false, 0).as_ref(),
            "Talk to the crab…",
            "an empty idle field keeps the product's own prompt"
        );
    }

    /// One Esc stops the turn and hands the queue to the next one: the branch
    /// cancels, reads idle at once, and the post-batch drain sends what is
    /// queued as the cancellation is acknowledged (#2631).
    #[test]
    fn esc_cancels_the_turn_and_sends_the_queue() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let branch = production
            .split("// One Esc stops the turn, and the queue becomes the next turn.")
            .nth(1)
            .and_then(|rest| rest.split("// Page keys scroll the transcript").next())
            .expect("the Esc branch for a running turn");
        assert!(branch.contains("cancel.cancel()"), "Esc cancels the turn");
        assert!(
            branch.contains("self.turn.busy = false;"),
            "the pane stops claiming a turn is running the moment Esc lands"
        );
        assert!(
            branch.contains("Sending the {waiting} queued."),
            "the stop says the queue is next"
        );
        assert!(
            !branch.contains("self.spawn_turn("),
            "no second turn starts while the cancel is still unwinding"
        );
        assert!(
            production.contains("if !self.turn.busy {\n            self.turn.cancel = None;\n            self.drain_pending(cx);"),
            "the turn's own event drains the queue once the cancel is acknowledged"
        );
    }

    /// With nothing queued Esc is only a stop: the wording stays the plain
    /// cancellation and the drain finds nothing to send (#2631).
    #[test]
    fn esc_with_no_queue_only_cancels() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        // rustfmt wraps a long call chain across lines, so compare the code
        // with its whitespace removed: the same call, however it is laid out.
        // Both sides are flattened, because the message itself holds a space.
        let flat: String = production.chars().filter(|c| !c.is_whitespace()).collect();
        let plain = "self.turn.live.notices.push(\"Operation cancelled.\".to_string());"
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        assert!(
            flat.contains(&plain),
            "an empty queue keeps the plain wording"
        );
        let drain = production
            .split("    pub(crate) fn drain_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Create a conversation").next())
            .expect("drain_pending");
        assert!(
            drain.contains("if waiting.is_empty() {\n            return;\n        }"),
            "an empty queue sends nothing"
        );
    }

    /// A pane's turn-end paths flush that leaf's own queue (#2695): settle,
    /// fail and the Cancelled branch all hand the session to
    /// `drain_pane_pending`, which takes all, joins all and spawns one turn,
    /// the window's contract. Before this the leaf had no drain at all, so
    /// messages 2 and 3 sat in the strip after the stop.
    #[test]
    fn esc_flush_pane_turn_end_drains_the_leaf_queue() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let settle = production
            .split("    pub(crate) fn settle_pane(")
            .nth(1)
            .and_then(|rest| rest.split("    /// The pane's turn failed").next())
            .expect("settle_pane");
        assert!(
            settle.contains("self.drain_pane_pending(session, cx);"),
            "a settled pane flushes its queue"
        );
        let fail = production
            .split("    pub(crate) fn fail_pane(")
            .nth(1)
            .and_then(|rest| rest.split("    /// A pane's turn ended").next())
            .expect("fail_pane");
        assert!(
            fail.contains("self.drain_pane_pending(session, cx);"),
            "a failed pane flushes its queue"
        );
        let cancelled = production
            .split("agent_bridge::TurnEvent::Cancelled { session } =>")
            .nth(1)
            .and_then(|rest| rest.split("agent_bridge::TurnEvent::TurnReverted").next())
            .expect("the Cancelled branch");
        assert!(
            cancelled.contains("self.drain_pane_pending(session, cx);"),
            "a stopped pane flushes when the cancellation lands, the #2631 ordering"
        );
        let drain = production
            .split("    pub(crate) fn drain_pane_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
            .expect("drain_pane_pending");
        assert!(
            drain.contains("if pane.read(cx).busy {"),
            "never a second spawn into a running or unwinding turn"
        );
        assert!(
            drain.contains("agent_bridge::queue_take(session)"),
            "the flush takes the whole queue, not one message"
        );
        assert!(
            drain.contains("agent_bridge::PendingMessage::join(&msgs)"),
            "the flush joins every message into one turn"
        );
        assert!(
            drain.contains("agent_bridge::spawn_turn("),
            "the flush sends the joined turn"
        );
    }

    /// #2811: the cancel arm is the only place a stop is recorded, so three
    /// things have to stay true there. The card says it was stopped (otherwise
    /// a spinner turns over a turn nobody is running), the stream the user saw
    /// is left alone (the dropped future cannot promise the ledger has it, so a
    /// reload would wipe it), and the row that makes the verdict survive a
    /// reload is written.
    #[test]
    fn the_cancel_arm_records_the_stop_and_keeps_the_stream() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let cancelled = production
            .split("agent_bridge::TurnEvent::Cancelled { session } =>")
            .nth(1)
            .and_then(|rest| rest.split("agent_bridge::TurnEvent::TurnReverted").next())
            .expect("the Cancelled branch");
        assert!(
            cancelled.contains("self.turn.live.stop();"),
            "the card must say the turn was stopped instead of spinning, \
             through the one call that also freezes its clock (#2849)"
        );
        assert!(
            cancelled.contains("agent_bridge::spawn_record_turn_stopped("),
            "and the stop must outlive the card as a row"
        );
        assert!(
            !cancelled.contains("reload = Some(session);"),
            "no reload here: the dropped future cannot promise the ledger has the text"
        );
    }

    /// Chrome may not refuse a turn the window minted for a named session
    /// (#2795).
    ///
    /// `open_overlays()` is window state with no session in it, so the overlay
    /// gate in `send_from_pane` used to refuse every pane send while any
    /// overlay was open. The plan card's Approve and Discard taps, and a
    /// command's own reply turn, are exactly that shape: a click for one
    /// session, refused because another part of the window held the keyboard.
    /// The plan then sat in Editing with a dead button, and the composer of
    /// every other session stayed dead behind the same overlay until restart.
    /// The gate now reads the send's origin, and the window's own path names
    /// itself.
    #[test]
    fn the_overlay_gate_only_refuses_a_composers_own_send() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let send = production
            .split("    pub(crate) fn send_from_pane(")
            .nth(1)
            .and_then(|rest| {
                rest.split("        // A send ends that pane's offer")
                    .next()
            })
            .expect("send_from_pane");
        assert!(
            send.contains("origin: SendOrigin,"),
            "the send carries who asked for it"
        );
        assert!(
            send.contains("origin == SendOrigin::Composer"),
            "the overlay gate is keyed on the composer, not on the window"
        );
        let turn = production
            .split("    pub(crate) fn send_turn_to(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Dispatch the one turn").next())
            .expect("send_turn_to");
        assert!(
            turn.contains("SendOrigin::Window"),
            "a turn the window minted is never refused by chrome"
        );
        assert!(
            production.contains("None, SendOrigin::Composer, cx)"),
            "a pane composer's own send keeps the gate"
        );
    }

    /// The reload a settle kicks off must not wipe the card of the turn the
    /// flush just started (#2695): the live copy retires only while the leaf
    /// is idle, the same guard the window's reload keeps.
    #[test]
    fn esc_flush_reload_keeps_the_drained_turns_card() {
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let reload = production
            .split("    pub(crate) fn reload_pane(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Send one turn to the session").next())
            .expect("reload_pane");
        let guarded = reload
            .split("if !pane.busy {")
            .nth(1)
            .expect("the reload guards the live clear with the busy flag");
        assert!(
            guarded.trim_start().starts_with("pane.live.clear();"),
            "the clear sits inside the guard, so a drained turn keeps its card"
        );
    }

    /// The flush leaves a readable trail (#2695): every push and take names
    /// its session and count, both drains log the joined send, and a pane
    /// stop names what is still waiting. A live first-only repro is then a
    /// log read, not a guess.
    #[test]
    fn esc_flush_receipts_log_the_queue_flow() {
        let bridge = include_str!("agent_bridge.rs");
        let push = bridge
            .split("pub fn queue_push(")
            .nth(1)
            .and_then(|rest| rest.split("/// What is waiting").next())
            .expect("queue_push");
        assert!(
            push.contains("crate::logging::info("),
            "a push leaves a receipt"
        );
        let take = bridge
            .split("pub fn queue_take(")
            .nth(1)
            .and_then(|rest| rest.split("/// The newest message").next())
            .expect("queue_take");
        assert!(
            take.contains("crate::logging::info("),
            "a take leaves a receipt"
        );
        let src = DASHBOARD;
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let window_drain = production
            .split("    pub(crate) fn drain_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Create a conversation").next())
            .expect("drain_pending");
        assert!(
            window_drain.contains("queue flush: session"),
            "the window's flush logs its joined send"
        );
        let pane_drain = production
            .split("    pub(crate) fn drain_pane_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
            .expect("drain_pane_pending");
        assert!(
            pane_drain.contains("pane queue flush: session"),
            "the pane's flush logs its joined send"
        );
        let stop = production
            .split("    pub(crate) fn cancel_focused_pane_turn(")
            .nth(1)
            .and_then(|rest| rest.split("    /// `/stop`").next())
            .expect("cancel_focused_pane_turn");
        assert!(
            stop.contains("agent_bridge::queue_snapshot(id).len()"),
            "a pane stop names how much is still queued"
        );
    }

    /// The queue keeps to two rows until the user opens it, so a burst of
    /// background reports cannot own the conversation's column (#2629).
    #[test]
    fn the_queue_strip_folds_to_two_rows_until_it_is_opened() {
        assert_eq!(queue_rows(0, false), 0);
        assert_eq!(queue_rows(2, false), 2);
        assert_eq!(queue_rows(5, false), 2);
        assert_eq!(queue_rows(5, true), 5);
    }

    #[test]
    fn a_stale_pane_focus_hands_the_keyboard_back_to_the_chat() {
        // #2620: the id outlives the leaf it names. Read literally it marked
        // every pane unfocused, so no frame painted a composer and a Finder
        // drop had nothing to land in.
        let chat = Uuid::new_v4();
        let other = Uuid::new_v4();
        let leaves = [chat, other];
        assert_eq!(
            resolve_split_focus(&leaves, Some(Uuid::new_v4()), Some(chat)),
            None,
            "a stale id falls back to the chat leaf"
        );
        assert_eq!(
            resolve_split_focus(&leaves, Some(other), Some(chat)),
            Some(other),
            "a live id keeps the pane holding the caret"
        );
        assert_eq!(
            resolve_split_focus(&leaves, Some(chat), Some(chat)),
            None,
            "the chat leaf names itself None"
        );
    }

    #[test]
    fn a_stale_pane_focus_picks_a_surviving_leaf_when_the_chat_is_gone() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let leaves = [a, b];
        assert_eq!(
            resolve_split_focus(&leaves, Some(Uuid::new_v4()), None),
            Some(a),
            "the first surviving leaf owns the keyboard"
        );
    }

    #[test]
    fn one_leaf_is_never_a_split() {
        let only = Uuid::new_v4();
        assert_eq!(resolve_split_focus(&[only], Some(only), Some(only)), None);
        assert_eq!(resolve_split_focus(&[], Some(only), Some(only)), None);
    }

    #[test]
    fn the_pane_footer_never_repeats_the_running_task_strip() {
        // #2620: the strip above the composer is the single source (#2617).
        // Printing the roster again in the one-line footer ran it past the
        // pane's edge and over its neighbour.
        let src = DASHBOARD;
        let footer = src
            .split("fn pane_footer_text(")
            .nth(1)
            .and_then(|rest| rest.split("\n    pub(crate) fn ").next())
            .expect("pane footer text");
        assert!(
            !footer.contains('\u{23f3}') && !footer.contains('\u{231b}'),
            "the footer carries no task strip"
        );
    }

    #[test]
    fn the_pane_footer_clips_instead_of_overflowing() {
        // The row moved to its own module (#2817 follow-up), so the scan
        // follows it there.
        let src = include_str!("pane_chrome.rs");
        let footer = src
            .split("fn pane_footer(")
            .nth(1)
            .and_then(|rest| rest.split("\n/// ").next())
            .expect("pane footer");
        assert!(footer.contains(".max_w_full()"), "the row is bounded");
        assert!(footer.contains(".overflow_hidden()"), "the row clips");
        assert!(
            footer.contains(".whitespace_nowrap()"),
            "the row is one line"
        );
        assert!(footer.contains(".truncate()"), "the text ellipsizes");
    }

    #[test]
    fn a_topic_tab_shows_two_words_and_elides_the_rest() {
        assert_eq!(topic_label("Refactor the split"), "Refactor the\u{2026}");
        assert_eq!(topic_label("Two words"), "Two words");
        assert_eq!(topic_label("OpenCrabs"), "OpenCrabs");
        // Runs of whitespace collapse instead of becoming empty words, so a
        // double space does not eat the second word's slot.
        assert_eq!(topic_label("  too   many   spaces  "), "too many\u{2026}");
        // The tab is shortened, the name is not: the rest of the window still
        // shows the whole thing (#2477).
        let stored = String::from("Refactor the split pane");
        assert_eq!(topic_label(&stored), "Refactor the\u{2026}");
        assert_eq!(stored, "Refactor the split pane");
    }

    #[test]
    fn the_help_screen_paints_one_row_per_entry() {
        use opencrabs::ui_core::help_keys::{self, HelpSection};

        // One painted row per entry of the collection the window reads, and
        // no row the window cannot answer. A blank chord or a blank
        // description is a row that costs space and says nothing.
        let painted = help_rows();
        let shared: Vec<_> = help_keys::gui_rows().collect();
        assert_eq!(
            painted.len(),
            shared.len(),
            "the overlay must paint every shared row with a window chord"
        );
        assert!(
            painted.len() >= 8,
            "the window answers more chords than the {} listed",
            painted.len()
        );
        for (section, row) in &painted {
            let chord = row.gui.expect("every painted row has a chord");
            assert!(!chord.is_empty(), "{} has no chord", row.description);
            assert!(!row.description.is_empty(), "{chord} explains nothing");
            assert_eq!(*section, row.section, "{chord} is under the wrong heading");
        }
        // Sections print in the order the shared table declares them, and a
        // heading never repeats mid-list.
        let mut seen: Vec<HelpSection> = Vec::new();
        for (section, _) in &painted {
            if seen.last() != Some(section) {
                seen.push(*section);
            }
        }
        let declared: Vec<HelpSection> = HelpSection::ALL
            .into_iter()
            .filter(|s| seen.contains(s))
            .collect();
        assert_eq!(seen, declared, "a section printed twice or out of order");
        // The door the panel hangs off is itself one of the rows (#2320).
        assert_eq!(help_keys::find("help").and_then(|r| r.gui), Some("ctrl-h"));
    }

    #[test]
    fn help_lists_every_rail_chord() {
        use opencrabs::ui_core::help_keys::{self, HelpSection};

        // The rail's commands are only discoverable if the table lists
        // them: each one appears exactly once, under SESSIONS, with a
        // window chord (#2355). ctrl+u is absent on purpose: the usage
        // ledger already documents it once, in GLOBAL. ctrl+s likewise:
        // GLOBAL's "List sessions" row is that command. Projects uses alt+p
        // so the rail can keep bare p for its filter (#2669, #2812). z stays
        // absent since the fold tree became chips (#2701).
        let wanted = [
            ("up/down", "Move the highlight in the list"),
            ("enter", "Open the highlighted session"),
            ("ctrl+f", "Find a session"),
            ("ctrl+shift+p", "Pin the selected session"),
            ("ctrl+d", "Delete session (asks first)"),
            ("ctrl+g", "Session files"),
            ("p", "Browse projects"),
            ("esc", "Cancel or step back"),
        ];
        let sessions: Vec<_> = help_keys::section_rows(HelpSection::Sessions).collect();
        for (chord, description) in wanted {
            let hits: Vec<_> = sessions.iter().filter(|row| row.neutral == chord).collect();
            assert_eq!(hits.len(), 1, "{chord} must be listed exactly once");
            let row = hits[0];
            assert_eq!(row.description, description, "{chord} changed its words");
            assert_eq!(
                row.gui,
                Some(match chord {
                    "up/down" => "up/down",
                    "ctrl+f" => "ctrl-f",
                    "ctrl+shift+p" => "ctrl-shift-p",
                    "ctrl+d" => "ctrl-d",
                    "ctrl+s" => "ctrl-s",
                    "ctrl+g" => "ctrl-g",
                    "p" => "alt-p",
                    _ => chord,
                }),
                "{chord} must carry the window's spelling"
            );
        }
        // r, t and c are terminal-only now (#2812): the window spends its bare
        // characters on the rail's find field, so a typed name cannot double as
        // a command. Each row stays in the table, with no window chord, so the
        // terminal keeps its keys and the window points at the row menu.
        for (chord, description) in [
            ("r", "Rename the selected session"),
            ("t", "Topics on, then a new topic"),
            ("c", "Close or reopen the selected topic"),
        ] {
            let hits: Vec<_> = sessions.iter().filter(|row| row.neutral == chord).collect();
            assert_eq!(hits.len(), 1, "{chord} must be listed exactly once");
            let row = hits[0];
            assert_eq!(row.description, description, "{chord} changed its words");
            assert_eq!(row.gui, None, "{chord} must not advertise a window chord");
        }
        // Rows with a window chord are the rail's commands, and those are what
        // must match the list above. Terminal-only rows (the topic keys in
        // /sessions) carry no window chord and are checked by their own tests.
        let rail: Vec<_> = sessions.iter().filter(|row| row.gui.is_some()).collect();
        assert_eq!(rail.len(), wanted.len(), "SESSIONS lists nothing else");
    }

    #[test]
    fn a_typed_character_on_the_rail_opens_the_filter() {
        // A source scan, because the handler needs a window and a key event to
        // exercise. The rule here is wiring (#2812): the first printable
        // character arms the field and types itself, and the window no longer
        // spends bare letters on commands that would swallow it.
        let src = DASHBOARD;
        let block = src
            .split_once("if self.screen == Screen::Chat && self.rail_open {")
            .expect("the rail key block moved")
            .1;
        let block = block
            .split_once("if self.screen == Screen::Chat {")
            .expect("the rail key block lost its end")
            .0;
        assert!(
            block.contains("_ if self.rail_focused =>"),
            "a typed character must reach the rail's find field"
        );
        assert!(
            block.contains("self.arm_rail_filter(cx);"),
            "typing must arm the same field ctrl+f arms"
        );
        assert!(
            block.contains("self.sessions.filter.push(ch);"),
            "the character that opened the field must also land in it"
        );
        for key in ["\"r\"", "\"t\"", "\"c\""] {
            assert!(
                !block.contains(&format!("{key} if self.rail_focused")),
                "{key} must not answer bare on the window's rail"
            );
        }
    }

    #[test]
    fn the_help_screen_opens_on_ctrl_h_and_cmd_question() {
        // A source scan, because the handler needs a window and a key event
        // to exercise. The rule here is wiring: both doors land on the same
        // toggle, Esc closes the panel before it reaches anything behind it,
        // and the panel reads the shared table instead of its own copy of the
        // chords.
        // The two doors (ctrl+h and Cmd+?) are pinned where the chords
        // live, in `screen_keys`; this pins the wiring from that table to
        // the panel, the half that lives in this file (#2356).
        let src = DASHBOARD;
        assert!(
            src.contains("ScreenAction::ToggleHelp => self.toggle_help(cx)"),
            "the chord table's help action must land on toggle_help"
        );
        assert!(src.contains("fn toggle_help"));
        assert!(
            src.contains("self.help_open = false;"),
            "Esc must close the panel"
        );
        assert!(src.contains("help_keys::section_rows"));
        assert!(src.contains("filter(|row| row.gui.is_some())"));
        // The panel's body moved to its own module (#2817 follow-up) and then
        // to the card view (#2893), so the row loop is read where it is
        // written.
        let panel = include_str!("help_view.rs");
        assert!(
            panel.contains("help_rows()"),
            "the renderer must paint the list the test checks"
        );
    }

    /// The seeker's pick has exactly three meanings, and none of them is a
    /// silent drop (#2604, #2605).
    #[test]
    fn topic_pick_names_the_three_cases() {
        assert_eq!(topic_pick(true, true), TopicPick::Warn);
        assert_eq!(topic_pick(true, false), TopicPick::Warn);
        assert_eq!(topic_pick(false, true), TopicPick::Draft);
        assert_eq!(topic_pick(false, false), TopicPick::EnableThenDraft);
    }

    /// The arm must route all three cases: a warning, a prompt, and the flag
    /// write that ends in the same prompt. The needles are split so the scan
    /// cannot count its own source text.
    #[test]
    fn seeker_topic_pick_never_drops_silently() {
        let src = DASHBOARD;
        assert!(
            src.contains(&["TopicPick::", "EnableThenDraft => {"].concat()),
            "a conversation with topics off must take the enabling path"
        );
        assert!(
            src.contains(&["self.enable_topics", "_then(id,"].concat()),
            "the enabling path must write the flag before opening the prompt"
        );
        assert!(
            src.contains(&["self.topic_warning = Some(", "title);"].concat()),
            "a topic pick must arm the warning with the row it named"
        );
        assert!(
            src.contains("OpenCrabs - Warning"),
            "the warning carries the header bar the report asked for"
        );
    }

    #[test]
    fn delete_dialog_answers_enter_and_escape() {
        // The confirmation is a modal with two answers, so the keyboard gets
        // both (#2456). It sits before the Esc ladder, because a dialog that
        // owns the keyboard has to answer every key itself, and `delete` runs
        // from the same branch the button's click runs.
        let src = DASHBOARD;
        let branch = [
            "if let Some(id) = self.confirm_delete {\n            match ev.keystroke.key.as_str() {\n                \"enter\" => {",
            "self.delete_session(id, cx);",
        ];
        assert!(
            src.contains(branch[0]),
            "Enter must confirm the delete dialog"
        );
        assert!(
            src.contains(branch[1]),
            "the confirm path must delete the row it names"
        );
        let gate = src
            .find(branch[0])
            .expect("the dialog branch is in the handler");
        let esc_ladder = src
            .find("if ev.keystroke.key == \"escape\" {")
            .expect("the Esc ladder is in the handler");
        assert!(
            gate < esc_ladder,
            "the dialog answers before the ladder, not after it"
        );
    }

    #[test]
    fn seeker_owns_the_keyboard_before_the_composer_does() {
        // The card is only a launcher if it gets the characters: its branch
        // must run before the composer gate, or a focused editor types into
        // itself what the user meant for the field (#2458).
        let src = DASHBOARD;
        let hook = src
            .find("self.finder_key(ks, cx)")
            .expect("the card must answer keys in on_key");
        let gate = src
            .find("if self.composer.editor.read(cx).focus_handle.is_focused(window)")
            .expect("the composer gate is in the handler");
        assert!(
            hook < gate,
            "the card reads keys before the composer, not after it"
        );
        for needle in [
            "fn finder_key",
            "fn open_finder",
            "fn run_finder_pick",
            "fn finder_items",
            "OverlayId::CommandPalette",
        ] {
            assert!(src.contains(needle), "{needle} is part of the wiring");
        }
    }

    #[test]
    fn the_seeker_hint_is_visible_and_clickable() {
        // #2747: the chord that opens the Seeker was invisible unless you
        // already knew it. The chat screen draws no header (#2596), so the
        // hint sits on the footer row that already carries the session name,
        // and it is a click target, not a hover tooltip: hover-only hints are
        // invisible to exactly the people who need them.
        let src = DASHBOARD;
        let start = src
            .find("id(\"seeker-hint\")")
            .expect("the chat footer carries the Seeker hint");
        let block = src
            .get(start..)
            .expect("find returns a char boundary, so the tail is sliceable");
        let end = block
            .find(".child(\"\u{b7} seek\")")
            .expect("the hint names what it opens");
        let hint = block
            .get(..end)
            .expect("find returns a char boundary, so the head is sliceable");
        assert!(
            hint.contains(".on_click(cx.listener("),
            "the hint must be a click target, not plain text"
        );
        assert!(
            hint.contains("this.open_finder(cx)"),
            "clicking the hint must open the Seeker"
        );
        assert!(
            hint.contains("seeker_chord_label()"),
            "the chord printed must come from the shared label, not a literal"
        );
    }

    #[test]
    fn tui_chords_bind_the_window_like_the_terminal() {
        // The three commands the window spelled differently: new session,
        // sessions, fold newest turn. The terminal's chord is the one chord,
        // because the window has to answer on every platform and the command
        // key only exists on a Mac (#2319). ctrl+l stays as the alias the
        // session list has always answered.
        let src = DASHBOARD;
        for needle in [
            ["ev.keystroke.key == ", "\"n\""].concat(),
            ["modifiers.control && (ev.keystroke.key == ", "\"s\""].concat(),
            ["ev.keystroke.key == ", "\"o\""].concat(),
        ] {
            assert!(
                src.contains(&needle),
                "the window must answer {needle} as the terminal does"
            );
        }
        // And the command key answers none of the three, so no habit rides a
        // chord that only exists on one platform.
        for key in ["n", "o", "s"] {
            let banned = format!("modifiers.platform && ev.keystroke.key == \"{key}\"");
            assert!(
                !src.contains(&banned),
                "the command key must not be the chord for {key}"
            );
        }
    }

    #[test]
    fn chat_footer_points_at_the_seeker() {
        // The footer stopped being a hand-maintained keymap (#2321). It carries
        // live state (provider, model, directory, branch, tokens per second,
        // ctx budget), the door to the Seeker card, and the sidebar chord
        // (#2877): ctrl+s opens and closes the rail, and a user who does not
        // know the chord had no on-screen way to learn it. The rest of the
        // retired strip taught keys the window no longer binds and went stale
        // the moment a binding moved, so it stays banned below.
        let src = DASHBOARD;
        // The chat footer prints the default chord beside that door (#2894),
        // so the static sentence now stands on the two screens that have no
        // chord of their own to print.
        let door = ["\"alt+space · ", "seek help\""].concat();
        assert_eq!(
            src.matches(&door).count(),
            2,
            "usage and files carry the static Seeker door"
        );
        assert!(
            src.contains("palette::seeker_chord_label()"),
            "the chat door names the chord the window answers"
        );
        let sidebar = ["\"ctrl+s · ", "sidebar\""].concat();
        assert_eq!(
            src.matches(&sidebar).count(),
            3,
            "chat, usage and files each advertise the sidebar's real chord"
        );
        assert!(
            !src.contains(&["back to ", "chat"].concat()),
            "no footer advertises a chord the window does not bind"
        );
        // None of the eight entries of the retired strip survives. The needles
        // are split so the scan cannot count this test's own source text.
        let retired = [
            ["cmd-u · ", "usage"],
            ["cmd-f · ", "find chat"],
            ["cmd-g · ", "files"],
            ["cmd-n · ", "new chat"],
            ["cmd-o · ", "fold turn"],
            ["cmd-b · ", "sidebar"],
            ["cmd-shift-f · ", "pin"],
            ["cmd-⌫ · ", "delete chat"],
        ];
        for parts in retired {
            let needle = parts.concat();
            let quoted = format!("\"{needle}\"");
            assert!(
                !src.contains(&quoted),
                "the footer must not advertise {needle} any more"
            );
        }
        // The identity line survives: it is live state, which the strip was
        // not. It lives in `status.rs` now (#2817 follow-up), so the scan names
        // that file. The ctx budget that used to sit beside it is gone on
        // purpose (#2628): the tool block's own footer already prints it.
        assert!(include_str!("status.rs").contains("fn status_identity"));
    }

    /// The manager is born in ONE setter, and the bridge calls it. A second
    /// call site would mean two construction paths, which is how the two
    /// surfaces drifted before (#2322). The needle is built in halves so this
    /// test's own source does not count itself.
    #[test]
    fn the_bridge_creates_the_background_manager() {
        let src = include_str!("agent_bridge.rs");
        let needle = [".with_message_enqueue", "_callback(Some("].concat();
        let sites = src.match_indices(needle.as_str()).count();
        assert_eq!(
            sites, 1,
            "exactly one manager setter, found {sites}: the GUI had none"
        );
        let head = src
            .get(..src.find(needle.as_str()).expect("the call site exists"))
            .expect("a call site is a char boundary");
        assert!(
            head.contains("fn build"),
            "the setter belongs to the bridge's build fn"
        );
    }

    /// A completion on a session no channel owns has to have somewhere to
    /// report, which is what the local route is for (#1036). One call only.
    #[test]
    fn the_bridge_registers_the_local_route() {
        // One route for the whole window (#1036), counted in the shipped
        // source: a test that names the call in its own body is not a second
        // registration, and counting the whole file made the result depend on
        // how a test writes its needle (#2673).
        let src = include_str!("agent_bridge.rs");
        let shipped = src
            .split("#[cfg(test)]")
            .next()
            .expect("the shipped source before the tests");
        let needle = ["register_local", "_route("].concat();
        let sites = shipped.match_indices(needle.as_str()).count();
        assert_eq!(sites, 1, "one local route registration, found {sites}");
    }

    /// Two sessions on the rail, newest first, the way `SessionRepository::list`
    /// returns them (#2444).
    fn rail() -> Vec<opencrabs::db::models::Session> {
        let newest = opencrabs::db::models::Session::new(Some("newest".into()), None, None);
        let older = opencrabs::db::models::Session::new(Some("older".into()), None, None);
        vec![newest, older]
    }

    /// #2444: a launch opens the conversation the user was in, not whatever the
    /// rail happens to list first. A pointer at a session that is gone, or no
    /// pointer at all, falls back to the newest instead of opening nothing.
    #[test]
    fn a_launch_prefers_the_last_session_and_falls_back_to_the_newest() {
        let items = rail();
        let newest = items[0].id;
        let older = items[1].id;

        assert_eq!(
            pick_initial_session(&items, Some(older)),
            Some(older),
            "the conversation in flight last time wins over the rail's order"
        );
        assert_eq!(
            pick_initial_session(&items, None),
            Some(newest),
            "a first run opens the newest"
        );
        assert_eq!(
            pick_initial_session(&items, Some(uuid::Uuid::new_v4())),
            Some(newest),
            "a pointer at a session that no longer exists falls back, never blanks the pane"
        );
        assert_eq!(
            pick_initial_session(&[], Some(older)),
            None,
            "an empty rail opens nothing rather than inventing an id"
        );
    }

    /// #2444: opening a conversation records it for the next launch, through
    /// the one helper both surfaces share. A scratch home keeps the owner's own
    /// pointer out of `cargo test`.
    #[test]
    fn opening_a_session_remembers_it_for_the_next_launch() {
        let home = std::env::temp_dir().join(format!(
            "opencrabs-gui-last-session-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).expect("scratch home");
        let id = uuid::Uuid::new_v4();
        opencrabs::config::profile::with_home_override(home, || {
            remember_open_session(id);
            assert_eq!(
                opencrabs::utils::last_session::read(),
                Some(id),
                "the pointer the next launch reads names this conversation"
            );
        });

        // And the wiring: `open_session` is the door every open goes through.
        // Since #2452 the door delegates to `begin_conversation` and the
        // record lives one layer down there, so both links of the composed
        // path are pinned: the door routes through the helper and the helper
        // does the recording. Halves, so this test cannot match itself.
        let src = DASHBOARD;
        let call = ["remember_open", "_session(id)"].concat();
        let delegate = ["begin_conversation", "(id, cx)"].concat();
        let (_, after) = src
            .split_once(
                ["fn open_", "session(&mut self, id: Uuid"]
                    .concat()
                    .as_str(),
            )
            .expect("open_session exists");
        let (door, _) = after
            .split_once("\n    pub(crate) fn ")
            .expect("the next method ends it");
        // #2665 split the door in two: `open_session` commits and
        // `open_session_handing_back` makes the keyboard decision. The open
        // still lands in `begin_conversation`, one link further down.
        let handing_back = ["open_session_", "handing_back(id, true, cx)"].concat();
        assert!(
            door.contains(&handing_back),
            "open_session commits through open_session_handing_back"
        );
        let (_, back) = src
            .split_once(["fn open_session_", "handing_back("].concat().as_str())
            .expect("open_session_handing_back exists");
        let (back_body, _) = back
            .split_once("\n    pub(crate) fn ")
            .expect("the next method ends it");
        assert!(
            back_body.contains(&delegate),
            "open_session_handing_back routes through begin_conversation"
        );
        let (_, helper) = src
            .split_once(["fn begin_", "conversation(&mut self"].concat().as_str())
            .expect("begin_conversation exists");
        let (recording, _) = helper
            .split_once("\n    pub(crate) fn ")
            .expect("the next method ends it");
        assert!(
            recording.contains(&call),
            "begin_conversation records the conversation it opens"
        );
    }

    /// The window and every split pane walk ONE history, and it is the file
    /// the terminal reads, so recall shows the same past whichever composer
    /// has the caret (#2494).
    #[test]
    fn gui_composer_history_is_one_shared_list() {
        let src = DASHBOARD;
        let shared = ["composer::new_shared_", "history("].concat();
        assert!(src.contains(&shared), "the window loads one shared list");
        let pane = ["editor::Editor::new_with_", "history(history"].concat();
        assert!(
            src.contains(&pane),
            "a split pane takes the window's handle, never a private list"
        );
        let field = ["history: composer::", "SharedHistory,"].concat();
        assert!(src.contains(&field), "the dashboard owns the handle");
    }

    /// Switching conversations files the draft under the one being left and
    /// puts the destination's own draft back. A pane whose editor is dropped
    /// files its text before it goes (#2494).
    #[test]
    fn gui_switching_conversations_keeps_the_draft() {
        let src = DASHBOARD;
        let (_, helper) = src
            .split_once(&["fn begin_", "conversation(&mut self, id: Uuid"].concat())
            .expect("begin_conversation exists");
        let (body, _) = helper
            .split_once("\n    pub(crate) fn ")
            .expect("the next method ends it");
        let filed = ["self.composer.drafts.", "store(previous, draft)"].concat();
        assert!(body.contains(&filed), "leaving files the draft");
        let back = ["self.composer.drafts.", "take(id)"].concat();
        assert!(body.contains(&back), "arriving takes the stored draft");
        let restore = ["ed.restore_", "state(draft, cx)"].concat();
        assert!(body.contains(&restore), "the returned draft is put back");
        let pane = ["self.stash_pane_", "draft(session, cx)"].concat();
        assert!(
            src.contains(&pane),
            "a closing pane files its draft before its editor is dropped"
        );
    }

    /// The completion event resumes the session that started the task, never
    /// only the conversation on screen (#2704). Halves again: the file this
    /// reads includes this test.
    #[test]
    fn the_completion_event_resumes_its_own_session() {
        let src = DASHBOARD;
        let event = ["TurnEvent::", "BackgroundTaskDone"].concat();
        assert!(
            src.contains(&event),
            "the poll loop must handle the completion event"
        );
        let arm = src.split(&event).nth(1).expect("the completion arm");
        let arm = arm
            .split("agent_bridge::TurnEvent::")
            .next()
            .expect("the end of the arm");
        assert!(
            arm.contains("self.running_here(session_id, cx)"),
            "a session already working takes the receipt in its queue"
        );
        assert!(
            arm.contains("self.resume_off_screen_task(session_id"),
            "a completion for a conversation nobody is looking at starts its turn"
        );
        assert!(
            !arm.contains("background task finished on session"),
            "the receipt is resumed, not logged away"
        );
    }

    /// A conversation longer than the display window still settles (#2705).
    ///
    /// The old expectation was `turns.len() + 1`, taken from the newest 200
    /// rows: past that the count stops growing, so a real turn could never be
    /// seen to land and its ctx and rate were thrown away. Measured on the
    /// session's row total, the same turn lands on the first reload.
    #[test]
    fn a_session_past_the_window_still_settles() {
        let rows_before = 335usize;
        let expect = Some(rows_before + 1);
        assert!(
            !settle_landed(expect, rows_before as i64),
            "the turn's row is not in the ledger yet"
        );
        assert!(
            settle_landed(expect, (rows_before + 1) as i64),
            "the row lands on the first reload, however long the conversation is"
        );
        assert!(
            settle_landed(None, 0),
            "a reload with nothing to wait for is never short"
        );
    }

    /// The /cd picker's listing (#2527): the parent entry first, then the
    /// subdirectories sorted by name, files never listed.
    #[test]
    fn cd_entries_list_parent_dirs_only_sorted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir(root.join("zebra")).expect("zebra");
        std::fs::create_dir(root.join("alpha")).expect("alpha");
        std::fs::write(root.join("file.txt"), "x").expect("file");

        let entries = cd_entries_for(root, false);
        // The parent row is a `..`-terminated path, which Rust's file_name()
        // reports as None: name it directly instead of unwrapping.
        let names: Vec<String> = entries
            .iter()
            .map(|p| {
                if p.ends_with("..") {
                    "..".to_string()
                } else {
                    p.file_name().unwrap().to_string_lossy().to_string()
                }
            })
            .collect();
        // Parent first, then children alphabetical, files skipped.
        assert_eq!(names.first().map(String::as_str), Some(".."));
        assert_eq!(
            names,
            vec!["..".to_string(), "alpha".to_string(), "zebra".to_string()]
        );
    }

    /// Dotfile directories stay hidden until the `.` toggle turns them on,
    /// the same rule the terminal's dialog uses (#2527).
    #[test]
    fn cd_entries_hide_dotfiles_until_toggled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir(root.join(".secret")).expect("secret");
        std::fs::create_dir(root.join("plain")).expect("plain");

        let hidden_off = cd_entries_for(root, false);
        let names_off: Vec<String> = hidden_off
            .iter()
            .filter(|p| !p.ends_with(".."))
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            !names_off.contains(&".secret".to_string()),
            "dotfile dirs are hidden by default"
        );
        let hidden_on = cd_entries_for(root, true);
        let names_on: Vec<String> = hidden_on
            .iter()
            .filter(|p| !p.ends_with(".."))
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            names_on.contains(&".secret".to_string()),
            "the toggle lists them"
        );
    }

    /// The window's /cd wiring (#2527), read off the source because a bare
    /// `/cd` needs a live window to open the screen: bare `/cd` opens the
    /// picker (never a spawned text command, so no thinking spinner), the
    /// keys match the terminal's dialog, and select-here returns to chat
    /// before the apply task spawns so a slow write cannot hold the screen.
    #[test]
    fn bare_cd_opens_the_picker_screen_and_keys_match_the_terminal() {
        let src = DASHBOARD;
        // Bare `/cd` opens the screen in run_text_command, with no turn
        // thread in the arm: that thread was the phantom spinner (#2527).
        let (_, after) = src
            .split_once(["C::Cd => {", "\n"].concat().as_str())
            .expect("run_text_command handles Cd");
        // Bound the arm at its own closing brace: everything to end-of-file
        // would sweep in later arms that legitimately spawn text commands.
        let (cd_arm, _) = after.split_once("\n            }").unwrap_or((after, ""));
        assert!(
            cd_arm.contains("self.open_cd_picker(cx)"),
            "bare /cd opens the picker screen"
        );
        let (bare, rest) = cd_arm
            .split_once("self.open_cd_picker(cx)")
            .expect("the bare arm exists");
        assert!(
            !bare.is_empty() && rest.contains("apply_operator_cd"),
            "with a path the change applies locally on the service"
        );
        assert!(
            !cd_arm.contains("spawn_text_command"),
            "no text-command thread in the /cd arm, so no spinner"
        );
        // The screen owns the keyboard with the terminal's bindings.
        assert!(
            src.contains("\"tab\" | \"space\" => self.cd_select_here(cx),"),
            "Tab and Space select here, as in the terminal"
        );
        assert!(
            src.contains("\"enter\" => self.cd_descend(self.cd_selected.min(last), cx),"),
            "Enter descends, as in the terminal"
        );
        // Esc cancels from the screen, alongside the other screens.
        assert!(
            src.contains("if self.screen == Screen::DirectoryPicker {")
                && src.contains("self.close_cd_picker(cx);"),
            "Esc cancels the picker"
        );
        // Select-here switches to chat FIRST, the ordering #2526 just fixed
        // on the terminal: a slow apply must never hold the screen open.
        let (_, after) = src
            .split_once("fn cd_select_here")
            .expect("cd_select_here exists");
        let (body, _) = after
            .split_once("\n    pub(crate) fn ")
            .unwrap_or((after, ""));
        let switch = body
            .find("self.screen = Screen::Chat;")
            .expect("select switches to chat");
        let spawn = body.find("cx.spawn").expect("select applies in a task");
        assert!(
            switch < spawn,
            "the conversation returns before the apply task spawns"
        );
    }

    /// The terminal's Ctrl+C ladder, as a decision (#2308): scrolled up the
    /// press only gives the bottom back, and it never costs the window.
    #[test]
    fn ctrl_c_gives_the_bottom_back_before_it_takes_the_window() {
        let now = std::time::Instant::now();
        let armed = Some(now - std::time::Duration::from_secs(1));
        for pending in [None, armed] {
            assert_eq!(
                super::ctrl_c_action(false, true, pending, now),
                super::CtrlCAction::SnapBottom,
                "a reader scrolled up gets the bottom, never the quit"
            );
            assert_eq!(
                super::ctrl_c_action(false, false, pending, now),
                super::CtrlCAction::SnapBottom,
                "and a draft in the field does not change that"
            );
        }
    }

    /// The first press throws the draft away and arms; the second inside the
    /// three second window leaves (#2308).
    #[test]
    fn ctrl_c_quits_on_a_second_press_inside_the_window_only() {
        let now = std::time::Instant::now();
        assert_eq!(
            super::ctrl_c_action(true, false, None, now),
            super::CtrlCAction::ClearAndArm,
            "nothing armed yet: the press only clears"
        );
        assert_eq!(
            super::ctrl_c_action(
                true,
                true,
                Some(now - std::time::Duration::from_secs(1)),
                now
            ),
            super::CtrlCAction::Quit,
            "a second press one second later quits"
        );
        assert_eq!(
            super::ctrl_c_action(true, true, Some(now - super::QUIT_WINDOW), now),
            super::CtrlCAction::ClearAndArm,
            "the window is three seconds wide and it is spent at the edge"
        );
        assert_eq!(
            super::ctrl_c_action(
                true,
                true,
                Some(now + std::time::Duration::from_secs(1)),
                now
            ),
            super::CtrlCAction::ClearAndArm,
            "a clock that moved backwards never quits"
        );
    }

    /// Text typed between the two presses is a fresh start, never a
    /// confirmation: the press throws it away and arms again (#2308).
    #[test]
    fn ctrl_c_clears_a_draft_typed_since_the_first_press() {
        let now = std::time::Instant::now();
        assert_eq!(
            super::ctrl_c_action(
                true,
                false,
                Some(now - std::time::Duration::from_secs(1)),
                now
            ),
            super::CtrlCAction::ClearAndArm,
            "a draft that came back is thrown away, not read as consent"
        );
    }

    /// The branch sits ahead of the composer gate, so the key answers while
    /// the caret is in the field (#2308).
    #[test]
    fn ctrl_c_branch_runs_ahead_of_the_composer_gate() {
        let src = DASHBOARD;
        let branch = src
            .find("self.ctrl_c_press(cx);")
            .expect("the Ctrl+C branch is in on_key");
        let gate = src
            .find("if self.composer.editor.read(cx).focus_handle.is_focused(window)")
            .expect("the composer gate is in on_key");
        assert!(
            branch < gate,
            "Ctrl+C is judged before the editor takes the keyboard"
        );
    }

    /// The composer's own ctrl-c binding would take the key before any window
    /// listener, so an interceptor hands it over first, and a selection still
    /// reaches the copy chord (#2308, #2587).
    #[test]
    fn ctrl_c_reaches_the_window_ahead_of_the_copy_chord() {
        let src = DASHBOARD;
        assert!(
            src.contains("cx.intercept_keystrokes("),
            "the quit lane is judged before keymap dispatch"
        );
        assert!(
            src.contains("cx.stop_propagation();"),
            "the press is handed to the window instead of the copy action"
        );
        assert!(
            src.contains("editor.has_selection()"),
            "a composer selection keeps its copy"
        );
        assert!(
            include_str!("editor.rs").contains("KeyBinding::new(\"ctrl-c\", Copy, None)"),
            "the editor still binds the key to copy"
        );
    }

    /// A pane that switches conversation keeps the turn it was running (#2854).
    /// The switch used to file the draft and drop the whole view, so the live
    /// turn lost its tool blocks, its thinking card and its budget, and no
    /// reload could bring them back. Read off the source because a switch needs
    /// a live window: each of these is one line a later edit could drop without
    /// any test noticing.
    #[test]
    fn a_pane_switch_parks_the_view_it_leaves_behind() {
        // Every needle is split in two: `include_str!` puts this test's own
        // text inside the source it scans, so a whole literal would match
        // itself right here and pin nothing at all.
        let parks = ["self.park_p", "ane("].concat();
        let drops = ["pane_states", ".remove"].concat();

        for door in ["open_topic_in_pane", "open_picked_in_focused_pane"] {
            let body = fn_body(DASHBOARD, door);
            assert!(body.contains(&parks), "{door} parks the pane it leaves");
            assert!(
                !body.contains(&drops),
                "{door} must not drop the view it leaves"
            );
        }

        // The parking lot itself: the view moves under its session and comes
        // back into the drawn map, restored rather than rebuilt.
        let park = fn_body(DASHBOARD, "park_pane");
        assert!(
            park.contains(&["parked_panes", ".insert("].concat()),
            "a parked view is kept under its session"
        );
        let open = fn_body(DASHBOARD, "open_pane_state");
        assert!(
            open.contains(&["parked_panes", ".remove(&id)"].concat()),
            "opening a pane takes the parked view back"
        );
        assert!(
            open.contains(&["pane_states", ".insert(id, view)"].concat()),
            "the parked view is re-inserted, not rebuilt"
        );
    }

    /// A turn settles where it ran, not where the user is looking (#2854).
    /// `settle_pane` returned early whenever the pane was off screen, so a turn
    /// that finished after a switch recorded no metrics, reloaded no rows and
    /// never drained its queue.
    #[test]
    fn a_pane_settles_while_it_is_off_screen() {
        let settle = fn_body(DASHBOARD, "settle_pane");
        assert!(
            settle.contains(&["self.pane_view(", "session)"].concat()),
            "settling resolves the pane, drawn or parked"
        );
        assert!(
            !settle.contains(&["pane_states", ".get(&session)"].concat()),
            "settling must not read only the drawn map"
        );
    }

    /// The leaf's live card carries its own turn (#2854): it was handed a
    /// hardcoded `false` and a default budget, so a running pane drew no live
    /// card and printed a zero window.
    #[test]
    fn a_pane_live_card_carries_its_own_turn() {
        assert!(
            PANE_VIEW.contains(&["            self.b", "usy,\n"].concat()),
            "the pane passes its own busy flag to the transcript"
        );
        assert!(
            PANE_VIEW.contains(&["ctx_used: self.ctx_u", "sed,"].concat()),
            "the pane's card reads the pane's own ctx"
        );
        assert!(
            PANE_VIEW.contains(&["ctx_max: self.ctx_m", "ax,"].concat()),
            "the pane's card reads the pane's own window"
        );
    }

    /// The pencil opens a field in the row itself. Neither send path can fall
    /// back to replacing the composer's draft or consuming a composer target.
    #[test]
    fn an_edit_is_confirmed_inside_its_own_card() {
        let messages = include_str!("dashboard_messages.rs");
        let begin = messages
            .split("fn msg_edit_begin(")
            .nth(1)
            .and_then(|rest| rest.split("/// Send the rewrite open").next())
            .expect("window edit begin");
        assert!(begin.contains("self.new_inline_editor(text, cx)"));
        assert!(begin.contains("self.edit_row = Some(chat_leaf::InlineEdit"));
        assert!(!begin.contains("self.composer.editor"));
        assert!(!begin.contains("show_editing"));
        let confirm = messages
            .split("fn msg_edit_confirm(")
            .nth(1)
            .and_then(|rest| rest.split("/// Close the rewrite").next())
            .expect("window edit confirm");
        assert!(confirm.contains("self.edit_row.take()"));
        assert!(confirm.contains("self.send_edited(open.target, text, cx)"));
        let transcript = include_str!("transcript.rs");
        assert!(transcript.contains("Some(editing) => inline_edit_box(editing)"));
        assert!(transcript.contains("msg-edit-confirm-{id}"));
        assert!(transcript.contains("msg-edit-cancel-{id}"));
        let sends = include_str!("dashboard_turns.rs");
        assert!(!sends.contains("self.composer.edit.take()"));
        assert!(!sends.contains("pane.edit.take()"));
        assert!(PANE_VIEW.contains("this.pane_edit_confirm(session, text, cx)"));
    }

    /// Copy, React, Reply and Forward are resolved from the row that was
    /// pressed, on either role and in either kind of leaf. Reply keeps the
    /// draft intact and Forward keeps the destination picker in that leaf.
    #[test]
    fn every_row_menu_uses_the_pressed_row_and_leaf() {
        let messages = include_str!("dashboard_messages.rs");
        let menu = messages
            .split("fn msg_menu_items(")
            .nth(1)
            .and_then(|rest| rest.split("/// Toggle one row").next())
            .expect("message menu");
        for action in ["CopyMarkdown", "React", "Reply", "Forward", "Select"] {
            assert!(
                menu.contains(&format!("MessageAction::{action}.label()")),
                "{action} is not offered"
            );
        }
        assert!(menu.contains("self.msg_menu_row.clone()"));
        assert!(!menu.contains("is_answer"));
        assert!(!menu.contains("row.role"));
        assert!(PANE_VIEW.contains("msg_menu_open(row.clone(), Some(session)"));
        let reply = messages
            .split("fn reply_rows(")
            .nth(1)
            .and_then(|rest| rest.split("/// The composer a reply chip").next())
            .expect("reply attachment");
        assert!(reply.contains("editor.attach_reply(author, preview, quote, cx)"));
        assert!(!reply.contains("restore_draft"));
        let forward = messages
            .split("fn forward_open(")
            .nth(1)
            .and_then(|rest| rest.split("/// Back out of the picker").next())
            .expect("forward open");
        assert!(forward.contains("self.forward_leaf = leaf"));
        assert!(PANE_VIEW.contains("win.forward_picker(cx)"));
    }

    /// History menus close on an outside press even when text selection stops
    /// the bubble event, and only the current menu's five-second timer fires.
    #[test]
    fn message_menu_dismisses_outside_and_expires_only_current_menu() {
        assert_eq!(MSG_MENU_TTL, std::time::Duration::from_secs(5));
        assert!(msg_menu_expiry_due(true, 8, 8));
        assert!(!msg_menu_expiry_due(false, 8, 8));
        assert!(!msg_menu_expiry_due(true, 9, 8));

        let messages = include_str!("dashboard_messages.rs");
        assert!(messages.contains("cx.background_executor().timer(MSG_MENU_TTL)"));
        assert_eq!(
            messages.matches("self.msg_menu_arm_timeout(cx);").count(),
            2,
            "the row menu and its reaction picker both expire"
        );
        let menu = include_str!("menu.rs");
        assert!(
            menu.contains("menu_card(id, items).on_mouse_down_out(dismiss)"),
            "outside presses must dismiss during capture, before text stops them"
        );
        let transcript = include_str!("transcript.rs");
        assert!(
            transcript
                .contains("dismissible_menu_popover(\"message-context-menu\", rows, at, dismiss)")
        );
        assert!(
            transcript.contains(
                "dismissible_menu_popover(\"react-picker\", vec![grid_row], at, dismiss)"
            )
        );
        assert!(DASHBOARD.contains("this.msg_menu_close(cx)"));
    }
}

#[cfg(test)]
mod gui {
    //! The window's own tests, under one module name so a selection can be
    //! made with `cargo test -- gui::tests`: the panel and the popup are the
    //! two places the GUI has behaviour of its own that no headless test can
    //! reach through a real window.

    mod tests {
        use super::super::Config;
        use crate::composer_view::popup_window;
        use crate::dashboard_source::DASHBOARD;

        /// Eight names is the most a popup may cover: beyond that it would
        /// hide the conversation it belongs to.
        #[test]
        fn the_list_paints_at_most_eight_rows() {
            let window = popup_window(33, 0, 8);
            assert_eq!(window, 0..8);
        }

        /// The window stays put while the selection walks its rows: the eye
        /// keeps a stable block of names instead of a list that scrolls on
        /// every arrow.
        #[test]
        fn the_window_slides_only_past_its_last_row() {
            assert_eq!(popup_window(33, 7, 8), 0..8, "last row, no slide yet");
            assert_eq!(popup_window(33, 8, 8), 1..9, "one step past: slide by one");
        }

        /// A short list does not scroll at all.
        #[test]
        fn a_short_list_never_scrolls() {
            assert_eq!(popup_window(3, 0, 8), 0..3);
            assert_eq!(popup_window(3, 2, 8), 0..3);
        }

        /// The selection is clamped rather than trusted: the list shrinks as
        /// the draft grows, and a stale index must never slice past the end.
        #[test]
        fn a_stale_selection_is_clamped() {
            assert_eq!(popup_window(4, 99, 8), 0..4);
            assert_eq!(popup_window(1, 99, 8), 0..1);
        }

        /// A one-row budget still yields one row instead of an empty range,
        /// which would paint nothing and panic on an empty slice elsewhere.
        #[test]
        fn a_zero_budget_still_shows_one_row() {
            assert_eq!(popup_window(5, 2, 0), 2..3);
        }

        /// A line starting with a slash is read before the agent gates, or
        /// `/stop` and `/help` would be dead in exactly the states that need
        /// them. Source scan, because `send_message` needs a live window.
        #[test]
        fn a_command_is_read_before_the_agent_gates() {
            let src = DASHBOARD;
            let parse = src
                .find("commands::parse_command(&text)")
                .expect("send_message parses the line");
            let offline = src
                .find("agent offline: {}")
                .expect("the offline gate exists");
            assert!(
                parse < offline,
                "the command parse must precede the agent gates"
            );
        }

        /// A blank home reaches all four capabilities the built-in provider
        /// serves, with no wizard and no key. This is what `boot_config` has
        /// to leave behind, and it is testable without a window or a network
        /// call: the shipped example carries the vision, generation and
        /// transcription models, and seeding is what puts that section on
        /// disk. Under a temp home, never the live `~/.opencrabs`.
        #[test]
        fn provisioning_a_fresh_home_reaches_all_four_capabilities() {
            let home = tempfile::tempdir().expect("a temp home");
            opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
                assert!(
                    opencrabs::config::seed::ensure_config_seeded(),
                    "a blank home gets the shipped example"
                );
                let config = Config::load().expect("the shipped example parses");
                let section = config
                    .providers
                    .opencrabs
                    .as_ref()
                    .expect("the built-in section survives seeding");
                assert_eq!(
                    section.default_model.as_deref(),
                    Some("deepseek-v4.1-flash"),
                    "text"
                );
                assert!(
                    section
                        .vision_model
                        .as_deref()
                        .is_some_and(|m| !m.is_empty()),
                    "vision"
                );
                assert!(
                    section
                        .generation_model
                        .as_deref()
                        .is_some_and(|m| !m.is_empty()),
                    "image generation"
                );
                assert!(
                    section.stt_model.as_deref().is_some_and(|m| !m.is_empty()),
                    "transcription"
                );
                // And the one call the window makes here is warranted: no
                // token on disk and no other usable provider.
                assert!(
                    opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&config),
                    "a blank home must ask the proxy for its token"
                );
            });
        }

        /// Provisioning is a one-time act: it writes the token and enables the
        /// section, and the same check that said "yes, provision this" must
        /// then say no. Without that, every start would ask the proxy again.
        ///
        /// The two writes are the ones `provision_builtin_provider` makes
        /// before it goes to the network, so this covers its effect on the
        /// config without a token request.
        #[test]
        fn provisioning_stops_asking_once_the_token_is_written() {
            let home = tempfile::tempdir().expect("a temp home");
            opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
                assert!(opencrabs::config::seed::ensure_config_seeded());
                assert!(opencrabs::config::seed::ensure_keys_seeded());
                let before = Config::load().expect("the seeded config parses");
                assert!(
                    opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&before),
                    "a blank home is the case the fix exists for"
                );

                Config::write_key("providers.opencrabs", "enabled", "true")
                    .expect("the section is enabled");
                Config::write_key("providers.opencrabs", "api_key", "test-token")
                    .expect("the token is written");

                let after = Config::load().expect("the provisioned config parses");
                assert!(
                    !opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&after),
                    "a provisioned home must not be touched again"
                );
            });
        }

        /// And the provisioned config is one the provider factory walks to the
        /// built-in provider: the same registry answer `create_provider`
        /// consumes, checked without a reactor or a request.
        #[test]
        fn a_provisioned_home_resolves_the_builtin_provider() {
            let home = tempfile::tempdir().expect("a temp home");
            opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
                assert!(opencrabs::config::seed::ensure_config_seeded());
                assert!(opencrabs::config::seed::ensure_keys_seeded());
                Config::write_key("providers.opencrabs", "enabled", "true")
                    .expect("the section is enabled");
                Config::write_key("providers.opencrabs", "api_key", "test-token")
                    .expect("the token is written");

                let config = Config::load().expect("the provisioned config parses");
                let (provider, model) = config.providers.active_provider_and_model();
                assert_eq!(provider, "opencrabs", "the built-in provider is the one");
                assert_eq!(
                    model, "deepseek-v4.1-flash",
                    "with the model the shipped section declares"
                );
            });
        }

        /// The preview gate asks about the work, not only about the rail's
        /// pane-less set: an open turn, a parked turn holding a question or a
        /// tool, or a background pane-less session each mean the newest row
        /// can still change. All three empty means nothing to read.
        #[test]
        fn a_preview_refresh_follows_any_live_work() {
            use super::super::previews_can_move;
            assert!(previews_can_move(true, false, 0), "the open turn streams");
            assert!(
                previews_can_move(false, true, 0),
                "a parked turn is working"
            );
            assert!(
                previews_can_move(false, false, 1),
                "a pane-less session runs"
            );
            assert!(
                !previews_can_move(false, false, 0),
                "with no work at all the last row cannot have moved"
            );
        }
    }
}
