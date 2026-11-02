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
mod commands;
mod editor;
mod icon;
mod logging;
mod markdown;
mod models;
mod onboard;
mod palette;
mod plan_strip;
mod preview;
mod screen_keys;
mod sessions;
mod theme;
mod themes_view;
mod topic_tabs;
mod transcript;
mod usage_view;

use crate::screen_keys::{
    ChordMods, FocusTarget, ScreenAction, action_label, focus_target, screen_chord,
};
use crate::theme::{BASE_REM, Size, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP, text};
use opencrabs::ui_core::split::{self, Layout};
use transcript::{LiveTurn, TranscriptState};
// The terminal's own command table, shared rather than copied.
use opencrabs::ui_core::commands as slash;

use futures::channel::oneshot;

use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Div, DragMoveEvent, Entity,
    ExternalPaths, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Stateful,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px, rems, rgb, rgba,
    size,
};
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::utils::drop_landing::human_size;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use uuid::Uuid;

use opencrabs::config::Config;
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
}

/// The second pane of a split: its session, transcript, scroll and layout (#2438).
struct SplitPane {
    session: uuid::Uuid,
    /// The session's title as the rail shows it, for the pane header.
    title: String,
    transcript: TranscriptState,
    scroll: ScrollHandle,
    layout: Layout,
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

/// The side panel beside a previewing screen (`themes-sidebar`), plus the
/// `gap_3` between it and the transcript. The transcript is planned against
/// what is left, so its tiles never run under the panel.
const THEMES_PANEL_WIDTH: f32 = 260.0;

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
    /// Monotonic tag on the latest period fetch; only its result is applied,
    /// so a slow fetch landing after a newer one cannot paint stale data.
    pending: u64,
    /// The session-files browser (ctrl+g): rows for the open conversation and
    /// the session they were fetched for, so a slow fetch landing after a
    /// rail click paints nothing.
    files: Vec<agent_bridge::SessionFile>,
    files_for: Option<Uuid>,
    files_err: Option<SharedString>,
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
    /// The second pane of a split, when one is open (#2438).
    split: Option<SplitPane>,
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
    /// The `...` menu over the chat is open (#2303). Its only item is New topic.
    topic_menu: bool,
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
    /// The model picker's rows, built when the screen opens so a slow frame
    /// never rebuilds the catalogue.
    models_rows: Vec<models::Row>,
    /// The row the arrows stand on.
    models_index: usize,
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

impl Dashboard {
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

    /// Open one conversation: select it in the rail, switch the right pane to
    /// chat, and load its newest window off the UI thread. Every door (rail
    /// click, launch, ctrl+u return) goes through here, so the screen and the
    /// session can never disagree.
    fn open_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.sessions.selected = Some(id);
        if self.transcript.session != Some(id) {
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
        }
        self.transcript.session = Some(id);
        self.transcript.error = None;
        self.transcript.turns = Vec::new();
        self.transcript.hidden = 0;
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

    /// Ask the next frame to put focus where the new screen expects it.
    ///
    /// A transition calls this instead of touching focus itself: only the
    /// render holds a window to focus through, and this is the one place that
    /// decides which element a screen owns (#2350, #2356).
    fn refocus_screen(&mut self) {
        self.refocus = Some(focus_target(self.screen));
    }

