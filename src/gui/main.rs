//! OpenCrabs desktop client. Phase 1: the TUI mirrored into GPUI.
//!
//! Built only with `--features gui` (`required-features` on the `[[bin]]`), so
//! the TUI build and the CI gate never link gpui.
//!
//! The window has two panes: the session rail (every real session, most
//! recent first) and the right pane, which is the chat by default, with the
//! newest conversation already open. Cmd+U swaps the right pane to the real
//! usage ledger and Esc (or Cmd+U again) returns to the conversation; the two
//! screens are never mixed. There is no sample data anywhere: when a read
//! cannot happen, the pane says why.
//!
//! Keys go through the neutral layer where a TUI analogue exists: a gpui
//! `KeyDownEvent` is translated into a `UiKey` at this edge and the usage
//! decision is `ui_core::usage::input::decide`, the same function the TUI
//! runs. The footer's hint labels come from the same `advertises` rule set.
//! GUI-only concerns live outside that vocabulary: the zoom keys, the scroll
//! containers, and rail clicks.

mod agent_bridge;
mod breakdown;
mod editor;
mod icon;
mod logging;
mod markdown;
mod plan_strip;
mod sessions;
mod theme;
mod transcript;
mod usage_view;

use crate::theme::{
    AMBER, BASE_REM, BG, RED, TEAL, TEXT, TEXT_FAINT, TEXT_MUTED, TEXT_NORMAL, ZOOM_MAX, ZOOM_MIN,
    ZOOM_STEP,
};

use futures::channel::oneshot;

use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Div, DragMoveEvent, Entity,
    ExternalPaths, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ScrollHandle, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rems, rgb, rgba, size,
};
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::utils::drop_landing::human_size;
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
/// ledger is opened with Cmd+U, and the conversation's tracked files with
/// Cmd+F, because the owner asked for exactly that split: an open
/// conversation must never be labelled or treated as usage.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Chat,
    Usage,
    Files,
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

