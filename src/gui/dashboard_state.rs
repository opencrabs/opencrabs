//! The window's own state: which screen draws, the view it was given, and the
//! drags and menus in flight. The drawing lives in `dashboard_render.rs` and the
//! behaviour in the `dashboard_*` modules beside it.
//!
//! `main.rs` declares the modules and starts the window; this is what the frame
//! reads and what every transition writes.

use super::*;

/// Which pane the right side draws. Chat is the default screen, the usage
/// ledger is opened with ctrl+u, and the conversation's tracked files with
/// ctrl+g, because the owner asked for exactly that split: an open
/// conversation must never be labelled or treated as usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Screen {
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

/// The place the footer names: the open conversation's working directory and
/// the branch checked out in it (#3036).
///
/// The window used to print the directory the process was launched from, which
/// never moves, so a `/cd` changed where the agent works and left the footer
/// naming the old project. The branch was read once at startup for the same
/// reason. Both are resolved from the session now, and the branch is kept here
/// because reading it runs `git`: it is re-read only when the directory
/// changes, never on every frame.
pub(crate) struct FooterPlace {
    /// The conversation it was read for. `None` means no conversation is open,
    /// so the row falls back to the launch directory.
    pub(crate) session: Option<Uuid>,
    pub(crate) dir: std::path::PathBuf,
    pub(crate) branch: Option<String>,
}