    /// Run what the screen table named. The table decides and this applies, so
    /// the routing is unit tested and the effect stays in one place.
    fn apply_screen_action(&mut self, action: ScreenAction, cx: &mut Context<Self>) {
        match action {
            ScreenAction::EnterUsage | ScreenAction::LeaveUsage => self.toggle_usage(cx),
            ScreenAction::EnterFiles | ScreenAction::LeaveFiles => self.toggle_files(cx),
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
            commands::GuiAction::ToggleHelp => self.toggle_help(cx),
            commands::GuiAction::NewSession => self.new_session(cx),
            commands::GuiAction::StopTurn => self.stop_turn(cx),
            commands::GuiAction::OpenSessions => self.toggle_rail(cx),
            commands::GuiAction::OpenModels => self.open_models(arg, cx),
            commands::GuiAction::OpenOnboard(name) => self.open_onboard(name, arg, cx),
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
    /// Start a split: open the sidebar, focus it, and ask for the second
    /// session. The chat's own session stays on the left or top (#2438).
    fn begin_split(&mut self, layout: Layout, cx: &mut Context<Self>) {
        if self.transcript.session.is_none() {
            self.live
                .notices
                .push("Open a session before splitting it.".to_string());
            cx.notify();
            return;
        }
        if !self.rail_open {
            self.toggle_rail(cx);
        }
        self.split_pick = Some(layout);
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
        self.rail_focused = false;
        self.refocus_screen();
        cx.notify();
    }

    /// Fill the second pane with `id` and load its newest messages (#2438).
    fn choose_split(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(layout) = self.split_pick else {
            return;
        };
        if self.transcript.session == Some(id) {
            self.live
                .notices
                .push("That session is already in the chat. Pick another.".to_string());
            cx.notify();
            return;
        }
        self.split_pick = None;
        self.rail_focused = false;
        self.refocus_screen();
        let title = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "(untitled)".to_string());
        self.split = Some(SplitPane {
            session: id,
            title,
            transcript: TranscriptState {
                session: Some(id),
                ..Default::default()
            },
            scroll: ScrollHandle::new(),
            layout,
        });
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = transcript::load_recent(db_path, id).await;
            this.update(cx, |this, cx| {
                if let Some(pane) = this.split.as_mut().filter(|p| p.session == id) {
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

    /// N in the pick: a new session that fills the second pane (#2438).
    fn split_new(&mut self, cx: &mut Context<Self>) {
        self.split_creating = true;
        self.new_session(cx);
    }

    fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        if self.rail_open {
            self.close_rail_filter(cx);
        }
        self.rail_open = !self.rail_open;
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
        self.models_rows = models::rows(
            current
                .as_ref()
                .map(|(provider, model)| (provider.as_str(), model.as_str())),
        );
        if let Some(arg) = arg {
            self.switch_model(arg, cx);
            return;
        }
        // The highlight lands on the live pair, so Enter on the screen is a
        // no-op instead of an accidental switch.
        self.models_index = self
            .models_rows
            .iter()
            .position(|row| row.current)
            .unwrap_or(0);
        self.screen = Screen::Models;
        self.rail_actions = None;
        self.sessions.rail_menu = None;
        self.refocus_screen();
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
        let mut confirm = false;
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
                    // Space is the terminal's toggle on this list. On a text field
                    // it is an ordinary character, so it still types one there.
                    if !ob.toggle_focused_channel()
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
                _ => {}
            }
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
        self.screen = Screen::Onboard;
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
            Screen::Models => self.screen = Screen::Usage,
            // Usage from the wizard: the screen closes first, so the ledger is
            // not left behind a half-filled form.
            Screen::Onboard => {
                self.onboard = None;
                self.screen = Screen::Usage;
            }
            Screen::Themes => self.close_themes(true, cx),
        }
        self.refocus_screen();
        cx.notify();
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
            // Queued messages live in the strip above the input, the same
            // place the TUI draws them. Writing them into the transcript put
            // the queue in the middle of the conversation.
            self.queued
                .push(agent_bridge::PendingMessage::plain(text.clone()));
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
                        .tools
                        .push(transcript::ToolCall::running(tool_name, description));
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
                        None => self.live.tools.push(transcript::ToolCall {
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
                            .tools
                            .push(transcript::ToolCall::detached(task_id, label));
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
    fn drain_pending(&mut self, cx: &mut Context<Self>) {
        if self.queued.is_empty() || self.busy {
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
        let msgs = std::mem::take(&mut self.queued);
        if let Some(joined) = agent_bridge::PendingMessage::join(&msgs) {
            self.live.begin(joined.display_text.clone());
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

    /// Open the rename prompt for the open topic, pre-filled with its title
    /// (#2304). Enter saves the new name; Esc leaves the title as it was.
    fn start_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.transcript.session else {
            return;
        };
        let current = self
            .sessions
            .items
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_default();
        self.topic_menu = false;
        self.topic_rename = Some(id);
        self.topic_draft = Some(current);
        cx.notify();
    }

    /// Load the info panel for the open session. The panel is read-only: it
    /// shows the id, the parent, the created time, and the message count.
    fn open_topic_info(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.transcript.session else {
            return;
        };
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

    /// Close the open topic, or reopen it if it is closed (#2305). The state
    /// is re-read from the database, so the dimming and the send guard follow
    /// what was actually written.
    fn toggle_closed_topic(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.transcript.session else {
            return;
        };
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

    /// The tab strip for the open chat's topic family (#2400): the parent,
    /// then each of its topics in list order. The open one is highlighted, and
    /// a click opens that session.
    fn topic_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = self.transcript.session else {
            return Vec::new();
        };
        let root = self.sessions.parents.get(&open).copied().unwrap_or(open);
        if !self.sessions.is_topic_enabled(root) {
            return Vec::new();
        }
        let title = |id: Uuid| {
            self.sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| "Untitled".to_string())
        };
        let mut ids = vec![root];
        ids.extend(
            self.sessions
                .items
                .iter()
                .filter(|s| self.sessions.parents.get(&s.id) == Some(&root))
                .map(|s| s.id),
        );
        ids.into_iter()
            .map(|id| {
                let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.open_session(id, cx);
                });
                // A topic with its own icon shows it before the name (#2414).
                let label = match self.sessions.topic_icons.get(&id) {
                    Some(icon) => format!("{icon} {}", title(id)),
                    None => title(id),
                };
                topic_tab(id, label, id == open, move |ev, window, app| {
                    click(ev, window, app)
                })
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
        self.topic_menu = false;
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

    /// Fold or unfold a session's topic tree. The change shows at once and is
    /// saved to the ledger, so the TUI shows the same tree (#2427).
    fn toggle_fold(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let collapsed = !self.sessions.collapsed.contains(&id);
        if collapsed {
            self.sessions.collapsed.insert(id);
        } else {
            self.sessions.collapsed.remove(&id);
        }
        cx.notify();
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = sessions::set_collapsed(db_path, id, collapsed).await {
                logging::error(&format!("fold topics {id}: {e}"));
                this.update(cx, |this, cx| {
                    this.live.notices.push(format!("topic tree: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Re-read which sessions are topics and what each topic's parent is.
    fn refresh_topic_state(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let parents = sessions::load_topic_parents(db_path.clone()).await;
            let closed = sessions::load_closed(db_path.clone()).await;
            let collapsed = sessions::load_collapsed(db_path.clone()).await;
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
                match collapsed {
                    Ok(ids) => this.sessions.collapsed = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("collapsed topics refresh: {e}")),
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
            let result = sessions::load_sessions(db_path).await;
            this.update(cx, |this, cx| {
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
        // Split panes (#2438): the chord opens a split, and while one waits for
        // its session, N makes a new one and Esc drops the pick.
        if self.screen == Screen::Chat {
            let ks = &ev.keystroke;
            if let Some(layout) = split::chord_layout(
                &ks.key,
                ks.key_char.as_deref(),
                ks.modifiers.control,
                ks.modifiers.shift,
            ) {
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
        }
        // Modifiers go in the line too: a bare key name cannot tell ctrl+h
        // from a typed h, which is exactly what a help-door report needs.
        let m = &ev.keystroke.modifiers;
        let mods = ChordMods {
            control: m.control,
            shift: m.shift,
            platform: m.platform,
        };
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
        // ctrl+shift+f pins the row the keyboard is on, or the open
        // conversation when the rail has not armed one. It comes before the
        // plain find key because both carry `f`; shift decides which one it
        // was.
        if ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == "f"
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
                "escape" | "enter" => self.close_rail_filter(cx),
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
            // The confirm dialog sits above everything, so Esc answers it
            // first: cancelling leaves the conversation untouched.
            if self.confirm_delete.take().is_some() {
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
            if self.screen == Screen::Models {
                self.screen = Screen::Chat;
                self.refocus_screen();
                cx.notify();
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
                // Idle Esc hands the keyboard between the composer and the
                // rail: the one keyboard bridge that makes arrow navigation
                // reachable without a mouse click first (#2351). Second Esc
                // hands it back.
                if self.editor.read(cx).focus_handle.is_focused(window) {
                    window.blur();
                } else {
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
                let offset = self.transcript_scroll.offset();
                let max = self.transcript_scroll.max_offset().height;
                let dy = if key == "pagedown" { -page } else { page };
                let y = (offset.y + dy).min(px(0.0)).max(-max);
                self.transcript_scroll.set_offset(gpui::point(offset.x, y));
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
        if let Some(next) = zoom_step(&ev.keystroke.key, self.rem).filter(|_| chord) {
            self.rem = next;
            window.set_rem_size(px(self.rem));
            logging::debug(&format!("zoom -> {:.2}x", self.rem / BASE_REM));
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
                // Tab moves the keyboard between the composer and the sidebar.
                // While an emoji or command list is open, Tab accepts there (#2417).
                "tab" if self.rail_focused || !self.editor.read(cx).list_open() => {
                    self.rail_focused = !self.rail_focused;
                    if self.rail_focused {
                        self.sessions.cursor = self.sessions.cursor.or(self.sessions.selected);
                    } else {
                        self.refocus_screen();
                    }
                    cx.notify();
                    return;
                }
                "escape" if self.rail_focused => {
                    self.rail_focused = false;
                    self.refocus_screen();
                    cx.notify();
                    return;
                }
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
        // The model picker is a list: arrows walk it and Enter switches to the
        // row under the highlight. Esc leaves, handled above with the screens.
        if self.screen == Screen::Models {
            let last = self.models_rows.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                "up" => self.models_index = self.models_index.saturating_sub(1),
                "down" => self.models_index = (self.models_index + 1).min(last),
                "enter" => {
                    if let Some(row) = self.models_rows.get(self.models_index.min(last)) {
                        let pair = row.pair();
                        self.switch_model(pair, cx);
                    }
                }
                _ => {}
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
        match input::decide(&mut self.dash, translate_key(ev)) {
            UsageKeyOutcome::Consumed => cx.notify(),
            UsageKeyOutcome::Period(period) => {
                logging::info(&format!("period -> {period:?}: fetching"));
                self.dash.set_period(period);
                self.pending += 1;
                let pending = self.pending;
                let db_path = self.db_path.clone();
                cx.spawn(async move |this, cx| {
                    // The fetch runs on its own big-stack thread (see
                    // `load_view`): gpui's background workers are
                    // dispatch-queue threads with tiny stacks. The previous
                    // view stays on screen until the result lands.
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
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The composer leaves the arrows to a list that is on screen (#2404).
        let window_owns_arrows =
            (self.screen == Screen::Chat && self.rail_open && self.rail_focused)
                || self.screen == Screen::Themes;
        cx.set_global(editor::ArrowsToWindow(window_owns_arrows));
        // The pending focus from the last transition is applied here because
        // this is the only place holding a window to focus through. Applying
        // it before the tree is built means the frame that shows the new
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
            // A session with topics gets the chevron that folds its tree (#2427).
            let fold = self.sessions.topics.contains(&id).then(|| {
                let collapsed = self.sessions.collapsed.contains(&id);
                let toggle = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.toggle_fold(id, cx);
                });
                sessions::fold_button(id, collapsed).on_click(toggle)
            });
            let r = sessions::row(s, flags, action, menu, fold);
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
        let rail = sessions::rail(
            &self.sessions,
            rail_rows,
            self.rail_width,
            &self.rail_scroll,
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
            Screen::Onboard => "OpenCrabs \u{b7} onboarding".to_string(),
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
        // One click listener per attachment tile, minted here for the same
        // reason as the folds: only this `cx` can build one, and each tile has
        // to open its own file (#2395).
        let mut attachment_handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        for key in self.transcript.attachment_paths() {
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

        let (middle, footer): (AnyElement, AnyElement) = match self.screen {
            Screen::Chat => {
                (
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        .child(transcript::view(
                            &self.transcript,
                            &self.transcript_scroll,
                            &self.live,
                            self.busy,
                            pane_width,
                            fold_handlers,
                            attachment_handlers,
                        ))
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
            Screen::Usage => {
                let body: Div = if self.dash.cache_detail_open {
                    let cache = match &self.state {
                        LoadState::Loaded(vm) => vm.cache.as_ref(),
                        LoadState::Failed(_) => None,
                    };
                    breakdown::panel(cache)
                } else {
                    match &self.state {
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
                    }
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
                            .child("t · today · w · weekly · m · monthly · a · all time"),
                    )
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
                    let (number, total) = ob.progress();
                    // The rail is the flow the current mode walks, with the
                    // step the user stands on marked. Channel sub-steps are
                    // reached from Channels and are not stops.
                    let mut rail = div().flex().flex_col().gap_1().w(px(190.0)).pr_5();
                    for (n, title, here) in ob.rail() {
                        rail = rail.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(text(
                                    format!("{n:>2}"),
                                    Size::Xs,
                                    if here { shown.amber } else { shown.text_faint },
                                ))
                                .child(text(
                                    title,
                                    Size::Sm,
                                    if here {
                                        shown.text_strong
                                    } else {
                                        shown.text_muted
                                    },
                                )),
                        );
                    }
                    let mut column = div().flex().flex_col().gap_3().flex_1().min_h_0().w_full();
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
                        for (index, info) in provider_rows {
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
                                    info.name,
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

                        // The model list: the provider's own catalogue, or the
                        // config's fallback, filtered by whatever is typed in
                        // the search row above it.
                        let models = ob.models();
                        let chosen = ob.model();
                        let search_focused = ob.field == ob.search_field();
                        // Same window as the provider list: ten models painted
                        // whole is fine, a hundred is not, and the selection
                        // has to stay in view when the arrows move.
                        let (visible_models, hidden_models_above, hidden_models_below) =
                            ob.model_rows(14);
                        let mut model_rows = div().flex().flex_col().gap_1().w_full();
                        if hidden_models_above > 0 {
                            model_rows = model_rows.child(text(
                                format!("\u{2191} {hidden_models_above} more"),
                                Size::Xs,
                                shown.text_faint,
                            ));
                        }
                        for (index, name) in visible_models.iter() {
                            // Copied out of the borrow: the click closure has to
                            // own the index, or it pins the whole row list.
                            let index = *index;
                            let here = *name == chosen;
                            let mut row = div()
                                .id(SharedString::from(format!("onboard-model-{index}")))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .rounded(px(6.0))
                                .child(text(
                                    if here { "\u{25cf}" } else { " " },
                                    Size::Sm,
                                    if here { shown.amber } else { shown.text_muted },
                                ))
                                .child(text(
                                    name.clone(),
                                    Size::Sm,
                                    if here { shown.text_strong } else { shown.text },
                                ));
                            if here {
                                row = row.bg(rgb(shown.panel_hi));
                            }
                            model_rows = model_rows.child(row.on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    if let Some(ob) = this.onboard.as_mut() {
                                        ob.wizard.ps.selected_model = index;
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
                                .child(text(
                                    if ob.is_fetching() {
                                        "model \u{b7} fetching the provider catalogue".to_string()
                                    } else {
                                        format!("model \u{b7} {} available", models.len())
                                    },
                                    Size::Xs,
                                    shown.text_faint,
                                ))
                                .child(
                                    div()
                                        .id("onboard-search")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(6.0))
                                        .bg(rgb(if search_focused {
                                            shown.panel_hi
                                        } else {
                                            shown.panel
                                        }))
                                        .border_1()
                                        .border_color(rgb(if search_focused {
                                            shown.focus
                                        } else {
                                            shown.track
                                        }))
                                        .child(text(
                                            if ob.wizard.ps.model_filter.is_empty() {
                                                "search the list".to_string()
                                            } else {
                                                ob.wizard.ps.model_filter.clone()
                                            },
                                            Size::Sm,
                                            if ob.wizard.ps.model_filter.is_empty() {
                                                shown.text_faint
                                            } else {
                                                shown.text
                                            },
                                        ))
                                        .child(text(
                                            if search_focused { "\u{258f}" } else { "" },
                                            Size::Sm,
                                            shown.amber,
                                        ))
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if let Some(ob) = this.onboard.as_mut() {
                                                let field = ob.search_field();
                                                ob.field = field;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div().id("onboard-models").w_full().child(model_rows).child(
                                        if hidden_models_below > 0 {
                                            text(
                                                format!(
                                                    "\u{2193} {hidden_models_below} more below"
                                                ),
                                                Size::Xs,
                                                shown.text_faint,
                                            )
                                        } else {
                                            text("", Size::Xs, shown.text_faint)
                                        },
                                    ),
                                ),
                        );
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
                                    .border_color(rgb(if here { shown.focus } else { shown.track }))
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
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
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
                                        .bg(rgb(if here { shown.panel_hi } else { shown.panel }))
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
                                        .bg(rgb(shown.panel))
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
                                    .bg(rgb(shown.panel))
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

                    let footer = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                            if ob.is_channel_step() {
                                // Space is the key that flips a channel here,
                                // so the hint names it instead of "save".
                                "space \u{b7} toggle \u{b7} enter \u{b7} continue \u{b7} esc \u{b7} back"
                            } else if ob.step()
                                == opencrabs::tui::onboarding::OnboardingStep::BrainSetup
                            {
                                // This step writes files before it moves, so
                                // the hint says what Enter starts.
                                "enter \u{b7} write my brain files \u{b7} esc \u{b7} back"
                            } else if ob.wizard.quick_jump {
                                "enter \u{b7} save \u{b7} esc \u{b7} close"
                            } else {
                                "enter \u{b7} continue \u{b7} esc \u{b7} back"
                            },
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_muted))
                                .child("tab \u{b7} next field \u{b7} arrows \u{b7} walk"),
                        );
                    (
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_h_0()
                            .child(rail)
                            .child(
                                div()
                                    .id("onboard-body")
                                    .flex_1()
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
                // Every pair the terminal offers, the live one marked. Enter
                // switches to the row under the highlight and stays, so a user
                // can compare without reopening; Esc goes back.
                let shown = palette::current();
                let last = self.models_rows.len().saturating_sub(1);
                let current = self.models_index.min(last);
                let mut list = div().flex().flex_col().gap_1();
                for (index, row) in self.models_rows.iter().enumerate() {
                    let pair = row.pair();
                    let selected = index == current;
                    let mut entry = div()
                        .id(SharedString::from(format!("model-row-{index}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded(px(6.0))
                        .child(text(
                            if row.current { "\u{25cf}" } else { " " },
                            Size::Sm,
                            if row.current {
                                shown.amber
                            } else {
                                shown.text_muted
                            },
                        ))
                        .child(text(pair, Size::Sm, shown.text))
                        .child(text(row.provider_name.clone(), Size::Xs, shown.text_muted));
                    if selected {
                        entry = entry.bg(rgb(shown.panel_hi));
                    }
                    list =
                        list.child(entry.on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.models_index = index;
                            let pair = this
                                .models_rows
                                .get(index)
                                .map(models::Row::pair)
                                .unwrap_or_default();
                            this.switch_model(pair, cx);
                        })));
                }
                let scroller = div()
                    .id("models-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list);
                let count = self.models_rows.len();
                let footer = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(div().text_xs().text_color(rgb(shown.text_muted)).child(
                        SharedString::from(format!(
                            "{count} model{}",
                            if count == 1 { "" } else { "s" }
                        )),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(shown.text_muted))
                            .child("enter \u{b7} switch \u{b7} esc \u{b7} back"),
                    );
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
                    .child(text("themes", Size::Sm, shown.text))
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
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(shown.panel_hi))
                    .bg(rgb(shown.panel))
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
                        .rounded(px(6.0))
                        .bg(rgb(shown.panel_hi))
                        .border_1()
                        .border_color(rgb(shown.accent))
                        .text_color(rgb(shown.text))
                        .cursor_pointer()
                        .on_click(keep)
                        .child("keep"),
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
                let primary_width = if self.split.is_some() {
                    (pane_width - THEMES_PANEL_WIDTH) / 2.0 - 8.0
                } else {
                    pane_width - THEMES_PANEL_WIDTH
                };
                let primary =
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(transcript::view(
                            &self.transcript,
                            &self.transcript_scroll,
                            &self.live,
                            self.busy,
                            primary_width,
                            fold_handlers,
                            attachment_handlers,
                        ));
                let body = match &self.split {
                    None => primary,
                    Some(pane) => {
                        let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                            this.split = None;
                            cx.notify();
                        });
                        let secondary = div()
                            .id("split-secondary")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(split_header(pane, close))
                            .child(transcript::view(
                                &pane.transcript,
                                &pane.scroll,
                                &LiveTurn::default(),
                                false,
                                primary_width,
                                HashMap::new(),
                                HashMap::new(),
                            ));
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .gap_2()
                            .when(pane.layout.is_stacked(), |d| d.flex_col())
                            .when(!pane.layout.is_stacked(), |d| d.flex_row())
                            .child(primary)
                            .child(secondary)
                    }
                };
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
            let toggle = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_menu = !this.topic_menu;
                cx.notify();
            });
            let mut items: Vec<Stateful<Div>> = Vec::new();
            if self.open_chat_topics_on() {
                let new_topic = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.start_topic_draft(cx);
                });
                items.push(sessions::menu_item("New topic").on_click(new_topic));
            }
            let view = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.topic_menu = false;
                this.open_topic_info(cx);
            });
            items.push(sessions::menu_item("View topic info").on_click(view));
            if is_topic {
                let edit = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.start_rename(cx);
                });
                items.push(sessions::menu_item("Edit topic").on_click(edit));
                let closed_now = self
                    .transcript
                    .session
                    .is_some_and(|id| self.sessions.is_closed(id));
                let close_label = if closed_now {
                    "Reopen topic"
                } else {
                    "Close topic"
                };
                let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.topic_menu = false;
                    this.toggle_closed_topic(cx);
                });
                items.push(sessions::menu_item(close_label).on_click(close));
                let delete = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.topic_menu = false;
                    if let Some(id) = this.transcript.session {
                        this.request_delete(id, cx);
                    }
                });
                items.push(sessions::menu_item("Delete topic").on_click(delete));
            }
            // The tabs (#2400): the family's parent, then its topics. The
            // leading button moves the whole row to another side (#2402).
            let tabs = self.topic_tabs(cx);
            let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.cycle_topic_edge(cx);
            });
            let strip = topic_strip(
                self.topic_edge,
                toggle_edge,
                tabs,
                topic_bar(self.topic_menu, toggle, items),
            );
            topic_row = Some((self.topic_edge, strip.into_any_element()));
        }
        let mut right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_3()
            // Less top and bottom room than the old 24px on every side: the
            // header lines up with the sessions title, and the input is not
            // floating in empty space above the window edge.
            .px_6()
            .pt_3()
            .pb_4();
        // The chat screen has no header: the session name leads the footer
        // instead. The other screens keep their title above the body.
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
                if !self.queued.is_empty() {
                    col = col.child(queue_strip(&self.queued));
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
            Screen::Onboard => right.child(footer),
        };

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
                    let next = from_w + dx / this.rem;
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
            let picks: Vec<Stateful<Div>> = TOPIC_ICONS
                .iter()
                .map(|glyph| {
                    let glyph: &'static str = glyph;
                    let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.topic_draft_icon = Some(glyph.to_string());
                        cx.notify();
                    });
                    div()
                        .id(SharedString::from(format!("topic-icon-{glyph}")))
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(pick)
                        .child(SharedString::from(glyph))
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
                    .rounded(px(10.0))
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
        root
    }
}

/// The drag handle between the rail and the chat pane. One narrow column with
/// the resize cursor; the pointer position during the drag comes from the
/// window, so this element only has to catch the press.
fn splitter(
    grab: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    div()
        .id("rail-splitter")
        .w(px(4.0))
        .h_full()
        .flex_none()
        .cursor_col_resize()
        .bg(rgb(palette::current().panel_hi))
        .hover(|d| d.bg(rgb(palette::current().accent)))
        .on_mouse_down(MouseButton::Left, grab)
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
    let sessions_state = match session_list {
        Ok(items) => sessions::SessionsState {
            items,
            favorites: favorites.clone(),
            topics: topics.clone(),
            parents: parents.clone(),
            closed: closed.clone(),
            topic_icons: HashMap::new(),
            collapsed: HashSet::new(),
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
            topic_icons: HashMap::new(),
            collapsed: HashSet::new(),
            selected: None,
            cursor: None,
            rail_menu: None,
            error: Some(e.into()),
            filter: String::new(),
            filter_active: false,
        },
    };
    // Chat is the default screen: the newest conversation opens on launch.
    // The usage ledger is something the user asks for (ctrl+u). They never
    // share the pane, so a conversation can never render as "usage".
    let initial_session = sessions_state.items.first().map(|s| s.id);
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
                    Dashboard {
                        screen: Screen::Chat,
                        state,
                        dash: DashboardState::default(),
                        focus: focus.clone(),
                        // The chat opens with the composer holding focus, so its
                        // caret shows and typing needs no click first (#2433).
                        refocus: Some(focus_target(Screen::Chat)),
                        pending: 0,
                        files: Vec::new(),
                        files_for: None,
                        files_err: None,
                        rem: BASE_REM,
                        db_path: db_path.clone(),
                        sessions: sessions_state,
                        rail_width: RAIL_DEFAULT_REM,
                        rail_open: true,
                        rail_focused: false,
                        update_available: None,
                        update_prompt: None,
                        help_filter: None,
                        split: None,
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
                        topic_menu: false,
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
                        models_rows: Vec::new(),
                        models_index: 0,
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

/// The TUI's ctx counter, rebuilt for the footer: same numbers, same
/// grammar (`ctx: 12K/200K (6%)`) and the same colour thresholds, so the
/// desktop window cannot drift from the terminal (`tui::render::input`).
///
/// `None` (no answer yet) or an unknown window draws a dash, exactly like
/// the TUI's placeholder, instead of a percentage of nothing.
/// The queued messages, above the input where the user can see what will go
/// out next. Same shape as the TUI's pending list: one line per message, the
/// count in the header, and a word on when they send.
fn queue_strip(queued: &[agent_bridge::PendingMessage]) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_sm()
        .bg(rgb(palette::current().panel))
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().amber))
                .child(SharedString::from(format!(
                    "queued {} · sends when this turn ends",
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
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel));
    for (index, (glyph, code)) in items.iter().enumerate() {
        let on = index == selected;
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded_sm()
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
        .rounded_sm()
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
            .rounded_sm();
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

/// The chat's `...` button, right aligned, with its items below it while the
/// menu is open (#2303, #2304). Only drawn for a topic or a session with topics on.
fn topic_bar(
    open: bool,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    items: Vec<Stateful<Div>>,
) -> impl IntoElement {
    let button = div()
        .id("topic-menu-button")
        .px_2()
        .py_0p5()
        .rounded_sm()
        .cursor_pointer()
        .text_color(rgb(palette::current().text_muted))
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(toggle)
        .child("...");
    let mut bar = div().flex().flex_col().items_end().w_full().child(button);
    if open {
        for item in items {
            bar = bar.child(item);
        }
    }
    bar
}

/// The name prompt above the input, while a topic is being named (#2303).
/// The topic row (#2400, #2402): the leading button that moves it, the tabs,
/// and the `...` menu. Across the top or bottom it is one row; down a side it
/// is one column.
fn topic_strip(
    edge: topic_tabs::Edge,
    toggle_edge: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    tabs: Vec<AnyElement>,
    bar: impl IntoElement,
) -> Div {
    // A square target the size of the rail's action icons, so it is easy to
    // hit and reads as a control rather than a stray glyph (#2414).
    let leading = div()
        .id("topic-edge-button")
        .flex()
        .items_center()
        .justify_center()
        .size(rems(1.9))
        .rounded_sm()
        .flex_none()
        .cursor_pointer()
        .text_color(rgb(palette::current().text_muted))
        .text_size(rems(1.25))
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(toggle_edge)
        .child(SharedString::from("\u{25EB}"));
    if edge.is_vertical() {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(12.0))
            .gap_2()
            .p_2()
            .child(leading)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_hidden()
                    .children(tabs),
            )
            .child(bar)
    } else {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .min_w_0()
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
            .child(bar)
    }
}

/// One tab of the topic strip (#2400). The open session is marked by an
/// accent label and a raised background, the same as the rail's selection.
fn topic_tab(
    id: Uuid,
    label: String,
    current: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let colour = if current {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    div()
        .id(SharedString::from(format!("topic-tab-{id}")))
        .px_2()
        .py_0p5()
        .rounded_sm()
        .flex_none()
        .text_sm()
        .text_color(rgb(colour))
        .cursor_pointer()
        .when(current, |d| d.bg(rgb(palette::current().panel_hi)))
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(on_click)
        .child(SharedString::from(label))
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
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel))
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
                        .rounded_sm()
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
                        .rounded_sm()
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

/// The header of the second pane: its title and a close button (#2438).
fn split_header(
    pane: &SplitPane,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let title = pane.title.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(rgb(palette::current().panel_hi))
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .id("split-close")
                .px_2()
                .cursor_pointer()
                .text_color(rgb(palette::current().text_muted))
                .hover(|d| d.text_color(rgb(palette::current().accent)))
                .on_click(close)
                .child(SharedString::from("×")),
        )
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
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel))
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

/// The icons a new topic can carry, as the picker offers them (#2414).
const TOPIC_ICONS: [&str; 12] = [
    "💬", "📰", "💡", "⚡", "🎙", "🔥", "🦀", "🧪", "📈", "🛠", "📚", "🎯",
];

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
        .rounded_md()
        .border_1()
        .border_color(rgb(palette::current().accent))
        .bg(rgb(palette::current().bg))
        .text_color(rgb(palette::current().text))
        .child(SharedString::from(format!("{draft}|")));
    let icon_badge = div()
        .flex_none()
        .size(rems(2.4))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(palette::current().bg))
        .text_size(rems(1.4))
        .child(SharedString::from(icon.to_string()));
    let picker = div().flex().flex_row().flex_wrap().gap_1().children(picks);
    let panel = div()
        .id("topic-prompt-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_3()
        .w(rems(28.0))
        .p_4()
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel))
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
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel))
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
                .rounded(px(10.0))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(rgb(palette::current().panel))
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
                                .rounded_sm()
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
                                .rounded_sm()
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

#[cfg(test)]
mod tests {
    use super::{THEMES_HISTORY_LEN, remember_preview};

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
            ("ctrl+shift+f", "Pin the selected session"),
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
                    "ctrl+shift+f" => "ctrl-shift-f",
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