/// The phase 1 window. Every string on screen is real data or the real error.
struct Dashboard {
    /// Which pane the right side draws: chat by default, usage on Cmd+U.
    screen: Screen,
    state: LoadState,
    /// Which card is focused, whether the breakdown is open: the shared
    /// state the TUI's own handler mutates, driven by the same `decide`.
    dash: DashboardState,
    focus: FocusHandle,
    /// Monotonic tag on the latest period fetch; only its result is applied,
    /// so a slow fetch landing after a newer one cannot paint stale data.
    pending: u64,
    /// The session-files browser (Cmd+F): rows for the open conversation and
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
    /// `RAIL_MIN_REM`: dragging past it closes the rail instead, and Cmd+B
    /// brings it back at the last width.
    rail_width: f32,
    /// Whether the rail is drawn at all.
    rail_open: bool,
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
    /// The plan checklist strip above the editor, re-read when its file moves.
    plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per selection rather than once per frame.
    plan_for: Option<Uuid>,
    /// The selected session's transcript and its scroll handle, so a fresh
    /// load can open at the bottom where the conversation ends.
    transcript: transcript::TranscriptState,
    transcript_scroll: ScrollHandle,
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
    /// The checkout's branch, read once at startup: the footer's own field in
    /// the TUI. `None` outside a git tree, which the footer shows as a dash
    /// rather than a made-up name.
    branch: Option<String>,
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
    /// click, launch, Cmd+U return) goes through here, so the screen and the
    /// session can never disagree.
    fn open_session(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.sessions.selected = Some(id);
        if self.transcript.session != Some(id) {
            // A different conversation has its own budget and its own last
            // prompt size; carrying the previous one over would show a
            // counter for a session the user is not reading.
            self.ctx_tokens = None;
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
                            let (hidden, turns) = transcript::turns(&rows, total);
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
        cx.notify();
    }

    /// Fold or unfold one turn. Folding is the default, so the set holds the
    /// anchors the user OPENED, keyed exactly as the TUI keys its override
    /// map (#2155), which is what lets an opened turn survive a reload.
    fn toggle_turn(&mut self, anchor: &str, cx: &mut Context<Self>) {
        if !self.transcript.expanded.remove(anchor) {
            self.transcript.expanded.insert(anchor.to_string());
        }
        cx.notify();
    }

    /// Cmd+O: toggle the newest turn, the way ctrl+o does in the TUI.
    fn toggle_newest_turn(&mut self, cx: &mut Context<Self>) {
        let Some(anchor) = self.transcript.newest_anchor().map(str::to_string) else {
            return;
        };
        self.toggle_turn(&anchor, cx);
    }

    /// Cmd+U: open the usage ledger, or return to the conversation. The panes
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
        }
        cx.notify();
    }

    /// Cmd+G: the open conversation's tracked files, or back to the chat. A
    /// no-op without an open conversation, because a files browser for no
    /// session is a lie. The TUI reaches the same list with `f` on the
    /// sessions browser (`AppMode::SessionFiles`).
    fn toggle_files(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Chat {
            self.screen = Screen::Chat;
            cx.notify();
            return;
        }
        let Some(sid) = self.transcript.session else {
            logging::info("cmd-g ignored: no open conversation");
            return;
        };
        self.screen = Screen::Files;
        self.files.clear();
        self.files_err = None;
        self.files_for = Some(sid);
        self.fetch_files(sid, cx);
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
        cx.notify();
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
                    .text_color(rgb(TEXT_MUTED))
                    .child(err.clone()),
            );
        }
        if self.files.is_empty() {
            return body.child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT_FAINT))
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
                            .text_color(rgb(TEXT_NORMAL))
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
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
                    self.open_session(id, cx);
                }
                agent_bridge::TurnEvent::SessionCreateFailed(e) => {
                    self.live.notices.push(format!("new session: {e}"));
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
                    let open = self
                        .live
                        .tools
                        .iter_mut()
                        .rev()
                        .find(|c| !c.done && c.name == tool_name);
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
                        }),
                    }
                }
                // Every API response and tool execution reports the context
                // size it just ran with, so the footer tracks the turn while
                // it is still in flight.
                agent_bridge::TurnEvent::ProgressFor(_, ProgressEvent::TokenCount(count)) => {
                    self.ctx_tokens = Some(count.min(u32::MAX as usize) as u32);
                }
                // Detached-task rows, compaction notices, queue echoes: the
                // phase-2 transcript mirror. They persist to the DB and show
                // on the reload; nothing to draw live yet.
                agent_bridge::TurnEvent::ProgressFor(_, _) => {}
                agent_bridge::TurnEvent::Done {
                    content,
                    context_tokens,
                } => {
                    // The provider's own prompt size for the call that just
                    // answered: the honest number for the ctx footer, not an
                    // estimate of what we think we sent.
                    self.ctx_tokens = Some(context_tokens);
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
                    self.busy = false;
                    self.live.thinking = false;
                    reload = self.transcript.session;
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

    /// Arm the delete dialog. Every delete path (Cmd+Backspace, a row's
    /// strip) lands here instead of erasing, so the erase is always the
    /// second, deliberate click.
    fn request_delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = Some(id);
        cx.notify();
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
                        let (hidden, turns) = transcript::turns(&rows, total);
                        let landed = expect.is_none_or(|want| turns.len() >= want);
                        this.transcript.hidden = hidden;
                        this.transcript.turns = turns;
                        if landed {
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

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        logging::debug(&format!("key {:?}", ev.keystroke.key));
        // Cmd+U is the screen switch and it must work while typing, so it
        // sits before the editor focus gate: the editor only claims IME
        // escapes, and this key belongs to the window.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "u" {
            self.toggle_usage(cx);
            return;
        }
        // Cmd+O folds or unfolds the newest turn, the desktop half of the
        // TUI's ctrl+o. It sits with the screen keys so it works while typing.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "o" && self.screen == Screen::Chat
        {
            self.toggle_newest_turn(cx);
            return;
        }
        // Cmd+N opens a new conversation, the desktop half of the TUI's
        // ctrl+n. The row is created in the ledger first, so the rail and the
        // transcript agree on which session the next turn is written to.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "n" {
            self.new_session(cx);
            return;
        }
        // Cmd+B shows or hides the rail, the desktop half of a TUI pane
        // toggle. Like the screen keys it sits before the editor gate. A
        // hidden rail has no field to keep listening, so the filter exits.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "b" {
            if self.rail_open {
                self.close_rail_filter(cx);
            }
            self.rail_open = !self.rail_open;
            cx.notify();
            return;
        }
        // Cmd+F is the rail's find field: it narrows the session list to
        // chats whose title (or id prefix) matches, live, with the match
        // count on the bar. Any exit clears it, so the rail never hides a
        // filter behind a closed field.
        // Cmd+Shift+F pins the open conversation. It comes before the plain
        // find key because both carry `f`; shift decides which one it was.
        if ev.keystroke.modifiers.platform
            && ev.keystroke.modifiers.shift
            && ev.keystroke.key == "f"
        {
            if let Some(id) = self.transcript.session {
                self.toggle_favorite(id, cx);
            }
            return;
        }
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "f" {
            if self.sessions.filter_active {
                self.close_rail_filter(cx);
            } else {
                self.sessions.filter_active = true;
                cx.notify();
            }
            return;
        }
        // Cmd+G lists the open conversation's tracked files, the desktop half
        // of the TUI sessions-browser `f` (`AppMode::SessionFiles`). A screen
        // key, so it works while typing like the rest of these.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "g" {
            self.toggle_files(cx);
            return;
        }
        // While the rail filter is open it owns the keyboard, editor focus
        // included: printable characters extend the query, backspace (plain
        // or with the command key) shortens it, Enter and Esc clear it and
        // close. Anything else is swallowed so typing can never leak into
        // the chat editor while the field says it is listening.
        if self.sessions.filter_active {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "escape" | "enter" => self.close_rail_filter(cx),
                "backspace" => {
                    self.sessions.filter.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.sessions.filter.push(ch);
                        cx.notify();
                    }
                }
            }
            return;
        }
        // Cmd+Backspace deletes the open conversation, the desktop half of
        // the TUI's session delete. The row is soft-deleted in the ledger and
        // the rail re-fetches, so the row leaves because it is gone.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "backspace" {
            if let Some(id) = self.transcript.session {
                self.request_delete(id, cx);
            }
            return;
        }
        // Esc twice stops the running turn, the TUI's own gesture. It sits
        // before the editor gate because the editor owns the keyboard while
        // typing and would swallow it.
        if ev.keystroke.key == "escape" {
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
            if !self.busy {
                self.esc_at = None;
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
        // While the editor holds focus it owns the keyboard: its actions
        // (send, newline, movement) run through the keymap, and raw keys
        // reaching this handler would double-handle Enter and friends.
        if self.editor.read(cx).focus_handle.is_focused(window) {
            return;
        }
        // Zoom before everything: it is a GUI-only concern with no UiKey, and
        // it must work in every mode, breakdown open or not.
        if let Some(next) = zoom_step(&ev.keystroke.key, self.rem) {
            self.rem = next;
            window.set_rem_size(px(self.rem));
            logging::debug(&format!("zoom -> {:.2}x", self.rem / BASE_REM));
            cx.notify();
            return;
        }
        // Chat owns no usage keys. Esc included: backing out of the
        // conversation into the ledger was the mixing the owner called out,
        // and Esc must never close the window (phase-0 lesson).
        if self.screen == Screen::Chat {
            return;
        }
        // The files browser is a list, not the ledger's grid: Esc (or the
        // Cmd+G toggle again) is the only key it owns, and it goes back to
        // the conversation.
        if self.screen == Screen::Files {
            if translate_key(ev) == UiKey::Esc {
                self.screen = Screen::Chat;
                cx.notify();
            }
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
            let favorite = self.sessions.is_favorite(id);
            // A row's actions stay out of sight until asked for. There is no
            // always-visible delete: an ✕ on every row reads as "close" and
            // costs conversations. Slide the row left, or press the pin key,
            // and the strip appears on that row alone.
            let action = if self.rail_actions == Some(id) {
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
                        .child(sessions::delete_button().on_click(del)),
                )
            } else {
                None
            };
            let r = sessions::row(s, selected, favorite, action);
            // The press starts the slide and closes any other row's strip.
            let press = cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                this.rail_slide_opened = None;
                this.rail_swipe = Some((id, f32::from(window.mouse_position().x)));
                if this.rail_actions.is_some_and(|open| open != id) {
                    this.rail_actions = None;
                }
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
                this.close_rail_filter(cx);
                this.rail_actions = None;
                this.open_session(id, cx);
            });
            let r = r.on_mouse_down(MouseButton::Left, press);
            rail_rows.push(sessions::on_click(r, handler));
        }
        let rail = sessions::rail(&self.sessions, rail_rows, self.rail_width);

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
            Screen::Chat => format!(
                "OpenCrabs \u{b7} {}",
                self.session_title().unwrap_or("chat")
            ),
        };
        window.set_window_title(&label);
        let chrome = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
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
            fold_handlers.insert(anchor, Box::new(handler));
        }

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
                            fold_handlers,
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
                        let identity = status_identity(
                            &provider,
                            &model,
                            &tilde_path(&opencrabs::utils::cwd::launch_cwd()),
                            self.branch.as_deref().unwrap_or("-"),
                            self.live.tokens_per_sec(),
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
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("cmd-u · usage")
                                    .child("cmd-f · find chat")
                                    .child("cmd-g · files")
                                    .child("cmd-n · new chat")
                                    .child("cmd-o · fold turn")
                                    .child("cmd-b · sidebar")
                                    .child("cmd-shift-f · pin")
                                    .child("cmd-⌫ · delete chat"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
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
                                    .text_color(rgb(RED))
                                    .child("Usage data failed to load"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(TEXT_MUTED))
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
                            .text_color(rgb(TEXT_MUTED))
                            .child("t · today · w · weekly · m · monthly · a · all time"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child("cmd-u · back to chat"),
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
                let footer =
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(
                            SharedString::from(format!(
                                "{count} file{} tracked",
                                if count == 1 { "" } else { "s" }
                            )),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(TEXT_MUTED))
                                .child("cmd-g · back to chat"),
                        );
                (scroller.into_any_element(), footer.into_any_element())
            }
        };

        // The input box and the live turn panel belong to the chat screen.
        // An editor parked under the usage ledger is the mixing the owner
        // called out, so usage draws the footer hint and nothing else.
        let mut right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .p_6()
            .child(chrome)
            .child(middle);
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
                col = col.child(self.editor.clone());
                col.child(footer)
            }
            Screen::Usage => right.child(footer),
            Screen::Files => right.child(footer),
        };

        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
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
                this.delete_session(id, cx);
            });
            let cancel = cx.listener(|this, _ev, _window, cx| {
                this.confirm_delete = None;
                cx.notify();
            });
            root = root.child(confirm_delete_overlay(title, confirm, cancel));
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
                    .border_color(rgb(theme::ACCENT))
                    .bg(rgba(0x101010dd))
                    .child(div().text_xl().child("📎"))
                    .child(div().text_lg().child("Drop to attach"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child("files and images ride with your next message"),
                    ),
            );
        }
        root.child(right)
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
        .bg(rgb(theme::PANEL_HI))
        .hover(|d| d.bg(rgb(theme::ACCENT)))
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
    match Config::load() {
        Ok(config) => agent_bridge::spawn_bootstrap(config, agent_sink.clone()),
        Err(e) => {
            let _ = agent_sink.send(agent_bridge::TurnEvent::BootstrapFailed(format!(
                "config load: {e}"
            )));
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
    let sessions_state = match session_list {
        Ok(items) => sessions::SessionsState {
            items,
            favorites: favorites.clone(),
            selected: None,
            error: None,
            filter: String::new(),
            filter_active: false,
        },
        Err(e) => sessions::SessionsState {
            items: Vec::new(),
            favorites,
            selected: None,
            error: Some(e.into()),
            filter: String::new(),
            filter_active: false,
        },
    };
    // Chat is the default screen: the newest conversation opens on launch.
    // The usage ledger is something the user asks for (Cmd+U). They never
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
                        pending: 0,
                        files: Vec::new(),
                        files_for: None,
                        files_err: None,
                        rem: BASE_REM,
                        db_path: db_path.clone(),
                        sessions: sessions_state,
                        rail_width: RAIL_DEFAULT_REM,
                        rail_open: true,
                        dragging_rail: false,
                        drag_from: None,
                        drag_seen: None,
                        rail_actions: None,
                        rail_swipe: None,
                        rail_slide_opened: None,
                        confirm_delete: None,
                        plan: plan_strip::PlanStripState::default(),
                        plan_for: None,
                        transcript: transcript::TranscriptState {
                            session: initial_session,
                            ..Default::default()
                        },
                        transcript_scroll: ScrollHandle::new(),
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
                        branch: git_branch(&opencrabs::utils::cwd::launch_cwd()),
                    }
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
        .bg(rgb(theme::PANEL))
        .child(
            div()
                .text_xs()
                .text_color(rgb(AMBER))
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
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(format!("{}. {short}{suffix}", i + 1))),
        );
    }
    col
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
        return ("ctx: -".to_string(), TEXT_FAINT);
    };
    let pct = (used as f64 / max as f64) * 100.0;
    let color = if pct > 80.0 {
        RED
    } else if pct > 60.0 {
        AMBER
    } else {
        TEAL
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
                .border_color(rgb(theme::PANEL_HI))
                .bg(rgb(theme::PANEL))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from("Delete this conversation?")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
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
                                .text_color(rgb(TEXT_NORMAL))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(theme::PANEL_HI)))
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
                                .text_color(rgb(RED))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(RED))
                                .hover(|d| d.bg(rgb(theme::SELECTION)))
                                .child(SharedString::from("delete"))
                                .on_click(confirm),
                        ),
                ),
        )
}

