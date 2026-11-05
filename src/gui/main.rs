//! OpenCrabs desktop client. Phase 1: the TUI mirrored into GPUI.
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
//! for a session other than the open one is logged, not resumed: the window
//! runs one conversation at a time.

mod agent_bridge;
mod attachment_view;
mod bang;
mod breakdown;
mod command_palette;
mod commands;
mod editor;
mod icon;
mod logging;
mod markdown;
mod mission_control;
mod model_fetch;
mod model_picker;
mod models;
mod onboard;
mod palette;
mod plan_strip;
mod preview;
mod screen_keys;
mod sessions;
mod skills_view;
mod text_commands;
mod theme;
mod themes_view;
mod topic_icons;
mod topic_tabs;
mod transcript;
mod usage_view;

use crate::screen_keys::{
    ChordMods, FocusTarget, KeyboardOwner, OverlayId, ScreenAction, action_label, focus_target,
    keyboard_owner, screen_chord,
};
use crate::theme::{BASE_REM, Size, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP, text};
use opencrabs::ui_core::actions;
use opencrabs::ui_core::channel_status;
use opencrabs::ui_core::model_layout;
use opencrabs::ui_core::model_selector::{FetchRequest, ProviderSelectorState};
use opencrabs::ui_core::pane_tree::PaneTree;
use opencrabs::ui_core::rail;
use opencrabs::ui_core::split::{self, Layout};
use opencrabs::ui_core::topic_bar;
use transcript::{CopyDrag, LiveTurn, TranscriptState};
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
    AnyElement, App, Application, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Div,
    DragMoveEvent, Entity, ExternalPaths, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, div, point,
    prelude::*, px, rems, rgb, rgba, size,
};
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::utils::drop_landing::human_size;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use uuid::Uuid;

use opencrabs::brain::mission_control::{McActivity, McAnalytics, McInboxItem, McScheduleItem};
use opencrabs::config::Config;
use opencrabs::tui::app::mission_control::log_viewer::LogViewerState;
use opencrabs::tui::render::format_token_count_raw;
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
    /// The themes screen (ctrl+shift+t): preview a theme, keep it or restore.
    Themes,
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

/// One pane beside the window's own chat: its transcript and scroll (#2438).
/// Where it sits, how much space it holds and which session it shows all live
/// in the pane tree, so a pane carries neither a layout nor a session of its
/// own (#2472).
struct SplitPane {
    /// The session's title as the rail shows it, for the pane header.
    title: String,
    transcript: TranscriptState,
    scroll: ScrollHandle,
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

/// How many times a finished turn's reload is retried while its rows are
/// still being written. Eight attempts at the poll cadence is ~1.2s, long
/// enough for the write and short enough to stay invisible; past it the live
/// turn keeps the answer rather than the pane going blank.
const SETTLE_ATTEMPTS: u8 = 8;

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

/// The side panel beside a previewing screen (`themes-sidebar`), plus the
/// `gap_3` between it and the transcript. The transcript is planned against
/// what is left, so its tiles never run under the panel.
const THEMES_PANEL_WIDTH: f32 = 260.0;

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
    let line = channel_status::status_line(&borrowed, &channel_ownership()).map(SharedString::from);
    logging::debug(&format!(
        "chip probe: {} channels -> {}",
        names.len(),
        line.as_ref().map(|s| s.as_ref()).unwrap_or("hidden")
    ));
    line
}

/// The phase 1 window. Every string on screen is real data or the real error.
struct Dashboard {
    /// Which pane the right side draws: chat by default, usage on ctrl+u.
    screen: Screen,
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
    /// Rail width in rem, moved by the splitter's drag. Never below
    /// `RAIL_MIN_REM`: dragging past it closes the rail instead, and ctrl+s
    /// brings it back at the last width.
    rail_width: f32,
    /// Whether the rail is drawn at all.
    rail_open: bool,
    /// Whether the sidebar holds the keyboard. Tab moves it; while it does,
    /// the arrows walk its rows, and the composer keeps them otherwise (#2417).
    rail_focused: bool,
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
    /// The state every pane but the chat keeps, keyed by session (#2472).
    pane_states: HashMap<Uuid, SplitPane>,
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
    /// The layout a split is waiting on a session for; the sidebar picks it (#2438).
    split_pick: Option<Layout>,
    /// A split's new session is being created, so the created row fills the pane.
    split_creating: bool,
    /// The icon picked for the topic being named; None means the default (#2414).
    topic_draft_icon: Option<String>,
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
    /// The row whose pin/delete strip is showing. One at a time: opening a
    /// row's actions closes the one before it.
    rail_actions: Option<Uuid>,
    /// A leftward slide in flight on a rail row: the session and the pointer
    /// x where the drag began. Past the threshold the row's strip opens.
    rail_swipe: Option<(Uuid, f32)>,
    /// The row whose strip a slide just opened. gpui fires a click on mouse-up
    /// whenever the pointer is still over the element, movement or not, so
    /// without this the release that ends a slide also opens the chat the
    /// slide was meant to act on.
    rail_slide_opened: Option<Uuid>,
    /// The conversation a delete is waiting on. Every delete path arms this
    /// and only the dialog's own button erases, so no gesture can cost a
    /// conversation.
    confirm_delete: Option<Uuid>,
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
    /// The topic name being typed, while the name prompt is open (#2303).
    topic_draft: Option<String>,
    /// Which side of the chat the topic row sits on (#2402).
    topic_edge: topic_tabs::Edge,
    /// The topic being renamed, while the prompt is open for a rename (#2304).
    /// None means the prompt names a new topic.
    topic_rename: Option<uuid::Uuid>,
    /// The read-only info panel for a topic, while it is open (#2304).
    topic_info: Option<sessions::TopicInfo>,
    /// The palette the window had when the themes screen opened. Esc puts it
    /// back, so a preview is never kept by accident.
    themes_origin: Option<palette::Palette>,
    /// The theme the themes screen is previewing, if the user has clicked one.
    themes_pick: Option<String>,
    /// The row the keyboard is standing on in the themes sidebar, and every
    /// theme this visit has already previewed, oldest first. The history is
    /// how a good preview is found again without hunting the list.
    themes_index: usize,
    themes_history: Vec<String>,
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
    /// The onboarding wizard while its screen is open. Built on first open:
    /// constructing it reads the live config, which is not something to do on
    /// every window start when most sessions never onboard.
    onboard: Option<onboard::Onboard>,
    /// The plan checklist strip above the editor, re-read when its file moves.
    plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per selection rather than once per frame.
    plan_for: Option<Uuid>,
    /// The selected session's transcript and its scroll handle, so a fresh
    /// load can open at the bottom where the conversation ends.
    transcript: transcript::TranscriptState,
    transcript_scroll: ScrollHandle,
    /// The rail's own scroll handle, so a cursor moved by the arrows can
    /// ask the next frame to bring its row into view (#2351).
    rail_scroll: ScrollHandle,
    /// The message editor (task 4). Typing goes to it while it holds focus;
    /// its Sent events arrive through the subscription made in `main`.
    editor: Entity<editor::Editor>,
    /// Agent-side state (task 5). Events from the bridge's bootstrap and
    /// turn threads land in the receiver; the poll tick drains it.
    agent_rx: std::sync::mpsc::Receiver<agent_bridge::TurnEvent>,
    sink: agent_bridge::EventSink,
    /// Set once bootstrap reports the service live. Turns run on the pane's
    /// open conversation (`transcript.session`), never on a session of the
    /// bridge's own, so the answer always lands where the question was typed.
    agent_ready: bool,
    /// The turn in flight: question, stream, tool rows, notices. It renders
    /// as the transcript's last entry and clears once the DB holds the rows,
    /// so a slow or silent turn never looks like an empty conversation.
    live: transcript::LiveTurn,
    /// Reload attempts made while waiting for the turn's rows to persist.
    /// Bounded, so a write that never lands cannot spin the fetch loop.
    settle_retries: u8,
    /// Turns the transcript must hold before the live turn is dropped: the
    /// count on screen when the send happened, plus the new one.
    settle_expect: Option<usize>,
    /// A turn is running: sends queue instead of starting a second one.
    busy: bool,
    /// The running turn's cancellation token, so Esc twice stops it the way
    /// the TUI's second Esc does. `None` when no turn is in flight.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// When the last Esc landed. A second Esc inside the window cancels the
    /// turn; a lone Esc is ignored, because Esc is also how a user clears a
    /// selection.
    esc_at: Option<std::time::Instant>,
    /// Messages typed while busy; joined and sent when the turn completes.
    queued: Vec<agent_bridge::PendingMessage>,
    /// Set when the agent stack failed to come up; sends are refused.
    agent_error: Option<String>,
    /// Prompt tokens the provider last reported (`ProgressEvent::TokenCount`
    /// and the finished turn's own `context_tokens`). `None` until the first
    /// answer, which is why the TUI shows a dash there too.
    ctx_tokens: Option<u32>,
    /// The open conversation's context window: the compaction budget the
    /// service actually enforces for it, not a hardcoded 200K.
    ctx_max: u32,
    /// Per-turn card metrics keyed by turn anchor (#2324): what a settled
    /// turn cost and how fast it answered. In memory only, so a restarted
    /// window forgets instead of showing the session's current number as if
    /// it belonged to an old turn.
    turn_metrics: HashMap<String, transcript::TurnMetrics>,
    /// Metrics for the turn that just settled, waiting for its rows to land
    /// so the anchor they get keyed by exists. Session-tagged: a reload for
    /// another conversation must not claim them.
    pending_metrics: Option<(Uuid, transcript::TurnMetrics)>,
    /// The checkout's branch, read once at startup: the footer's own field in
    /// the TUI. `None` outside a git tree, which the footer shows as a dash
    /// rather than a made-up name.
    branch: Option<String>,
}

/// How many previews the themes sidebar's history strip keeps. A trail, not a
/// second list: enough to step back a few times, short enough to stay quiet.
const THEMES_HISTORY_LEN: usize = 6;

/// Record a preview in the history strip.
///
/// Newest goes last so the strip reads oldest to newest, the same theme never
/// appears twice (previewing it again moves it forward), and the trailing end
/// is dropped once the trail is full.
fn remember_preview(history: &mut Vec<String>, name: &str) {
    history.retain(|seen| seen != name);
    history.push(name.to_string());
    while history.len() > THEMES_HISTORY_LEN {
        history.remove(0);
    }
}

/// Apply sidebar visibility without letting a closed rail retain keyboard
/// focus or a hidden filter. Width belongs to the window layout and is not
/// part of this transition.
fn set_rail_open_state(
    open: bool,
    rail_open: &mut bool,
    rail_focused: &mut bool,
    sessions: &mut sessions::SessionsState,
) {
    *rail_open = open;
    if !open {
        *rail_focused = false;
        sessions.filter.clear();
        sessions.filter_active = false;
    }
}

#[cfg(test)]
mod sidebar_action_tests {
    use super::*;

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
            assert_eq!(focused, initially_open);
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
    fn sidebar_actions_existing_toggle_uses_the_same_transition() {
        let source = include_str!("main.rs");
        let body = source
            .rsplit("fn toggle_rail")
            .next()
            .and_then(|tail| tail.split("fn stop_turn").next())
            .expect("toggle body");
        assert!(body.contains("self.set_rail_open(open, cx)"));
        assert!(!body.contains("self.rail_open = !self.rail_open"));
    }
}

impl Dashboard {
    /// Re-read the channel chip now (#2452): after a Start click succeeds,
    /// the daemon appears within a second or two, so poll instead of
    /// waiting for the next 5-second tick. All IO runs off the UI thread.
    fn refresh_channel_chip(&mut self, cx: &mut Context<Self>) {
        let names = self.chip_channel_names.clone();
        cx.spawn(async move |this, cx| {
            let line = cx
                .background_executor()
                .spawn(async move { channel_chip_line_from(&names) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.channel_chip != line {
                    logging::info(&format!(
                        "channel chip -> {}",
                        line.as_ref().map(|s| s.as_ref()).unwrap_or("hidden")
                    ));
                    this.channel_chip = line;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The chip's action (#2452): install and start the daemon service via
    /// the onboarding path (launchd on macOS, systemd user unit on Linux).
    /// This is the only way the GUI ever starts one, and only on a click:
    /// nothing here spawns a ChannelManager or claims a channel lock.
    fn start_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon_starting {
            return;
        }
        self.daemon_starting = true;
        self.daemon_error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { opencrabs::tui::onboarding::config::install_daemon_service() })
                .await;
            this.update(cx, |this, cx| {
                this.daemon_starting = false;
                match result {
                    Ok(()) => {
                        // The probe loop and the refresh below confirm the
                        // daemon is live; the chip drops on its own word.
                        this.refresh_channel_chip(cx);
                        logging::info("daemon service installed");
                    }
                    Err(e) => {
                        this.daemon_error = Some(SharedString::from(e));
                        cx.notify();
                    }
                }
            })
        })
        .detach();
    }

    /// Title of the conversation the chat pane is on, if the rail knows it.
    fn session_title(&self) -> Option<&str> {
        self.transcript.session.and_then(|id| {
            self.sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref())
        })
    }

    /// One door for arriving on a conversation (#2481): every surface that
    /// belongs to the conversation on screen resets here, so nothing carries
    /// across. Budget, metrics, queue, composer draft and attachments are
    /// per-conversation state; a fresh topic must open as a completely
    /// isolated conversation, and a plain switch must stop carrying a draft.
    /// The reset keeps the same-session guard the old open_session had:
    /// re-clicking the chip of the conversation already on screen is not a
    /// switch, so its queue and draft survive. The create-topic path always
    /// passes a brand-new id, so the full reset fires there every time.
    fn begin_conversation(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let switched = self.transcript.session != Some(id);
        self.sessions.selected = Some(id);
        if switched {
            // A different conversation has its own budget and its own last
            // prompt size; carrying the previous one over would show a
            // counter for a session the user is not reading.
            self.ctx_tokens = None;
            // Metrics belong to the turn they settled, never to the session
            // the user just switched to.
            self.pending_metrics = None;
            if let Some(b) = agent_bridge::bridge() {
                self.ctx_max = b.service.context_limit_for_session(id);
            }
            // The queue belongs to the conversation on screen: a message
            // typed for one session must never be drained into another
            // (#2441), and it must not sit waiting on one the user left.
            self.queued.clear();
            // The draft and its attachments belong to the conversation they
            // were typed in (#2481).
            self.editor.update(cx, |ed, cx| ed.begin_conversation(cx));
        }
        let previous = self.transcript.session;
        self.transcript.session = Some(id);
        // The chat is a leaf of the pane tree, so switching conversations
        // moves that leaf instead of leaving a stale pane behind (#2472).
        self.adopt_chat_leaf(previous, id);
        // The next launch opens this conversation. Both surfaces read the same
        // file, so switching here means the terminal comes back to it too, and
        // a restart no longer lands on whatever the rail listed first (#2444).
        remember_open_session(id);
        agent_bridge::set_composer_session(Some(id));
        self.transcript.error = None;
        self.transcript.turns = Vec::new();
        self.transcript.hidden = 0;
    }

    /// Open one conversation: select it in the rail, switch the right pane to
    /// chat, and load its newest window off the UI thread. Every door (rail
    /// click, launch, ctrl+u return, create topic) goes through here, and the
    /// per-conversation reset lives one layer down in `begin_conversation`,
    /// so the screen and the session can never disagree.
    fn open_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.begin_conversation(id, cx);
        self.screen = Screen::Chat;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                // Apply only if the user still sits on this session; a slow
                // fetch for an abandoned click paints nothing.
                if this.transcript.session == Some(id) {
                    match result {
                        Ok((rows, total)) => {
                            let (hidden, turns) =
                                transcript::turns_with_metrics(&rows, total, &this.turn_metrics);
                            this.transcript.hidden = hidden;
                            this.transcript.turns = turns;
                        }
                        Err(e) => this.transcript.error = Some(e.into()),
                    }
                    this.transcript_scroll.scroll_to_bottom();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        self.refocus_screen();
        cx.notify();
    }

    /// Fold or unfold one turn. Folding is the default, so the set holds the
    /// anchors the user OPENED, keyed exactly as the TUI keys its override
    /// map (#2155), which is what lets an opened turn survive a reload.
    fn toggle_turn(&mut self, anchor: &str, cx: &mut Context<Self>) {
        if !self.transcript.expanded.remove(anchor) {
            self.transcript.expanded.insert(anchor.to_string());
        } else {
            // A folded turn forgets its full details, so reopening starts at the top (#2440).
            self.transcript.details.remove(anchor);
        }
        cx.notify();
    }

    /// ctrl+o: toggle the newest turn, the same chord the TUI answers.
    /// The TUI's three presses: folded, then expanded, then details in full,
    /// then folded again (#2440). The first press opens the turn; the second
    /// shows its reasoning whole.
    fn toggle_newest_turn(&mut self, cx: &mut Context<Self>) {
        // A turn in flight owns the chord: the card on screen is the live one,
        // so ctrl+o opens or folds IT, the way it opens a settled turn (#2442).
        if !self.live.is_empty() {
            self.transcript.live_expanded = !self.transcript.live_expanded;
            cx.notify();
            return;
        }
        let Some(anchor) = self.transcript.newest_anchor().map(str::to_string) else {
            return;
        };
        if !self.transcript.expanded.contains(&anchor) {
            self.transcript.expanded.insert(anchor);
        } else if self.transcript.details.remove(&anchor) {
            self.transcript.expanded.remove(&anchor);
        } else {
            self.transcript.details.insert(anchor);
        }
        cx.notify();
    }

    /// Every overlay open right now, bottom to top. The keyboard owner is
    /// derived from this one list, so an opener cannot forget to move focus
    /// and a closer cannot forget to hand it back (#2454, #2455).
    fn open_overlays(&self) -> Vec<OverlayId> {
        // A full screen is its own surface: the rail and the chat overlays are
        // not drawn over it, so nothing below can own a key there.
        if self.screen != Screen::Chat {
            return Vec::new();
        }
        let mut open = Vec::new();
        if self.rail_open && self.rail_focused {
            open.push(OverlayId::Rail);
        }
        if self.split_pick.is_some() {
            open.push(OverlayId::SplitPick);
        }
        if self.topic_info.is_some() {
            open.push(OverlayId::TopicInfo);
        }
        if self.topic_draft.is_some() {
            open.push(OverlayId::TopicPrompt);
        }
        if self.help_open {
            open.push(OverlayId::Help);
        }
        if self.preview.is_some() {
            open.push(OverlayId::Preview);
        }
        if self.rail_actions.is_some() {
            open.push(OverlayId::RowActions);
        }
        if self.sessions.rail_menu.is_some() {
            open.push(OverlayId::RailMenu);
        }
        if self.confirm_delete.is_some() {
            open.push(OverlayId::ConfirmDelete);
        }
        if self.sessions.filter_active {
            open.push(OverlayId::RailFind);
        }
        if self.finder.is_some() {
            open.push(OverlayId::CommandPalette);
        }
        open
    }

    /// Ask the next frame to put focus where the current screen expects it.
    ///
    /// A transition calls this instead of touching focus itself: only the
    /// render holds a window to focus through, and this is the one place that
    /// decides which element owns the keyboard (#2350, #2356, #2454).
    fn refocus_screen(&mut self) {
        self.refocus = Some(focus_target(keyboard_owner(
            &self.open_overlays(),
            self.screen,
        )));
    }

    /// An overlay just opened or closed. Same derived decision, named so a
    /// call site reads as taking the keyboard rather than re-labelling the
    /// screen.
    fn refocus_overlay(&mut self) {
        self.refocus_screen();
    }

    /// Open the Seeker card (`alt+space`, `cmd-k`): one field over the
    /// window that finds a conversation, a command or a surface with the same
    /// scoring the terminal uses (#2458).
    ///
    /// It opens only when nothing else owns the keyboard, so two overlays
    /// never stack and the card always has something to hand focus back to.
    fn open_finder(&mut self, cx: &mut Context<Self>) {
        if !self.open_overlays().is_empty() {
            return;
        }
        self.finder = Some(PaletteState::new(self.finder_items()));
        self.finder_channels = configured_channels();
        self.finder_selecting = false;
        self.finder_activity_reset();
        // The caret blinks on a timer, and the loop dies with the card (#2491).
        // It only toggles while the field is fresh; past CARET_IDLE_AFTER the
        // caret stays off until the next key restarts it (#2531).
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(530))
                    .await;
                let open = this
                    .update(cx, |view, cx| {
                        if view.finder.is_none() {
                            return false;
                        }
                        if opencrabs::ui_core::palette::caret_blinks(
                            view.finder_activity,
                            std::time::Instant::now(),
                            CARET_IDLE_AFTER,
                        ) {
                            view.finder_caret = !view.finder_caret;
                        } else {
                            view.finder_caret = false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !open {
                    break;
                }
            }
        })
        .detach();
        self.refocus_overlay();
        logging::info("finder: opened");
        cx.notify();
    }

    /// A key reached the Seeker field: the caret comes back on and its idle
    /// clock restarts, so a paused field hides it and typing re-arms it
    /// (#2531).
    fn finder_activity_reset(&mut self) {
        self.finder_activity = std::time::Instant::now();
        self.finder_caret = true;
    }

    /// Close the card and hand the keyboard back to what was under it.
    fn close_finder(&mut self, cx: &mut Context<Self>) {
        self.finder = None;
        self.finder_selecting = false;
        self.refocus_screen();
        cx.notify();
    }

