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
//! resumes the session that started the task, whether or not its conversation
//! is the one on screen (#2704).

mod accent;
mod agent_bridge;
mod attachment_view;
mod bang;
mod breakdown;
mod command_palette;
mod commands;
mod editor;
mod fonts;
mod icon;
mod layout_store;
mod logging;
mod markdown;
mod menu;
mod mission_control;
mod model_fetch;
mod model_picker;
mod models;
mod onboard;
mod palette;
mod plan_strip;
mod platform_icons;
mod preview;
mod rail_activity;
mod screen_keys;
mod sessions;
mod skills_view;
mod telegram_topics;
mod text_commands;
mod theme;
mod themes_view;
mod thumbs;
mod topic_icons;
mod topic_tabs;
mod transcript;
mod usage_view;

use crate::screen_keys::{
    ChordMods, FocusTarget, KeyboardOwner, OverlayId, ScreenAction, focus_target, keyboard_owner,
    screen_chord,
};
use crate::theme::{BASE_REM, Size, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP, text};
use opencrabs::db::models::Message;
use opencrabs::ui_core::actions;
use opencrabs::ui_core::channel_status;
use opencrabs::ui_core::composer;
use opencrabs::ui_core::message_select::{MessageAction, Selection};
use opencrabs::ui_core::model_layout;
use opencrabs::ui_core::model_selector::{FetchRequest, ProviderSelectorState};
use opencrabs::ui_core::pane_tree::{DropHalf, PaneTree, drop_half};
use opencrabs::ui_core::rail;
use opencrabs::ui_core::scroll::{Follow, scrolled_from_top, wheel_releases_follow};
use opencrabs::ui_core::split::{self, Layout};
use opencrabs::ui_core::topic_bar;
use transcript::{CopyDrag, TranscriptState};
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
    DragMoveEvent, Entity, ExternalPaths, FocusHandle, Image, KeyDownEvent, KeystrokeEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
    ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Stateful, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, relative, rems, rgb, rgba, size,
};
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::brain::tools::suggest_options::{SuggestionItem, SuggestionStyle};
use opencrabs::utils::drop_landing::human_size;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use uuid::Uuid;

use opencrabs::brain::mission_control::{McActivity, McAnalytics, McInboxItem, McScheduleItem};
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

/// One pane beside the window's own chat: its transcript and scroll (#2438).
/// Where it sits, how much space it holds and which session it shows all live
/// in the pane tree, so a pane carries neither a layout nor a session of its
/// own (#2472).
struct SplitPane {
    /// The session's title as the rail shows it, for the pane header.
    title: String,
    transcript: TranscriptState,
    scroll: ScrollHandle,
    /// This pane's own bottom-follow state (#2685), per leaf like every other
    /// piece of pane chrome.
    follow: Follow,
    /// The pane's own composer (#2530). What is typed here stays with the pane
    /// it was typed into, so moving focus never carries a draft across.
    editor: Entity<editor::Editor>,
    /// The pane's own plan strip, read from its session's plan file and cached
    /// by mtime like the window's.
    plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per pane rather than once per frame.
    plan_for: Option<Uuid>,
    /// The turn this pane is running (#2614). A leaf owns its own stream, so
    /// sending here never repaints the window's conversation.
    live: transcript::LiveTurn,
    /// Whether this pane's turn is in flight: its composer takes a queue
    /// instead of starting a second one, exactly as the window does.
    busy: bool,
    /// The token that cancels THIS pane's turn, so `/stop` in one leaf leaves
    /// the other running.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// This pane's own drag-to-copy state (#2623). A pane transcript is built
    /// with `ViewHooks::default()`, which leaves `drag: None`, so every pane
    /// had no anchor, no highlight and no release handler: the gesture was
    /// dead the moment a second pane existed. It cannot share the window's
    /// either, because the row text is stored by ordinal and two panes plus
    /// the window all count rows from zero.
    copy_drag: Rc<CopyDrag>,
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

/// A running turn parked under the conversation that owns it (#2616).
///
/// The window paints one conversation at a time, and every session may be
/// mid-turn on its own. Turn state therefore travels with the session:
/// switching conversations parks the card, its cancel handle, the reload
/// handshake and the last prompt size here, and coming back puts them on
/// screen again. An entry exists while a turn is in flight, and also while a
/// card has ended but the reload has not replaced it yet, so a profile with
/// thousands of conversations still costs nothing.
struct StashedTurn {
    live: transcript::LiveTurn,
    cancel: Option<tokio_util::sync::CancellationToken>,
    settle_expect: Option<usize>,
    settle_retries: u8,
    ctx_tokens: Option<u32>,
    /// Whether the turn was still running when the window parked it (#2811).
    ///
    /// Restored verbatim instead of assumed. A card the user stopped has
    /// nothing left to animate, so forcing `busy` back on would put a spinner
    /// over a turn nobody is running, which is the frozen look the report
    /// described. The rail and the queue ask this flag rather than the entry's
    /// presence, so a parked stop reads as idle in both.
    busy: bool,
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

/// The phase 1 window. Every string on screen is real data or the real error.
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
    /// The one monotonic origin every rail animation reads, so all rows blink
    /// on the same frame instead of one clock each.
    rail_clock: std::time::Instant,
    /// When the rail's live previews were last re-read, so a stream of chunks
    /// costs at most one query a second instead of one per token.
    preview_refreshed_at: Option<std::time::Instant>,
    /// Monotonic generation of the ctrl+f deep search (#2720). Each keystroke
    /// bumps it and spawns a debounced content query that carries the value;
    /// a result only lands when its generation still matches, so a slow query
    /// for "nee" can never paint over the hits for "needle".
    rail_find_gen: u64,
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
    /// Where that row's right press landed, in window coordinates (#2690).
    /// The card opens under the pointer instead of pinned to the row's top
    /// corner, so a long message cannot put it half a screen away.
    msg_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// Select mode (#2613). The picks live in the model, keyed by message id,
    /// so a scroll or a refresh never drops one; `is_active` is what the
    /// selection bar answers to.
    msg_select: Selection<Uuid>,
    /// A forward waiting for its destination (#2608): the rows picked when
    /// Forward was pressed. `Some` swaps the composer for the session picker.
    forward_pick: Option<Vec<Message>>,
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
    /// The Group topics picker: the parent row being grouped under, the
    /// candidate sessions (id, title, date), the multi-selection, the keyboard
    /// cursor, a guard sentence on refusal, and a busy flag while attaching.
    group_target: Option<Uuid>,
    group_candidates: Vec<(Uuid, String, String)>,
    group_selected: std::collections::HashSet<Uuid>,
    group_cursor: usize,
    group_error: Option<String>,
    group_busy: bool,
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
    topic_draft: Option<String>,
    /// The pick the window has to explain before it goes on (#2605): the
    /// seeker's topic pick named a topic, and a topic cannot hold topics, so
    /// this carries that row's title for the warning instead of opening a
    /// prompt that would nest one under another.
    topic_warning: Option<String>,
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
    /// Which accent control the pointer is dragging, if any.
    themes_drag: Option<accent::Drag>,
    /// The wheel's box in window coordinates, recorded at paint time so a
    /// press can be turned into a hue (#2619).
    themes_wheel: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// The saturation and brightness tracks' boxes, recorded the same way.
    themes_sat_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    themes_val_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// Whether the font list under the cards is open (#2786). Closed by
    /// default, because the list is the machine's whole family catalog and the
    /// modal is small.
    fonts_open: bool,
    /// Whether the installed families have been read. The enumeration walks
    /// CoreText on macOS and keeps no cache, so it runs once per process.
    fonts_loaded: bool,
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
    /// The selected session's transcript and its scroll handle, so a fresh
    /// load can open at the bottom where the conversation ends.
    transcript: transcript::TranscriptState,
    transcript_scroll: ScrollHandle,
    /// Whether the chat transcript keeps following its bottom (#2685). A one
    /// shot snap could not: it is consumed by the next layout, and every row
    /// that lands afterwards is out of view. It is released when the reader
    /// scrolls up and re-armed when they send, open or come back (#2698).
    transcript_follow: Follow,
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
    /// The message editor (task 4). Typing goes to it while it holds focus;
    /// its Sent events arrive through the subscription made in `main`.
    editor: Entity<editor::Editor>,
    /// The submitted history every composer in this window walks. One handle
    /// cloned into the window's field and every split pane, so a message sent
    /// in one is recalled in all of them, in real time, and the terminal reads
    /// the same list off the same file.
    history: composer::SharedHistory,
    /// Half-typed text keyed by the conversation it was typed in. Switching
    /// away and back returns the same draft instead of throwing it away.
    drafts: composer::DraftStore,
    /// Poll ticks counted since the last repaint that a live turn asked for.
    /// The spinner and the clock are the only things moving in that branch, and
    /// rebuilding every pane's transcript 20 times a second to advance them is
    /// what made a split screen crawl (#2688).
    anim_tick: u64,
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
    /// Turns running in conversations the window is not painting (#2616).
    /// Keyed by the session that owns each one, so a switch parks the turn on
    /// screen and restores the destination's, and the window never has to
    /// guess which conversation a card belongs to.
    stashed: std::collections::HashMap<Uuid, StashedTurn>,
    /// Reload attempts made while waiting for the turn's rows to persist.
    /// Bounded, so a write that never lands cannot spin the fetch loop.
    settle_retries: u8,
    /// When the current settle wait started. `None` between turns: the budget
    /// is measured from the first reload that found the rows short, not from
    /// the send, because the write only exists once the turn has ended.
    settle_deadline: Option<std::time::Instant>,
    /// Late re-reads already spent on this turn. Two is the whole allowance,
    /// so a write that truly never lands leaves a log line instead of a loop.
    settle_rechecks: u8,
    /// Rows the session must hold before the live turn is dropped: the
    /// session's row total when the send happened, plus the one the new turn
    /// appends. Measured on the ledger, not on the windowed transcript, so a
    /// conversation longer than `transcript::WINDOW` still settles (#2705).
    settle_expect: Option<usize>,
    /// A turn is running: sends queue instead of starting a second one.
    busy: bool,
    /// The running turn's cancellation token, so Esc twice stops it the way
    /// the TUI's second Esc does. `None` when no turn is in flight.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// The hint last painted on the composers, so a tick only touches them when
    /// the state actually changed (#2630).
    hint_painted: Option<SharedString>,
    /// Whether the queue strip is showing every message or only its head.
    /// A queue of five rows pushes the conversation off the screen, so past
    /// two messages the strip folds and a click opens it (#2629).
    queue_expanded: bool,
    /// Text typed before there is a conversation to key it to: while the agent
    /// stack is still booting, or before the first session exists. Everything
    /// else waits in `agent_bridge`'s per-session queue, the one the agent
    /// itself drains between tool calls. Keeping a second list here meant the
    /// strip could show five messages the agent could not see, so they never
    /// went out (#2629).
    unsent: Vec<agent_bridge::PendingMessage>,
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
    /// Metrics for the turns that just settled, waiting for their rows to land
    /// so the anchor they get keyed by exists. Keyed by session, one slot each:
    /// a reload for another conversation must not claim them, and a split can
    /// settle two panes before either reload comes back (#2632).
    pending_metrics: HashMap<Uuid, transcript::TurnMetrics>,
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
        item_body(
            src,
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        // the caret there would eat the next arrow.
        let src = include_str!("main.rs");
        let keys = fn_body(src, "on_key");
        assert!(
            keys.contains("open_session_handing_back(id, false, cx)"),
            "the arrows preview the row and keep the rail armed"
        );
    }

