//! Starting the desktop window.
//!
//! `main.rs` keeps the module list and a three-line `fn main`; everything the
//! process does before the first frame lives here, so the order that matters
//! (version check, single instance, then the window) reads in one place.

use super::*;

/// Handle the command line, then start the window.
///
/// The order is load-bearing: a `--version` run must answer and exit before
/// the log, the ledger or any connector is opened (#2841), and the profile is
/// claimed only once a window exists (#2784).
pub(crate) fn run() {
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
    // Which hints the reader has hidden, read before the first frame so a
    // dismissed line never flashes in and out (#3121). A read that fails shows
    // every hint rather than none: a repeated hint is a small annoyance, a
    // silently empty card is a feature that looks broken.
    let hint_dismissed = hint_dismiss::load(db_path.clone());
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
    // The frame that file remembers (#3082). Kept beside the tree because
    // `restore_layout` consumes the tree, and the boot needs the frame before
    // the window exists.
    let saved_window = saved_layout.as_ref().and_then(|saved| saved.window);
    // Whether the rail was drawn (#3105). Read here for the same reason as the
    // frame: the first paint already has to know, and a rail the user closed
    // must not flash open and then shut on every launch.
    let saved_rail_open = saved_layout.as_ref().and_then(|saved| saved.rail_open);

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
        // The work area, not the whole screen (#3082). A fixed 980x760 asks for
        // 1470x1140 physical on a 1080p display at 150%, which is taller than
        // the screen: the input opened under the taskbar with no way to drag it
        // back. Fitted, the frame always lands inside what the platform lets it
        // use.
        //
        // A frame the last run left wins over the default, as long as it still
        // sits on a screen this launch can see: a window remembered on a
        // monitor that is no longer attached would open where nothing can be
        // clicked.
        let bounds = window_geometry::boot_window_bounds(saved_window.as_ref(), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                // Below this the single-column grid still fits every card's
                // 420px minimum, so nothing clips on load.
                window_min_size: Some(opencrabs::ui_core::window_geometry::MIN_SIZE),
                titlebar: Some(TitlebarOptions {
                    title: Some("OpenCrabs".into()),
                    // One constant, shared with the frame that decides who
                    // draws the min, max and close buttons (#3083): a window
                    // created with one answer and drawn with the other ends up
                    // with two sets of buttons or none.
                    appears_transparent: TITLEBAR_APPEARS_TRANSPARENT,
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
                        // Up off the top of the field reaches the follow-up
                        // offer above it, the mirror of Below and ahead of
                        // history (#3101).
                        editor::EditorEvent::Above => {
                            if let Some(session) = this.chat_session {
                                this.focus_options(session, cx);
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
                        // The frame the last run left (#3082). Seeded from the
                        // file so the first paint already knows where the
                        // window is, and so an untouched window writes nothing.
                        window_frame: saved_window,
                        window_frame_seen: saved_window,
                        window_frame_saved: saved_window,
                        db_path: db_path.clone(),
                        hint_dismissed,
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
                        voice_paint: voice_play::PaintState::default(),
                        rail_width: RAIL_DEFAULT_REM,
                        // The rail the last run closed stays closed (#3105).
                        rail_open: saved_rail_open.unwrap_or(true),
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
                        split_opened_rail: false,
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
                        prompt_docs: HashMap::new(),
                        preview_zoom: 1.0,
                        preview_video: None,
                        preview_document: None,
                        preview_document_generation: 0,
                        editor_dialogs: HashMap::new(),
                        editor_focus: None,
                        editor_generation: 0,
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
                        topic_strip_rect: std::rc::Rc::default(),
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
                        models_scope_all: false,
                        models_apply_confirm: None,
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
                        options_focus: None,
                        task_open: HashMap::new(),
                        task_tails: HashMap::new(),
                        task_detail: None,
                        task_detail_scroll: ScrollHandle::new(),
                        task_detail_generation: 0,
                        settle_ledger: opencrabs::utils::settle_ledger::SettleLedger::open(),
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
                        voice_pending: std::collections::HashSet::new(),
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