    /// The rows the card can find, built from what this window already holds:
    /// the shared command table, the destinations both clients can open, and
    /// the conversation rail.
    fn finder_items(&self) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = slash::SLASH_COMMANDS
            .iter()
            // A destination prints its own command row: listing both would
            // show `/usage` twice (#2491).
            .filter(|cmd| !find::is_surface_command(cmd.name))
            .map(|cmd| {
                PaletteItem::new(
                    cmd.name,
                    cmd.description,
                    PaletteKind::Command,
                    PaletteAction::RunSlash(cmd.name),
                )
                .with_keywords(find::keywords_for(cmd.name))
            })
            .collect();
        items.extend(find::surface_items());
        items.extend(help_keys::seeker_items(help_keys::HelpClient::Gui));
        // The docs pages: a query that is not a command, a screen or a
        // conversation is usually a question the documentation answers (#2461).
        items.extend(docs::items());
        // The channel setup rows: the word that names a channel has to find the
        // row that starts its setup, not only the page that describes it (#2462).
        items.extend(find::channel_items());
        // Standalone actions share one catalog; execution stays in this window.
        items.extend(actions::items());
        for skill in opencrabs::brain::skills::load_all_skills() {
            items.push(
                PaletteItem::new(
                    skill.slash_name.clone(),
                    skill.description.clone(),
                    PaletteKind::Skill,
                    PaletteAction::RunSkill(skill.slash_name.clone()),
                )
                .with_keywords("skill workflow recipe")
                .with_description(&skill.description),
            );
        }
        for session in &self.sessions.items {
            let title = session
                .title
                .clone()
                .unwrap_or_else(|| "Untitled conversation".to_string());
            items.push(PaletteItem::new(
                title,
                session.updated_at.format("%Y-%m-%d %H:%M").to_string(),
                PaletteKind::Session,
                PaletteAction::OpenSession(session.id.as_u128()),
            ));
        }
        items
    }

    /// Every key while the card is up belongs to it: typing narrows the list,
    /// the arrows move the armed row and Enter runs it. Nothing falls through
    /// to the composer, which is the whole point of the card (#2458).
    fn finder_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        // Any key is activity: the caret comes back and its idle clock
        // restarts before the key does its work (#2531).
        self.finder_activity_reset();
        if let Some(edit) = find::editing_chord(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.platform,
            ks.modifiers.alt,
            ks.modifiers.shift,
        ) {
            self.apply_finder_edit(edit, cx);
            cx.notify();
            return;
        }
        let Some(state) = self.finder.as_mut() else {
            return;
        };
        match ks.key.as_str() {
            "escape" => {
                self.close_finder(cx);
                return;
            }
            "enter" => {
                self.run_finder_pick(cx);
                return;
            }
            "up" => state.move_by(-1, find::SEEKER_PAGE),
            "down" => state.move_by(1, find::SEEKER_PAGE),
            "pageup" => state.move_by(-(find::SEEKER_PAGE as i32), find::SEEKER_PAGE),
            "pagedown" => state.move_by(find::SEEKER_PAGE as i32, find::SEEKER_PAGE),
            "space" => state.type_char(' '),
            _ => {
                // An editing chord returned above. Any other modified key is
                // still a chord, never text to append to the query.
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    return;
                }
                if let Some(text) = ks.key_char.as_deref() {
                    for ch in text.chars() {
                        state.type_char(ch);
                    }
                }
            }
        }
        cx.notify();
    }

    fn apply_finder_edit(&mut self, edit: find::SeekerEdit, cx: &mut Context<Self>) {
        let Some(state) = self.finder.as_mut() else {
            return;
        };
        match edit {
            find::SeekerEdit::Move {
                delta,
                word,
                select,
            } => match (word, select) {
                (true, true) => state.select_word_to(delta),
                (true, false) => state.move_caret_word(delta),
                (false, true) => {
                    let len = state.query().chars().count();
                    let target = if delta < 0 {
                        state.caret().saturating_sub(delta.unsigned_abs())
                    } else {
                        state.caret().saturating_add(delta.unsigned_abs()).min(len)
                    };
                    state.select_to(target);
                }
                (false, false) => state.move_caret(delta),
            },
            find::SeekerEdit::Home { select } => {
                if select {
                    state.select_to(0);
                } else {
                    state.move_caret_to(0);
                }
            }
            find::SeekerEdit::End { select } => {
                let end = state.query().chars().count();
                if select {
                    state.select_to(end);
                } else {
                    state.move_caret_to(end);
                }
            }
            find::SeekerEdit::Backspace { word: true } => state.delete_word_back(),
            find::SeekerEdit::Backspace { word: false } => state.backspace(),
            find::SeekerEdit::Delete { word: true } => state.delete_word_forward(),
            find::SeekerEdit::Delete { word: false } => state.delete_forward(),
            find::SeekerEdit::SelectAll => state.select_all(),
            find::SeekerEdit::Copy => {
                if let Some(text) = state.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                }
            }
            find::SeekerEdit::Cut => {
                if let Some(text) = state.take_selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            find::SeekerEdit::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    state.insert_text(&text);
                }
            }
        }
    }

    /// Translate a window point to the Seeker's character caret through the
    /// shaped line captured during paint, exactly as the composer does.
    fn seeker_index_for_position(&self, position: Point<Pixels>, cx: &App) -> usize {
        let Some(state) = self.finder.as_ref() else {
            return 0;
        };
        let paint = self.finder_paint.read(cx);
        let (Some(line), Some(bounds)) = (paint.line.as_ref(), paint.bounds.as_ref()) else {
            return 0;
        };
        let relative = point(position.x - bounds.left(), position.y - bounds.top());
        let byte = line
            .closest_index_for_position(relative, paint.line_height)
            .unwrap_or_else(|index| index)
            .min(state.query().len());
        find::byte_to_char(state.query(), byte)
    }

    fn finder_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let index = self.seeker_index_for_position(event.position, cx);
        self.finder_selecting = true;
        self.finder_activity_reset();
        if let Some(state) = self.finder.as_mut() {
            if event.modifiers.shift {
                state.select_to(index);
            } else {
                state.move_caret_to(index);
            }
        }
        cx.notify();
    }

    fn finder_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.finder_selecting {
            return;
        }
        let index = self.seeker_index_for_position(event.position, cx);
        self.finder_activity_reset();
        if let Some(state) = self.finder.as_mut() {
            state.select_to(index);
        }
        cx.notify();
    }

    fn finder_mouse_up(&mut self, _cx: &mut Context<Self>) {
        self.finder_selecting = false;
    }

    /// Run the row Enter is on, or the best match when no arrow was pressed.
    fn run_finder_pick(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.finder.as_ref().and_then(|state| state.selected()) else {
            self.close_finder(cx);
            return;
        };
        self.finder = None;
        self.finder_selecting = false;
        self.refocus_screen();
        match row.item.action {
            PaletteAction::RunSlash(name) => self.send_message(name.to_string(), cx),
            PaletteAction::OpenSession(id) => self.open_session(Uuid::from_u128(id), cx),
            PaletteAction::OpenSurface(surface) => self.open_surface(surface, cx),
            PaletteAction::OpenSidebar => self.set_rail_open(true, cx),
            PaletteAction::CloseSidebar => self.set_rail_open(false, cx),
            PaletteAction::SwitchModel { provider, model } => {
                self.send_message(format!("/models {provider}/{model}"), cx)
            }
            PaletteAction::RunSkill(name) | PaletteAction::RunUserCommand(name) => {
                self.send_message(name, cx)
            }
            PaletteAction::SetupChannel(channel) => self.send_message(
                format!(
                    "Set up {} as a channel: walk me through each step and make the config change with me.",
                    channel.name()
                ),
                cx,
            ),
            // The miss row asks in a fresh session. send_message is the
            // whole seam: with none open it queues the text and creates the
            // session itself (#2482).
            PaletteAction::Ask(text) => self.send_message(text, cx),
            PaletteAction::NewTopic => {
                // The same card re-armed: field empty, conversations only,
                // so a few letters pick the session (#2482).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::NewTopic,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::NewTopicOn(id) => {
                // The exact path the "New topic" menu item walks: the
                // conversation on screen, then the draft dialog (#2482).
                self.open_session(Uuid::from_u128(id), cx);
                self.start_topic_draft(cx);
            }
            PaletteAction::Favorite => {
                // Same card, same picker, different purpose: the pick pins
                // the conversation (#2487).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::Favorite,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::FavoriteToggle(id) => {
                // The exact path the rail's pin button walks (#2487).
                self.toggle_favorite(Uuid::from_u128(id), cx);
            }
            PaletteAction::OpenDocs(path) => {
                let url = docs::url(path);
                preview::launch(&preview::url_command(&url));
            }
        }
    }

    /// Open the destination a surface row names, through the same toggles the
    /// chords use, so a pick and a key press land in exactly the same place.
    fn open_surface(&mut self, surface: find::Surface, cx: &mut Context<Self>) {
        match surface {
            find::Surface::Usage => self.toggle_usage(cx),
            find::Surface::Files => self.toggle_files(cx),
            find::Surface::Themes => self.toggle_themes(cx),
            find::Surface::Models => self.open_models(None, cx),
            find::Surface::Sessions => self.toggle_rail(cx),
            find::Surface::Help => self.toggle_help(cx),
            find::Surface::Skills => self.open_skills(cx),
            // `/profiles` is a text command now (#2469): the palette row takes
            // the same door as typing it, never the model.
            find::Surface::Profiles => {
                if let Some(outcome) = commands::parse_command("/profiles") {
                    self.run_command(outcome, cx);
                }
            }
            find::Surface::MissionControl => self.open_mission_control(cx),
        }
    }

    /// A click on printed row `ix`: arm it, then take the same path Enter
    /// takes, so a click and a key press can never act on different rows.
    /// A kind chip pressed on the Seeker card: the list reshapes through the
    /// engine's own filter, so both frontends filter identically (#2491).
    fn set_finder_kind_filter(&mut self, want: Option<PaletteKind>, cx: &mut Context<Self>) {
        if let Some(state) = self.finder.as_mut() {
            state.set_kind_filter(want);
            cx.notify();
        }
    }

    /// The wheel over the results card steps the same engine window the
    /// arrows walk (#2520), so wheel and keys can never disagree.
    fn finder_scroll(&mut self, rows: i32, cx: &mut Context<Self>) {
        if rows == 0 {
            return;
        }
        if let Some(state) = self.finder.as_mut() {
            state.move_by(rows, find::SEEKER_PAGE);
            cx.notify();
        }
    }

    fn pick_finder_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.finder.as_mut() {
            state.arm(ix);
        }
        self.run_finder_pick(cx);
    }

    /// Run what the screen table named. The table decides and this applies, so
    /// the routing is unit tested and the effect stays in one place.
    fn apply_screen_action(&mut self, action: ScreenAction, cx: &mut Context<Self>) {
        match action {
            ScreenAction::EnterUsage | ScreenAction::LeaveUsage => self.toggle_usage(cx),
            ScreenAction::EnterFiles | ScreenAction::LeaveFiles => self.toggle_files(cx),
            ScreenAction::LeaveMissionControl => self.close_mission_control(cx),
            ScreenAction::ToggleHelp => self.toggle_help(cx),
        }
    }

    /// Run a submitted command, or say why it does not run here.
    ///
    /// The reading is `commands::parse_command`, which is pure and unit
    /// tested; this only applies what it decided. Nothing in here may fall
    /// through to the agent: a command the window cannot run gets one line,
    /// because a model asked to answer `/usage` writes a paragraph about
    /// usage instead of opening the ledger (#2374).
    fn run_command(&mut self, outcome: commands::Outcome, cx: &mut Context<Self>) {
        // The pair has to travel with the action: `/models openai/gpt-5` is a
        // switch, a bare `/models` is the picker, and dropping the argument
        // would silently turn one into the other.
        let (action, arg) = match outcome {
            commands::Outcome::Run(action) => (action, None),
            commands::Outcome::RunWith(action, arg) => (action, Some(arg)),
            other => {
                if let Some(line) = commands::message(&other) {
                    self.live.notices.push(line);
                    cx.notify();
                }
                return;
            }
        };
        match action {
            commands::GuiAction::OpenThemes => {
                // The toggle closes it, so ask first: `/theme` on the themes
                // screen must not throw the preview away.
                if self.screen != Screen::Themes {
                    self.toggle_themes(cx);
                }
            }
            commands::GuiAction::OpenUsage => {
                if self.screen != Screen::Usage {
                    self.toggle_usage(cx);
                }
            }
            commands::GuiAction::OpenMissionControl => {
                if self.screen != Screen::MissionControl {
                    self.open_mission_control(cx);
                }
            }
            commands::GuiAction::ToggleHelp => self.toggle_help(cx),
            commands::GuiAction::NewSession => self.new_session(cx),
            commands::GuiAction::StopTurn => self.stop_turn(cx),
            commands::GuiAction::OpenSessions => self.toggle_rail(cx),
            commands::GuiAction::OpenModels => self.open_models(arg, cx),
            commands::GuiAction::OpenSkills => self.open_skills(cx),
            commands::GuiAction::OpenOnboard(name) => self.open_onboard(name, arg, cx),
            commands::GuiAction::Text(command) => self.run_text_command(command, arg, cx),
            commands::GuiAction::Quit => cx.quit(),
            commands::GuiAction::Evolve => {
                let session = self.transcript.session.unwrap_or_else(Uuid::nil);
                self.live
                    .notices
                    .push("Checking for a new release...".to_string());
                agent_bridge::spawn_evolve(self.sink.clone(), session);
            }
        }
    }

    /// The sessions rail: `/sessions` and ctrl+s share one toggle, so the
    /// command and the chord can never disagree about what showing it means.
    /// A hidden rail has no field to keep listening, so the filter exits.
    /// Start a split: open the sidebar, focus it, and ask for the session
    /// that shares the space with the pane holding the caret. The pane being
    /// split is the one the caret is on, so a nested split grows from the
    /// pane being read rather than from the window's first leaf (#2472).
    fn begin_split(&mut self, layout: Layout, cx: &mut Context<Self>) {
        let target = self.pane_focus.or(self.transcript.session);
        let target = target.filter(|id| self.panes.as_ref().is_some_and(|t| t.contains(*id)));
        let Some(target) = target else {
            self.live
                .notices
                .push("Open a session before splitting it.".to_string());
            cx.notify();
            return;
        };
        if !self.rail_open {
            self.toggle_rail(cx);
        }
        self.split_pick = Some(layout);
        self.split_target = Some(target);
        // A fresh split starts with the pane it grows from in front, whatever
        // the last one left behind (#2474).
        self.pane_focus = if Some(target) == self.transcript.session {
            None
        } else {
            Some(target)
        };
        self.rail_focused = true;
        self.sessions.cursor = self.transcript.session;
        // Focus goes to the window, not the composer, so the sidebar keys and the
        // dialog's N and Esc reach it while the split waits.
        self.refocus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// Esc during the pick: the split is dropped and the chat keeps focus.
    fn cancel_split_pick(&mut self, cx: &mut Context<Self>) {
        self.split_pick = None;
        self.split_target = None;
        self.rail_focused = false;
        self.refocus_screen();
        cx.notify();
    }

    /// The session holding the caret: the chat's own session when no pane
    /// took it (#2474).
    fn focused_pane(&self) -> Option<Uuid> {
        self.pane_focus.or(self.transcript.session)
    }

    /// Close one pane and hand its space to its neighbour (#2438, #2472).
    /// Closing the chat itself moves the window to the first survivor, whose
    /// leaf takes the chat's place, so the panes that remain keep the slots
    /// they were dragged to and the window never shows a session the user
    /// did not pick.
    fn close_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_mut() else {
            return;
        };
        if tree.leaves().len() <= 1 {
            return;
        }
        let Some(absorbed) = tree.close(session) else {
            return;
        };
        self.pane_states.remove(&session);
        self.split_drag = None;
        self.pane_focus = None;
        if self.transcript.session == Some(session) {
            self.open_session(absorbed, cx);
        } else {
            self.refocus_screen();
            cx.notify();
        }
    }

    /// `ctrl+x` and the × on the focused pane's header: close the pane holding
    /// the caret (#2474).
    fn close_focused_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.focused_pane() {
            self.close_pane(session, cx);
        }
    }

    /// Tab moves the caret between the panes in the order they are drawn, so
    /// the accent follows the layout the eye reads (#2475, #2472). With one
    /// pane there is nothing to switch to, so the rail keeps the key as it
    /// always did. The pane holding the caret wears the accent in its header
    /// and takes the page keys.
    fn cycle_pane_focus(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_ref() else {
            return;
        };
        let leaves = tree.leaves();
        if leaves.len() < 2 {
            return;
        }
        let at = self
            .focused_pane()
            .and_then(|id| leaves.iter().position(|leaf| *leaf == id))
            .unwrap_or(0);
        let next = leaves[(at + 1) % leaves.len()];
        // The chat's own leaf is the pane the caret reaches as `None`, which
        // is how every other surface already names it.
        self.pane_focus = if Some(next) == self.transcript.session {
            None
        } else {
            Some(next)
        };
        // A receipt in the daily log, so "Tab did nothing" can be told apart
        // from "Tab moved the caret and the header did not show it".
        logging::debug(&format!("pane focus -> {:?}", self.pane_focus));
        cx.notify();
    }

    /// Fill a new pane with `id`, splitting the pane the pick started from,
    /// and load its newest messages (#2438, #2472).
    fn choose_split(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(layout) = self.split_pick else {
            return;
        };
        let Some(target) = self.split_target else {
            self.split_pick = None;
            return;
        };
        if self.transcript.session == Some(id)
            || self.panes.as_ref().is_some_and(|tree| tree.contains(id))
        {
            self.live
                .notices
                .push("That session is already open in a pane. Pick another.".to_string());
            cx.notify();
            return;
        }
        self.split_pick = None;
        self.split_target = None;
        self.rail_focused = false;
        // The pane just picked is the one the caret and ctrl+x land on, so a
        // split opens ready to act on what was clicked (#2474).
        self.pane_focus = Some(id);
        self.refocus_screen();
        let split = self
            .panes
            .as_mut()
            .is_some_and(|tree| tree.split(target, layout, id));
        if !split {
            self.pane_focus = None;
            self.live
                .notices
                .push("That pane is no longer open.".to_string());
            cx.notify();
            return;
        }
        let title = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "(untitled)".to_string());
        self.pane_states.insert(
            id,
            SplitPane {
                title,
                transcript: TranscriptState {
                    session: Some(id),
                    ..Default::default()
                },
                scroll: ScrollHandle::new(),
            },
        );
        // Paint the pane now, with its header, rather than waiting for the
        // transcript load to come back: an empty pane that appears at once
        // reads as a split, where a silent wait reads as nothing (#2438).
        cx.notify();
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                if let Some(pane) = this.pane_states.get_mut(&id) {
                    match result {
                        Ok((rows, total)) => {
                            let (hidden, turns) =
                                transcript::turns_with_metrics(&rows, total, &this.turn_metrics);
                            pane.transcript.hidden = hidden;
                            pane.transcript.turns = turns;
                        }
                        Err(e) => pane.transcript.error = Some(e.into()),
                    }
                    pane.scroll.scroll_to_bottom();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keep the chat on a leaf of the tree (#2472). A first open starts the
    /// tree; a plain switch moves the chat's own leaf to the new session;
    /// opening a session another pane already shows moves it into the chat and
    /// closes that pane, so one session never sits in two leaves.
    fn adopt_chat_leaf(&mut self, previous: Option<Uuid>, id: Uuid) {
        let Some(tree) = self.panes.as_mut() else {
            self.panes = Some(PaneTree::leaf(id));
            return;
        };
        if tree.contains(id) {
            self.pane_states.remove(&id);
            if let Some(was) = previous
                && was != id
                && tree.leaves().len() > 1
            {
                tree.close(was);
            }
            return;
        }
        if let Some(was) = previous
            && tree.rename_leaf(was, id)
        {
            return;
        }
        self.panes = Some(PaneTree::leaf(id));
        self.pane_states.clear();
    }

    /// Grab the divider between two children of one split (#2472, #2476).
    /// A double click puts that pair back to even, the quick way out of a
    /// share dragged somewhere awkward.
    fn begin_divider_drag(
        &mut self,
        path: &[usize],
        divider: usize,
        ev: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let stacked = {
            let Some(tree) = self.panes.as_mut() else {
                return;
            };
            let Some(PaneTree::Split {
                layout, children, ..
            }) = tree.at(path)
            else {
                return;
            };
            if divider + 1 >= children.len() {
                return;
            }
            if ev.click_count == 2 {
                tree.even_divider(path, divider);
                cx.notify();
                return;
            }
            layout.is_stacked()
        };
        let axis = if stacked {
            ev.position.y
        } else {
            ev.position.x
        };
        self.split_drag = Some(DividerDrag {
            path: path.to_vec(),
            divider,
            stacked,
            last: f32::from(axis),
        });
        cx.notify();
    }

    /// The commands that answer in text or compose a turn (#2469).
    ///
    /// `/compact` and `/goal` are dispatched here because they need
    /// `send_message`, which only the window has. Everything else runs on the
    /// bridge against the engine's own entry points, so the answer a window
    /// user reads is the one the terminal prints.
    fn run_text_command(
        &mut self,
        command: text_commands::TextCommand,
        arg: Option<String>,
        cx: &mut Context<Self>,
    ) {
        use text_commands::TextCommand as C;
        let arg = arg.unwrap_or_default();
        match command {
            C::Compact => {
                let pct = match (self.ctx_tokens, self.ctx_max) {
                    (Some(used), max) if max > 0 => f64::from(used) / f64::from(max) * 100.0,
                    _ => 0.0,
                };
                // Send first, then report (#1375): a notice printed before the
                // trigger claims a compaction that may never start.
                self.send_message(
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string(),
                    cx,
                );
                self.live
                    .notices
                    .push(opencrabs::tui::compact_notice::requested(pct));
                cx.notify();
            }
            C::Cowork => {
                // No screen in either frontend: the agent calls the tool that
                // builds the deep link, exactly as the terminal asks it to.
                self.send_message(text_commands::cowork_prompt(&arg), cx);
            }
            C::Goal => {
                if opencrabs::brain::goal::is_bare(&arg) {
                    // Bare `/goal` is denied with the usage line and never
                    // reaches the model, exactly as the terminal does.
                    self.live
                        .notices
                        .push(opencrabs::brain::goal::goal_usage_warning());
                    cx.notify();
                } else {
                    self.send_message(opencrabs::brain::goal::goal_command_prompt(&arg), cx);
                }
            }
            C::Cd => {
                // The picker owns bare `/cd` (#2527): a screen, not a text
                // reply that also lights the thinking spinner. With a path
                // the change applies locally on the bridge service, the same
                // applier the terminal uses, with no turn and no spinner.
                if arg.is_empty() {
                    self.open_cd_picker(cx);
                } else {
                    let Some(session) = self.transcript.session else {
                        self.live
                            .notices
                            .push("No conversation is open. Type /new first.".to_string());
                        cx.notify();
                        return;
                    };
                    let expanded = opencrabs::brain::tools::error::expand_tilde(&arg);
                    if !expanded.is_dir() {
                        self.live
                            .notices
                            .push(text_commands::cd_not_a_directory(&arg));
                        cx.notify();
                        return;
                    }
                    cx.spawn(async move |this, cx| {
                        let applied = match agent_bridge::bridge() {
                            Some(b) => b.service.apply_operator_cd(session, &expanded).await,
                            None => expanded.clone(),
                        };
                        let _ = this.update(cx, |this, cx| {
                            this.live
                                .notices
                                .push(text_commands::cd_reply(&applied.display().to_string()));
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
            other => {
                let Some(session) = self.transcript.session else {
                    self.live
                        .notices
                        .push("No conversation is open. Type /new first.".to_string());
                    cx.notify();
                    return;
                };
                agent_bridge::spawn_text_command(
                    self.db_path.clone(),
                    session,
                    text_commands::name(other).to_string(),
                    arg,
                    self.sink.clone(),
                );
                cx.notify();
            }
        }
    }

    /// N in the pick: a new session that fills the second pane (#2438).
    fn split_new(&mut self, cx: &mut Context<Self>) {
        self.split_creating = true;
        self.new_session(cx);
    }

    fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        let open = !self.rail_open;
        self.set_rail_open(open, cx);
    }

    fn set_rail_open(&mut self, open: bool, cx: &mut Context<Self>) {
        set_rail_open_state(
            open,
            &mut self.rail_open,
            &mut self.rail_focused,
            &mut self.sessions,
        );
        if !open {
            self.refocus_screen();
        }
        cx.notify();
    }

    /// `/stop`: abort the turn in flight, the same effect as Esc Esc and the
    /// same wording. The pane stops claiming a turn is running the moment the
    /// user asks, and the service's own failure event lands afterwards.
    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = self.cancel.take() else {
            self.live.notices.push("Nothing is running.".to_string());
            cx.notify();
            return;
        };
        cancel.cancel();
        self.busy = false;
        self.live.thinking = false;
        self.live.notices.push("Operation cancelled.".to_string());
        logging::info("turn cancelled by /stop");
        cx.notify();
    }

    /// `/models` with no argument opens the picker. With an argument it is the
    /// switch the terminal performs, and no screen opens: the reply is the
    /// answer, exactly like the terminal's.
    fn open_models(&mut self, arg: Option<String>, cx: &mut Context<Self>) {
        // The catalogue is read once, here, so a frame never walks it.
        let current =
            agent_bridge::bridge().map(|b| (b.service.provider_name(), b.service.provider_model()));
        self.reset_model_picker(current, None);
        if let Some(arg) = arg {
            self.switch_model(arg, cx);
            return;
        }
        self.models_query.clear();
        self.screen = Screen::Models;
        self.rail_actions = None;
        self.sessions.rail_menu = None;
        self.start_models_fetch();
        self.refocus_screen();
        cx.notify();
    }

    /// Reset the one picker catalogue used by `/models` and Brain Fuel. The
    /// optional wizard selector contributes its unsaved provider settings.
    fn reset_model_picker(
        &mut self,
        current: Option<(String, String)>,
        edited: Option<&ProviderSelectorState>,
    ) {
        self.models_rows = models::rows_for_picker(
            current
                .as_ref()
                .map(|(provider, model)| (provider.as_str(), model.as_str())),
            edited,
        );
        let mut catalogue = ProviderSelectorState::default();
        catalogue.load_custom_names();
        let active_provider = current
            .as_ref()
            .map(|(provider, _)| provider.as_str())
            .or_else(|| edited.map(|selector| selector.current_provider().id))
            .unwrap_or_default();
        self.models_pending = opencrabs::ui_core::model_selector::discovery_requests(
            &catalogue,
            active_provider,
            edited,
        );
        self.models_fetch = None;
        self.models_fetch_lifecycle.invalidate();
        self.models_fetch_error = None;
        self.models_fetching = false;
        self.models_index = self
            .models_rows
            .iter()
            .position(|row| row.current)
            .unwrap_or(0);
    }

    /// Restart discovery after an onboarding provider or credential edit.
    fn refresh_onboard_models(&mut self) {
        let Some(ob) = self
            .onboard
            .as_ref()
            .filter(|ob| ob.wizard.step == onboard::OnboardingStep::ProviderAuth)
        else {
            return;
        };
        let edited = ob.wizard.ps.clone();
        let provider = if edited.is_custom() {
            edited.custom_name.clone()
        } else {
            edited.current_provider().id.to_string()
        };
        let current = Some((provider, ob.model()));
        self.reset_model_picker(current, Some(&edited));
        self.start_models_fetch();
    }

    /// Refresh every provider with a saved key, not only the active one.
    /// Requests run one at a time; generation and provider identity are both
    /// checked before a response can alter the visible catalogue.
    fn start_models_fetch(&mut self) {
        if self.models_fetch.is_some() {
            return;
        }
        self.models_fetch_lifecycle.invalidate();
        if let Some(request) = self.models_pending.first().cloned() {
            let generation = self.models_fetch_lifecycle.begin(&request);
            self.models_fetch = Some(model_fetch::spawn(generation, request));
            self.models_fetching = true;
        } else {
            self.models_fetching = false;
        }
    }

    /// Drain the shared model-fetch consumer. True means a visible state
    /// changed and the poll tick must repaint even when no agent event arrived.
    fn poll_model_fetches(&mut self) -> bool {
        let outcome = self
            .models_fetch
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some((generation, outcome)) = outcome else {
            return false;
        };
        self.models_fetch = None;
        self.models_pending
            .retain(|pending| pending.provider_index != outcome.provider_index);
        let provider_index = outcome.provider_index;
        let Some(outcome) = self
            .models_fetch_lifecycle
            .finish(generation, provider_index, outcome)
        else {
            self.models_fetching = false;
            return true;
        };
        if matches!(self.screen, Screen::Models | Screen::Onboard) {
            if let Some(error) = outcome.error {
                let error = format!("{}: {error}", outcome.provider_name);
                self.models_fetch_error = Some(match self.models_fetch_error.take() {
                    Some(previous) => format!("{previous}; {error}"),
                    None => error,
                });
            }
            if !outcome.models.is_empty() {
                let selected = models::filtered_rows(&self.models_rows, &self.models_query)
                    .get(self.models_index)
                    .map(models::Row::pair);
                let current = if let Some(ob) = self.onboard.as_ref()
                    && ob.wizard.step == onboard::OnboardingStep::ProviderAuth
                {
                    let provider = if ob.wizard.ps.is_custom() {
                        ob.wizard.ps.custom_name.clone()
                    } else {
                        ob.provider_id().to_string()
                    };
                    Some((provider, ob.model()))
                } else {
                    agent_bridge::bridge().map(|bridge| {
                        (
                            bridge.service.provider_name(),
                            bridge.service.provider_model(),
                        )
                    })
                };
                models::replace_provider(
                    &mut self.models_rows,
                    &outcome.provider_id,
                    &outcome.provider_name,
                    &outcome.models,
                    current
                        .as_ref()
                        .map(|(provider, model)| (provider.as_str(), model.as_str())),
                );
                let visible = models::filtered_rows(&self.models_rows, &self.models_query);
                self.models_index = visible
                    .iter()
                    .position(|row| Some(row.pair()) == selected)
                    .or_else(|| visible.iter().position(|row| row.current))
                    .unwrap_or_else(|| self.models_index.min(visible.len().saturating_sub(1)));
            }
        }
        self.start_models_fetch();
        true
    }

    /// `/skills` opens the list the terminal's own dialog shows: every skill
    /// the loader returns, built-ins first, from the same three places. Enter
    /// runs the one under the highlight by submitting its prompt body, which
    /// is exactly what the slash does in the TUI (#2466).
    fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.skills_all = skills_view::rows();
        self.skills_query.clear();
        self.refresh_skills();
        self.skills_index = 0;
        self.screen = Screen::Skills;
        self.rail_actions = None;
        self.sessions.rail_menu = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Narrow the visible rows to `skills_query`, the same filter the
    /// terminal's dialog applies on every keystroke. The selection restarts at
    /// the top because the row that was highlighted may no longer exist.
    fn refresh_skills(&mut self) {
        self.skills_rows = skills_view::matching(&self.skills_all, &self.skills_query)
            .into_iter()
            .cloned()
            .collect();
        let last = self.skills_rows.len().saturating_sub(1);
        self.skills_index = self.skills_index.min(last);
    }

    /// Run the skill at `index`: back to the conversation, then the same
    /// prompt the terminal would have sent. `prompt`, not the raw body, so a
    /// `review_gate` skill carries its reminder into the turn.
    fn run_skill(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(skill) = self.skills_rows.get(index) else {
            return;
        };
        let prompt = skills_view::prompt(skill);
        let slash = skill.slash_name.clone();
        self.screen = Screen::Chat;
        self.rail_actions = None;
        self.sessions.rail_menu = None;
        self.refocus_screen();
        self.live.notices.push(format!("Running {slash}"));
        self.send_message(prompt, cx);
    }

    /// The one model-picker keyboard path. Onboarding Enter selects and then
    /// falls through to the wizard's normal confirm; standalone Enter switches.
    fn shared_models_key(
        &mut self,
        ev: &KeyDownEvent,
        onboarding: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let visible = models::filtered_rows(&self.models_rows, &self.models_query);
        let last = visible.len().saturating_sub(1);
        let mut query_changed = false;
        match ev.keystroke.key.as_str() {
            "up" => self.models_index = self.models_index.saturating_sub(1),
            "down" => self.models_index = (self.models_index + 1).min(last),
            "enter" => {
                let pair = visible
                    .get(self.models_index.min(last))
                    .map(models::Row::pair)
                    .or_else(|| {
                        let provider = if onboarding {
                            self.onboard.as_ref().map(|ob| {
                                if ob.wizard.ps.is_custom() {
                                    ob.wizard.ps.custom_name.clone()
                                } else {
                                    ob.provider_id().to_string()
                                }
                            })
                        } else {
                            agent_bridge::bridge().map(|bridge| bridge.service.provider_name())
                        };
                        models::manual_pair(
                            &self.models_query,
                            provider.as_deref().unwrap_or_default(),
                        )
                    });
                if let Some(pair) = pair {
                    if onboarding {
                        self.select_onboard_pair(&pair);
                        return false;
                    }
                    self.switch_model(pair, cx);
                }
                return !onboarding;
            }
            "backspace" => {
                self.models_query.pop();
                self.models_index = 0;
                query_changed = true;
            }
            "escape" if !onboarding => {
                if self.models_query.is_empty() {
                    self.screen = Screen::Chat;
                    self.refocus_screen();
                } else {
                    self.models_query.clear();
                    self.models_index = 0;
                }
                return true;
            }
            "tab" | "escape" if onboarding => return false,
            _ => {
                if ev.keystroke.modifiers.control
                    || ev.keystroke.modifiers.alt
                    || ev.keystroke.modifiers.platform
                {
                    return false;
                }
                let Some(ch) = ev
                    .keystroke
                    .key_char
                    .as_ref()
                    .and_then(|s| s.chars().next())
                    .filter(|ch| !ch.is_control())
                else {
                    return false;
                };
                self.models_query.push(ch);
                self.models_index = 0;
                query_changed = true;
            }
        }
        if query_changed
            && onboarding
            && let Some(ob) = self.onboard.as_mut()
        {
            ob.wizard.ps.model_filter = self.models_query.clone();
            ob.wizard.ps.selected_model = 0;
        }
        true
    }

    fn select_onboard_pair(&mut self, pair: &str) -> bool {
        let Some((provider, model)) = pair.split_once('/') else {
            return false;
        };
        if self
            .onboard
            .as_mut()
            .is_some_and(|ob| ob.select_pair(provider, model))
        {
            self.models_query.clear();
            self.models_index = models::ensure_pair(&mut self.models_rows, provider, model);
            return true;
        }
        false
    }

    fn activate_model_pair(&mut self, pair: String, onboarding: bool, cx: &mut Context<Self>) {
        if !onboarding {
            self.switch_model(pair, cx);
            return;
        }
        let provider = pair.split_once('/').map(|(provider, _)| provider);
        let provider_changed = self.onboard.as_ref().is_some_and(|ob| {
            let current = if ob.wizard.ps.is_custom() {
                ob.wizard.ps.custom_name.as_str()
            } else {
                ob.provider_id()
            };
            Some(current) != provider
        });
        if self.select_onboard_pair(&pair) && provider_changed {
            self.refresh_onboard_models();
        }
        cx.notify();
    }

    /// The wizard owns the keyboard while its screen is open: Tab and the
    /// arrows walk rows, typed characters land in the focused field, Enter
    /// confirms the step and closes when there is nothing after it.
    fn onboard_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.onboard.is_none() {
            self.screen = Screen::Chat;
            cx.notify();
            return;
        }
        let m = &ev.keystroke.modifiers;
        let plain = !m.control && !m.platform && !m.alt;
        let key = ev.keystroke.key.clone();
        let typed = if plain {
            ev.keystroke
                .key_char
                .as_ref()
                .and_then(|s| s.chars().next())
                .filter(|ch| !ch.is_control())
        } else {
            None
        };
        let (
            before_provider,
            before_field,
            before_key_field,
            before_endpoint_field,
            completing_input,
        ) = {
            let ob = self.onboard.as_ref().expect("checked above");
            (
                ob.wizard.ps.selected_provider,
                ob.field,
                ob.key_field(),
                ob.custom_base_url_field(),
                key == "enter"
                    && (ob.key_field() == Some(ob.field)
                        || ob.custom_base_url_field() == Some(ob.field)),
            )
        };
        if self.onboard.as_ref().is_some_and(|ob| {
            ob.step() == onboard::OnboardingStep::ProviderAuth && ob.field == ob.search_field()
        }) && self.shared_models_key(ev, true, cx)
        {
            cx.notify();
            return;
        }
        if completing_input {
            // Enter on the key or custom endpoint completes that field, as in
            // the terminal; it does not skip the list by confirming the step.
            let ob = self.onboard.as_mut().expect("checked above");
            ob.field = ob.search_field();
            self.refresh_onboard_models();
            cx.notify();
            return;
        }
        let mut confirm = false;
        let mut refresh_models = false;
        {
            let ob = self.onboard.as_mut().expect("checked above");
            match key.as_str() {
                "tab" => ob.focus_next(),
                "enter" => {
                    // On the channel list Enter flips the row it stands on, the
                    // same as the terminal; anywhere else it confirms the step.
                    if ob.focused_channel().is_some() {
                        ob.toggle_focused_channel();
                    } else {
                        confirm = true;
                    }
                }
                "space" => {
                    // Space is the terminal's flip for every row that has one: a
                    // channel, a voice engine, an image switch, the daemon switch.
                    // On a text field it is an ordinary character and still types
                    // one there (#2432).
                    if !ob.activate_focused_row()
                        && let Some(ch) = typed
                    {
                        ob.type_char(ch);
                    }
                }
                "up" => ob.walk(-1),
                "down" => ob.walk(1),
                "backspace" => {
                    if !ob.backspace() {
                        ob.focus_prev();
                    }
                }
                _ => {
                    if let Some(ch) = typed {
                        ob.type_char(ch);
                    }
                }
            }
            let provider_changed = ob.wizard.ps.selected_provider != before_provider;
            let completed_input_with_tab = key == "tab"
                && (before_key_field == Some(before_field)
                    || before_endpoint_field == Some(before_field));
            if provider_changed || completed_input_with_tab {
                refresh_models = true;
            }
        }
        if confirm {
            match self.onboard.as_mut().map(|ob| ob.advance()) {
                Some(onboard::Advance::Done) => {
                    self.finish_onboard(cx);
                    return;
                }
                Some(onboard::Advance::GenerateBrain) => {
                    self.write_brain(cx);
                }
                Some(onboard::Advance::Blocked) => {
                    // The reason is on the wizard and the pane draws it above
                    // the footer, so the step stays put and nothing is lost.
                }
                Some(onboard::Advance::Moved)
                    if self
                        .onboard
                        .as_ref()
                        .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth) =>
                {
                    refresh_models = true;
                }
                Some(onboard::Advance::Moved) | None => {}
            }
        }
        if refresh_models {
            self.refresh_onboard_models();
        }
        cx.notify();
    }

    /// Start the brain writer on the prose the user typed into the brain step.
    /// The step stays up until the thread reports back, so the window never
    /// claims the files exist before they do.
    fn write_brain(&mut self, cx: &mut Context<Self>) {
        let picked = self
            .onboard
            .as_mut()
            .and_then(|ob| ob.brain_prompt().map(|prompt| (prompt, ob.workspace())));
        let Some((prompt, workspace)) = picked else {
            return;
        };
        let Some(bridge) = agent_bridge::bridge() else {
            // No service to write with: the step still finishes, its prose is
            // handed to the writer on save instead of silently dropped.
            if let Some(ob) = self.onboard.as_mut() {
                ob.mark_brain_failed("the agent is offline".to_string());
            }
            return;
        };
        if let Some(ob) = self.onboard.as_mut() {
            ob.mark_brain_generating();
        }
        self.live
            .notices
            .push("writing your brain files".to_string());
        agent_bridge::spawn_brain_generation(
            bridge.service.clone(),
            prompt,
            workspace,
            self.sink.clone(),
        );
        cx.notify();
    }

    /// The flow is over: write what the wizard collected and close it. A
    /// failure keeps the wizard on screen with the reason, because nothing was
    /// saved (#914).
    fn finish_onboard(&mut self, cx: &mut Context<Self>) {
        let finished = self
            .onboard
            .as_mut()
            .map(|ob| (ob.finish(), ob.workspace()));
        match finished {
            Some((onboard::Finish::Failed(e), _)) => {
                logging::error(&format!("onboarding save: {e}"));
                self.live.notices.push(format!("setup did not save: {e}"));
            }
            Some((onboard::Finish::Saved { brain_prompt }, workspace)) => {
                self.onboard = None;
                self.screen = Screen::Chat;
                self.refocus_screen();
                self.reload_agent(cx, Some("setup saved"));
                if let Some(prompt) = brain_prompt {
                    self.live
                        .notices
                        .push("generating personalized brain files".to_string());
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_brain_generation(
                            bridge.service.clone(),
                            prompt,
                            workspace,
                            self.sink.clone(),
                        );
                    }
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// Esc: one step back, or out of the wizard. Nothing is discarded that a
    /// confirmed step already wrote.
    fn onboard_escape(&mut self, cx: &mut Context<Self>) {
        if self
            .onboard
            .as_ref()
            .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth)
            && !self.models_query.is_empty()
        {
            self.models_query.clear();
            self.models_index = 0;
            if let Some(ob) = self.onboard.as_mut() {
                ob.wizard.ps.model_filter.clear();
                ob.wizard.ps.selected_model = 0;
            }
            cx.notify();
            return;
        }
        let stayed = self.onboard.as_mut().map(|ob| ob.escape()).unwrap_or(false);
        if stayed {
            cx.notify();
        } else {
            self.close_onboard(cx);
        }
    }

    fn close_onboard(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.onboard = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Bring the provider stack up on the config as it is on disk now.
    ///
    /// Saving the wizard is not enough by itself: a fresh install launches on
    /// the placeholder provider, and the brain files are the system prompt, so
    /// the stack has to be rebuilt on the settings that were just written. The
    /// terminal does the same thing with `rebuild_agent_service`.
    fn reload_agent(&mut self, cx: &mut Context<Self>, saved: Option<&str>) {
        let config = match Config::load() {
            Ok(config) => config,
            Err(e) => {
                self.live
                    .notices
                    .push(format!("saved, but the settings would not load: {e}"));
                cx.notify();
                return;
            }
        };
        self.agent_ready = false;
        if let Some(line) = saved {
            self.live.notices.push(line.to_string());
        }
        agent_bridge::spawn_bootstrap(config, self.sink.clone());
        cx.notify();
    }

    /// `/onboard` and the step commands. The wizard opens on the step the
    /// command named, carrying the live config, so the screen shows what is
    /// configured today rather than a blank form.
    fn open_onboard(&mut self, command: &'static str, arg: Option<String>, cx: &mut Context<Self>) {
        let Some(step) = onboard::step_for_command(command) else {
            // Every door in `GUI_SUPPORTED` names a step; if a future edit
            // adds one without a step, say so instead of opening at random.
            self.live
                .notices
                .push(format!("{command} has no onboarding step wired yet."));
            cx.notify();
            return;
        };
        // A bare `/onboard` is the full run, resumed where the user stopped, so
        // finishing it records the steps as done. A command that names one step
        // is a quick jump: it must not mark the run complete (#2060).
        let mut wizard = if command == "/onboard" {
            onboard::Onboard::open()
        } else {
            onboard::Onboard::open_at(step)
        };
        // `/channels whatsapp` opens that dialog, not the menu: the terminal
        // reads the same word off the line (#271). An unknown channel leaves
        // the menu up, which is what `open_channel` reports.
        if let Some(channel) = arg.as_deref() {
            wizard.open_channel(channel);
        }
        // On a machine that already answers, the provider step starts on the
        // pair the session is using instead of the first row of the catalogue
        // (#2060). A first run keeps the built-in preselection from `open_at`.
        if step != onboard::OnboardingStep::HealthCheck
            && let Some(bridge) = agent_bridge::bridge()
        {
            wizard.seed_session_pair(
                &bridge.service.provider_name(),
                &bridge.service.provider_model(),
            );
        }
        self.onboard = Some(wizard);
        if self
            .onboard
            .as_ref()
            .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth)
            && let Some(ob) = self.onboard.as_mut()
        {
            ob.field = 0;
        }
        self.models_query.clear();
        self.screen = Screen::Onboard;
        self.refresh_onboard_models();
        self.rail_actions = None;
        self.sessions.rail_menu = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Ask the service to switch. The answer arrives as an event on the UI
    /// thread, because a switch can do network work per candidate.
    fn switch_model(&mut self, arg: String, cx: &mut Context<Self>) {
        // A bare model id is allowed, the same as the terminal: the row it
        // names is resolved before the ask goes out.
        let pair = match models::find(&self.models_rows, &arg) {
            Some(row) => row.pair(),
            None => arg,
        };
        let Some(session) = self.transcript.session else {
            self.live.notices.push(
                "No conversation is open, so there is nothing to switch. Type /new first."
                    .to_string(),
            );
            cx.notify();
            return;
        };
        let Some(bridge) = agent_bridge::bridge() else {
            self.live
                .notices
                .push("The agent is still starting. Try again in a moment.".to_string());
            cx.notify();
            return;
        };
        agent_bridge::spawn_switch_model(bridge.service.clone(), session, pair, self.sink.clone());
        cx.notify();
    }

    /// ctrl+u: open the usage ledger, or return to the conversation. The panes
    /// never mix and the chat stays exactly where it was.
    fn toggle_usage(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Chat => self.screen = Screen::Usage,
            Screen::Usage => {
                self.screen = Screen::Chat;
                if let Some(id) = self.transcript.session
                    && self.transcript.turns.is_empty()
                    && self.transcript.hidden == 0
                    && self.transcript.error.is_none()
                {
                    self.open_session(id, cx);
                    return;
                }
            }
            Screen::Files => self.screen = Screen::Usage,
            Screen::MissionControl => self.screen = Screen::Usage,
            Screen::Models => self.screen = Screen::Usage,
            // Usage from the wizard: the screen closes first, so the ledger is
            // not left behind a half-filled form.
            Screen::Onboard => {
                self.onboard = None;
                self.screen = Screen::Usage;
            }
            Screen::Themes => self.close_themes(true, cx),
            // The skills list is a full screen too: ctrl+shift+u leaves it for
            // the ledger, the same as any other screen.
            Screen::Skills => self.screen = Screen::Usage,
            // The picker is transient: the chord just closes it, the way Esc
            // does, instead of hopping to the ledger.
            Screen::DirectoryPicker => self.screen = Screen::Chat,
        }
        self.refocus_screen();
        cx.notify();
    }

    /// The visible period switcher is another door into the same period
    /// loader as the terminal's t/w/m/a keys, not a second filter path.
    fn usage_period_switcher(&self, cx: &mut Context<Self>) -> Div {
        let mut bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(text("Period", Size::Xs, palette::current().text_muted));
        for (period, label, id) in [
            (Period::Today, "Today", "usage-period-today"),
            (Period::Week, "Week", "usage-period-week"),
            (Period::Month, "Month", "usage-period-month"),
            (Period::AllTime, "All time", "usage-period-all-time"),
        ] {
            let active = self.dash.period == period;
            bar = bar.child(
                usage_view::period_tab(label, active)
                    .id(id)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_usage_period(period, cx);
                    })),
            );
        }
        bar
    }

    fn set_usage_period(&mut self, period: Period, cx: &mut Context<Self>) {
        if !self.dash.set_period(period) {
            return;
        }
        logging::info(&format!("period -> {period:?}: fetching"));
        self.pending += 1;
        let pending = self.pending;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // The fetch runs on its own big-stack thread (see `load_view`):
            // gpui's background workers are dispatch-queue threads with tiny
            // stacks. The previous view stays visible until the result lands.
            let state = load_view(db_path, period).await;
            this.update(cx, |this, cx| {
                if pending == this.pending {
                    this.state = state;
                    logging::info("period fetch applied");
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// `/mission-control`: the window's copy of the terminal's screen. The
    /// panels are drawn from the same services the terminal reads, so the
    /// screen answers with live data rather than a mock row (#2426).
    fn open_mission_control(&mut self, cx: &mut Context<Self>) {
        self.onboard = None;
        self.rail_actions = None;
        self.screen = Screen::MissionControl;
        // A detail left open when the screen closed does not re-open: the
        // screen starts on its panels, and the row is clamped to the snapshot
        // that is on hand.
        self.mc_nav.detail = false;
        self.mc_nav.clamp(self.mc.as_ref());
        // A decision's message belongs to the visit that made it.
        self.mc_notice = None;
        // The four services are read once, on entry, exactly as the terminal
        // does on open. A frame never waits on SQLite.
        self.fetch_mission_control(cx);
        // The root takes the keyboard, as on the ledger: the panel keys are
        // not the composer's, and a stale editor focus would send a message.
        self.refocus_screen();
        cx.notify();
    }

    /// Read the Mission Control snapshot on the bridge's DB thread and apply
    /// it only if the screen is still asking for that same window; a stale
    /// read paints nothing.
    fn fetch_mission_control(&mut self, cx: &mut Context<Self>) {
        if self.mc_loading {
            return;
        }
        self.mc_loading = true;
        self.mc_err = None;
        let db_path = self.db_path.clone();
        let window = self.mc_window;
        cx.spawn(async move |this, cx| {
            let result = mission_control::load(db_path, window).await;
            this.update(cx, |this, cx| {
                this.mc_loading = false;
                if this.mc_window == window {
                    match result {
                        Ok(snapshot) => {
                            this.mc = Some(snapshot);
                            this.mc_err = None;
                        }
                        Err(e) => this.mc_err = Some(e.into()),
                    }
                    // A fresh read can be shorter than the last one; the
                    // selection follows it instead of pointing past the end.
                    this.mc_nav.clamp(this.mc.as_ref());
                } else {
                    // The window moved while this read was in flight, so this
                    // answer is for a window nobody is looking at: read the
                    // one that is showing now.
                    this.fetch_mission_control(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Esc, or `/usage` reached from here: back to the conversation with the
    /// chat exactly where it was left.
    fn close_mission_control(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// Mission Control's bare keys, the terminal's own bindings: Tab and `l`
    /// walk the panels, `h` and Shift-Tab walk them back, `j`/`k` and the
    /// arrows move the row, `g`/`G` jump to the ends, Enter opens the record.
    fn mission_control_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        // Chords belong to the window (zoom, the screen doors), never to a
        // panel: swallowing them would break the keys that leave the screen.
        if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
            return;
        }
        // The log viewer takes every key while it is open, the terminal's own
        // rule: a stray keystroke must not move a selection nobody can see.
        if self.mc_logs.is_some() {
            let key = ks.key.clone();
            let shift = ks.modifiers.shift;
            let close = self
                .mc_logs
                .as_mut()
                .is_some_and(|viewer| mission_control::log_key(viewer, &key, shift));
            if close {
                self.mc_logs = None;
            }
            cx.notify();
            return;
        }
        // Uppercase L opens the log viewer, because lowercase `l` cycles the
        // panels, exactly as the terminal binds it (#1528).
        if ks.modifiers.shift && ks.key == "l" {
            let mut viewer = LogViewerState::open();
            // Land on the newest entries: the thing just reproduced is at the
            // end of the file, not the start of the window.
            viewer.scroll_to_end();
            self.mc_logs = Some(viewer);
            self.mc_nav.detail = false;
            cx.notify();
            return;
        }
        // The window filter the footer advertises: D/W/M/A, the terminal's
        // capitals, so the bare letters stay free for apply and reject. A new
        // window is a new read: the numbers are the services' to produce,
        // never arithmetic done in the frame. Asking again for the window
        // already on screen is not a read at all (#2426).
        if ks.modifiers.shift {
            match mission_control::window_key(self.mc_window, &ks.key) {
                mission_control::WindowKey::Switch(window) => {
                    self.mc_window = window;
                    self.fetch_mission_control(cx);
                    return;
                }
                // The window is already the one asked for: the capital is
                // still the filter's, so it is swallowed rather than falling
                // through to the inbox, where `a` applies a proposal.
                mission_control::WindowKey::Same => return,
                mission_control::WindowKey::Pass => {}
            }
        }
        // Decide a proposal: `a` installs it, `r` archives it, the terminal's
        // own letters. Lowercase on purpose: the capitals are already the
        // analytics window filter, so `A` re-windows and `a` applies.
        let decision = match ks.key.as_str() {
            "a" => Some(mission_control::Decision::Apply),
            "r" => Some(mission_control::Decision::Reject),
            _ => None,
        };
        if let Some(decision) = decision {
            self.decide_selected(decision, cx);
            return;
        }
        match ks.key.as_str() {
            "tab" | "l" => self.mc_nav.focus_next(self.mc.as_ref()),
            "backtab" | "h" => self.mc_nav.focus_prev(self.mc.as_ref()),
            "up" | "k" => self.mc_nav.move_selection(self.mc.as_ref(), -1),
            "down" | "j" => self.mc_nav.move_selection(self.mc.as_ref(), 1),
            "home" => self.mc_nav.home(),
            "end" => self.mc_nav.end(self.mc.as_ref()),
            "g" if ks.modifiers.shift => self.mc_nav.end(self.mc.as_ref()),
            "g" => self.mc_nav.home(),
            "enter" => {
                if !self.mc_nav.detail {
                    self.mc_nav.open_detail(self.mc.as_ref());
                }
            }
            _ => return,
        }
        self.mc_nav.clamp(self.mc.as_ref());
        cx.notify();
    }

    /// `a` / `r`: apply or reject the selected proposal through the same
    /// `RsiProposalsTool` the terminal and the agent use, so an applied
    /// proposal is byte-identical whichever client asked (#2426).
    ///
    /// Both keys are the inbox's, as in the terminal: the other panels hold
    /// no proposal to decide.
    fn decide_selected(&mut self, decision: mission_control::Decision, cx: &mut Context<Self>) {
        if self.mc_nav.panel != mission_control::Panel::Inbox {
            return;
        }
        let Some(item) = self
            .mc
            .as_ref()
            .and_then(|snapshot| snapshot.inbox.get(self.mc_nav.selected))
            .cloned()
        else {
            return;
        };
        let verb = match decision {
            mission_control::Decision::Apply => "Apply",
            mission_control::Decision::Reject => "Reject",
        };
        let Some(bridge) = agent_bridge::bridge() else {
            self.mc_notice = Some(mission_control::Notice {
                text: format!("{verb} failed: the agent is not running"),
                ok: false,
            });
            cx.notify();
            return;
        };
        let service = bridge.service.clone();
        cx.spawn(async move |this, cx| {
            let result = mission_control::decide(service, item.id, item.kind, decision).await;
            this.update(cx, |this, cx| {
                this.mc_notice = Some(match result {
                    Ok(text) => mission_control::Notice { text, ok: true },
                    Err(e) => mission_control::Notice {
                        text: format!("{verb} failed: {e}"),
                        ok: false,
                    },
                });
                // A decided record must not leave its popup describing the
                // row that took its place: the overlay closes, and the list
                // plus the notice say what happened.
                this.mc_nav.detail = false;
                // The inbox is one row shorter now, so every panel is read
                // again: the selection follows the list the row left behind.
                this.mc_loading = false;
                this.fetch_mission_control(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// ctrl+shift+t: the themes screen. Opening it remembers the palette the
    /// window shows now; closing it without keeping restores that palette.
    fn toggle_themes(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Themes {
            self.close_themes(true, cx);
            return;
        }
        self.themes_origin = Some(palette::current());
        self.themes_pick = None;
        // The keyboard starts at the top of the list; the row that is already
        // in force is marked as selected, so the eye finds it either way.
        self.themes_index = 0;
        self.themes_history.clear();
        self.rail_actions = None;
        self.screen = Screen::Themes;
        // The window's root takes the keyboard, not the composer: on this
        // screen the arrows walk the list and Enter previews, so a stale
        // editor focus would send a message instead.
        self.refocus_screen();
        cx.notify();
    }

    /// Leave the themes screen. `restore` puts back the palette from before
    /// the screen opened: Esc and the toggle use it, Keep does not.
    fn close_themes(&mut self, restore: bool, cx: &mut Context<Self>) {
        if let Some(origin) = self.themes_origin.take()
            && restore
        {
            palette::restore(origin);
        }
        self.themes_pick = None;
        self.themes_history.clear();
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// `/cd` with no argument: the directory picker screen (#2527). The
    /// terminal opens its dialog on the session's working directory; the
    /// window opens the same screen on the same seed, so both surfaces
    /// answer the same key with the same screen. Needs an open conversation
    /// like every other session-bound command here.
    fn open_cd_picker(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.transcript.session else {
            self.live
                .notices
                .push("No conversation is open. Type /new first.".to_string());
            cx.notify();
            return;
        };
        let here = agent_bridge::bridge()
            .map(|b| b.service.get_working_directory_for_session(session))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        self.cd_dir = here;
        self.cd_show_hidden = false;
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, false);
        self.rail_actions = None;
        self.screen = Screen::DirectoryPicker;
        // The screen takes the keyboard, the same rule as Themes: on this
        // screen the arrows walk the list, so a stale editor focus would
        // send a message instead.
        self.refocus_screen();
        cx.notify();
    }

    /// Leave the picker: nothing is applied, the conversation comes back.
    fn close_cd_picker(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// Tab/Space on a row: make it the session's working directory. The
    /// conversation comes back FIRST and the apply follows in a spawned
    /// task, the ordering the terminal's picker was just fixed to use
    /// (#2526): a slow persistence write must never hold the screen open.
    fn cd_select_here(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.transcript.session else {
            self.close_cd_picker(cx);
            return;
        };
        let dir = self.cd_dir.clone();
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let applied = match agent_bridge::bridge() {
                Some(b) => b.service.apply_operator_cd(session, &dir).await,
                None => dir.clone(),
            };
            let _ = this.update(cx, |this, cx| {
                this.live
                    .notices
                    .push(text_commands::cd_reply(&applied.display().to_string()));
                cx.notify();
            });
        })
        .detach();
    }

    /// Enter on a row: descend into it. `..` climbs back up one level.
    /// Reads are synchronous and local, the way the terminal's dialog does
    /// them, so the row list can never drift from what the screen shows.
    fn cd_descend(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(next) = self.cd_entries.get(index) else {
            return;
        };
        self.cd_dir = next.clone();
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, self.cd_show_hidden);
        cx.notify();
    }

    /// `.` in the picker: show or hide the dotfile directories, the same
    /// toggle the terminal's dialog binds.
    fn cd_toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.cd_show_hidden = !self.cd_show_hidden;
        self.cd_selected = 0;
        self.cd_entries = cd_entries_for(&self.cd_dir, self.cd_show_hidden);
        cx.notify();
    }

    /// A row click or Enter on the highlighted row: show the theme on the window,
    /// without saving it. The preview is remembered in the history list so the
    /// same theme can be walked back to.
    fn preview_theme(&mut self, name: String, cx: &mut Context<Self>) {
        palette::preview(&name);
        remember_preview(&mut self.themes_history, &name);
        self.themes_pick = Some(name);
        cx.notify();
    }

    /// Keep: save the previewed theme under the key for the system appearance
    /// the window is showing right now. Writes `[gui] dark_theme` or
    /// `[gui] light_theme`, so the other appearance keeps its own choice.
    fn keep_theme(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.themes_pick.clone() else {
            self.close_themes(true, cx);
            return;
        };
        let key = if palette::system_is_dark() {
            "dark_theme"
        } else {
            "light_theme"
        };
        match Config::write_key_string("gui", key, &name) {
            Ok(()) => logging::info(&format!("themes: kept '{name}' as [gui] {key}")),
            Err(e) => logging::error(&format!("themes: could not save '{name}': {e}")),
        }
        self.themes_origin = None;
        self.themes_pick = None;
        self.themes_history.clear();
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// ctrl+h, or Cmd+? on a Mac: the key reference, the window's door to the
    /// rows the terminal prints via `/help` (#2320). It is read-only, so it
    /// can sit above every other affordance: closing it leaves the screen
    /// exactly as it was, and opening it disarms the rail's row strip so a
    /// click meant for the panel cannot delete anything behind it.
    fn toggle_help(&mut self, cx: &mut Context<Self>) {
        self.help_open = !self.help_open;
        if self.help_open {
            self.rail_actions = None;
        } else {
            // Closing the reference is a return to the conversation, so the
            // caret goes back where typing is expected.
            self.refocus_screen();
        }
        cx.notify();
    }

    /// ctrl+g: the open conversation's tracked files, or back to the chat. A
    /// no-op without an open conversation, because a files browser for no
    /// session is a lie. The TUI reaches the same list with `f` on the
    /// sessions browser (`AppMode::SessionFiles`).
    fn toggle_files(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Chat {
            self.screen = Screen::Chat;
            self.refocus_screen();
            cx.notify();
            return;
        }
        let Some(sid) = self.transcript.session else {
            logging::info("ctrl-g ignored: no open conversation");
            return;
        };
        self.screen = Screen::Files;
        self.files.clear();
        self.files_err = None;
        self.files_for = Some(sid);
        self.fetch_files(sid, cx);
        self.refocus_screen();
        cx.notify();
    }

    /// Fetch the session's tracked files on the bridge's DB thread and apply
    /// them only if the user still sits on the files screen of the same
    /// session; a stale fetch paints nothing.
    fn fetch_files(&mut self, sid: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = agent_bridge::load_files(db_path, sid).await;
            this.update(cx, |this, cx| {
                if this.files_for == Some(sid) && this.screen == Screen::Files {
                    match result {
                        Ok(files) => this.files = files,
                        Err(e) => this.files_err = Some(e.into()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Leave the rail filter. The query is a transient find aid, so every
    /// exit clears it: the rail returns to the full list and never hides a
    /// filter behind a closed field.
    fn close_rail_filter(&mut self, cx: &mut Context<Self>) {
        self.sessions.filter.clear();
        self.sessions.filter_active = false;
        self.refocus_screen();
        cx.notify();
    }

    /// Drop the rail cursor when the row it points at is no longer shown,
    /// so the keyboard never arms a ghost (#2351). Clearing the filter only
    /// ever adds rows back, so it needs no call here.
    fn normalize_cursor(&mut self) {
        if !self.sessions.cursor_visible() {
            self.sessions.cursor = None;
        }
    }

    /// The session-files browser body: one row per tracked file, name on the
    /// left, tilde path and size on the right. Real rows from
    /// `FileService::list_files_for_session`, never placeholders.
    fn files_view(&self) -> Div {
        let mut body = div().flex().flex_col().gap(rems(0.75));
        if let Some(err) = &self.files_err {
            return body.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette::current().text_muted))
                    .child(err.clone()),
            );
        }
        if self.files.is_empty() {
            return body.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette::current().text_faint))
                    .child("no files tracked yet"),
            );
        }
        for f in &self.files {
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.path.display().to_string());
            let mut meta = tilde_path(&f.path);
            if let Some(size) = f.size {
                meta.push_str(&format!(" · {}", human_size(size.max(0) as usize)));
            }
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(rems(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(rgb(palette::current().text_normal))
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(meta)),
                    ),
            );
        }
        body
    }

    /// A file drag is over the window right now. The moves arrive at the poll
    /// cadence, so a few missed frames means the pointer left: no drag, no
    /// overlay, and the next real drag re-arms it.
    fn file_drag_active(&self) -> bool {
        self.drag_seen
            .is_some_and(|seen| seen.elapsed() < std::time::Duration::from_millis(400))
    }

    /// Send typed text as an agent turn (or queue it behind the running
    /// one). Called from the editor's Sent subscription.
    fn send_message(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // Commands are read before every other gate, because the two that
        // matter most are needed exactly when the window is busy or broken:
        // `/stop` has nothing to stop on an idle window, and `/help` is what a
        // user reaches for when the agent is offline. A line that starts with
        // a slash never becomes a turn (#2374).
        if let Some(outcome) = commands::parse_command(&text) {
            self.run_command(outcome, cx);
            return;
        }
        // `!cmd` runs a shell command here, not a turn, as the terminal does (#2437).
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            bang::spawn(cmd.to_string(), self.sink.clone());
            self.live.notices.push(format!("! {cmd}"));
            cx.notify();
            return;
        }
        // A closed topic refuses new turns. The notice says why, so the send is
        // never dropped without a word (#2305). It sits after the parse above
        // because a command is not a turn: `/new` and `/sessions` still work
        // from a closed topic.
        if self
            .transcript
            .session
            .is_some_and(|id| self.sessions.is_closed(id))
        {
            self.live
                .notices
                .push("This topic is closed. Reopen it to continue.".to_string());
            cx.notify();
            return;
        }
        if self.agent_error.is_some() {
            self.live.notices.push(format!(
                "agent offline: {}",
                self.agent_error.as_deref().unwrap_or("bootstrap failed")
            ));
            cx.notify();
            return;
        }
        if self.busy {
            // The message joins the session's queue, which the agent drains
            // between tool calls, the terminal's own path (#2441). With no
            // session open it waits in the window's own list.
            match self.transcript.session {
                Some(sid) => {
                    agent_bridge::queue_push(sid, agent_bridge::PendingMessage::plain(text))
                }
                None => self.queued.push(agent_bridge::PendingMessage::plain(text)),
            }
            cx.notify();
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the message, it fires on Ready. The queue
            // strip is where the user sees it.
            self.queued.push(agent_bridge::PendingMessage::plain(text));
            cx.notify();
            return;
        };
        // The turn goes to the conversation on screen. With none open there
        // is nothing to write into, so one is created and the text waits in
        // the queue rather than vanishing into a session nobody is looking at.
        let Some(sid) = self.transcript.session else {
            self.queued.push(agent_bridge::PendingMessage::plain(text));
            self.new_session(cx);
            return;
        };
        self.live.begin(text.clone());
        self.transcript.live_expanded = false;
        self.settle_retries = 0;
        // The finished turn must add one to the persisted count before the
        // live copy may be dropped (see `reload_transcript`).
        self.settle_expect = Some(self.transcript.turns.len() + 1);
        self.busy = true;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.cancel = Some(cancel.clone());
        // Attachments are tracked against the session the same way the TUI
        // does it (`FileService::get_or_create_file`), so the file lands in
        // the session's artifacts, not just in the message text.
        let attached = editor::marker_paths(&text);
        agent_bridge::spawn_turn(bridge.service.clone(), sid, text, self.sink.clone(), cancel);
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
        }
        cx.notify();
    }

    /// Drain the bridge channel: fold progress into the live view, finalize
    /// the turn on Done/Failed, reload the transcript so the DB becomes the
    /// rendering.
    fn poll(&mut self, cx: &mut Context<Self>) {
        // Retire a drag whose moves stopped before the drop arrived: the
        // window must not stay tinted after the pointer leaves it.
        if self.drag_seen.is_some() && !self.file_drag_active() {
            self.drag_seen = None;
            cx.notify();
        }
        let models_changed = self.poll_model_fetches();
        if models_changed {
            cx.notify();
        }
        let events = agent_bridge::drain(&self.agent_rx);
        if events.is_empty() {
            // A live turn needs a repaint even when no event arrived: that is
            // what advances the spinner frame and the `(5s ...)` clock. The
            // old early return froze the line between chunks, which read as
            // "nothing is happening" on a slow provider.
            if self.busy {
                cx.notify();
            }
            return;
        }
        let mut reload: Option<Uuid> = None;
        for ev in events {
            // Sub-agents run their own sessions: their tool rows, tokens and
            // notices belong to a conversation the user is not reading, so
            // they are dropped here rather than painted into this pane.
            if let agent_bridge::TurnEvent::ProgressFor(sid, _) = &ev
                && self.transcript.session != Some(*sid)
            {
                continue;
            }
            match ev {
                agent_bridge::TurnEvent::Ready => {
                    self.agent_ready = true;
                    logging::info("agent ready");
                    // The footer's denominator comes from the service, the
                    // same number compaction enforces for this session.
                    if let Some(b) = agent_bridge::bridge() {
                        self.ctx_max = match self.transcript.session {
                            Some(sid) => b.service.context_limit_for_session(sid),
                            None => b
                                .service
                                .context_window_for_model(&b.service.provider_model()),
                        };
                    }
                    // A window opened on an empty ledger has no conversation
                    // to send into: make one before the first message can be
                    // typed into nothing.
                    if self.transcript.session.is_none() {
                        self.new_session(cx);
                    }
                    self.drain_pending(cx);
                }
                agent_bridge::TurnEvent::SessionCreated(id) => {
                    logging::info(&format!("session created {id}"));
                    self.refresh_rail(cx);
                    let for_split = std::mem::take(&mut self.split_creating);
                    if for_split && self.split_pick.is_some() {
                        self.choose_split(id, cx);
                    } else {
                        self.open_session(id, cx);
                    }
                }
                agent_bridge::TurnEvent::SessionCreateFailed(e) => {
                    self.live.notices.push(format!("new session: {e}"));
                }
                agent_bridge::TurnEvent::UpdateAvailable(version) => {
                    self.update_available = Some(version);
                    cx.notify();
                }
                agent_bridge::TurnEvent::MessagesCleared(id) => {
                    if self.transcript.session == Some(id) {
                        self.transcript.turns = Vec::new();
                        self.transcript.hidden = 0;
                        self.transcript.error = None;
                    }
                    self.live.notices.push("Session cleared.".to_string());
                    cx.notify();
                }
                agent_bridge::TurnEvent::ClearFailed(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::BangOutput(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::EvolveDone(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                // A text command answered on the bridge: one line, the same
                // wording the terminal prints (#2469).
                agent_bridge::TurnEvent::CommandReply(text) => {
                    self.live.notices.push(text);
                    cx.notify();
                }
                // A text command that needs one visible turn: `/plan <query>`,
                // `/execute` seeding the checklist, or `/goal`.
                agent_bridge::TurnEvent::CommandTurn(text) => {
                    self.send_message(text, cx);
                }
                agent_bridge::TurnEvent::SessionDeleted(id) => {
                    logging::info(&format!("session deleted {id}"));
                    if self.transcript.session == Some(id) {
                        // The open pane pointed at a row that no longer is:
                        // clear it rather than leave a transcript of a
                        // deleted conversation on screen.
                        self.transcript.session = None;
                        self.transcript.turns.clear();
                        self.live.clear();
                        self.busy = false;
                        // The files screen of a deleted session has nothing
                        // left to list: back to the chat, never a ghost list.
                        if self.screen == Screen::Files {
                            self.screen = Screen::Chat;
                        }
                    }
                    self.refresh_rail(cx);
                }
                agent_bridge::TurnEvent::SessionDeleteFailed(e) => {
                    self.live.notices.push(format!("delete session: {e}"));
                }
                agent_bridge::TurnEvent::PlanPath { session, path } => {
                    // Stale lookups (user switched conversations while the
                    // DB read ran) are dropped, not painted.
                    if self.plan_for == Some(session) {
                        self.plan.set_path(path);
                        cx.notify();
                    }
                }
                agent_bridge::TurnEvent::BootstrapFailed(e) => {
                    logging::error(&format!("agent offline: {e}"));
                    self.agent_error = Some(e.clone());
                    self.live.notices.push(format!("agent offline: {e}"));
                }
                agent_bridge::TurnEvent::ModelSwitched(line) => {
                    logging::info(&format!("model switched: {line}"));
                    self.live.notices.push(line);
                    // The rail row carries the pair the service answers with,
                    // so a switch is only real once the row is re-read.
                    self.refresh_rail(cx);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ModelSwitchFailed(e) => {
                    logging::error(&format!("model switch: {e}"));
                    self.live.notices.push(format!("model switch: {e}"));
                    cx.notify();
                }
                agent_bridge::TurnEvent::BrainReady(line) => {
                    logging::info(&format!("brain: {line}"));
                    self.live.notices.push(format!("brain: {line}"));
                    // A brain written from the wizard's own step ends that
                    // step; the setup itself is saved by the same finish path
                    // the Enter key uses, so the two routes cannot diverge.
                    let mut step_done = false;
                    if let Some(ob) = self.onboard.as_mut()
                        && ob.step() == onboard::OnboardingStep::BrainSetup
                    {
                        ob.mark_brain_generated();
                        step_done = matches!(ob.advance(), onboard::Advance::Done);
                    }
                    if step_done {
                        self.finish_onboard(cx);
                    } else {
                        // The brain files are the system prompt, so the stack
                        // that just answered does not carry them yet: reload it
                        // the same way a saved config does, and the next answer
                        // does.
                        self.reload_agent(cx, None);
                    }
                }
                agent_bridge::TurnEvent::BrainFailed(e) => {
                    logging::error(&format!("brain: {e}"));
                    self.live
                        .notices
                        .push(format!("brain generation failed: {e}"));
                    // The step keeps the reason and stays up: Enter still
                    // finishes the setup, so a failed writer never traps the
                    // user, and the prose is retried on save.
                    if let Some(ob) = self.onboard.as_mut()
                        && ob.step() == onboard::OnboardingStep::BrainSetup
                    {
                        ob.mark_brain_failed(e);
                    }
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::Thinking) => {
                    self.live.thinking = true;
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::StreamingChunk { text }) => {
                    // Counted, not just appended: the live line's token number
                    // comes from the chunk itself, so it moves as the answer
                    // streams instead of appearing with the final answer.
                    self.live.thinking = false;
                    self.live.note_chunk(&text);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::IntermediateText { text, reasoning },
                ) => {
                    // Narration between tool calls is part of the answer the
                    // user is reading, so it joins the stream instead of
                    // opening a second copy somewhere else on screen. The
                    // reasoning that came with it is shown live, above the
                    // answer, the way the TUI shows it.
                    // The provider reports the same narration twice: once as
                    // streamed chunks and again as intermediate text. Appending
                    // both is what drew the paragraph twice, so both paths
                    // dedupe against what is already on screen.
                    self.live.thinking = false;
                    if let Some(reasoning) = reasoning {
                        self.live.push_reasoning(&reasoning);
                    }
                    self.live.push_narration(&text);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::ToolStarted {
                        tool_name,
                        tool_input,
                        ..
                    },
                ) => {
                    self.live.thinking = false;
                    // The ledger's own wording (`tui::app::messaging`), so a
                    // live row reads exactly like the row that persists.
                    let description =
                        opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
                    self.live
                        .push_tool(transcript::ToolCall::running(tool_name, description));
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::ToolCompleted {
                        tool_name, success, ..
                    },
                ) => {
                    // Close the newest open row this call opened; with no open
                    // row (an event lost across a mode switch) draw it closed
                    // rather than dropping the call from the turn.
                    //
                    // Rows carrying a `task_id` are detached commands: their own
                    // finish settles them, so a synchronous call must never close
                    // one that happens to share its tool name.
                    let open = self
                        .live
                        .tools
                        .iter_mut()
                        .rev()
                        .find(|c| !c.done && c.name == tool_name && c.task_id.is_none());
                    match open {
                        Some(call) => {
                            call.done = true;
                            call.success = success;
                        }
                        None => self.live.push_tool(transcript::ToolCall {
                            name: tool_name.clone(),
                            description: tool_name,
                            success,
                            done: true,
                            task_id: None,
                        }),
                    }
                }
                // Every API response and tool execution reports the context
                // size it just ran with, so the footer tracks the turn while
                // it is still in flight.
                // The agent took a queued message between tool calls: it shows
                // in the live turn right away, where the terminal puts it (#2441).
                agent_bridge::TurnEvent::ProgressFor(
                    sid,
                    ProgressEvent::QueuedUserMessage { text },
                ) if self.transcript.session == Some(sid) => {
                    self.live.injected.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::TokenCount(count)) => {
                    self.ctx_tokens = Some(count.min(u32::MAX as usize) as u32);
                }
                // A detached command's row lands synchronously, while the
                // turn that spawned it is still live, and its finish finds it
                // again by `task_id` (#2146). Keyed by id, never by position:
                // two parallel calls would otherwise settle each other's rows.
                agent_bridge::TurnEvent::ProgressFor(
                    sid,
                    ProgressEvent::DetachedTaskStarted { task_id, label },
                ) => {
                    if Some(sid) == self.transcript.session {
                        self.live.thinking = false;
                        self.live
                            .push_tool(transcript::ToolCall::detached(task_id, label));
                    }
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::DetachedTaskFinished {
                        task_id,
                        label,
                        success,
                        elapsed_secs,
                    },
                ) => {
                    // A finish with no row is not an error: the turn may have
                    // been settled, or the task outlived a restart. The receipt
                    // still arrives as `BackgroundTaskDone`.
                    if !self.settle_detached_row(&task_id, success, elapsed_secs) {
                        logging::debug(&format!(
                            "detached task {task_id} ({label}) finished with no row to settle"
                        ));
                    }
                }
                // Compaction notices and queue echoes: the phase-2 transcript
                // mirror. They persist to the DB and show on the reload;
                // nothing to draw live yet.
                agent_bridge::TurnEvent::ProgressFor(_, _) => {}
                agent_bridge::TurnEvent::Done {
                    content,
                    context_tokens,
                    tokens_per_second,
                } => {
                    // The provider's own prompt size for the call that just
                    // answered: the honest number for the ctx footer, not an
                    // estimate of what we think we sent.
                    self.ctx_tokens = Some(context_tokens);
                    // The same numbers, frozen for THIS turn, so the settled
                    // card can print what the turn cost instead of whatever
                    // the session's budget reads later (#2324).
                    if let Some(sid) = self.transcript.session {
                        self.pending_metrics = Some((
                            sid,
                            transcript::TurnMetrics {
                                ctx_used: context_tokens,
                                ctx_max: self.ctx_max,
                                tps: tokens_per_second,
                            },
                        ));
                    }
                    // Freeze the turn's rate on the same measure every other
                    // surface uses: the service's own tokens_per_second when
                    // it reported one, else the local active-streaming
                    // estimate. The footer and the settled card then agree.
                    self.live.finalize(tokens_per_second);
                    // The answer stays on screen: the stream when chunks
                    // arrived, otherwise the response the turn returned. Live
                    // state clears only once the reload proves the DB has it.
                    if self.live.stream.trim().is_empty() && !content.trim().is_empty() {
                        self.live.settled = Some(content.clone());
                    }
                    self.busy = false;
                    self.live.thinking = false;
                    // One line per turn, with the stream's own numbers: a
                    // turn that rendered as one silent dump is diagnosable
                    // from the log (chunks=0 means the provider never
                    // streamed, not that the window failed to draw).
                    logging::info(&format!(
                        "turn done ({} chars, {} chunks, {} bytes, first {:?}, {}s, {} tok), reloading transcript",
                        content.len(),
                        self.live.chunks,
                        self.live.chunk_bytes,
                        self.live.first_chunk,
                        self.live.elapsed_secs(),
                        self.live.tokens
                    ));
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::Failed(msg) => {
                    self.live.notices.push(msg);
                    // A failed turn has no ctx or rate to report; a stale
                    // record would land on whatever turn shows up next.
                    self.pending_metrics = None;
                    self.busy = false;
                    self.live.thinking = false;
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::BackgroundTaskDone {
                    session_id,
                    context_text,
                    display_text,
                } => {
                    // The whole point of backgrounding: the command ends and
                    // the agent is told, so the work is reported instead of the
                    // window going quiet with the turn already finished.
                    //
                    // The window runs one turn, on the conversation it has
                    // open (`agent_bridge` module doc). A completion for any
                    // other session is logged, not silently dropped: resuming a
                    // session nobody is looking at needs its own queue and turn
                    // runner, which is #2322's documented gap.
                    if Some(session_id) != self.transcript.session {
                        logging::warn(&format!(
                            "background task finished on session {session_id} while session {:?} is open: not resumed",
                            self.transcript.session
                        ));
                    } else if self.busy {
                        // Queue it behind the running turn rather than
                        // interrupting it: `QueuedUserMessage::join` keeps the
                        // system half out of what the user "said" (#765).
                        self.queued.push(agent_bridge::PendingMessage::system(
                            context_text,
                            display_text,
                        ));
                    } else {
                        self.resume_finished_task(session_id, context_text, display_text, cx);
                    }
                }
            }
        }
        if let Some(sid) = reload {
            self.reload_transcript(sid, cx);
        }
        // A finished turn is the moment the queue drains: before this it only
        // drained on `Ready`, so a message typed while busy never went out.
        if !self.busy {
            self.cancel = None;
            self.drain_pending(cx);
        }
        self.transcript_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The running detached command, formatted exactly as the TUI formats it.
    ///
    /// `None` when nothing runs, so the strip is omitted rather than drawn
    /// empty. Elapsed time is read from each task's own `Instant` at paint
    /// time, which is why the label ticks off the existing 50 ms poll with no
    /// extra timer.
    fn running_tasks_label(&self) -> Option<String> {
        let session = self.transcript.session?;
        let bridge = agent_bridge::bridge()?;
        let mgr = bridge.service.background_manager()?;
        let tasks: Vec<(String, u64)> = mgr
            .running_tasks(session)
            .into_iter()
            .map(|t| (t.label, t.started.elapsed().as_secs()))
            .collect();
        opencrabs::tui::render::format_background_tasks(&tasks, opencrabs::tui::render::LABEL_CHARS)
    }

    /// Settle one detached row in the live turn, if it still holds it.
    fn settle_detached_row(&mut self, task_id: &str, success: bool, elapsed_secs: f32) -> bool {
        transcript::settle_detached(&mut self.live.tools, task_id, success, elapsed_secs)
    }

    /// Run a turn on the open conversation for a finished background task.
    ///
    /// Same bookkeeping as `drain_pending`: the resumed turn must look like any
    /// other turn, so `live.begin` seeds the panel and `settle_expect` counts the
    /// row the answer will land in. Without a service there is nowhere to send
    /// it, so it queues and drains on `Ready` instead of vanishing.
    fn resume_finished_task(
        &mut self,
        session_id: Uuid,
        context_text: String,
        display_text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = agent_bridge::bridge() else {
            self.queued.push(agent_bridge::PendingMessage::system(
                context_text,
                display_text,
            ));
            return;
        };
        self.live.begin(display_text);
        self.transcript.live_expanded = false;
        self.settle_retries = 0;
        self.settle_expect = Some(self.transcript.turns.len() + 1);
        self.busy = true;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.cancel = Some(cancel.clone());
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session_id,
            context_text,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session_id, attached);
        }
        cx.notify();
    }

    /// Send everything queued while busy, joined into one turn.
    /// What the strip shows: the window's own list, then the session's queue.
    fn queued_for_strip(&self) -> Vec<agent_bridge::PendingMessage> {
        let mut out = self.queued.clone();
        if let Some(sid) = self.transcript.session {
            out.extend(agent_bridge::queue_snapshot(sid));
        }
        out
    }

    fn drain_pending(&mut self, cx: &mut Context<Self>) {
        let waiting_in_store = self
            .transcript
            .session
            .is_some_and(|sid| !agent_bridge::queue_snapshot(sid).is_empty());
        if (self.queued.is_empty() && !waiting_in_store) || self.busy {
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            self.queued.clear();
            return;
        };
        // No open conversation: keep the queue and open one. The queue drains
        // again when that session lands (`SessionCreated`).
        let Some(sid) = self.transcript.session else {
            self.new_session(cx);
            return;
        };
        let mut msgs = std::mem::take(&mut self.queued);
        msgs.extend(agent_bridge::queue_take(sid));
        if let Some(joined) = agent_bridge::PendingMessage::join(&msgs) {
            self.live.begin(joined.display_text.clone());
            self.transcript.live_expanded = false;
            self.settle_retries = 0;
            self.settle_expect = Some(self.transcript.turns.len() + 1);
            self.busy = true;
            let cancel = tokio_util::sync::CancellationToken::new();
            self.cancel = Some(cancel.clone());
            let attached = editor::marker_paths(&joined.context_text);
            agent_bridge::spawn_turn(
                bridge.service.clone(),
                sid,
                joined.context_text,
                self.sink.clone(),
                cancel,
            );
            if !attached.is_empty() {
                agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
            }
        }
        cx.notify();
    }

    /// Create a conversation and open it. The rail is refreshed from the
    /// ledger, never patched by hand: the new row is real data or it does
    /// not appear.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        // Same inheritance rule as the TUI: the row carries the provider and
        // model the service is actually answering with, so a later read of
        // the session cannot report a model that never ran.
        let (provider, model) = match agent_bridge::bridge() {
            Some(b) => (
                Some(b.service.provider_name()),
                Some(b.service.provider_model()),
            ),
            None => (None, None),
        };
        agent_bridge::spawn_create_session(
            self.db_path.clone(),
            self.sink.clone(),
            provider,
            model,
        );
        self.refocus_screen();
        cx.notify();
    }

    /// Pin or unpin one conversation, then re-read the pinned set from the
    /// ledger. The star flips on the click so the rail never lags a slow
    /// write, and the refetch replaces the guess with what was stored.
    fn toggle_favorite(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let pinned = self.sessions.is_favorite(id);
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_favorite(db_path, id, !pinned).await {
                logging::error(&format!("favorite {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.live.notices.push(format!("favorite: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            this.update(cx, |this, cx| this.refresh_favorites(cx)).ok();
        })
        .detach();
        cx.notify();
    }

    /// Turn topics on for a session from the rail menu (#2302). The write is
    /// durable: the flag is read back from the database, never kept only here.
    fn enable_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_topics(db_path.clone(), id, true).await {
                logging::error(&format!("enable topics {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.live.notices.push(format!("enable topics: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            let result = sessions::load_topics(db_path).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(ids) => this.sessions.topics = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("topics refresh: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Re-read the pinned ids. A separate fetch from the rows because the
    /// sidecar can change on its own (the TUI pins too).
    fn refresh_favorites(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = sessions::load_favorites(db_path).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(ids) => this.sessions.favorites = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("favorites refresh: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Arm the delete dialog. Every delete path (a row's strip, and nothing
    /// else) lands here instead of erasing, so the erase is always the second,
    /// deliberate click.
    fn request_delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = Some(id);
        cx.notify();
    }

    /// Open a file a tile stands for. The overlay decides what to draw from the
    /// path alone, so this stays one assignment: no state is derived at click
    /// time that could disagree with what the overlay renders (#2395).
    fn open_preview(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let open = preview::Preview::of(path);
        // A clip is the only kind whose overlay needs the disk: a poster frame
        // has to be extracted, and its length read. Doing it here, on the
        // click, is what keeps ffmpeg off the render path.
        self.preview_video = if open.kind == preview::PreviewKind::Video {
            Some(preview::VideoFacts::probe(
                &opencrabs::config::opencrabs_home(),
                &open.path,
            ))
        } else {
            None
        };
        // One line per open, so a click that lands and a click that never
        // arrives can be told apart in the GUI log instead of guessed at
        // (#2395). The path is the marker's own, which is the string the
        // overlay will draw.
        logging::info(&format!(
            "preview: open {} ({:?})",
            open.path.display(),
            open.kind
        ));
        self.preview = Some(open);
        self.preview_zoom = 1.0;
        cx.notify();
    }

    /// The inside of the preview: the part that differs per kind. The shell
    /// (title, kind, esc) is the same for all three, so a picture, a clip and
    /// a document cannot drift apart in how they open or close.
    fn preview_body(&self, open: &preview::Preview, cx: &mut Context<Self>) -> gpui::AnyElement {
        let path = open.path.clone();
        let open_file = {
            let path = path.clone();
            cx.listener(move |_this, _ev, _window, _cx| {
                preview::launch(&preview::open_command(&path));
            })
        };
        let reveal_file = {
            let path = path.clone();
            cx.listener(move |_this, _ev, _window, _cx| {
                preview::launch(&preview::reveal_command(&path));
            })
        };
        let wheel = cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
            let lines = match ev.delta {
                ScrollDelta::Lines(lines) => lines.y,
                // A pixel delta is a trackpad: a hundredth of a notch per
                // pixel keeps a fast flick from jumping the whole range.
                ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / 100.0,
            };
            let next = preview::zoom_after_scroll(this.preview_zoom, lines);
            if next != this.preview_zoom {
                this.preview_zoom = next;
                cx.notify();
            }
        });
        preview::body(
            open,
            preview::actions_row(
                open.open_label(),
                open.path.exists(),
                open_file,
                reveal_file,
            ),
            self.preview_zoom,
            wheel,
            self.preview_video.as_ref(),
        )
    }

    /// Delete a session, then let the rail fetch the truth back from the
    /// ledger. The row disappears because the row is gone, not because a
    /// local list was filtered.
    fn delete_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        agent_bridge::spawn_delete_session(self.db_path.clone(), id, self.sink.clone());
        // The pin is part of the row: it leaves with the row.
        self.sessions.favorites.retain(|f| *f != id);
        if self.rail_actions == Some(id) {
            self.rail_actions = None;
        }
        cx.notify();
    }

    /// Open the rename prompt for a topic, pre-filled with its title
    /// (#2304). Enter saves the new name; Esc leaves the title as it was.
    ///
    /// The id comes from the caller: the menu acts on the topic its own row
    /// opened it for, which is not always the one on screen.
    fn start_rename(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let current = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_default();
        self.topic_menu = None;
        self.topic_rename = Some(id);
        self.topic_draft = Some(current);
        cx.notify();
    }

    /// Load the info panel for a session. The panel is read-only: it shows
    /// the id, the parent, the created time, and the message count.
    fn open_topic_info(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(
            async move |this, cx| match sessions::load_topic_info(db_path, id).await {
                Ok(Some(info)) => {
                    this.update(cx, |this, cx| {
                        this.topic_info = Some(info);
                        cx.notify();
                    })
                    .ok();
                }
                Ok(None) => {}
                Err(e) => {
                    logging::error(&format!("topic info {id}: {e}"));
                    this.update(cx, |this, cx| {
                        this.live.notices.push(format!("topic info: {e}"));
                        cx.notify();
                    })
                    .ok();
                }
            },
        )
        .detach();
        cx.notify();
    }

    /// Close a topic, or reopen it if it is closed (#2305). The state is
    /// re-read from the database, so the dimming and the send guard follow
    /// what was actually written.
    fn toggle_closed_topic(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let closed = self.sessions.is_closed(id);
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_closed(db_path, id, !closed).await {
                logging::error(&format!("close topic {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.live.notices.push(format!("close topic: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            this.update(cx, |this, cx| this.refresh_topic_state(cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Whether the open chat is a session with topics turned on.
    fn open_chat_topics_on(&self) -> bool {
        self.transcript
            .session
            .is_some_and(|id| self.sessions.is_topic_enabled(id))
    }

    /// The conversation's own name, for the card above the topic bar (#2480).
    ///
    /// The card names the session a topic belongs to, never the topic: the
    /// topic is already marked in the row below it, and Telegram's forum header
    /// keeps the chat title on top for the same reason.
    fn open_chat_name(&self) -> Option<String> {
        let open = self.transcript.session?;
        let root = self.sessions.parents.get(&open).copied().unwrap_or(open);
        let stored = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == root)
            .and_then(|s| s.title.as_deref());
        Some(rail::rail_title(
            stored,
            self.sessions.topic_titles.get(&root).map(String::as_str),
        ))
    }

    /// The chat's `...` menu (#2480): what can be done to the conversation it
    /// belongs to, in a card under the button that opened it.
    ///
    /// The card floats over the chat as a layer. The old menu was a column of
    /// rows laid *inside* the topic strip, so opening it grew the strip and
    /// pushed the conversation down; the owner read that as a bare element
    /// appearing, not a menu (#2480).
    ///
    /// A right press on a topic tab opens this same card for that tab's
    /// session, so the actions follow the row that asked for them.
    fn chat_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.topic_menu?;
        let is_topic = self.sessions.parents.contains_key(&target);
        let mut items: Vec<Stateful<Div>> = Vec::new();
        if self.open_chat_topics_on() {
            let new_topic = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.start_topic_draft(cx);
            });
            items.push(sessions::menu_item("New topic").on_click(new_topic));
        }
        let (view_label, edit_label, delete_label) = if is_topic {
            ("View topic info", "Edit topic", "Delete topic")
        } else {
            ("View info", "Rename", "Delete conversation")
        };
        let view = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.open_topic_info(target, cx);
        });
        items.push(sessions::menu_item(view_label).on_click(view));
        let edit = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.start_rename(target, cx);
        });
        items.push(sessions::menu_item(edit_label).on_click(edit));
        if is_topic {
            let close_label = if self.sessions.is_closed(target) {
                "Reopen topic"
            } else {
                "Close topic"
            };
            let close = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.topic_menu = None;
                this.toggle_closed_topic(target, cx);
            });
            items.push(sessions::menu_item(close_label).on_click(close));
        }
        let delete = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.request_delete(target, cx);
        });
        items.push(sessions::menu_item(delete_label).on_click(delete));
        Some(
            // Anchored to the column, not the header row: the column is the
            // positioning context that paints last. px_6 on the column puts
            // the button's right edge one 1.5rem in, so right_6 lines the card
            // up with it; top_12 clears the header line under pt_3.
            sessions::menu_card("chat-menu", items)
                .absolute()
                .right_6()
                .top_12()
                .into_any_element(),
        )
    }

    /// The tab strip for the open chat's topic family (#2400, #2480): `All`
    /// first, then each of its topics in list order. The open one is marked,
    /// and a click opens that session.
    ///
    /// The conversation's own name is not a chip. It used to lead the strip
    /// (`vec![root]`), which put the session in the row of its own topics and
    /// made it read as a sibling. The name belongs to the card above, and
    /// `All` is the way back to it.
    fn topic_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = self.transcript.session else {
            return Vec::new();
        };
        let root = self.sessions.parents.get(&open).copied().unwrap_or(open);
        if !self.sessions.is_topic_enabled(root) {
            return Vec::new();
        }
        // The topic store outranks the stored session name (#2490): a rename
        // in Telegram reaches this label on the next refresh pass.
        let title = |id: Uuid| {
            let stored = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref());
            rail::rail_title(
                stored,
                self.sessions.topic_titles.get(&id).map(String::as_str),
            )
        };
        let topics: Vec<Uuid> = self
            .sessions
            .items
            .iter()
            .filter(|s| self.sessions.parents.get(&s.id) == Some(&root))
            .map(|s| s.id)
            .collect();
        // A horizontal bar leads with the icon, a rail stacks it above the name
        // (#2480). Both edges hug their content, so one chip can never fill the
        // rail the way it did before.
        let shape = topic_bar::chip_shape(self.topic_edge.is_vertical());
        let icon_rems = topic_tabs::style(self.topic_edge.is_vertical()).topic_icon_rems;
        topic_bar::bar_entries(root, &topics)
            .into_iter()
            .map(|entry| {
                let id = entry.session(root);
                let current = entry.is_open(root, open);
                let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.open_session(id, cx);
                });
                // A right press on a tab opens the same menu the `...` draws,
                // for the session that tab stands for (#2480). Before this it
                // opened nothing at all, and the only way to edit a topic was
                // to open it first and hunt the menu.
                let open_menu = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.topic_menu = Some(id);
                    cx.notify();
                });
                // `All` has the same icon slot and cell geometry as a topic.
                let (label, picture) = match entry {
                    topic_bar::BarEntry::All => (
                        "All".to_string(),
                        topic_icons::tab_icon(
                            topic_icons::store_name("general").as_str(),
                            icon_rems,
                        ),
                    ),
                    topic_bar::BarEntry::Topic(id) => {
                        // The icon sits before the name and is not a word, so the
                        // two-word shortening (#2477) is applied to the name alone.
                        // A stored picture is drawn; a stored emoji stays text (#2478).
                        let stored = self.sessions.topic_icons.get(&id).cloned();
                        let picture = stored
                            .as_deref()
                            .and_then(|stored| topic_icons::tab_icon(stored, icon_rems));
                        let glyph = match (&picture, stored.as_deref()) {
                            (None, Some(icon)) => Some(topic_icons::tab_glyph(icon, icon_rems)),
                            _ => None,
                        };
                        (topic_label(&title(id)), picture.or(glyph))
                    }
                };
                topic_tab(
                    id,
                    label,
                    picture,
                    shape,
                    current,
                    move |ev, window, app| click(ev, window, app),
                    move |ev, window, app| open_menu(ev, window, app),
                )
                .into_any_element()
            })
            .collect()
    }

    /// Open the name prompt for a new topic under the open session (#2303).
    /// Move the topic row to the next side and save the choice (#2402).
    fn cycle_topic_edge(&mut self, cx: &mut Context<Self>) {
        self.topic_edge = self.topic_edge.next();
        if let Err(e) = Config::write_key_string("gui", "topic_tabs", self.topic_edge.as_str()) {
            logging::error(&format!("topic tabs: could not save position: {e}"));
        }
        cx.notify();
    }

    fn start_topic_draft(&mut self, cx: &mut Context<Self>) {
        if !self.open_chat_topics_on() {
            return;
        }
        self.topic_draft_icon = None;
        self.topic_menu = None;
        self.topic_draft = Some(String::new());
        cx.notify();
    }

    /// Create the named topic under the open session, then open it. An empty
    /// name is refused and the prompt stays open.
    fn submit_topic(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.topic_draft.as_ref().map(|d| d.trim().to_string()) else {
            return;
        };
        if name.is_empty() {
            self.live.notices.push("a topic needs a name".to_string());
            cx.notify();
            return;
        }
        // A rename prompt renames the topic in place and changes nothing else.
        if let Some(target) = self.topic_rename {
            let db_path = self.db_path.clone();
            cx.spawn(async move |this, cx| {
                match sessions::rename_session(db_path, target, name).await {
                    Ok(()) => {
                        this.update(cx, |this, cx| {
                            this.topic_draft = None;
                            this.topic_rename = None;
                            this.refresh_rail(cx);
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        logging::error(&format!("rename topic {target}: {e}"));
                        this.update(cx, |this, cx| {
                            this.live.notices.push(format!("rename: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                }
            })
            .detach();
            cx.notify();
            return;
        }
        let Some(parent) = self.transcript.session else {
            return;
        };
        let db_path = self.db_path.clone();
        let icon = self
            .topic_draft_icon
            .clone()
            .unwrap_or_else(|| DEFAULT_TOPIC_ICON.to_string());
        cx.spawn(async move |this, cx| {
            match sessions::create_topic(db_path.clone(), parent, name).await {
                Ok(topic) => {
                    if let Err(e) = sessions::set_topic_icon(db_path, topic, Some(icon)).await {
                        logging::error(&format!("topic icon {topic}: {e}"));
                    }
                    this.update(cx, |this, cx| {
                        this.topic_draft = None;
                        this.topic_draft_icon = None;
                        this.refresh_rail(cx);
                        this.refresh_topic_state(cx);
                        this.open_session(topic, cx);
                    })
                    .ok();
                }
                Err(e) => {
                    logging::error(&format!("create topic under {parent}: {e}"));
                    this.update(cx, |this, cx| {
                        this.live.notices.push(format!("topic: {e}"));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Re-read which sessions are topics and what each topic's parent is.
    fn refresh_topic_state(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let parents = sessions::load_topic_parents(db_path.clone()).await;
            let closed = sessions::load_closed(db_path.clone()).await;
            let icons = sessions::load_topic_icons(db_path.clone()).await;
            let topics = sessions::load_topics(db_path).await;
            this.update(cx, |this, cx| {
                match closed {
                    Ok(ids) => this.sessions.closed = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("closed refresh: {e}")),
                }
                match parents {
                    Ok(pairs) => this.sessions.parents = pairs.into_iter().collect(),
                    Err(e) => logging::error(&format!("topic parents refresh: {e}")),
                }
                match topics {
                    Ok(ids) => this.sessions.topics = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("topics refresh: {e}")),
                }
                match icons {
                    Ok(pairs) => this.sessions.topic_icons = pairs.into_iter().collect(),
                    Err(e) => logging::error(&format!("topic icons refresh: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Refetch the rail from the ledger. The pinned set rides along: pins come
    /// from a sidecar the TUI also writes, so a list refresh that skipped it
    /// would draw a row in the wrong slot until the next pin click.
    fn refresh_rail(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = sessions::load_sessions(db_path.clone()).await;
            // One batched lookup per refresh pass, never per row per frame
            // (#2490): the topic store outranks the stored session name and a
            // rename has to reach the rail without a restart.
            let titles = match &result {
                Ok(items) => sessions::load_telegram_topic_titles(db_path, items).await,
                Err(_) => Ok(HashMap::new()),
            };
            this.update(cx, |this, cx| {
                match titles {
                    Ok(pairs) => this.sessions.topic_titles = pairs,
                    Err(e) => logging::error(&format!("topic titles refresh: {e}")),
                }
                match result {
                    Ok(items) => {
                        this.sessions.items = items;
                        this.sessions.error = None;
                        // A refetch can drop the row the cursor is on; a
                        // cursor pointing at a ghost would arm nothing.
                        if !this.sessions.cursor_visible() {
                            this.sessions.cursor = None;
                        }
                    }
                    Err(e) => this.sessions.error = Some(e.into()),
                }
                this.refresh_favorites(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Refetch the turn's session transcript from the DB, and drop the live
    /// turn only once the persisted rows are actually there.
    ///
    /// Clearing first was the bug behind an "empty session after the turn":
    /// the live copy vanished, the reload had not landed yet (or the rows
    /// were not written yet), and the pane showed nothing at all. The live
    /// turn is the only thing keeping a silent or slow turn visible, so it
    /// stays until the fetch returns a transcript that includes it.
    fn reload_transcript(&mut self, sid: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        // The turn that just finished should add one to the turn count. A
        // reload that comes back short is a write that has not landed yet.
        let expect = self.settle_expect;
        let attempt = self.settle_retries;
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, sid).await;
            this.update(cx, |this, cx| {
                if this.transcript.session != Some(sid) {
                    return;
                }
                match result {
                    Ok((rows, total)) => {
                        let (hidden, turns) =
                            transcript::turns_with_metrics(&rows, total, &this.turn_metrics);
                        let landed = expect.is_none_or(|want| turns.len() >= want);
                        this.transcript.hidden = hidden;
                        this.transcript.turns = turns;
                        if landed {
                            // The rows are in: the newest turn is the one
                            // that just answered, so it can claim the metrics
                            // recorded at `Done` (#2324).
                            this.attach_pending_metrics(sid);
                            this.live.clear();
                            this.settle_expect = None;
                            this.settle_retries = 0;
                        } else if attempt < SETTLE_ATTEMPTS {
                            // The row is still in flight: retry shortly, the
                            // live turn keeps the answer on screen meanwhile.
                            this.settle_retries = attempt + 1;
                            logging::info(&format!(
                                "transcript short of the finished turn, retry {}",
                                attempt + 1
                            ));
                            this.reload_transcript(sid, cx);
                        } else {
                            // Bounded: the live turn keeps the answer, so the
                            // pane is never empty even if the write never lands.
                            logging::info("transcript never caught up; keeping the live turn");
                            this.settle_expect = None;
                            this.settle_retries = 0;
                        }
                    }
                    Err(e) => this.transcript.error = Some(e.into()),
                }
                this.transcript_scroll.scroll_to_bottom();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Key the just-settled turn's metrics by the anchor its persisted rows
    /// produced, now that they exist. Only called once a reload proves the
    /// turn landed, so the newest turn on screen IS the one that answered;
    /// the map is what carries the numbers onto a later reload of the same
    /// conversation.
    fn attach_pending_metrics(&mut self, sid: Uuid) {
        let Some((session, metrics)) = self.pending_metrics else {
            return;
        };
        if session != sid {
            return;
        }
        self.pending_metrics = None;
        let Some(turn) = self.transcript.turns.last_mut() else {
            return;
        };
        turn.metrics = Some(metrics);
        self.turn_metrics.insert(turn.anchor.clone(), metrics);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The Seeker card owns every key while it is up, and the chord that
        // opens it sits ahead of every other reader: one field that finds a
        // conversation, a command or a surface is only useful if the composer
        // underneath cannot eat the characters (#2458). alt+space is the
        // chord every platform can send and every finger can reach; cmd-k
        // stays as the window's own alias (#2491).
        {
            let ks = &ev.keystroke;
            let chord = find::opens(ks.modifiers.alt, ks.modifiers.platform, &ks.key);
            if self.finder.is_some() {
                self.finder_key(ks, cx);
                return;
            }
            if chord && self.screen == Screen::Chat {
                self.open_finder(cx);
                return;
            }
        }
        // Split panes (#2438): the chord opens a split, and while one waits for
        // its session, N makes a new one and Esc drops the pick.
        if self.screen == Screen::Chat {
            let ks = &ev.keystroke;
            if let Some(layout) = split_chord(ks) {
                self.begin_split(layout, cx);
                return;
            }
            if self.split_pick.is_some() {
                match ks.key.as_str() {
                    "escape" => {
                        self.cancel_split_pick(cx);
                        return;
                    }
                    "n" if !ks.modifiers.control && !ks.modifiers.platform && !ks.modifiers.alt => {
                        self.split_new(cx);
                        return;
                    }
                    _ => {}
                }
            }
            // ctrl+x closes the pane holding the caret, the same chord the
            // terminal answers (#2474). It sits ahead of the composer so the
            // key never reaches the editor as a cut.
            if ks.modifiers.control
                && !ks.modifiers.platform
                && !ks.modifiers.alt
                && ks.key == "x"
                && self
                    .panes
                    .as_ref()
                    .is_some_and(|tree| tree.leaves().len() > 1)
            {
                self.close_focused_pane(cx);
                return;
            }
        }
        // Modifiers go in the line too: a bare key name cannot tell ctrl+h
        // from a typed h, which is exactly what a help-door report needs.
        let m = &ev.keystroke.modifiers;
        let mods = ChordMods {
            control: m.control,
            shift: m.shift,
            platform: m.platform,
        };
        // A transcript selection survives mouse release so the keyboard can
        // copy it too (#2529). A composer selection keeps its own GPUI copy
        // action; otherwise this is the transcript's shared cmd/ctrl-C path.
        let composer_has_copy_selection = self.editor.read(cx).focus_handle.is_focused(window)
            && self.editor.read(cx).has_selection();
        if self.screen == Screen::Chat
            && !composer_has_copy_selection
            && opencrabs::ui_core::copy_select::is_copy_chord(
                &ev.keystroke.key,
                m.control,
                m.platform,
            )
            && self.copy_drag.has_selection()
        {
            if let Some(item) = self.copy_drag.clipboard_item() {
                cx.write_to_clipboard(item);
            }
            cx.notify();
            return;
        }
        // The line carries the screen and the table's answer, because a report
        // that only says which key arrived cannot tell a keystroke that never
        // reached this handler from one that reached it and matched nothing
        // (#2356).
        let screen_action = screen_chord(&ev.keystroke.key, mods, self.screen);
        logging::debug(&format!(
            "key {:?} screen={:?} action={} ctrl={} cmd={} shift={} alt={}",
            ev.keystroke.key,
            self.screen,
            action_label(screen_action),
            m.control,
            m.platform,
            m.shift,
            m.alt
        ));
        // The screen chords are answered by one pure table and they must work
        // while typing, so they sit before the editor focus gate: the editor
        // only claims IME escapes, and these keys belong to the window.
        if let Some(action) = screen_action {
            self.apply_screen_action(action, cx);
            return;
        }
        // ctrl+o folds or unfolds the newest turn, the terminal's own chord.
        // It sits with the screen keys so it works while typing.
        if ev.keystroke.modifiers.control && ev.keystroke.key == "o" && self.screen == Screen::Chat
        {
            self.toggle_newest_turn(cx);
            return;
        }
        // ctrl+n opens a new conversation, the terminal's own chord. The row
        // is created in the ledger first, so the rail and the transcript agree
        // on which session the next turn is written to.
        if ev.keystroke.modifiers.control && ev.keystroke.key == "n" {
            self.new_session(cx);
            return;
        }
        // ctrl+s shows or hides the rail: the sidebar is the window's own
        // pane, and the chord rides the control key so it is the same on every
        // platform. ctrl+l stays as the alias the terminal's session list has
        // always answered. Like the screen keys it sits before the editor
        // gate. A hidden rail has no field to keep listening, so the filter
        // exits.
        if ev.keystroke.modifiers.control && (ev.keystroke.key == "s" || ev.keystroke.key == "l") {
            self.toggle_rail(cx);
            return;
        }
        // ctrl+f is the rail's find field: it narrows the session list to
        // chats whose title (or id prefix) matches, live, with the match
        // count on the bar. Any exit clears it, so the rail never hides a
        // filter behind a closed field.
        // ctrl+shift+p pins the row the keyboard is on, or the open
        // conversation when the rail has not armed one. It rides `p` on both
        // clients, so the terminal and the window pin with the same keys
        // (#2457); alt+space is the Seeker card.
        if ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == "p"
        {
            if let Some(id) = self.sessions.pin_target() {
                self.toggle_favorite(id, cx);
            }
            return;
        }
        if ev.keystroke.modifiers.control && ev.keystroke.key == "f" {
            if self.sessions.filter_active {
                self.close_rail_filter(cx);
            } else {
                self.sessions.filter_active = true;
                // The field takes the keyboard, so the composer's Enter and
                // arrows stop answering while it says it is listening (#2454).
                if self.sessions.cursor.is_none() || !self.sessions.cursor_visible() {
                    self.sessions.cursor = self.sessions.cursor_step(None, 1);
                }
                self.refocus_overlay();
                cx.notify();
            }
            return;
        }
        // cmd+t starts a new topic under the open session (#2303). It does
        // nothing unless that session has topics turned on.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "t" {
            self.start_topic_draft(cx);
            return;
        }
        // ctrl+shift+t opens the themes screen, where a theme is previewed and
        // kept. It sits with the screen keys so it works while typing.
        if ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == "t"
        {
            self.toggle_themes(cx);
            return;
        }
        // While the rail filter is open it owns the keyboard, editor focus
        // included: printable characters extend the query, backspace (plain
        // or with the command key) shortens it, Enter and Esc clear it and
        // close. Anything else is swallowed so typing can never leak into
        // the chat editor while the field says it is listening.
        // The topic info panel is read only; Esc closes it and nothing else.
        if self.topic_info.is_some() && ev.keystroke.key == "escape" {
            self.topic_info = None;
            cx.notify();
            return;
        }
        // The topic name prompt owns the keyboard while it is open (#2303):
        // typed characters extend the name, Enter creates the topic, Esc
        // cancels. Nothing leaks into the chat editor behind it.
        if self.topic_draft.is_some() {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "enter" => self.submit_topic(cx),
                "escape" => {
                    self.topic_draft = None;
                    self.topic_rename = None;
                    cx.notify();
                }
                "backspace" => {
                    if let Some(draft) = self.topic_draft.as_mut() {
                        draft.pop();
                    }
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                        && let Some(draft) = self.topic_draft.as_mut()
                    {
                        draft.push(ch);
                        cx.notify();
                    }
                }
            }
            return;
        }
        if self.sessions.filter_active {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "escape" => self.close_rail_filter(cx),
                "enter" => {
                    // Enter opens the row the field found, the terminal's own
                    // behaviour. It used to close the search and send nothing.
                    let target = self.sessions.find_target();
                    self.close_rail_filter(cx);
                    if let Some(id) = target {
                        self.open_session(id, cx);
                    }
                }
                "up" | "down" => {
                    // The arrows step the rail from inside the field, and the
                    // match that scrolls into view is the one Enter opens.
                    let delta = if ks.key == "down" { 1 } else { -1 };
                    if let Some(id) = self.sessions.cursor_step(self.sessions.cursor, delta) {
                        self.sessions.cursor = Some(id);
                        if let Some(ix) = self.sessions.cursor_index(id) {
                            self.rail_scroll.scroll_to_item(ix);
                        }
                    }
                    cx.notify();
                }
                "backspace" => {
                    self.sessions.filter.pop();
                    self.normalize_cursor();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.sessions.filter.push(ch);
                        self.normalize_cursor();
                        cx.notify();
                    }
                }
            }
            return;
        }
        // Deleting a conversation has no chord. ctrl+backspace is the editor's
        // word delete (#2313), so a window binding on it would break typing,
        // and the command key is macOS-only. The row strip's delete, behind
        // the confirm dialog, is the one path: the slide reveals it.
        // Esc twice stops the running turn, the TUI's own gesture. It sits
        // before the editor gate because the editor owns the keyboard while
        // typing and would swallow it.
        if self.help_open {
            let ks = &ev.keystroke;
            let scroll_by = match ks.key.as_str() {
                "up" => Some(-48.0),
                "down" => Some(48.0),
                "pageup" => Some(-400.0),
                "pagedown" => Some(400.0),
                _ => None,
            };
            if let Some(dy) = scroll_by {
                let offset = self.help_scroll.offset();
                let max = self.help_scroll.max_offset().height;
                let y = (offset.y + px(dy)).min(px(0.0)).max(-max);
                self.help_scroll.set_offset(gpui::point(offset.x, y));
                cx.notify();
                return;
            }
            if let Some(filter) = self.help_filter.as_mut() {
                match ks.key.as_str() {
                    "backspace" => {
                        filter.pop();
                        cx.notify();
                        return;
                    }
                    _ => {
                        if let Some(ch) = ks.key_char.as_deref().and_then(|s| s.chars().next())
                            && !ch.is_control()
                            && !ks.modifiers.platform
                            && !ks.modifiers.control
                        {
                            filter.push(ch);
                            cx.notify();
                            return;
                        }
                    }
                }
            } else if ks.key == "/" && !ks.modifiers.platform && !ks.modifiers.control {
                self.help_filter = Some(String::new());
                cx.notify();
                return;
            }
        }
        // The delete confirmation is a modal with two answers, so it owns
        // both keys while it is up: Enter confirms, Esc cancels, and every
        // other keystroke is swallowed rather than reaching anything behind
        // it (#2456).
        if let Some(id) = self.confirm_delete {
            match ev.keystroke.key.as_str() {
                "enter" => {
                    self.confirm_delete = None;
                    self.rail_actions = None;
                    self.delete_session(id, cx);
                }
                "escape" => {
                    self.confirm_delete = None;
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        if ev.keystroke.key == "escape" {
            // The key reference paints above everything and costs nothing to
            // close, so it goes first: the nearest thing to the user answers.
            if self.help_open {
                // A filter typed after `/` clears first; a second Esc closes (#2407).
                if self.help_filter.take().is_none() {
                    self.help_open = false;
                }
                cx.notify();
                return;
            }
            // A preview is the nearest thing to the pointer that Esc can
            // answer: it covers the window, so it goes before anything under
            // it (#2395).
            if self.preview.take().is_some() {
                // The facts belong to the file that was open, so they leave
                // with it: the next clip probes its own.
                self.preview_video = None;
                cx.notify();
                return;
            }
            // An open row strip closes next: Esc dismisses the nearest
            // affordance, never the far one.
            if self.rail_actions.take().is_some() {
                cx.notify();
                return;
            }
            // The context menu is the strip's neighbour: same nearness
            // rule, and a dismissal writes nothing (#2354).
            if self.sessions.rail_menu.take().is_some() {
                cx.notify();
                return;
            }
            // The command list lives inside the composer, so with it open Esc
            // closes the list and keeps the draft: the terminal's behaviour,
            // and the nearest thing answers first.
            let spelling = self.editor.read(cx).list_open();
            if spelling {
                self.editor.update(cx, |editor, cx| {
                    editor.dismiss_command_popup(cx);
                    editor.dismiss_emoji_popup(cx);
                });
                cx.notify();
                return;
            }
            // The log viewer covers the screen while it is open, so every key
            // including Esc belongs to it, exactly as the terminal binds it.
            if self.screen == Screen::MissionControl && self.mc_logs.is_some() {
                self.mission_control_key(ev, cx);
                return;
            }
            // An open Mission Control record is the nearest thing Esc can answer:
            // it closes the popup and leaves the screen alone.
            if self.screen == Screen::MissionControl && self.mc_nav.detail {
                self.mc_nav.detail = false;
                cx.notify();
                return;
            }
            // The usage ledger and the files list are full screens, so Esc
            // leaves them the same way their own chords do. It sits before
            // the turn gate: a running turn must not hold a screen open.
            if let Some(action) = screen_chord("escape", mods, self.screen) {
                self.apply_screen_action(action, cx);
                return;
            }
            // Esc on the themes screen is a cancel: the palette from before
            // the screen opened comes back, and nothing is saved. It stays out
            // of the table because the themes screen belongs to another lane.
            if self.screen == Screen::Themes {
                self.close_themes(true, cx);
                return;
            }
            // Esc on the models screen leaves it and changes nothing: the
            // picker is a list, not a commitment.
            if self.screen == Screen::Models || self.screen == Screen::Skills {
                self.screen = Screen::Chat;
                self.refocus_screen();
                cx.notify();
                return;
            }
            // Esc on the /cd picker cancels and changes nothing (#2527). It
            // sits with the screens above: a running turn must not hold it.
            if self.screen == Screen::DirectoryPicker {
                self.close_cd_picker(cx);
                return;
            }
            // Esc in the wizard goes back one step, clearing a model search
            // first, and closes the screen on the first step. The terminal's
            // own rule, so one stray key never throws away a half-filled run.
            if self.screen == Screen::Onboard {
                self.onboard_escape(cx);
                return;
            }
            if !self.busy {
                self.esc_at = None;
                // Idle Esc hands the keyboard back. The rail's mode is the
                // only thing Esc owns here, and Tab is what arms it. The old
                // blur bridge existed only because focus never moved off the
                // composer (#2351, #2454).
                if self.rail_focused {
                    self.rail_focused = false;
                    self.refocus_screen();
                }
                cx.notify();
                return;
            }
            let now = std::time::Instant::now();
            // Second Esc within 3s aborts, the TUI's own window.
            let armed = self
                .esc_at
                .is_some_and(|t| now.duration_since(t).as_millis() < 3000);
            if armed {
                self.esc_at = None;
                if let Some(cancel) = self.cancel.take() {
                    cancel.cancel();
                }
                // Same wording and immediate release as `abort_active_turn`:
                // the pane stops claiming a turn is running the moment the
                // user asked it to stop, and the service's own Failed event
                // lands afterwards.
                self.busy = false;
                self.live.thinking = false;
                self.live.notices.push("Operation cancelled.".to_string());
                logging::info("turn cancelled by Esc Esc");
            } else {
                self.esc_at = Some(now);
                self.live.notices.push("esc again to stop".to_string());
            }
            cx.notify();
            return;
        }
        // Page keys scroll the transcript and ctrl+k clears the session (#2430).
        // Both sit before the composer gate, so they work while the composer has focus.
        if self.screen == Screen::Chat {
            let key = ev.keystroke.key.as_str();
            if matches!(key, "pageup" | "pagedown") {
                let page = px(f32::from(window.viewport_size().height) * 0.8);
                let dy = if key == "pagedown" { -page } else { page };
                // The page keys act on the pane Tab moved the caret to, so a
                // split scrolls the session the accent is on rather than the
                // chat underneath it (#2475).
                // The page keys act on the pane the caret is on, so a split
                // scrolls the transcript the accent names rather than the
                // chat underneath it (#2475, #2472).
                let scroll = match self.pane_focus.and_then(|id| self.pane_states.get(&id)) {
                    Some(pane) => &pane.scroll,
                    None => &self.transcript_scroll,
                };
                let offset = scroll.offset();
                let max = scroll.max_offset().height;
                let y = (offset.y + dy).min(px(0.0)).max(-max);
                scroll.set_offset(gpui::point(offset.x, y));
                cx.notify();
                return;
            }
            if key == "k" && ev.keystroke.modifiers.control {
                if let Some(id) = self.transcript.session {
                    agent_bridge::spawn_clear_messages(self.db_path.clone(), id, self.sink.clone());
                }
                return;
            }
        }
        // Zoom with cmd or ctrl, in every mode and while the composer has focus.
        // It needs the chord, so a bare minus typed into the composer stays text.
        let chord = ev.keystroke.modifiers.platform || ev.keystroke.modifiers.control;
        if let Some(next) = zoom_step_for(&ev.keystroke.key, self.rem, chord) {
            self.rem = next;
            window.set_rem_size(px(self.rem));
            logging::debug(&format!("zoom -> {:.2}x", self.rem / BASE_REM));
            cx.notify();
            return;
        }
        // Tab moves the caret between the two panes while a split is open, so
        // the accent follows the keyboard instead of staying where the split
        // started (#2475). It runs ahead of the rail and of the composer gate
        // for the same reason: a live split owns Tab while it exists. An open
        // emoji or command list keeps the key, the way it already did (#2417).
        if ev.keystroke.key == "tab"
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.alt
            && self.screen == Screen::Chat
            && self
                .panes
                .as_ref()
                .is_some_and(|tree| tree.leaves().len() > 1)
            && !self.editor.read(cx).list_open()
        {
            self.cycle_pane_focus(cx);
            return;
        }
        // Tab moves the keyboard between the composer and the rail. It runs before
        // the composer gate, because taking the keyboard away from a focused
        // composer is the whole point (#2351, #2454). While an emoji or command
        // list is open, Tab accepts there instead (#2417).
        if ev.keystroke.key == "tab"
            && self.screen == Screen::Chat
            && self.rail_open
            && (self.rail_focused || !self.editor.read(cx).list_open())
        {
            self.rail_focused = !self.rail_focused;
            if self.rail_focused {
                self.sessions.cursor = self.sessions.cursor.or(self.sessions.selected);
            }
            // The owner decides where focus lands: the root while the rail
            // holds the keyboard, the composer once it hands back.
            self.refocus_screen();
            cx.notify();
            return;
        }
        // While the editor holds focus it owns the keyboard: its actions
        // (send, newline, movement) run through the keymap, and raw keys
        // reaching this handler would double-handle Enter and friends.
        if self.editor.read(cx).focus_handle.is_focused(window) {
            return;
        }
        // Rail arrows and enter. This sits after the editor gate, so the
        // composer keeps its up, down and enter actions while it holds
        // focus, and before the chat-screen return, which is the dead end
        // arrows used to die in (#2351).
        if self.screen == Screen::Chat && self.rail_open {
            match ev.keystroke.key.as_str() {
                "up" | "down" if self.rail_focused => {
                    // The arrows walk the rail and open the row they stand
                    // on, so the chat shows it without an Enter. Focus stays
                    // on the rail: the composer is not refocused here (#2404).
                    let delta = if ev.keystroke.key == "down" { 1 } else { -1 };
                    if let Some(id) = self.sessions.cursor_step(self.sessions.cursor, delta) {
                        self.sessions.cursor = Some(id);
                        if let Some(ix) = self.sessions.cursor_index(id) {
                            self.rail_scroll.scroll_to_item(ix);
                        }
                        // During a split pick the arrows only walk the list (#2438).
                        if self.split_pick.is_none() {
                            self.open_session(id, cx);
                        }
                    }
                    cx.notify();
                    return;
                }
                "enter" if self.rail_focused => {
                    if let Some(id) = self.sessions.cursor {
                        if self.split_pick.is_some() {
                            self.choose_split(id, cx);
                            cx.notify();
                            return;
                        }
                        self.open_session(id, cx);
                        // Back to the composer, the place a newly opened
                        // conversation is about to be typed into.
                        self.refocus_screen();
                    }
                    cx.notify();
                    return;
                }
                "d" if ev.keystroke.modifiers.control => {
                    // ctrl+d asks the dialog, it never erases (#2352). The
                    // cursor row wins; with nothing armed the open chat is
                    // the target, which is the row the transcript shows.
                    if let Some(target) = self.sessions.delete_target() {
                        self.request_delete(target, cx);
                    }
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        // Chat owns no usage keys. Esc included: backing out of the
        // conversation into the ledger was the mixing the owner called out,
        // and Esc must never close the window (phase-0 lesson).
        if self.screen == Screen::Chat {
            return;
        }
        // The files browser is a list, not the ledger's grid: Esc (or the
        // ctrl+g toggle again) is the only key it owns, and it goes back to
        // the conversation.
        if self.screen == Screen::Files {
            if translate_key(ev) == UiKey::Esc {
                self.screen = Screen::Chat;
                cx.notify();
            }
            return;
        }
        // The themes screen is a narrow sidebar over the conversation, so its
        // keys belong to the list: up and down walk the highlight, Enter
        // previews the highlighted row, and a click does the same thing with
        // the mouse. Esc restores the palette from before, handled above with
        // the overlays it belongs to.
        if self.screen == Screen::Themes {
            let names = themes_view::names();
            let last = names.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                // The arrows walk the list and preview the theme they stand
                // on, so a theme is judged without an Enter (#2404).
                "up" | "down" => {
                    let index = if ev.keystroke.key == "down" {
                        (self.themes_index + 1).min(last)
                    } else {
                        self.themes_index.saturating_sub(1)
                    };
                    self.themes_index = index;
                    if let Some(name) = names.get(index).copied() {
                        self.preview_theme(name.to_string(), cx);
                    }
                }
                // Enter applies the highlighted theme: the same keep as the
                // Keep button, so the choice is saved to [gui] (#2415).
                "enter" => {
                    if let Some(name) = names.get(self.themes_index.min(last)).copied() {
                        self.preview_theme(name.to_string(), cx);
                        self.keep_theme(cx);
                    }
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        // The model picker is a live-filtered list: arrows walk the visible
        // models (never its provider headers) and Enter switches. Esc clears
        // a filter first, then leaves, as the terminal's picker does.
        if self.screen == Screen::Models {
            self.shared_models_key(ev, false, cx);
            cx.notify();
            return;
        }
        // The skills screen is a list too: arrows walk it and Enter runs the
        // skill under the highlight, the same act as typing its slash.
        if self.screen == Screen::Skills {
            let last = self.skills_rows.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                "up" => self.skills_index = self.skills_index.saturating_sub(1),
                "down" => self.skills_index = (self.skills_index + 1).min(last),
                "enter" => self.run_skill(self.skills_index.min(last), cx),
                "backspace" => {
                    self.skills_query.pop();
                    self.skills_index = 0;
                    self.refresh_skills();
                }
                _ => {
                    // Typing narrows the list, as it does in the terminal's
                    // dialog: a list of forty skills is unreadable without it.
                    if let Some(ch) = ev
                        .keystroke
                        .key_char
                        .as_ref()
                        .and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.skills_query.push(ch);
                        self.skills_index = 0;
                        self.refresh_skills();
                    }
                }
            }
            cx.notify();
            return;
        }
        // The /cd picker owns the keyboard on its screen (#2527), the same
        // bindings the terminal's dialog uses: arrows walk, Enter descends,
        // `.` flips hidden directories, Tab and Space select here, Esc
        // cancels (handled above with the screens).
        if self.screen == Screen::DirectoryPicker {
            let last = self.cd_entries.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                "up" => self.cd_selected = self.cd_selected.saturating_sub(1),
                "down" => self.cd_selected = (self.cd_selected + 1).min(last),
                "enter" => self.cd_descend(self.cd_selected.min(last), cx),
                "tab" | "space" => self.cd_select_here(cx),
                _ => {
                    // `.` flips the hidden-directories toggle, as it does in
                    // the terminal's dialog.
                    if ev.keystroke.key_char.as_deref() == Some(".") {
                        self.cd_toggle_hidden(cx);
                    }
                }
            }
            cx.notify();
            return;
        }
        // The wizard owns the keyboard while it is open: typed characters go
        // into the focused field, Tab and the arrows walk the lists, Enter
        // confirms the step, Esc steps back (handled above).
        if self.screen == Screen::Onboard {
            self.onboard_key(ev, cx);
            return;
        }
        // Mission Control owns the keyboard on its own screen: bare keys walk
        // the panels and open the record, the terminal's own bindings.
        if self.screen == Screen::MissionControl {
            self.mission_control_key(ev, cx);
            return;
        }
        match input::decide(&mut self.dash, translate_key(ev)) {
            UsageKeyOutcome::Consumed => cx.notify(),
            UsageKeyOutcome::Period(period) => self.set_usage_period(period, cx),
            // `decide` maps Esc on the grid to Close, which is right for the
            // TUI's modal dialog and wrong for a desktop window: a stray Esc
            // must never destroy the window. The titlebar button and Cmd+Q
            // are the exits here.
            UsageKeyOutcome::Close => {
                // Esc on the usage screen means "back to the conversation".
                // With nothing to return to it is a no-op, never a window
                // close (phase-0 lesson).
                if self.transcript.session.is_some() {
                    self.screen = Screen::Chat;
                    cx.notify();
                } else {
                    logging::info("ignored Close: the window closes from its titlebar button");
                }
            }
            UsageKeyOutcome::NotConsumed => {}
        }
    }

    /// The pane body for the chat screen and the themes preview: the chat
    /// element alone while the tree is one leaf, or the whole tree drawn
    /// around it (#2472). The chat element is moved into its own leaf, so it
    /// is built once and never duplicated.
    fn panes_body(
        &mut self,
        chat: AnyElement,
        content: f32,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Shares are clamped to the pixel floor against the area the last
        // frame painted into, so a window resize never squeezes a pane below
        // a usable width (#2472). The clamp is idempotent, so it settles on
        // the first frame that has a measurement.
        let root_area = self.split_areas.borrow().get(&Vec::new()).copied();
        if let Some([x0, y0, x1, y1]) = root_area
            && let Some(tree) = self.panes.as_mut()
        {
            let span = match tree {
                PaneTree::Split { layout, .. } => {
                    if layout.is_stacked() {
                        y1 - y0
                    } else {
                        x1 - x0
                    }
                }
                PaneTree::Leaf { .. } => 0.0,
            };
            tree.normalize(span);
        }
        let Some(tree) = self.panes.as_ref() else {
            return chat;
        };
        if tree.leaves().len() < 2 {
            return chat;
        }
        let title = match self.session_title() {
            Some(title) => SharedString::from(title.to_string()),
            None => SharedString::from("Chat"),
        };
        let mut ctx = PanesCtx {
            chat: Some(chat),
            chat_session: self.transcript.session,
            chat_title: title,
            states: &self.pane_states,
            focus: self.pane_focus,
            content,
            areas: &self.split_areas,
            entity: cx.entity().downgrade(),
            interactive,
        };
        let mut path = Vec::new();
        let tree = self.panes.as_ref().expect("checked a moment ago");
        pane_element(tree, &mut ctx, &mut path, 1.0)
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus follows the keyboard owner, decided here because this is the
        // only place holding a window to focus through. A transition that
        // forgot to ask would leave the composer answering under an open
        // field, which is the bug class #2455 came from, so the owner is
        // reconciled every frame instead of trusted from each call site.
        let owner = keyboard_owner(&self.open_overlays(), self.screen);
        if self.last_owner != Some(owner) {
            self.last_owner = Some(owner);
            self.refocus = Some(focus_target(owner));
        }
        // Applied before the tree is built, so the frame that shows the new
        // screen is the frame that can be typed into.
        if let Some(target) = self.refocus.take() {
            match target {
                FocusTarget::Composer => window.focus(&self.editor.read(cx).focus_handle),
                FocusTarget::Root => window.focus(&self.focus),
            }
        }
        // The plan strip follows the open conversation. Where a plan lives
        // depends on what the session is bound to, so the lookup is a DB read
        // and runs once per selection, off the paint path; the strip itself
        // then only stats the file.
        if self.plan_for != self.transcript.session {
            self.plan_for = self.transcript.session;
            match self.transcript.session {
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
            // The header's ink is data, not layout: log the string the chip
            // will print whenever the open conversation changes, so a header
            // that reads wrong traces to its source without a rebuild. The
            // bare `\u{2026}` the owner screenshotted had no such trail and cost
            // two days of guessing at padding (#2480).
            if let Some(name) = self.open_chat_name() {
                logging::debug(&format!("header: painting name {name:?}"));
            }
        }
        self.plan.refresh();

        // The rail's rows: each carries its own click listener, built in its
        // own loop turn so the `&mut cx` borrow stays legal.
        let mut rail_rows = Vec::with_capacity(self.sessions.items.len());
        // Pinned rows first, the rest untouched: the TUI's own order.
        for s in self.sessions.ordered() {
            let id = s.id;
            let selected = self.sessions.selected == Some(id);
            let cursored = self.sessions.cursor == Some(id);
            let favorite = self.sessions.is_favorite(id);
            // A row's actions stay out of sight until asked for. There is no
            // always-visible delete: an ✕ on every row reads as "close" and
            // costs conversations. Slide the row left, or press the pin key,
            // and the strip appears on that row alone.
            let topics_on = self.sessions.is_topic_enabled(id);
            let action = if self.rail_actions == Some(id) {
                // Topic icon: turn topics on for this session, or open the new
                // topic prompt under it if they are already on (#2302, #2303).
                let topic = cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    if this.sessions.is_topic_enabled(id) {
                        this.open_session(id, cx);
                        this.start_topic_draft(cx);
                    } else {
                        this.enable_topics(id, cx);
                    }
                });
                let pin = cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.toggle_favorite(id, cx);
                });
                let del = cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.request_delete(id, cx);
                });
                Some(
                    div()
                        .id(SharedString::from(format!("session-actions-{id}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(sessions::pin_button(favorite).on_click(pin))
                        .child(sessions::topic_button(topics_on).on_click(topic))
                        .child(sessions::delete_button().on_click(del)),
                )
            } else {
                None
            };
            // Right-click asks for the row's menu: pin and delete without
            // the slide gesture or a chord (#2354). It closes any open
            // strip first: one affordance at a time, the rail's own rule.
            let ask_menu = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                this.sessions.rail_menu = Some(id);
                this.rail_actions = None;
                cx.notify();
            });
            // The menu overlays its row, so a pick must not fall through to
            // the row's click (which opens the chat) or read as a dismissal.
            let menu = if self.sessions.rail_menu == Some(id) {
                let topics_on = self.sessions.is_topic_enabled(id);
                let items = sessions::rail_menu_entries(favorite, topics_on)
                    .into_iter()
                    .map(|entry| {
                        let kind = entry.kind;
                        let pick = cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.sessions.rail_menu = None;
                            match kind {
                                sessions::MenuKind::Pin => this.toggle_favorite(id, cx),
                                sessions::MenuKind::EnableTopics => this.enable_topics(id, cx),
                                sessions::MenuKind::NewTopic => {
                                    // The prompt works on the open chat, so open the row first (#2397).
                                    this.open_session(id, cx);
                                    this.start_topic_draft(cx);
                                }
                                sessions::MenuKind::SplitSide => {
                                    this.open_session(id, cx);
                                    this.begin_split(Layout::Side, cx);
                                }
                                sessions::MenuKind::SplitStack => {
                                    this.open_session(id, cx);
                                    this.begin_split(Layout::Stack, cx);
                                }
                                sessions::MenuKind::Delete => this.request_delete(id, cx),
                            }
                        });
                        sessions::menu_item(entry.label).on_click(pick)
                    })
                    .collect::<Vec<_>>();
                Some(sessions::menu_popover(items))
            } else {
                None
            };
            let indented = self.sessions.is_indented(id);
            let closed = self.sessions.is_closed(id);
            let flags = sessions::RowFlags {
                selected,
                cursored,
                favorited: favorite,
                indented,
                closed,
            };
            let r = sessions::row(s, flags, action, menu);
            // The press starts the slide and closes any other row's strip.
            let press = cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                this.rail_slide_opened = None;
                this.rail_swipe = Some((id, f32::from(window.mouse_position().x)));
                if this.rail_actions.is_some_and(|open| open != id) {
                    this.rail_actions = None;
                }
                // A left press on the rail dismisses a context menu: the
                // press is elsewhere, so the menu goes with it (#2354).
                this.sessions.close_rail_menu();
                cx.notify();
            });
            let handler = cx.listener(move |this, _ev, _window, cx| {
                // The release that ended a slide is the tail of that gesture,
                // not a pick: it leaves the strip it just opened alone.
                if this.rail_slide_opened.take() == Some(id) {
                    cx.notify();
                    return;
                }
                // The find field is a jump aid: picking a row is its exit,
                // and the keyboard goes back to the conversation.
                // A split waiting for its second session takes this row (#2438).
                if this.split_pick.is_some() {
                    this.choose_split(id, cx);
                    return;
                }
                this.close_rail_filter(cx);
                this.rail_actions = None;
                this.open_session(id, cx);
            });
            let r = r.on_mouse_down(MouseButton::Right, ask_menu);
            let r = r.on_mouse_down(MouseButton::Left, press);
            rail_rows.push(sessions::on_click(r, handler));
        }
        // The channel chip (#2452): quiet notice plus the one action, drawn
        // at the end of the rail only while a configured channel has no
        // live owner. `Start daemon` runs the onboarding installer once;
        // the probe loop flips the chip off when the daemon is live.
        let chip_footer = self.channel_chip.clone().map(|line| {
            let start = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.start_daemon(cx);
            });
            let shown = palette::current();
            let mut chip = div()
                .id("channel-chip")
                .flex()
                .flex_col()
                .gap_1()
                .mt_auto()
                .w_full()
                .min_w_0()
                .flex_none()
                .p_1p5()
                .rounded_md()
                .border_1()
                .border_color(rgb(shown.track))
                .bg(rgb(shown.panel_hi))
                .text_xs()
                .text_color(rgb(shown.text_muted))
                .child(line);
            if let Some(err) = &self.daemon_error {
                chip = chip.child(div().text_color(rgb(shown.red)).child(err.clone()));
            }
            let action = if self.daemon_starting {
                "starting..."
            } else {
                "Start daemon"
            };
            chip.child(
                div()
                    .id("channel-chip-start")
                    .text_color(rgb(shown.accent))
                    .cursor_pointer()
                    .hover(|s| s.text_color(rgb(shown.text)))
                    .child(action)
                    .on_click(start),
            )
            .into_any_element()
        });
        let rail = sessions::rail(
            &self.sessions,
            rail_rows,
            self.rail_width,
            &self.rail_scroll,
            chip_footer,
        );

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
            Screen::Themes => "OpenCrabs \u{b7} themes".to_string(),
            Screen::Models => "OpenCrabs \u{b7} models".to_string(),
            Screen::Skills => "OpenCrabs \u{b7} skills".to_string(),
            Screen::DirectoryPicker => "OpenCrabs \u{b7} directory".to_string(),
            Screen::Onboard => "OpenCrabs \u{b7} onboarding".to_string(),
            Screen::MissionControl => "OpenCrabs \u{b7} mission control".to_string(),
            Screen::Chat => format!(
                "OpenCrabs \u{b7} {}",
                self.session_title().unwrap_or("chat")
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
            .child(icon::banner_logo())
            .child(div().text_lg().child(SharedString::from(label)));

        // One click listener per turn anchor, minted here because only the
        // view's own `cx` can build them; the transcript takes them by value.
        let mut fold_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for anchor in self.transcript.anchors() {
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
            std::rc::Rc::new(cx.listener(|this, _ev: &gpui::ClickEvent, _window, cx| {
                this.transcript.live_expanded = !this.transcript.live_expanded;
                cx.notify();
            }));
        // One click listener per attachment tile, minted here for the same
        // reason as the folds: only this `cx` can build one, and each tile has
        // to open its own file (#2395).
        let mut attachment_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for key in transcript::attachment_paths_with_live(&self.transcript, &self.live) {
            let path = std::path::PathBuf::from(key.clone());
            let handler = cx.listener(move |this, _ev, _window, cx| {
                this.open_preview(path.clone(), cx);
            });
            attachment_handlers.insert(key, std::rc::Rc::new(handler));
        }

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

        let (middle, footer): (AnyElement, AnyElement) = match self.screen {
            Screen::Chat => {
                // The chat is the primary pane, and a split draws its session
                // beside or below it (#2438). Two panes share the content
                // width, less the gap_2 that separates them, so a row is
                // never planned wider than the box it is painted into.
                // The chat's share of the pane area comes from the tree: a
                // leaf under stacked splits spans the full width, and a leaf
                // beside a sibling takes its share of it (#2472, #2473). A
                // row is still never planned wider than its box.
                let chat_share = self
                    .panes
                    .as_ref()
                    .zip(self.transcript.session)
                    .and_then(|(tree, id)| tree.leaf_width_share(id))
                    .unwrap_or(1.0);
                let primary_width = if chat_share < 1.0 {
                    split_pane_width(pane_width, false, chat_share)
                } else {
                    pane_width
                };
                // A single pane draws the conversation at a readable measure
                // and centres it; a split window is exempt and keeps every pane
                // at the span the tree hands it (#2586).
                let split_window = self
                    .panes
                    .as_ref()
                    .is_some_and(|tree| tree.leaves().len() >= 2);
                let measure = chat_measure(primary_width, split_window);
                let primary = transcript::view(
                    &self.transcript,
                    &self.transcript_scroll,
                    &self.live,
                    self.busy,
                    measure,
                    fold_handlers,
                    transcript::ViewHooks {
                        attachments: attachment_handlers,
                        drag: Some(self.copy_drag.clone()),
                        ctx_used: self.ctx_tokens,
                        ctx_max: self.ctx_max,
                        live_click: Some(live_click.clone()),
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
                    {
                        // Key hints left, ctx budget right: the TUI's own footer
                        // split (`tui::render::input` draws the counter as the
                        // input box's right-side title).
                        let (ctx_label, ctx_color) = ctx_footer(self.ctx_tokens, self.ctx_max);
                        // Who is answering, from where, on what branch, and how
                        // fast: the same fields the TUI's footer carries.
                        let (provider, model) = match agent_bridge::bridge() {
                            Some(b) => (b.service.provider_name(), b.service.provider_model()),
                            None => ("-".to_string(), "-".to_string()),
                        };
                        // The session name leads, as in the TUI footer.
                        let identity = format!(
                            "{} · {}",
                            self.session_title().unwrap_or("chat"),
                            status_identity(
                                &provider,
                                &model,
                                &tilde_path(&opencrabs::utils::cwd::launch_cwd()),
                                self.branch.as_deref().unwrap_or("-"),
                                self.live.tokens_per_sec(),
                            )
                        );
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_3()
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_muted))
                                    .child("ctrl+h · help"),
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
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(rgb(ctx_color))
                                    .child(SharedString::from(ctx_label)),
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
                            .child("ctrl+h · help"),
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
                            .child("ctrl+h · help"),
                    );
                (scroller.into_any_element(), footer.into_any_element())
            }
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
            Screen::Models => {
                let rendered = model_picker::render(
                    cx,
                    model_picker::ModelPickerView {
                        rows: &self.models_rows,
                        query: &self.models_query,
                        selected: self.models_index,
                        fetching: self.models_fetching,
                        pending: self.models_pending.len(),
                        error: self.models_fetch_error.as_deref(),
                        active_provider: "",
                        onboarding: false,
                        search_focused: true,
                        max_models: 18,
                        row_layout: picker_layout.model_row,
                        provider_label_width: picker_layout.provider_label_width,
                    },
                );
                let shown = palette::current();
                let footer = rendered.footer.child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .min_w_0()
                        .whitespace_normal()
                        .child("type · filter or enter a custom id · enter · switch · esc · back"),
                );
                (rendered.body.into_any_element(), footer.into_any_element())
            }
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
            Screen::Themes => {
                // The list is a sidebar, not a takeover: the conversation stays
                // on screen and the preview repaints the whole window around
                // it, so a theme is judged against real content. The rows are
                // compact, the one the arrows stand on is marked, and only a
                // click or Enter previews, so walking the list never flashes.
                let shown = palette::current();
                let picked = self.themes_pick.clone();
                let names = themes_view::names();
                let current = self.themes_index.min(names.len().saturating_sub(1));
                let mut list = div().flex().flex_col().gap_1();
                for (index, name) in names.iter().enumerate() {
                    let selected = picked.as_deref() == Some(*name);
                    let pick = name.to_string();
                    list = list.child(themes_view::row(name, selected, index == current).on_click(
                        cx.listener(move |this, _ev, _window, cx| {
                            this.themes_index = index;
                            this.preview_theme(pick.clone(), cx);
                        }),
                    ));
                }
                let scroller = div()
                    .id("themes-body")
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
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(shown.track))
                    .child(text("Themes", Size::Sm, shown.text))
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        SharedString::from(format!("{}/{}", current + 1, names.len())),
                    ));
                let mut panel = div()
                    .id("themes-sidebar")
                    .w(px(248.0))
                    .flex_shrink_0()
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
                // The trail of previews this visit, newest first and never
                // the one on screen: the row above is the step back.
                let earlier: Vec<String> = self
                    .themes_history
                    .iter()
                    .rev()
                    .filter(|name| Some(name.as_str()) != picked.as_deref())
                    .take(4)
                    .cloned()
                    .collect();
                if !earlier.is_empty() {
                    let mut history = div().flex().flex_col().gap_1();
                    for name in earlier {
                        let pick = name.clone();
                        history = history.child(themes_view::history_row(&name).on_click(
                            cx.listener(move |this, _ev, _window, cx| {
                                this.preview_theme(pick.clone(), cx);
                            }),
                        ));
                    }
                    panel = panel
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_faint))
                                .child("preview history"),
                        )
                        .child(history);
                }
                let keep = cx.listener(|this, _ev: &ClickEvent, _window, cx| this.keep_theme(cx));
                panel = panel.child(
                    div()
                        .id("themes-keep")
                        .flex()
                        .justify_center()
                        .px_3()
                        .py_1()
                        .rounded(px(CARD_RADIUS))
                        .bg(rgb(shown.panel_hi))
                        .border_1()
                        .border_color(rgb(shown.accent))
                        .text_color(rgb(shown.text))
                        .cursor_pointer()
                        .on_click(keep)
                        .child("Keep"),
                );
                let hint = match &picked {
                    Some(name) => format!(
                        "previewing {name} · keep saves it for the {} system appearance · esc restores",
                        if palette::system_is_dark() {
                            "dark"
                        } else {
                            "light"
                        }
                    ),
                    None => "arrows walk the list · enter previews · esc restores".to_string(),
                };
                // The footer names a newer release and opens the update dialog (#2406).
                let update_hint = self.update_available.clone().map(|version| {
                    let ask = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                        this.update_prompt = Some(false);
                        cx.notify();
                    });
                    div()
                        .id("update-hint")
                        .text_xs()
                        .text_color(rgb(shown.accent))
                        .cursor_pointer()
                        .on_click(ask)
                        .child(SharedString::from(format!(
                            "v{version} available · run /evolve to update"
                        )))
                });
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
                            .child(SharedString::from(hint)),
                    )
                    .children(update_hint);
                let preview_width = pane_width - THEMES_PANEL_WIDTH;
                let chat_share = self
                    .panes
                    .as_ref()
                    .zip(self.transcript.session)
                    .and_then(|(tree, id)| tree.leaf_width_share(id))
                    .unwrap_or(1.0);
                let primary_width = if chat_share < 1.0 {
                    split_pane_width(preview_width, false, chat_share)
                } else {
                    preview_width
                };
                // A right press on the conversation opens the same card the
                // `...` draws (#2480); a left press anywhere in it closes the
                // card, the way clicking off a menu dismisses it.
                let ask_menu = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    if this.transcript.session.is_some() {
                        this.topic_menu = this.transcript.session;
                        cx.notify();
                    }
                });
                // A left press in the conversation closes the card. The card
                // is the column's sibling, not this element's child, so its
                // own clicks never reach this listener and picking an item
                // still works.
                let dismiss_menu = cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                    if this.topic_menu.take().is_some() {
                        cx.notify();
                    }
                });
                let primary = div()
                    .id("chat-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .on_mouse_down(MouseButton::Right, ask_menu)
                    .on_mouse_down(MouseButton::Left, dismiss_menu)
                    .child(transcript::view(
                        &self.transcript,
                        &self.transcript_scroll,
                        &self.live,
                        self.busy,
                        primary_width,
                        fold_handlers,
                        transcript::ViewHooks {
                            attachments: attachment_handlers,
                            drag: Some(self.copy_drag.clone()),
                            ctx_used: self.ctx_tokens,
                            ctx_max: self.ctx_max,
                            live_click: Some(live_click.clone()),
                        },
                    ));
                // The preview draws the same panes, but never resizes them:
                // its boxes are narrower than the real ones, so the dividers
                // there do not take the pointer (#2438, #2472).
                let body = self.panes_body(primary.into_any_element(), preview_width, false, cx);
                let middle = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(body)
                    .child(panel);
                (middle.into_any_element(), footer.into_any_element())
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
            .transcript
            .session
            .is_some_and(|id| self.sessions.parents.contains_key(&id));
        if self.screen == Screen::Chat && (self.open_chat_topics_on() || is_topic) {
            // The tabs (#2400): the family's parent, then its topics. The
            // leading button moves the whole row to another side (#2402).
            // The `...` menu is NOT here any more (#2480): it lives at the
            // conversation's top right, over the chat it acts on.
            let tabs = self.topic_tabs(cx);
            let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.cycle_topic_edge(cx);
            });
            let strip = topic_strip(self.topic_edge, toggle_edge, tabs);
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
        // The chat screen leads with the conversation's name in a card (#2480),
        // then the topic row under it, the way Telegram's forum header reads.
        // The other screens keep their title above the body.
        if self.screen != Screen::Chat {
            right = right.child(chrome);
        } else {
            // The conversation's name leads the chat as plain header text
            // (#2480), and the `...` that acts on it sits at the far end of
            // the same row: top right of the conversation, always. The name
            // takes the width; nothing draws a card around either of them.
            let name = self.open_chat_name();
            let mut header = div()
                .relative()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .min_w_0()
                // Auto cross-axis stretch lets the negative gutters reach
                // both edges. A definite full width would stop the right
                // hairline short; the title and menu keep their inset.
                .mx(rems(-1.5))
                .px_6()
                .pb_2()
                .border_b_1()
                .border_color(topic_tabs::separator_color());
            header = match name {
                Some(name) => header.child(session_title(&name)),
                None => header.child(div().flex_1()),
            };
            if self.transcript.session.is_some() {
                let toggle = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.topic_menu = match this.topic_menu {
                        Some(_) => None,
                        None => this.transcript.session,
                    };
                    cx.notify();
                });
                header = header.child(
                    div()
                        .id("chat-menu-button")
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_2()
                        .py_0p5()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(palette::current().text_muted))
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(toggle)
                        .child(SharedString::from("...")),
                );
            }
            right = right.child(header);
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
            right = right.child(bar);
        }
        right = right.child(middle);
        right = match self.screen {
            Screen::Chat => {
                // The running checklist sits between the conversation and the
                // input, the same place the TUI's plan card holds. The key
                // hints go back UNDER the input where they were: the owner
                // called the move above the plan card out.
                let mut col = right;
                if let Some(plan_view) = self.plan.view() {
                    col = col.child(plan_view);
                }
                let waiting = self.queued_for_strip();
                if !waiting.is_empty() {
                    col = col.child(queue_strip(&waiting));
                }
                // A detached command sits directly above the editor: the same
                // place the TUI puts it (on the input box's top border), which
                // is where the eye goes while waiting.
                if let Some(label) = self.running_tasks_label() {
                    col = col.child(task_strip(label));
                }
                // The slash-command list sits over the input while a name is
                // not being spelled, the terminal's own placement. The name
                // prompt itself is the overlay below (#2398).
                if self.topic_draft.is_none() {
                    let popup = self.editor.read(cx).command_popup();
                    if let Some((names, selected)) = popup {
                        col = col.child(command_popup_view(&names, selected));
                    } else if let Some((items, selected)) = self.editor.read(cx).emoji_popup() {
                        col = col.child(emoji_popup_view(&items, selected));
                    }
                }
                col = col.child(self.editor.clone());
                if let Some(strip) = topic_bottom {
                    col = col.child(strip);
                }
                col.child(footer)
            }
            Screen::Usage => right.child(footer),
            Screen::Files => right.child(footer),
            Screen::Themes => right.child(footer),
            Screen::Models => right.child(footer),
            Screen::Skills => right.child(footer),
            Screen::DirectoryPicker => right.child(footer),
            Screen::Onboard => right.child(footer),
            Screen::MissionControl => right.child(footer),
        };

        // The card is the column's LAST child on purpose. Children paint in
        // order, so a menu drawn inside the header is painted before the
        // transcript and the conversation covers it.
        if let Some(menu) = self.chat_menu(cx) {
            right = right.child(menu);
        }

        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(palette::current().bg))
            .text_color(rgb(palette::current().text))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            // A context menu closes when its row is left alone: any left
            // press in the window dismisses it, the rail rows do the same
            // in their own press listener first (#2354).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    // A press on the sidebar gives it the keyboard; a press
                    // anywhere else hands the keys back to the composer (#2417).
                    if this.rail_open {
                        this.rail_focused = f32::from(ev.position.x) <= this.rail_width * this.rem;
                    }
                    if this.sessions.rail_menu.take().is_some() {
                        cx.notify();
                    }
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
                // A rail row slid left far enough: open its actions. The
                // threshold keeps a wobbling click from revealing anything.
                if let Some((id, from_x)) = this.rail_swipe
                    && sessions::slide_reveals(from_x, f32::from(ev.position.x))
                    && this.rail_actions != Some(id)
                {
                    this.rail_actions = Some(id);
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
                let added = this.editor.update(cx, |editor, cx| {
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
                    this.split_drag = None;
                }),
            );
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
                this.rail_actions = None;
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
            };
            root = root.child(help_overlay(&view));
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
            let label = if self.topic_rename.is_some() {
                "Rename topic"
            } else {
                "New topic"
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
            let picks: Vec<Stateful<Div>> = topic_icons::TOPIC_ICONS
                .iter()
                .map(|&(name, bytes)| {
                    let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.topic_draft_icon = Some(topic_icons::store_name(name));
                        cx.notify();
                    });
                    div()
                        .id(SharedString::from(format!("topic-icon-{name}")))
                        .p_0p5()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(pick)
                        .child(topic_icons::picture(bytes, 1.5))
                })
                .collect();
            root = root.child(topic_prompt_overlay(label, &draft, &icon, picks, cancel));
        }
        // The update dialog (#2406): two stages, and /evolve runs only after the second yes.
        if let Some(second) = self.update_prompt {
            let version = self.update_available.clone().unwrap_or_default();
            let confirm = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                if second {
                    this.update_prompt = None;
                    this.run_command(commands::Outcome::Run(commands::GuiAction::Evolve), cx);
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
    // First, so a panic during the first fetch still lands in the trail.
    logging::init();
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
            selected: None,
            cursor: None,
            rail_menu: None,
            error: None,
            filter: String::new(),
            filter_active: false,
        },
        Err(e) => sessions::SessionsState {
            items: Vec::new(),
            favorites,
            topics,
            parents,
            closed,
            topic_icons,
            topic_titles: HashMap::new(),
            selected: None,
            cursor: None,
            rail_menu: None,
            error: Some(e.into()),
            filter: String::new(),
            filter_active: false,
        },
    };
    // Chat is the default screen: the conversation the user was in opens on
    // launch, and a first run opens the newest. The usage ledger is something
    // the user asks for (ctrl+u); the two never share the pane, so a
    // conversation can never render as "usage" (#2444).
    let initial_session = pick_initial_session(
        &sessions_state.items,
        opencrabs::utils::last_session::read(),
    );
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
                ..Default::default()
            },
            |window, cx| {
                let focus = cx.focus_handle();
                let editor = cx.new(editor::Editor::new);
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
                    Dashboard {
                        screen: Screen::Chat,
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
                        rail_width: RAIL_DEFAULT_REM,
                        rail_open: true,
                        rail_focused: false,
                        update_available: None,
                        update_prompt: None,
                        help_filter: None,
                        panes: None,
                        pane_states: HashMap::new(),
                        pane_focus: None,
                        split_areas: Rc::new(std::cell::RefCell::new(HashMap::new())),
                        split_drag: None,
                        split_target: None,
                        copy_drag: Rc::new(CopyDrag::default()),
                        split_pick: None,
                        split_creating: false,
                        topic_draft_icon: None,
                        help_scroll: ScrollHandle::new(),
                        dragging_rail: false,
                        drag_from: None,
                        drag_seen: None,
                        rail_actions: None,
                        rail_swipe: None,
                        rail_slide_opened: None,
                        confirm_delete: None,
                        preview: None,
                        preview_zoom: 1.0,
                        preview_video: None,
                        help_open: false,
                        topic_menu: None,
                        topic_draft: None,
                        topic_edge: topic_tabs::Edge::from_config(
                            Config::load()
                                .ok()
                                .and_then(|c| c.gui.topic_tabs)
                                .as_deref(),
                        ),
                        topic_rename: None,
                        topic_info: None,
                        themes_origin: None,
                        themes_pick: None,
                        themes_index: 0,
                        themes_history: Vec::new(),
                        cd_dir: std::env::current_dir().unwrap_or_default(),
                        cd_entries: Vec::new(),
                        cd_selected: 0,
                        cd_show_hidden: false,
                        models_rows: Vec::new(),
                        models_query: String::new(),
                        models_index: 0,
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
                        onboard: None,
                        plan: plan_strip::PlanStripState::default(),
                        plan_for: None,
                        transcript: transcript::TranscriptState {
                            session: initial_session,
                            ..Default::default()
                        },
                        transcript_scroll: ScrollHandle::new(),
                        rail_scroll: ScrollHandle::new(),
                        editor,
                        agent_rx,
                        sink: agent_sink,
                        agent_ready: false,
                        live: transcript::LiveTurn::default(),
                        settle_expect: None,
                        settle_retries: 0,
                        busy: false,
                        cancel: None,
                        esc_at: None,
                        queued: Vec::new(),
                        agent_error: None,
                        ctx_tokens: None,
                        ctx_max: 0,
                        turn_metrics: HashMap::new(),
                        pending_metrics: None,
                        branch: git_branch(&opencrabs::utils::cwd::launch_cwd()),
                    }
                });
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
                window.focus(&view.read(cx).editor.read(cx).focus_handle);
                // Open the newest conversation before the first paint (chat
                // is the default). The fetch runs off the UI thread, like
                // every other DB read in this window.
                if let Some(id) = initial_session {
                    view.update(cx, |this, cx| this.open_session(id, cx));
                }
                view
            },
        )
        .expect("failed to open the gpui window");
        cx.activate(true);
    });
}

/// One Mission Control frame: a responsive grid of cards drawn from the
/// snapshot the four services produced when the screen opened. Every row here
/// was read from a service; a quiet instance says so, and a failed read says
/// that instead, because a card that invents a row is worse than one that
/// admits it has none (#2426).
///
/// The grid reflows with the pane (#2459). The terminal draws a fixed 2x2 and
/// hides the panels that do not fit; a window has the room to draw all of
/// them, so the column count follows the width it actually has and a resize
/// never pushes a card out of the frame. Each card is sized by the grid and
/// clips its own rows: a twenty-five item inbox must not decide how tall the
/// screen is.
fn mission_control_body(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    err: Option<&SharedString>,
    loading: bool,
    shown: palette::Palette,
    cols: usize,
) -> impl IntoElement {
    let cards = mission_control_cards(snapshot, nav, shown);
    let count = cards.len();
    let mut grid = div()
        .id("mission-control-body")
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_w_0()
        .min_h_0()
        .gap_2()
        .px_4()
        .pb_2();
    if let Some(err) = err {
        grid = grid.child(text(format!("snapshot failed: {err}"), Size::Xs, shown.red));
    } else if loading && snapshot.is_none() {
        grid = grid.child(text("reading the services…", Size::Xs, shown.text_muted));
    }
    let mut cards = cards.into_iter();
    for slots in mission_control::grid_rows(count, cols) {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .gap_2();
        for slot in slots {
            match slot {
                Some(_) => {
                    if let Some(card) = cards.next() {
                        row = row.child(card);
                    }
                }
                // An empty slot holds the last row's cards to the same width
                // as the rows above instead of letting one card stretch over
                // the whole pane.
                None => row = row.child(div().flex_1().min_w_0().min_h_0()),
            }
        }
        grid = grid.child(row);
    }
    grid
}

/// The cards the screen can show, in reading order: the three lists the
/// terminal keeps on its left, then the analytics breakdown, one card per
/// section (#2459). An analytics section with nothing in it is skipped, so a
/// new instance shows four cards rather than seven empty boxes.
fn mission_control_cards(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Vec<Div> {
    let mut cards = vec![
        mc_inbox_panel(snapshot, nav, shown),
        mc_activity_panel(snapshot, nav, shown),
        mc_schedule_panel(snapshot, nav, shown),
    ];
    cards.extend(mc_analytics_cards(
        snapshot,
        nav.panel == mission_control::Panel::Analytics,
        shown,
    ));
    cards
}

/// The analytics sections shared by the dashboard and its detail popup.
/// Reusing this list keeps the popup from becoming a second, stale renderer.
fn mc_analytics_cards(
    snapshot: Option<&mission_control::Snapshot>,
    focused: bool,
    shown: palette::Palette,
) -> Vec<Div> {
    let analytics = snapshot.map(|s| &s.analytics);
    let mut cards = vec![mc_totals_panel(snapshot, focused, shown)];
    let Some(a) = analytics else {
        return cards;
    };
    if !a.model_tools.is_empty() {
        cards.push(mc_models_panel(a, shown));
    }
    if !a.flakiest_tools.is_empty() {
        cards.push(mc_flakiest_panel(
            a,
            snapshot.map(|s| s.window).unwrap_or_default(),
            shown,
        ));
    }
    if a.phantom.total > 0 {
        cards.push(mc_phantoms_panel(a, shown));
    }
    if !a.rsi_top_dimensions.is_empty() {
        cards.push(mc_rsi_panel(a, shown));
    }
    if !a.brain_files.is_empty() {
        cards.push(mc_brain_panel(a, shown));
    }
    if !a.top_tools.is_empty() {
        cards.push(mc_top_tools_panel(a, shown));
    }
    cards
}

/// One row inside a card. The lists stay one truncated line each; the
/// analytics numbers are a label and a value, so the value sits flush right
/// and the label ellipsizes before it, which keeps the columns readable at any
/// card width.
enum McRow {
    /// A whole line, truncated when the card is narrower than it.
    Line(String, u32),
    /// A label on the left, a value flush against the right.
    Pair(String, String, u32),
}

impl McRow {
    fn color(&self) -> u32 {
        match self {
            Self::Line(_, color) | Self::Pair(_, _, color) => *color,
        }
    }
}

/// One card: a title, then the rows the services produced. The card is a grid
/// cell first, so it takes the width and height the grid gives it and never
/// grows to fit its content: `min_w_0` stops a long shell command from
/// widening its column, `min_h_0` plus `overflow_hidden` stop a long list from
/// pushing the row below it out of the frame, and the row area scrolls when
/// there is more than the card can show (#2459).
fn mc_panel(
    id: &'static str,
    title: String,
    rows: Vec<McRow>,
    empty: &'static str,
    focused: bool,
    selected: Option<usize>,
    shown: palette::Palette,
) -> Div {
    let title_color = if focused {
        shown.focus
    } else {
        shown.text_title
    };
    let mut rows_view = div()
        .id(SharedString::from(format!("mc-rows-{id}")))
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_w_0()
        .min_h_0()
        .gap_1()
        .overflow_y_scroll();
    if rows.is_empty() {
        rows_view = rows_view.child(text(empty, Size::Xs, shown.text_faint));
    }
    for (index, row) in rows.into_iter().enumerate() {
        let armed = selected == Some(index);
        let color = if armed { shown.focus } else { row.color() };
        let mut cell = div().w_full().min_w_0().text_color(rgb(color));
        if armed {
            cell = cell.bg(rgb(shown.selection)).rounded(px(4.0)).px_1();
        }
        match row {
            McRow::Line(line, _) => cell = cell.text_xs().truncate().child(line),
            McRow::Pair(label, value, _) => {
                cell = cell
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(text(label, Size::Xs, color).flex_1().min_w_0().truncate())
                    .child(text(value, Size::Xs, color).flex_none());
            }
        }
        rows_view = rows_view.child(cell);
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        // The focused card wears the ring the rest of the window uses, so the
        // keyboard's target is never a guess.
        .border_color(rgb(if focused { shown.focus } else { shown.panel_hi }))
        .bg(glass_fill(shown.panel))
        .child(text(title, Size::Sm, title_color))
        .child(rows_view)
}

/// The RSI proposals nobody has decided yet: kind and label, then the summary
/// the agent wrote for it. Six fit the card and the window slides with the
/// selection instead of hiding whatever fell past the sixth, so `j` never walks
/// off the screen.
fn mc_inbox_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    let items = snapshot.map(|s| s.inbox.as_slice()).unwrap_or_default();
    let selected = nav.selected.min(items.len().saturating_sub(1));
    let (start, len) = mission_control::visible_window(items.len(), Some(selected), 6);
    let mut rows = Vec::new();
    for item in items.iter().skip(start).take(len) {
        rows.push(McRow::Line(
            format!("{} \u{b7} {}", item.kind.label(), item.label),
            shown.text_normal,
        ));
        rows.push(McRow::Line(format!("  {}", item.summary), shown.text_faint));
        rows.push(McRow::Line(
            format!(
                "  {} \u{b7} {}",
                item.source,
                mc_relative_time(Some(item.created_at))
            ),
            shown.text_faint,
        ));
    }
    let focused = nav.panel == mission_control::Panel::Inbox;
    mc_panel(
        "inbox",
        format!("Inbox ({})", items.len()),
        rows,
        "No pending proposals.",
        focused,
        // The selected item's first line, in the rows actually drawn.
        (focused && !items.is_empty()).then(|| (selected - start) * 3),
        shown,
    )
}

/// The analytics tab line: the terminal's D/W/M/All, the active one bracketed.
fn mc_window_tabs(window: mission_control::TimeWindow) -> String {
    use mission_control::TimeWindow;
    [
        (TimeWindow::Day, "D"),
        (TimeWindow::Week, "W"),
        (TimeWindow::Month, "M"),
        (TimeWindow::All, "All"),
    ]
    .iter()
    .map(|(w, label)| {
        if *w == window {
            format!("[{label}]")
        } else {
            (*label).to_string()
        }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// The label the flakiest card carries, the same words the terminal uses.
fn mc_window_label(window: mission_control::TimeWindow) -> &'static str {
    use mission_control::TimeWindow;
    match window {
        TimeWindow::Day => "24h",
        TimeWindow::Week => "7d",
        TimeWindow::Month => "30d",
        TimeWindow::All => "all-time",
    }
}

/// A failure rate's colour at both ends of the scale, so the flakiest card and
/// the model card agree about what "bad" looks like.
fn mc_rate_color(rate: f64, shown: palette::Palette) -> u32 {
    if rate >= 25.0 {
        shown.red
    } else if rate >= 10.0 {
        shown.warn
    } else {
        shown.text_muted
    }
}

/// Usage and reliability for the selected window: the filter tabs, the totals
/// the terminal leads with, the RSI liveness line (#469), and the brain size.
fn mc_totals_panel(
    snapshot: Option<&mission_control::Snapshot>,
    focused: bool,
    shown: palette::Palette,
) -> Div {
    let mut rows: Vec<McRow> = Vec::new();
    if let Some(snap) = snapshot {
        let a = &snap.analytics;
        let fail_pct = if a.tool_total_calls > 0 {
            (a.tool_total_fails as f64 / a.tool_total_calls as f64) * 100.0
        } else {
            0.0
        };
        // The snapshot carries the window it was read for; the tabs show that
        // one, so a slow read can never label November numbers as September's.
        rows.push(McRow::Line(mc_window_tabs(snap.window), shown.teal));
        rows.push(McRow::Pair(
            "Tools".to_string(),
            format!("{} calls", a.tool_total_calls),
            shown.text_normal,
        ));
        rows.push(McRow::Pair(
            "Fails".to_string(),
            format!("{} ({fail_pct:.1}%)", a.tool_total_fails),
            if a.tool_total_fails > 0 {
                shown.warn
            } else {
                shown.text_muted
            },
        ));
        rows.push(McRow::Pair(
            "Recov".to_string(),
            format!("{} recovered", a.streaming.total),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "Verify".to_string(),
            format!(
                "{} ok / {} rollbk",
                a.brain_verify.passes, a.brain_verify.rollbacks
            ),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "RSI".to_string(),
            format!("{} applied", a.rsi_applied_total),
            shown.text_muted,
        ));
        rows.push(McRow::Pair(
            "RSI live".to_string(),
            opencrabs::brain::mission_control::staleness::rsi_staleness_line(
                chrono::Utc::now().timestamp(),
                a.rsi_last_call_ts,
                a.tool_events_since_rsi,
            ),
            shown.text_faint,
        ));
        rows.push(McRow::Pair(
            "Brain".to_string(),
            format!("{:.1} KB / {} files", a.brain_total_kb, a.brain_files.len()),
            shown.text_muted,
        ));
    }
    mc_panel(
        "totals",
        "Totals".to_string(),
        rows,
        "No numbers yet.",
        focused,
        None,
        shown,
    )
}

/// Per-model tool reliability, most calls first, each row carrying the
/// terminal's ✓/⚠/✗ health glyph and its phantom rate (#900).
fn mc_models_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .model_tools
        .iter()
        .take(8)
        .map(|m| {
            let phantoms = a
                .phantom
                .by_model
                .iter()
                .find(|(name, _, _)| name == &m.model)
                .map(|(_, total, _)| *total)
                .unwrap_or(0);
            let phantom_rate = if m.total > 0 {
                (phantoms as f64 / m.total as f64) * 100.0
            } else {
                0.0
            };
            let (glyph, color) = if m.fail_rate >= 15.0 || phantom_rate >= 10.0 {
                ("\u{2717}", shown.red)
            } else if m.fail_rate >= 5.0 || phantom_rate > 0.0 {
                ("\u{26a0}", shown.warn)
            } else {
                ("\u{2713}", shown.green)
            };
            McRow::Pair(
                m.model.clone(),
                format!(
                    "{glyph} {:.1}% fail \u{b7} {:.1}% ph",
                    m.fail_rate, phantom_rate
                ),
                color,
            )
        })
        .collect();
    mc_panel(
        "models",
        "Model reliability".to_string(),
        rows,
        "No models yet.",
        false,
        None,
        shown,
    )
}

/// Highest failure rates, the window named in the title the way the terminal
/// names it.
fn mc_flakiest_panel(
    a: &McAnalytics,
    window: mission_control::TimeWindow,
    shown: palette::Palette,
) -> Div {
    let rows = a
        .flakiest_tools
        .iter()
        .take(8)
        .map(|t| {
            McRow::Pair(
                t.name.clone(),
                format!("{:.1}%", t.fail_rate),
                mc_rate_color(t.fail_rate, shown),
            )
        })
        .collect();
    mc_panel(
        "flakiest",
        format!("Flakiest ({})", mc_window_label(window)),
        rows,
        "No flaky tools.",
        false,
        None,
        shown,
    )
}

/// Rows for the phantom card. A totals-only line can ellipsize into three
/// dots under the header, so an empty model breakdown stays empty and lets
/// the shared panel render its empty state instead.
fn mc_phantoms_rows(a: &McAnalytics, shown: palette::Palette) -> Vec<McRow> {
    if a.phantom.by_model.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![McRow::Line(
        format!(
            "{} detected, {} resolved ({:.1}%)",
            a.phantom.total, a.phantom.resolved, a.phantom.resolved_pct
        ),
        shown.green,
    )];
    for (model, total, resolved) in a.phantom.by_model.iter().take(6) {
        rows.push(McRow::Pair(
            model.clone(),
            format!("{total} ({resolved} resolved)"),
            shown.text_muted,
        ));
    }
    rows
}

/// Phantom tool calls: detected, resolved, then the per-model breakdown the
/// terminal shows.
fn mc_phantoms_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    mc_panel(
        "phantoms",
        "Phantoms".to_string(),
        mc_phantoms_rows(a, shown),
        "No phantoms detected.",
        false,
        None,
        shown,
    )
}

/// RSI improvements applied, counted per dimension.
fn mc_rsi_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .rsi_top_dimensions
        .iter()
        .take(8)
        .map(|(dim, n)| McRow::Pair(dim.clone(), n.to_string(), shown.green))
        .collect();
    mc_panel(
        "rsi",
        "RSI applied".to_string(),
        rows,
        "Nothing applied yet.",
        false,
        None,
        shown,
    )
}

/// The brain files and their sizes, largest first.
fn mc_brain_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let rows = a
        .brain_files
        .iter()
        .take(14)
        .map(|f| McRow::Pair(f.name.clone(), format!("{:.1} KB", f.kb), shown.text_muted))
        .collect();
    mc_panel(
        "brain",
        "Brain files".to_string(),
        rows,
        "No brain files.",
        false,
        None,
        shown,
    )
}