    #[test]
    fn a_created_session_and_a_new_split_leave_the_caret_in_the_composer() {
        // #2665: every one of these was clicked from the rail, so each has to
        // take the keys off it before asking for focus again.
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let render = dashboard_render(src);
        for (anchor, why) in [
            ("let ask_menu = cx.listener", "a row's right press"),
            ("let toggle_pin = cx.listener", "a row's pin"),
            ("let ask = cx.listener", "a topic chip's right press"),
        ] {
            let (_head, tail) = render
                .split_once(anchor)
                .unwrap_or_else(|| panic!("{why} still opens its card from here"));
            let listener = tail.split("});").next().unwrap_or("");
            assert!(
                listener.contains("this.rail_hands_back();"),
                "{why} hands the keys back before it opens the card"
            );
        }
        // A row press is not a pick yet, and the caret does not wait for the
        // release to find out what it was.
        let (_head, tail) = render
            .split_once("this.rail_slide_opened = None;")
            .unwrap_or_else(|| panic!("the row press still starts the slide"));
        let listener = tail.split("});").next().unwrap_or("");
        assert!(
            listener.contains("this.rail_hands_back();"),
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
        let src = include_str!("main.rs");
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
        let overlay = fn_body(src, "topic_prompt_overlay");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let overlay = fn_body(src, "topic_prompt_overlay");
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
    /// reaches the card. Scoped to the window render: this test module sits
    /// above it, so a bare split would land in the test's own strings.
    #[test]
    fn rail_card_resolves_the_stored_icon() {
        let src = include_str!("main.rs");
        let render = dashboard_render(src);
        let wiring = render
            .split("icon: self")
            .nth(1)
            .expect("rail icon wiring")
            .split("};")
            .next()
            .expect("wiring before the flags close");
        assert!(
            wiring.contains("topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)"),
            "an embedded mark renders at the full rail-card media size"
        );
        assert!(
            wiring.contains("self.custom_icon_picture(stored, sessions::RAIL_MARK_REMS)"),
            "a custom image renders from the view's cache at the same size"
        );
        assert!(
            wiring.contains("topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS)"),
            "a legacy glyph keeps its text slot at the same size"
        );
    }

    #[test]
    fn a_topic_switch_hands_the_caret_back() {
        // #2741: a click on a topic chip took the caret and never asked for it
        // back, on both of the paths that do not go through `open_session`:
        // the session already sits in another leaf, or the leaf is renamed.
        let src = include_str!("main.rs");
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
    fn a_committed_row_leaves_the_keys_in_the_chat_leaf() {
        // #2741: the chat leaf adopts the row's conversation, so a split that
        // kept the caret in a neighbouring pane typed into a conversation the
        // click did not open.
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
fn busy_repaint_due(tick: u64) -> bool {
    tick.is_multiple_of(BUSY_REPAINT_TICKS)
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
    /// The name of the conversation on screen, as every surface prints it
    /// (#2766). One parse lives behind this: the platform is the card's mark
    /// and the channel key is bookkeeping, so neither belongs in a header, a
    /// pane border or the footer identity.
    fn session_title(&self) -> Option<String> {
        self.transcript.session.and_then(|id| {
            self.sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref())
                .map(|stored| rail::chip_name(Some(stored), None))
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
            // The turn on screen belongs to the conversation being left. A
            // turn still running there is parked under it, and the
            // destination's own turn (or an empty slot) comes on screen, so
            // neither the spinner nor the ctx counter follows the switch
            // (#2616). Runs before `transcript.session` moves: the previous
            // id is the key the parked turn is stored under.
            swap_visible_turn(
                &mut self.stashed,
                self.transcript.session,
                id,
                &mut self.live,
                &mut self.busy,
                &mut self.cancel,
                &mut self.settle_expect,
                &mut self.settle_retries,
                &mut self.ctx_tokens,
            );
            // Every switch says who it left, who it arrived on and whether a
            // card was parked (#2811). The report that started this had only
            // screenshots to work from: a switch left no trace at all, so "the
            // state vanished" could not be told apart from "it was never
            // parked". One line makes the next repro greppable.
            logging::info(&format!(
                "session switch {:?} -> {id}: {}",
                self.transcript.session,
                if self
                    .transcript
                    .session
                    .is_some_and(|from| self.stashed.contains_key(&from))
                {
                    "card parked"
                } else {
                    "nothing parked"
                }
            ));
            // Metrics belong to the turn they settled, never to the session
            // the user just switched to.
            self.pending_metrics.clear();
            if let Some(b) = agent_bridge::bridge() {
                self.ctx_max = b.service.context_limit_for_session(id);
            }
            // A recall belongs to the conversation it started in too, so its
            // messages go back to wait instead of vanishing with the old draft
            // (#2637, #2696).
            let returned = self.editor.update(cx, |ed, _| ed.release_recall(false));
            if let Some((session, msgs)) = returned {
                for msg in msgs {
                    agent_bridge::queue_recall_return(session, msg);
                }
            }
            // The draft and its attachments belong to the conversation they
            // were typed in (#2481). The text is filed under the conversation
            // being left and the destination's own draft comes back, so a
            // switch and a switch back return the same half-typed message.
            let previous = self.transcript.session;
            let draft = self.editor.update(cx, |ed, cx| ed.take_draft(cx));
            if let Some(previous) = previous {
                self.drafts.store(previous, draft);
            }
            let returning = self.drafts.take(id);
            self.editor.update(cx, move |ed, cx| {
                ed.begin_conversation(cx);
                if let Some(draft) = returning {
                    ed.restore_state(draft, cx);
                }
            });
        }
        let previous = self.transcript.session;
        self.transcript.session = Some(id);
        // The chat is a leaf of the pane tree, so switching conversations
        // moves that leaf instead of leaving a stale pane behind (#2472).
        self.adopt_chat_leaf(previous, id, cx);
        // The next launch opens this conversation. Both surfaces read the same
        // file, so switching here means the terminal comes back to it too, and
        // a restart no longer lands on whatever the rail listed first (#2444).
        remember_open_session(id);
        agent_bridge::set_composer_session(Some(id));
        self.transcript.error = None;
        self.transcript.turns = Vec::new();
        self.transcript.rows = Vec::new();
        self.transcript.hidden = 0;
        // A menu or a pick belongs to the rows that were on screen: keeping
        // either across a conversation switch would act on another chat's
        // messages, so both are dropped here with the rows (#2613, #2608).
        self.msg_menu = None;
        self.msg_select.cancel();
        self.forward_pick = None;
        self.forward_query.clear();
        self.forward_cursor = None;
    }

    /// Keep the chat transcript on its newest row (#2685).
    ///
    /// The snap is judged from the reader's own place: a transcript pinned at
    /// the bottom keeps following a turn as it streams, and one the reader
    /// scrolled up stays where they put it. Content that grows moves the max
    /// offset and leaves the offset alone, so growth can never be mistaken for
    /// the reader leaving.
    fn follow_chat(&mut self) {
        let offset = scrolled_from_top(f32::from(self.transcript_scroll.offset().y));
        let max = f32::from(self.transcript_scroll.max_offset().height);
        if self.transcript_follow.update(offset, max) {
            self.transcript_scroll.scroll_to_bottom();
        }
    }

    /// Give the chat transcript back its follow and land on the newest row.
    ///
    /// Sending, opening a conversation and coming back from a panel all say
    /// the same thing: the reader wants the end of it (#2698).
    fn rearm_transcript_follow(&mut self) {
        self.transcript_follow.rearm();
        self.transcript_scroll.scroll_to_bottom();
    }

    /// Stop following because the reader moved by hand: a wheel tick up or a
    /// PageUp (#2698). The TUI drops `auto_scroll` on the same two gestures.
    fn release_transcript_follow(&mut self) {
        self.transcript_follow.release();
    }

    /// One Ctrl+C press (#2308), the terminal's three steps ported whole.
    ///
    /// Scrolled up, the press only gives the bottom back. Otherwise it throws
    /// the draft away and arms a three second window, and a second press
    /// inside that window stops the turn in flight and closes the window. The
    /// keystroke interceptor and `on_key` both land here, so the key means one
    /// thing wherever it is caught.
    fn ctrl_c_press(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let input_empty = self.editor.read(cx).is_empty();
        let action = ctrl_c_action(
            self.transcript_follow.is_following(),
            input_empty,
            self.ctrl_c_pending,
            now,
        );
        match action {
            CtrlCAction::SnapBottom => {
                self.ctrl_c_pending = None;
                self.rearm_transcript_follow();
                // The pane the keyboard is on follows the same rule as the
                // window: it is the transcript the reader was looking at.
                if let Some(id) = self.pane_focus {
                    self.rearm_pane_follow(id);
                }
                self.editor
                    .update(cx, |editor, cx| editor.discard_draft(cx));
                logging::info("ctrl+c snapped the transcript to the bottom");
            }
            CtrlCAction::ClearAndArm => {
                self.ctrl_c_pending = Some(now);
                self.editor
                    .update(cx, |editor, cx| editor.discard_draft(cx));
                self.live
                    .notices
                    .push("Press Ctrl+C again to quit".to_string());
            }
            CtrlCAction::Quit => {
                self.ctrl_c_pending = None;
                // The stop reaches the pane the keyboard is on first, then the
                // window's own turn, the same order Esc uses.
                self.cancel_focused_pane_turn("Ctrl+C", cx);
                if let Some(cancel) = self.cancel.take() {
                    cancel.cancel();
                }
                self.busy = false;
                self.live.thinking = false;
                self.cancel = None;
                logging::info("ctrl+c quit the window");
                cx.quit();
                return;
            }
        }
        // The draft is gone, so the hint that describes an empty field is the
        // one that belongs under it now (#2308).
        self.sync_composer_hint(cx);
        cx.notify();
    }

    /// The same pair for one pane's transcript (#2685, #2698).
    fn release_pane_follow(&mut self, session: Uuid) {
        if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.follow.release();
        }
    }

    fn rearm_pane_follow(&mut self, session: Uuid) {
        if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.follow.rearm();
            pane.scroll.scroll_to_bottom();
        }
    }

    /// The same for one pane's transcript (#2685).
    fn follow_pane(&mut self, session: Uuid) {
        if let Some(pane) = self.pane_states.get_mut(&session) {
            Self::snap_pane_if_following(pane);
        }
    }

    /// Snap one pane's transcript to its newest row only while that pane is
    /// still following: a reader who scrolled up keeps their place while rows
    /// land under them (#2685, #2698).
    fn snap_pane_if_following(pane: &mut SplitPane) {
        let offset = scrolled_from_top(f32::from(pane.scroll.offset().y));
        let max = f32::from(pane.scroll.max_offset().height);
        if pane.follow.update(offset, max) {
            pane.scroll.scroll_to_bottom();
        }
    }

    /// Switch one leaf to another conversation (#2686).
    ///
    /// A topic chip drawn inside a pane has to move that pane. The window door
    /// (`open_session`) moves the chat leaf instead, so a click in a second
    /// pane used to move a conversation the user was not reading and leave the
    /// pane they were looking at untouched. The chat leaf and a session that
    /// already sits on screen keep going through the window door, because that
    /// path already closes a duplicate pane and moves the chat's own leaf.
    fn open_topic_in_pane(&mut self, pane: Uuid, id: Uuid, cx: &mut Context<Self>) {
        if Some(pane) == self.transcript.session || Some(id) == self.transcript.session {
            self.open_session(id, cx);
            return;
        }
        if self.panes.as_ref().is_some_and(|tree| tree.contains(id)) {
            // Already on screen: hand it the keyboard instead of showing one
            // conversation in two leaves.
            self.pane_focus = Some(id);
            self.rail_focused = false;
            self.sync_composer_session();
            self.request_composer_caret(cx);
            cx.notify();
            return;
        }
        let renamed = self
            .panes
            .as_mut()
            .is_some_and(|tree| tree.rename_leaf(pane, id));
        if !renamed {
            return;
        }
        // The pane's transcript, live turn, plan strip and editor all belong
        // to the conversation that just left the leaf; its draft is filed
        // under that conversation first.
        self.stash_pane_draft(pane, cx);
        self.pane_states.remove(&pane);
        self.open_pane_state(id, cx);
        self.pane_focus = Some(id);
        self.rail_focused = false;
        self.sync_composer_session();
        self.request_composer_caret(cx);
        self.persist_layout();
        cx.notify();
    }

    /// Open one conversation: select it in the rail, switch the right pane to
    /// chat, and load its newest window off the UI thread. Every door (rail
    /// click, launch, ctrl+u return, create topic) goes through here, and the
    /// per-conversation reset lives one layer down in `begin_conversation`,
    /// so the screen and the session can never disagree.
    fn open_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.open_session_handing_back(id, true, cx);
    }

    /// The same open, with the keyboard decision made explicit (#2665).
    ///
    /// `hand_back` is true for every commit (rail click, Enter, the palette,
    /// the find field, a topic tab, a created topic): the row is chosen, so
    /// the next keystroke is a message and the composer takes the caret. It is
    /// false while the arrows walk the rail, because that walk opens the row
    /// it stands on as a preview, and moving focus to the composer would stop
    /// the next arrow from arriving.
    fn open_session_handing_back(&mut self, id: Uuid, hand_back: bool, cx: &mut Context<Self>) {
        self.begin_conversation(id, cx);
        // Opening a conversation is reading its plan: the rail's ring stops
        // here, not at paint, so a repaint can never be mistaken for a read and
        // a ring never clears itself just because the row scrolled past.
        if let Some(evidence) = self
            .rail_index
            .evidence(id, false, std::time::Instant::now())
        {
            self.rail_seen.mark(id, evidence.stamp);
        }
        self.screen = Screen::Chat;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                // Apply only if the user still sits on this session; a slow
                // fetch for an abandoned click paints nothing.
                if this.transcript.session == Some(id) {
                    match result {
                        Ok(loaded) => {
                            let (hidden, turns) = transcript::turns_with_origins(
                                &loaded.rows,
                                loaded.total,
                                &this.turn_metrics,
                                &loaded.origins,
                            );
                            this.transcript.hidden = hidden;
                            this.transcript.turns = turns;
                            this.transcript.rows = loaded.rows;
                        }
                        Err(e) => this.transcript.error = Some(e.into()),
                    }
                    // Opening a conversation is a fresh start: follow its end
                    // even if the reader was parked in the last one's history
                    // (#2698).
                    this.rearm_transcript_follow();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        // A committed row takes the keyboard off the rail: the rail holds it
        // only while the list is being walked (#2665). The chat leaf is the
        // pane that adopted the conversation, so the caret goes there; a stored
        // split pane would otherwise keep it in a conversation the click did
        // not open (#2741).
        if hand_back {
            self.rail_focused = false;
            self.pane_focus = None;
            self.request_composer_caret(cx);
        }
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
        // The theme modal owns the keyboard on every screen, including the
        // non-chat ones: it is the topmost surface while it is open, so
        // nothing behind it may answer a key (#2619).
        if self.themes_open {
            return vec![OverlayId::Themes];
        }
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
        if self.sessions.rail_menu.is_some() {
            open.push(OverlayId::RailMenu);
        }
        if self.confirm_delete.is_some() {
            open.push(OverlayId::ConfirmDelete);
        }
        if self.group_target.is_some() {
            open.push(OverlayId::GroupPick);
        }
        if self.topic_warning.is_some() {
            open.push(OverlayId::Warning);
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

    /// A pointer gesture on the sidebar hands the keys back to the composer.
    ///
    /// The rail used to take the keyboard from the press's own x position
    /// (`x <= rail_width`), so every press in the column armed it and only
    /// Esc, Tab or a committed row gave the keys back. Every affordance the
    /// pointer touched without committing a pick cost an Esc before the
    /// composer would take a character: the empty space under the rows, the
    /// scrollbar, the brand header, a filter chip. A press is not
    /// a walk (#2798): the rail keeps the keys only while a key put it there,
    /// and a split still waiting for its pane keeps them because its dialog
    /// answers N and Esc.
    fn rail_hands_back(&mut self) {
        if !self.rail_focused || self.split_pick.is_some() {
            return;
        }
        self.rail_focused = false;
        self.refocus_screen();
    }

    /// Ask the composer that is drawn to take the caret, and let the field
    /// re-assert it from its own render (#2741).
    ///
    /// The press that led here is dispatched before the next frame is drawn,
    /// and it can still hand the window root the keys: any element without
    /// `track_focus` lets it, and the owner may not even change, so the
    /// reconciler in `render` never fires. The deferred request is the one
    /// that sticks, because the editor claims the caret while the tree is
    /// being built, after that press is done. `refocus_screen` stays the one
    /// decision point, so the two can never disagree about who owns the keys.
    fn request_composer_caret(&mut self, cx: &mut Context<Self>) {
        let composer = self.composer_entity();
        composer.update(cx, |editor, cx| {
            editor.focus_pending = true;
            cx.notify();
        });
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
        // The window-only rows: grouping sessions into topics is a window
        // modal, so that row prints here and nowhere else (#2713). The split
        // rows live in the shared catalog because both surfaces own panes.
        items.extend(actions::gui_items());
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
        // Deep search (#2721): when the query names message content, the
        // conversation row carries the matching snippet in place of the date
        // and the query as a keyword, so a session whose title never mentions
        // the word still surfaces. With no content hit the row is unchanged.
        let seeker_query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        let snippets: HashMap<Uuid, String> = self
            .seeker_find
            .get(&seeker_query)
            .map(|rows| {
                rows.iter()
                    .map(|row| (row.session_id, row.snippet.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for session in &self.sessions.items {
            let title = session
                .title
                .clone()
                .unwrap_or_else(|| "Untitled conversation".to_string());
            let item = match snippets.get(&session.id) {
                Some(snippet) if !seeker_query.is_empty() => PaletteItem::new(
                    title,
                    snippet.clone(),
                    PaletteKind::Session,
                    PaletteAction::OpenSession(session.id.as_u128()),
                )
                .with_keywords(&seeker_query),
                _ => PaletteItem::new(
                    title,
                    session.updated_at.format("%Y-%m-%d %H:%M").to_string(),
                    PaletteKind::Session,
                    PaletteAction::OpenSession(session.id.as_u128()),
                ),
            };
            items.push(item);
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
        self.seeker_find_if_changed(cx);
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
        self.seeker_find_if_changed(cx);
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
            // A Seeker row is a pick: while a split waits it fills the pane,
            // the way a rail row does (#2760).
            PaletteAction::OpenSession(id) => self.pick_or_open(Uuid::from_u128(id), cx),
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
                // The picker lists every conversation, topics included, so
                // the pick means one of three things (#2604, #2605): a topic
                // cannot hold topics, a conversation with them on opens the
                // prompt, and one that never had them needs the flag written
                // before the same prompt can open.
                let id = Uuid::from_u128(id);
                match topic_pick(
                    self.sessions.parents.contains_key(&id),
                    self.sessions.is_topic_enabled(id),
                ) {
                    TopicPick::Warn => {
                        let title = self
                            .sessions
                            .items
                            .iter()
                            .find(|s| s.id == id)
                            .and_then(|s| s.title.clone())
                            .unwrap_or_else(|| "That row".to_string());
                        self.topic_warning = Some(title);
                        cx.notify();
                    }
                    TopicPick::Draft => {
                        // The exact path the "New topic" menu item walks
                        // (#2482).
                        self.open_session(id, cx);
                        self.start_topic_draft(cx);
                    }
                    TopicPick::EnableThenDraft => {
                        self.open_session(id, cx);
                        self.enable_topics_then(id, |this, cx| this.start_topic_draft(cx), cx);
                    }
                }
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
            PaletteAction::GroupTopics => {
                // Same card, same empty field, different purpose: the pick
                // names the conversation that holds the topics (#2713).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::GroupTopics,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::GroupTopicsOn(id) => {
                // The exact path the rail row's right-click entry walks
                // (#2713).
                self.start_group_topics(Uuid::from_u128(id), cx);
            }
            PaletteAction::BeginSplit(layout) => {
                // The same door the chord and the rail row menu walk, from the
                // pane holding the caret (#2712).
                self.begin_split(layout, cx);
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
            find::Surface::Models => self.open_models(None, self.focused_pane(), cx),
            find::Surface::Sessions => self.toggle_rail(cx),
            find::Surface::Help => self.toggle_help(cx),
            find::Surface::Skills => self.open_skills(cx),
            // `/profiles` is a text command now (#2469): the palette row takes
            // the same door as typing it, never the model.
            find::Surface::Profiles => {
                if let Some(outcome) = commands::parse_command("/profiles") {
                    self.run_command(outcome, self.focused_pane(), cx);
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
    fn run_command(
        &mut self,
        outcome: commands::Outcome,
        session: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
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
                // The toggle closes it, so ask first: `/theme` on the open
                // modal must not reopen it behind itself.
                if !self.themes_open {
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
            commands::GuiAction::OpenModels => self.open_models(arg, session, cx),
            commands::GuiAction::OpenSkills => self.open_skills(cx),
            commands::GuiAction::OpenOnboard(name) => self.open_onboard(name, arg, cx),
            commands::GuiAction::Text(command) => self.run_text_command(command, arg, session, cx),
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
        // Every door splits the pane holding the caret (#2625), and the caret
        // is resolved the way the render resolves it rather than read
        // literally: a cached `pane_focus` naming a leaf the tree no longer
        // holds made all three doors refuse with "Open a session before
        // splitting it." while the window was plainly drawing a live focused
        // pane (#2757).
        let Some(target) = self.split_origin() else {
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
        // The picker's cursor starts on the pane being split, not on the
        // chat's row: with the caret in a right-hand pane, the chat's row is
        // a different session and the list opened pointing somewhere else
        // (#2625).
        self.sessions.cursor = Some(target);
        // Focus goes to the window, not the composer, so the sidebar keys and the
        // dialog's N and Esc reach it while the split waits.
        self.refocus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// Split the pane holding the caret and put `id` in the new slot (#2625).
    ///
    /// The rail's row menu used to open the row and then start a pick, which
    /// had two faults: `open_session` moves the chat leaf to the clicked row,
    /// so the split grew from the window's first pane instead of the pane
    /// being read, and for a session the tree did not hold yet the leaf reset
    /// in `adopt_chat_leaf` threw the split away entirely. Choosing the menu
    /// entry already named the session, so the pick is answered here and the
    /// second step is skipped.
    fn split_row(&mut self, id: Uuid, layout: Layout, cx: &mut Context<Self>) {
        let chat = self.transcript.session;
        // The pane the split grows from, seeded from the chat's own leaf when
        // the window holds no tree yet: the same rule the chord door walks, so
        // the two doors cannot disagree about the originating pane (#2757).
        let Some(target) = self.split_origin() else {
            self.live
                .notices
                .push("Open a session before splitting it.".to_string());
            cx.notify();
            return;
        };
        if Some(id) == chat || self.panes.as_ref().is_some_and(|tree| tree.contains(id)) {
            self.live
                .notices
                .push("That session is already open in a pane.".to_string());
            cx.notify();
            return;
        }
        self.split_pick = Some(layout);
        self.split_target = Some(target);
        self.choose_split(id, cx);
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

    /// The leaf a split door grows from (#2757).
    ///
    /// `focused_pane` reads `pane_focus` literally, and that id is a cache: a
    /// closed pane, a layout pruned at boot, or a session moved into the chat
    /// leaf all leave it naming a leaf the tree no longer holds. Filtered
    /// literally, that stale id made every door push "Open a session before
    /// splitting it." and return before the picker was ever set, while the
    /// window went on drawing a live focused pane, because the render already
    /// resolves the same id through `resolve_split_focus` (#2620). The door
    /// now asks the same question the render asks: the caret leaf, then the
    /// chat's own leaf, then the first surviving one. A window with no tree
    /// starts from the chat's leaf, the way the row menu always has.
    fn split_origin(&mut self) -> Option<Uuid> {
        let chat = self.transcript.session;
        if self.panes.is_none()
            && let Some(chat) = chat
        {
            self.panes = Some(PaneTree::leaf(chat));
        }
        let leaves = self.panes.as_ref()?.leaves();
        // `resolve_split_focus` answers `None` for the chat leaf, which is the
        // caret unless another pane took it.
        resolve_split_focus(&leaves, self.pane_focus, chat)
            .or(chat)
            .filter(|id| leaves.contains(id))
            .or_else(|| leaves.last().copied())
    }

    /// Point recall at the composer that has the caret. A recall keeps the
    /// session it took its messages from, so moving the caret cannot land an
    /// edit in another pane's queue (#2629/#2637).
    fn sync_composer_session(&mut self) {
        agent_bridge::set_composer_session(self.focused_pane());
    }

    /// Write the split the window is showing, so the next launch puts the same
    /// panes back in the same slots at the same sizes (#2675). Called wherever
    /// the layout can change under the user's hand: a split, a close, a
    /// divider drag, a caret move, a conversation switch. The chat's own leaf
    /// goes with it, so a launch knows which pane was the window's transcript
    /// (#2689).
    fn persist_layout(&self) {
        layout_store::save(
            &layout_store::path(),
            self.panes.as_ref(),
            self.pane_focus,
            self.transcript.session,
        );
    }

    /// Put back the split the last run left (#2675). Runs once, after the boot
    /// open, because the chat's own leaf has to exist before the tree can
    /// agree with it. A saved layout that does not hold the conversation on
    /// screen, a pane whose session the ledger no longer has, and a file that
    /// cannot be read all fall back to the plain chat: a wrong window is worse
    /// than a plain one.
    fn restore_layout(&mut self, saved: Option<layout_store::SavedLayout>, cx: &mut Context<Self>) {
        let Some(saved) = saved else {
            return;
        };
        let Some(chat) = self.transcript.session else {
            return;
        };
        let Some(tree) = saved.panes else {
            return;
        };
        if !tree.contains(chat) {
            logging::info("gui layout: the chat is not one of its panes, opening plain");
            return;
        }
        let known: std::collections::HashSet<Uuid> =
            self.sessions.items.iter().map(|item| item.id).collect();
        let Some(tree) = layout_store::prune(tree, &known) else {
            logging::info("gui layout: fewer than two panes survive, opening plain");
            return;
        };
        let leaves = tree.leaves();
        self.panes = Some(tree);
        // Every pane but the chat needs its own state: its composer, its
        // transcript, its plan strip. The chat's is the window's own.
        for leaf in leaves.iter().copied().filter(|id| *id != chat) {
            self.open_pane_state(leaf, cx);
        }
        // The caret goes back where it was, unless that pane is gone or it is
        // the chat itself, which the window spells as `None`.
        self.pane_focus = saved.focus.filter(|id| *id != chat && leaves.contains(id));
        self.sync_composer_session();
        // The pruned tree is what this run holds now, so the file matches it.
        self.persist_layout();
        logging::info(&format!(
            "gui layout: restored {} panes, caret {:?}",
            leaves.len(),
            self.pane_focus
        ));
        cx.notify();
    }

    /// The composer the keyboard owns (#2610): the focused pane's own field,
    /// or the window's when the chat leaf holds the caret.
    ///
    /// A split draws one real field per leaf, so anything that aims the
    /// keyboard or hands the composer a file resolves the leaf first. Aiming
    /// at the window's editor while a pane owns the caret puts the text and
    /// the attachment chips into a field the split never draws.
    fn composer_entity(&self) -> Entity<editor::Editor> {
        match self.split_focus() {
            Some(id) if Some(id) == self.transcript.session => self.editor.clone(),
            Some(id) => self
                .pane_states
                .get(&id)
                .map(|pane| pane.editor.clone())
                .unwrap_or_else(|| self.editor.clone()),
            None => self.editor.clone(),
        }
    }

    /// Name the element the window gives the keys to, for the caret receipt
    /// (#2741). Three answers: the composer that is drawn, the window root
    /// that owns overlays and non-chat screens, or a field the dashboard does
    /// not own (the rail's find box, a rename prompt).
    fn caret_owner_name(&self, window: &Window, cx: &App) -> &'static str {
        let Some(handle) = window.focused(cx) else {
            return "none";
        };
        if handle == self.composer_entity().read(cx).focus_handle {
            "composer"
        } else if handle == self.focus {
            "root"
        } else {
            "field"
        }
    }

    /// The leaf that owns the keyboard in the split, `None` meaning the chat
    /// leaf. Resolved through [`resolve_split_focus`] so the rules can be
    /// tested without a window.
    fn split_focus(&self) -> Option<Uuid> {
        let leaves = self
            .panes
            .as_ref()
            .map(|tree| tree.leaves())
            .unwrap_or_default();
        resolve_split_focus(&leaves, self.pane_focus, self.transcript.session)
    }

    /// Expand or collapse one pane's plan strip (#2624). The choice is per
    /// leaf: a pane in a wall of eight opens on its bar, and the one the user
    /// is reading trades room for the whole checklist without touching the
    /// others. The chat leaf keys on its own session, the same id every other
    /// pane uses, so the two cannot collide.
    fn toggle_pane_plan(&mut self, session: Uuid, cx: &mut Context<Self>) {
        if !self.plan_expanded.remove(&session) {
            self.plan_expanded.insert(session);
        }
        // An expanded strip takes room from the transcript under it, and a
        // viewport that shrinks must not leave the newest row clipped while
        // the reader believes they are at the bottom (#2685, #2624).
        if Some(session) == self.transcript.session {
            self.transcript_follow.rearm();
        } else if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.follow.rearm();
        }
        cx.notify();
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
        // The text typed in this pane is filed under its conversation before
        // the editor goes, so reopening it returns the draft.
        self.stash_pane_draft(session, cx);
        self.pane_states.remove(&session);
        self.split_drag = None;
        self.pane_focus = None;
        self.sync_composer_session();
        if self.transcript.session == Some(session) {
            self.open_session(absorbed, cx);
        } else {
            self.refocus_screen();
            cx.notify();
        }
        // The pane is gone: the file must say so before the next launch (#2675).
        self.persist_layout();
    }

    /// `ctrl+x` and the × on the focused pane's header: close the pane holding
    /// the caret (#2474).
    fn close_focused_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.focused_pane() {
            self.close_pane(session, cx);
        }
    }

    /// Alt+Tab walks the caret to the next pane and Alt+Shift+Tab to the
    /// previous one (#2580, #2579). Tab alone keeps every job it already had:
    /// accepting the emoji picker and the follow-up suggestion, and walking to
    /// the rail. Binding rotation to the bare key made those two fight for one
    /// keystroke, which is why the terminal moved to Alt and the window
    /// followed here.
    ///
    /// The leaves are visited in the order they are drawn, so the accent
    /// follows the layout the eye reads (#2475, #2472). With one pane there is
    /// nothing to switch to, so the key keeps whatever it did. The pane
    /// holding the caret wears the accent in its header and takes the page
    /// keys.
    fn cycle_pane_focus(&mut self, clockwise: bool, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_ref() else {
            return;
        };
        let leaves = tree.leaves();
        let at = self
            .focused_pane()
            .and_then(|id| leaves.iter().position(|leaf| *leaf == id))
            .unwrap_or(0);
        let rotation = if clockwise {
            split::PaneRotation::Clockwise
        } else {
            split::PaneRotation::Anticlockwise
        };
        // The wraparound lives in the core, where it is tested without a
        // window (#2580).
        let Some(index) = split::rotated_index(at, leaves.len(), rotation) else {
            return;
        };
        let next = leaves[index];
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
        self.sync_composer_session();
        // The keyboard walks with the caret (#2610). Without this the split
        // highlighted the new pane while every keystroke still landed in the
        // chat leaf's field, so the chord appeared to rotate exactly once.
        self.refocus_screen();
        // The caret is the layout too: the next launch puts it back (#2675).
        self.persist_layout();
        cx.notify();
    }

    /// Everything a fresh pane needs, keyed by the session it shows: its own
    /// composer wired to the send path, its own drag-to-copy, its plan path,
    /// and its newest messages. Split out of `choose_split` so a restored
    /// layout builds a pane exactly the way a split does (#2675).
    fn open_pane_state(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let title = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "(untitled)".to_string());
        // The pane's composer is its own entity (#2530), so a draft belongs to
        // the pane it was typed in and the send carries that pane's session.
        // It walks the window's shared history, not a private one, so history
        // is the same whichever pane has the caret.
        let pane_editor = cx.new({
            let history = self.history.clone();
            let history_path = composer::default_history_path();
            move |cx| editor::Editor::new_with_history(history, history_path, cx)
        });
        // A draft filed before this pane was closed, moved, or switched comes
        // back with it.
        if let Some(draft) = self.drafts.take(id) {
            pane_editor.update(cx, |ed, cx| ed.restore_state(draft, cx));
        }
        cx.subscribe(&pane_editor, move |this, _editor, event, cx| match event {
            editor::EditorEvent::Sent(text) => {
                this.send_from_pane(id, text.clone(), None, SendOrigin::Composer, cx);
            }
            editor::EditorEvent::Preview(path) => this.open_preview(path.clone(), cx),
        })
        .detach();
        // The pane's own drag-to-copy (#2623). The transcript cannot reach a
        // composer, so the pane's editor is the one that says "Copied to
        // clipboard" once a dragged range lands (#2587).
        let pane_copy = Rc::new(CopyDrag::default());
        {
            let editor = pane_editor.downgrade();
            pane_copy.set_copied_listener(Rc::new(move |cx: &mut App| {
                let _ = editor.update(cx, |editor, cx| editor.show_copied(cx));
            }));
        }
        // The pane's plan lives wherever its session is bound, which is a DB
        // read; the same lookup the window's strip uses, resolved once here.
        agent_bridge::spawn_plan_path(self.db_path.clone(), id, self.sink.clone());
        self.pane_states.insert(
            id,
            SplitPane {
                title,
                transcript: TranscriptState {
                    session: Some(id),
                    ..Default::default()
                },
                scroll: ScrollHandle::new(),
                follow: Follow::new(),
                editor: pane_editor,
                plan: plan_strip::PlanStripState::default(),
                plan_for: Some(id),
                live: transcript::LiveTurn::default(),
                busy: false,
                cancel: None,
                copy_drag: pane_copy,
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
                        Ok(loaded) => {
                            let (hidden, turns) = transcript::turns_with_origins(
                                &loaded.rows,
                                loaded.total,
                                &this.turn_metrics,
                                &loaded.origins,
                            );
                            pane.transcript.hidden = hidden;
                            pane.transcript.turns = turns;
                            pane.transcript.rows = loaded.rows;
                        }
                        Err(e) => pane.transcript.error = Some(e.into()),
                    }
                    // A new pane opens on its newest row (#2698).
                    pane.follow.rearm();
                    pane.scroll.scroll_to_bottom();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// A pick from any list: during a split pick it fills the waiting pane,
    /// otherwise it opens the conversation (#2760).
    ///
    /// The rail's full rows and its Enter key asked the picker first, but the
    /// topic chips, the filter's Enter and the Seeker's session rows opened
    /// directly. A topic chip also stops propagation, so the parent row never
    /// got the chance: the topic landed in the chat leaf while the picker
    /// stayed armed on the pane it started from, and the next click split
    /// something nobody pointed at.
    fn pick_or_open(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.split_pick.is_some() {
            self.choose_split(id, cx);
        } else {
            self.open_session(id, cx);
        }
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
        self.sync_composer_session();
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
        self.open_pane_state(id, cx);
        // The layout is the user's now, so it is written down for the next
        // launch (#2675).
        self.persist_layout();
    }

    /// File one pane's half-typed text under the conversation it belongs to,
    /// before the pane's editor is dropped (#2494).
    fn stash_pane_draft(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get(&session) else {
            return;
        };
        let editor = pane.editor.clone();
        let draft = editor.update(cx, |ed, cx| ed.take_draft(cx));
        self.drafts.store(session, draft);
    }

    /// The same for every pane, for a tree replaced wholesale.
    fn stash_all_pane_drafts(&mut self, cx: &mut Context<Self>) {
        let sessions: Vec<Uuid> = self.pane_states.keys().copied().collect();
        for session in sessions {
            self.stash_pane_draft(session, cx);
        }
    }

    /// Keep the chat on a leaf of the tree (#2472). A first open starts the
    /// tree; a plain switch moves the chat's own leaf to the new session;
    /// opening a session another pane already shows moves it into the chat and
    /// closes that pane, so one session never sits in two leaves.
    fn adopt_chat_leaf(&mut self, previous: Option<Uuid>, id: Uuid, cx: &mut Context<Self>) {
        self.place_chat_leaf(previous, id, cx);
        // The chat's leaf is part of the saved layout, so every path that
        // moves it writes the tree, the one-pane case included (#2675).
        self.persist_layout();
    }

    /// The body of `adopt_chat_leaf`. It writes nothing, so the caller can
    /// save once, whichever path the switch took.
    fn place_chat_leaf(&mut self, previous: Option<Uuid>, id: Uuid, cx: &mut Context<Self>) {
        let Some(tree) = self.panes.as_mut() else {
            self.panes = Some(PaneTree::leaf(id));
            return;
        };
        if tree.contains(id) {
            // The tree work first, so the borrow of `self.panes` ends before
            // the pane's draft is filed.
            if let Some(was) = previous
                && was != id
                && tree.leaves().len() > 1
            {
                tree.close(was);
            }
            self.stash_pane_draft(id, cx);
            self.pane_states.remove(&id);
            return;
        }
        if let Some(was) = previous
            && tree.rename_leaf(was, id)
        {
            return;
        }
        self.panes = Some(PaneTree::leaf(id));
        self.stash_all_pane_drafts(cx);
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
        // A double click is a layout change on its own: the pair goes back to
        // even and that size is what the next launch must show (#2675). It is
        // answered before the borrow below so the write can take the window.
        if ev.click_count == 2 {
            let evened = self
                .panes
                .as_mut()
                .is_some_and(|tree| tree.even_divider(path, divider));
            if evened {
                self.persist_layout();
                cx.notify();
            }
            return;
        }
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
    /// `/compact` and `/goal` are dispatched here because they compose a turn,
    /// which only the window has. `send_command_turn` keeps that turn on the
    /// session that asked for it. Everything else runs on the bridge against
    /// the engine's own entry points, so the answer a window user reads is the
    /// one the terminal prints.
    fn run_text_command(
        &mut self,
        command: text_commands::TextCommand,
        arg: Option<String>,
        session: Option<Uuid>,
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
                self.send_command_turn(
                    session,
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string(),
                    Some("/compact".to_string()),
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
                let shown = format!("/cowork {arg}");
                self.send_command_turn(
                    session,
                    text_commands::cowork_prompt(&arg),
                    Some(shown.trim_end().to_string()),
                    cx,
                );
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
                    let shown = format!("/goal {arg}");
                    self.send_command_turn(
                        session,
                        opencrabs::brain::goal::goal_command_prompt(&arg),
                        Some(shown.trim_end().to_string()),
                        cx,
                    );
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
                    let Some(session) = session else {
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
                            this.push_session_notice(
                                session,
                                text_commands::cd_reply(&applied.display().to_string()),
                            );
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
            other => {
                let Some(session) = session else {
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
        // Closing bumps the deep-search generation so a query still in flight
        // bows out instead of repopulating a rail that is no longer shown
        // (#2720).
        if !open {
            self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
        }
        // Both directions move the keyboard: opening hands it to the list,
        // closing gives it back to the input (#2665).
        self.refocus_screen();
        cx.notify();
    }

    /// Stop the turn the focused split leaf is running, if there is one.
    ///
    /// `/stop` and Esc share this so the two cannot drift about which turn
    /// they stop or what they leave behind (#2614, #2657). The window's turn
    /// is a separate question: a leaf runs with `pane.busy` set while
    /// `self.busy` stays clear, so a stop that looked only at `self.busy`
    /// walked straight past a running pane and reported nothing to do.
    ///
    /// Returns true when a pane turn was actually cancelled. `why` is the name
    /// in the log line, so a stop can still be traced to the key that did it.
    fn cancel_focused_pane_turn(&mut self, why: &str, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.pane_focus else {
            return false;
        };
        let Some(pane) = self.pane_states.get_mut(&id) else {
            return false;
        };
        let Some(cancel) = pane.cancel.take() else {
            return false;
        };
        cancel.cancel();
        pane.busy = false;
        pane.live.thinking = false;
        // The card ends here, not when the service's event lands (#2811): the
        // user asked for the stop and the pane must stop claiming a turn is
        // running before the cancellation unwinds.
        pane.live.stopped = true;
        pane.live.notices.push("Operation cancelled.".to_string());
        // The stop names its session and what is still waiting, so the flush
        // that lands with the cancellation event can be paired to it (#2695).
        logging::info(&format!(
            "pane turn cancelled by {why}: session {id}, {} still queued",
            agent_bridge::queue_snapshot(id).len()
        ));
        cx.notify();
        true
    }

    /// `/stop`: abort the turn in flight, the same effect as Esc and the
    /// same wording. The pane stops claiming a turn is running the moment the
    /// user asks, and the service's own failure event lands afterwards.
    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        // A stop ends the offer too (#2708): the turn that made the buttons is
        // gone, so they can only send a follow-up nobody asked for.
        for sid in [self.focused_pane(), self.transcript.session]
            .into_iter()
            .flatten()
        {
            agent_bridge::options_take(sid);
        }
        // A split leaf runs its own turn, so `/stop` reaches the pane the
        // keyboard is on before it reports nothing to stop (#2614).
        if self.cancel_focused_pane_turn("/stop", cx) {
            return;
        }
        let Some(cancel) = self.cancel.take() else {
            self.live.notices.push("Nothing is running.".to_string());
            cx.notify();
            return;
        };
        cancel.cancel();
        self.busy = false;
        self.live.thinking = false;
        self.live.stopped = true;
        self.live.notices.push("Operation cancelled.".to_string());
        logging::info("turn cancelled by /stop");
        cx.notify();
    }

    /// `/models` with no argument opens the picker. With an argument it is the
    /// switch the terminal performs, and no screen opens: the reply is the
    /// answer, exactly like the terminal's.
    fn open_models(&mut self, arg: Option<String>, session: Option<Uuid>, cx: &mut Context<Self>) {
        let target = session.or_else(|| self.focused_pane());
        self.models_session = target;
        // The catalogue is read once, here, so a frame never walks it. A pane
        // carries its own pair, so the picker marks that pair rather than the
        // window's when the command came from a split.
        let current = agent_bridge::bridge().map(|b| match target {
            Some(sid) => (
                b.service.provider_name_for_session(sid),
                b.service.provider_model_for_session(sid),
            ),
            None => (b.service.provider_name(), b.service.provider_model()),
        });
        self.reset_model_picker(current, None);
        if let Some(arg) = arg {
            self.switch_model(arg, cx);
            return;
        }
        self.models_query.clear();
        self.screen = Screen::Models;
        // The list opens on the provider answering right now, so the marked
        // row and the picker's cursor agree from the first frame.
        self.models_provider = self
            .models_rows
            .iter()
            .find(|row| row.current)
            .map(|row| row.provider.clone());
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
                            let target = self.models_session.or(self.transcript.session);
                            agent_bridge::bridge().map(|bridge| match target {
                                Some(sid) => bridge.service.provider_name_for_session(sid),
                                None => bridge.service.provider_name(),
                            })
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
        self.sessions.rail_menu = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Put a one-line answer in the conversation that owns it, so a pane never
    /// reads a switch receipt meant for the window chat.
    fn push_session_notice(&mut self, session: Uuid, line: String) {
        if self.transcript.session == Some(session) {
            self.live.notices.push(line);
        } else if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.live.notices.push(line);
        } else {
            self.live.notices.push(line);
        }
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
        let Some(session) = self.models_session.or_else(|| self.focused_pane()) else {
            self.live.notices.push(
                "No conversation is open, so there is nothing to switch. Type /new first."
                    .to_string(),
            );
            cx.notify();
            return;
        };
        // One pick, one switch. Enter can repeat faster than the provider
        // handshake, and every repeat used to persist another confirmation.
        if self.models_switching.is_some() {
            logging::debug("model switch already in flight");
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            self.push_session_notice(
                session,
                "The agent is still starting. Try again in a moment.".to_string(),
            );
            cx.notify();
            return;
        };
        self.models_switching = Some(session);
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

    /// Esc, or `/usage` reached from here: back to the conversation, landing
    /// on its newest row. It used to promise "exactly where it was left",
    /// which is the wrong promise twice over: rows land while a panel is
    /// open, and a reader who stepped away did not ask to be parked in
    /// history when they come back (#2698).
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

    /// ctrl+shift+t: the theme modal (#2619). It opens over the conversation,
    /// centred, so the choice is made against real content. Picking a card
    /// saves it at once, the way Telegram's theme picker does; there is no
    /// draft to keep or discard.
    fn toggle_themes(&mut self, cx: &mut Context<Self>) {
        if self.themes_open {
            self.close_themes(cx);
            return;
        }
        // The wheel opens on the accent in force, so a second visit starts
        // where the first left off.
        let accent = palette::saved_mode()
            .1
            .as_deref()
            .and_then(gui_theme::parse_accent)
            .unwrap_or(gui_theme::DEFAULT_ACCENT);
        let (hue, sat, val) = gui_theme::rgb_to_hsv(accent);
        self.themes_hue = hue;
        self.themes_sat = sat.max(0.25);
        self.themes_val = val.max(0.35);
        self.themes_card = match palette::saved_mode().0 {
            GuiTheme::Orgn => 0,
            GuiTheme::Light => 1,
            GuiTheme::Custom => 2,
            GuiTheme::Auto => 3,
        };
        self.themes_drag = None;
        self.themes_open = true;
        // The window's root takes the keyboard, not the composer: the modal
        // owns the arrows and Enter, so a stale editor focus would send a
        // message instead.
        self.refocus_overlay();
        cx.notify();
    }

    /// Leave the theme modal. A pick was already saved, so closing only puts
    /// the screen back; a card the arrows merely previewed is undone here, so
    /// the window never keeps a theme the config does not hold.
    fn close_themes(&mut self, cx: &mut Context<Self>) {
        self.themes_drag = None;
        self.themes_open = false;
        self.fonts_open = false;
        palette::adopt(palette::saved());
        self.refocus_overlay();
        cx.notify();
    }

    /// The accent the wheel is painting right now.
    fn themes_accent(&self) -> u32 {
        gui_theme::hsv_to_rgb(self.themes_hue, self.themes_sat, self.themes_val)
    }

    /// The modal's own keys. The arrows walk the four cards without writing
    /// anything, so the window repaints in the card under the keyboard and the
    /// choice is made by looking. Enter saves the card it stands on; Escape
    /// closes and leaves the saved theme alone. Every other key is swallowed,
    /// because the window behind the modal is not listening (#2619).
    fn theme_modal_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        const CARDS: [GuiTheme; 4] = [
            GuiTheme::Orgn,
            GuiTheme::Light,
            GuiTheme::Custom,
            GuiTheme::Auto,
        ];
        match ks.key.as_str() {
            "escape" => {
                // The font list is the innermost surface, so Escape closes it
                // before the modal (#2786).
                if self.fonts_open {
                    self.fonts_open = false;
                    cx.notify();
                } else {
                    self.close_themes(cx);
                }
            }
            "left" | "up" => {
                self.themes_card = (self.themes_card + 3) % 4;
                let card = CARDS[self.themes_card];
                self.preview_card(card, cx);
            }
            "right" | "down" | "tab" => {
                self.themes_card = (self.themes_card + 1) % 4;
                let card = CARDS[self.themes_card];
                self.preview_card(card, cx);
            }
            "enter" => {
                let card = CARDS[self.themes_card.min(3)];
                self.pick_theme(card, cx);
                // Custom is the only card with something left to do under it:
                // the wheel edits the accent, so picking it keeps the modal.
                if card != GuiTheme::Custom {
                    self.close_themes(cx);
                } else {
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Repaint the window in a card's theme without saving it. The arrows walk
    /// the four cards this way, so the choice is made by looking; nothing is
    /// written until a pick, and closing puts the saved theme back.
    fn preview_card(&mut self, mode: GuiTheme, cx: &mut Context<Self>) {
        let accent = if mode == GuiTheme::Custom {
            Some(gui_theme::format_accent(self.themes_accent()))
        } else {
            palette::saved_mode().1
        };
        palette::adopt(gui_theme::resolve(
            mode,
            palette::system_is_dark(),
            accent.as_deref(),
        ));
        cx.notify();
    }

    /// Turn a point in window coordinates into the accent control under it and
    /// start dragging it. A press that lands on none of them is ignored, so a
    /// click on the card's padding does not move the colour.
    fn themes_press_at(&mut self, x: f32, y: f32) -> bool {
        let boxed = |area: &std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>| *area.borrow();
        if let Some(bounds) = boxed(&self.themes_wheel)
            && let Some(hit) = accent::wheel_hit(bounds, x, y)
        {
            self.themes_hue = hit.hue;
            // Past the rim the drag pins to full saturation instead of
            // wrapping, so a fast flick outward keeps the colour it had.
            self.themes_sat = hit.saturation;
            self.themes_drag = Some(accent::Drag::Wheel);
            return true;
        }
        if let Some(bounds) = boxed(&self.themes_sat_track)
            && accent::inside(bounds, x, y, accent::SLACK)
        {
            self.themes_sat = accent::slider_value(bounds, x);
            self.themes_drag = Some(accent::Drag::Saturation);
            return true;
        }
        if let Some(bounds) = boxed(&self.themes_val_track)
            && accent::inside(bounds, x, y, accent::SLACK)
        {
            self.themes_val = accent::slider_value(bounds, x);
            self.themes_drag = Some(accent::Drag::Value);
            return true;
        }
        false
    }

    /// Move the control a drag already holds. The pointer is followed even
    /// once it leaves the painted box, the way a slider normally behaves, so a
    /// fast drag cannot drop the value halfway.
    fn themes_drag_to(&mut self, x: f32, y: f32) -> bool {
        let drag = match self.themes_drag {
            Some(drag) => drag,
            None => return false,
        };
        match drag {
            accent::Drag::Wheel => {
                // A wheel is polar, so a drag reads the angle from wherever the
                // pointer went and pins the saturation at the rim.
                if let Some(hit) = (*self.themes_wheel.borrow())
                    .and_then(|bounds| accent::wheel_drag(bounds, x, y))
                {
                    self.themes_hue = hit.hue;
                    self.themes_sat = hit.saturation;
                }
                true
            }
            accent::Drag::Saturation => {
                if let Some(bounds) = *self.themes_sat_track.borrow() {
                    self.themes_sat = accent::slider_value(bounds, x);
                }
                true
            }
            accent::Drag::Value => {
                if let Some(bounds) = *self.themes_val_track.borrow() {
                    self.themes_val = accent::slider_value(bounds, x);
                }
                true
            }
        }
    }

    /// Pick a card: apply the theme and save it in one step (#2619).
    fn pick_theme(&mut self, mode: GuiTheme, cx: &mut Context<Self>) {
        let hex = gui_theme::format_accent(self.themes_accent());
        let name = match mode {
            GuiTheme::Orgn => "orgn",
            GuiTheme::Light => "light",
            GuiTheme::Custom => "custom",
            GuiTheme::Auto => "auto",
        };
        match Config::write_key_string("gui", "theme", name) {
            Ok(()) => logging::info(&format!("themes: kept '{name}' as [gui] theme")),
            Err(e) => logging::error(&format!("themes: could not save '{name}': {e}")),
        }
        // Only `custom` carries an accent; the other three keep whatever was
        // saved, so switching away and back does not lose the wheel's colour.
        let accent = if mode == GuiTheme::Custom {
            match Config::write_key_string("gui", "custom_accent", &hex) {
                Ok(()) => logging::info(&format!("themes: kept accent {hex}")),
                Err(e) => logging::error(&format!("themes: could not save accent {hex}: {e}")),
            }
            Some(hex)
        } else {
            palette::saved_mode().1
        };
        palette::remember_mode(mode, accent.clone());
        palette::adopt(gui_theme::resolve(
            mode,
            palette::system_is_dark(),
            accent.as_deref(),
        ));
        cx.notify();
    }

    /// Repaint the window with the accent the wheel is showing, without
    /// saving it: a drag paints live, and the save lands on release.
    fn preview_accent(&mut self, cx: &mut Context<Self>) {
        let hex = gui_theme::format_accent(self.themes_accent());
        palette::preview(GuiTheme::Custom, Some(&hex));
        cx.notify();
    }

    /// Save the accent a drag settled on (#2619). A release with no drag under
    /// it is not a save: it is a click that missed every control.
    fn keep_accent(&mut self, cx: &mut Context<Self>) {
        if self.themes_drag.is_none() {
            return;
        }
        self.themes_drag = None;
        let hex = gui_theme::format_accent(self.themes_accent());
        match Config::write_key_string("gui", "custom_accent", &hex) {
            Ok(()) => logging::info(&format!("themes: kept accent {hex}")),
            Err(e) => logging::error(&format!("themes: could not save accent {hex}: {e}")),
        }
        // The wheel only shows under `custom`, so the mode in force is the one
        // to keep; the accent rides along with it.
        palette::remember_mode(palette::saved_mode().0, Some(hex));
        cx.notify();
    }

    /// The theme modal itself: a centred card over the window, the shape of
    /// Telegram's picker. A card applies and saves on the click, so there is no
    /// Keep button; Escape and a click on the scrim close it. Custom keeps the
    /// modal open, because the wheel beneath the cards is the point of picking
    /// it (#2619).
    ///
    /// It is attached last in the root's paint order, so while it is open it is
    /// the surface the user is looking at and the only one the pointer reaches.
    fn themes_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let saved = palette::saved_mode().0;
        let accent = palette::current().accent;
        let mut cards: Vec<Stateful<Div>> = Vec::with_capacity(themes_view::CARDS.len());
        for (index, &(mode, label, hint)) in themes_view::CARDS.iter().enumerate() {
            let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.themes_card = index;
                this.pick_theme(mode, cx);
                // Custom needs the wheel, so its click leaves the modal
                // standing; the other three are complete on their own.
                if mode != GuiTheme::Custom {
                    this.close_themes(cx);
                }
            });
            cards.push(
                themes_view::card(mode, label, hint, self.themes_card == index, accent)
                    .on_click(pick),
            );
        }
        // The wheel is the only draggable thing in the window: a press seeds
        // the hue and the drag repaints live, with the save landing on release,
        // so a drag never writes the config once per frame.
        let press = cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
            let x = f32::from(ev.position.x);
            let y = f32::from(ev.position.y);
            if this.themes_press_at(x, y) {
                this.preview_accent(cx);
            }
        });
        let slide = cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
            let x = f32::from(ev.position.x);
            let y = f32::from(ev.position.y);
            if this.themes_drag_to(x, y) {
                this.preview_accent(cx);
            }
        });
        let release = cx.listener(|this, _ev: &MouseUpEvent, _window, cx| {
            this.keep_accent(cx);
        });
        // A drag that ends outside the row still saves: `keep_accent` ignores a
        // release with no drag under it.
        let release_out = cx.listener(|this, _ev: &MouseUpEvent, _window, cx| {
            this.keep_accent(cx);
        });
        let accent_row = (saved == GuiTheme::Custom).then(|| {
            let areas = themes_view::AccentAreas {
                wheel: self.themes_wheel.clone(),
                saturation: self.themes_sat_track.clone(),
                value: self.themes_val_track.clone(),
            };
            let hex = gui_theme::format_accent(self.themes_accent());
            let shown = palette::current();
            div()
                .id("theme-accent-row")
                .on_mouse_down(MouseButton::Left, press)
                .on_mouse_move(slide)
                .on_mouse_up(MouseButton::Left, release)
                .on_mouse_up_out(MouseButton::Left, release_out)
                .child(themes_view::accent_row(
                    self.themes_hue,
                    self.themes_sat,
                    self.themes_val,
                    &hex,
                    shown,
                    areas,
                ))
                .into_any_element()
        });
        // The font row (#2786): the family in force, and the machine's
        // families under it once the row is opened. The rows are built here
        // because only this `cx` can mint a click listener, and only while the
        // list is open: the catalog is the whole machine, so a closed row must
        // not build three hundred elements on every frame.
        let shown = palette::current();
        let current_font = fonts::current_ui();
        let font_picks: Vec<Stateful<Div>> = if self.fonts_open {
            fonts::picker_rows()
                .into_iter()
                .map(|family| {
                    let selected = family.eq_ignore_ascii_case(&current_font);
                    // The id and the label are read before the family moves
                    // into the listener, which is the only thing that needs it
                    // after the row is built.
                    let id = SharedString::from(format!("theme-font-{family}"));
                    let label = SharedString::from(gui_font::row_label(&family));
                    let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.pick_font(&family, cx);
                    });
                    div()
                        .id(id)
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(shown.panel_hi)))
                        .text_xs()
                        .text_color(rgb(if selected {
                            shown.accent
                        } else {
                            shown.text_normal
                        }))
                        .child(label)
                        .on_click(pick)
                })
                .collect()
        } else {
            Vec::new()
        };
        let font_toggle = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.toggle_fonts(cx);
        });
        let font_row =
            themes_view::font_row(&current_font, self.fonts_open, font_toggle, font_picks);
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.close_themes(cx);
        });
        themes_view::modal(
            cards,
            accent_row,
            font_row,
            "A click saves the card at once; the arrows walk them and Enter picks.",
            dismiss,
        )
        .into_any_element()
    }

    /// Open or close the font list under the theme cards (#2786).
    fn toggle_fonts(&mut self, cx: &mut Context<Self>) {
        self.fonts_open = !self.fonts_open;
        cx.notify();
    }

    /// Keep a family from the font list (#2786). A pick saves at once, the way
    /// a theme card does, and closes the list so the modal stays the size it
    /// was. The whole window repaints in the new family on the next frame.
    fn pick_font(&mut self, family: &str, cx: &mut Context<Self>) {
        match fonts::keep_ui(family) {
            Ok(()) => logging::info(&format!(
                "fonts: kept '{}' as [gui] font",
                fonts::current_ui()
            )),
            Err(e) => logging::error(&format!("fonts: could not save '{family}': {e}")),
        }
        self.fonts_open = false;
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

    /// ctrl+h, or Cmd+? on a Mac: the key reference, the window's door to the
    /// rows the terminal prints via `/help` (#2320). It is read-only, so it
    /// can sit above every other affordance: closing it leaves the screen
    /// exactly as it was.
    fn toggle_help(&mut self, cx: &mut Context<Self>) {
        self.help_open = !self.help_open;
        if !self.help_open {
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

    /// Put the rail's find field in front of the keyboard (#2812).
    ///
    /// The row the keyboard stands on is armed, so Enter has a target, and the
    /// overlay takes the keys so the composer's Enter and arrows stop answering
    /// while the field says it is listening (#2454). `ctrl+f` and the first
    /// typed character both come through here, so the two ways into the field
    /// cannot drift apart.
    fn arm_rail_filter(&mut self, cx: &mut Context<Self>) {
        self.sessions.filter_active = true;
        if self.sessions.cursor.is_none() || !self.sessions.cursor_visible() {
            self.sessions.cursor = self.sessions.cursor_step(None, 1);
        }
        self.refocus_overlay();
        cx.notify();
    }

    /// Leave the rail filter. The query is a transient find aid, so every
    /// exit clears it: the rail returns to the full list and never hides a
    /// filter behind a closed field.
    fn close_rail_filter(&mut self, cx: &mut Context<Self>) {
        self.sessions.filter.clear();
        self.sessions.filter_active = false;
        // The deep hits clear with the field, and the generation bumps so a
        // query still in flight for the old text bows out instead of landing
        // on an empty rail (#2720).
        self.sessions.content_matches.clear();
        self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
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
    /// A message typed in a pane's own composer (#2530). The window runs one
    /// turn at a time and that turn follows the open conversation, so the pane
    /// hands its session to the window and sends there, the same repair the
    /// terminal makes on a split send. The pane's session becomes the chat, so
    /// the pane closes with it: one session never sits in two leaves (#2472).
    /// Whether the window draws more than one pane (#2530). Window-level
    /// chrome steps aside for the per-pane copies only then, so an unsplit
    /// window keeps the layout it has always had.
    fn window_is_split(&self) -> bool {
        self.panes
            .as_ref()
            .is_some_and(|tree| tree.leaves().len() > 1)
    }

    /// A pane's own send. `display` is the row the reader gets when the text is
    /// scaffolding the model needs and the user does not (#2756).
    fn send_from_pane(
        &mut self,
        session: Uuid,
        text: String,
        display: Option<String>,
        origin: SendOrigin,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // A send under an open overlay is the composer answering keys nobody
        // gave it (#2767). The caret repair keeps the composer out of the
        // dispatch path, and this refuses the one path that got around it: a
        // name typed into the topic prompt was submitted here instead, so the
        // topic was never created and the name reached the agent as a message.
        // Only a composer's own send is that case. The window mints turns of
        // its own for a named session (the plan card's Approve and Discard,
        // a command's reply turn), and chrome is not theirs to refuse: a plan
        // waiting on approval kept its tap landing nowhere while the overlay
        // that ate it stayed up.
        let overlays = self.open_overlays();
        if origin == SendOrigin::Composer
            && !overlays.is_empty()
            && matches!(self.screen, Screen::Chat)
        {
            logging::debug(&format!(
                "send refused: {:?} owns the keyboard",
                overlays.last()
            ));
            return;
        }
        // A send ends that pane's offer, the same rule the window's own
        // composer follows (#2708).
        agent_bridge::options_take(session);
        // A slash line is a screen command: `/help`, `/models` and `/sessions`
        // read and write the window's own chrome, so they run there and the
        // leaf is left where it is. A prompt is the pane's own turn (#2614).
        if let Some(outcome) = commands::parse_command(&text) {
            // Commands act on the window's chrome, but the reply belongs to
            // the field it was typed in: after the command runs, the notices
            // it wrote move to that pane, so a split never answers in the
            // wrong leaf (#2614).
            let before = self.live.notices.len();
            self.run_command(outcome, Some(session), cx);
            if Some(session) != self.transcript.session
                && let Some(pane) = self.pane_states.get_mut(&session)
            {
                let split_at = before.min(self.live.notices.len());
                let moved: Vec<String> = self.live.notices.split_off(split_at);
                pane.live.notices.extend(moved);
            }
            cx.notify();
            return;
        }
        if let Some(cmd) = opencrabs::ui_core::bang::bang_command(&text) {
            if let Some(pane) = self.pane_states.get_mut(&session) {
                pane.live.notices.push(format!("! {cmd}"));
            }
            bang::spawn(cmd.to_string(), self.sink.clone());
            cx.notify();
            return;
        }
        if let Some(err) = self.agent_error.clone() {
            if let Some(pane) = self.pane_states.get_mut(&session) {
                pane.live.notices.push(format!("agent offline: {err}"));
            }
            cx.notify();
            return;
        }
        // One turn per leaf: a second send while this pane is working joins
        // the session's queue, the terminal's own path (#2441).
        let Some(pane_busy) = self.pane_states.get(&session).map(|p| p.busy) else {
            return;
        };
        if pane_busy {
            agent_bridge::queue_push(session, Self::pending_message(text, display.clone()));
            cx.notify();
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            agent_bridge::queue_push(session, Self::pending_message(text, display.clone()));
            cx.notify();
            return;
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let shown = display.clone().unwrap_or_else(|| text.clone());
        if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.live = transcript::LiveTurn::default();
            pane.live.begin(shown);
            pane.transcript.live_expanded = false;
            pane.busy = true;
            pane.cancel = Some(cancel.clone());
            // Sending in a pane puts its reader back on the end of it (#2698).
            pane.follow.rearm();
            pane.scroll.scroll_to_bottom();
        }
        // Attachments are tracked against the pane's session, so the file
        // lands in that conversation's artifacts, exactly as the window's does.
        let attached = editor::marker_paths(&text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session,
            text,
            display,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session, attached);
        }
        cx.notify();
    }

    /// What this session's card should say it is doing right now (#2718), from
    /// whichever owner holds its turn: a split leaf, a turn parked under a
    /// conversation the user switched away from, the open turn, or a turn
    /// running with no leaf at all. `None` when nothing is in flight, so the
    /// card keeps the message the ledger holds.
    ///
    /// The four sources are checked in that order because a session can only
    /// be in one of them, and the ones that own a card of their own are more
    /// specific than the rail's own map.
    fn rail_activity_for(&self, id: Uuid) -> Option<SharedString> {
        let line = |live: &transcript::LiveTurn| {
            let text = rail_activity_line(live);
            (!text.is_empty()).then(|| SharedString::from(text))
        };
        if let Some(pane) = self.pane_states.get(&id)
            && pane.busy
        {
            return line(&pane.live);
        }
        if let Some(turn) = self.stashed.get(&id) {
            return line(&turn.live);
        }
        if self.busy && self.transcript.session == Some(id) {
            return line(&self.live);
        }
        self.rail_live.get(&id).and_then(line)
    }

    /// The progress of a pane's own turn, folded into that leaf's card
    /// (#2614). The window's live state is untouched: a split shows two turns
    /// streaming at once without either one bleeding into the other.
    fn fold_pane_progress(
        &mut self,
        session: Uuid,
        progress: opencrabs::brain::agent::ProgressEvent,
        at: std::time::Instant,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.pane_states.get_mut(&session) {
            fold_live(&mut pane.live, progress, at);
            // Rows landing under a reader who scrolled up must not drag them
            // back to the bottom (#2685, #2698).
            Self::snap_pane_if_following(pane);
            cx.notify();
            return;
        }
        // A conversation the user switched away from keeps its turn in the
        // stash (#2616). Feeding the card there is what stops it freezing at
        // the frame the switch left: opening it again shows the tool rows and
        // the counter the turn actually reached, not a stale snapshot.
        if let Some(turn) = self.stashed.get_mut(&session) {
            fold_live(&mut turn.live, progress, at);
            cx.notify();
            return;
        }
        // A session with no leaf is what a sub-agent looks like: its
        // progress arrives with its own id and there is no pane to fold it
        // into. The event is the only trace of that turn the window holds,
        // so the rail records it here instead of dropping it (#2636).
        //
        // The card also keeps the event, not just a flag (#2718): with no
        // message persisted yet, a card that only knows "streaming" shows the
        // last thing said in a previous turn, or nothing at all, while the
        // turn is running tool calls. The same fold the panes use gives the
        // card a line that is true right now.
        let first = self.rail.observe_stream(session);
        // Read the line before folding and after it, so the repaint happens
        // when the sentence the card shows actually changes. The first
        // sighting always repaints; a repeat that leaves the line alone does
        // not (#2636), and one that moves the turn from a finished call to the
        // next one does, or the card would freeze on the first tool it saw.
        let before = self
            .rail_live
            .get(&session)
            .map(rail_activity_line)
            .unwrap_or_default();
        match self.rail_live.get_mut(&session) {
            Some(live) => fold_live(live, progress, at),
            None => {
                let mut live = transcript::LiveTurn::default();
                fold_live(&mut live, progress, at);
                self.rail_live.insert(session, live);
            }
        }
        let moved = self
            .rail_live
            .get(&session)
            .is_some_and(|live| rail_activity_line(live) != before);
        if first || moved {
            cx.notify();
        }
    }

    /// The pane's turn finished: its own card settles and its rows come back
    /// from the DB, so the window's conversation never moves (#2614).
    fn settle_pane(
        &mut self,
        session: Uuid,
        tokens_per_second: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        let calls = {
            let Some(pane) = self.pane_states.get_mut(&session) else {
                return;
            };
            pane.live.finalize(tokens_per_second);
            pane.live.thinking = false;
            pane.busy = false;
            pane.cancel = None;
            // The turn ends where the reader is, not where it ended (#2698).
            Self::snap_pane_if_following(pane);
            pane.live.tools.len()
        };
        // The leaf's card keeps the numbers it showed in flight, the same ones
        // the window's card keeps (#2632): the ctx budget every pane shares,
        // and the calls this leaf made.
        self.pending_metrics.insert(
            session,
            transcript::TurnMetrics {
                ctx_used: self.ctx_tokens.unwrap_or(0),
                ctx_max: self.ctx_max,
                tps: tokens_per_second,
                tool_calls: calls,
            },
        );
        cx.notify();
        self.reload_pane(session, cx);
        // The leaf's turn is over: whatever is still queued for its session
        // becomes the next turn, the same flush the window runs (#2695).
        self.drain_pane_pending(session, cx);
    }

    /// The pane's turn failed: the reason lands in that leaf's notices instead
    /// of the window's (#2614).
    fn fail_pane(&mut self, session: Uuid, text: String, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get_mut(&session) else {
            return;
        };
        pane.live.notices.push(text);
        pane.live.thinking = false;
        pane.busy = false;
        pane.cancel = None;
        cx.notify();
        // A failed turn still hands the queue to the next one: the text the
        // user typed behind it is not part of the failure (#2695).
        self.drain_pane_pending(session, cx);
    }

    /// A pane's turn ended (settled, failed, or stopped): everything still
    /// queued for its session goes out as ONE joined turn, the exact contract
    /// the window's `drain_pending` keeps (#2695). Before this, only the
    /// window's post-batch drain existed and it reads the window's session,
    /// so a leaf's queue survived the turn and sat in the strip until some
    /// later turn happened to drain it mid-flight.
    ///
    /// The busy gate keeps the #2631 ordering: a stop flushes when its
    /// cancellation event lands, never a second spawn into a turn still
    /// unwinding, and a user who already sent the next turn by hand is never
    /// doubled up on.
    fn drain_pane_pending(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_states.get(&session) else {
            return;
        };
        if pane.busy {
            return;
        }
        let waiting = agent_bridge::queue_snapshot(session);
        if waiting.is_empty() {
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the text, the same rule the window keeps.
            return;
        };
        let msgs = agent_bridge::queue_take(session);
        let Some(joined) = agent_bridge::PendingMessage::join(&msgs) else {
            return;
        };
        logging::info(&format!(
            "pane queue flush: session {session}, sending {} queued as one turn",
            msgs.len()
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        if let Some(pane) = self.pane_states.get_mut(&session) {
            pane.live = transcript::LiveTurn::default();
            pane.live.begin(joined.display_text.clone());
            pane.transcript.live_expanded = false;
            pane.busy = true;
            pane.cancel = Some(cancel.clone());
            // A flushed queue is a send too, so the pane follows its answer
            // the way a typed one does (#2698).
            pane.follow.rearm();
            pane.scroll.scroll_to_bottom();
        }
        let attached = editor::marker_paths(&joined.context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session,
            joined.context_text,
            Some(joined.display_text),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session, attached);
        }
        cx.notify();
    }

    /// Re-read one pane's rows from the ledger, the same read `add_pane` makes
    /// when it first draws the leaf (#2614).
    fn reload_pane(&mut self, session: Uuid, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, session).await;
            this.update(cx, |this, cx| match result {
                Ok(loaded) => {
                    let (hidden, turns) = transcript::turns_with_origins(
                        &loaded.rows,
                        loaded.total,
                        &this.turn_metrics,
                        &loaded.origins,
                    );
                    // The turn this leaf's card was watching has landed, so it
                    // takes the numbers it showed in flight before the rows are
                    // drawn (#2632).
                    let turns = this.claim_pane_metrics(session, turns);
                    if let Some(pane) = this.pane_states.get_mut(&session) {
                        pane.transcript.hidden = hidden;
                        pane.transcript.turns = turns;
                        // The persisted row is on screen: the live copy
                        // retires, exactly as the window's does, unless the
                        // flush already started the next turn (#2695):
                        // clearing then would wipe the card of the turn now
                        // running, the same guard the window's reload keeps.
                        if !pane.busy {
                            pane.live.clear();
                        }
                        // The persisted rows land where the reader is watching
                        // (#2698), never under a reader parked in history.
                        Self::snap_pane_if_following(pane);
                    }
                    cx.notify();
                }
                Err(e) => {
                    if let Some(pane) = this.pane_states.get_mut(&session) {
                        pane.transcript.error = Some(e.into());
                        pane.scroll.scroll_to_bottom();
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Send one turn to the session it belongs to: the window's chat when that
    /// is the session, otherwise the split leaf drawing it (#2672).
    ///
    /// A command typed into a pane resolved its session at the door and then
    /// the turn it asked for fell through to the window's chat, so `/execute`
    /// in pane B approved B's plan and ran the seeded checklist in A. Both
    /// paths already own the queue and busy gates for their leaf, so the turn
    /// keeps the conversation it was asked for.
    fn send_turn_to(
        &mut self,
        session: Uuid,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if Some(session) == self.transcript.session {
            self.send_message_showing(text, display, cx);
        } else {
            // The window asked for this turn, not a field: the overlay gate
            // belongs to composer sends and must not eat it (#2795).
            self.send_from_pane(session, text, display, SendOrigin::Window, cx);
        }
    }

    /// Dispatch the one turn a window command asked for, into the session that
    /// ran it (#2672). With no session open the window's own path keeps the
    /// text: it waits in the queue and a conversation is created, exactly as a
    /// composer send with nothing on screen does.
    fn send_command_turn(
        &mut self,
        session: Option<Uuid>,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match session {
            Some(session) => self.send_turn_to(session, text, display, cx),
            None => self.send_message_showing(text, display, cx),
        }
    }

    fn send_message(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_message_showing(text, None, cx);
    }

    /// The same send, with the row history should keep when the text is a
    /// prompt the user must not read (#2756). A command that composes a turn
    /// passes its receipt here, so the `[SYSTEM: ...]` scaffolding stays in
    /// the model's context and never becomes a bubble.
    fn send_message_showing(
        &mut self,
        text: String,
        display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // A send ends the offer (#2708): the buttons belonged to the turn that
        // made them, and leaving them up over a new question would send a
        // stale follow-up into it.
        if let Some(sid) = self.transcript.session {
            agent_bridge::options_take(sid);
        }
        // The composer the text came from: when it sends a recalled message,
        // that message leaves the queue with the text while the rest of its
        // walk goes back to wait (#2637).
        let sent_from = self.composer_entity();
        // Commands are read before every other gate, because the two that
        // matter most are needed exactly when the window is busy or broken:
        // `/stop` has nothing to stop on an idle window, and `/help` is what a
        // user reaches for when the agent is offline. A line that starts with
        // a slash never becomes a turn (#2374).
        if let Some(outcome) = commands::parse_command(&text) {
            self.run_command(outcome, self.transcript.session, cx);
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
            // session open it waits in the window's own list. Busy is read
            // for the conversation on screen, so work running in another one
            // never queues this text behind it (#2616).
            // Up already took the recalled message out of the queue (#2637),
            // so this text is simply queued behind the turn. The messages the
            // walk took before it go back to wait when the recall is released
            // below, after this one, which is the order they were queued in.
            match self.transcript.session {
                Some(sid) => {
                    agent_bridge::queue_push(sid, Self::pending_message(text, display.clone()))
                }
                None => self
                    .unsent
                    .push(Self::pending_message(text, display.clone())),
            }
            self.finish_recall(&sent_from, cx);
            // The composer now says a message is waiting on the turn, the
            // moment it is queued rather than on the next turn event (#2630).
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the message, it fires on Ready. The queue
            // strip is where the user sees it.
            self.unsent
                .push(Self::pending_message(text, display.clone()));
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        };
        // The turn goes to the conversation on screen. With none open there
        // is nothing to write into, so one is created and the text waits in
        // the queue rather than vanishing into a session nobody is looking at.
        let Some(sid) = self.transcript.session else {
            self.unsent
                .push(Self::pending_message(text, display.clone()));
            self.new_session(cx);
            return;
        };
        self.live
            .begin(display.clone().unwrap_or_else(|| text.clone()));
        // A send is the reader asking for the end of the conversation (#2698).
        self.rearm_transcript_follow();
        self.transcript.live_expanded = false;
        self.settle_retries = 0;
        self.settle_deadline = None;
        self.settle_rechecks = 0;
        // The finished turn must add one row to the session before the live
        // copy may be dropped (see `reload_transcript`). Measured on the
        // session's own row total, which never saturates the way the windowed
        // turn count does (#2705).
        self.settle_expect = Some(self.transcript.row_total() + 1);
        // A recalled message is the turn's own text now, so it leaves the
        // queue with it; the messages the walk took before it go back to wait
        // (#2637).
        self.finish_recall(&sent_from, cx);
        self.busy = true;
        self.sync_composer_hint(cx);
        let cancel = tokio_util::sync::CancellationToken::new();
        self.cancel = Some(cancel.clone());
        // Attachments are tracked against the session the same way the TUI
        // does it (`FileService::get_or_create_file`), so the file lands in
        // the session's artifacts, not just in the message text.
        let attached = editor::marker_paths(&text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            sid,
            text,
            display,
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), sid, attached);
        }
        cx.notify();
    }

    /// The two taps a plan waiting on approval offers (#2755).
    ///
    /// They send the literal commands the terminal and Telegram send, through
    /// the composer path that parses them, so a tap and a typed `/execute` are
    /// one code path and never two implementations of an approval.
    fn plan_actions(&mut self, session: Uuid, cx: &mut Context<Self>) -> plan_strip::PlanActions {
        let approve = cx.listener(move |this, _ev: &gpui::ClickEvent, _window, cx| {
            cx.stop_propagation();
            this.send_turn_to(session, "/execute".to_string(), None, cx);
        });
        let discard = cx.listener(move |this, _ev: &gpui::ClickEvent, _window, cx| {
            cx.stop_propagation();
            this.send_turn_to(session, "/discard".to_string(), None, cx);
        });
        plan_strip::PlanActions {
            on_approve: std::rc::Rc::new(approve),
            on_discard: std::rc::Rc::new(discard),
        }
    }

    /// The queue entry for a send: a hidden prompt keeps its display line, so
    /// the queue strip never reads `[SYSTEM: ...]` (#2756).
    fn pending_message(text: String, display: Option<String>) -> agent_bridge::PendingMessage {
        match display {
            Some(shown) => agent_bridge::PendingMessage::system(text, shown),
            None => agent_bridge::PendingMessage::plain(text),
        }
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
        // Pruned even on the busy path: a flash window that closed while a
        // turn was streaming must not keep the map alive.
        let flash_active = self.plan_flash_active(std::time::Instant::now());
        let events = agent_bridge::drain(&self.agent_rx);
        if events.is_empty() {
            // A live turn needs a repaint even when no event arrived: that is
            // what advances the spinner frame and the `(5s ...)` clock. The
            // old early return froze the line between chunks, which read as
            // "nothing is happening" on a slow provider.
            // A pane runs its own turn too (#2614), and its spinner, clock
            // and token counter advance on the same repaint. So does a rail row
            // with work in flight, and a plan waiting for approval keeps
            // pulsing its ring until the owner opens it (#2636).
            if self.busy
                || self.pane_states.values().any(|pane| pane.busy)
                || self
                    .rail_activity_map()
                    .values()
                    .any(|row| row.badge.is_some() || row.ring)
                // A rail flash is an animation of its own: the plan bar has to
                // keep alternating orange and its resting colour while the
                // window is open, and stop the moment it closes.
                || flash_active
            {
                // Only the spinner, the clock and the flash move in this
                // branch, so they repaint on a slower cadence than the poll
                // itself. At 20 Hz this walked every pane's transcript five
                // times as often as the animation needed (#2688).
                self.anim_tick = self.anim_tick.wrapping_add(1);
                if busy_repaint_due(self.anim_tick) {
                    cx.notify();
                }
            }
            return;
        }
        let mut reload: Option<Uuid> = None;
        for ev in events {
            // The tool's follow-up offer is stored here, before the pane
            // filter below, because it belongs to a session and not to a card
            // (#2708). Handling it in the match further down would have sent a
            // split leaf's offer into `fold_pane_progress` with the rest of
            // that leaf's progress, and the pane would never draw it.
            if let agent_bridge::TurnEvent::ProgressFor(
                sid,
                ProgressEvent::SuggestedOptions(items),
                _,
            ) = &ev
            {
                agent_bridge::set_options(*sid, items.clone());
                cx.notify();
            }
            // A split leaf runs its own turn (#2614), so its progress paints
            // into that leaf's card. A sub-agent's session has no leaf and is
            // dropped here rather than painted into somebody else's pane.
            // This filter is also what keeps one conversation's tool rows off
            // another's card (#2616): only the session on screen reaches the
            // window's live turn below.
            if let agent_bridge::TurnEvent::ProgressFor(sid, progress, at) = &ev
                && self.transcript.session != Some(*sid)
            {
                self.fold_pane_progress(*sid, progress.clone(), *at, cx);
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
                    self.sync_composer_hint(cx);
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
                    // The card must drop the line of a chat that no longer has
                    // messages. Only the previews are re-read: a rail refresh
                    // here would reorder rows under the cursor.
                    self.refresh_previews(cx);
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
                // wording the terminal prints (#2469). The line belongs to the
                // session that ran the command, so a pane answers on its own
                // strip; a window-level notice (surface, boot summary) carries
                // no session and stays where it has always been (#2672).
                agent_bridge::TurnEvent::CommandReply { session, text } => {
                    match session {
                        Some(id) => self.push_session_notice(id, text),
                        None => self.live.notices.push(text),
                    }
                    cx.notify();
                }
                // A text command that needs one visible turn: `/plan <query>`,
                // `/execute` seeding the checklist, or `/goal`.
                agent_bridge::TurnEvent::CommandTurn {
                    session,
                    text,
                    display,
                } => {
                    self.send_turn_to(session, text, display, cx);
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
                        // The deleted conversation's turn, if one ran there,
                        // goes with it: the row it belonged to is gone.
                        self.stashed.remove(&id);
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
                    // DB read ran) are dropped, not painted. A pane's own
                    // lookup lands on that pane's strip (#2530).
                    if self.plan_for == Some(session) {
                        self.plan.set_path(path);
                        cx.notify();
                    } else if let Some(pane) = self.pane_states.get_mut(&session)
                        && pane.plan_for == Some(session)
                    {
                        pane.plan.set_path(path);
                        cx.notify();
                    }
                }
                agent_bridge::TurnEvent::BootstrapFailed(e) => {
                    logging::error(&format!("agent offline: {e}"));
                    self.agent_error = Some(e.clone());
                    self.live.notices.push(format!("agent offline: {e}"));
                }
                agent_bridge::TurnEvent::ModelSwitched { session, line } => {
                    if self.models_switching.take() != Some(session) {
                        logging::debug("stale model switch completion ignored");
                        continue;
                    }
                    logging::info(&format!("model switched: {line}"));
                    self.push_session_notice(session, line);
                    // The rail row carries the pair the service answers with,
                    // so a switch is only real once the row is re-read.
                    self.refresh_rail(cx);
                    // The gauge is denominated in the pair that serves the
                    // session now. `ctx_max` used to be recomputed only on
                    // `Ready` and on open, so a switch left the old provider's
                    // window on screen: the number was the one thing that
                    // would have proved the pick landed, and it never moved.
                    // The last prompt size came from the old provider's
                    // tokenizer, so it goes with it (#2644).
                    if self.transcript.session == Some(session)
                        && let Some(b) = agent_bridge::bridge()
                    {
                        self.ctx_max = b.service.context_limit_for_session(session);
                        self.ctx_tokens = None;
                    }
                    // A pick is a commitment: apply it, say so once, and put
                    // the user back in the conversation that asked. A stale
                    // completion never closes a different screen.
                    if self.screen == Screen::Models && self.models_session == Some(session) {
                        self.screen = Screen::Chat;
                        if self.transcript.session == Some(session) {
                            self.pane_focus = None;
                        } else if self
                            .panes
                            .as_ref()
                            .is_some_and(|tree| tree.contains(session))
                        {
                            self.pane_focus = Some(session);
                        }
                        self.sync_composer_session();
                        self.refocus_screen();
                    }
                    cx.notify();
                }
                agent_bridge::TurnEvent::ModelSwitchFailed { session, error } => {
                    if self.models_switching.take() != Some(session) {
                        logging::debug("stale model switch failure ignored");
                        continue;
                    }
                    logging::error(&format!("model switch: {error}"));
                    self.push_session_notice(session, format!("model switch: {error}"));
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
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::Thinking, _) => {
                    self.live.thinking = true;
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::StreamingChunk { text },
                    at,
                ) => {
                    // Counted, not just appended: the live line's token number
                    // comes from the chunk itself, so it moves as the answer
                    // streams instead of appearing with the final answer.
                    //
                    // Stamped with its ARRIVAL (#2703): `drain` hands this loop
                    // a whole batch, so reading the clock here would stamp every
                    // chunk in the batch with one instant and collapse the
                    // window the rate is measured over.
                    self.live.thinking = false;
                    self.live.note_chunk_at(&text, at);
                    // The rail's preview follows the answer as it streams, at
                    // most one batched read a second.
                    self.refresh_previews_if_live(cx);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::IntermediateText { text, reasoning },
                    at,
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
                        self.live.push_reasoning_at(&reasoning, at);
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
                    _,
                ) => {
                    self.live.thinking = false;
                    // The ledger's own wording (`tui::app::messaging`), so a
                    // live row reads exactly like the row that persists.
                    let description =
                        opencrabs::tui::app::App::format_tool_description(&tool_name, &tool_input);
                    self.live
                        .push_tool(transcript::ToolCall::running(tool_name, description));
                    // A silent tool run still moves the preview's tool line, so
                    // the same throttled read the chunk path uses is asked for
                    // here too. Without it a long build leaves the rail frozen
                    // for exactly the sessions that are working longest (#762's
                    // lesson, applied to the preview).
                    self.refresh_previews_if_live(cx);
                }
                agent_bridge::TurnEvent::ProgressFor(
                    _,
                    ProgressEvent::ToolCompleted {
                        tool_name, success, ..
                    },
                    _,
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
                    _,
                ) if self.transcript.session == Some(sid) => {
                    self.live.injected.push(text);
                    cx.notify();
                }
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::TokenCount(count), _) => {
                    self.ctx_tokens = Some(count.min(u32::MAX as usize) as u32);
                }
                // A detached command's row lands synchronously, while the
                // turn that spawned it is still live, and its finish finds it
                // again by `task_id` (#2146). Keyed by id, never by position:
                // two parallel calls would otherwise settle each other's rows.
                agent_bridge::TurnEvent::ProgressFor(
                    sid,
                    ProgressEvent::DetachedTaskStarted { task_id, label },
                    _,
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
                    _,
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
                // A plan just closed (#2255, #2691). The settle emits this
                // before the plan file is archived, so it is the one signal
                // that cannot be missed by a file read. The chime is opt-in
                // and feature-gated, and no toast or desktop notification is
                // raised, exactly as the terminal's arm.
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::PlanCompleted { .. }, _) => {
                    Self::play_task_chime();
                }
                // Compaction notices and queue echoes: the phase-2 transcript
                // mirror. They persist to the DB and show on the reload;
                // nothing to draw live yet. The follow-up offer is already
                // stored further up this loop (#2708), so it reaches this
                // catch-all spent.
                agent_bridge::TurnEvent::ProgressFor(_, _, _) => {}
                agent_bridge::TurnEvent::Done {
                    session,
                    content,
                    context_tokens,
                    tokens_per_second,
                } => {
                    // The card's live line belongs to this turn (#2718), so
                    // it retires with it: the preview under it is the ledger's
                    // and stops representing the turn the moment it settles.
                    self.rail_live.remove(&session);
                    // A settled turn is what moves a row up the rail (#2666).
                    // The ledger bumps `updated_at` beside the message insert
                    // and the rail paints that order, so without a refetch
                    // here a conversation used a minute ago kept the slot and
                    // the date it had when the rail was last read.
                    self.refresh_rail(cx);
                    // A split pane runs its own turn (#2614): the answer
                    // belongs to that leaf, so the window's footer and its
                    // conversation are left exactly where the user put them.
                    if self.transcript.session != Some(session) {
                        // The turn finished in a conversation that is not on
                        // screen: its parked card retires here, and the rows
                        // in the ledger are what drawing that session reads
                        // when the user opens it again (#2616). A split pane's
                        // own turn settles in its leaf.
                        self.stashed.remove(&session);
                        self.settle_pane(session, tokens_per_second, cx);
                        // A sub-agent's turn ends the same way and has no pane
                        // to settle (#2636). Clearing is idempotent, so a leaf
                        // that was never recorded costs nothing.
                        if self.rail.clear(session) {
                            cx.notify();
                        }
                        continue;
                    }
                    // The provider's own prompt size for the call that just
                    // answered: the honest number for the ctx footer, not an
                    // estimate of what we think we sent.
                    self.ctx_tokens = Some(context_tokens);
                    // The same numbers, frozen for THIS turn, so the settled
                    // card can print what the turn cost instead of whatever
                    // the session's budget reads later (#2324).
                    if let Some(sid) = self.transcript.session {
                        self.pending_metrics.insert(
                            sid,
                            transcript::TurnMetrics {
                                ctx_used: context_tokens,
                                ctx_max: self.ctx_max,
                                tps: tokens_per_second,
                                tool_calls: self.live.tools.len(),
                            },
                        );
                    }
                    // Freeze the turn's rate on the same measure every other
                    // surface uses: the service's own tokens_per_second when
                    // it reported one, else the local active-streaming
                    // estimate. The footer and the settled card then agree.
                    // Captured before finalize, which resets the accumulator:
                    // the window is what turns a token count into a rate, and
                    // a log that prints the count alone cannot tell a short
                    // window from an inflated numerator (#2703).
                    let active_secs = self.live.streaming_active_secs();
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
                        "turn done ({} chars, {} chunks, {} bytes, first {:?}, {}s elapsed, {:.2}s active, {} tok, rate {:.1} tok/s, service {:?}), reloading transcript",
                        content.len(),
                        self.live.chunks,
                        self.live.chunk_bytes,
                        self.live.first_chunk,
                        self.live.elapsed_secs(),
                        active_secs,
                        self.live.tokens,
                        self.live.tokens_per_sec(),
                        tokens_per_second
                    ));
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::Failed { session, text } => {
                    // Same as Done: the live line was this turn's, so it goes
                    // with it (#2718).
                    self.rail_live.remove(&session);
                    // A failure is still activity, so the row moves like a
                    // settled turn does (#2666).
                    self.refresh_rail(cx);
                    if self.transcript.session != Some(session) {
                        // Same as Done: a failure for a conversation that is
                        // not on screen retires its parked card and leaves the
                        // visible turn untouched (#2616).
                        self.stashed.remove(&session);
                        self.fail_pane(session, text, cx);
                        // A failed sub-agent turn is still a finished turn, so
                        // its rail badge has to go with it (#2636).
                        if self.rail.clear(session) {
                            cx.notify();
                        }
                        continue;
                    }
                    // A card a stop already took back has no line to write on:
                    // the cancellation landing afterwards is the echo of the
                    // stop, not news (#2635).
                    if !self.live.is_empty() {
                        self.live.notices.push(text);
                    }
                    // A failed turn still ran, so it keeps the numbers it
                    // actually measured instead of showing a bare clock while
                    // its neighbours carry ctx and rate (#2705). The record is
                    // only written when the provider streamed something: a
                    // failure before the first call has no reading of its own,
                    // and borrowing the previous turn's budget would be a lie.
                    self.pending_metrics.remove(&session);
                    self.live.finalize(None);
                    if self.live.tokens > 0
                        && let Some(used) = self.ctx_tokens
                    {
                        self.pending_metrics.insert(
                            session,
                            transcript::TurnMetrics {
                                ctx_used: used,
                                ctx_max: self.ctx_max,
                                tps: Some(f64::from(self.live.tokens_per_sec()))
                                    .filter(|t| *t > 0.0),
                                tool_calls: self.live.tools.len(),
                            },
                        );
                    }
                    self.busy = false;
                    self.live.thinking = false;
                    reload = self.transcript.session;
                }
                agent_bridge::TurnEvent::Cancelled { session } => {
                    // A stop ends the turn, so the card's live line does too
                    // (#2718).
                    self.rail_live.remove(&session);
                    // A stop dropped the turn where it stood (#2657), so no
                    // Done and no Failed follows. This is the settle, and it
                    // carries no text: whoever asked for the stop already said
                    // "Operation cancelled", so a second line would be noise.
                    self.refresh_rail(cx);
                    self.pending_metrics.remove(&session);
                    // The verdict needs a row (#2811). A cancelled turn writes
                    // no error and no answer, so without this the turn settles
                    // as Finished and the stop vanishes on the next reload.
                    // Written for the cancelled session, whichever one is on
                    // screen, because the turn was real where it ran.
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_record_turn_stopped(bridge.service.clone(), session);
                    }
                    if self.transcript.session == Some(session) {
                        // No reload on purpose. The dropped future cannot
                        // promise the ledger has the text it streamed, and a
                        // re-read now would wipe what the user can still see.
                        // The card keeps its stream, with the clock and rate
                        // frozen where the stop landed.
                        self.live.finalize(None);
                        self.busy = false;
                        self.live.thinking = false;
                        // The stop is the settle (#2811): the card keeps its
                        // stream and now says it was stopped, instead of
                        // waiting on a spinner that will never turn again.
                        self.live.stopped = true;
                        self.cancel = None;
                        logging::info("turn settled after the stop dropped it");
                    } else {
                        // A split leaf: same settle inside its own card, and
                        // the parked copy of a switched-away turn retires.
                        self.stashed.remove(&session);
                        if let Some(pane) = self.pane_states.get_mut(&session) {
                            pane.live.finalize(None);
                            pane.live.thinking = false;
                            pane.live.stopped = true;
                            pane.busy = false;
                            pane.cancel = None;
                        }
                        // The stop is acknowledged: the leaf's queue becomes
                        // the next turn now, never while the cancel unwinds
                        // (#2695, the #2631 ordering the window keeps).
                        self.drain_pane_pending(session, cx);
                    }
                    // The turn is gone, so its rail spinner goes with it.
                    self.rail.clear(session);
                    // Unconditional (#2811): a card that just stopped has to
                    // paint its stop glyph once. It is static from here, so
                    // this is the only repaint it needs, and the animation
                    // gate stays off instead of repainting an idle window five
                    // times a second for a turn nobody is running.
                    cx.notify();
                }
                agent_bridge::TurnEvent::TurnReverted { session } => {
                    // The stopped turn's rows are deleted and its question is
                    // back in the composer, so the transcript must stop drawing
                    // the bubble the user just took back (#2635).
                    if self.transcript.session == Some(session) {
                        reload = Some(session);
                    }
                }
                agent_bridge::TurnEvent::TurnResumed {
                    session,
                    user_message,
                    cancel,
                } => {
                    // The last process died inside this turn and it runs again
                    // now (#2627). The card has to say so from the first frame:
                    // a recovered turn that painted into a window believing it
                    // was idle would look exactly like the work being lost.
                    if self.transcript.session == Some(session) {
                        self.live = transcript::LiveTurn::default();
                        // The ledger already holds this question: the process
                        // that asked it died after writing the row. Resuming is
                        // not a fresh send, and starting it as one stamped the
                        // resume time, missed the row in the identity window,
                        // and painted the question twice (#2740).
                        self.live.resume(user_message);
                        self.busy = true;
                        self.cancel = Some(cancel);
                        // No reload here on purpose: the turn's rows are
                        // already in the ledger and a reload landing now would
                        // fight the card it is about to fill.
                    } else {
                        // Recovered in a conversation nobody is looking at: it
                        // parks under its own session, which is where a switch
                        // brings it back whole (#2616).
                        let mut live = transcript::LiveTurn::default();
                        // Same rule as the on-screen branch: the parked copy
                        // draws a row the ledger already has, so it starts as a
                        // resumed turn, not a fresh one (#2740).
                        live.resume(user_message);
                        self.stashed.insert(
                            session,
                            StashedTurn {
                                live,
                                cancel: Some(cancel),
                                settle_expect: None,
                                settle_retries: 0,
                                ctx_tokens: None,
                                // Recovered mid-flight, so the turn is running.
                                busy: true,
                            },
                        );
                    }
                    self.refresh_rail(cx);
                    cx.notify();
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
                    // The completion belongs to the session that started the
                    // task, never to whatever conversation happens to be on
                    // screen. The terminal resumes that session wherever the
                    // user is looking (`tui/app/state.rs`), and the window now
                    // does the same (#2704): a session already working takes
                    // the receipt in its queue, and an idle one starts its turn
                    // and parks it in the stash when it is not on screen.
                    if self.running_here(session_id) {
                        // Queue it against the session the task belongs to, in
                        // the store the agent drains, so it goes out between
                        // tool calls instead of waiting for the turn to end
                        // (#2629). `QueuedUserMessage::join` keeps the system
                        // half out of what the user "said" (#765).
                        agent_bridge::queue_push(
                            session_id,
                            agent_bridge::PendingMessage::system(context_text, display_text),
                        );
                    } else if self.transcript.session == Some(session_id) {
                        self.resume_finished_task(session_id, context_text, display_text, cx);
                    } else {
                        self.resume_off_screen_task(session_id, context_text, display_text, cx);
                    }
                }
            }
        }
        if let Some(sid) = reload {
            self.reload_transcript(sid, cx);
        }
        // A finished turn is the moment the queue drains: before this it only
        // drained on `Ready`, so a message typed while busy never went out.
        // The hint follows the turn and the queue, including a report that
        // landed mid-turn, so it is repainted on every event batch (#2630).
        self.sync_composer_hint(cx);
        if !self.busy {
            self.cancel = None;
            self.drain_pending(cx);
        }
        self.follow_chat();
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
        self.running_tasks_label_for(session)
    }

    /// The same roster for ANY session, not just the open conversation, so every
    /// pane prints its own detached commands above its composer (#2593).
    fn running_tasks_label_for(&self, session: Uuid) -> Option<String> {
        let bridge = agent_bridge::bridge()?;
        let mgr = bridge.service.background_manager()?;
        let tasks: Vec<(String, u64)> = mgr
            .running_tasks(session)
            .into_iter()
            .map(|t| (t.label, t.started.elapsed().as_secs()))
            .collect();
        opencrabs::tui::render::format_background_tasks(&tasks, opencrabs::tui::render::LABEL_CHARS)
    }

    /// The context-menu popover for one rail row (or topic chip), built only
    /// when that id's menu is open. Shared by full rows and chips so a chip's
    /// right-click offers the same pin / rename / split / delete picks.
    ///
    /// The card opens at the press that asked for it (#2690). Rows and chips
    /// both record their press point, so the card lands under the pointer
    /// rather than beside the rail: a chip sits mid-row, and the row's right
    /// edge is not where the click happened.
    fn rail_menu_popover_for(&self, id: Uuid, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.sessions.rail_menu != Some(id) {
            return None;
        }
        let favorite = self.sessions.is_favorite(id);
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
                        sessions::MenuKind::Rename => this.start_rename(id, cx),
                        sessions::MenuKind::EnableTopics => this.enable_topics(id, cx),
                        sessions::MenuKind::NewTopic => {
                            // The prompt works on the open chat, so open the row first (#2397).
                            this.open_session(id, cx);
                            this.start_topic_draft(cx);
                        }
                        sessions::MenuKind::GroupTopics => this.start_group_topics(id, cx),
                        sessions::MenuKind::SplitSide => this.split_row(id, Layout::Side, cx),
                        sessions::MenuKind::SplitStack => this.split_row(id, Layout::Stack, cx),
                        sessions::MenuKind::Delete => this.request_delete(id, cx),
                    }
                });
                sessions::menu_item(entry.label).on_click(pick)
            })
            .collect::<Vec<_>>();
        // The press point is the anchor: the card opens under the pointer
        // that asked for it, whichever row or chip that was (#2690).
        let at = self
            .rail_menu_at
            .unwrap_or(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        Some(sessions::menu_popover("rail-context-menu", items, at))
    }

    /// Drop every composer's context menu: a press outside the card is the
    /// press that closes it (#2749). Every field is checked, because a split
    /// pane has its own and only one of them opened the card.
    fn close_composer_menus(&mut self, cx: &mut Context<Self>) {
        let mut editors = vec![self.editor.clone()];
        editors.extend(self.pane_states.values().map(|pane| pane.editor.clone()));
        for editor in editors {
            editor.update(cx, |editor, cx| editor.close_context_menu(cx));
        }
    }

    /// The rail's badges for every row it is about to draw (#2636).
    ///
    /// Resolved in one pass before the rows are built: the plan index needs
    /// `&mut self` and the row iterator holds `&self`, so the two cannot
    /// interleave. Evidence is cached by mtime, so this costs one `stat` per
    /// session that has a plan file and nothing for the rest.
    fn rail_activity_map(&mut self) -> HashMap<Uuid, rail_activity::RowState> {
        let rows: Vec<Uuid> = self.sessions.ordered().map(|s| s.id).collect();
        // The sweep throttle reads a moving clock; the animations read the
        // window's fixed origin. Two different questions, two clocks.
        let now = std::time::Instant::now();
        // One bridge lookup for the whole pass, not one per row: a detached
        // command outlives its turn, so it is the only trace that session still
        // has work in flight (#762).
        let manager = agent_bridge::bridge().and_then(|b| b.service.background_manager());
        let mut map = HashMap::with_capacity(rows.len());
        for id in rows {
            let live = self.rail.is_live(
                id,
                // The open session's turn, plus any turn parked under its own
                // session: a switched-away turn keeps running, so its rail row
                // must keep its spinner instead of claiming idle work (#2616).
                // Split leaves keep their own flag. A parked card that has
                // ended is NOT live (#2811): the entry outlives the turn so the
                // stop survives the switch, and reading presence as work would
                // spin a row for a turn nobody is running.
                (self.busy && self.transcript.session == Some(id))
                    || self.stashed.get(&id).is_some_and(|turn| turn.busy),
                self.pane_states.get(&id).is_some_and(|pane| pane.busy),
                manager.as_ref().is_some_and(|mgr| mgr.running_for(id) > 0),
            );
            let plan = self.rail_index.evidence(id, live, now);
            let seen = plan.is_some_and(|evidence| self.rail_seen.is_seen(id, evidence.stamp));
            map.insert(id, rail_activity::row_state(live, plan, seen));
        }
        map
    }

    /// The rail card's plan progress for every row it is about to draw:
    /// `(done, total)` for an approved plan with a checklist, nothing else.
    ///
    /// A separate pass from the badges because the idle repaint check only
    /// asks about animation, and this question is static state. The index
    /// already cached the paths and documents for the badge pass, so each row
    /// here costs one `stat` at most and a parse only when the file moved.
    /// Snapshot every session's plan progress before the rail filters (#2719).
    /// The plan chip in `visible()` reads this map, so it must hold a row's
    /// counts even when that row is about to be hidden: filling it from the
    /// filtered `ordered()` would chase its own tail. The index resolves on
    /// demand, so iterating all items warms it for the badge pass that follows.
    fn snapshot_rail_plan_progress(&mut self) {
        let now = std::time::Instant::now();
        let ids: Vec<Uuid> = self.sessions.items.iter().map(|s| s.id).collect();
        self.sessions.plan_progress.clear();
        for id in ids {
            if let Some(progress) = self.rail_index.progress(id, now) {
                self.sessions.plan_progress.insert(id, progress);
            }
        }
    }

    /// The plan bar's counts for every rail row, taken from the snapshot the
    /// frame already built.
    ///
    /// Resolving them a second time walked every session again, one `stat` and
    /// doc lookup each, for a map the frame already had (#2805). The snapshot
    /// is keyed on all sessions, so it covers every row `ordered()` can yield.
    fn rail_plan_progress_map(&self) -> HashMap<Uuid, (usize, usize)> {
        self.sessions.plan_progress.clone()
    }

    /// The detached commands every rail row is running, formatted once per
    /// frame: `⏳ cargo test 32s` per session, `None` when nothing runs.
    ///
    /// The rail card answers "is anything still going in that conversation"
    /// without opening it, which is the one fact a streaming spinner cannot
    /// carry: a detached command takes the turn idle, so the row would
    /// otherwise read as finished while a build grinds on (#762).
    ///
    /// Shaped by [`opencrabs::tui::render::format_background_tasks`], the same
    /// formatter the composer strip and the terminal's footer use, so one
    /// running task cannot read three different ways. One manager lookup for
    /// the whole pass, never one per row.
    fn rail_running_map(&self) -> HashMap<Uuid, SharedString> {
        let Some(manager) = agent_bridge::bridge().and_then(|b| b.service.background_manager())
        else {
            return HashMap::new();
        };
        let mut map = HashMap::new();
        for id in self.sessions.ordered().map(|s| s.id) {
            let tasks: Vec<(String, u64)> = manager
                .running_tasks(id)
                .into_iter()
                .map(|t| (t.label, t.started.elapsed().as_secs()))
                .collect();
            if let Some(line) = opencrabs::tui::render::format_background_tasks(
                &tasks,
                opencrabs::tui::render::LABEL_CHARS,
            ) {
                map.insert(id, SharedString::from(format!("\u{23f3} {line}")));
            }
        }
        map
    }

    /// One pane's footer text (#2593): pane index, whether that session is
    /// mid-turn, who is answering it, and its plan progress. The detached
    /// commands ride the strip above the composer instead, so the one-line
    /// footer never grows past the pane. Resolved here, once per frame,
    /// because it reads the bridge and the plan state, neither of which
    /// belongs in a draw pass per pane.
    fn pane_footer_text(&self, session: Uuid, index: usize, count: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        if count > 1 {
            parts.push(format!("[{index}/{count}]"));
        }
        if self.busy && self.transcript.session == Some(session) {
            // The GUI runs one conversation at a time, so the only pane that
            // can be mid-turn is the open one.
            parts.push("⠙ working".to_string());
        }
        if let Some(bridge) = agent_bridge::bridge() {
            let (provider, model) = bridge.service.provider_pair_for_session(session);
            if !model.is_empty() {
                let label = model.rsplit('/').next().unwrap_or(&model).to_string();
                parts.push(format!("{provider} / {label}"));
            }
        }
        // The detached commands are NOT repeated here: each pane prints its
        // own strip above the composer, right-aligned in the palette's cyan
        // (#2617). Printing the same label a second time in a one-line footer
        // made it run past the pane's edge and over its neighbour.
        if self.plan_for == Some(session)
            && let Some(progress) = self.plan.progress_label()
        {
            parts.push(progress);
        }
        parts.join("  ·  ")
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
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        };
        self.live.begin(display_text.clone());
        // A new turn re-arms the follow, the terminal's own rule (#2698).
        self.rearm_transcript_follow();
        self.transcript.live_expanded = false;
        self.settle_retries = 0;
        self.settle_deadline = None;
        self.settle_rechecks = 0;
        self.settle_expect = Some(self.transcript.row_total() + 1);
        self.busy = true;
        self.sync_composer_hint(cx);
        let cancel = tokio_util::sync::CancellationToken::new();
        self.cancel = Some(cancel.clone());
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session_id, attached);
        }
        cx.notify();
    }

    /// Whether `session` already has a turn in flight in this window: the
    /// conversation on screen while it is busy, a split leaf still working, or
    /// a turn parked in the stash (#2704).
    ///
    /// A completion for one of those joins the session's queue instead of
    /// starting a second turn over it, which is the rule the terminal applies
    /// to its own per-session state.
    fn running_here(&self, session: Uuid) -> bool {
        if self.transcript.session == Some(session) && self.busy {
            return true;
        }
        if self.pane_states.get(&session).is_some_and(|p| p.busy) {
            return true;
        }
        // A parked card that has ended is not work in flight (#2811). Its
        // entry outlives the turn so the stop survives a switch, and reading
        // presence as work would queue the completion instead of resuming it.
        self.stashed.get(&session).is_some_and(|turn| turn.busy)
    }

    /// Start the turn a completion asks for on a conversation nobody is
    /// looking at, parking its card under that session (#2704).
    ///
    /// The terminal resumes the owning session wherever the user is looking,
    /// so the window does too instead of logging the receipt away. The parked
    /// turn is the same shape a switch leaves behind (#2616): `swap_visible_turn`
    /// brings it back whole when that conversation is opened.
    fn resume_off_screen_task(
        &mut self,
        session_id: Uuid,
        context_text: String,
        display_text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: key it to the session it finished on, so it
            // resumes when the stack is ready (#2629).
            agent_bridge::queue_push(
                session_id,
                agent_bridge::PendingMessage::system(context_text, display_text),
            );
            return;
        };
        let mut live = transcript::LiveTurn::default();
        live.begin(display_text.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        self.stashed.insert(
            session_id,
            StashedTurn {
                live,
                cancel: Some(cancel.clone()),
                // The rows land under a conversation that is not on screen, so
                // no reload has to be proven here; opening it reads them.
                settle_expect: None,
                settle_retries: 0,
                ctx_tokens: None,
                // A task completion just started this turn.
                busy: true,
            },
        );
        let attached = editor::marker_paths(&context_text);
        agent_bridge::spawn_turn(
            bridge.service.clone(),
            session_id,
            context_text,
            Some(display_text.clone()),
            self.sink.clone(),
            cancel,
        );
        if !attached.is_empty() {
            agent_bridge::spawn_track_files(self.db_path.clone(), session_id, attached);
        }
        // The rail says the session is working from the first frame, the same
        // signal a visible turn gives (#2666, #2636).
        self.refresh_rail(cx);
        cx.notify();
    }

    /// What the composer says about the state it is in: nothing running, a turn
    /// running, a queue waiting on it, or a queue already on its way. The words
    /// live under the caret because that is where the user is typing, and the
    /// queue card is a list, not an instruction manual (#2630).
    fn composer_hint(&self) -> SharedString {
        composer_hint_for(self.busy, self.queued_for_strip().len())
    }

    /// Paint the hint on every composer: the focused field and each split
    /// pane's own, so a pane that takes the keyboard already says the right
    /// thing. Only a real state change touches them (#2630).
    fn sync_composer_hint(&mut self, cx: &mut Context<Self>) {
        let hint = self.composer_hint();
        if self.hint_painted.as_ref() == Some(&hint) {
            return;
        }
        self.hint_painted = Some(hint.clone());
        self.editor
            .update(cx, |ed, cx| ed.set_placeholder(hint.clone(), cx));
        let panes: Vec<_> = self
            .pane_states
            .values()
            .map(|pane| pane.editor.clone())
            .collect();
        for editor in panes {
            editor.update(cx, |ed, cx| ed.set_placeholder(hint.clone(), cx));
        }
    }

    /// A composer just sent its field. The field holds every message its recall
    /// took, joined into one edit, so the whole text is the one going out and
    /// nothing goes back to wait. Other composers release their whole recall
    /// (#2637, #2696).
    fn finish_recall(&mut self, sent: &Entity<editor::Editor>, cx: &mut Context<Self>) {
        let mut editors = vec![self.editor.clone()];
        editors.extend(self.pane_states.values().map(|pane| pane.editor.clone()));
        for editor in editors {
            let keep = editor.entity_id() == sent.entity_id();
            let returned = editor.update(cx, |ed, _| ed.release_recall(keep));
            if let Some((session, msgs)) = returned {
                for msg in msgs {
                    agent_bridge::queue_recall_return(session, msg);
                }
            }
        }
    }

    /// What the strip shows, oldest first: the text waiting on the open
    /// session, then anything typed before a session existed. One store backs
    /// both, so what the user reads is exactly what will send (#2629).
    fn queued_for_strip(&self) -> Vec<agent_bridge::PendingMessage> {
        let mut out = self.unsent.clone();
        if let Some(sid) = self.transcript.session {
            out.extend(agent_bridge::queue_snapshot(sid));
        }
        out
    }

    /// The offered labels as drawable rows, each minted with the session it
    /// belongs to so a pick sends into that conversation and nowhere else
    /// (#2708). The offer is taken as the click lands: the buttons belong to
    /// the turn that made them, so a second press must not send a stale one.
    fn option_rows(
        &self,
        session: Uuid,
        offers: Vec<SuggestionItem>,
        cx: &mut Context<Self>,
    ) -> Vec<(SuggestionItem, transcript::ClickHandler)> {
        offers
            .into_iter()
            .map(|item| {
                let label = item.label.clone();
                let pick: transcript::ClickHandler = Rc::new(cx.listener(
                    move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        agent_bridge::options_take(session);
                        this.send_turn_to(session, label.clone(), None, cx);
                    },
                ));
                (item, pick)
            })
            .collect()
    }

    fn drain_pending(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        // Boot text has no session to live in yet. It joins the open one here,
        // so there is never a second list the agent cannot see (#2629).
        if let Some(sid) = self.transcript.session
            && !self.unsent.is_empty()
        {
            for msg in std::mem::take(&mut self.unsent) {
                agent_bridge::queue_push(sid, msg);
            }
        }
        let waiting = match self.transcript.session {
            Some(sid) => agent_bridge::queue_snapshot(sid),
            None => self.unsent.clone(),
        };
        if waiting.is_empty() {
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            // Still booting: keep the text. Clearing it here dropped whatever
            // the user typed whenever the window polled before the stack came
            // up (#2629).
            return;
        };
        // No open conversation: keep the queue and open one. The queue drains
        // again when that session lands (`SessionCreated`).
        let Some(sid) = self.transcript.session else {
            self.new_session(cx);
            return;
        };
        let msgs = agent_bridge::queue_take(sid);
        if let Some(joined) = agent_bridge::PendingMessage::join(&msgs) {
            // The flush receipt: how many queued messages ride out in this
            // one turn, so a first-only report can be read against the pushes
            // and takes that precede it (#2695).
            logging::info(&format!(
                "queue flush: session {sid}, sending {} queued as one turn",
                msgs.len()
            ));
            self.live.begin(joined.display_text.clone());
            // A flushed queue is a send, so it follows its own answer (#2698).
            self.rearm_transcript_follow();
            self.transcript.live_expanded = false;
            self.settle_retries = 0;
            self.settle_deadline = None;
            self.settle_rechecks = 0;
            self.settle_expect = Some(self.transcript.row_total() + 1);
            self.busy = true;
            self.sync_composer_hint(cx);
            let cancel = tokio_util::sync::CancellationToken::new();
            self.cancel = Some(cancel.clone());
            let attached = editor::marker_paths(&joined.context_text);
            agent_bridge::spawn_turn(
                bridge.service.clone(),
                sid,
                joined.context_text,
                Some(joined.display_text),
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
        //
        // It inherits the pair from the conversation on screen, not from the
        // global slot. The global slot follows the config `enabled` flags and
        // every config reload swaps it (agent_bridge provider_hook), so
        // reading it here started each new conversation on a provider the
        // user had already switched away from (#2643).
        let active = self.focused_pane();
        let (provider, model) = match agent_bridge::bridge() {
            Some(b) => match active {
                Some(sid) => (
                    Some(b.service.provider_name_for_session(sid)),
                    Some(b.service.provider_model_for_session(sid)),
                ),
                None => (
                    Some(b.service.provider_name()),
                    Some(b.service.provider_model()),
                ),
            },
            None => (None, None),
        };
        agent_bridge::spawn_create_session(
            self.db_path.clone(),
            self.sink.clone(),
            provider,
            model,
        );
        // A new conversation is about to be typed into, so the rail gives the
        // keyboard up even when the click came from its own button (#2665).
        self.rail_focused = false;
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
        self.enable_topics_then(id, |_, _| {}, cx);
    }

    /// Turn topics on, then run `after` once the read-back has landed (#2604).
    ///
    /// The seeker's pick can name a conversation that never had topics on, and
    /// a prompt opened before the flag reaches `sessions.topics` is dropped by
    /// the same guard the rail menu never hits. `after` runs inside the update
    /// that stores the read-back, so it cannot outrun the flag it depends on.
    fn enable_topics_then(
        &mut self,
        id: Uuid,
        after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
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
                    Ok(ids) => {
                        this.sessions.topics = ids.into_iter().collect();
                        after(this, cx);
                    }
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

    /// Arm the delete dialog. Every delete path (a row's menu, and nothing
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
        // A deleted conversation must not keep a draft waiting for a composer
        // it can never reach. Its pane's editor leaves with the conversation.
        self.drafts.remove(id);
        self.pane_states.remove(&id);
        // The pin is part of the row: it leaves with the row.
        self.sessions.favorites.retain(|f| *f != id);
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
        // The dialog opens on the session's own icon, so a rename that never
        // touches the picker keeps it (#2791). No stored icon means None, and
        // the default the badge shows is never written back over nothing.
        self.topic_draft_icon = self.sessions.topic_icons.get(&id).cloned();
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

    /// The session a new topic hangs under (#2686): the leaf that owns the
    /// keyboard, or the chat leaf when the window is unsplit. A split used to
    /// create the topic under `transcript.session` whatever pane was focused,
    /// and refused outright when that leaf had topics off.
    fn topic_parent(&self) -> Option<Uuid> {
        self.split_focus().or(self.transcript.session)
    }

    /// The conversation a new topic would hang under, or `None` when nothing
    /// on screen can hold one (#2605).
    ///
    /// [`Self::topic_parent`] answers "which pane asked". This answers "can
    /// that pane hold a topic", and the two used to disagree: the menu offered
    /// New topic because the chat leaf had topics on, the draft then bailed
    /// because the focused pane did not, the menu closed, and the typed name
    /// went to the composer and got sent as a message. A prompt that cannot
    /// create anything must never open, so both the row and the prompt resolve
    /// through here.
    fn topic_draft_parent(&self) -> Option<Uuid> {
        if let Some(id) = self.topic_parent()
            && self.sessions.is_topic_enabled(id)
        {
            return Some(id);
        }
        self.transcript
            .session
            .filter(|id| self.sessions.is_topic_enabled(*id))
    }

    /// The chat's menu (#2480): what can be done to the conversation it
    /// belongs to. It opens from a right press on the conversation or on a
    /// topic tab; the header button that used to open it went with the title
    /// row (#2596).
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
        if self.topic_draft_parent().is_some() {
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
        // Under the press that opened it (#2690), through the same popover the
        // rail uses. The card used to sit at the column's top corner, which is
        // where the header used to be: not where the user clicked, and in a
        // split not even the pane whose tab was pressed.
        let at = self
            .topic_menu_at
            .unwrap_or(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        Some(sessions::menu_popover("chat-menu", items, at))
    }

    /// The tab strip for the open chat's topic family (#2400, #2480): `All`
    /// first, then each of its topics in list order. The open one is marked,
    /// and a click opens that session.
    ///
    /// The conversation's own name is not a chip. It used to lead the strip
    /// (`vec![root]`), which put the session in the row of its own topics and
    /// made it read as a sibling. The name belongs to the card above, and
    /// `All` is the way back to it.
    ///
    /// `pane` names the leaf the strip is drawn in, `None` for the unsplit
    /// window (#2686). Every read and every click resolves against it, so a
    /// pane's strip shows that pane's family and a click moves that pane.
    fn topic_tabs(&self, pane: Option<Uuid>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = pane.or(self.transcript.session) else {
            return Vec::new();
        };
        let root = self.sessions.parents.get(&open).copied().unwrap_or(open);
        if !self.sessions.is_topic_enabled(root) {
            return Vec::new();
        }
        // The topic store outranks the stored session name (#2490): a rename
        // in Telegram reaches this label on the next refresh pass. With no live
        // name yet, the chip strips the row's platform prefix and channel key
        // instead of printing them (#2762).
        let name = |id: Uuid| {
            let stored = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref());
            rail::chip_name(
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
        // A wide family folds its tail into one pill, and the open topic keeps
        // its chip either way (#2762).
        let chips: Vec<topic_bar::TopicChip> = topics
            .iter()
            .map(|id| topic_bar::TopicChip {
                id: *id,
                activity: self.topic_is_busy(*id),
            })
            .collect();
        let plan = topic_bar::bar_plan(root, open, &chips, topic_bar::BAR_TOPICS_SHOWN);
        let mut cells: Vec<AnyElement> = Vec::with_capacity(plan.entries.len() + 1);
        for entry in plan.entries {
            let id = entry.session(root);
            let current = entry.is_open(root, open);
            let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                match pane {
                    Some(leaf) => this.open_topic_in_pane(leaf, id, cx),
                    None => this.open_session(id, cx),
                }
            });
            // A right press on a tab opens the same menu the `...` draws,
            // for the session that tab stands for (#2480). Before this it
            // opened nothing at all, and the only way to edit a topic was
            // to open it first and hunt the menu.
            let open_menu = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.topic_menu = Some(id);
                // The press point, so the card opens under the click rather
                // than at the column's top corner (#2690).
                this.topic_menu_at = Some(ev.position);
                cx.notify();
            });
            // A busy topic is marked with a dot, the same signal the rail uses
            // for work in flight.
            let activity = chips.iter().any(|chip| chip.id == id && chip.activity);
            // `All` has the same icon slot and cell geometry as a topic.
            let (label, picture) = match entry {
                topic_bar::BarEntry::All => (
                    "All".to_string(),
                    topic_icons::tab_icon(topic_icons::store_name("general").as_str(), icon_rems),
                ),
                topic_bar::BarEntry::Topic(id) => {
                    // The icon sits before the name and is not a word, so the
                    // two-word shortening (#2477) is applied to the name alone.
                    // A stored picture is drawn; a stored emoji stays text (#2478).
                    // A custom image resolves from the view's cache before the
                    // text fallback, so an img: reference never prints as a
                    // label (#2790).
                    let stored = self.sessions.topic_icons.get(&id).cloned();
                    let picture = stored.as_deref().and_then(|stored| {
                        topic_icons::tab_icon(stored, icon_rems)
                            .or_else(|| self.custom_icon_picture(stored, icon_rems))
                    });
                    let glyph = match (&picture, stored.as_deref()) {
                        (None, Some(icon)) if !icon.starts_with(topic_icons::IMG_PREFIX) => {
                            Some(topic_icons::tab_glyph(icon, icon_rems))
                        }
                        _ => None,
                    };
                    (topic_label(&name(id)), picture.or(glyph))
                }
            };
            cells.push(
                topic_tab(
                    id,
                    label,
                    picture,
                    shape,
                    current,
                    activity,
                    move |ev, window, app| click(ev, window, app),
                    move |ev, window, app| open_menu(ev, window, app),
                )
                .into_any_element(),
            );
        }
        if let Some(overflow) = plan.overflow {
            cells.push(overflow_chip(topic_bar::overflow_label(&overflow), shape));
        }
        cells
    }

    /// Whether a turn is in flight in one topic (#2762).
    ///
    /// A leaf drawing that session knows outright. The window's own chat knows
    /// for the conversation it has open. For a topic nobody is drawing there is
    /// no signal: the store keeps no per-topic unread count, so the chip stays
    /// quiet rather than inventing one.
    fn topic_is_busy(&self, id: Uuid) -> bool {
        match self.pane_states.get(&id) {
            Some(pane) => pane.busy,
            None => self.busy && self.transcript.session == Some(id),
        }
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
        if self.topic_draft_parent().is_none() {
            // Nothing on screen can hold a topic. Say so and drop the menu in
            // the same breath: the old guard returned with the menu already
            // gone and no overlay owning the keys, so the typed name and its
            // Enter fell through to the composer and were sent as a message
            // (#2605). A prompt that cannot create anything never opens.
            logging::debug("topic draft: no conversation here can hold topics");
            self.topic_menu = None;
            self.live
                .notices
                .push("topics are off for this conversation".to_string());
            self.refocus_screen();
            cx.notify();
            return;
        }
        self.topic_draft_icon = None;
        self.topic_menu = None;
        self.topic_draft = Some(String::new());
        // The prompt is the field being typed into, so it takes the keyboard
        // while it is open; without this the name landed in the composer
        // behind it (#2665).
        self.refocus_overlay();
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
            // The icon rides along with the rename (#2791): the draft was
            // seeded from the stored value, so only a real change writes.
            let stored_icon = self.sessions.topic_icons.get(&target).cloned();
            let draft_icon = self.topic_draft_icon.clone();
            cx.spawn(async move |this, cx| {
                match sessions::rename_session(db_path.clone(), target, name).await {
                    Ok(()) => {
                        let icon_err = if sessions::icon_write_needed(
                            stored_icon.as_deref(),
                            draft_icon.as_deref(),
                        ) {
                            sessions::set_topic_icon(db_path, target, draft_icon)
                                .await
                                .err()
                        } else {
                            None
                        };
                        this.update(cx, |this, cx| {
                            if let Some(e) = icon_err {
                                logging::error(&format!("rename icon {target}: {e}"));
                                this.live.notices.push(format!("rename icon: {e}"));
                            }
                            this.topic_draft = None;
                            this.topic_rename = None;
                            this.topic_draft_icon = None;
                            this.refresh_rail(cx);
                            // The stored icon may have changed, so the tabs
                            // and the rail read it back with the rest of the
                            // topic state, images included (#2791).
                            this.refresh_topic_state(cx);
                            // The prompt closed, so the caret goes back to the
                            // input the rename was asked from (#2665).
                            this.rail_focused = false;
                            this.refocus_screen();
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
        let Some(parent) = self.topic_draft_parent() else {
            return;
        };
        let db_path = self.db_path.clone();
        let icon = self
            .topic_draft_icon
            .clone()
            .unwrap_or_else(|| DEFAULT_TOPIC_ICON.to_string());
        cx.spawn(async move |this, cx| {
            // A Telegram forum gets a real forum thread before anything local
            // happens (#2762). Only the Bot API can make a topic, and the
            // thread id it returns is the key the bridge looks the session up
            // by, so a row written without it is a topic the chat never sees.
            // `Ok(None)` keeps the ordinary local path; `Err` is reported,
            // never papered over with a local-only row.
            let created = match telegram_topics::create_forum_topic_if_forum(
                db_path.clone(),
                parent,
                name.clone(),
            )
            .await
            {
                Ok(Some(topic)) => Ok(topic),
                Ok(None) => sessions::create_topic(db_path.clone(), parent, name).await,
                Err(e) => Err(e),
            };
            match created {
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

    /// Open the Group topics picker for `id`: load every other session as a
    /// candidate, drop the ones already in a topic tree, and show the multi
    /// select. The rail's own parent map is the source of truth, so a session
    /// that is already a parent or a child cannot be re-grouped by accident.
    fn start_group_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // Close the context menu the entry was clicked from.
        self.sessions.rail_menu = None;
        self.group_target = Some(id);
        self.group_candidates = Vec::new();
        self.group_selected.clear();
        self.group_cursor = 0;
        self.group_error = None;
        self.group_busy = false;
        self.refocus_screen();
        let db_path = self.db_path.clone();
        // Taken rows: topics themselves, every child (a key) and every parent
        // (a value) already in the rail's tree.
        let mut taken: std::collections::HashSet<Uuid> = self.sessions.topics.clone();
        taken.extend(self.sessions.parents.keys().copied());
        taken.extend(self.sessions.parents.values().copied());
        cx.spawn(async move |this, cx| {
            match sessions::load_group_candidates(db_path, id).await {
                Ok(rows) => {
                    this.update(cx, |this, cx| {
                        // A stale open (target changed while loading) is dropped.
                        if this.group_target != Some(id) {
                            return;
                        }
                        this.group_candidates = rows
                            .into_iter()
                            .filter(|(sid, _, _)| !taken.contains(sid))
                            .collect();
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.group_target == Some(id) {
                            this.group_error = Some(e);
                            cx.notify();
                        }
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Attach every checked candidate under the target, then close the picker
    /// and refresh the rail. A guard refusal stops the batch and shows its
    /// sentence instead of a raw constraint error.
    fn submit_group_topics(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.group_target else {
            return;
        };
        if self.group_busy {
            return;
        }
        let children: Vec<Uuid> = self
            .group_candidates
            .iter()
            .map(|(id, _, _)| *id)
            .filter(|id| self.group_selected.contains(id))
            .collect();
        if children.is_empty() {
            self.close_group_topics();
            return;
        }
        self.group_busy = true;
        self.group_error = None;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            for child in children {
                if let Err(e) = sessions::attach_topic(db_path.clone(), child, parent).await {
                    this.update(cx, |this, cx| {
                        this.group_busy = false;
                        this.group_error = Some(e);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.close_group_topics();
                this.refresh_rail(cx);
                this.refresh_topic_state(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Dismiss the picker and hand the keyboard back to whatever opened it.
    fn close_group_topics(&mut self) {
        self.group_target = None;
        self.group_candidates = Vec::new();
        self.group_selected.clear();
        self.group_cursor = 0;
        self.group_error = None;
        self.group_busy = false;
        self.refocus_screen();
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
                // The custom images follow the stored values, so a rename
                // that writes one reaches the rail without a restart (#2790).
                this.refresh_icon_images();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the custom icon images the rail and tabs draw (#2790). One
    /// file read per icon, at refresh time; the paint only clones an Arc.
    /// A missing or undecodable file logs and drops out, so the row falls
    /// back to its platform mark instead of drawing a blank box.
    fn refresh_icon_images(&mut self) {
        let mut images = HashMap::new();
        for stored in self.sessions.topic_icons.values() {
            let Some(path) = topic_icons::stored_image_path(stored) else {
                continue;
            };
            let Some(format) = topic_icons::image_format(stored) else {
                continue;
            };
            match std::fs::read(&path) {
                Ok(bytes) => {
                    images.insert(stored.clone(), Arc::new(Image::from_bytes(format, bytes)));
                }
                Err(e) => logging::error(&format!("icon image {stored}: {e}")),
            }
        }
        self.icon_images = images;
    }

    /// The cached picture for a stored `img:` value, or None for anything
    /// else (#2790). Never reads a file: the cache is filled at refresh
    /// time and when an image is picked.
    fn custom_icon_picture(&self, stored: &str, size: f32) -> Option<AnyElement> {
        let image = self.icon_images.get(stored)?;
        Some(topic_icons::cached_picture(image.clone(), size))
    }

    /// The icon picker's "add image" cell: the platform's own file picker,
    /// then a validated copy into the icon store (#2790). A cancelled
    /// prompt changes nothing.
    fn pick_icon_image(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an icon image".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first().cloned() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.attach_icon_image(path, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Validate one picked file and store it as the draft's icon (#2790).
    /// The copy gets a fresh uuid name, so two picks of the same file never
    /// fight over one slot and an old icon stays untouched until the rename
    /// is submitted. Failures land in the notices, never silent.
    fn attach_icon_image(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                self.live.notices.push(format!("icon image: {e}"));
                cx.notify();
                return;
            }
        };
        if !topic_icons::image_pick_allowed(&path, meta.len()) {
            self.live.notices.push(
                "icon image: pick a png, jpg, webp, gif, bmp, tiff or svg under 5MB".to_string(),
            );
            cx.notify();
            return;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.live.notices.push(format!("icon image: {e}"));
                cx.notify();
                return;
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let filename = format!("{}.{ext}", uuid::Uuid::new_v4());
        let dir = topic_icons::image_store_dir();
        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(&filename), &bytes))
        {
            self.live.notices.push(format!("icon image: {e}"));
            cx.notify();
            return;
        }
        let stored = topic_icons::img_store_name(&filename);
        if let Some(format) = topic_icons::image_format(&filename) {
            self.icon_images
                .insert(stored.clone(), Arc::new(Image::from_bytes(format, bytes)));
        }
        self.topic_draft_icon = Some(stored);
        cx.notify();
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
                // Previews follow the rows, so a new or renamed session gets
                // its line without a second trigger.
                this.refresh_previews(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One batched preview read for the rows the rail already holds.
    ///
    /// Never called from a paint: the row loop reads `sessions.previews`. The
    /// rail repaints on a shared clock, so asking per row there would be a
    /// query per card per frame.
    fn refresh_previews(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<uuid::Uuid> = self.sessions.items.iter().map(|s| s.id).collect();
        if ids.is_empty() {
            self.sessions.previews.clear();
            return;
        }
        let db_path = self.db_path.clone();
        cx.spawn(
            async move |this, cx| match sessions::load_previews(db_path, ids).await {
                Ok(pairs) => {
                    this.update(cx, |this, cx| {
                        this.sessions.previews = pairs;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => logging::error(&format!("rail previews: {e}")),
            },
        )
        .detach();
    }

    /// Keep a running conversation's preview warm without hammering the DB:
    /// at most one read a second, and only while a turn can still move the
    /// line. The chunk and tool events are the triggers, so no timer is added
    /// for this.
    fn refresh_previews_if_live(&mut self, cx: &mut Context<Self>) {
        // A parked turn is still working even though its progress is not
        // reaching the rail set.
        let stashed_live = self
            .stashed
            .values()
            .any(|t| t.live.user.is_some() || !t.live.tools.is_empty());
        if !previews_can_move(self.busy, stashed_live, self.rail.streaming_count()) {
            return;
        }
        if self
            .preview_refreshed_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.preview_refreshed_at = Some(std::time::Instant::now());
        self.refresh_previews(cx);
    }

    /// ctrl+f's deep search (#2720): let the typing settle, then read which
    /// sessions mention the query in their messages, off the paint path. Each
    /// call bumps the generation and the spawned query carries it; a result
    /// only lands when the generation still matches, so the newest text wins
    /// even when an older query finishes last (the notice timer's trick,
    /// editor.rs). An empty field clears the hits at once and spawns nothing.
    fn schedule_rail_find(&mut self, cx: &mut Context<Self>) {
        let query = self.sessions.filter.trim().to_string();
        self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
        let generation = self.rail_find_gen;
        if query.is_empty() {
            self.sessions.content_matches.clear();
            cx.notify();
            return;
        }
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // A burst of keystrokes costs one query, not one per character.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            // A newer keystroke or a closed rail already moved the generation
            // on: this query is stale, so it never touches the database.
            let stale = this
                .update(cx, |this, _| this.rail_find_gen != generation)
                .unwrap_or(true);
            if stale {
                return;
            }
            match sessions::load_content_matches(db_path, query).await {
                Ok(rows) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.rail_find_gen == generation {
                            this.apply_rail_find(rows);
                            cx.notify();
                        }
                    });
                }
                Err(e) => logging::error(&format!("rail find: {e}")),
            }
        })
        .detach();
    }

    /// Fold the content rows into the rail's deep-hit map (#2720): title
    /// matches lead in ledger order, content-only sessions follow with their
    /// snippet, and a session found both ways appears once. Only the
    /// snippet-bearing rows are kept here; `visible()` already holds the title
    /// matches on screen through `matches`, so this map is purely the deep
    /// hits and the snippets that stand in for their previews.
    fn apply_rail_find(&mut self, content: Vec<opencrabs::db::repository::ContentSearchRow>) {
        let title_ids: Vec<Uuid> = self
            .sessions
            .items
            .iter()
            .filter(|s| sessions::matches(s, &self.sessions.filter))
            .map(|s| s.id)
            .collect();
        let merged = sessions::merge_find_results(title_ids, &content);
        self.sessions.content_matches = merged
            .into_iter()
            .filter_map(|m| m.snippet.map(|snippet| (m.session_id, snippet)))
            .collect();
    }

    /// Schedule a deep read only when the trimmed query actually changed
    /// (#2721). Both typing (finder_key) and the editing chords
    /// (apply_finder_edit) funnel here, while caret moves and arrows leave the
    /// query untouched and so never rebuild the catalog.
    fn seeker_find_if_changed(&mut self, cx: &mut Context<Self>) {
        let query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        if query == self.seeker_scheduled_query {
            return;
        }
        self.seeker_scheduled_query = query;
        self.schedule_seeker_find(cx);
    }

    /// Seeker deep search (#2721): let the typing settle, then read which
    /// sessions mention the query in their messages and rebuild the card's
    /// rows with the snippets. Same generation guard as the rail's ctrl+f, so
    /// the newest query wins even when an older read finishes last. A cached
    /// query rebuilds at once; an empty query needs no read.
    fn schedule_seeker_find(&mut self, cx: &mut Context<Self>) {
        let query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        if query.is_empty() {
            return;
        }
        if self.seeker_find.contains_key(&query) {
            self.refresh_seeker_items(cx);
            return;
        }
        self.seeker_find_gen = self.seeker_find_gen.wrapping_add(1);
        let generation = self.seeker_find_gen;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // A burst of keystrokes costs one query, not one per character.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let stale = this
                .update(cx, |this, _| this.seeker_find_gen != generation)
                .unwrap_or(true);
            if stale {
                return;
            }
            match sessions::load_content_matches(db_path, query.clone()).await {
                Ok(rows) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.seeker_find_gen == generation {
                            this.seeker_find.insert(query, rows);
                            this.refresh_seeker_items(cx);
                        }
                    });
                }
                Err(e) => logging::error(&format!("seeker find: {e}")),
            }
        })
        .detach();
    }

    /// Rebuild the open Seeker's rows from the current catalog (which now
    /// folds in any cached content hits) without touching the query or caret.
    fn refresh_seeker_items(&mut self, cx: &mut Context<Self>) {
        let items = self.finder_items();
        if let Some(state) = self.finder.as_mut() {
            state.refresh_items(items);
        }
        cx.notify();
    }

    /// The bar that stands where the composer stands in select mode (#2613):
    /// how many rows are picked, Forward, and Cancel. It borrows the
    /// composer's box so the swap reads as one surface changing state, not a
    /// second panel appearing.
    fn select_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette::current();
        let count = self.msg_select.count();
        let cancel = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_select_cancel(cx);
        });
        let forward = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            // A count of zero forwards nothing, so the click is refused where
            // it happens rather than opening an empty picker.
            if this.msg_select.is_empty() {
                return;
            }
            this.msg_forward_begin(cx);
        });
        // Copy applies to the same set Forward would take (#2613): the bar is
        // the bulk-action surface, so it carries both bulk actions.
        let copy = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            if this.msg_select.is_empty() {
                return;
            }
            this.msg_copy_picks(cx);
        });
        let armed = count > 0;
        div()
            .id("message-select-bar")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .w_full()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.panel_hi))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.text_normal))
                    .child(SharedString::from(if count == 1 {
                        "1 message selected".to_string()
                    } else {
                        format!("{count} messages selected")
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("message-select-copy")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .when(armed, |d| {
                                d.cursor_pointer()
                                    .bg(rgb(palette.panel_hi))
                                    .text_color(rgb(palette.text_normal))
                                    .hover(|d| d.bg(rgb(palette.track)))
                            })
                            .when(!armed, |d| d.text_color(rgb(palette.text_faint)))
                            .child(SharedString::from("Copy"))
                            .on_click(copy),
                    )
                    .child(
                        div()
                            .id("message-select-cancel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(palette.text_muted))
                            .hover(|d| d.bg(rgb(palette.panel_hi)))
                            .child(SharedString::from("Cancel"))
                            .on_click(cancel),
                    )
                    .child(
                        div()
                            .id("message-select-forward")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .when(armed, |d| {
                                d.cursor_pointer()
                                    .bg(rgb(palette.teal))
                                    .text_color(rgb(palette.bg))
                                    .hover(|d| d.opacity(0.9))
                            })
                            .when(!armed, |d| {
                                d.bg(rgb(palette.track)).text_color(rgb(palette.text_faint))
                            })
                            .child(SharedString::from("Forward"))
                            .on_click(forward),
                    ),
            )
            .into_any_element()
    }

    /// The destination picker (#2608): the rows that will be forwarded, the
    /// query field, and the sessions a forward can land in. It replaces the
    /// composer so the choice is made where a message would have been typed.
    fn forward_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette::current();
        let count = self.forward_pick.as_ref().map_or(0, Vec::len);
        let targets = self.forward_destinations();
        let cancel = cx.listener(|this, _ev, _window, cx| {
            cx.stop_propagation();
            this.forward_cancel(cx);
        });
        let mut rows: Vec<AnyElement> = Vec::new();
        for (id, label) in targets.iter().take(FORWARD_PICKER_ROWS) {
            let target = *id;
            let picked = self.forward_cursor == Some(target);
            let pick = cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.forward_into(target, cx);
            });
            rows.push(
                div()
                    .id(SharedString::from(format!("forward-to-{id}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .when(picked, |d| d.bg(rgb(palette.selection)))
                    .hover(|d| d.bg(rgb(palette.panel_hi)))
                    .child(SharedString::from(label.clone()))
                    .child(div().text_xs().text_color(rgb(palette.text_faint)).child(
                        SharedString::from(target.to_string().chars().take(8).collect::<String>()),
                    ))
                    .on_click(pick)
                    .into_any_element(),
            );
        }
        let empty = if targets.is_empty() {
            Some(
                div()
                    .text_sm()
                    .text_color(rgb(palette.text_faint))
                    .child(SharedString::from("No session to forward to")),
            )
        } else {
            None
        };
        div()
            .id("message-forward-picker")
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.panel_hi))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().text_color(rgb(palette.text_normal)).child(
                        SharedString::from(match count {
                            1 => "Forward 1 message to".to_string(),
                            n => format!("Forward {n} messages to"),
                        }),
                    ))
                    .child(
                        div()
                            .id("message-forward-cancel")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(palette.text_muted))
                            .hover(|d| d.bg(rgb(palette.panel_hi)))
                            .child(SharedString::from("Cancel"))
                            .on_click(cancel),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(palette.track))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if self.forward_query.is_empty() {
                                palette.text_faint
                            } else {
                                palette.text
                            }))
                            .child(SharedString::from(if self.forward_query.is_empty() {
                                "Type to filter sessions".to_string()
                            } else {
                                self.forward_query.clone()
                            })),
                    ),
            )
            .children(empty)
            .children(rows)
            .into_any_element()
    }

    /// The rail's own label for a session (#2608): the topic name outranks the
    /// stored one, resolved by the same helper the rail paints with, so a
    /// forward names the source the way the user sees it.
    fn session_label(&self, id: Uuid) -> String {
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
    }

    /// The session the rows being forwarded came from (#2608).
    fn forward_source(&self) -> Option<Uuid> {
        self.forward_pick
            .as_ref()
            .and_then(|rows| rows.first())
            .map(|m| m.session_id)
    }

    /// The sessions a forward can land in, in the rail's own order: every row
    /// the user can see except the source, minus closed sessions that refuse
    /// turns, narrowed by the picker's query. A forward into the chat it came
    /// from is not a forward, so the source is never offered.
    fn forward_destinations(&self) -> Vec<(Uuid, String)> {
        let source = self.forward_source();
        let query = self.forward_query.trim().to_lowercase();
        self.sessions
            .ordered()
            .filter(|s| Some(s.id) != source && !self.sessions.is_closed(s.id))
            .map(|s| (s.id, self.session_label(s.id)))
            .filter(|(id, label)| {
                query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || id.to_string().starts_with(&query)
            })
            .collect()
    }

    /// Open the message menu on a stored row (#2613), at the press that
    /// asked for it (#2690).
    fn msg_menu_open(&mut self, id: Uuid, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        // One menu at a time: the rail's open menu closes, and so does any
        // other row's.
        self.sessions.close_rail_menu();
        self.msg_menu = Some(id);
        self.msg_menu_at = Some(at);
        cx.notify();
    }

    /// Copy one stored row's text (#2613). Selection-aware, as the issue asks:
    /// with rows picked this copies the whole selection in transcript order,
    /// and with nothing picked it copies the row the menu was opened on.
    fn msg_copy(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let texts: Vec<String> = if self.msg_select.is_empty() {
            self.transcript
                .rows
                .iter()
                .filter(|m| m.id == id)
                .map(|m| m.content.clone())
                .collect()
        } else {
            self.msg_picked_rows()
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        self.copy_texts(&texts, cx);
        self.msg_menu = None;
        cx.notify();
    }

    /// Join rows the way the drag selector joins them and put the result on the
    /// clipboard, so a copied selection and a copied drag read the same (#2613).
    fn copy_texts(&mut self, texts: &[String], cx: &mut Context<Self>) {
        if texts.is_empty() {
            return;
        }
        if let Some(text) = opencrabs::ui_core::copy_select::copied_text(texts, 0, texts.len() - 1)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// The selection bar's Copy (#2613): every picked row, transcript order.
    fn msg_copy_picks(&mut self, cx: &mut Context<Self>) {
        let texts: Vec<String> = self
            .msg_picked_rows()
            .into_iter()
            .map(|m| m.content)
            .collect();
        self.copy_texts(&texts, cx);
    }

    /// The items a row's menu shows (#2613), resolved once per frame. Only
    /// the actions that work end to end are listed: a menu entry with nothing
    /// behind it is worse than an absent one.
    fn msg_menu_items(
        &self,
        id: Uuid,
        cx: &mut Context<Self>,
    ) -> Vec<(String, transcript::ClickHandler)> {
        let mut items: Vec<(String, transcript::ClickHandler)> = Vec::new();
        let copy = cx.listener(move |this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_copy(id, cx);
        });
        items.push((MessageAction::CopyText.label().to_string(), Rc::new(copy)));
        // Forward needs somewhere to go: with no other session on the rail the
        // item would open an empty picker, so it is left out instead.
        if !self.forward_destinations_for(id).is_empty() {
            let forward = cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.msg_forward_one(id, cx);
            });
            items.push((MessageAction::Forward.label().to_string(), Rc::new(forward)));
        }
        let select = cx.listener(move |this, _ev, _window, cx| {
            cx.stop_propagation();
            this.msg_select_row(id, cx);
        });
        items.push((MessageAction::Select.label().to_string(), Rc::new(select)));
        items
    }

    /// Whether a forward of `id` has a destination at all (#2608). The menu
    /// asks before offering Forward, and it asks about the row's own session,
    /// not about whatever pick happens to be open.
    fn forward_destinations_for(&self, id: Uuid) -> Vec<(Uuid, String)> {
        let Some(source) = self
            .transcript
            .rows
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.session_id)
        else {
            return Vec::new();
        };
        self.sessions
            .ordered()
            .filter(|s| s.id != source && !self.sessions.is_closed(s.id))
            .map(|s| (s.id, self.session_label(s.id)))
            .collect()
    }

    /// Toggle one row in select mode (#2613). A shift-click extends from the
    /// anchor instead, the same gesture the transcript's text selection uses
    /// for a run of rows.
    fn msg_toggle(&mut self, id: Uuid, shift: bool, cx: &mut Context<Self>) {
        if !self.msg_select.is_active() {
            self.msg_select.begin();
        }
        let order: Vec<Uuid> = self.transcript.message_ids();
        if shift {
            self.msg_select.extend(&order, &id);
        } else {
            self.msg_select.toggle(id);
        }
        self.msg_menu = None;
        cx.notify();
    }

    /// "Select" from a row's menu: enter select mode with that row picked, so
    /// a run of rows starts at the one the user pointed at.
    fn msg_select_row(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.msg_select.begin();
        self.msg_select.toggle(id);
        self.msg_menu = None;
        cx.notify();
    }

    /// Leave select mode and drop the picks (#2613).
    fn msg_select_cancel(&mut self, cx: &mut Context<Self>) {
        self.msg_select.cancel();
        self.msg_menu = None;
        cx.notify();
    }

    /// The rows the current picks stand for, in transcript order (#2608).
    /// Order matters: the destination reads them the way the source showed
    /// them, not the order they happened to be clicked.
    fn msg_picked_rows(&self) -> Vec<Message> {
        let order: Vec<Uuid> = self.transcript.message_ids();
        self.msg_select
            .picked_in_order(&order)
            .into_iter()
            .filter_map(|id| self.transcript.rows.iter().find(|m| m.id == id).cloned())
            .collect()
    }

    /// "Forward" on a single row: pick it and go straight to the destination
    /// picker, so one message does not need select mode first (#2613).
    fn msg_forward_one(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // A single-row forward never enters select mode: there is nothing to
        // count, so the bar would be a step with one option.
        let rows: Vec<Message> = self
            .transcript
            .rows
            .iter()
            .filter(|m| m.id == id)
            .cloned()
            .collect();
        self.msg_menu = None;
        self.forward_open(rows, cx);
    }

    /// Forward the picks: open the destination picker, or say why it cannot
    /// open (#2608).
    fn msg_forward_begin(&mut self, cx: &mut Context<Self>) {
        let rows = self.msg_picked_rows();
        self.forward_open(rows, cx);
    }

    /// Hand the rows to the picker. The composer is swapped for it, so the
    /// destination is chosen where a message would have been typed.
    fn forward_open(&mut self, rows: Vec<Message>, cx: &mut Context<Self>) {
        if rows.is_empty() {
            return;
        }
        self.forward_pick = Some(rows);
        self.forward_query.clear();
        self.forward_cursor = self.forward_destinations().first().map(|(id, _)| *id);
        self.msg_menu = None;
        cx.notify();
    }

    /// Back out of the picker, keeping the picks: the user who opened it by
    /// mistake lands back in select mode with the same rows still chosen.
    fn forward_cancel(&mut self, cx: &mut Context<Self>) {
        self.forward_pick = None;
        self.forward_query.clear();
        self.forward_cursor = None;
        cx.notify();
    }

    /// Walk the destination list (#2608), wrapping nowhere: the ends clamp,
    /// like the rail's cursor.
    fn forward_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let targets: Vec<Uuid> = self
            .forward_destinations()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if targets.is_empty() {
            self.forward_cursor = None;
            cx.notify();
            return;
        }
        let at = self
            .forward_cursor
            .and_then(|id| targets.iter().position(|t| *t == id));
        let next = match (at, delta.signum()) {
            (Some(ix), 1) => (ix + 1).min(targets.len() - 1),
            (Some(ix), -1) => ix.saturating_sub(1),
            (None, 1) => 0,
            _ => targets.len() - 1,
        };
        self.forward_cursor = Some(targets[next]);
        cx.notify();
    }

    /// Write the rows into the destination (#2608): a click on a row, or Enter
    /// on the picker's cursor.
    fn forward_commit(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self
            .forward_cursor
            .or_else(|| self.forward_destinations().first().map(|(id, _)| *id))
        else {
            return;
        };
        self.forward_into(target, cx);
    }

    /// Copy the picked rows into `target`. The write runs off the UI thread;
    /// the destination is only written to, never prompted, so a forward can
    /// never start a turn the user did not ask for.
    fn forward_into(&mut self, target: Uuid, cx: &mut Context<Self>) {
        let Some(rows) = self.forward_pick.clone() else {
            return;
        };
        let Some(source) = self.forward_source() else {
            return;
        };
        if target == source {
            return;
        }
        let label = self.session_label(source);
        let db_path = self.db_path.clone();
        self.forward_pick = None;
        self.forward_query.clear();
        self.forward_cursor = None;
        self.msg_select.cancel();
        self.msg_menu = None;
        cx.spawn(async move |this, cx| {
            let result = transcript::forward_rows(db_path, rows, target, label).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(landed) => {
                        // A forward into the chat on screen has to show up:
                        // the rows are written, so a reload finds them. Any
                        // other destination is announced only, because the
                        // window runs one conversation at a time.
                        if this.transcript.session == Some(target) {
                            this.reload_transcript(target, cx);
                        }
                        logging::info(&format!("forward: {landed} row(s) into {target}"));
                    }
                    Err(e) => logging::error(&format!("forward into {target}: {e}")),
                }
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
        // The turn that just finished should add at least one row to the
        // session. A reload that comes back short is a write that has not
        // landed yet. The count is the session's own row total, so a
        // conversation past the display window settles like any other (#2705).
        let expect = self.settle_expect;
        let attempt = self.settle_retries;
        // The budget starts at the first read that found the rows short, not
        // when the question was sent: it measures the write, which only exists
        // once the turn has ended.
        let deadline = *self
            .settle_deadline
            .get_or_insert_with(|| std::time::Instant::now() + SETTLE_BUDGET);
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, sid).await;
            this.update(cx, |this, cx| {
                if this.transcript.session != Some(sid) {
                    return;
                }
                match result {
                    Ok(loaded) => {
                        let (hidden, turns) = transcript::turns_with_origins(
                            &loaded.rows,
                            loaded.total,
                            &this.turn_metrics,
                            &loaded.origins,
                        );
                        let landed = settle_landed(expect, loaded.total);
                        // Taken before `rows` is moved out below: every retry
                        // log names the row total it was comparing against.
                        let total = loaded.total;
                        this.transcript.hidden = hidden;
                        this.transcript.turns = turns;
                        // The rows now carry the question the card opened the
                        // turn with, so this exchange has been identified
                        // against the DB once. Record it, because the
                        // persisted turn is skipped while the card draws it:
                        // dropping the card's own question here would leave
                        // the prompt on neither side of the seam (#2705).
                        let question_landed = this
                            .transcript
                            .turns
                            .last()
                            .is_some_and(|turn| transcript::newest_live_turn_matches(turn, &this.live));
                        if question_landed {
                            this.live.landed = true;
                        }
                        // A turn that is still running owns the live card.
                        // Reading the conversation under it (a boot resume, or
                        // opening a session mid-turn) must not wipe the card
                        // the work is painting into (#2627).
                        let in_flight = this.busy;
                        // The ordered ids the selection resolves its ranges and
                        // its copy order against, which the visible turns alone
                        // cannot answer once they are folded (#2613).
                        this.transcript.rows = loaded.rows;
                        if landed {
                            // The rows are in: the newest turn is the one
                            // that just answered, so it can claim the metrics
                            // recorded at `Done` (#2324).
                            if !in_flight {
                                this.attach_pending_metrics(sid);
                                this.live.clear();
                            }
                            this.settle_expect = None;
                            this.settle_retries = 0;
                            this.settle_deadline = None;
                            this.settle_rechecks = 0;
                        } else if in_flight {
                            // Short of the count while a turn is running is not
                            // a missed write, it is a turn still writing.
                            this.settle_expect = None;
                            this.settle_retries = 0;
                            this.settle_deadline = None;
                        } else if std::time::Instant::now() < deadline {
                            // The row is still in flight: retry shortly, the
                            // live turn keeps the answer on screen meanwhile.
                            // The row total travels with the attempt so the
                            // next incident is diagnosable from the log alone.
                            this.settle_retries = attempt + 1;
                            logging::info(&format!(
                                "transcript short of the finished turn, retry {} (rows {total}, want {})",
                                attempt + 1,
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            this.reload_transcript(sid, cx);
                        } else if this.settle_rechecks < SETTLE_RECHECKS {
                            // The budget ran out, but the payload is usually
                            // written by now and only this read missed it. One
                            // more look after a pause turns a permanently
                            // pinned live card into a bounded delay (the
                            // 2026-11-08 incident sat there until the session
                            // was reopened).
                            this.settle_rechecks += 1;
                            this.settle_expect = expect;
                            this.settle_retries = 0;
                            this.settle_deadline = None;
                            logging::info(&format!(
                                "transcript short after {SETTLE_BUDGET:?}; recheck {} (rows {total}, want {})",
                                this.settle_rechecks,
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            let view = cx.entity().downgrade();
                            cx.spawn(async move |_, cx| {
                                cx.background_executor().timer(SETTLE_RECHECK).await;
                                view.update(cx, |this, cx| {
                                    if this.transcript.session == Some(sid) {
                                        this.reload_transcript(sid, cx);
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        } else {
                            // Bounded: the live turn keeps the answer, so the
                            // pane is never empty even if the write never lands.
                            logging::info(&format!(
                                "transcript never caught up; keeping the live turn (rows {total}, want {})",
                                expect.map_or("?".to_string(), |n| n.to_string()),
                            ));
                            this.settle_expect = None;
                            this.settle_retries = 0;
                            this.settle_deadline = None;
                        }
                    }
                    Err(e) => this.transcript.error = Some(e.into()),
                }
                // The turn has landed: follow it only if the reader is still
                // at the bottom, the terminal's own rule (#2685, #2698).
                this.follow_chat();
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
        let Some(metrics) = self.pending_metrics.remove(&sid) else {
            return;
        };
        let Some(turn) = self.transcript.turns.last_mut() else {
            return;
        };
        turn.metrics = Some(metrics);
        // The persisted row can carry no ledger at all, and the card must not
        // claim the turn did nothing (#2632).
        turn.summary.tool_calls = turn.summary.tool_calls.max(metrics.tool_calls);
        let anchor = turn.anchor.clone();
        self.turn_metrics.insert(anchor.clone(), metrics);
        // The card the user was watching keeps its rows once it settles
        // (#2632): a turn that folds its ledger away the moment it finishes
        // reads as if the work never happened. The chevron still folds it, and
        // older turns keep the terminal's folded default.
        self.transcript.expanded.insert(anchor);
    }

    /// Give a freshly loaded pane the numbers its turn recorded at settle: the
    /// last turn of those rows is the one that just answered, and the anchor it
    /// landed under is what keeps the numbers on a later reload (#2632).
    fn claim_pane_metrics(
        &mut self,
        session: Uuid,
        mut turns: Vec<transcript::Turn>,
    ) -> Vec<transcript::Turn> {
        let Some(metrics) = self.pending_metrics.remove(&session) else {
            return turns;
        };
        if let Some(turn) = turns.last_mut() {
            turn.metrics = Some(metrics);
            turn.summary.tool_calls = turn.summary.tool_calls.max(metrics.tool_calls);
            self.turn_metrics.insert(turn.anchor.clone(), metrics);
        }
        turns
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The theme modal owns every key while it is open, on every screen: it
        // is the topmost surface, so nothing under it may answer a keystroke,
        // the composer's arrows and Enter included (#2619).
        if self.themes_open {
            let ks = &ev.keystroke;
            if ks.modifiers.control && ks.modifiers.shift && ks.key == "t" {
                self.close_themes(cx);
                return;
            }
            self.theme_modal_key(ks, cx);
            return;
        }
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
            // Alt+Tab rotates the caret between the panes and Alt+Shift+Tab
            // the other way (#2580, #2579). It runs ahead of the composer gate
            // so the chord works with the caret in the composer, and ahead of
            // bare Tab, which keeps its own jobs.
            if let Some(rotation) = rotation_chord(ks) {
                self.cycle_pane_focus(rotation == split::PaneRotation::Clockwise, cx);
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
            // The forward picker owns the keyboard while it is open (#2608):
            // the destination is narrowed by typing, the arrows walk it, Enter
            // takes it and Esc backs out to the picks it came from. It sits
            // this early for the same reason the split pick does: the
            // composer underneath must not answer a key meant for the bar.
            if self.forward_pick.is_some() {
                match ks.key.as_str() {
                    "escape" => self.forward_cancel(cx),
                    "enter" => self.forward_commit(cx),
                    "backspace" => {
                        self.forward_query.pop();
                        self.forward_cursor =
                            self.forward_destinations().first().map(|(id, _)| *id);
                        cx.notify();
                    }
                    "up" | "down" => {
                        let delta = if ks.key == "down" { 1 } else { -1 };
                        self.forward_step(delta, cx);
                    }
                    _ => {
                        if !ks.modifiers.control
                            && !ks.modifiers.platform
                            && !ks.modifiers.alt
                            && let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                            && !ch.is_control()
                        {
                            self.forward_query.push(ch);
                            self.forward_cursor =
                                self.forward_destinations().first().map(|(id, _)| *id);
                            cx.notify();
                        }
                    }
                }
                return;
            }
            // Select mode (#2613): Esc leaves it, Enter forwards the picks.
            // Every other key falls through, so the window's chords keep
            // answering while the selection bar replaces the composer.
            if self.msg_select.is_active() {
                match ks.key.as_str() {
                    "escape" => {
                        self.msg_select_cancel(cx);
                        return;
                    }
                    "enter" => {
                        self.msg_forward_begin(cx);
                        return;
                    }
                    _ => {}
                }
            }
            // A row's menu closes on Esc, exactly like the rail's (#2613).
            if self.msg_menu.is_some() && ks.key == "escape" {
                self.msg_menu = None;
                cx.notify();
                return;
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
            // The chord takes the release path: copy, drop the highlight, and
            // say so in the composer exactly like a mouse release (#2587).
            if self.copy_drag.finish_copy(cx) {
                self.copy_drag.announce_copied(cx);
            }
            cx.notify();
            return;
        }
        let screen_action = screen_chord(&ev.keystroke.key, mods, self.screen);
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
                self.arm_rail_filter(cx);
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
        // The open filter panel takes Escape first (#2787): it closes the
        // chips without touching the find query underneath.
        if self.sessions.rail_filter_open && ev.keystroke.key.as_str() == "escape" {
            self.sessions.rail_filter_open = false;
            cx.notify();
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
                        // The field answers the split pick the way the row
                        // underneath it does (#2760).
                        self.pick_or_open(id, cx);
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
                    self.schedule_rail_find(cx);
                    self.normalize_cursor();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.sessions.filter.push(ch);
                        self.schedule_rail_find(cx);
                        self.normalize_cursor();
                        cx.notify();
                    }
                }
            }
            return;
        }
        // Deleting a conversation has no chord. ctrl+backspace is the editor's
        // word delete (#2313), so a window binding on it would break typing,
        // and the command key is macOS-only. The row menu's delete, behind
        // the confirm dialog, is the one path: right-click and slide reveal it.
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
        // A warning that stopped a pick has one answer, so it owns the
        // keyboard while it is up (#2605): Enter or Esc closes it, and every
        // other keystroke is swallowed instead of reaching the card behind
        // it.
        if self.topic_warning.is_some() {
            if matches!(ev.keystroke.key.as_str(), "enter" | "escape") {
                self.topic_warning = None;
                self.refocus_screen();
            }
            cx.notify();
            return;
        }
        // The Group topics picker is a multi select modal: arrows move the
        // cursor, Space toggles the row under it, Enter attaches the checked
        // rows, Esc cancels. Every other key is swallowed so nothing behind
        // the card answers while it is up.
        if self.group_target.is_some() {
            match ev.keystroke.key.as_str() {
                "up" | "k" if !self.group_candidates.is_empty() => {
                    self.group_cursor = self.group_cursor.saturating_sub(1);
                }
                "down" | "j" if !self.group_candidates.is_empty() => {
                    self.group_cursor =
                        (self.group_cursor + 1).min(self.group_candidates.len() - 1);
                }
                "space" => {
                    if let Some((id, _, _)) = self.group_candidates.get(self.group_cursor) {
                        let id = *id;
                        if !self.group_selected.insert(id) {
                            self.group_selected.remove(&id);
                        }
                    }
                }
                "enter" => {
                    self.submit_group_topics(cx);
                }
                "escape" => {
                    self.close_group_topics();
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        // The delete confirmation is a modal with two answers, so it owns
        // both keys while it is up: Enter confirms, Esc cancels, and every
        // other keystroke is swallowed rather than reaching anything behind
        // it (#2456).
        if let Some(id) = self.confirm_delete {
            match ev.keystroke.key.as_str() {
                "enter" => {
                    self.confirm_delete = None;
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
            // The context menu closes next: Esc dismisses the nearest
            // affordance, never the far one, and a dismissal writes
            // nothing (#2354).
            if self.sessions.rail_menu.take().is_some() {
                cx.notify();
                return;
            }
            // The command list lives inside the composer, so with it open Esc
            // closes the list and keeps the draft: the terminal's behaviour,
            // and the nearest thing answers first.
            // The list belongs to whichever field opened it, so Esc closes the
            // focused pane's list rather than the chat's under it (#2614).
            let owner = self.composer_entity();
            let spelling = owner.read(cx).list_open();
            if spelling {
                owner.update(cx, |editor, cx| {
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
            // A split leaf runs its own turn with `pane.busy` set while
            // `self.busy` stays clear, so the idle path below would hand the
            // keyboard back and leave that turn streaming (#2657). The stop
            // reaches the pane the keyboard is on first, the same order
            // `/stop` uses (#2614).
            if self.cancel_focused_pane_turn("Esc", cx) {
                return;
            }
            if !self.busy {
                // Idle Esc hands the keyboard back. The rail's mode is the
                // only thing Esc owns here, and Tab is what arms it. The old
                // blur bridge existed only because focus never moved off the
                // composer (#2351, #2454). A turn running in another
                // conversation counts as idle here: Esc must never cancel work
                // the user cannot see (#2616).
                if self.rail_focused {
                    self.rail_focused = false;
                    self.refocus_screen();
                }
                cx.notify();
                return;
            }
            // One Esc stops the turn, and the queue becomes the next turn. The
            // old two-press window existed because nothing flushed the queue on
            // a stop, so a single press left the user watching a process they
            // had already asked to kill with their typed text stranded behind
            // it (#2631).
            //
            // A stop that beats the first token takes the message back
            // (#2635): the turn never reached the conversation, so the text
            // belongs in the composer to be corrected and sent again instead of
            // sitting in a chat that will never answer it, and the rows the
            // turn opened are deleted rather than redrawn.
            let mut returned = false;
            if let Some(sid) = self.transcript.session
                && self.busy
                && self.live.untouched()
            {
                let composer = self.composer_entity();
                // Only into an empty field: anything typed since the send is
                // the user's, and taking the question back must never cost it.
                // One condition, not two nested ones: the take has to run only
                // on an empty field, and clippy reads the nesting as a mistake
                // rather than the guard it is (#2641).
                if composer.read(cx).content.trim().is_empty()
                    && let Some(text) = self.live.user.take()
                {
                    composer.update(cx, |editor, cx| {
                        editor.restore_draft(text, cx);
                        editor.show_returned(cx);
                    });
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_undo_last_turn(
                            bridge.service.clone(),
                            sid,
                            self.sink.clone(),
                        );
                    }
                    self.live.clear();
                    returned = true;
                }
            }
            if let Some(cancel) = self.cancel.take() {
                cancel.cancel();
            }
            // Same wording and immediate release as `abort_active_turn`: the
            // pane stops claiming a turn is running the moment the user asked
            // it to stop, and the service's own Failed event lands afterwards.
            self.busy = false;
            self.live.thinking = false;
            // Only a turn that still has something to show keeps the stop
            // (#2811): a handed-back question already cleared the card, and
            // marking that one stopped would draw an empty card with no text.
            if !returned {
                self.live.stopped = true;
            }
            self.cancel = None;
            let waiting = self.queued_for_strip().len();
            if waiting > 0 {
                // The send itself waits for the cancellation to be
                // acknowledged: a second `spawn_turn` on a session whose cancel
                // is still unwinding would run two turns into one
                // conversation. The turn's own event drains the queue (see the
                // post-batch `drain_pending`), which is milliseconds after the
                // stop (#2631).
                self.live.notices.push(format!(
                    "Operation cancelled. Sending the {waiting} queued."
                ));
            } else if !returned {
                // A handed-back message already says so in the composer, so
                // the notice would be the second time the user is told.
                self.live.notices.push("Operation cancelled.".to_string());
            }
            logging::info("turn cancelled by Esc");
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        }
        // Page keys scroll the transcript and ctrl+k clears the session (#2430).
        // Both sit before the composer gate, so they work while the composer has focus.
        if self.screen == Screen::Chat {
            let key = ev.keystroke.key.as_str();
            if matches!(key, "pageup" | "pagedown") {
                let page = px(f32::from(window.viewport_size().height) * 0.8);
                let up = key == "pageup";
                let dy = if up { page } else { -page };
                // The page keys act on the pane the caret is on, so a split
                // scrolls the transcript the accent names rather than the
                // chat underneath it (#2475, #2472).
                let target = self
                    .pane_focus
                    .filter(|id| self.pane_states.contains_key(id));
                let (x, y, max) = match target.and_then(|id| self.pane_states.get(&id)) {
                    Some(pane) => {
                        let offset = pane.scroll.offset();
                        let max = pane.scroll.max_offset().height;
                        (offset.x, (offset.y + dy).min(px(0.0)).max(-max), max)
                    }
                    None => {
                        let offset = self.transcript_scroll.offset();
                        let max = self.transcript_scroll.max_offset().height;
                        (offset.x, (offset.y + dy).min(px(0.0)).max(-max), max)
                    }
                };
                match target {
                    Some(id) => {
                        if let Some(pane) = self.pane_states.get_mut(&id) {
                            pane.scroll.set_offset(gpui::point(x, y));
                        }
                    }
                    None => self.transcript_scroll.set_offset(gpui::point(x, y)),
                }
                // A page key is the reader moving by hand, the same gesture
                // the terminal binds to its own scroll: up releases the follow
                // and down gives it back only once it lands on the end, so a
                // row that lands mid-page never drags the reader back down
                // (#2698).
                let at_end = y >= -max;
                match (target, at_end) {
                    (Some(id), true) => self.rearm_pane_follow(id),
                    (Some(id), false) => self.release_pane_follow(id),
                    (None, true) => self.rearm_transcript_follow(),
                    (None, false) => self.release_transcript_follow(),
                }
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
        // Ctrl+C is the terminal's own ladder (#2308): scrolled up it gives
        // the bottom back, otherwise it throws the draft away and arms a three
        // second quit window, and the second press stops the turn in flight and
        // closes the window. It sits ahead of the composer gate so the key
        // answers while the caret is in the field; the interceptor in `main` is
        // what keeps the editor's copy chord from taking the press first when
        // nothing is selected.
        if ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.alt
            && ev.keystroke.key == "c"
        {
            self.ctrl_c_press(cx);
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
                            self.open_session_handing_back(id, false, cx);
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
                // A printable character with the rail focused starts a search
                // (#2812). The field arms itself and takes the character, so
                // the list narrows as a name is typed instead of asking for
                // ctrl+f first. The letters the terminal answers (r, t, c) are
                // not spent here: the first keystroke of "rust" cannot also be
                // the rename command, so the window keeps them on the row menu
                // and the terminal keeps the bare keys.
                _ if self.rail_focused => {
                    let ks = &ev.keystroke;
                    if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                        // A chord keeps its own meaning; only bare typing
                        // opens the field.
                    } else if let Some(ch) = ks.key_char.as_deref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                        && !ch.is_whitespace()
                    {
                        self.arm_rail_filter(cx);
                        self.sessions.filter.push(ch);
                        self.schedule_rail_find(cx);
                        self.normalize_cursor();
                        return;
                    }
                }
                // Every other key falls through to the handlers below. The
                // arms above are guarded, so the match still needs a wildcard
                // to be exhaustive: a keystroke the rail does not claim is not
                // an error, it is simply somebody else's.
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

    /// Ring for plan tasks that settled since the last frame (#2691).
    ///
    /// The terminal does this from its tick (`tui::app::state`), reading the
    /// plan file the tool writes. A gpui window has no tick, so the render
    /// path is the equivalent place: it already re-reads the strips by
    /// mtime, and this pass turns the settled tasks into one chime. The
    /// window strip and every pane strip share one seen set, so a plan drawn
    /// twice rings once, and a reload of an unchanged file rings not at all.
    fn chime_settled_plan_tasks(&mut self) {
        let now = std::time::Instant::now();

        // A pane strip only re-reads its file when it is drawn, and this pass
        // runs before the split body is built. Refresh here so a background
        // pane that settled rings on this frame instead of the next redraw it
        // happens to get (#2733). Each read is mtime-gated.
        for pane in self.pane_states.values_mut() {
            pane.plan.refresh();
        }

        // Every plan in this frame, each paired with the row that draws it, in
        // one order so a settled task reaches the chime, the banner, and the
        // flash from a single pass. A pre-init plan writes the nil session
        // until the hook stamps it, so the drawing row is the honest fallback.
        let mut plans: Vec<Option<&opencrabs::tui::plan::PlanDocument>> =
            Vec::with_capacity(1 + self.pane_states.len());
        let mut owners: Vec<Uuid> = Vec::with_capacity(1 + self.pane_states.len());

        plans.push(self.plan.document());
        owners.push(self.transcript.session.unwrap_or(Uuid::nil()));
        for (pane_id, pane) in &self.pane_states {
            plans.push(pane.plan.document());
            owners.push(*pane_id);
        }

        let seen = &mut self.announced_task_completions;

        // Which sessions settle in this pass. Read before the announce, because
        // the announce is what writes them into `seen`, the only record of what
        // already rang. Asking `contains` rather than `insert` keeps that write
        // in one place, so the flash and the chime cannot disagree about which
        // task was fresh.
        let mut settled: Vec<Uuid> = Vec::new();
        for (slot, owner) in plans.iter().zip(owners.iter()) {
            let Some(doc) = *slot else {
                continue;
            };
            let fresh = opencrabs::utils::completion_sound::resolved_task_ids(Some(doc))
                .into_iter()
                .any(|id| !seen.contains(&id));
            if !fresh {
                continue;
            }
            settled.push(if doc.session_id == Uuid::nil() {
                *owner
            } else {
                doc.session_id
            });
        }

        // #2729: the same set drives the OS banner, so a task that rings also
        // reaches the Notification Center, and neither can fire twice for one
        // settle.
        if opencrabs::utils::desktop_notify::announce_settled_tasks(seen, &plans) == 0 {
            return;
        }
        // One flash per settled session, on the same instant the chime rings:
        // the sound and the blink are two surfaces of one event.
        for session in settled {
            if session == Uuid::nil() {
                continue;
            }
            self.plan_flash_until.insert(
                session,
                now + opencrabs::utils::completion_sound::FLASH_WINDOW,
            );
        }
        Self::play_task_chime();
    }

    /// Whether any rail flash window is still open, dropping the expired ones.
    ///
    /// Read by the idle poll: while a bar blinks, the rail has to keep
    /// repainting so the orange alternates, and once every window closed the
    /// repaint budget goes back to zero. Pruned here rather than at paint time
    /// so the map cannot grow past the sessions that are actually flashing.
    fn plan_flash_active(&mut self, now: std::time::Instant) -> bool {
        self.plan_flash_until.retain(|_, until| now < *until);
        !self.plan_flash_until.is_empty()
    }

    /// The chime itself, honouring the operator's opt-in and the build
    /// feature (#2691). The flag is read at the moment of the chime, so
    /// flipping `[agent] completion_notification_enabled` in a live session
    /// takes effect without a restart, exactly as the terminal's arm does.
    #[cfg(feature = "notification-sound")]
    fn play_task_chime() {
        let enabled = opencrabs::config::Config::load()
            .map(|c| c.agent.completion_notification_enabled)
            .unwrap_or(false);
        if enabled {
            opencrabs::utils::completion_sound::play_completion_sound();
        }
    }

    #[cfg(not(feature = "notification-sound"))]
    fn play_task_chime() {}

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
            Some(title) => SharedString::from(title),
            None => SharedString::from("Chat"),
        };
        // Every pane carries its own status row (#2593). Resolved once here, from
        // the pane's own session, so the draw pass walks the tree with a plain
        // string per leaf instead of touching the bridge or the plan file.
        let leaves = tree.leaves();
        let count = leaves.len();
        // One owner for the keyboard, resolved the same way the drop handler
        // and the caret resolve it (#2620).
        let focus = self.split_focus();
        let mut footers: HashMap<Uuid, SharedString> = HashMap::new();
        for (i, session) in leaves.iter().enumerate() {
            footers.insert(
                *session,
                SharedString::from(self.pane_footer_text(*session, i + 1, count)),
            );
        }
        // Per-pane chrome (#2530): every pane carries its own plan strip,
        // queue and composer, exactly as the terminal's panes do. Built here
        // because a leaf walk has no Context and the plan strips need a
        // re-read. The themes preview draws the same tree without panes
        // owning the keyboard, so it keeps the shared chrome off.
        let mut chrome: HashMap<Uuid, AnyElement> = HashMap::new();
        // One topic strip per pane, in that pane's own chrome (#2686): the
        // strip used to resolve `self.transcript.session`, so every pane in a
        // wall showed the chat leaf's topics and clicking one moved a
        // conversation the user was not reading.
        let mut tops: HashMap<Uuid, AnyElement> = HashMap::new();
        if interactive {
            for session in &leaves {
                let is_chat = Some(*session) == self.transcript.session;
                let focused = if is_chat {
                    focus.is_none()
                } else {
                    focus == Some(*session)
                };
                // This pane's own topics, resolved from its own session.
                // The root is the pane's parent when the pane holds a topic,
                // so the open chip is that pane's session rather than the
                // chat's. In a split the strip belongs to the pane that
                // carries the family, not to every topic inside it: a wall of
                // topic panes repeated the same bar once per pane (#2761).
                let pane_root = self
                    .sessions
                    .parents
                    .get(session)
                    .copied()
                    .unwrap_or(*session);
                let carries_topics = leaves.len() == 1 || pane_root == *session;
                if carries_topics && self.sessions.is_topic_enabled(pane_root) {
                    let pane_key = session.to_string();
                    let tabs = self.topic_tabs(Some(*session), cx);
                    let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.cycle_topic_edge(cx);
                    });
                    let strip = topic_strip(&pane_key, topic_tabs::Edge::Top, toggle_edge, tabs);
                    tops.insert(*session, strip.into_any_element());
                }
                // Each pane's plan and queue, read from that pane's session.
                // The plan is the terminal's three-line pane strip (#2622):
                // a split pane is a fraction of the window, and the full
                // checklist clipped mid-word and pushed its header past the
                // pane's edge.
                // How much plan this pane can read (#2624): a wall of panes
                // keeps a bar, a pair keeps the running task and the next one
                // or two. The width comes from the tree, so a narrow column
                // compacts even when there are only two panes.
                let share = self
                    .panes
                    .as_ref()
                    .and_then(|tree| tree.leaf_width_share(*session))
                    .unwrap_or(1.0);
                let pane_width = if share < 1.0 {
                    split_pane_width(content, false, share)
                } else {
                    content
                };
                let density = plan_strip::pane_density(count, pane_width);
                let expanded = self.plan_expanded.contains(session);
                // A plan waiting on approval is the one card a pane must not
                // lose in a split (#2755): it is the only place the tap lives.
                let actions = self.plan_actions(*session, cx);
                let (plan_view, queued, editor) = if is_chat {
                    self.plan.refresh();
                    (
                        self.plan
                            .editing_view(Some(&actions))
                            .or_else(|| self.plan.pane_view(density, expanded)),
                        self.queued_for_strip(),
                        self.editor.clone(),
                    )
                } else {
                    match self.pane_states.get_mut(session) {
                        Some(pane) => {
                            pane.plan.refresh();
                            (
                                pane.plan
                                    .editing_view(Some(&actions))
                                    .or_else(|| pane.plan.pane_view(density, expanded)),
                                agent_bridge::queue_snapshot(*session),
                                pane.editor.clone(),
                            )
                        }
                        None => continue,
                    }
                };
                let mut col = div().flex().flex_col().flex_none().gap_2();
                if let Some(view) = plan_view {
                    // A click trades the conversation's room for the whole
                    // checklist, per pane (#2624). The handler stops the event
                    // so the press does not also hand the leaf the keyboard.
                    let plan_session = *session;
                    col = col.child(
                        view.id(SharedString::from(format!("plan-strip-{plan_session}")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                                    cx.stop_propagation();
                                    this.toggle_pane_plan(plan_session, cx);
                                }),
                            ),
                    );
                }
                if !queued.is_empty() {
                    let strip_session = *session;
                    // Only a folded queue is worth a click, so a two-message
                    // list does not react to a stray tap (#2629).
                    let folds = queued.len() > 2;
                    col = col.child(
                        queue_strip(&queued, self.queue_expanded)
                            .id(SharedString::from(format!("queue-strip-{strip_session}")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this: &mut Self, _, _, cx| {
                                    cx.stop_propagation();
                                    if folds {
                                        this.queue_expanded = !this.queue_expanded;
                                        cx.notify();
                                    }
                                }),
                            ),
                    );
                }
                // The follow-up offer sits with the queue, above the composer
                // it will send into (#2708).
                let offers = agent_bridge::options_snapshot(*session);
                if !offers.is_empty() {
                    let rows = self.option_rows(*session, offers, cx);
                    col = col.child(options_strip(&rows));
                }
                if let Some(label) = self.running_tasks_label_for(*session) {
                    col = col.child(task_strip(label));
                }
                // The pane holding the keyboard draws the real composer; the
                // others show the draft they hold, dimmed and inert, which is
                // the same "disabled while unfocused" language the rest of the
                // split uses.
                // Select mode and the forward picker both stand where the
                // composer stands (#2613, #2608): they are answers to the
                // conversation, not a second panel, and the composer has
                // nothing to say while either is up. Only the chat pane draws
                // them: one selection belongs to one conversation.
                // The picker outranks the bar: it is opened from select mode
                // and the picks are still live under it, so the bar would
                // otherwise keep the slot the destination is chosen in.
                let composer: AnyElement = if focused {
                    if is_chat && self.forward_pick.is_some() {
                        self.forward_picker(cx)
                    } else if is_chat && self.msg_select.is_active() {
                        self.select_bar(cx)
                    } else {
                        // The popups that hang over the input travel with it:
                        // a split must not lose the slash list or the emoji
                        // panel, and they read the field of the pane that owns
                        // the keyboard, not the chat underneath it (#2530,
                        // #2610).
                        let mut cell = div().flex().flex_col().flex_none().gap_2();
                        let slash_popup = editor.read(cx).command_popup();
                        let emoji_popup = editor.read(cx).emoji_popup();
                        if self.topic_draft.is_none()
                            && let Some((names, selected)) = slash_popup
                        {
                            cell = cell.child(command_popup_view(&names, selected));
                        } else if self.topic_draft.is_none()
                            && let Some((items, selected)) = emoji_popup
                        {
                            cell = cell.child(emoji_popup_view(&items, selected));
                        }
                        cell.child(editor).into_any_element()
                    }
                } else {
                    let draft = editor.read(cx).content.clone();
                    pane_composer_dim(draft, *session, is_chat, cx.entity().downgrade())
                };
                col = col.child(composer);
                chrome.insert(*session, col.into_any_element());
            }
        }
        let mut ctx = PanesCtx {
            chat: Some(chat),
            chat_session: self.transcript.session,
            chat_title: title,
            states: &self.pane_states,
            // The resolved owner, not the raw id: a stale `pane_focus` named a
            // leaf the tree no longer holds, so every leaf read as unfocused,
            // drew dim, and painted no ring (#2620).
            focus,
            content,
            areas: &self.split_areas,
            entity: cx.entity().downgrade(),
            interactive,
            footers: &footers,
            chrome: &mut chrome,
            tops: &mut tops,
            drop: self.pane_drop,
        };
        let mut path = Vec::new();
        let tree = self.panes.as_ref().expect("checked a moment ago");
        pane_element(tree, &mut ctx, &mut path, 1.0)
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    let composer = self.composer_entity();
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
            let composer = self.composer_entity();
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
            let composer = self.composer_entity();
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
            self.follow_pane(session);
        }
        // The rail is an overlay only while it holds the keys. A path that
        // focuses the composer directly (a drop, a pasted image, the
        // paperclip's deferred request) never asks `refocus_screen`, so the
        // claim is repaired here: an armed rail over a focused input would
        // steal the caret back on the next transition (#2665).
        if self.rail_focused
            && self.rail_open
            && self
                .composer_entity()
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
        }
        self.plan.refresh();
        // A task that just settled rings once (#2691). This is the window's
        // tick: the strip was re-read by mtime just above, and the pane
        // strips are re-read again when the split body is built below, so a
        // pane that settles between frames is caught on the next one.
        self.chime_settled_plan_tasks();

        // The rail's rows: each carries its own click listener, built in its
        // own loop turn so the `&mut cx` borrow stays legal.
        let mut rail_rows: Vec<AnyElement> = Vec::with_capacity(self.sessions.items.len());
        // The plan chip filters rows by progress, so resolve every session's
        // counts before any pass calls `visible()` (#2719). This also warms
        // the index the badge and bar passes read below.
        self.snapshot_rail_plan_progress();
        // What every row says about work in flight (#2636), resolved once
        // before the loop: the index needs `&mut self` and the row iterator
        // holds `&self`.
        let activity = self.rail_activity_map();
        // The plan bar reads the same cached index the badges just warmed:
        // one stat per row with a plan file, no second loader.
        let plan_progress = self.rail_plan_progress_map();
        // Detached commands say "work is still running here" even when the
        // turn is idle, so every card answers it without being opened.
        let running_tasks = self.rail_running_map();
        // One clock for the whole pass, so every flashing card blinks on the
        // same frame instead of one behind its neighbour.
        let flash_now = std::time::Instant::now();
        // Telegram-style topics: a parent's children paint as compact chips
        // on its card, not as their own rows. Resolve the chip set once before
        // the loop so the paint can skip those rows and hang the strip on the
        // parent. A live search falls back to flat rows so a match is never
        // hidden inside a chip; a favorite stays a chip on its parent, never
        // a row. Chips always paint: a terminal-written `topics_collapsed`
        // flag never blanks the strip, the GUI has no row-era fold (#2701).
        let searching = !self.sessions.filter.trim().is_empty();
        let visible_ids: std::collections::HashSet<Uuid> =
            self.sessions.visible().map(|s| s.id).collect();
        let chip_children: std::collections::HashSet<Uuid> = if searching {
            std::collections::HashSet::new()
        } else {
            self.sessions
                .visible()
                .filter_map(|s| self.sessions.parents.get(&s.id).map(|p| (s.id, *p)))
                .filter(|(_, p)| visible_ids.contains(p))
                .map(|(cid, _)| cid)
                .collect()
        };
        let mut children_by_parent: HashMap<Uuid, Vec<(Uuid, String)>> = HashMap::new();
        for s in self.sessions.visible() {
            if chip_children.contains(&s.id)
                && let Some(p) = self.sessions.parents.get(&s.id)
            {
                children_by_parent.entry(*p).or_default().push((
                    s.id,
                    // A chip names its own topic, never the chat plus the
                    // topic plus the channel key (#2766): the card above it
                    // already says the chat.
                    rail::chip_name(
                        s.title.as_deref(),
                        self.sessions.topic_titles.get(&s.id).map(String::as_str),
                    ),
                ));
            }
        }
        // Pinned rows first, the rest untouched: the TUI's own order.
        for s in self.sessions.ordered() {
            let id = s.id;
            // This child paints as a chip on its parent's card, not a row.
            if chip_children.contains(&id) {
                continue;
            }
            let selected = self.sessions.selected == Some(id);
            let cursored = self.sessions.cursor == Some(id);
            let favorite = self.sessions.is_favorite(id);
            // Right-click asks for the row's menu: pin and delete without
            // the slide gesture or a chord (#2354).
            let ask_menu = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                this.sessions.rail_menu = Some(id);
                // Where the card opens from: the row's own height, so it
                // lands beside the row instead of over it (#2742).
                this.rail_menu_at = Some(ev.position);
                // The card takes the pointer, not the keyboard: the composer
                // keeps its caret while the menu is open (#2798).
                this.rail_hands_back();
                cx.notify();
            });
            let indented = self.sessions.is_indented(id);
            let closed = self.sessions.is_closed(id);
            // One glyph cell, from the window's shared clock, so a list of
            // busy rows blinks as one spinner instead of ten (#2636).
            let state = activity.get(&id).copied().unwrap_or_default();
            let badge = state
                .badge
                .map(|_| rail_activity::frame(self.rail_clock.elapsed()));
            // A selected topic owns its parent's existing activity/preview slot
            // (#2804). Cursor, plan progress and running tasks stay with the parent.
            let progress_id = children_by_parent.get(&id).map_or(id, |kids| {
                self.sessions
                    .progress_id(id, kids.iter().map(|(kid_id, _)| *kid_id))
            });
            // The visible pin and the context menu share one favorite writer
            // (#2803). Stopping the press keeps the row closed and prevents a slide.
            let toggle_pin = cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.rail_hands_back();
                this.toggle_favorite(id, cx);
            });
            let pin_toggle = sessions::pin_toggle(id, favorite)
                .on_mouse_down(MouseButton::Left, toggle_pin);
            let flags = sessions::RowFlags {
                selected,
                cursored,
                pin_toggle: Some(pin_toggle),
                indented,
                closed,
                badge,
                // Already formatted in the loader: the paint only clones a
                // shared string, so the shared clock costs no allocation.
                // While ctrl+f is open a deep hit's snippet stands in for the
                // preview so the reader sees why the row matched (#2720).
                // Idle rows read the DB preview.
                preview: self
                    .sessions
                    .content_matches
                    .get(&progress_id)
                    .filter(|_| self.sessions.filter_active)
                    .map(|snippet| SharedString::from(snippet.clone()))
                    .or_else(|| self.sessions.previews.get(&progress_id).cloned()),
                // A turn nobody has a pane for still gets a line on its card
                // (#2718). The wording is the pane's own (#2326), so a split
                // leaf and its rail row never describe the same turn two
                // different ways.
                activity: self.rail_activity_for(progress_id),
                plan: plan_progress.get(&id).copied(),
                // The bar blinks orange for a few seconds after a task in
                // this session's plan settles, on the same event that rings
                // the chime, then falls back to its resting teal.
                flash: opencrabs::utils::completion_sound::flash_blink_on(
                    self.plan_flash_until.get(&id).copied(),
                    flash_now,
                ),
                // The card's last line: whatever detached command is still
                // running here, formatted exactly as the composer strip does.
                running: running_tasks.get(&id).cloned(),
                // A card stands for its topics, so its stamp adds their spend
                // (#2807). A topic keeps its own ledger row and paints no
                // stamp of its own, which makes the card the only place the
                // money under it can be summed.
                cost: self.sessions.grouped_cost(s.total_cost, id),
                // The stored icon leads the row (#2790), resolved exactly
                // like the topic tab resolves it: an embedded mark, a
                // custom image from the view's cache, or a legacy glyph.
                // The row adds the channel mark beside it (#2800). A pick in
                // the rename dialog reaches the card without a restart, and
                // the paint never reads a file.
                icon: self.sessions.topic_icons.get(&id).and_then(|stored| {
                    topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)
                        .or_else(|| {
                            self.custom_icon_picture(stored, sessions::RAIL_MARK_REMS)
                        })
                        .or_else(|| {
                            (!stored.starts_with(topic_icons::IMG_PREFIX)).then(|| {
                                topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS)
                            })
                        })
                }),
            };
            // A parent card carries its topics as inline chips: three by
            // default, a "+N" pill expands the rest, a collapse pill folds
            // them back. Each chip opens its child on click and offers the
            // shared context menu on right-click, with its own live glyph.
            let chips = children_by_parent.get(&id).map(|kids| {
                let expanded = self.rail_chips_expanded.contains(&id);
                let limit = if expanded { kids.len() } else { 3 };
                // Each chip is measured as it is built: the strip places the
                // chips itself so the first row can stop short of the pin while
                // the rows below keep the full card width (#2809).
                let mut els: Vec<(Stateful<Div>, f32)> = Vec::new();
                for (kid_id, title) in kids.iter().take(limit) {
                    let kid_id = *kid_id;
                    let st = activity.get(&kid_id).copied().unwrap_or_default();
                    let live = st
                        .badge
                        .map(|_| rail_activity::frame(self.rail_clock.elapsed()));
                    let sel = self.sessions.selected == Some(kid_id)
                        || self.sessions.cursor == Some(kid_id);
                    let open = cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.close_rail_filter(cx);
                        // A chip is a pick like any row: while a split waits,
                        // it fills the pane instead of opening the topic in
                        // the chat leaf (#2760).
                        this.pick_or_open(kid_id, cx);
                    });
                    let ask = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        this.sessions.rail_menu = Some(kid_id);
                        // A chip sits inside its parent's row, so the press
                        // gives the card its height the same way a row's
                        // does (#2742).
                        this.rail_menu_at = Some(ev.position);
                        // The press stopped at the chip, so the caret comes
                        // back from here rather than from the window root
                        // (#2798).
                        this.rail_hands_back();
                        cx.notify();
                    });
                    let chip = sessions::topic_chip(
                        kid_id,
                        title,
                        live,
                        st.ring,
                        sel,
                        plan_progress.get(&kid_id).copied(),
                    )
                    .on_click(open)
                    .on_mouse_down(MouseButton::Right, ask);
                    let width = sessions::chip_width(
                        &*window,
                        title,
                        live.is_some() || st.ring,
                        sel,
                    );
                    els.push((chip, width));
                }
                let hidden = kids.len().saturating_sub(limit);
                if hidden > 0 {
                    let label = SharedString::from(format!("+{hidden}"));
                    let width = sessions::count_chip_width(&*window, &label);
                    let expand = cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.rail_chips_expanded.insert(id);
                        cx.notify();
                    });
                    els.push((sessions::count_chip(id, label).on_click(expand), width));
                } else if expanded && kids.len() > 3 {
                    let label = SharedString::from("\u{2212}");
                    let width = sessions::count_chip_width(&*window, &label);
                    let collapse = cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.rail_chips_expanded.remove(&id);
                        cx.notify();
                    });
                    els.push((sessions::count_chip(id, label).on_click(collapse), width));
                }
                // The pin floats at the strip's right edge, so the first row
                // hands it the space and every row below runs edge to edge
                // (#2809). The strip is packed here, where the widths and the
                // card's own width are both known.
                let widths: Vec<f32> = els.iter().map(|(_, width)| *width).collect();
                let rem = self.rem;
                let (first, rest) = sessions::strip_budgets(rem, self.rail_width * rem);
                let counts = sessions::pack_rows(&widths, rem * 0.25, first, rest);
                let mut chips = els.into_iter().map(|(chip, _)| chip);
                let rows: Vec<Vec<Stateful<Div>>> = counts
                    .into_iter()
                    .map(|count| chips.by_ref().take(count).collect())
                    .collect();
                sessions::topic_chips(id, rows)
            });
            // No menu is passed in: the card is built once at the window
            // root, because a child of the row would inherit the row's frame
            // and the card is placed in window coordinates (#2742).
            let r = sessions::row(s, flags, chips);
            // The press starts the slide that opens the card's menu.
            let press = cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                this.rail_slide_opened = None;
                this.rail_swipe = Some((id, f32::from(window.mouse_position().x)));
                // A left press on the rail dismisses a context menu: the
                // press is elsewhere, so the menu goes with it (#2354).
                this.sessions.close_rail_menu();
                // The filter panel folds on a row press too (#2787): the
                // reader picked a chat, the chips have done their job.
                this.sessions.rail_filter_open = false;
                // Same rule for a transcript row's menu (#2613): the rail is
                // outside the transcript, so the press closes it too.
                this.msg_menu = None;
                // A press is not a pick yet, and the caret does not wait for
                // one: whatever the release does, the keys are the composer's
                // from the press on (#2798).
                this.rail_hands_back();
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
                this.open_session(id, cx);
            });
            let r = r.on_mouse_down(MouseButton::Right, ask_menu);
            let r = r.on_mouse_down(MouseButton::Left, press);
            // A plan waiting for approval rings its row until it is opened.
            // The border lives on this wrapper, never inside `sessions::row`:
            // the row stays the square, edge-to-edge tile #2540 pins, and the
            // ring is a frame around it rather than a card. Wrapped after the
            // click is attached, so the row keeps the handlers it was built
            // with and only gains a frame.
            rail_rows.push(
                rail_activity::ring(
                    sessions::on_click(r, handler),
                    state.ring,
                    self.rail_clock.elapsed(),
                )
                .into_any_element(),
            );
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
        // The rail filter menu (#2719, #2787): one funnel button top-right.
        // Click opens the plan-state chips plus the cron/RSI toggle; click
        // again closes them. The funnel wears the accent while a non-default
        // filter hides rows, so a closed panel never silently filters. The
        // state lives on `sessions`, so it survives the repaint and resets
        // when the rail closes.
        let rail_filter = self.sessions.rail_filter;
        let pal = palette::current();
        let toggle = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.sessions.rail_filter_open = !this.sessions.rail_filter_open;
            cx.notify();
        });
        let mut control = div()
            .id("rail-filter-menu")
            .flex()
            .flex_col()
            .items_end()
            .gap_0p5()
            .child(
                div()
                    .id("rail-filter-toggle")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .p_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(pal.panel_hi)))
                    .child(sessions::filter_icon(
                        1.0,
                        if sessions::filter_is_active(&rail_filter) {
                            pal.accent
                        } else {
                            pal.text_muted
                        },
                    ))
                    .on_click(toggle),
            );
        if self.sessions.rail_filter_open {
            let plan_chips: [(&'static str, &'static str, sessions::PlanFilter); 4] = [
                ("rail-filter-all", "all", sessions::PlanFilter::All),
                ("rail-filter-active", "active", sessions::PlanFilter::Active),
                ("rail-filter-done", "done", sessions::PlanFilter::Done),
                (
                    "rail-filter-noplan",
                    "no plan",
                    sessions::PlanFilter::NoPlan,
                ),
            ];
            let mut chips = div().flex().flex_row().items_center().gap_0p5();
            for (id, label, plan) in plan_chips {
                let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    this.sessions.rail_filter.plan = plan;
                    cx.notify();
                });
                chips = chips.child(
                    sessions::filter_chip(id, label, rail_filter.plan == plan).on_click(pick),
                );
            }
            let auto = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.sessions.rail_filter.show_automation =
                    !this.sessions.rail_filter.show_automation;
                cx.notify();
            });
            chips = chips.child(
                sessions::filter_chip("rail-filter-auto", "auto", rail_filter.show_automation)
                    .on_click(auto),
            );
            control = control.child(chips);
        }
        let rail = sessions::rail(
            &self.sessions,
            rail_rows,
            self.rail_width,
            &self.rail_scroll,
            chip_footer,
            Some(control.into_any_element()),
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
            Screen::Models => "OpenCrabs \u{b7} models".to_string(),
            Screen::Skills => "OpenCrabs \u{b7} skills".to_string(),
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
                this.transcript.live_expanded = !this.transcript.live_expanded;
                cx.notify();
            }));
        // One click listener per attachment tile, minted here for the same
        // reason as the folds: only this `cx` can build one, and each tile has
        // to open its own file (#2395).
        let mut attachment_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for key in transcript::attachment_paths_with_live(
            &self.transcript,
            // The window holds exactly the visible conversation's turn: it was
            // parked and handed back by `swap_visible_turn`, so a live card
            // here always belongs to the session on screen (#2616).
            &self.live,
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
        let msg_order: Vec<Uuid> = self.transcript.message_ids();
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
        for id in &msg_order {
            let id = *id;
            if select_on {
                let handler = cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    this.msg_toggle(id, ev.modifiers().shift, cx);
                });
                message_click.insert(id, std::rc::Rc::new(handler));
            }
            let handler = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.msg_menu_open(id, ev.position, cx);
            });
            message_press.insert(id, std::rc::Rc::new(handler));
        }
        // A left press anywhere in the transcript dismisses the message menu
        // (#2613): the press is elsewhere, so the menu goes with it, the same
        // rule the rail applies to its own menu (#2354). The card and the rows
        // stop this from firing on their own presses.
        let msg_dismiss: transcript::PressHandler =
            Rc::new(cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                if this.msg_menu.take().is_some() {
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
            .zip(self.transcript.session)
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
                let primary = transcript::view(
                    &self.transcript,
                    &self.transcript_scroll,
                    // The visible conversation's turn, never another's: the
                    // window keeps exactly one and swaps them on a switch
                    // (#2616).
                    &self.live,
                    self.busy,
                    measure,
                    fold_handlers,
                    transcript::ViewHooks {
                        attachments: attachment_handlers,
                        drag: Some(self.copy_drag.clone()),
                        wheel: Some(wheel),
                        ctx_used: self.ctx_tokens,
                        ctx_max: self.ctx_max,
                        live_click: Some(live_click.clone()),
                        message_click: message_click.clone(),
                        message_press: message_press.clone(),
                        selection: select_on.then(|| msg_picked.clone()),
                        menu_dismiss: Some(msg_dismiss.clone()),
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
                            Some(b) => match self.transcript.session {
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
                                    .child("alt+space · seek help"),
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
                            .child("alt+space · seek help"),
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
                            .child("alt+space · seek help"),
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
                let shown = palette::current();
                // `/models` is the Brain Fuel step in the terminal
                // (`tui::onboarding::deep_link` maps the command to
                // `ProviderAuth`), so the window paints the same two lists in
                // the same order: the providers, then the models of the
                // provider the list marks. The wizard's rail, step counter and
                // key fields describe a run, not a switch, so they stay out
                // (#2582).
                let visible = models::filtered_rows(&self.models_rows, &self.models_query);
                let providers = models::providers(&visible);
                let selected = self
                    .models_provider
                    .as_deref()
                    .and_then(|id| providers.iter().position(|row| row.provider == id))
                    .unwrap_or(0);
                let (start, end) = opencrabs::tui::onboarding_render::visible_window(
                    providers.len(),
                    selected,
                    models::PROVIDERS_VISIBLE,
                );
                let mut provider_list = div().flex().flex_col().gap_1().w_full().min_w_0();
                if start > 0 {
                    provider_list = provider_list.child(text(
                        format!("\u{2191} {start} more"),
                        Size::Xs,
                        shown.text_faint,
                    ));
                }
                for (offset, row) in providers[start..end].iter().enumerate() {
                    let here = start + offset == selected;
                    let id = row.provider.clone();
                    // The cursor follows the click, so Enter and the arrows
                    // keep working from the provider the hand just chose.
                    let first = row.first;
                    let count = row.count;
                    // A provider with one model has nothing left to choose, so
                    // its tile is the switch itself (#2782).
                    let single = models::single_model_pair(&visible, row);
                    let mut entry = div()
                        .id(SharedString::from(format!("models-provider-{id}")))
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
                        .child(
                            text(
                                row.name.clone(),
                                Size::Sm,
                                if here { shown.text_strong } else { shown.text },
                            )
                            .flex_1()
                            .min_w_0()
                            .truncate(),
                        )
                        .child(text(
                            format!("{count} model{}", if count == 1 { "" } else { "s" }),
                            Size::Xs,
                            shown.text_muted,
                        ));
                    if here {
                        entry = entry.bg(rgb(shown.panel_hi));
                    }
                    provider_list = provider_list.child(entry.on_click(cx.listener(
                        move |this, _ev, _window, cx| {
                            this.models_provider = Some(id.clone());
                            this.models_index = first;
                            // One model in view means the click is the pick:
                            // the picker used to stay open on a list with a
                            // single row and nothing to confirm.
                            if let Some(pair) = single.clone() {
                                this.switch_model(pair, cx);
                            }
                            cx.notify();
                        },
                    )));
                }
                if end < providers.len() {
                    provider_list = provider_list.child(text(
                        format!("\u{2193} {} more below", providers.len() - end),
                        Size::Xs,
                        shown.text_faint,
                    ));
                }
                // The pair this screen would answer with, the same line the
                // step prints under its title. Read once: the picker needs the
                // provider for an unknown id, and the header needs both.
                let live = agent_bridge::bridge().map(|bridge| {
                    match self.models_session.or(self.transcript.session) {
                        Some(sid) => (
                            bridge.service.provider_name_for_session(sid),
                            bridge.service.provider_model_for_session(sid),
                        ),
                        None => (
                            bridge.service.provider_name(),
                            bridge.service.provider_model(),
                        ),
                    }
                });
                let pair = live
                    .as_ref()
                    .map(|(provider, model)| format!("{provider} \u{b7} {model}"))
                    .unwrap_or_default();
                let active_provider = live
                    .as_ref()
                    .map(|(provider, _)| provider.as_str())
                    .unwrap_or_default();
                let header = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(text(
                        onboard::OnboardingStep::ProviderAuth.title(),
                        Size::Xl,
                        shown.text_title,
                    ))
                    .child(text(pair, Size::Xs, shown.text_faint));
                let rendered = model_picker::render(
                    cx,
                    model_picker::ModelPickerView {
                        rows: &self.models_rows,
                        query: &self.models_query,
                        selected: self.models_index,
                        fetching: self.models_fetching,
                        pending: self.models_pending.len(),
                        error: self.models_fetch_error.as_deref(),
                        active_provider,
                        onboarding: false,
                        search_focused: true,
                        max_models: 18,
                        row_layout: picker_layout.model_row,
                        provider_label_width: picker_layout.provider_label_width,
                    },
                );
                let footer = rendered.footer.child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .min_w_0()
                        .whitespace_normal()
                        .child("type \u{b7} filter or enter a custom id \u{b7} enter \u{b7} switch \u{b7} esc \u{b7} back"),
                );
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(header)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(text("provider", Size::Xs, shown.text_faint))
                            .child(div().id("models-providers").w_full().child(provider_list)),
                    )
                    .child(rendered.body);
                (body.into_any_element(), footer.into_any_element())
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
                    let open = self.transcript.session;
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
                            queue_strip(&waiting, self.queue_expanded)
                                .id("queue-strip-chat")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut Self, _, _, cx| {
                                        if this.queued_for_strip().len() > 2 {
                                            this.queue_expanded = !this.queue_expanded;
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
                    if let Some(sid) = self.transcript.session {
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
                    // The slash-command list sits over the input while a name is
                    // not being spelled, the terminal's own placement. The name
                    // prompt itself is the overlay below (#2398). The list floats
                    // over the composer, so it lines up with it: same measure,
                    // still the child directly above the editor, so the order it
                    // paints in does not change (#2590).
                    if self.topic_draft.is_none() {
                        let popup = self.editor.read(cx).command_popup();
                        if let Some((names, selected)) = popup {
                            col = col.child(central(
                                command_popup_view(&names, selected).into_any_element(),
                                centred_chat,
                                chat_measure_px,
                            ));
                        } else if let Some((items, selected)) = self.editor.read(cx).emoji_popup() {
                            col = col.child(central(
                                emoji_popup_view(&items, selected).into_any_element(),
                                centred_chat,
                                chat_measure_px,
                            ));
                        }
                    }
                    col = col.child(central(
                        self.editor.clone().into_any_element(),
                        centred_chat,
                        chat_measure_px,
                    ));
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
            Screen::DirectoryPicker => right.child(footer),
            Screen::Onboard => right.child(footer),
            Screen::MissionControl => right.child(footer),
        };

        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
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
                let composer = this.composer_entity();
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
        // The Group topics picker: a multi select card painted above both
        // panes. Rows toggle on click, the keyboard cursor and Space mirror
        // the same selection, and only the group button writes anything.
        if self.group_target.is_some() {
            let parent_title = self
                .group_target
                .and_then(|id| self.sessions.items.iter().find(|s| s.id == id))
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "(untitled)".to_string());
            let mut list = div().id("group-pick-list").flex().flex_col().gap_0p5();
            for (idx, (sid, title, date)) in self.group_candidates.iter().enumerate() {
                let sid = *sid;
                let checked = self.group_selected.contains(&sid);
                let cursored = idx == self.group_cursor;
                let toggle = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    this.group_cursor = idx;
                    if !this.group_selected.insert(sid) {
                        this.group_selected.remove(&sid);
                    }
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette::current().text_muted))
                            .child(SharedString::from(date.clone())),
                    );
                list = list.child(row);
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
                            "Pick sessions to attach as topics. Space toggles, Enter groups.",
                        )),
                )
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
            let mut picks: Vec<Stateful<Div>> = topic_icons::TOPIC_ICONS
                .iter()
                .map(|&(name, bytes)| {
                    let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.topic_draft_icon = Some(topic_icons::store_name(name));
                        cx.notify();
                    });
                    div()
                        .id(SharedString::from(format!("topic-icon-{name}")))
                        // A pick is a picture, not a bullet (#2768): at the
                        // old 1.5rem the eight badges were specks and picking
                        // one was guesswork. They render at 4.0rem now
                        // (#2789): big enough to recognise, and the 512x512
                        // sources stay crisp at 2x.
                        .p_1()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(pick)
                        .child(topic_icons::picture(bytes, 4.0))
                })
                .collect();
            // One extra cell opens the platform file picker for a custom
            // image, Telegram style (#2790). The tile matches the embedded
            // picks' geometry so the grid keeps its rhythm.
            let add_image = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.pick_icon_image(cx);
            });
            picks.push(
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
            );
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
                picks,
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
                        this.transcript.session,
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
        if let Some(id) = self.msg_menu
            && let Some(at) = self.msg_menu_at
        {
            let items = self.msg_menu_items(id, cx);
            root = root.child(
                gpui::deferred(transcript::message_menu_popover(&items, at)).with_priority(2),
            );
        }
        // The theme modal is attached last, so it paints over every other
        // surface: while it is open it is what the user is looking at, and the
        // scrim is what the pointer meets (#2619).
        if self.themes_open {
            root = root.child(self.themes_modal(cx));
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
                        rail_clock: std::time::Instant::now(),
                        preview_refreshed_at: None,
                        rail_find_gen: 0,
                        seeker_find: HashMap::new(),
                        seeker_find_gen: 0,
                        seeker_scheduled_query: String::new(),
                        rail_chips_expanded: std::collections::HashSet::new(),
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
                        pane_drop: None,
                        copy_drag,
                        split_pick: None,
                        split_creating: false,
                        msg_menu: None,
                        msg_menu_at: None,
                        msg_select: Selection::new(),
                        forward_pick: None,
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
                        group_target: None,
                        group_candidates: Vec::new(),
                        group_selected: std::collections::HashSet::new(),
                        group_cursor: 0,
                        group_error: None,
                        group_busy: false,
                        preview: None,
                        preview_zoom: 1.0,
                        preview_video: None,
                        help_open: false,
                        topic_menu: None,
                        topic_menu_at: None,
                        topic_draft: None,
                        topic_warning: None,
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
                        themes_drag: None,
                        themes_wheel: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        themes_sat_track: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        themes_val_track: std::rc::Rc::new(std::cell::RefCell::new(None)),
                        fonts_open: false,
                        fonts_loaded: false,
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
                        onboard: None,
                        plan: plan_strip::PlanStripState::default(),
                        plan_for: None,
                        plan_expanded: std::collections::HashSet::new(),
                        announced_task_completions: std::collections::HashSet::new(),
                        plan_flash_until: HashMap::new(),
                        transcript: transcript::TranscriptState {
                            session: initial_session,
                            ..Default::default()
                        },
                        transcript_scroll: ScrollHandle::new(),
                        transcript_follow: Follow::new(),
                        ctrl_c_pending: None,
                        // Registered by `main` once the window exists: it needs
                        // a handle on this view to judge the press (#2308).
                        ctrl_c_interceptor: None,
                        rail_scroll: ScrollHandle::new(),
                        editor,
                        history: dashboard_history,
                        drafts: composer::DraftStore::new(),
                        anim_tick: 0,
                        agent_rx,
                        sink: agent_sink,
                        agent_ready: false,
                        live: transcript::LiveTurn::default(),
                        stashed: HashMap::new(),
                        settle_expect: None,
                        settle_retries: 0,
                        settle_deadline: None,
                        settle_rechecks: 0,
                        busy: false,
                        cancel: None,
                        hint_painted: None,
                        queue_expanded: false,
                        unsent: Vec::new(),
                        agent_error: None,
                        ctx_tokens: None,
                        ctx_max: 0,
                        turn_metrics: HashMap::new(),
                        pending_metrics: HashMap::new(),
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
                                    let editor = this.editor.read(cx);
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
                window.focus(&view.read(cx).editor.read(cx).focus_handle);
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
            // A line is laid out through an explicit flex row with a growing,
            // shrinkable text child (#2642): a truncated text element inside a
            // plain width-full block div is measured once at the grid's
            // flex-basis zero and stays an ellipsis forever (gpui caches the
            // nowrap layout on wrap_width, which never changes here). The
            // label/value arm below already uses this shape and renders.
            McRow::Line(line, _) => {
                cell = cell
                    .flex()
                    .flex_row()
                    .child(text(line, Size::Xs, color).flex_1().min_w_0().truncate());
            }
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

/// Rows for the phantom card. An empty model breakdown stays empty and lets
/// the shared panel render its empty state instead. (#2642 fixed the line
/// rows that used to ellipsize into three dots under the header; this guard
/// is about the data, not the rendering.)
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
///
/// Past two messages the list folds to its head and the rest is a count, so a
/// busy session cannot push the conversation off the screen; `expanded` is the
/// click that opens it (#2629).
fn queue_strip(queued: &[agent_bridge::PendingMessage], expanded: bool) -> Div {
    let folded = !expanded && queued.len() > 2;
    let shown = queue_rows(queued.len(), expanded);
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
                .child(SharedString::from(format!("queued {}", queued.len()))),
        );
    for (i, m) in queued.iter().take(shown).enumerate() {
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
    if queued.len() > shown {
        let rest = queued.len() - shown;
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(if folded {
                    format!("… {rest} more · click to expand")
                } else {
                    format!("… {rest} more")
                })),
        );
    }
    col
}

