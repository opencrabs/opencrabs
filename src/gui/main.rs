cut map:
  stage 1:   238-  263     26 lines  '/// Which pane the right side draws. Chat is the default scr'
  stage 2:   265-  308     44 lines  '/// A divider drag in flight (#2472, #2476): which split, wh'
  stage 3:   310-  315      6 lines  "/// The rail's default width, and the floor it may be dragge"
  stage 1:   317-  332     16 lines  "/// The place the footer names: the open conversation's work"
  stage 1:   334-  996    663 lines  '/// The window root. Every string on screen is real data or '
  stage 3:   998- 1028     31 lines  '/// Apply sidebar visibility without letting a closed rail r'
  stage 4:  1030- 4485   3456 lines  'impl Render for Dashboard {'
  stage 2:  4487- 4508     22 lines  '/// The drag handle between the rail and the chat pane. The '
  stage 5:  4510- 5241    732 lines  'fn main() {'
  stage 1:  5243- 5260     18 lines  '/// Who asked for a send.'
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
mod dashboard_render;
mod dashboard_screens;
mod dashboard_state;
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
use crate::dashboard_panes::{DividerDrag, PaneDrag, PaneGhost, splitter};
use crate::dashboard_rail::{
    RAIL_DEFAULT_REM, RAIL_MAX_REM, RAIL_MIN_REM, set_rail_open_state,
};
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