const MC_TOOL_BAR_WIDTH: usize = 12;

/// The same proportional usage bar as the terminal, kept fixed-width so the
/// call count and failure rate remain aligned in a GUI card.
fn mc_tool_bar(total: i64, max: i64) -> String {
    let max = max.max(1);
    let filled = ((total.max(0) as f64 / max as f64) * MC_TOOL_BAR_WIDTH as f64)
        .round()
        .clamp(0.0, MC_TOOL_BAR_WIDTH as f64) as usize;
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(MC_TOOL_BAR_WIDTH - filled)
    )
}

/// The busiest tools, with the proportional bar, call count and failure rate
/// the terminal pairs with each name.
fn mc_top_tools_panel(a: &McAnalytics, shown: palette::Palette) -> Div {
    let max = a
        .top_tools
        .iter()
        .map(|t| t.total)
        .max()
        .unwrap_or(1)
        .max(1);
    let rows = a
        .top_tools
        .iter()
        .take(10)
        .map(|t| {
            let color = if t.failures > 0 {
                shown.text_normal
            } else {
                shown.text_muted
            };
            McRow::Pair(
                t.name.clone(),
                format!(
                    "{} {} calls \u{b7} {} failed \u{b7} {:.1}%",
                    mc_tool_bar(t.total, max),
                    t.total,
                    t.failures,
                    t.fail_rate
                ),
                color,
            )
        })
        .collect();
    mc_panel(
        "top-tools",
        "Top tools".to_string(),
        rows,
        "No tools yet.",
        false,
        None,
        shown,
    )
}