/// The follow-up options `suggest_options` offered, above the composer where
/// the queued list sits. One numbered row per label, styled the way the
/// channel buttons are: primary in the accent, danger in the red, default
/// quiet. A click sends that exact label as the next turn (#2708).
///
/// The rows arrive with their handlers already minted by the window, because
/// only it knows which session the offer belongs to. Nothing here reaches the
/// database: see `agent_bridge::SUGGESTIONS` for why there is nothing to
/// persist.
fn options_strip(rows: &[(SuggestionItem, transcript::ClickHandler)]) -> Div {
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
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from("suggested")),
        );
    for (i, (item, pick)) in rows.iter().enumerate() {
        let color = match item.style {
            SuggestionStyle::Primary => palette::current().accent,
            SuggestionStyle::Danger => palette::current().red,
            SuggestionStyle::Default => palette::current().text_muted,
        };
        let pick = pick.clone();
        col = col.child(
            div()
                .id(SharedString::from(format!("option-row-{i}")))
                .text_xs()
                .text_color(rgb(color))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                .child(SharedString::from(format!("{}. {}", i + 1, item.label)))
                .on_click(move |ev, window, app| pick(ev, window, app)),
        );
    }
    col
}

/// The composer's hint line for a state. Grey and quiet, it names the keys
/// that work right now: stopping the turn, recalling a queued message, and
/// queueing another (#2630).
fn composer_hint_for(busy: bool, queued: usize) -> SharedString {
    match (busy, queued) {
        (true, 0) => SharedString::from("esc stops · typing queues"),
        (true, n) => SharedString::from(format!("queued {n} · ↑ recalls · esc stops")),
        (false, 0) => SharedString::from("Talk to the crab…"),
        (false, n) => SharedString::from(format!("{n} queued · sending now")),
    }
}