/// The window root. Every string on screen is real data or the real error.
pub(crate) struct Dashboard {
    /// Which pane the right side draws: chat by default, usage on ctrl+u.
    pub(crate) screen: Screen,
    /// The screen the previous frame drew (#2698). Coming back to the chat is
    /// a return, not a resume: every panel that closes hands the reader the
    /// newest row instead of the history they left behind.
    pub(crate) last_screen: Screen,
    pub(crate) state: LoadState,
    /// Which card is focused, whether the breakdown is open: the shared
    /// state the TUI's own handler mutates, driven by the same `decide`.
    pub(crate) dash: DashboardState,
    pub(crate) focus: FocusHandle,
    /// Where the next frame should put focus. Every screen transition sets it,
    /// so the caret is never left on an element that stopped drawing and a
    /// screen key always has a named receiver.
    pub(crate) refocus: Option<FocusTarget>,
    /// The owner focus was last set for. A frame where it changed applies the
    /// new target, so an opener that forgets to ask still cannot leave the
    /// composer answering under the field that just took the keyboard.
    pub(crate) last_owner: Option<KeyboardOwner>,
    /// The Seeker card. The query, the ranking and the armed row live in
    /// `ui_core::palette`; this is whether the card is up, and what it was
    /// given to find when it opened (#2458).
    pub(crate) finder: Option<PaletteState>,
    /// Which channels the config says are live, read once when the card opens,
    /// so the hint can tell an unconfigured channel from a working one without
    /// touching the disk on every keystroke (#2462).
    pub(crate) finder_channels: Vec<Channel>,
    /// The channel chip (#2452): `Some(line)` draws a quiet notice at the
    /// bottom of the rail that the configured channels have no live owner,
    /// with a Start daemon action. `None` means either everything is being
    /// served or nothing is configured; the probe refreshes every few
    /// seconds, off the UI thread.
    pub(crate) channel_chip: Option<SharedString>,
    /// The configured channel names, read from the config ONCE when the
    /// window opens, on the UI thread. The 5s probe never calls Config:
    /// on a GPUI background worker the config build overflows the small
    /// stack and killed the window with SIGILL (2026-11-03 crash reports).
    pub(crate) chip_channel_names: Vec<String>,
    /// The Start daemon click is in flight: a second click while launchctl
    /// or systemctl is running would stack two installs.
    pub(crate) daemon_starting: bool,
    /// What the last Start daemon attempt failed with, so the chip can say
    /// why nothing changed after the click.
    pub(crate) daemon_error: Option<SharedString>,
    /// Whether the card's caret is on this half of the blink. The card is a
    /// drawn div, not an input widget, so nothing blinks for it unless a
    /// timer says so (#2491).
    pub(crate) finder_caret: bool,
    /// When the Seeker field last took a key. The caret blinks for
    /// [`CARET_IDLE_AFTER`] after that instant and then holds off, the way
    /// Spotlight drops its caret when typing stops (#2531).
    pub(crate) finder_activity: std::time::Instant,
    /// Latest shaped text and bounds for Seeker mouse hit testing (#2534).
    pub(crate) finder_paint: Entity<command_palette::FinderPaint>,
    /// A left-button drag is extending the field's selection.
    pub(crate) finder_selecting: bool,
    /// The Seeker's own take (#3044). The card records in place: its glass
    /// bubble replaces the card while the device is open, and the words land
    /// in the field where the person was already typing.
    pub(crate) seeker_capture: Option<voice_input::Capture>,
    pub(crate) seeker_transcription: Option<voice_input::Transcription>,
    /// The high-resolution procedural glass drawn while this take is live.
    pub(crate) seeker_orb: Option<seeker_orb::OrbAnimation>,
    /// The live microphone meter that drives the glass motion.
    pub(crate) seeker_voice_levels: Vec<f32>,
    /// The recording was asked to stop and its WAV is being built.
    pub(crate) seeker_voice_finishing: bool,
    /// Whether the words this take produces should be run as a search the
    /// moment they land, or only put in the field for editing (#3044). The
    /// chord sends; clicking the glass hands the words back.
    pub(crate) seeker_voice_send: bool,
    /// Whether the Seeker's voice loop is already ticking. Same one-shot shape
    /// as the composer's: one loop per take, never one per frame.
    pub(crate) seeker_voice_looping: bool,
    /// Why the last take failed, said in the card. The card is what the person
    /// is looking at, so an error drawn behind it would be an error nobody
    /// reads.
    pub(crate) seeker_voice_error: Option<SharedString>,
    /// Monotonic tag on the latest period fetch; only its result is applied,
    /// so a slow fetch landing after a newer one cannot paint stale data.
    pub(crate) pending: u64,
    /// The session-files browser (ctrl+g): rows for the open conversation and
    /// the session they were fetched for, so a slow fetch landing after a
    /// rail click paints nothing.
    pub(crate) files: Vec<agent_bridge::SessionFile>,
    pub(crate) files_for: Option<Uuid>,
    pub(crate) files_err: Option<SharedString>,
    /// Mission Control's snapshot: the four services read once, so a frame
    /// never touches SQLite or the disk (#2426).
    pub(crate) mc: Option<mission_control::Snapshot>,
    /// Why the last read failed, when it did. The screen says so instead of
    /// drawing an empty instance as if nothing were waiting on the user.
    pub(crate) mc_err: Option<SharedString>,
    /// A read is in flight. The screen shows the last snapshot and this flag
    /// rather than clearing to a blank pane.
    pub(crate) mc_loading: bool,
    /// The analytics window the snapshot was read for. It starts at the
    /// terminal's default (a month, `tui::app::mission_control`) so both
    /// clients open on the same numbers.
    pub(crate) mc_window: mission_control::TimeWindow,
    /// Which panel the keyboard is on, which row, and whether the detail is
    /// open. The rules live in `mission_control::Nav` so they are tested
    /// without a window.
    pub(crate) mc_nav: mission_control::Nav,
    /// The log viewer while `L` has it open. It is the terminal's own state
    /// type, so the window reads a log with the same parser and the same
    /// filters (#1528).
    pub(crate) mc_logs: Option<LogViewerState>,
    /// The result of the last apply or reject, said on the screen so the
    /// decision is not silent. Cleared when the screen is reopened.
    pub(crate) mc_notice: Option<mission_control::Notice>,
    /// The window's rem size in px. The zoom keys move it and every
    /// rem-based length in the theme follows.
    pub(crate) rem: f32,
    /// The frame the window is showing (#3082), recorded on every paint because
    /// that is the only place the window is in hand. `poll` writes it once it
    /// stops moving, and `persist_layout` carries it with the pane tree.
    pub(crate) window_frame: Option<layout_store::WindowState>,
    /// The frame `poll` saw on its previous tick. A drag produces a new frame
    /// on every paint, so the write waits for two ticks to agree before it
    /// happens: one write at the end of a drag instead of twenty a second.
    pub(crate) window_frame_seen: Option<layout_store::WindowState>,
    /// The frame already on disk, so a settled window costs no write at all.
    pub(crate) window_frame_saved: Option<layout_store::WindowState>,
    /// Ledger path resolved once at startup, before the window opens.
    /// Period fetches reuse it; they never re-read the config, because
    /// tiny-stack background threads must not run `Config::load`.
    pub(crate) db_path: std::path::PathBuf,
    /// The hint ids the reader has hidden, read once at startup and appended
    /// to on each dismissal (#3121).
    ///
    /// Held here rather than queried per frame: the engine asks on every
    /// repaint which lines may print, and a database read behind a paint is a
    /// stutter the reader would feel as the window thinking.
    pub(crate) hint_dismissed: Vec<String>,
    /// The session rail's live state.
    pub(crate) sessions: sessions::SessionsState,
    /// Which rail rows have work in flight, and which plans wait (#2636).
    /// Memory only: the open turn and each split leaf already keep their own
    /// `busy`, so this holds the one thing nothing else records, a session
    /// streaming with no pane of its own.
    pub(crate) rail: rail_activity::RailActivity,
    /// The id to plan-file index behind each row's badge, with its mtime cache.
    pub(crate) rail_index: rail_activity::PlanIndex,
    /// The plans the owner has already opened, so their rings stop (#2636).
    pub(crate) rail_seen: rail_activity::SeenPlans,
    /// When the rail's live previews were last re-read, so a stream of chunks
    /// costs at most one query a second instead of one per token.
    pub(crate) preview_refreshed_at: Option<std::time::Instant>,
    /// How many preview reads are in flight, and whether another was asked for
    /// while one ran. A busy ledger made a read outlast the one-second tick,
    /// so the next tick stacked a second thread and a second connection on the
    /// same 1.2 GB file (#2817). One read at a time, and the last ask wins.
    pub(crate) previews_in_flight: std::rc::Rc<std::cell::Cell<usize>>,
    pub(crate) previews_dirty: std::rc::Rc<std::cell::Cell<bool>>,
    /// Monotonic generation of the ctrl+f deep search (#2720). Each keystroke
    /// bumps it and spawns a debounced content query that carries the value;
    /// a result only lands when its generation still matches, so a slow query
    /// for "nee" can never paint over the hits for "needle".
    pub(crate) rail_find_gen: u64,
    /// The last ledger signature the rail was refetched from (#2881). A turn
    /// that arrives on a messaging channel produces no window event, so nothing
    /// on the UI side asks the rail to reload and it kept drawing the list it
    /// loaded at startup. The probe compares this pair and pays for a reload
    /// only when the ledger actually moved.
    pub(crate) rail_ledger_sig: Option<(i64, i64)>,
    /// Seeker deep search (#2721): content hits per trimmed query, cached so
    /// re-opening the same query costs no second read. The card's rows are
    /// rebuilt from this map when a debounced result lands.
    pub(crate) seeker_find: HashMap<String, Vec<opencrabs::db::repository::ContentSearchRow>>,
    /// Monotonic generation of the Seeker deep search, the rail's trick again:
    /// a result only rebuilds the card when its generation still matches.
    pub(crate) seeker_find_gen: u64,
    /// The trimmed query the deep search was last scheduled for, so caret
    /// moves and arrow navigation never rebuild the catalog (#2721).
    pub(crate) seeker_scheduled_query: String,
    /// Parents whose topic chips are expanded past the first three, with the
    /// instant each one opened. A chip strip shows three plus a "+N" pill by
    /// default; tapping it reveals the rest, and the collapse pill folds them
    /// back (Telegram topic style).
    ///
    /// The instant is the disclosure's own clock (#3020): an expanded strip is
    /// a picker, not a setting, so it folds itself after a bounded idle period
    /// instead of staying open until the window restarts.
    pub(crate) rail_chips_expanded: std::collections::HashMap<Uuid, std::time::Instant>,
    /// The last voice state the window painted (#3054): active playback and
    /// waveform loads. The poll compares one snapshot, so position changes
    /// and completed background decodes both reach the visible previews.
    pub(crate) voice_paint: voice_play::PaintState,
    /// Rail width in rem, moved by the splitter's drag. Never below
    /// `RAIL_MIN_REM`: dragging past it closes the rail instead, and ctrl+s
    /// brings it back at the last width.
    pub(crate) rail_width: f32,
    /// Whether the rail is drawn at all.
    pub(crate) rail_open: bool,
    /// Whether the sidebar holds the keyboard. Tab moves it; while it does,
    /// the arrows walk its rows, and the composer keeps them otherwise (#2417).
    pub(crate) rail_focused: bool,
    /// Poll ticks since the window opened (#2899). The poll runs at 20 Hz and
    /// most of what it checks only changes what a repaint would draw, so the
    /// answers that cost a file read or a bridge lock are resolved on the
    /// repaint cadence and reused in between.
    pub(crate) poll_tick: u64,
    /// The last answer `rail_animating` gave, reused between the polls that
    /// resolve it.
    pub(crate) rail_anim_live: bool,
    /// When the settled-task pass last ran (#2899). It clones every plan and
    /// resolves their settled ids, so it runs on its own clock rather than
    /// once per frame.
    pub(crate) chime_last: Option<std::time::Instant>,
    /// This frame's status row per leaf (#2899). Building it reads the bridge
    /// for the session's provider pair and its plan progress, and every pane
    /// used to resolve its own twice: once in the stamp the walk builds and
    /// again in its own draw. Resolved once per frame, for every leaf, before
    /// either of them reads it.
    pub(crate) frame_footers: HashMap<Uuid, SharedString>,
    /// The detached-command line per leaf for this frame (#2899). Same reason:
    /// the label takes the background manager's lock and formats a string.
    pub(crate) frame_tasks: HashMap<Uuid, SharedString>,
    /// When `frame_tasks` was last rebuilt. The label ticks in whole seconds,
    /// so a second rebuild inside the same second would take the lock and
    /// allocate the same strings for text that cannot have changed (#2805).
    pub(crate) frame_tasks_at: Option<std::time::Instant>,
    /// A newer release, when the update check found one (#2406).
    pub(crate) update_available: Option<String>,
    /// The update dialog: None when closed, Some(false) asks whether to update,
    /// Some(true) asks again before /evolve runs.
    pub(crate) update_prompt: Option<bool>,
    /// The Help Center filter typed after `/`; None while it is not open (#2407).
    pub(crate) help_filter: Option<String>,
    /// The commands the Help Center prints: slash names, loaded skills and the
    /// user's own entries (#3044). Read once when the reference opens, because
    /// two of the three sources walk the disk; a per-frame read would be a
    /// directory walk and a parse on every repaint.
    pub(crate) help_commands: CommandLists,
    /// Drag-to-copy state shared by the chat's rows (#2383).
    pub(crate) copy_drag: Rc<CopyDrag>,
    /// The window's panes as a tree (#2472). One leaf is the unsplit window,
    /// so the plain chat is the base case rather than a special one, and a
    /// nested split is just another node. `None` until a conversation opens.
    pub(crate) panes: Option<PaneTree>,
    /// The state every pane but the chat keeps, keyed by session (#2472). Each
    /// leaf is its own entity, so it repaints and is read on its own (#2805).
    pub(crate) pane_states: HashMap<Uuid, Entity<PaneView>>,
    /// A pane switched to another conversation, kept whole under the session it
    /// was showing (#2854). A switch used to drop the entity, and with it the
    /// live turn, the tool blocks, the thinking flag and the cancel token, so a
    /// turn that was still running came back as an empty pane. The window has
    /// parked its own turn this way since #2616; this is the same idea for a
    /// leaf. Keyed by the session the view was showing, so switching back
    /// restores exactly what left.
    pub(crate) parked_panes: HashMap<Uuid, Entity<PaneView>>,
    /// The session holding the caret and answering `ctrl+x`; `None` is the
    /// window's own chat (#2474, #2475).
    pub(crate) pane_focus: Option<Uuid>,
    /// Where each split begins and ends along both axes, in px, keyed by the
    /// split's path from the root, written by the body itself every frame
    /// (#2472, #2476). Mapping pointer travel onto a share needs the span it
    /// travels across, and an element is never told its own children's bounds
    /// any other way.
    pub(crate) split_areas: Rc<std::cell::RefCell<HashMap<Vec<usize>, [f32; 4]>>>,
    /// A divider drag in flight: the split's path, which divider, where the
    /// pointer was along the drag axis in px, and the share when it began.
    /// The moves land on the window root the way the rail splitter's do, so
    /// the pointer can leave the handle (#2472, #2476).
    pub(crate) split_drag: Option<DividerDrag>,
    /// The pane a pending pick splits; the sidebar fills it (#2472).
    pub(crate) split_target: Option<Uuid>,
    /// Where a dragged pane would land (#2683): the pane under the pointer and
    /// which half of it. Live only while a pane is in flight, so a stale value
    /// can never outline a pane nobody is dragging.
    pub(crate) pane_drop: Option<(Uuid, DropHalf)>,
    /// The layout a split is waiting on a session for; the sidebar picks it (#2438).
    pub(crate) split_pick: Option<Layout>,
    /// Whether the pick is what opened the sidebar (#3104).
    ///
    /// The pick needs the rail up to name a session, so a closed rail opens for
    /// it. Only that opening is undone when the pick lands: a rail the user
    /// opened themselves, or one a row menu and the Seeker split into, stays
    /// open because closing it would take away something they asked for.
    pub(crate) split_opened_rail: bool,
    /// A split's new session is being created, so the created row fills the pane.
    pub(crate) split_creating: bool,
    /// The row whose message menu is open (#2613). One at a time, like the
    /// rail's: opening another closes the first.
    pub(crate) msg_menu: Option<Uuid>,
    /// The row that menu was opened on, captured at the press (#2827).
    ///
    /// The menu's actions used to read the row back out of the window's own
    /// transcript, which meant a row living in a split leaf could not be
    /// copied, answered or forwarded: it was never in that transcript. The
    /// press already has the row, so it is carried instead of looked up.
    pub(crate) msg_menu_row: Option<Message>,
    /// Which leaf that row lives in (#2827). `None` is the window's own chat;
    /// `Some` is the split leaf of that session, whose composer an answer
    /// belongs in.
    pub(crate) msg_menu_leaf: Option<Uuid>,
    /// True when the captured row is an exact, unsaved live entry (#2925).
    /// React and Select stay off it because there is no persisted row id.
    pub(crate) msg_menu_live: bool,
    /// The row open for an in-place rewrite in the window's chat (#2826).
    /// A split leaf holds its own on its `PaneView`.
    pub(crate) edit_row: Option<chat_leaf::InlineEdit>,
    /// Where that row's right press landed, in window coordinates (#2690).
    /// The card opens under the pointer instead of pinned to the row's top
    /// corner, so a long message cannot put it half a screen away.
    pub(crate) msg_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// Which menu's expiry timer is current. Opening another menu invalidates
    /// the old timer, so it cannot close the new one early (#2862).
    pub(crate) msg_menu_generation: u64,
    /// The row whose copy icon is showing its check (#2750). One at a time:
    /// copying another row moves the mark rather than lighting two.
    pub(crate) copied_row: Option<Uuid>,
    /// Which copy's timer is current, the same generation rule the menu's
    /// expiry follows (#2750).
    pub(crate) copied_generation: u64,
    /// The row whose crab pack is open (#2825). One card at a time, so it
    /// closes the message menu that opened it.
    pub(crate) react_menu: Option<Uuid>,
    /// Where that pack opened, carried over from the menu that asked for it.
    pub(crate) react_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// Select mode (#2613). The picks live in the model, keyed by message id,
    /// so a scroll or a refresh never drops one; `is_active` is what the
    /// selection bar answers to.
    pub(crate) msg_select: Selection<Uuid>,
    /// A forward waiting for its destination (#2608): the rows picked when
    /// Forward was pressed. `Some` swaps the composer for the session picker.
    pub(crate) forward_pick: Option<Vec<Message>>,
    /// Which leaf opened that picker (#2827). `None` is the window's chat;
    /// `Some` puts the picker in the split leaf whose row was forwarded.
    pub(crate) forward_leaf: Option<Uuid>,
    /// The destination picker's query, typed into the bar itself.
    pub(crate) forward_query: String,
    /// The destination the picker points at, so the arrows walk it and Enter
    /// takes it. `None` falls back to the first match.
    pub(crate) forward_cursor: Option<Uuid>,
    /// The icon picked for the topic being named; None means the default (#2414).
    pub(crate) topic_draft_icon: Option<String>,
    /// Custom icon images read once from the store, keyed by stored value
    /// (#2790). The paint clones an Arc, never reads a file; the map
    /// rebuilds when the topic icons load and after an image is picked.
    pub(crate) icon_images: HashMap<String, Arc<Image>>,
    /// The Help Center body's scroll position, so the arrows can move it (#2407).
    pub(crate) help_scroll: ScrollHandle,
    /// A splitter drag is in flight. The move events land on the window root,
    /// not the 4px handle, so the pointer can leave the handle without ending
    /// the drag.
    pub(crate) dragging_rail: bool,
    /// The pointer x and rail width when the drag began, in pixels.
    pub(crate) drag_from: Option<(f32, f32)>,
    /// When the last external file-drag move was seen. gpui turns a Finder
    /// drag into a synthetic drag of `ExternalPaths` but keeps `active_drag`
    /// private, so freshness is how a drag that leaves the window stops
    /// counting: the poll tick retires the flag a few frames after the moves
    /// stop, and the drop clears it outright.
    pub(crate) drag_seen: Option<std::time::Instant>,
    /// Where the right press that opened the rail's context menu landed, in
    /// window coordinates. The card opens just past the rail at that
    /// height instead of over the row it belongs to (#2742).
    pub(crate) rail_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// The turn running in a session that has no leaf of its own (#2718), so
    /// its rail card can say what it is doing instead of showing the last
    /// message the ledger holds. Kept apart from the panes and the stash on
    /// purpose: this is the only record of a turn nobody is looking at, and
    /// the entry is dropped the moment that turn settles.
    pub(crate) rail_live: HashMap<Uuid, transcript::LiveTurn>,
    /// A leftward slide in flight on a rail row: the session and the pointer
    /// x where the drag began. Past the threshold the row's menu opens.
    pub(crate) rail_swipe: Option<(Uuid, f32)>,
    /// The row whose menu a slide just opened. gpui fires a click on mouse-up
    /// whenever the pointer is still over the element, movement or not, so
    /// without this the release that ends a slide also opens the chat the
    /// slide was meant to act on.
    pub(crate) rail_slide_opened: Option<Uuid>,
    /// The conversation a delete is waiting on. Every delete path arms this
    /// and only the dialog's own button erases, so no gesture can cost a
    /// conversation.
    pub(crate) confirm_delete: Option<Uuid>,
    /// The conversation whose topics are about to be folded in and switched
    /// off (#2863). Arming this is the first click; the dialog's own button is
    /// the second, because the fold cannot be undone.
    pub(crate) confirm_disable_topics: Option<Uuid>,
    /// The Group topics picker: the parent row being grouped under, the
    /// candidate sessions (id, title, date), the multi-selection, the keyboard
    /// cursor, a guard sentence on refusal, and a busy flag while attaching.
    pub(crate) group_target: Option<Uuid>,
    pub(crate) group_candidates: Vec<(Uuid, String, String)>,
    pub(crate) group_selected: std::collections::HashSet<Uuid>,
    pub(crate) group_cursor: usize,
    pub(crate) group_error: Option<String>,
    pub(crate) group_busy: bool,
    /// Whether the picker has already shown its fold warning (#2864). The
    /// owner asked for a refusal that can be forced: the first Enter names
    /// what will happen to the selected families, the second one goes ahead.
    pub(crate) group_warned: bool,
    /// What has been typed into the picker's field (#2864). Narrowing happens
    /// against this, never against the list in place, so clearing the field
    /// brings every candidate back.
    pub(crate) group_filter: String,
    /// Whether the key reference is open (#2320). The overlay reads the same
    /// table the terminal's `/help` prints, so the two cannot disagree.
    pub(crate) help_open: bool,
    /// The file a click on a transcript tile opened (#2395). One at a time:
    /// the overlay is modal, so a second preview replaces the first instead of
    /// stacking.
    pub(crate) preview: Option<preview::Preview>,
    /// Where each folded prompt's markdown lives, keyed by row id (#3064).
    /// Written when the window installs its rows, so a card never promises a
    /// file nobody made. Empty until the first install lands.
    pub(crate) prompt_docs: HashMap<Uuid, std::path::PathBuf>,
    /// How far the previewed image is zoomed, 1.0 at open. Reset on every open
    /// so a fresh click never inherits the last picture's magnification.
    pub(crate) preview_zoom: f32,
    /// What a clip's overlay can show: its poster frame, length and size,
    /// probed once on the click that opened it. Never gathered while painting,
    /// because both facts come from a subprocess and a render that shells out
    /// is a render that stutters.
    pub(crate) preview_video: Option<preview::VideoFacts>,
    /// Markdown and plain text load off the UI thread and carry their own
    /// scroll position. A generation rejects a response for an older file.
    pub(crate) preview_document: Option<preview_document::DocumentState>,
    pub(crate) preview_document_generation: u64,
    /// The built-in editors the bang operator opened, one per conversation
    /// (#3163). Keyed by the session that asked, so a split keeps every
    /// pane's buffer, cwd and focus apart: pane A's `!vi` must never hand
    /// pane B's text to the dialog, and each session resolves its file
    /// against its own working directory.
    pub(crate) editor_dialogs: HashMap<Uuid, editor_dialog::EditorDialog>,
    /// Which conversation's editor is on top and holds the keyboard (#3163).
    pub(crate) editor_focus: Option<Uuid>,
    /// Bumps on every open and close, so a file read that lands late lands
    /// on the dialog that actually asked for it (#2395).
    pub(crate) editor_generation: u64,
    /// The chat's `...` menu, as the session it acts on (#2480).
    ///
    /// It carries the id, never a bare flag: a right press on a topic tab
    /// opens the same menu for a session that is not the one on screen.
    pub(crate) topic_menu: Option<uuid::Uuid>,
    /// Where that right press landed, in window coordinates (#2690). The card
    /// opens under the pointer instead of at the column's top corner, which in
    /// a split was not even the pane whose tab was pressed.
    pub(crate) topic_menu_at: Option<gpui::Point<gpui::Pixels>>,
    /// The topic name being typed, while the name prompt is open (#2303).
    /// A real text field, so the prompt carries a caret and a selection and
    /// answers the composer's editing chords (#2892). A bare `String` put the
    /// caret at the end and gave `cmd-a`, `alt+left` and `cmd+x` nothing to
    /// act on, so a rename could only be typed or backspaced.
    pub(crate) topic_draft: Option<TextField>,
    /// The pick the window has to explain before it goes on (#2605): the
    /// seeker's topic pick named a topic, and a topic cannot hold topics, so
    /// this carries that row's title for the warning instead of opening a
    /// prompt that would nest one under another.
    pub(crate) topic_warning: Option<String>,
    /// A pick that named a conversation already open in a pane (#2819). The
    /// plain open path used to drag it into the chat leaf and close the pane
    /// it came from, so a pick could destroy a split the user built. This
    /// carries that row's title for the card that says so instead.
    pub(crate) pane_warning: Option<String>,
    /// Which side of the chat the topic row sits on (#2402).
    pub(crate) topic_edge: topic_tabs::Edge,
    /// Where the chat's topic row landed on the last paint, in window
    /// coordinates (#3097). A split pick's scrim leaves that band bright so a
    /// press reaches a chip on any edge, and it is measured rather than
    /// derived because a side row is centred beside the conversation, so no
    /// constant knows where it starts.
    pub(crate) topic_strip_rect: std::rc::Rc<std::cell::RefCell<Option<topic_tabs::StripRect>>>,
    /// Whether the chat's topic row shows its whole family (#2949). The row
    /// folds past `BAR_TOPICS_SHOWN` and prints a pill; the pill was a label,
    /// so the folded topics could only be reached from the rail. A press
    /// expands the row, which then wraps and scrolls, and the same pill folds
    /// it back.
    pub(crate) chat_topics_expanded: bool,
    /// The topic being renamed, while the prompt is open for a rename (#2304).
    /// None means the prompt names a new topic.
    pub(crate) topic_rename: Option<uuid::Uuid>,
    /// The read-only info panel for a topic, while it is open (#2304).
    pub(crate) topic_info: Option<sessions::TopicInfo>,
    /// The theme modal's accent, as hue/saturation/value so a drag keeps its
    /// precision instead of round-tripping through 8-bit channels (#2619).
    pub(crate) themes_hue: f32,
    pub(crate) themes_sat: f32,
    pub(crate) themes_val: f32,
    /// Whether the theme modal is open. It is an overlay, not a screen: the
    /// window behind it keeps showing whatever it showed, so the choice is
    /// made against real content (#2619).
    pub(crate) themes_open: bool,
    /// The card the keyboard is standing on: 0 orgn, 1 light, 2 custom, 3 auto.
    pub(crate) themes_card: usize,
    /// The whole-window transparency in force (#2922). Zero keeps the native
    /// window opaque; a positive value composites the root with the desktop.
    pub(crate) window_transparency: u8,
    /// The native compositing mode last applied by the renderer. Tracking it
    /// avoids sending the same platform mutation on every frame.
    pub(crate) window_background_transparent: bool,
    /// Which accent control the pointer is dragging, if any.
    pub(crate) themes_drag: Option<accent::Drag>,
    /// Whether the pointer is dragging the whole-window transparency track.
    pub(crate) themes_transparency_drag: bool,
    /// The wheel's box in window coordinates, recorded at paint time so a
    /// press can be turned into a hue (#2619).
    pub(crate) themes_wheel: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// The saturation and brightness tracks' boxes, recorded the same way.
    pub(crate) themes_sat_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    pub(crate) themes_val_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// The whole-window transparency track, recorded the same way (#2922).
    pub(crate) themes_transparency_track: std::rc::Rc<std::cell::RefCell<Option<[f32; 4]>>>,
    /// Whether the font list under the cards is open (#2786). Closed by
    /// default, because the list is the machine's whole family catalog and the
    /// modal is small.
    pub(crate) fonts_open: bool,
    /// Whether the installed families have been read. The enumeration walks
    /// CoreText on macOS and keeps no cache, so it runs once per process.
    pub(crate) fonts_loaded: bool,
    /// The settings modal (#2878): the values are read fresh from the loaded
    /// config on open, one row per `settings_view::SPECS` entry, followed by
    /// the credential rows. `settings_secret` is the one armed credential
    /// edit: its row index and the typed buffer, which is only ever painted
    /// as bullets.
    pub(crate) settings_open: bool,
    pub(crate) settings_values: Vec<settings_view::RowValue>,
    pub(crate) settings_creds: Vec<settings_view::CredSpec>,
    pub(crate) settings_cursor: usize,
    /// Which rail category the settings modal paints (#2890).
    pub(crate) settings_section: settings_view::SettingsSection,
    pub(crate) settings_scroll: ScrollHandle,
    pub(crate) settings_status: Option<String>,
    pub(crate) settings_secret: Option<(usize, String)>,
    /// The /cd picker's browse state (#2527): the directory on screen, its
    /// subdirectories read at open and on every descend, the row the
    /// keyboard stands on, and whether dotfile directories are listed.
    pub(crate) cd_dir: std::path::PathBuf,
    pub(crate) cd_entries: Vec<std::path::PathBuf>,
    pub(crate) cd_selected: usize,
    pub(crate) cd_show_hidden: bool,
    /// The session the open picker is about. Pinned when the screen opens,
    /// because a split window's caret leaf is not the chat leaf: reading
    /// `chat_session` at either end sent the change to the conversation the
    /// window happens to hold while the reader was looking at a pane, and a
    /// switch made while the picker was open moved the target out from under
    /// them.
    pub(crate) cd_session: Option<Uuid>,
    /// The model picker's rows, built when the screen opens so a slow frame
    /// never rebuilds the catalogue.
    pub(crate) models_rows: Vec<models::Row>,
    /// The live search text. Selection indexes the filtered rows, never the
    /// unfiltered catalogue underneath them.
    pub(crate) models_query: String,
    /// The row the arrows stand on.
    pub(crate) models_index: usize,
    /// The provider the models screen's list marks. A click sets it and moves
    /// the model cursor to that provider's first visible model.
    pub(crate) models_provider: Option<String>,
    /// The session that opened the picker. A split pane owns its own model,
    /// so the picker must apply and answer in this session, not the window.
    pub(crate) models_session: Option<Uuid>,
    /// The session whose switch is in flight. A second Enter while this is
    /// set is ignored, so one pick cannot write the same confirmation twice.
    pub(crate) models_switching: Option<Uuid>,
    /// Whether the picker's next pick applies to every non-archived session
    /// (#3035). All-sessions is an explicit act, so it starts off and every
    /// opening of the picker leaves it off.
    pub(crate) models_scope_all: bool,
    /// A scope-all pick that has been counted and is waiting for its second
    /// tap (#3035): the pair, and how many other sessions the write would
    /// change. The prompt names that number before anything is written.
    pub(crate) models_apply_confirm: Option<(String, usize)>,
    /// Shared request lifecycle and pending providers. Static rows are only a
    /// fallback; every configured provider gets a live discovery request.
    pub(crate) models_fetch: Option<std::sync::mpsc::Receiver<(u64, model_fetch::FetchOutcome)>>,
    pub(crate) models_fetch_lifecycle: opencrabs::ui_core::model_selector::FetchLifecycle,
    pub(crate) models_pending: Vec<FetchRequest>,
    pub(crate) models_fetching: bool,
    pub(crate) models_fetch_error: Option<String>,
    /// The skills screen's rows, read from the terminal's own loader when the
    /// screen opens so the list can never drift from `/skills` in the TUI,
    /// and the row the arrows stand on.
    pub(crate) skills_all: Vec<opencrabs::brain::skills::Skill>,
    pub(crate) skills_rows: Vec<opencrabs::brain::skills::Skill>,
    pub(crate) skills_query: String,
    pub(crate) skills_index: usize,
    /// Project rows and navigation, loaded by the shared service on demand.
    pub(crate) projects: projects::ProjectsState,
    /// The onboarding wizard while its screen is open. Built on first open:
    /// constructing it reads the live config, which is not something to do on
    /// every window start when most sessions never onboard.
    pub(crate) onboard: Option<onboard::Onboard>,
    /// The plan checklist strip above the editor, re-read when its file moves.
    pub(crate) plan: plan_strip::PlanStripState,
    /// Which session the strip's path was resolved for, so the DB lookup runs
    /// once per selection rather than once per frame.
    pub(crate) plan_for: Option<Uuid>,
    /// Panes whose plan strip the user expanded (#2624). A pane opens at the
    /// density its pane count and width allow, and a click on the card trades
    /// the conversation's room for the checklist, per pane, so expanding one
    /// leaf never grows the card in the others.
    pub(crate) plan_expanded: std::collections::HashSet<Uuid>,
    /// Sessions whose task list is open. At rest it is always the one line:
    /// a session is here only after a click or a Down from the composer, and
    /// leaves when it has nothing running, so the next task starts collapsed.
    pub(crate) tasks_open: std::collections::HashSet<Uuid>,
    /// The session whose task list holds the keyboard, after Down from its
    /// composer. The row the keys stand on is that session's `task_open`.
    pub(crate) task_focus: Option<Uuid>,
    /// The follow-up offer holding the keyboard, after Up from a composer with
    /// an offer waiting (#3101): the session it belongs to and the row the
    /// keys stand on. One at a time, because the caret is in one place; `None`
    /// while the field has it.
    pub(crate) options_focus: Option<(Uuid, usize)>,
    /// The task whose output is showing, per session.
    pub(crate) task_open: HashMap<Uuid, Uuid>,
    /// The last output read for an open task, with when it was read and the
    /// file it came from once that file was found. Re-read on a one-second
    /// tick, the same cadence the strip's elapsed time moves at.
    pub(crate) task_tails: HashMap<Uuid, TaskTail>,
    /// The task opened into its full view, with Enter on its row or the
    /// row's open door. It outlives the task: the view stays on the last
    /// output once the task ends, until the reader closes it.
    pub(crate) task_detail: Option<task_detail::TaskDetail>,
    /// The scroll of that view's output.
    pub(crate) task_detail_scroll: ScrollHandle,
    /// Bumped on every open, so the repaint loop of a view that was closed
    /// and reopened ends instead of doubling up.
    pub(crate) task_detail_generation: u64,
    /// Plan tasks whose settle already reached the rail flash (#2691),
    /// durable across launches and keyed by session (#3004). One ledger for
    /// the whole window, shared by the window strip and every pane strip, so a
    /// plan drawn in two places flashes once. The terminal keeps its own
    /// ledger over the same file (`utils::settle_ledger`), and each write
    /// merges with what is on disk, so the two surfaces cannot erase each
    /// other's records.
    pub(crate) settle_ledger: opencrabs::utils::settle_ledger::SettleLedger,
    /// Until when each session's rail plan bar stays flashing orange, per
    /// session, keyed by the plan's own session id (#2701 follow-up).
    ///
    /// The deadline is written on the exact event that rings the chime, so the
    /// sound and the blink are two surfaces of one fact: a task just settled.
    /// It is memory only, like the rail's streaming set: a window restart has
    /// nothing to flash about.
    pub(crate) plan_flash_until: HashMap<Uuid, std::time::Instant>,
    /// The conversation on screen: its transcript, its viewport and whether
    /// that viewport follows the newest row (#2805). One value, so a frame
    /// that touches the chat stops being a frame that rebuilds the window.
    pub(crate) chat_state: chat_leaf::ChatState,
    /// The session `chat_state` holds, mirrored on the window because the
    /// rail, the split fill, the plan strip and the model picker decide with
    /// it on every frame. Written in exactly one place, `set_chat_session`, so
    /// it cannot drift from the owner it mirrors.
    pub(crate) chat_session: Option<Uuid>,
    /// The one middle-click auto-scroll gesture in this window (#2964). Its
    /// target owns the scroll state; this value owns only pointer and timing.
    pub(crate) middle_scroll: Option<middle_scroll::MiddleScroll>,
    /// Whether GPUI already has the next auto-scroll frame queued. This stays
    /// set through cancellation until that callback arrives, preventing a new
    /// gesture from starting a second animation loop beside the old one.
    pub(crate) middle_scroll_frame_pending: bool,
    /// When the first Ctrl+C armed the quit window (#2308). `None` means no
    /// press is waiting, and the next one only clears and arms again.
    pub(crate) ctrl_c_pending: Option<std::time::Instant>,
    /// The keystroke interceptor that hands Ctrl+C to this window before the
    /// composer's own copy chord can take it (#2308). Held here because
    /// dropping the subscription would silently unregister it.
    pub(crate) ctrl_c_interceptor: Option<gpui::Subscription>,
    /// The rail's own scroll handle, so a cursor moved by the arrows can
    /// ask the next frame to bring its row into view (#2351).
    pub(crate) rail_scroll: ScrollHandle,
    /// The rail itself, drawn as a cached view so a repaint it has no stake in
    /// replays its last prepaint instead of rebuilding every row (#2933).
    pub(crate) rail_view: Entity<rail_view::RailView>,
    /// The composer and the queue: the field the user types in, the drafts kept
    /// per conversation, the messages waiting behind the turn, and the flags
    /// the hint and the spinner read. One value, so the chat leaf inherits one
    /// field instead of seven (#2805).
    pub(crate) composer: chat_leaf::Composer,
    /// The submitted history every composer in this window walks. One handle
    /// cloned into the window's field and every split pane, so a message sent
    /// in one is recalled in all of them, in real time, and the terminal reads
    /// the same list off the same file.
    pub(crate) history: composer::SharedHistory,
    /// Agent-side state (task 5). Events from the bridge's bootstrap and
    /// turn threads land in the receiver; the poll tick drains it.
    pub(crate) agent_rx: std::sync::mpsc::Receiver<agent_bridge::TurnEvent>,
    /// Reloads the watcher thread hands the window: a config.toml, keys.toml or
    /// commands.toml save, already loaded and published to `Config::current()`
    /// by the terminal's own reload pass. Drained on the poll tick, which
    /// re-derives what the window cached at boot (`dashboard_config`).
    pub(crate) config_reload: std::sync::mpsc::Receiver<config_reload::Reload>,
    pub(crate) sink: agent_bridge::EventSink,
    /// Set once bootstrap reports the service live. Turns run on the pane's
    /// open conversation (`chat_state.transcript.session`), never on a session
    /// of the bridge's own, so the answer always lands where the question was
    /// typed.
    pub(crate) agent_ready: bool,
    /// The turn in flight: the card, whether it is still running, the handle
    /// that stops it, the prompt size the provider reported and the reload
    /// handshake waiting for its rows. One value, so a switch moves the turn
    /// whole and the chat leaf inherits one field instead of five (#2805).
    pub(crate) turn: chat_leaf::Turn,
    /// Turns running in conversations the window is not painting (#2616).
    /// Keyed by the session that owns each one, so a switch parks the turn on
    /// screen and restores the destination's, and the window never has to
    /// guess which conversation a card belongs to.
    pub(crate) stashed: std::collections::HashMap<Uuid, chat_leaf::Turn>,
    /// Sessions whose spoken reply is being made right now (#3092).
    ///
    /// The turn has settled and its card is on screen, and the clip is still a
    /// network round trip away. Kept on the window rather than on the card
    /// because the reload that follows `Done` rebuilds every card, so a flag
    /// living on one of them would be erased by the next read. A split leaf
    /// reads the same set, so a pane's card says it too.
    pub(crate) voice_pending: std::collections::HashSet<Uuid>,
    /// The open conversation's context window: the compaction budget the
    /// service actually enforces for it, not a hardcoded 200K.
    pub(crate) ctx_max: u32,
    /// What each settled turn cost and how fast it answered (#2324), plus the
    /// numbers waiting for their rows to land. One owner, like the handshake
    /// above: both are read from the same reload.
    pub(crate) metrics: chat_leaf::Metrics,
    /// The footer's place, resolved from the open conversation rather than
    /// from the process (#3036): the session it was read for, the directory
    /// that session works in, and the branch checked out there. The window
    /// used to print the launch directory and a branch read once at startup,
    /// so a `/cd` moved the session and left the footer naming the old place.
    pub(crate) footer_dir: Option<FooterPlace>,
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
pub(crate) enum SendOrigin {
    /// Typed into a composer: the overlay gate applies.
    Composer,
    /// Minted by the window for a named session: chrome cannot refuse it.
    Window,
}