/// The RSI journal, newest first, each row carrying the level glyph the
/// terminal paints, and the selection that follows the keyboard.
fn mc_activity_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    use opencrabs::brain::mission_control::McActivityLevel;
    let entries = snapshot.map(|s| s.activity.as_slice()).unwrap_or_default();
    let focused = nav.panel == mission_control::Panel::Activity;
    let mut rows = Vec::new();
    for entry in entries {
        let (glyph, color) = match entry.level {
            McActivityLevel::Error => ("\u{2717}", shown.red),
            McActivityLevel::Warn => ("\u{26a0}", shown.warn),
            McActivityLevel::Success => ("\u{2713}", shown.green),
            McActivityLevel::Info => ("\u{b7}", shown.text_muted),
        };
        rows.push(McRow::Line(
            format!(
                "{glyph} {} \u{b7} {}",
                mc_relative_time(entry.timestamp),
                entry.detail
            ),
            color,
        ));
    }
    let (start, len) = mission_control::visible_window(
        rows.len(),
        focused.then_some(nav.selected),
        mission_control::ACTIVITY_ROWS,
    );
    let selected = focused.then(|| nav.selected.saturating_sub(start));
    let rows: Vec<McRow> = rows.into_iter().skip(start).take(len).collect();
    let selected = selected.filter(|_| !rows.is_empty());
    mc_panel(
        "activity",
        format!("Activity ({})", entries.len()),
        rows,
        "No activity yet.",
        focused,
        selected,
        shown,
    )
}