/// How many queued messages stay on screen. Two, until the user opens the
/// fold, so a burst of background reports never owns the window (#2629).
fn queue_rows(len: usize, expanded: bool) -> usize {
    if expanded || len <= 2 { len } else { 2 }
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
fn emoji_popup_view(items: &[editor::EmojiRow], selected: usize) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel));
    for (index, row) in items.iter().enumerate() {
        let on = index == selected;
        // A crab row is one of our pictures (#2769): it has no character to
        // draw, so the row shows the picture the chat will receive.
        let face: AnyElement = match row.crab {
            Some(bytes) => topic_icons::picture(bytes, 1.6).into_any_element(),
            None => SharedString::from(row.glyph.to_string()).into_any_element(),
        };
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
                .child(face)
                .child(SharedString::from(format!(":{}:", row.shortcode))),
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

/// The card's single line for a turn running with no pane of its own (#2718).
///
/// The pane paints the lead in the accent colour and the tail muted, and a
/// card line has one colour to spend, so the halves are joined with the same
/// separator the terminal uses. Empty means the turn has said nothing yet: the
/// card then keeps its preview instead of blanking a line to show a blank.
fn rail_activity_line(live: &transcript::LiveTurn) -> String {
    // `live_status_line` invents "OpenCrabs is thinking..." for a turn that has
    // produced nothing at all, which on a card would out-shout the preview it
    // is replacing. The rail waits for the first real event (#2718).
    let idle = !live.thinking
        && live.tools.is_empty()
        && live.chunks == 0
        && live.stream.is_empty()
        && live.reasoning.is_empty();
    if idle {
        return String::new();
    }
    let (lead, tail) = transcript::live_status_line(live);
    match (lead.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail,
        (false, true) => lead,
        (false, false) => format!("{lead} \u{b7} {tail}"),
    }
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
/// window: the outgoing turn is stored whole (card, cancel handle, reload
/// handshake, prompt size) and the incoming one restored, so neither the
/// spinner nor the counters jump to a conversation that never sent anything
/// (#2616). A session with nothing in flight gets an empty slot, which is
/// what makes a switch to an idle conversation paint as idle.
///
/// A card that has ended is parked too (#2811). Gating the park on `busy` alone
/// dropped every turn the moment it stopped running, so a turn the user had
/// just stopped came back as an empty slot and the stop, the stream and the
/// verdict went with it. What decides the park is whether the card holds
/// anything the ledger cannot draw yet, which is `!live.is_empty()`; `busy`
/// rides along so the restored card knows whether anything is still moving.
#[allow(clippy::too_many_arguments)]
fn swap_visible_turn(
    stashed: &mut std::collections::HashMap<Uuid, StashedTurn>,
    from: Option<Uuid>,
    to: Uuid,
    live: &mut transcript::LiveTurn,
    busy: &mut bool,
    cancel: &mut Option<tokio_util::sync::CancellationToken>,
    settle_expect: &mut Option<usize>,
    settle_retries: &mut u8,
    ctx_tokens: &mut Option<u32>,
) {
    if from == Some(to) {
        return;
    }
    if let Some(from) = from
        && (*busy || !live.is_empty())
    {
        stashed.insert(
            from,
            StashedTurn {
                live: std::mem::take(live),
                cancel: cancel.take(),
                settle_expect: settle_expect.take(),
                settle_retries: std::mem::take(settle_retries),
                ctx_tokens: ctx_tokens.take(),
                busy: *busy,
            },
        );
    }
    match stashed.remove(&to) {
        Some(turn) => {
            *live = turn.live;
            *cancel = turn.cancel;
            *settle_expect = turn.settle_expect;
            *settle_retries = turn.settle_retries;
            *ctx_tokens = turn.ctx_tokens;
            // The flag travels with the card (#2811): a stopped turn comes
            // back stopped, not "running again".
            *busy = turn.busy;
        }
        None => {
            *live = transcript::LiveTurn::default();
            *cancel = None;
            *settle_expect = None;
            *settle_retries = 0;
            *ctx_tokens = None;
            *busy = false;
        }
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

/// The name prompt above the input, while a topic is being named (#2303).
/// The topic row (#2400, #2402): the leading button that moves it and the
/// tabs. Across the top or bottom it is one row; down a side it is one column.
///
/// The `...` and its items used to close this row (#2303). They are gone from
/// here (#2480): the menu belongs to the conversation, so it lives at the
/// chat's top right and floats over the transcript instead of growing this
/// strip by five rows when it opens.
fn topic_strip(
    key: &str,
    edge: topic_tabs::Edge,
    toggle_edge: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    tabs: Vec<AnyElement>,
) -> Div {
    // A split draws one strip per pane (#2687), so the control cannot carry a
    // fixed id: two strips sharing one would share their hover state, and a
    // hover in one pane would light the button in another.
    let id = SharedString::from(format!("topic-edge-button-{key}"));
    // The window's strip has always sat flush with the conversation's measure
    // (#2590). A pane is a fraction of that width, so its strip keeps a small
    // inset instead of the window's 24px.
    let in_pane = key != "window";
    // The orientation control shares the topic cell's generous hit target.
    let control = topic_tabs::style(edge.is_vertical());
    let separator = topic_tabs::separator_color();
    let leading = div()
        .id(id)
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
            .when(!in_pane, |d| d.mx(rems(-1.5)).px_6())
            .when(in_pane, |d| d.px_2())
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
#[allow(clippy::too_many_arguments)]
fn topic_tab(
    id: Uuid,
    label: String,
    icon: Option<AnyElement>,
    shape: topic_bar::ChipShape,
    current: bool,
    activity: bool,
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
        // Work in flight in this topic: a dot after the name, in the accent
        // the open chip uses, so the strip answers "is anything happening in
        // there" without opening it (#2762).
        .when(activity, |d| {
            d.child(
                div()
                    .size(px(6.0))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(topic_tabs::TELEGRAM_ACCENT)),
            )
        })
        .child(content)
}

/// The pill standing for the topics a narrow bar folded away (#2762). A label,
/// not a control: it says how much of the family is off the edge, and how much
/// of that is busy. Same cell geometry as a chip, so the row keeps one height.
fn overflow_chip(label: String, shape: topic_bar::ChipShape) -> AnyElement {
    let vertical = shape == topic_bar::ChipShape::IconAbove;
    let style = topic_tabs::style(vertical);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(rgb(palette::current().text_faint))
        .when(vertical, |d| d.size(rems(style.cell_rems)).p_1())
        .when(!vertical, |d| d.flex_row().gap_1().px_2().py_0p5())
        .child(SharedString::from(label))
        .into_any_element()
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

/// Whether the window-level footer row stands down (#2641). A split hands
/// that row to each pane, exactly as the terminal's status bar yields its
/// height in split view (#2575), so two footers never stack and the
/// window-wide pair can never contradict the pair the pane above is running.
fn window_footer_yields(pane_count: usize) -> bool {
    pane_count > 1
}

#[cfg(test)]
mod window_footer_tests {
    use super::window_footer_yields;

    /// The window keeps its footer while it is one pane: the row is the only
    /// place the pair, the path and the branch are named.
    #[test]
    fn one_pane_keeps_the_window_footer() {
        assert!(!window_footer_yields(1));
    }

    /// Two panes or more hand the row to the panes: each one already names its
    /// own pair, so the window row would only print a second, staler model.
    #[test]
    fn a_split_yields_the_window_footer() {
        assert!(window_footer_yields(2));
        assert!(window_footer_yields(4));
    }
}

/// A pane's status row at its bottom edge (#2593): the fields the window
/// footer carries window-wide, scoped to the pane's own session. Unfocused
/// panes draw it muted and the focused one in the accent, so feedback stays
/// visible on every panel while the pane holding the keyboard stays the only
/// one at full strength.
fn pane_footer(id: SharedString, text: Option<SharedString>, focused: bool) -> AnyElement {
    let Some(text) = text else {
        return div().into_any_element();
    };
    let color = if focused {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        // One line, and only one line: a long model name, roster or plan
        // label clipped the text at the window's edge and painted the rest
        // over the neighbouring pane. The row now clips and the text
        // ellipsizes inside it (#2620).
        .min_w_0()
        .max_w_full()
        .overflow_hidden()
        .gap_2()
        .px_2()
        .py_1()
        .text_xs()
        .whitespace_nowrap()
        .text_color(rgb(color))
        .border_t_1()
        .border_color(rgb(split_rule_color()))
        .child(div().min_w_0().overflow_hidden().truncate().child(text))
        .into_any_element()
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

/// A pane's composer while another pane holds the keyboard (#2530): the draft
/// it keeps, dimmed and inert. Clicking it hands the keyboard to that pane, so
/// the next frame draws the real field in the same spot and the typed text
/// never lands in a pane the user is not looking at.
fn pane_composer_dim(
    draft: String,
    session: Uuid,
    is_chat: bool,
    entity: gpui::WeakEntity<Dashboard>,
) -> AnyElement {
    let empty = draft.trim().is_empty();
    let shown: SharedString = if empty {
        SharedString::from("Talk to the crab\u{2026}")
    } else {
        SharedString::from(draft.replace('\n', " "))
    };
    let color = if empty {
        palette::current().text_muted
    } else {
        palette::current().text
    };
    div()
        .id(SharedString::from(format!("pane-composer-{session}")))
        .flex()
        .flex_none()
        // A pane that does not own the keyboard still shows the draft of the
        // session it holds, and a long draft used to widen the whole pane
        // instead of staying inside it: `flex_none` with no width floor lets
        // the row refuse to shrink below its text, and the column grew with
        // it (#2688). The dim row is one line by definition, so it takes the
        // pane's width, clamps to it and ellipsises the rest.
        .w_full()
        .min_w_0()
        .truncate()
        .min_h(px(44.))
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .opacity(0.55)
        .cursor(CursorStyle::IBeam)
        .text_size(px(14.))
        .text_color(rgb(color))
        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
            let _ = entity.update(cx, |this, cx| {
                this.pane_focus = if is_chat { None } else { Some(session) };
                // The next launch puts the caret back on this pane (#2675).
                this.persist_layout();
                this.refocus_screen();
                cx.notify();
            });
        })
        .child(shown)
        .into_any_element()
}

/// One pane's header: the session's name and the × that closes that pane
/// (#2474). Every pane carries one, so a split reads as two sessions rather
/// than a session beside a nameless strip, and the pane holding the caret
/// wears the accent so Tab's move is visible.
fn pane_header(
    id: SharedString,
    title: SharedString,
    focused: bool,
    draggable: bool,
    session: Uuid,
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
    let rule_color = focus_ring_color(focused);
    let header_bg = if focused {
        palette::current().panel_hi
    } else {
        palette::current().bg
    };
    // The header is the pane's grip (#2683): pressing it and moving picks the
    // whole pane up, and the drop target under the pointer decides where it
    // lands. The ghost carries the pane's title, so what follows the pointer is
    // the pane the user grabbed and not a blank chip.
    let drag_title = title.clone();
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
        // The grip needs a stateful element: `on_drag` is only offered by
        // `StatefulInteractiveElement`, and a bare `Div` has no id to key the
        // drag on, so the pane's own key rides here (#2683).
        .id(SharedString::from(format!("pane-grip-{id}")))
        .when(draggable, |d| {
            d.cursor(CursorStyle::OpenHand).on_drag(
                PaneDrag { session },
                move |_drag, _offset, _window, cx| {
                    let title = drag_title.clone();
                    cx.new(|_| PaneGhost { title })
                },
            )
        })
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

/// How far a pane is drawn back while another one owns the keyboard (#2612).
///
/// It has to read as "disabled" at a glance without hiding the transcript: at
/// 0.62 the body text still parses, and the accent ring on the live pane is
/// unmistakable beside it.
const UNFOCUSED_PANE_OPACITY: f32 = 0.62;

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

/// The chord that rotates the panes (#2580, #2579): `alt+tab` clockwise and
/// `alt+shift+tab` anticlockwise.
///
/// The window reads it through here rather than inline so the rule stays in
/// one place, [`split::rotation_chord`], where `ctrl+alt+tab` and
/// `cmd+alt+tab` are refused and a shifted tab spelled `backtab` is still
/// accepted.
fn rotation_chord(ks: &gpui::Keystroke) -> Option<split::PaneRotation> {
    split::rotation_chord(
        &ks.key,
        ks.modifiers.alt,
        ks.modifiers.control,
        ks.modifiers.platform,
        ks.modifiers.shift,
    )
}

/// The colour of the rule between two panes (#2473). `text_faint` is the
/// quietest colour that still contrasts with `bg` on every theme, where
/// `panel_hi` sat within a few points of the background on the light ones.
fn split_rule_color() -> u32 {
    palette::current().text_faint
}

/// The ring around a pane (#2612): the accent for the leaf that owns the
/// keyboard, the divider colour for the rest.
///
/// Both states carry the same width, so moving the caret never shifts a row.
/// The unfocused ring is not decoration: it is what keeps a dimmed pane
/// readable as a pane instead of as a hole in the window.
fn focus_ring_color(focused: bool) -> u32 {
    if focused {
        palette::current().accent
    } else {
        split_rule_color()
    }
}

/// The band that shows where a dragged pane will land (#2683).
///
/// It covers the half of the pane the pointer is nearest and sits inside the
/// pane's own box, so the drop is aimed before the release instead of guessed
/// after it. The tint is the accent at low alpha with a solid edge, so it reads
/// over a transcript on every theme without hiding the text under it.
fn drop_indicator(half: DropHalf) -> AnyElement {
    let accent = palette::current().accent;
    let band = div()
        .absolute()
        .bg(rgba((accent << 8) | 0x2e))
        .border_2()
        .border_color(rgb(accent));
    let band = if half.layout.is_stacked() {
        let band = band.left(px(0.)).right(px(0.)).h(relative(0.5));
        if half.after {
            band.bottom(px(0.))
        } else {
            band.top(px(0.))
        }
    } else {
        let band = band.top(px(0.)).bottom(px(0.)).w(relative(0.5));
        if half.after {
            band.right(px(0.))
        } else {
            band.left(px(0.))
        }
    };
    band.into_any_element()
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

/// The same centred column for the chat's chrome (#2588): the plan card, the
/// composer and the footer take the conversation's measure instead of the
/// pane's whole width, so the lower half of the window reads on one axis with
/// the text above it. It carries no vertical flex, because these rows keep the
/// height they ask for rather than growing into the column's slack.
fn chat_measure_row(view: AnyElement, width: f32) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .w_full()
        .min_w_0()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(width))
                .min_w_0()
                .child(view),
        )
        .into_any_element()
}

/// The chat's chrome takes the conversation's measure (#2588, #2590), so the
/// plan card, the strips, the popups, the composer and the footer all sit on
/// the axis the text above them draws on. It is a no-op wherever the transcript
/// itself is not capped, which covers every split pane and every window
/// narrower than the measure.
fn central(view: AnyElement, centred: bool, measure: f32) -> AnyElement {
    if centred {
        chat_measure_row(view, measure)
    } else {
        view
    }
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
    /// One compact footer per pane session (#2593), precomputed before the
    /// walk so a draw pass never reads the bridge or the plan file.
    footers: &'a HashMap<Uuid, SharedString>,
    /// Each pane's own plan strip, queue and composer (#2530). A leaf has no
    /// `Context`, so this is built up front in `panes_body`, where the plan
    /// files can be re-read and an editor entity read, and the walk only has
    /// to hand each leaf its own. The chat leaf gets the window's chrome, so
    /// an unsplit window never draws its composer twice.
    chrome: &'a mut HashMap<Uuid, AnyElement>,
    /// Where a dragged pane would land (#2683): the pane under the pointer and
    /// the half of it. Read by each leaf to draw its landing band.
    drop: Option<(Uuid, DropHalf)>,
    /// Each pane's own topic strip (#2686, #2687), drawn between its header
    /// and its conversation. Built in `panes_body` because a leaf has no
    /// `Context` to run a click listener from.
    tops: &'a mut HashMap<Uuid, AnyElement>,
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
            // The pane's own wheel, so a tick up releases only this pane's
            // follow and never the window's (#2698).
            let wheel_weak = ctx.entity.clone();
            let wheel_session = *session;
            let wheel = move |ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut App| {
                if !wheel_releases_follow(wheel_dy(&ev.delta)) {
                    return;
                }
                let _ = wheel_weak.update(cx, |this, cx| {
                    this.release_pane_follow(wheel_session);
                    cx.notify();
                });
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
                            &pane.live,
                            false,
                            if width_share < 1.0 {
                                split_pane_width(ctx.content, false, width_share)
                            } else {
                                ctx.content
                            },
                            HashMap::new(),
                            transcript::ViewHooks {
                                // The pane's own drag-to-copy (#2623): the
                                // gesture was dead in every split because this
                                // was `default()`, which leaves `drag: None`.
                                drag: Some(pane.copy_drag.clone()),
                                wheel: Some(Rc::new(wheel)),
                                ..Default::default()
                            },
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
            // Clicking anywhere in a pane takes the keyboard, not only the dim
            // composer (#2610). The pane under the pointer is the pane the next
            // keystroke belongs to, and a click that only highlighted a row
            // left the caret where it was.
            let reach = ctx.entity.clone();
            let reach_session = *session;
            let take_keyboard = move |_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                let next = if is_chat { None } else { Some(reach_session) };
                let _ = reach.update(cx, |this, cx| {
                    if this.pane_focus != next {
                        this.pane_focus = next;
                        // A click that takes the keyboard changes which pane
                        // the next launch puts the caret on (#2675).
                        this.persist_layout();
                        this.refocus_screen();
                        cx.notify();
                    }
                });
            };
            let interactive = ctx.interactive;
            // Where a dragged pane would land if released here (#2683). Only
            // the target draws a band, and only while a pane is in flight.
            let landing = ctx
                .drop
                .filter(|(target, _)| target == session)
                .map(|(_, half)| half);
            let hover_weak = ctx.entity.clone();
            let hover_session = *session;
            let drop_weak = ctx.entity.clone();
            let drop_session = *session;
            let mut node = div()
                .id(SharedString::from(format!("pane-{session}")))
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                // The leaf that owns the keyboard wears the accent all the way
                // round, and the leaves that do not are drawn back so a split
                // reads as one live pane beside disabled ones (#2612). Same
                // border width on both states, so focus never shifts a row.
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(focus_ring_color(focused)))
                .when(!focused, |d| d.opacity(UNFOCUSED_PANE_OPACITY))
                .when(interactive, |d| {
                    // Capture, not bubble. A transcript row stops the press
                    // on its way up (`cx.stop_propagation()` in the text-row
                    // handler), so a bubble-phase listener never ran for a
                    // click on text, which is most of a pane: the caret
                    // stayed on another leaf and ctrl+| split that one while
                    // the window called the pane just clicked focused
                    // (#2758).
                    d.capture_any_mouse_down(take_keyboard)
                        // Every pane is a drop target (#2683). The half the
                        // pointer is nearest picks the axis and the side while
                        // the drag is still in flight, so the band under it is
                        // the layout the release will produce.
                        .on_drag_move::<PaneDrag>(move |ev, _window, cx| {
                            let from = ev.drag(cx).session;
                            let b = ev.bounds;
                            let bounds = [
                                f32::from(b.origin.x),
                                f32::from(b.origin.y),
                                f32::from(b.origin.x + b.size.width),
                                f32::from(b.origin.y + b.size.height),
                            ];
                            let x = f32::from(ev.event.position.x);
                            let y = f32::from(ev.event.position.y);
                            // gpui delivers `on_drag_move` to EVERY pane while
                            // a drag is live, hovered or not (the dispatch has
                            // no `is_hovered` gate, unlike `on_mouse_move`), so
                            // each pane used to answer with its own bounds and
                            // the last pane painted won: the band landed on a
                            // pane the pointer was nowhere near, and its half
                            // described that pane's nearest edge instead of the
                            // one under the pointer (#2759).
                            let inside = x >= bounds[0]
                                && x <= bounds[2]
                                && y >= bounds[1]
                                && y <= bounds[3];
                            let _ = hover_weak.update(cx, |this, cx| {
                                if inside {
                                    // A pane dropped on itself is not a move,
                                    // so it never shows a landing band of its
                                    // own.
                                    let next = (from != hover_session)
                                        .then(|| (hover_session, drop_half(bounds, x, y)));
                                    if this.pane_drop != next {
                                        this.pane_drop = next;
                                        cx.notify();
                                    }
                                } else if this
                                    .pane_drop
                                    .is_some_and(|(target, _)| target == hover_session)
                                {
                                    // Clear only a band this pane owns, so a
                                    // pane hearing the move does not wipe the
                                    // band the pane under the pointer set.
                                    this.pane_drop = None;
                                    cx.notify();
                                }
                            });
                        })
                        .on_drop::<PaneDrag>(move |drag, _window, cx| {
                            let from = drag.session;
                            let _ = drop_weak.update(cx, |this, cx| {
                                // The half the last move recorded here, or the
                                // right half when the pointer arrived and left
                                // without a move of its own.
                                let half = this
                                    .pane_drop
                                    .filter(|(target, _)| *target == drop_session)
                                    .map(|(_, half)| half)
                                    .unwrap_or(DropHalf {
                                        layout: Layout::Side,
                                        after: true,
                                    });
                                this.pane_drop = None;
                                let moved = this
                                    .panes
                                    .as_mut()
                                    .is_some_and(|tree| tree.move_leaf(from, drop_session, half));
                                if moved {
                                    // A move is the layout the user chose, so
                                    // the release is when it gets written
                                    // (#2683, #2675).
                                    this.persist_layout();
                                }
                                cx.notify();
                            });
                        })
                })
                .child(pane_header(
                    SharedString::from(format!("pane-close-{session}")),
                    title,
                    focused,
                    interactive,
                    *session,
                    close,
                ))
                // The pane's topics sit inside its box, under its name
                // (#2687): a strip above the whole wall described the chat
                // while the panes it hung over held something else.
                .when_some(ctx.tops.remove(session), |d, top| {
                    d.child(div().flex().flex_none().min_w_0().child(top))
                })
                // The transcript takes the slack, the strips and the composer
                // below it keep their height (#2624). Without the wrapper the
                // conversation's own min height won the shrink and the plan
                // strip was squeezed to nothing in a short pane, while the
                // composer survived because its field has a min size: the plan
                // card vanished exactly where the panes got small.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(body),
                );
            // The pane's own plan, queue and composer sit between its
            // transcript and its footer, where the window-level ones sit in
            // an unsplit window (#2530). `flex_none` keeps every one of them
            // at its own height: a shrinkable stack lost its top rows first.
            if let Some(chrome) = ctx.chrome.remove(session) {
                node = node.child(div().flex().flex_col().flex_none().min_w_0().child(chrome));
            }
            node.child(pane_footer(
                SharedString::from(format!("pane-footer-{session}")),
                ctx.footers.get(session).cloned(),
                focused,
            ))
            // Painted last, so the landing band covers the pane's own rows
            // instead of hiding under its transcript (#2683).
            .when_some(landing, |d, half| d.child(drop_indicator(half)))
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
    custom_picture: Option<AnyElement>,
    pick_image: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
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
    // stored glyph when the row predates the picture set (#2478). A custom
    // image arrives already built from the view's cache (#2790); an img:
    // reference never prints as a label, even when its file is missing.
    let picture = custom_picture.or_else(|| {
        topic_icons::topic_icon(icon)
            .or_else(|| topic_icons::crab_icon(icon))
            .map(|bytes| topic_icons::picture(bytes, 3.2).into_any_element())
    });
    // The badge is the session's own image, so it doubles as the upload
    // trigger: clicking it opens the same file picker the add-image cell
    // opens (#2790). The picture itself is the button, Telegram style.
    let icon_badge = div()
        .id("topic-prompt-image")
        .flex_none()
        // The badge carries the same picture the picker showed, so it has to
        // be big enough to recognise (#2768, #2789).
        .size(rems(4.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().bg))
        .text_size(rems(1.4))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(pick_image)
        .when(
            picture.is_none() && !icon.starts_with(topic_icons::IMG_PREFIX),
            |d| d.child(SharedString::from(icon.to_string())),
        )
        .children(picture);
    let picker = div().flex().flex_row().flex_wrap().gap_1().children(picks);
    let panel = div()
        .id("topic-prompt-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_3()
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

/// What the seeker's topic pick means (#2604, #2605). The window already
/// holds both facts the answer needs, so the branch is testable without one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TopicPick {
    /// The pick named a topic, and a topic cannot hold topics.
    Warn,
    /// The conversation has topics on, so the prompt can open.
    Draft,
    /// The conversation never had topics on: write the flag, read it back,
    /// then open the prompt.
    EnableThenDraft,
}

fn topic_pick(is_topic: bool, topics_enabled: bool) -> TopicPick {
    if is_topic {
        TopicPick::Warn
    } else if topics_enabled {
        TopicPick::Draft
    } else {
        TopicPick::EnableThenDraft
    }
}

/// A warning the window has to say before it does anything else (#2605). The
/// header bar names the app and the kind, so a card that stopped a pick is
/// never read as a progress notice, and one button is the whole answer.
fn warning_overlay(
    body: &str,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let p = palette::current();
    div()
        .id("warning-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(
            div()
                .id("warning-panel")
                .occlude()
                .flex()
                .flex_col()
                .w(rems(26.0))
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(p.panel_hi))
                .bg(glass_fill(p.panel))
                .overflow_hidden()
                .child(
                    div()
                        .id("warning-header")
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(p.panel_hi))
                        .bg(rgb(p.panel_hi))
                        .text_sm()
                        .text_color(rgb(p.warn))
                        .child(SharedString::from("OpenCrabs - Warning")),
                )
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(p.text))
                                .child(SharedString::from(body.to_string())),
                        )
                        .child(
                            div().flex().flex_row().justify_end().child(
                                div()
                                    .id("warning-ok")
                                    .px_3()
                                    .py_1()
                                    .rounded(px(4.0))
                                    .text_xs()
                                    .cursor_pointer()
                                    .text_color(rgb(p.accent))
                                    .hover(|d| d.bg(rgb(p.panel_hi)))
                                    .on_click(dismiss)
                                    .child(SharedString::from("ok")),
                            ),
                        ),
                ),
        )
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
    use super::{CHAT_MEASURE_PX, cd_entries_for, pick_initial_session, remember_open_session};

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
        let src = include_str!("main.rs");
        let (_, rest) = src
            .split_once(concat!("fn pane_", "element("))
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

    /// The theme modal is a centred overlay over the conversation: a scrim
    /// that dismisses, four cards, and no Keep button, because a pick saves at
    /// once. The sidebar, its row helper and its preview history went with the
    /// screen that hosted them (#2619).
    ///
    /// A source scan, because the rule is about wiring: the modal needs a
    /// window and a click to exercise.
    #[test]
    fn theme_modal_is_centred_and_saves_on_the_pick() {
        let src = include_str!("main.rs");
        let view = include_str!("themes_view.rs");
        // The scrim lives with the modal it belongs to; the wiring that
        // opens it lives with the dashboard.
        let scrim = [".id(\"theme-modal-", "scrim\")"].concat();
        let modal = ["themes_view::", "modal("].concat();
        let card = [
            "themes_view::card(mode, label, hint, self.themes_card == ",
            "index, accent)",
        ]
        .concat();
        let cards = [
            "for (index, &(mode, label, hint)) in themes_view::CARDS",
            ".iter().enumerate()",
        ]
        .concat();
        let close = [".id(\"theme-modal-", "close\")"].concat();
        assert!(view.contains(&scrim), "the scrim is what dismisses");
        assert!(
            view.contains(&close),
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

        // Two arming paths: the row's menu, and ctrl+d on the
        // cursor row (#2352). Both ask the dialog; arming is not deleting.
        // The needles are split so the scan cannot count its own source text.
        let strip_arm = ["this.request", "_delete(id, cx);"].concat();
        let chord_arm = ["self.request", "_delete(target, cx);"].concat();
        assert!(
            src.contains(&strip_arm),
            "the row menu must arm the dialog"
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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

        let mut stashed: std::collections::HashMap<uuid::Uuid, super::StashedTurn> =
            std::collections::HashMap::new();
        let mut live = super::transcript::LiveTurn::default();

        // A turn running in conversation A, with every piece of state the
        // card needs to look alive. Declared already carrying it, so the test
        // reads as the turn it describes and never as a blank that is filled
        // in later.
        live.begin("ask A".to_string());
        live.thinking = true;
        live.tokens = 42;
        let mut busy = true;
        let mut cancel = Some(tokio_util::sync::CancellationToken::new());
        let mut settle_expect: Option<usize> = Some(7);
        let mut settle_retries: u8 = 2;
        let mut ctx_tokens: Option<u32> = Some(1_234);

        super::swap_visible_turn(
            &mut stashed,
            Some(a),
            b,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );

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
        assert_eq!(stashed[&a].settle_expect, Some(7));
        assert!(!busy, "the conversation now on screen is idle");
        assert!(live.user.is_none(), "B never inherits A's card");
        assert_eq!(ctx_tokens, None, "nor A's context counter");

        // B runs its own turn, then the user goes back to A.
        live.begin("ask B".to_string());
        busy = true;
        super::swap_visible_turn(
            &mut stashed,
            Some(b),
            a,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );

        assert_eq!(
            live.user.as_deref(),
            Some("ask A"),
            "A's card comes back, so its tool block and counters keep ticking"
        );
        assert!(live.thinking);
        assert_eq!(live.tokens, 42);
        assert!(busy, "A is still running");
        assert!(cancel.is_some(), "A's cancel token came back with it");
        assert_eq!(settle_expect, Some(7));
        assert_eq!(settle_retries, 2);
        assert_eq!(ctx_tokens, Some(1_234));
        assert_eq!(stashed.len(), 1);
        assert_eq!(
            stashed[&b].live.user.as_deref(),
            Some("ask B"),
            "B's turn was parked on the way out"
        );

        // Opening the conversation already on screen is not a switch: nothing
        // is parked and the running turn is left alone.
        super::swap_visible_turn(
            &mut stashed,
            Some(a),
            a,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );
        assert_eq!(stashed.len(), 1, "a no-op switch parks nothing");
        assert_eq!(live.user.as_deref(), Some("ask A"));
        assert!(busy);
    }

    /// #2811: stopping a turn clears `busy` immediately, and the park used to
    /// be gated on `busy` alone. A turn the user had just stopped was therefore
    /// dropped on the way out of the session and came back as an empty pane,
    /// which is why the stop looked like it had never happened.
    #[test]
    fn a_stopped_card_survives_a_switch_without_claiming_it_is_running() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let mut stashed: std::collections::HashMap<uuid::Uuid, super::StashedTurn> =
            std::collections::HashMap::new();
        let mut live = super::transcript::LiveTurn::default();
        live.begin("ask A".to_string());
        live.stream = "half an answer".to_string();
        live.stopped = true;
        // What the stop already did: nothing is running any more.
        let mut busy = false;
        let mut cancel: Option<tokio_util::sync::CancellationToken> = None;
        let mut settle_expect: Option<usize> = None;
        let mut settle_retries: u8 = 0;
        let mut ctx_tokens: Option<u32> = None;

        super::swap_visible_turn(
            &mut stashed,
            Some(a),
            b,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );

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

        super::swap_visible_turn(
            &mut stashed,
            Some(b),
            a,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );

        assert_eq!(
            live.stream, "half an answer",
            "the stream the user saw came back with the card"
        );
        assert!(live.stopped, "and so did the fact it was stopped");
        assert!(
            !busy,
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
        let mut stashed: std::collections::HashMap<uuid::Uuid, super::StashedTurn> =
            std::collections::HashMap::new();
        let mut live = super::transcript::LiveTurn::default();
        let mut busy = false;
        let mut cancel: Option<tokio_util::sync::CancellationToken> = None;
        let mut settle_expect: Option<usize> = None;
        let mut settle_retries: u8 = 0;
        let mut ctx_tokens: Option<u32> = None;

        super::swap_visible_turn(
            &mut stashed,
            Some(a),
            b,
            &mut live,
            &mut busy,
            &mut cancel,
            &mut settle_expect,
            &mut settle_retries,
            &mut ctx_tokens,
        );

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
        let src = include_str!("main.rs");
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
        // paints from its own `SplitPane::live`, and the third window
        // handover was the Themes preview leaf the centered modal replaced
        // (#2619).
        assert_eq!(
            production.matches("&self.live,").count(),
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
        let src = include_str!("main.rs");
        let arm = src
            .split("agent_bridge::TurnEvent::TurnResumed {")
            .nth(1)
            .expect("the resumed-turn arm");
        let arm = arm
            .split("agent_bridge::TurnEvent::BackgroundTaskDone")
            .next()
            .expect("the end of the arm");
        assert!(
            arm.contains("self.live.resume(user_message)"),
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
        assert!(arm.contains("self.busy = true"), "a recovered turn is busy");
        assert!(
            arm.contains("self.cancel = Some(cancel)"),
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let pane = src
            .split("fn pane_element(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("pane element");
        assert!(
            !pane.contains("ViewHooks::default()"),
            "a pane must hand its transcript a drag state"
        );
        assert!(
            pane.contains("drag: Some(pane.copy_drag.clone())"),
            "the pane's own drag state is wired to its transcript"
        );
        let state = src
            .split("struct SplitPane {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("split pane");
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
        let code = include_str!("main.rs")
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
        let code = include_str!("main.rs")
            .split("\nmod tests {")
            .next()
            .expect("the production half of the file");
        assert!(
            code.contains("wheel_releases_follow(wheel_dy(&ev.delta))"),
            "the wheel is judged by the shared rule, never a second one"
        );
        assert!(
            code.contains("this.release_transcript_follow();"),
            "a tick over the conversation releases the window's follow"
        );
        assert!(
            code.contains("this.release_pane_follow(wheel_session);"),
            "a tick over a pane releases that pane's follow, not the window's"
        );
        assert_eq!(
            code.matches("wheel: Some(Rc::new(wheel))").count(),
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
        let src = include_str!("main.rs");
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
            branch.contains("(Some(id), false) => self.release_pane_follow(id)"),
            "a page up releases the pane it scrolled"
        );
    }

    #[test]
    fn a_panel_that_closes_hands_the_chat_back_at_its_newest_row() {
        // #2698: coming back from a panel is a return, not a resume. The
        // screen transition is the one place every panel funnels through, so
        // the re-arm lives there instead of in each close path.
        let src = include_str!("main.rs");
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
        item_body(
            src,
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

        let src = include_str!("main.rs");
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
        let render = dashboard_render(src);
        assert!(
            render.contains("activity: self.rail_activity_for(id)"),
            "the render feeds the card from whichever owner holds the turn"
        );
        let lookup = fn_body(src, "rail_activity_for");
        for source in [
            "self.pane_states.get(&id)",
            "self.stashed.get(&id)",
            "self.busy && self.transcript.session == Some(id)",
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

    #[test]
    fn the_menu_opens_at_the_pointer_that_asked_for_it() {
        // #2690: the press point is the anchor. The card used to be placed
        // beside the rail at the press height, which is not where the click
        // happened: a chip sits mid-row, and in a split the rail edge is not
        // the pane the user was looking at.
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let asks = dashboard_render(src)
            .matches("this.rail_menu_at = Some(ev.position);")
            .count();
        assert_eq!(
            asks, 3,
            "a rail row, a topic chip and the swipe each record their press"
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
        let src = include_str!("main.rs");
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
    fn a_row_menu_split_grows_from_the_focused_pane() {
        // #2625: the row menu named the session, so opening it as the chat
        // first and then asking again grew the split from the first pane.
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let render = dashboard_render(src);
        assert!(
            !render.contains("this.open_session(kid_id, cx)"),
            "the chip must not open the topic directly"
        );
        assert!(
            render.contains("this.pick_or_open(kid_id, cx)"),
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let body = fn_body(src, "begin_split");
        assert!(
            body.contains("self.sessions.cursor = Some(target);"),
            "the cursor starts on the pane being split"
        );
        assert!(
            !body.contains("self.sessions.cursor = self.transcript.session;"),
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
        let src = include_str!("main.rs");
        let body = fn_body(src, "begin_split");
        assert!(
            body.contains("let Some(target) = self.split_origin() else"),
            "the chord door splits the pane that holds the caret"
        );
        assert!(
            !body.contains("self.pane_focus.or(self.transcript.session)"),
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
        let src = include_str!("main.rs");
        let body = src
            .split("fn panes_body(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("panes body");
        assert!(
            body.contains("self.plan.pane_view(density, expanded)"),
            "the chat leaf in a split draws the compact strip"
        );
        assert!(
            body.contains("pane.plan.pane_view(density, expanded)"),
            "every pane draws the compact strip"
        );
        assert!(
            body.contains("plan_strip::pane_density(count, pane_width)"),
            "the shape follows the pane count and the pane's own width"
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
        let src = include_str!("main.rs");
        let body = src
            .split("fn pane_element(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("pane element");
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
        let src = include_str!("main.rs");
        let body = src
            .split(".on_drag_move::<PaneDrag>(")
            .nth(1)
            .and_then(|rest| rest.split(".on_drop::<PaneDrag>(").next())
            .expect("the drag-move listener");
        assert!(
            body.contains("let inside = x >= bounds[0]"),
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
            body.contains(".is_some_and(|(target, _)| target == hover_session)"),
            "a pane clears only the band it owns"
        );
    }

    #[test]
    fn every_pane_keeps_its_strips_at_their_own_height() {
        // #2624: the transcript takes the slack and the strips keep their
        // height. Without this the plan card was squeezed to zero in a short
        // pane, which is why it only ever vanished in a split.
        let src = include_str!("main.rs");
        let body = src
            .split("fn pane_element(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("pane element");
        assert!(
            body.contains(".flex_none().min_w_0().child(chrome)"),
            "the chrome stack is not shrinkable"
        );
        assert!(
            body.contains(
                "div()\n                        .flex()\n                        .flex_col()\n                        .flex_1()\n                        .min_w_0()\n                        .min_h_0()\n                        .child(body)"
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
        let src = include_str!("main.rs");
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
            branch.contains("self.busy = false;"),
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
            production.contains("if !self.busy {\n            self.cancel = None;\n            self.drain_pending(cx);"),
            "the turn's own event drains the queue once the cancel is acknowledged"
        );
    }

    /// With nothing queued Esc is only a stop: the wording stays the plain
    /// cancellation and the drain finds nothing to send (#2631).
    #[test]
    fn esc_with_no_queue_only_cancels() {
        let src = include_str!("main.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        assert!(
            production.contains("self.live.notices.push(\"Operation cancelled.\".to_string());"),
            "an empty queue keeps the plain wording"
        );
        let drain = production
            .split("    fn drain_pending(")
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
        let src = include_str!("main.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let settle = production
            .split("    fn settle_pane(")
            .nth(1)
            .and_then(|rest| rest.split("    /// The pane's turn failed").next())
            .expect("settle_pane");
        assert!(
            settle.contains("self.drain_pane_pending(session, cx);"),
            "a settled pane flushes its queue"
        );
        let fail = production
            .split("    fn fail_pane(")
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
            .split("    fn drain_pane_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
            .expect("drain_pane_pending");
        assert!(
            drain.contains("if pane.busy {"),
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
        let src = include_str!("main.rs");
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
            cancelled.contains("self.live.stopped = true;"),
            "the card must say the turn was stopped instead of spinning"
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
        let src = include_str!("main.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let send = production
            .split("    fn send_from_pane(")
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
            .split("    fn send_turn_to(")
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
        let src = include_str!("main.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let reload = production
            .split("    fn reload_pane(")
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
        let src = include_str!("main.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        let window_drain = production
            .split("    fn drain_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Create a conversation").next())
            .expect("drain_pending");
        assert!(
            window_drain.contains("queue flush: session"),
            "the window's flush logs its joined send"
        );
        let pane_drain = production
            .split("    fn drain_pane_pending(")
            .nth(1)
            .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
            .expect("drain_pane_pending");
        assert!(
            pane_drain.contains("pane queue flush: session"),
            "the pane's flush logs its joined send"
        );
        let stop = production
            .split("    fn cancel_focused_pane_turn(")
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
        let src = include_str!("main.rs");
        let footer = src
            .split("fn pane_footer_text(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("pane footer text");
        assert!(
            !footer.contains('\u{23f3}') && !footer.contains('\u{231b}'),
            "the footer carries no task strip"
        );
    }

    #[test]
    fn the_pane_footer_clips_instead_of_overflowing() {
        let src = include_str!("main.rs");
        let footer = src
            .split("fn pane_footer(id: SharedString")
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
        // GLOBAL's "List sessions" row is that command. p and z are absent on
        // purpose: the window has no projects screen (#2669) and answers no z
        // since the fold tree became chips (#2701), so neither advertises a
        // window chord.
        let wanted = [
            ("up/down", "Move the highlight in the list"),
            ("enter", "Open the highlighted session"),
            ("ctrl+f", "Find a session"),
            ("ctrl+shift+p", "Pin the selected session"),
            ("ctrl+d", "Delete session (asks first)"),
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
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
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
    fn chat_footer_points_at_the_seeker() {
        // The footer stopped being a hand-maintained keymap (#2321). It carries
        // live state (provider, model, directory, branch, tokens per second,
        // ctx budget) and exactly one hint: the door to the Seeker card. A
        // strip of chords taught keys the window no longer binds and went
        // stale the moment a binding moved, so every screen answers with the
        // same three words instead.
        let src = include_str!("main.rs");
        let door = ["\"alt+space · ", "seek help\""].concat();
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
        // #2665 split the door in two: `open_session` commits and
        // `open_session_handing_back` makes the keyboard decision. The open
        // still lands in `begin_conversation`, one link further down.
        let handing_back = ["open_session_", "handing_back(id, true, cx)"].concat();
        assert!(
            door.contains(&handing_back),
            "open_session commits through open_session_handing_back"
        );
        let (_, back) = src
            .split_once(
                ["fn open_session_", "handing_back(&mut self, id: Uuid"]
                    .concat()
                    .as_str(),
            )
            .expect("open_session_handing_back exists");
        let (back_body, _) = back
            .split_once("\n    fn ")
            .expect("the next method ends it");
        assert!(
            back_body.contains(&delegate),
            "open_session_handing_back routes through begin_conversation"
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

    /// The window and every split pane walk ONE history, and it is the file
    /// the terminal reads, so recall shows the same past whichever composer
    /// has the caret (#2494).
    #[test]
    fn gui_composer_history_is_one_shared_list() {
        let src = include_str!("main.rs");
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
        let src = include_str!("main.rs");
        let (_, helper) = src
            .split_once(&["fn begin_", "conversation(&mut self, id: Uuid"].concat())
            .expect("begin_conversation exists");
        let (body, _) = helper
            .split_once("\n    fn ")
            .expect("the next method ends it");
        let filed = ["self.drafts.", "store(previous, draft)"].concat();
        assert!(body.contains(&filed), "leaving files the draft");
        let back = ["self.drafts.", "take(id)"].concat();
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
        let src = include_str!("main.rs");
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
            arm.contains("self.running_here(session_id)"),
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
        let src = include_str!("main.rs");
        let branch = src
            .find("self.ctrl_c_press(cx);")
            .expect("the Ctrl+C branch is in on_key");
        let gate = src
            .find("if self.editor.read(cx).focus_handle.is_focused(window)")
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
        let src = include_str!("main.rs");
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