#[cfg(test)]
mod tests {
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

        // Both arming paths exist and go through the dialog: Cmd+Backspace
        // for the open conversation, and the row's slide-out strip.
        assert!(
            src.contains("self.request_delete(id, cx);"),
            "Cmd+Backspace must arm the dialog"
        );
        assert!(
            src.contains("this.request_delete(id, cx);"),
            "the row strip must arm the dialog"
        );
        // Esc is the dialog's cancel, and it must answer the dialog before
        // the row strip or the turn it would otherwise abort.
        assert!(
            src.contains("if self.confirm_delete.take().is_some()"),
            "Esc must cancel the dialog first"
        );
    }

    use super::*;

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
        assert_eq!(ctx_footer(Some(10_000), 200_000).1, TEAL);
        assert_eq!(ctx_footer(Some(130_000), 200_000).1, AMBER);
        assert_eq!(ctx_footer(Some(170_000), 200_000).1, RED);
    }

    #[test]
    fn a_budget_with_no_sample_stays_a_dash() {
        // Nothing measured, or no known window: never "0%" and never a
        // counter for a conversation that has not run a turn yet.
        assert_eq!(
            ctx_footer(None, 200_000),
            ("ctx: -".to_string(), TEXT_FAINT)
        );
        assert_eq!(ctx_footer(Some(900), 0), ("ctx: -".to_string(), TEXT_FAINT));
    }
}