/// Cron jobs and pending approvals: label, expression, and the state the
/// terminal marks (awaiting you, or paused), with the focused row selected.
fn mc_schedule_panel(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    shown: palette::Palette,
) -> Div {
    let jobs = snapshot.map(|s| s.schedule.as_slice()).unwrap_or_default();
    let focused = nav.panel == mission_control::Panel::Schedule;
    let mut rows = Vec::new();
    for job in jobs {
        let color = if job.awaiting_user {
            shown.amber
        } else if job.enabled {
            shown.text_normal
        } else {
            shown.text_faint
        };
        let state = if job.awaiting_user {
            " \u{b7} awaiting you"
        } else if !job.enabled {
            " \u{b7} paused"
        } else {
            ""
        };
        rows.push(McRow::Line(
            format!(
                "{} \u{b7} {} \u{b7} {}{}",
                job.kind.label(),
                job.label,
                job.schedule,
                state
            ),
            color,
        ));
    }
    let (start, len) = mission_control::visible_window(
        rows.len(),
        focused.then_some(nav.selected),
        mission_control::ACTIVITY_ROWS,
    );
    let selected = focused.then(|| nav.selected.saturating_sub(start));
    let rows: Vec<McRow> = rows.into_iter().skip(start).take(len).collect();
    let selected = selected.filter(|_| !rows.is_empty());
    mc_panel(
        "schedule",
        format!("Schedule ({})", jobs.len()),
        rows,
        "No scheduled jobs.",
        focused,
        selected,
        shown,
    )
}

/// The focused panel's full record or analytics dashboard, drawn over
/// everything. Field order follows `tui/render/mission_control/detail_popup.rs`
/// and every value comes from the snapshot already on hand. Enter opens it,
/// Esc closes it, and a click on the card is inert so reading never throws it
/// away.
fn mission_control_detail(
    snapshot: Option<&mission_control::Snapshot>,
    nav: &mission_control::Nav,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let shown = palette::current();
    let title = match nav.panel {
        mission_control::Panel::Inbox => snapshot
            .and_then(|s| s.inbox.get(nav.selected))
            .map(|item| format!("Inbox detail \u{b7} {}", item.kind.label())),
        _ => None,
    }
    .unwrap_or_else(|| format!("{} detail", nav.panel.title()));
    let close = std::rc::Rc::new(close);
    let scrim_close = close.clone();
    let button_close = close.clone();
    let mut card = div()
        .id("mission-control-detail")
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .w_full()
        .max_w(px(720.0))
        .max_h(px(560.0))
        // The field area scrolls inside this bounded card.
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(glass_fill(shown.panel))
        // Inert: the click stops here instead of reaching the scrim.
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mb_1()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(shown.focus))
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .id("mission-control-detail-close")
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .cursor_pointer()
                        .child("esc")
                        .on_click(move |ev, window, cx| button_close(ev, window, cx)),
                ),
        );
    let body = match nav.panel {
        mission_control::Panel::Inbox => mc_inbox_detail(
            snapshot.and_then(|s| s.inbox.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Activity => mc_activity_detail(
            snapshot.and_then(|s| s.activity.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Schedule => mc_schedule_detail(
            snapshot.and_then(|s| s.schedule.get(nav.selected)).cloned(),
            shown,
        ),
        mission_control::Panel::Analytics => mc_analytics_detail(snapshot, shown),
    };
    card = card.child(body);
    div()
        .id("mission-control-detail-scrim")
        // Modal: the dim scrim owns the pointer, so a click meant for a panel
        // behind it cannot move the selection under the popup.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0add))
        .on_click(move |ev, window, cx| scrim_close(ev, window, cx))
        .child(card)
}

/// The scrollable body shared by all four detail views.
fn mc_detail_body() -> Stateful<Div> {
    div()
        .id("mission-control-detail-fields")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_1()
        .overflow_y_scroll()
}

fn mc_detail_heading(title: &'static str, shown: palette::Palette) -> Div {
    div()
        .mt_2()
        .text_xs()
        .text_color(rgb(shown.text_title))
        .child(title)
}

fn mc_inbox_detail(item: Option<McInboxItem>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(item) = item else {
        return body.child(text("No proposal selected.", Size::Xs, shown.text_faint));
    };
    body = body
        .child(mc_kv("Kind", item.kind.label().to_string(), shown))
        .child(mc_kv("Label", item.label.clone(), shown))
        .child(mc_kv("Source", item.source.clone(), shown))
        .child(mc_kv(
            "Filed",
            item.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            shown,
        ))
        .child(mc_kv("ID", item.id.clone(), shown))
        .child(mc_detail_heading("Summary", shown));
    for line in item.summary.split('\n') {
        body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
    }
    body
}

fn mc_activity_level_label(
    level: opencrabs::brain::mission_control::McActivityLevel,
) -> &'static str {
    use opencrabs::brain::mission_control::McActivityLevel;
    match level {
        McActivityLevel::Success => "success",
        McActivityLevel::Info => "info",
        McActivityLevel::Warn => "warn",
        McActivityLevel::Error => "error",
    }
}

fn mc_activity_detail(entry: Option<McActivity>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(entry) = entry else {
        return body.child(text("No activity selected.", Size::Xs, shown.text_faint));
    };
    body = body
        .child(mc_kv(
            "When",
            entry
                .timestamp
                .map(|ts| ts.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "undated".into()),
            shown,
        ))
        .child(mc_kv("Source", entry.source.clone(), shown))
        .child(mc_kv(
            "Level",
            mc_activity_level_label(entry.level).to_string(),
            shown,
        ))
        .child(mc_detail_heading("Detail", shown));
    for line in entry.detail.split('\n') {
        body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
    }
    body
}

fn mc_schedule_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn mc_schedule_detail(job: Option<McScheduleItem>, shown: palette::Palette) -> Stateful<Div> {
    let mut body = mc_detail_body();
    let Some(job) = job else {
        return body.child(text(
            "No schedule item selected.",
            Size::Xs,
            shown.text_faint,
        ));
    };
    let kind = match job.kind {
        opencrabs::brain::mission_control::McScheduleKind::Cron => "cron job",
        opencrabs::brain::mission_control::McScheduleKind::PendingApproval => "pending approval",
    };
    let state = if job.awaiting_user {
        "awaiting user"
    } else if !job.enabled {
        "paused"
    } else {
        "active"
    };

    body = body
        .child(mc_kv("Kind", kind.to_string(), shown))
        .child(mc_kv("Label", job.label.clone(), shown))
        .child(mc_kv("State", state.to_string(), shown))
        .child(mc_detail_heading("Schedule", shown))
        .child(mc_kv("Cron", job.schedule.clone(), shown))
        .child(mc_kv(
            "Next",
            mc_timestamp(job.next_run_at).unwrap_or_else(|| "not scheduled".into()),
            shown,
        ))
        .child(mc_detail_heading("Last run", shown))
        .child(mc_kv(
            "At",
            mc_timestamp(job.last_run_at).unwrap_or_else(|| "never".into()),
            shown,
        ));
    if let Some(status) = job.last_run_status.as_deref() {
        let status = match status {
            "running" => "running...",
            other => other,
        };
        body = body.child(mc_kv("Status", status.to_string(), shown));
    }
    if let Some(cost) = job.last_run_cost {
        body = body.child(mc_kv("Cost", format!("${cost:.4}"), shown));
    }
    if let Some(secs) = job.last_run_duration_secs {
        body = body.child(mc_kv("Duration", mc_schedule_duration(secs), shown));
    }
    if let Some(target) = job.deliver_to.as_deref() {
        body = body
            .child(mc_detail_heading("Delivery", shown))
            .child(mc_kv("Target", target.to_string(), shown));
    }
    if !job.prompt.is_empty() {
        let prompt = if job.prompt.len() > 300 {
            format!(
                "{}...",
                opencrabs::utils::string::truncate_str(&job.prompt, 300)
            )
        } else {
            job.prompt.clone()
        };
        body = body.child(mc_detail_heading("Prompt", shown));
        for line in prompt.split('\n') {
            body = body.child(text(line.to_string(), Size::Xs, shown.text_normal));
        }
    }
    body = body.child(mc_detail_heading("Meta", shown)).child(mc_kv(
        "Created",
        job.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        shown,
    ));
    if let Some(profile) = job.profile_name.as_deref() {
        body = body.child(mc_kv("Profile", profile.to_string(), shown));
    }
    body.child(mc_kv("ID", job.id.clone(), shown))
}

fn mc_analytics_detail(
    snapshot: Option<&mission_control::Snapshot>,
    shown: palette::Palette,
) -> Stateful<Div> {
    let cards = mc_analytics_cards(snapshot, true, shown);
    let count = cards.len();
    let mut cards = cards.into_iter();
    let mut grid = div().flex().flex_col().w_full().gap_2().pr_1();
    for slots in mission_control::grid_rows(count, 2) {
        let mut row = div().flex().flex_row().w_full().h(px(180.0)).gap_2();
        for slot in slots {
            match slot {
                Some(_) => {
                    if let Some(card) = cards.next() {
                        row = row.child(card);
                    }
                }
                None => row = row.child(div().flex_1().min_w_0()),
            }
        }
        grid = grid.child(row);
    }
    mc_detail_body().child(grid)
}

/// How many log lines the window draws at once: a page key moves by a screen,
/// and a 512 KB tail never becomes thousands of elements in one frame.
const MC_LOG_ROWS: usize = 44;

/// The log reader, drawn over the screen it was opened from. The state, its
/// filters and its file paging are the terminal's own type, so the two
/// clients read the same lines with the same parser and the same levels.
fn mission_control_logs(
    viewer: &mut LogViewerState,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    use opencrabs::logging::reader::LogLevel;
    let shown = palette::current();
    // The state clamps scrolling to what fits; the renderer is the part that
    // knows how many rows fit, exactly as in the terminal.
    viewer.viewport_rows = MC_LOG_ROWS;
    let scroll = viewer.scroll.min(viewer.max_scroll());
    let file = viewer.file_name();
    let level = viewer.min_level.label();
    let total = viewer.entries.len();
    let filtered = viewer.visible().len();
    let search = viewer.search.clone();
    let searching = viewer.search_active;
    let error = viewer.error.clone();

    // The rows on screen: each entry's header, then its continuation lines,
    // starting at the scroll offset and stopping at the bottom.
    let mut rows: Vec<(String, u32)> = Vec::new();
    let mut row = 0usize;
    'outer: for entry in viewer.visible() {
        let color = match entry.level {
            LogLevel::Error => shown.red,
            LogLevel::Warn => shown.warn,
            LogLevel::Info => shown.text_normal,
            LogLevel::Debug => shown.text_muted,
            LogLevel::Trace => shown.text_faint,
        };
        let lines = std::iter::once((
            format!(
                "{} {:<5} {} \u{b7} {}",
                entry.timestamp,
                entry.level.label(),
                entry.target,
                entry.message
            ),
            color,
        ))
        .chain(
            entry
                .continuation
                .iter()
                .map(|line| (format!("  {line}"), shown.text_faint)),
        );
        for (line, color) in lines {
            if row >= scroll {
                rows.push((line, color));
            }
            row += 1;
            if rows.len() >= MC_LOG_ROWS {
                break 'outer;
            }
        }
    }

    let close = std::rc::Rc::new(close);
    let scrim_close = close.clone();
    let mut card = div()
        .id("mission-control-logs")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_1()
        .p_3()
        .w_full()
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(glass_fill(shown.panel))
        // Inert: a click inside the reader never closes it by accident.
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(shown.focus))
                        .truncate()
                        .child(format!(
                            "{file} \u{b7} level {level} \u{b7} {filtered}/{total} entries"
                        )),
                )
                .child(
                    div()
                        .id("mission-control-logs-close")
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .cursor_pointer()
                        .child("esc")
                        .on_click(move |ev, window, cx| scrim_close(ev, window, cx)),
                ),
        );
    if !search.is_empty() || searching {
        // The search box, with a caret while the keys are going into it.
        card = card.child(text(
            format!("/{search}{}", if searching { "\u{258f}" } else { "" }),
            Size::Xs,
            if searching {
                shown.focus
            } else {
                shown.text_muted
            },
        ));
    }
    if let Some(err) = error {
        card = card.child(text(err, Size::Xs, shown.red));
    } else if rows.is_empty() {
        card = card.child(text("No entries match.", Size::Xs, shown.text_faint));
    }
    let mut body = div().flex().flex_col().flex_1().min_h_0();
    for (line, color) in rows {
        body = body.child(
            div()
                .text_xs()
                .truncate()
                .text_color(rgb(color))
                .child(line),
        );
    }
    card = card.child(body).child(text(
        "j/k scroll \u{b7} PgUp/PgDn page \u{b7} g/G ends \u{b7} / search \u{b7} [ ] file \u{b7} 1-5 level \u{b7} Esc close",
        Size::Xs,
        shown.text_muted,
    ));
    div()
        .id("mission-control-logs-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .p_4()
        .bg(rgba(0x0a0a0add))
        .on_click(move |ev, window, cx| close(ev, window, cx))
        .child(card)
}

/// One label/value line in the detail popup, in the terminal's columns.
fn mc_kv(key: &str, value: String, shown: palette::Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            div()
                .w(px(56.0))
                .text_xs()
                .text_color(rgb(shown.text_title))
                .child(key.to_string()),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(rgb(shown.text_normal))
                .truncate()
                .child(value),
        )
}

/// Format an optional timestamp for the schedule detail fields.
fn mc_timestamp(ts: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    ts.map(|ts| ts.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// An event's age in the terminal's own shorthand, and "undated" when the
/// journal had no parseable time: a missing date must never read as now.
fn mc_relative_time(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let Some(ts) = ts else {
        return "undated".to_string();
    };
    let secs = (chrono::Utc::now() - ts).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// The queued messages, above the input where the user can see what will go
/// out next. Same shape as the TUI's pending list: one line per message, the
/// count in the header, and a word on when they send.
fn queue_strip(queued: &[agent_bridge::PendingMessage]) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().amber))
                .child(SharedString::from(format!(
                    "queued {} · sent between tool calls · ↑ edits the last",
                    queued.len()
                ))),
        );
    for (i, m) in queued.iter().take(4).enumerate() {
        let text = m.display_text.lines().next().unwrap_or_default();
        let mut chars = text.chars();
        let short: String = chars.by_ref().take(96).collect();
        let suffix = if chars.next().is_some() { "…" } else { "" };
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("{}. {short}{suffix}", i + 1))),
        );
    }
    col
}

/// Which slice of the list the popup paints.
///
/// The window never moves until the selection walks past its last row, so the
/// eye keeps a stable block of names instead of a list that scrolls on every
/// arrow. A selection outside the range is clamped, because the list shrinks
/// as the draft grows.
fn popup_window(total: usize, selected: usize, visible: usize) -> Range<usize> {
    let visible = visible.max(1);
    let shown = total.min(visible);
    let selected = selected.min(total.saturating_sub(1));
    let start = selected.saturating_sub(visible - 1).min(total - shown);
    start..start + shown
}

/// The slash-command list above the input: the one table the terminal reads
/// too, narrowed to what the draft is spelling.
///
/// Only the visible window is built, eight rows at most, and it slides once
/// the selection walks past the last of them. Descriptions are clipped here
/// rather than in the table, because the same strings are printed in full by
/// the terminal's `/help`.
/// The emoji list under a `:query` (#2413): the glyph and its shortcode per row,
/// the highlighted row marked as the command list marks its own.
fn emoji_popup_view(items: &[(&'static str, &'static str)], selected: usize) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel));
    for (index, (glyph, code)) in items.iter().enumerate() {
        let on = index == selected;
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded(px(4.0))
                .text_color(rgb(palette::current().text))
                // A subtle fill with an accent edge: the focus colour under
                // muted text was unreadable in the themes (#2421).
                .when(on, |d| {
                    d.bg(rgb(palette::current().panel_hi))
                        .border_l_2()
                        .border_color(rgb(palette::current().accent))
                })
                .child(SharedString::from(glyph.to_string()))
                .child(SharedString::from(format!(":{code}:"))),
        );
    }
    col
}

fn command_popup_view(names: &[&'static str], selected: usize) -> Div {
    const VISIBLE: usize = 8;
    let window = popup_window(names.len(), selected, VISIBLE);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel));
    for (offset, name) in names[window.clone()].iter().enumerate() {
        let picked = window.start + offset == selected;
        let description = match slash::lookup(name) {
            Some(cmd) => slash::clipped(cmd.description, 52).to_string(),
            None => String::new(),
        };
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_1()
            .rounded(px(4.0));
        if picked {
            row = row.bg(rgb(palette::current().panel_hi));
        }
        let name_color = if picked {
            palette::current().amber
        } else {
            palette::current().text
        };
        col = col.child(
            row.child(
                div()
                    .w(px(112.))
                    .text_xs()
                    .text_color(rgb(name_color))
                    .child(SharedString::from(*name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette::current().text_muted))
                    .child(SharedString::from(description)),
            ),
        );
    }
    col.child(
        div()
            .text_xs()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(format!(
                "{}/{} · ↑↓ select · tab complete · enter run",
                selected + 1,
                names.len()
            ))),
    )
}

/// The running detached command, above the input.
///
/// Same formatter as the TUI (`tui::render::background`), so the two surfaces
/// cannot disagree about what a running task looks like. No border and no
/// percentage width: it sits in the column flow and disappears when idle.
fn task_strip(text: String) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(palette::current().amber))
        .child(SharedString::from(format!("⏳ {text}")))
}

/// The footer's identity line: who is answering, from where, on what branch,
/// and how fast this turn is streaming. Every field is read live; a field
/// with no value prints a dash instead of a guess.
fn status_identity(provider: &str, model: &str, cwd: &str, branch: &str, tps: f32) -> String {
    if tps > 0.0 {
        format!("{provider} · {model} · {cwd} · {branch} · {tps:.1} tok/s")
    } else {
        format!("{provider} · {model} · {cwd} · {branch}")
    }
}

/// A path with the home directory collapsed to `~`, the way a shell prompt
/// prints it. The home prefix is compared as a whole component, so
/// `/Users/al` never matches `/Users/adolfousierstudio`.
fn tilde_path(p: &std::path::Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return p.display().to_string();
    };
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// The checkout's branch name, or `None` when the directory is not a repo.
/// Run once at startup: this is a footer label, not a per-frame subprocess.
fn git_branch(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// The TUI's ctx counter, rebuilt for the footer: same numbers, same
/// grammar (`ctx: 12K/200K (6%)`) and the same colour thresholds, so the
/// desktop window cannot drift from the terminal (`tui::render::input`).
///
/// `None` (no answer yet) or an unknown window draws a dash, exactly like
/// the TUI's placeholder, instead of a percentage of nothing.
fn ctx_footer(used: Option<u32>, max: u32) -> (String, u32) {
    let Some(used) = used.filter(|_| max > 0) else {
        return ("ctx: -".to_string(), palette::current().text_faint);
    };
    let pct = (used as f64 / max as f64) * 100.0;
    let color = if pct > 80.0 {
        palette::current().red
    } else if pct > 60.0 {
        palette::current().amber
    } else {
        palette::current().teal
    };
    (
        format!(
            "ctx: {}/{} ({:.0}%)",
            format_token_count_raw(used as i64),
            format_token_count_raw(max as i64),
            pct
        ),
        color,
    )
}

/// The key reference, painted over everything else (#2320).
///
/// Rows come from `ui_core::help_keys`, the table the terminal's `/help`
/// prints, and only the rows this window actually answers are listed: `gui`
/// is `None` for a chord whose port has not landed yet, and a help screen
/// that advertises a dead chord is worse than one that stays quiet.
/// The rows the overlay paints, in display order: every row whose `gui`
/// spelling exists, section by section (#2320).
///
/// A function rather than a loop inside the renderer, so a test can compare
/// the painted list with the shared table without building a window. The
/// window answers a chord or it does not, and a row the window ignores is a
/// row it must not advertise.
fn help_rows() -> Vec<(
    opencrabs::ui_core::help_keys::HelpSection,
    &'static opencrabs::ui_core::help_keys::HelpRow,
)> {
    use opencrabs::ui_core::help_keys::{self, HelpSection};

    let mut out = Vec::new();
    for section in HelpSection::ALL {
        out.extend(
            help_keys::section_rows(section)
                .filter(|row| row.gui.is_some())
                .map(|row| (section, row)),
        );
    }
    out
}

/// The most characters the header prints before it shortens (#2480).
///
/// A name is a label, not the conversation: the rail and the window title keep
/// the whole string.
const HEADER_NAME_CHARS: usize = 42;

/// The name the header prints.
///
/// The shortening happens here, in Rust, and never leans on the text engine.
/// `truncate()` on a stretched column painted a bare `\u{2026}` on the owner's
/// screen while the stored name was `CrabsDev`: a full width band whose only
/// ink was three dots. Whatever this function returns is what gets painted.
fn header_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return rail::UNTITLED.to_string();
    }
    let mut chars = trimmed.chars();
    let short: String = chars.by_ref().take(HEADER_NAME_CHARS).collect();
    if chars.next().is_some() {
        format!("{short}\u{2026}")
    } else {
        short
    }
}

/// The conversation's name on the header bar (#2480).
///
/// Telegram's forum header puts the chat title on top and the topics under it.
/// The GUI chat screen had no header at all (the comment below used to say the
/// session name led the footer), so a row of topic chips was the first thing
/// the eye met and the session it belonged to was nowhere. Plain header text,
/// flush left, and the two-word rule does not apply: the name is the full
/// name, and only the chips are shortened.
///
/// The card that used to wrap it is retired (owner, 2026-11-04): "still
/// looking like a fucking useless button instead of taking full width ...
/// remove its button, leave the clean title in the bar". Three passes of
/// padding and border tuning never changed the read, because the shape itself
/// was the problem: a bordered box on a bar looks like a button no matter how
/// snug it is. What is left is the name, plain, across the whole row: no
/// border, no fill, no padding, nothing that can read as a control.
fn session_title(name: &str) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(palette::current().text_normal))
        .child(SharedString::from(header_name(name)))
}

/// The name prompt above the input, while a topic is being named (#2303).
/// The topic row (#2400, #2402): the leading button that moves it and the
/// tabs. Across the top or bottom it is one row; down a side it is one column.
///
/// The `...` and its items used to close this row (#2303). They are gone from
/// here (#2480): the menu belongs to the conversation, so it lives at the
/// chat's top right and floats over the transcript instead of growing this
/// strip by five rows when it opens.
fn topic_strip(
    edge: topic_tabs::Edge,
    toggle_edge: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    tabs: Vec<AnyElement>,
) -> Div {
    // The orientation control shares the topic cell's generous hit target.
    let control = topic_tabs::style(edge.is_vertical());
    let separator = topic_tabs::separator_color();
    let leading = div()
        .id("topic-edge-button")
        .flex()
        .items_center()
        .justify_center()
        .size(rems(control.control_rems))
        .flex_none()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(toggle_edge)
        .child(topic_icons::orientation_icon(
            control.control_icon_rems,
            palette::current().text_muted,
        ));
    if edge.is_vertical() {
        // A compact fixed rail gives every cell the same square geometry.
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(topic_tabs::CELL_REMS))
            .h_full()
            .items_center()
            .gap_2()
            .py_2()
            .when(edge == topic_tabs::Edge::Left, |d| d.border_r_1())
            .when(edge == topic_tabs::Edge::Right, |d| d.border_l_1())
            .border_color(separator)
            .child(leading)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .children(tabs),
            )
    } else {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .min_w_0()
            // Match the title hairline end to end without moving the tabs.
            .mx(rems(-1.5))
            .px_6()
            .border_b_1()
            .border_color(separator)
            .child(leading)
            // The tabs wrap onto a second line rather than being cut off, so a
            // narrow window never hides a topic (#2414).
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .children(tabs),
            )
    }
}

/// The most words a topic tab prints (#2477). A tab is a label, not the name:
/// Telegram's topic bar shows a short title and keeps the rest on the topic
/// itself, and a tab that grows with a long name pushes its neighbours off the
/// strip.
const TOPIC_TAB_WORDS: usize = 2;

/// The label a topic tab prints: at most [`TOPIC_TAB_WORDS`] words, with an
/// ellipsis when the name runs longer (#2477).
///
/// Only the tab is shortened. The stored name is never touched, so the
/// sidebar, the session list and the rename prompt still read the name that
/// was given, and shortening the tab cannot lose it.
fn topic_label(name: &str) -> String {
    let mut words = name.split_whitespace();
    let mut label = String::new();
    for word in words.by_ref().take(TOPIC_TAB_WORDS) {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(word);
    }
    if words.next().is_some() {
        label.push('\u{2026}');
    }
    label
}

/// One tab of the topic strip (#2400, #2480). The open session is marked the
/// way Telegram's topic bar marks it: the name takes the accent colour, with
/// an opaque rule on the cell's leading edge down a rail or bottom edge on a
/// horizontal bar.
///
/// Both marks reserve their space while inactive, so switching topics never
/// changes the strip's geometry. The cells themselves stay transparent.
fn topic_tab(
    id: Uuid,
    label: String,
    icon: Option<AnyElement>,
    shape: topic_bar::ChipShape,
    current: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_right: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let colour = if current {
        topic_tabs::TELEGRAM_ACCENT
    } else {
        palette::current().text_muted
    };
    let rule = topic_tabs::focus_color(topic_tabs::TELEGRAM_ACCENT, current);
    let vertical = shape == topic_bar::ChipShape::IconAbove;
    let style = topic_tabs::style(vertical);
    let content = div()
        .when(vertical, |d| {
            d.w_full().text_center().text_xs().line_clamp(2)
        })
        .child(SharedString::from(label));
    div()
        .id(SharedString::from(format!("topic-tab-{id}")))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(rgb(colour))
        .cursor_pointer()
        .when(vertical, |d| {
            d.size(rems(style.cell_rems))
                .flex_col()
                .gap_0()
                .p_1()
                .border_l(px(style.focus_bar_px))
                .border_color(rule)
        })
        .when(!vertical, |d| {
            d.flex_row()
                .gap_1()
                .px_2()
                .py_0p5()
                .border_b(px(style.focus_bar_px))
                .border_color(rule)
        })
        .hover(|d| d.text_color(rgb(topic_tabs::TELEGRAM_ACCENT)))
        .on_click(on_click)
        .on_mouse_down(MouseButton::Right, on_right)
        .children(icon)
        .child(content)
}

/// The update dialog (#2406). The first stage asks whether to update by
/// running /evolve; the second stage asks again before the download starts.
fn update_prompt_overlay(
    version: &str,
    second: bool,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (title, body, yes) = if second {
        (
            "Run /evolve now?",
            "The release is downloaded, then OpenCrabs needs a restart.",
            "run",
        )
    } else {
        (
            "Update to the new version?",
            "Running /evolve downloads the latest release.",
            "continue",
        )
    };
    let panel = div()
        .id("update-prompt-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_3()
        .w(rems(26.0))
        .p_4()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("v{version} is available"))),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(body)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(
                    div()
                        .id("update-prompt-cancel")
                        .px_3()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(palette::current().text_muted))
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(cancel)
                        .child(SharedString::from("cancel")),
                )
                .child(
                    div()
                        .id("update-prompt-yes")
                        .px_3()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(palette::current().accent))
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(confirm)
                        .child(SharedString::from(yes)),
                ),
        );
    div()
        .id("update-prompt-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(panel)
}

/// One pane's header: the session's name and the × that closes that pane
/// (#2474). Every pane carries one, so a split reads as two sessions rather
/// than a session beside a nameless strip, and the pane holding the caret
/// wears the accent so Tab's move is visible.
fn pane_header(
    id: SharedString,
    title: SharedString,
    focused: bool,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let title_color = if focused {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    // The focused pane wears the accent as a full-width band under its header,
    // so which pane Tab moved to is legible at a glance and not only from the
    // title colour (#2475). A one-pixel rule in the panel colour read as
    // nothing on both themes, which is why the band is two pixels and the
    // unfocused header sits on the same contrast as the divider.
    let rule_color = if focused {
        palette::current().accent
    } else {
        split_rule_color()
    };
    let header_bg = if focused {
        palette::current().panel_hi
    } else {
        palette::current().bg
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .bg(rgb(header_bg))
        .border_b_2()
        .border_color(rgb(rule_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(title_color))
                .child(title),
        )
        .child(
            div()
                .id(id)
                .px_2()
                .cursor_pointer()
                .text_color(rgb(palette::current().text_muted))
                .hover(|d| d.text_color(rgb(palette::current().accent)))
                .on_click(close)
                .child(SharedString::from("×")),
        )
}

/// The gap either side of the rule between two panes, in px: `gap_2()`.
const SPLIT_GAP_PX: f32 = 8.0;

/// The rule that separates two panes, in px (#2473). Two pixels rather than
/// one, and drawn in the faint text colour rather than a panel tone: at one
/// pixel of `panel_hi` the boundary disappeared into both themes, so a split
/// still read as one transcript with a gap in it.
const SPLIT_DIVIDER_PX: f32 = 2.0;

/// How wide the strip that takes the drag is, in px (#2476). The rule inside
/// it stays [`SPLIT_DIVIDER_PX`]; the strip either side of it is transparent
/// and takes the pointer, because a two pixel target is a pixel hunt.
const SPLIT_HANDLE_PX: f32 = 12.0;

/// The chord that opens a split (#2438, #2471): `ctrl-shift-pipe` puts the
/// panes side by side and `ctrl-shift-underscore` stacks them.
///
/// The window reads it through here rather than inline so the two shapes the
/// platforms send are handled in one place: the base key with shift held, and
/// the shifted glyph on its own, which is all macOS reports once it has folded
/// shift into the character. The rule for which shape means what lives in
/// [`split::chord_layout`].
fn split_chord(ks: &gpui::Keystroke) -> Option<split::Layout> {
    split::chord_layout(
        &ks.key,
        ks.key_char.as_deref(),
        ks.modifiers.control,
        ks.modifiers.shift,
    )
}

/// The colour of the rule between two panes (#2473). `text_faint` is the
/// quietest colour that still contrasts with `bg` on every theme, where
/// `panel_hi` sat within a few points of the background on the light ones.
fn split_rule_color() -> u32 {
    palette::current().text_faint
}

/// What one pane may plan its rows against (#2473, #2476). Side by side the
/// halves share the content width less the gap either side of the handle and
/// the handle itself, and `share` is the chat's part of that. Stacked, each
/// pane spans the full width and only the height is divided, so a row there is
/// planned against all of it.
fn split_pane_width(content: f32, stacked: bool, share: f32) -> f32 {
    if stacked {
        content
    } else {
        (content - SPLIT_GAP_PX * 2.0 - SPLIT_HANDLE_PX) * share
    }
}

/// The widest the conversation itself draws, in px (#2586). A line of body
/// text past this is a wall: the eye travels too far back to the left margin
/// to find the next one, so the transcript is centred inside a wide pane
/// instead of stretching edge to edge. Any pane under this width draws at
/// its own width, unchanged.
pub const CHAT_MEASURE_PX: f32 = 760.0;

/// What the conversation draws at, given the pane it sits in and whether the
/// window is split (#2586). A split window is exempt: every pane there takes
/// its full span, however narrow, because a half-width pane is already the
/// measure. The cap is for the single pane only, so it applies when `split`
/// is false and never in a half of a split.
pub fn chat_measure(pane_width: f32, split: bool) -> f32 {
    if split {
        pane_width
    } else {
        pane_width.min(CHAT_MEASURE_PX)
    }
}

/// The column a single pane draws its conversation in (#2586): the transcript
/// sits at the measure and the slack goes to the two margins, so the text is
/// centred in the window instead of stretching to a wall. Only a pane that is
/// wider than the measure is wrapped, so a narrow window keeps the exact tree
/// it had, and a split pane is never wrapped because it never takes the cap.
fn chat_measure_column(view: AnyElement, width: f32) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(width))
                .min_w_0()
                .min_h_0()
                .child(view),
        )
        .into_any_element()
}

/// The rule between two panes: vertical when they sit side by side, horizontal
/// when they stack, and draggable to resize both (#2473, #2476). The strip is
/// the target and the rule inside it is what is drawn, so the layout is
/// unchanged but the pointer has a real target to grab.
fn split_divider(
    stacked: bool,
    grab: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("split-divider")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(stacked, |d| d.h(px(SPLIT_HANDLE_PX)).w_full())
        .when(!stacked, |d| d.w(px(SPLIT_HANDLE_PX)).h_full())
        .cursor(if stacked {
            CursorStyle::ResizeUpDown
        } else {
            CursorStyle::ResizeLeftRight
        })
        .on_mouse_down(MouseButton::Left, grab)
        .child(
            div()
                .when(stacked, |d| d.h(px(SPLIT_DIVIDER_PX)).w_full())
                .when(!stacked, |d| d.w(px(SPLIT_DIVIDER_PX)).h_full())
                .bg(rgb(split_rule_color())),
        )
}

/// Everything the pane walk needs to draw one leaf or one split: the chat
/// element (drawn once, at its own leaf), the state each other pane keeps, the
/// caret, the tree's measured areas, and the window handle the per-pane
/// handlers report through (#2472).
struct PanesCtx<'a> {
    chat: Option<AnyElement>,
    chat_session: Option<Uuid>,
    chat_title: SharedString,
    states: &'a HashMap<Uuid, SplitPane>,
    focus: Option<Uuid>,
    content: f32,
    areas: &'a Rc<std::cell::RefCell<HashMap<Vec<usize>, [f32; 4]>>>,
    entity: gpui::WeakEntity<Dashboard>,
    /// Whether the dividers take the pointer. The themes preview draws the
    /// same panes but never resizes them (#2438).
    interactive: bool,
}

/// Draw the pane tree (#2472). A leaf is one pane: its header, its ×, and the
/// transcript it holds. A split lays its children along its own axis with a
/// divider between each pair, and every share comes from the tree rather than
/// from a single window-wide ratio, so a nested split divides only the pane it
/// grew from. A tree of one leaf draws the chat untouched, which is the
/// unsplit window the window has always had.
fn pane_element(
    tree: &PaneTree,
    ctx: &mut PanesCtx,
    path: &mut Vec<usize>,
    width_share: f32,
) -> AnyElement {
    match tree {
        PaneTree::Leaf { session, .. } => {
            let is_chat = Some(*session) == ctx.chat_session;
            let focused = if is_chat {
                ctx.focus.is_none()
            } else {
                ctx.focus == Some(*session)
            };
            let title = if is_chat {
                ctx.chat_title.clone()
            } else {
                match ctx.states.get(session) {
                    Some(pane) => SharedString::from(pane.title.clone()),
                    None => SharedString::from("(untitled)"),
                }
            };
            let body: AnyElement = if is_chat {
                ctx.chat.take().unwrap_or_else(|| div().into_any_element())
            } else {
                match ctx.states.get(session) {
                    Some(pane) => div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(transcript::view(
                            &pane.transcript,
                            &pane.scroll,
                            &LiveTurn::default(),
                            false,
                            if width_share < 1.0 {
                                split_pane_width(ctx.content, false, width_share)
                            } else {
                                ctx.content
                            },
                            HashMap::new(),
                            transcript::ViewHooks::default(),
                        ))
                        .into_any_element(),
                    None => div().into_any_element(),
                }
            };
            let weak = ctx.entity.clone();
            let close_session = *session;
            let close = move |_ev: &ClickEvent, _window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| this.close_pane(close_session, cx));
            };
            div()
                .id(SharedString::from(format!("pane-{session}")))
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(pane_header(
                    SharedString::from(format!("pane-close-{session}")),
                    title,
                    focused,
                    close,
                ))
                .child(body)
                .into_any_element()
        }
        PaneTree::Split {
            layout, children, ..
        } => {
            let stacked = layout.is_stacked();
            let here = path.clone();
            let areas = ctx.areas.clone();
            let key = path.clone();
            let mut row = div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .gap_2()
                .when(stacked, |d| d.flex_col())
                .when(!stacked, |d| d.flex_row())
                // The span a drag maps onto, read from the two outer children
                // instead of assumed from the viewport: the footer, the input
                // and the topic strip all sit inside the window, so a share
                // measured against the viewport would drift from the pointer
                // (#2476). Each split records its own box, so a nested rule
                // maps onto the pane it lives in rather than the window.
                .on_children_prepainted(move |bounds, _window, _cx| {
                    let (Some(first), Some(last)) = (bounds.first(), bounds.last()) else {
                        return;
                    };
                    let start_x = f32::from(first.origin.x);
                    let start_y = f32::from(first.origin.y);
                    let end_x = f32::from(last.origin.x + last.size.width);
                    let end_y = f32::from(last.origin.y + last.size.height);
                    areas
                        .borrow_mut()
                        .insert(key.clone(), [start_x, start_y, end_x, end_y]);
                });
            for (ix, child) in children.iter().enumerate() {
                path.push(ix);
                let child_width = if stacked {
                    width_share
                } else {
                    width_share * child.share()
                };
                let element = pane_element(child, ctx, path, child_width);
                path.pop();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("split-slot-{here:?}-{ix}")))
                        .flex()
                        .flex_col()
                        .flex_shrink()
                        .flex_basis(gpui::relative(child.share()))
                        .min_w_0()
                        .min_h_0()
                        .child(element),
                );
                if ix + 1 < children.len() {
                    // The rule starts the drag and the window root finishes
                    // it, so the pointer can slide off the 12px strip without
                    // the panes snapping back (#2476).
                    let weak = ctx.entity.clone();
                    let drag_path = here.clone();
                    let interactive = ctx.interactive;
                    let grab = move |ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                        if !interactive {
                            return;
                        }
                        let _ = weak.update(cx, |this, cx| {
                            this.begin_divider_drag(&drag_path, ix, ev, cx);
                        });
                    };
                    row = row.child(split_divider(stacked, grab));
                }
            }
            row.into_any_element()
        }
    }
}

/// The dim over the chat and the card in its bottom right while a split waits
/// for its second session (#2438). The scrim starts past the sidebar, so the
/// rows stay bright and clickable.
fn split_pick_overlay(layout: Layout, rail_px: gpui::Pixels) -> impl IntoElement {
    let card = div()
        .absolute()
        .bottom(px(24.0))
        .right(px(24.0))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(format!(
                    "Split {}: select a session to split with",
                    layout.label()
                ))),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_faint))
                .child("or press N for a new session · esc cancels"),
        );
    div()
        .id("split-pick-scrim")
        .occlude()
        .absolute()
        .top_0()
        .bottom_0()
        .right_0()
        .left(rail_px)
        .bg(rgba(0x0a0a0a88))
        .child(card)
}

/// The icon a topic gets when none is picked (#2414).
const DEFAULT_TOPIC_ICON: &str = "💬";

/// The name prompt for a topic (#2398, #2414): a centred card over a dark scrim.
/// The card holds the name field with the chosen icon before it, and a row of
/// icons to pick from. Enter creates, Esc or a click outside cancels.
fn topic_prompt_overlay(
    label: &str,
    draft: &str,
    icon: &str,
    picks: Vec<Stateful<Div>>,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let field = div()
        .flex_1()
        .min_w_0()
        .px_3()
        .py_2()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().accent))
        .bg(glass_fill(palette::current().bg))
        .text_color(rgb(palette::current().text))
        .child(SharedString::from(format!("{draft}|")));
    // The chosen icon, as a picture when it names an embedded one and as the
    // stored glyph when the row predates the picture set (#2478).
    let picture = topic_icons::topic_icon(icon)
        .or_else(|| topic_icons::crab_icon(icon))
        .map(|bytes| topic_icons::picture(bytes, 1.6).into_any_element());
    let icon_badge = div()
        .flex_none()
        .size(rems(2.4))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().bg))
        .text_size(rems(1.4))
        .when(picture.is_none(), |d| {
            d.child(SharedString::from(icon.to_string()))
        })
        .children(picture);
    let picker = div().flex().flex_row().flex_wrap().gap_1().children(picks);
    let panel = div()
        .id("topic-prompt-panel")
        .occlude()
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
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(icon_badge)
                .child(field),
        )
        .child(picker)
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_faint))
                .child("enter creates · esc cancels"),
        );
    div()
        .id("topic-prompt-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .on_click(cancel)
        .child(panel)
}

/// The topic info panel (#2304): read only, over the window. A click on the
/// scrim or Esc closes it. It holds no editable field.
fn topic_info_overlay(
    info: &sessions::TopicInfo,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let created = chrono::DateTime::<chrono::Utc>::from_timestamp(info.created_at, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let parent = info
        .parent_title
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let line = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_row()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .w(rems(8.0))
                    .text_color(rgb(palette::current().text_muted))
                    .child(label),
            )
            .child(
                div()
                    .text_color(rgb(palette::current().text))
                    .child(SharedString::from(value)),
            )
    };
    let panel = div()
        .id("topic-info-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_2()
        .w(rems(30.0))
        .p_4()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child("topic info"),
        )
        .child(line("id", info.id.to_string()))
        .child(line("parent", parent))
        .child(line("created", created))
        .child(line("messages", info.messages.to_string()));
    div()
        .id("topic-info-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .on_click(close)
        .child(panel)
}

/// What the Help Center shows: the running version, a newer release if the
/// check found one, the `/` filter, and the scroll position the arrows move.
struct HelpView {
    version: &'static str,
    update: Option<String>,
    filter: Option<String>,
    scroll: ScrollHandle,
}

/// The Help Center (#2407): a full-window screen. The header carries the logo,
/// the title and the version; the body lists every key and scrolls; the footer
/// names the keys that drive the screen. Esc closes it.
fn help_overlay(view: &HelpView) -> impl IntoElement {
    let query = view.filter.as_deref().unwrap_or("").to_lowercase();
    let mut body = div().flex().flex_col().gap_2().px_4().py_2();
    let mut printed: Option<opencrabs::ui_core::help_keys::HelpSection> = None;
    for (section, row) in help_rows() {
        let chord = row.gui.unwrap_or_default();
        let matches = query.is_empty()
            || row.description.to_lowercase().contains(&query)
            || chord.to_lowercase().contains(&query);
        if !matches {
            continue;
        }
        if printed != Some(section) {
            body = body.child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(rgb(palette::current().accent))
                    .child(SharedString::from(section.title())),
            );
            printed = Some(section);
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(
                    div()
                        .w(rems(9.0))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(palette::current().text_normal))
                        .child(SharedString::from(chord)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(row.description)),
                ),
        );
    }

    let mut title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_base()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from("OpenCrabs Help Center")),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("v{}", view.version))),
        );
    if let Some(latest) = &view.update {
        title = title.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().accent))
                .child(SharedString::from(format!("v{latest} available"))),
        );
    }
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(rgb(palette::current().panel_hi))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().text_lg().child(SharedString::from("🦀")))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from("esc")),
        );

    let scroller = div()
        .id("help-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&view.scroll)
        .child(body);

    let keys = div()
        .flex()
        .flex_row()
        .gap_3()
        .text_xs()
        .text_color(rgb(palette::current().text_muted))
        .child(SharedString::from("↑↓ scroll"))
        .child(SharedString::from("/ search"))
        .child(SharedString::from("esc back"));
    let mut footer = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(rgb(palette::current().panel_hi))
        .child(keys);
    if let Some(filter) = &view.filter {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().accent))
                .child(SharedString::from(format!("/ {filter}"))),
        );
    }

    div()
        .id("help-screen")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(palette::current().bg))
        .text_color(rgb(palette::current().text))
        .child(header)
        .child(scroller)
        .child(footer)
}

/// The delete confirmation: a dim card over both panes naming the
/// conversation, with the erase on its own button and nothing else. Every
/// delete path routes here, so the conversation is never one mis-click or one
/// mis-swipe away from gone.
fn confirm_delete_overlay(
    title: String,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("confirm-delete-scrim")
        // Modal: the dim scrim takes the pointer so a click meant for the
        // rail behind it cannot open or delete anything else.
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
        .child(
            div()
                .id("confirm-delete")
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(24.0))
                .p_4()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(glass_fill(palette::current().panel))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from("Delete this conversation?")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(
                            "Its messages leave the ledger. This cannot be undone.",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("confirm-delete-cancel")
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
                                .id("confirm-delete-yes")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().red))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(palette::current().red))
                                .hover(|d| d.bg(rgb(palette::current().selection)))
                                .child(SharedString::from("delete"))
                                .on_click(confirm),
                        ),
                ),
        )
}

/// The /cd picker's rows (#2527): the parent entry first when one exists,
/// then the subdirectories, dotfile directories hidden until toggled, all
/// sorted by name. A free function so the tests can exercise the real
/// listing without a live window, the same shape the terminal's dialog
/// builds in `open_directory_picker`.
fn cd_entries_for(dir: &std::path::Path, show_hidden: bool) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if dir.parent().is_some() {
        files.push(dir.join(".."));
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false);
            if hidden && !show_hidden {
                continue;
            }
            files.push(path);
        }
    }
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    files
}

#[cfg(test)]
mod tests {
    use super::{
        CHAT_MEASURE_PX, THEMES_HISTORY_LEN, cd_entries_for, pick_initial_session,
        remember_open_session, remember_preview,
    };

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
        let src = include_str!("main.rs");
        let (_, rest) = src
            .split_once("fn pane_element(")
            .expect("split leaf renderer exists");
        let (leaf, _) = rest
            .split_once("let close_session = *session;")
            .expect("pane leaf body ends before its close handler");
        assert!(
            !leaf.contains("chat_measure") && !leaf.contains("CHAT_MEASURE_PX"),
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
            src.contains("let measure = chat_measure(primary_width, split_window);"),
            "the measure comes from the pane width, exempting a split window"
        );
    }

    /// The primary Chat screen must share its mouse selection state with the
    /// transcript. The split view already did this; Chat silently passed no
    /// state and rendered no selectable rows (#2562).
    #[test]
    fn primary_chat_transcript_enables_drag_copy() {
        let src = include_str!("main.rs");
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
        assert_eq!(super::mc_schedule_duration(0), "0s");
        assert_eq!(super::mc_schedule_duration(42), "42s");
        assert_eq!(super::mc_schedule_duration(61), "1m 1s");
        assert_eq!(super::mc_schedule_duration(3661), "61m 1s");
    }

    #[test]
    fn mission_top_tool_bar_tracks_relative_usage() {
        assert_eq!(super::mc_tool_bar(0, 100), "░".repeat(12));
        assert_eq!(
            super::mc_tool_bar(50, 100),
            format!("{}{}", "█".repeat(6), "░".repeat(6))
        );
        assert_eq!(super::mc_tool_bar(100, 100), "█".repeat(12));
        assert_eq!(super::mc_tool_bar(200, 100), "█".repeat(12));
    }

    #[test]
    fn mission_mc_phantoms_empty_breakdown_has_no_rows() {
        let mut analytics = McAnalytics::default();
        analytics.phantom.total = 1;
        assert!(
            super::mc_phantoms_rows(&analytics, palette::current()).is_empty(),
            "an empty model breakdown must reach the panel's empty state"
        );

        analytics.phantom.by_model.push(("gpt-5".into(), 1, 0));
        assert_eq!(
            super::mc_phantoms_rows(&analytics, palette::current()).len(),
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
        let src = include_str!("main.rs");
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
        let main = include_str!("main.rs");
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
        let help = main.find(&["root = root.child(help_", "overlay("].concat());
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

    /// The themes sidebar keeps a short trail of what was previewed: newest
    /// last, no duplicates, and never longer than the strip is meant to be.
    /// Re-previewing an earlier theme moves it forward instead of listing it
    /// twice, which is what makes the trail walkable.
    #[test]
    fn themes_preview_history_is_newest_last_unique_and_capped() {
        let mut history: Vec<String> = Vec::new();
        remember_preview(&mut history, "dark");
        remember_preview(&mut history, "light");
        assert_eq!(history, vec!["dark".to_string(), "light".to_string()]);

        // The same theme again moves to the end, no second row.
        remember_preview(&mut history, "dark");
        assert_eq!(history, vec!["light".to_string(), "dark".to_string()]);

        for name in ["a", "b", "c", "d", "e", "f"] {
            remember_preview(&mut history, name);
        }
        assert_eq!(history.len(), THEMES_HISTORY_LEN);
        assert_eq!(history.last().map(String::as_str), Some("f"));
        assert!(!history.iter().any(|seen| seen == "dark"));
    }

    /// #2543: the rail keeps a four-pixel drag target but paints only the
    /// same one-pixel theme hairline used below the chat title.
    #[test]
    fn rail_splitter_paints_a_hairline_inside_its_drag_target() {
        let src = include_str!("main.rs");
        let splitter = src
            .split("fn splitter(")
            .nth(1)
            .and_then(|rest| rest.split("/// The conversation").next())
            .expect("rail splitter renderer");
        assert!(splitter.contains(".w(px(4.0))"));
        assert!(splitter.contains(".w(px(1.0))"));
        assert!(splitter.contains("topic_tabs::separator_color()"));
        assert!(splitter.contains(".on_mouse_down(MouseButton::Left, grab)"));
        assert!(!splitter.contains(".bg(rgb(palette::current().panel_hi))"));
    }

    /// The themes screen paints the conversation next to a sidebar, so a
    /// preview is judged against real content and no row is reachable only by
    /// mouse: arrows and Enter run through the same handler as a click.
    #[test]
    fn themes_screen_draws_a_sidebar_beside_the_conversation() {
        let src = include_str!("main.rs");
        assert!(src.contains(".id(\"themes-sidebar\")"));
        assert!(src.contains("themes_view::row(name, selected, index == current)"));
        assert!(src.contains("themes_view::history_row"));
        assert!(src.contains("\"enter\" => {"));
        assert!(src.contains(".id(\"themes-keep\")"));
        assert!(src.contains(".child(text(\"Themes\", Size::Sm, shown.text))"));
        assert!(src.contains(".rounded(px(CARD_RADIUS))"));
        assert!(src.contains(".child(\"Keep\")"));
        let header = src
            .split("let header = div()")
            .find(|block| block.contains("text(\"Themes\""))
            .expect("themes header");
        assert!(header.contains(".border_b_1()"));
        assert!(header.contains(".border_color(rgb(shown.track))"));
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
        let src = include_str!("main.rs");
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
            !between.contains("\n    fn "),
            "the bridge must be reached from delete_session itself, not a neighbour"
        );

        // Two arming paths: the row's slide-out strip, and ctrl+d on the
        // cursor row (#2352). Both ask the dialog; arming is not deleting.
        // The needles are split so the scan cannot count its own source text.
        let strip_arm = ["this.request", "_delete(id, cx);"].concat();
        let chord_arm = ["self.request", "_delete(target, cx);"].concat();
        assert!(
            src.contains(&strip_arm),
            "the row strip must arm the dialog"
        );
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
        // the row strip or the turn it would otherwise abort.
        assert!(
            src.contains("if self.confirm_delete.take().is_some()"),
            "Esc must cancel the dialog first"
        );
    }

    use super::*;

    /// ctrl+d carries a delete, so the chord must never BE the delete: it
    /// resolves one row, then asks the same dialog the row's strip asks
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
        let src = include_str!("main.rs");
        let chord = ["\"d\" if ev.keystroke.modifiers", ".control"].concat();
        assert!(src.contains(&chord), "ctrl+d must be bound on the rail");
    }

    #[test]
    fn the_ctx_counter_speaks_the_tui_grammar() {
        assert_eq!(ctx_footer(Some(12_000), 200_000).0, "ctx: 12K/200K (6%)");
        assert_eq!(
            ctx_footer(Some(140_000), 1_000_000).0,
            "ctx: 140K/1.0M (14%)"
        );
    }

    #[test]
    fn the_ctx_counter_colours_at_the_tui_thresholds() {
        // Same boundaries as `tui::render::input`: >80 red, >60 amber, else
        // teal. Below 60 stays calm so a healthy turn does not shout.
        assert_eq!(ctx_footer(Some(10_000), 200_000).1, palette::current().teal);
        assert_eq!(
            ctx_footer(Some(130_000), 200_000).1,
            palette::current().amber
        );
        assert_eq!(ctx_footer(Some(170_000), 200_000).1, palette::current().red);
    }

    #[test]
    fn a_budget_with_no_sample_stays_a_dash() {
        // Nothing measured, or no known window: never "0%" and never a
        // counter for a conversation that has not run a turn yet.
        assert_eq!(
            ctx_footer(None, 200_000),
            ("ctx: -".to_string(), palette::current().text_faint)
        );
        assert_eq!(
            ctx_footer(Some(900), 0),
            ("ctx: -".to_string(), palette::current().text_faint)
        );
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
    fn help_overlay_paints_one_row_per_entry_in_the_shared_table() {
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
        // GLOBAL's "List sessions" row is that command.
        let wanted = [
            ("up/down", "Move the highlight in the list"),
            ("enter", "Open the highlighted session"),
            ("ctrl+f", "Find a session"),
            ("ctrl+shift+p", "Pin the selected session"),
            ("ctrl+d", "Delete the selected session"),
            ("ctrl+g", "Session files"),
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
                    _ => chord,
                }),
                "{chord} must carry the window's spelling"
            );
        }
        // Rows with a window chord are the rail's commands, and those are what
        // must match the list above. Terminal-only rows (the topic keys in
        // /sessions) carry no window chord and are checked by their own tests.
        let rail: Vec<_> = sessions.iter().filter(|row| row.gui.is_some()).collect();
        assert_eq!(rail.len(), wanted.len(), "SESSIONS lists nothing else");
    }

    #[test]
    fn help_overlay_opens_on_ctrl_h_and_cmd_question() {
        // A source scan, because the handler needs a window and a key event
        // to exercise. The rule here is wiring: both doors land on the same
        // toggle, Esc closes the panel before it reaches anything behind it,
        // and the panel reads the shared table instead of its own copy of the
        // chords.
        // The two doors (ctrl+h and Cmd+?) are pinned where the chords
        // live, in `screen_keys`; this pins the wiring from that table to
        // the panel, the half that lives in this file (#2356).
        let src = include_str!("main.rs");
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
        assert!(
            src.contains("for (section, row) in help_rows()"),
            "the renderer must paint the list the test checks"
        );
    }

    #[test]
    fn delete_dialog_answers_enter_and_escape() {
        // The confirmation is a modal with two answers, so the keyboard gets
        // both (#2456). It sits before the Esc ladder, because a dialog that
        // owns the keyboard has to answer every key itself, and `delete` runs
        // from the same branch the button's click runs.
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let hook = src
            .find("self.finder_key(ks, cx)")
            .expect("the card must answer keys in on_key");
        let gate = src
            .find("if self.editor.read(cx).focus_handle.is_focused(window)")
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
    fn tui_chords_bind_the_window_like_the_terminal() {
        // The three commands the window spelled differently: new session,
        // sessions, fold newest turn. The terminal's chord is the one chord,
        // because the window has to answer on every platform and the command
        // key only exists on a Mac (#2319). ctrl+l stays as the alias the
        // session list has always answered.
        let src = include_str!("main.rs");
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
    fn chat_footer_points_at_the_key_reference() {
        // The footer stopped being a hand-maintained keymap (#2321). It carries
        // live state (provider, model, directory, branch, tokens per second,
        // ctx budget) and exactly one hint: the door to the key reference. A
        // strip of chords taught keys the window no longer binds and went
        // stale the moment a binding moved, so every screen answers with the
        // same three words instead.
        let src = include_str!("main.rs");
        let door = ["\"ctrl+h · ", "help\""].concat();
        assert_eq!(
            src.matches(&door).count(),
            3,
            "chat, usage and files each carry the one door, and nothing else"
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
        // The identity line survives: it is live state, which the strip was not.
        assert!(src.contains("fn status_identity"));
        assert!(src.contains("fn ctx_footer"));
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
        let src = include_str!("agent_bridge.rs");
        let needle = ["register_local", "_route("].concat();
        let sites = src.match_indices(needle.as_str()).count();
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
        let src = include_str!("main.rs");
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
            .split_once("\n    fn ")
            .expect("the next method ends it");
        assert!(
            door.contains(&delegate),
            "open_session routes through begin_conversation"
        );
        let (_, helper) = src
            .split_once(["fn begin_", "conversation(&mut self"].concat().as_str())
            .expect("begin_conversation exists");
        let (recording, _) = helper
            .split_once("\n    fn ")
            .expect("the next method ends it");
        assert!(
            recording.contains(&call),
            "begin_conversation records the conversation it opens"
        );
    }

    /// The completion event is handled, and the cross-session branch says so
    /// out loud instead of dropping the receipt. Halves again: the file this
    /// reads includes this test.
    #[test]
    fn the_completion_event_is_handled() {
        let src = include_str!("main.rs");
        let event = ["TurnEvent::", "BackgroundTaskDone"].concat();
        let warn = ["logging::", "warn("].concat();
        assert!(
            src.contains(&event),
            "the poll loop must handle the completion event"
        );
        assert!(
            src.contains(&warn),
            "a completion for another session is logged, never dropped"
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
        let src = include_str!("main.rs");
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
        let (body, _) = after.split_once("\n    fn ").unwrap_or((after, ""));
        let switch = body
            .find("self.screen = Screen::Chat;")
            .expect("select switches to chat");
        let spawn = body.find("cx.spawn").expect("select applies in a task");
        assert!(
            switch < spawn,
            "the conversation returns before the apply task spawns"
        );
    }
}

#[cfg(test)]
mod gui {
    //! The window's own tests, under one module name so a selection can be
    //! made with `cargo test -- gui::tests`: the panel and the popup are the
    //! two places the GUI has behaviour of its own that no headless test can
    //! reach through a real window.

    mod tests {
        use super::super::{Config, popup_window};

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
            let src = include_str!("main.rs");
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
    }
}
