//! TUI Runner
//!
//! Main event loop and terminal setup for the TUI.

use super::app::App;
use super::capture::CaptureWriter;
use super::events::EventHandler;
use super::render;
use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// The concrete terminal type used everywhere in this module. The backend
/// wraps the real stdout in [`CaptureWriter`] so every byte the render loop
/// emits is recorded for the #1719 garble post-mortem.
pub(crate) type TuiTerminal = Terminal<CrosstermBackend<CaptureWriter<io::Stdout>>>;

/// Captures location + filtered backtrace of the last panic so the
/// render loop can correlate a caught panic with its source. `catch_unwind`
/// only hands back the payload message, not the location — so we stash
/// both via the panic hook (which runs before `catch_unwind` swallows it)
/// and read them out after.
///
/// The `file:line:col` alone usually points at ratatui internals
/// (e.g. `ratatui-core/.../buffer.rs:250`) — useless without knowing
/// which of *our* widgets called in. The filtered backtrace picks the
/// first frame under `opencrabs::tui::` so the caller widget is named.
static LAST_PANIC_LOCATION: LazyLock<Mutex<Option<PanicInfo>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Clone)]
struct PanicInfo {
    file: String,
    line: u32,
    column: u32,
    /// First OpenCrabs frame pulled out of the backtrace, if any.
    /// Format `module::fn at path:line` — typically the widget render call
    /// that fed ratatui an out-of-bounds coord.
    opencrabs_frame: Option<String>,
}

/// Walk a captured `Backtrace` and return the first frame whose symbol
/// name starts with `opencrabs::`. `Backtrace`'s `Display` impl renders
/// one frame per two lines:
///
/// ```text
///    N: symbol_name
///              at src/path.rs:LINE:COL
/// ```
///
/// We scan for any line containing `opencrabs::` (skipping the panic
/// hook itself), pull the symbol + `at ...` pair, and return them so
/// the render-panic log points at the actual widget that called
/// ratatui with an out-of-bounds coord, not ratatui's internal bounds
/// check.
fn first_opencrabs_frame(bt: &std::backtrace::Backtrace) -> Option<String> {
    let text = format!("{}", bt);
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.contains("opencrabs::")
            && !trimmed.contains("first_opencrabs_frame")
            && !trimmed.contains("runner::")
        {
            let symbol = trimmed
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == ':' || c == ' ')
                .to_string();
            let at = lines
                .peek()
                .filter(|l| l.trim().starts_with("at "))
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            return Some(if at.is_empty() {
                symbol
            } else {
                format!("{} {}", symbol, at)
            });
        }
    }
    None
}

/// Force-restore terminal state. Safe to call from signal handlers and panic hooks.
pub(crate) fn force_restore_terminal() {
    let _ = disable_raw_mode();
    let _ = restore_to(&mut io::stdout());
}

/// Write the complete restore sequence (kitty flags pop, alt screen, bracketed
/// paste, focus change, mouse capture, cursor show) to any sink. Split out
/// for #1964: every death path must emit the whole sequence, and the exact
/// bytes are pinned by tui_restore_test against a capture buffer.
pub(crate) fn restore_to<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    execute!(
        w,
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture
    )?;
    execute!(w, crossterm::cursor::Show)
}

/// Re-establish TUI terminal ownership after an editor handoff (#1744) gave
/// the real tty to a full-screen child. Mirrors the `run()` startup sequence:
/// raw mode, alternate screen, paste/focus/mouse capture, kitty flags — then
/// drains leftover keystrokes so editor escape bytes don't leak into the
/// input buffer, and clears for a full repaint.
pub(crate) fn reinit_terminal_for_editor(terminal: &mut TuiTerminal) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableMouseCapture
    )?;
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    while crossterm::event::poll(std::time::Duration::from_millis(10))? {
        let _ = crossterm::event::read();
    }
    terminal.clear()?;
    Ok(())
}

/// Run the TUI application
pub async fn run(mut app: App) -> Result<()> {
    // Boot-time theme apply (#1364 commit D): read the active config and
    // activate the named preset before the first render. `None` / empty /
    // unknown name falls through to CRAB_DARK (byte-identical default).
    // Live switching via `/theme set <name>` re-enters through `theme::set`
    // below; no need to reload config on every frame.
    {
        // Build the pack first: `presets::by_name` only sees pack themes once
        // `themes()` has run, and nothing else runs it before this lookup, so a
        // saved pack theme such as `orng` was lost on every restart (#2164).
        let _ = crate::tui::theme_catalog::theme_pack::themes();
        let theme_name = crate::config::Config::current().tui.theme.clone();
        if let Some(n) = theme_name
            .as_deref()
            .map(render::theme::configured_name)
            .filter(|s| !s.is_empty())
            && let Some(t) = render::presets::by_name(n).or_else(|| render::user_themes::find(n))
        {
            render::theme::set(t);
        }
    }

    // Install panic hook that restores terminal before printing the panic.
    // Without this, a panic leaves the terminal in raw mode with no cursor.
    // Also stash the panic location so the render `catch_unwind` path can
    // log WHERE a caught render panic happened (the payload only carries
    // the message, not the source location).
    //
    // #1791: the stash was the whole story, and it only ever reached the log
    // through the render path. Everything else unwound out and took the
    // location, the backtrace and the message with it. So the hook now also
    // persists the record it already computed, before the terminal is
    // restored and the default hook hands the text to a stderr that dies with
    // the window.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut record = crate::logging::panic_record::PanicRecord {
            file: None,
            line: None,
            column: None,
            message: crate::logging::panic_record::payload_message(info.payload()),
            opencrabs_frame: None,
        };
        let mut backtrace = String::new();
        if let Some(loc) = info.location() {
            record.file = Some(loc.file().to_string());
            record.line = Some(loc.line());
            record.column = Some(loc.column());
            // Force-capture unconditionally — render panics are rare,
            // and RUST_BACKTRACE=1 is usually unset in TUI sessions.
            let bt = std::backtrace::Backtrace::force_capture();
            let opencrabs_frame = first_opencrabs_frame(&bt);
            record.opencrabs_frame = opencrabs_frame.clone();
            backtrace = format!("{bt}");
            if let Ok(mut slot) = LAST_PANIC_LOCATION.lock() {
                *slot = Some(PanicInfo {
                    file: loc.file().to_string(),
                    line: loc.line(),
                    column: loc.column(),
                    opencrabs_frame,
                });
            }
        }
        crate::logging::panic_record::persist(&record, &backtrace, &crate::logging::log_dir());
        force_restore_terminal();
        default_hook(info);
    }));

    // SIGINT (Ctrl+C) handler — forces terminal restoration and exits.
    // In raw mode, Ctrl+C is just byte 0x03 and crossterm may eat it if
    // stuck mid-escape-sequence. This handler bypasses the event loop.
    let sigint_flag = Arc::new(AtomicBool::new(false));
    let sigint_clone = sigint_flag.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            sigint_clone.store(true, Ordering::SeqCst);
            force_restore_terminal();
            // Give a moment for terminal to restore, then hard-exit
            std::process::exit(130); // 128 + SIGINT(2)
        }
    });

    // #1964: SIGTERM (kill, tmux respawn, ssh drop) and SIGHUP killed the TUI
    // with mouse capture still enabled, and the shell echoed every mouse
    // wiggle as ^[[<35;…M garbage. Mirror the SIGINT path: restore, then exit.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        tokio::spawn(async move {
            let (Ok(mut sigterm), Ok(mut sighup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                tracing::warn!("TUI could not install SIGTERM/SIGHUP handlers");
                return;
            };
            let code = tokio::select! {
                _ = sigterm.recv() => 143,
                _ = sighup.recv() => 129,
            };
            tracing::error!("TUI received exit signal {code}, restoring terminal");
            force_restore_terminal();
            std::process::exit(code);
        });
    }

    // #1964: a predecessor that died without restoring (SIGKILL, or a binary
    // predating the panic hook) leaves DECSET modes on; clear them before we
    // take over the terminal.
    force_restore_terminal();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableMouseCapture
    )?;
    // Kitty keyboard protocol: enables unambiguous modifier reporting
    // so Shift+Enter is distinguishable from plain Enter. Silently
    // ignored by terminals that don't support it (no-op, no errors).
    // On Windows, the legacy Console API errors instead of ignoring, so
    // we deliberately discard the result.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    let backend = CrosstermBackend::new(CaptureWriter::new(stdout));
    let mut terminal = Terminal::new(backend)?;

    // Signal that the TUI owns stdout — suppress_stdio() will skip fd 1
    // redirection to avoid racing with ratatui's escape-sequence writes.
    crate::utils::fd_suppress::set_tui_active(true);

    // Probe terminal truecolor capability once. Must run after terminal
    // setup (raw mode + alternate screen) so crossterm's env-var checks
    // see the right state. Result is cached for all subsequent `role()`
    // calls.
    render::theme::init_capability();

    // Force a full clear so stale content from a previous exec() restart
    // is wiped.
    terminal.clear()?;

    // Drain any stale terminal events (e.g. mouse events queued after a crash
    // where DisableMouseCapture never ran). Without this, queued escape
    // sequences leak into the input buffer as raw characters.
    while crossterm::event::poll(std::time::Duration::from_millis(10))? {
        let _ = crossterm::event::read();
    }

    // Fast sync init: decide mode and arm the header card. If we're going
    // to Chat, draw a first frame immediately so the user sees the header
    // card + empty chat + input *before* the blocking session load runs.
    // Onboarding skips the first frame so the wizard renders on first paint.
    let draw_first_frame = app.initialize_sync();
    if draw_first_frame {
        let app_ref: &mut App = &mut app;
        terminal.draw(move |f| render::render(f, app_ref))?;
    }

    // Now do the slow async init: load last session, sessions list, pane
    // preload, update check, DB integrity warnings. The header card stays
    // visible during this and vanishes on the 500ms timer from its arming
    // point inside `initialize_sync`.
    app.initialize().await?;

    // Start terminal event listener — hold the handle so the loop can abort
    // and restart it around an editor handoff (#1744).
    let event_sender = app.event_sender();
    let mut listener_handle = EventHandler::start_terminal_listener(event_sender);

    // Run main loop
    let result = run_loop(&mut terminal, &mut app, &sigint_flag, &mut listener_handle).await;

    // Restore terminal
    crate::utils::fd_suppress::set_tui_active(false);
    force_restore_terminal();

    result
}

/// Main event loop
async fn run_loop(
    terminal: &mut TuiTerminal,
    app: &mut App,
    sigint_flag: &AtomicBool,
    listener_handle: &mut tokio::task::JoinHandle<()>,
) -> Result<()> {
    use super::events::TuiEvent;

    // Tracks the last applied mouse-capture state so we only call
    // Enable/DisableMouseCapture when the app's desired state changes.
    let mut mouse_capture_applied = true;

    loop {
        // Check SIGINT flag (redundant safety — the handler already exits,
        // but this catches the race where the flag is set before exit)
        if sigint_flag.load(Ordering::SeqCst) {
            break;
        }

        // Sync mouse-capture state if the user toggled it via F12.
        if app.mouse_capture_enabled != mouse_capture_applied {
            if app.mouse_capture_enabled {
                execute!(terminal.backend_mut(), EnableMouseCapture)?;
            } else {
                execute!(terminal.backend_mut(), DisableMouseCapture)?;
            }
            mouse_capture_applied = app.mouse_capture_enabled;
        }

        // Flush debounced session refresh from remote channels.
        // Only fires after 500ms of quiet to avoid blocking the loop with
        // rapid DB queries during multi-tool Telegram runs.
        if let Some((session_id, queued_at)) = app.pending_session_refresh
            && queued_at.elapsed() >= std::time::Duration::from_millis(500)
        {
            app.pending_session_refresh = None;
            if app.is_current_session(session_id)
                && !app.processing_sessions.contains(&session_id)
                && let Err(e) = app.load_session(session_id).await
            {
                tracing::warn!("Debounced session refresh failed: {}", e);
            }
        }

        // Consume pending resize (just clears the flag; ratatui's
        // autoresize inside draw() handles the actual buffer resize).
        app.pending_resize.take();

        // Editor handoff (#1744): the bang allowlist parked the request and
        // returned; this loop owns the terminal + event reader, so the tty
        // changes hands here, before the next drain/draw sees any of it.
        // Unix only (#1755): nothing is ever parked on Windows.
        #[cfg(unix)]
        if let Some((cmd, origin_session)) = app.pending_editor_handoff.take() {
            let result = super::editor::run_editor_handoff(
                terminal,
                app.event_sender(),
                listener_handle,
                &cmd,
                &app.working_directory,
            )
            .await;
            // `reinit_terminal_for_editor` always re-enables mouse capture,
            // so the sync block above must know the applied state is now
            // "on" and toggle it back if the user had disabled it with F12.
            mouse_capture_applied = true;
            let _ = app.event_sender().send(TuiEvent::SystemMessage {
                session_id: origin_session,
                text: result.text,
            });
        }

        // Wrap every frame in synchronized output (DEC private mode
        // 2026) so the terminal buffers all escape sequences and
        // displays the complete frame atomically.  This eliminates
        // flicker on resize because the clear + full redraw appear as
        // a single update to the terminal.
        let _ = execute!(
            terminal.backend_mut(),
            crossterm::terminal::BeginSynchronizedUpdate
        );

        // Render — wrap in catch_unwind so a render-time panic (e.g. a
        // ratatui buffer OOB from some edge-case layout) is caught, logged,
        // and the loop continues instead of crashing the whole TUI.
        // Reborrow terminal/app as fresh mutable references for this
        // iteration so moving them into the catch_unwind closure doesn't
        // consume the outer references permanently.
        let term_ref: &mut TuiTerminal = &mut *terminal;
        let app_ref: &mut App = &mut *app;
        // Clear prior location before each draw so we only see what THIS
        // frame hit (panic hook overwrites the slot unconditionally, but
        // being explicit is cheap insurance).
        if let Ok(mut slot) = LAST_PANIC_LOCATION.lock() {
            *slot = None;
        }
        let draw_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            term_ref.draw(move |f| render::render(f, app_ref))
        }));
        match draw_result {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown render panic".to_string()
                };
                let (loc, caller) = LAST_PANIC_LOCATION
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .map(|info| {
                        let loc = format!(" at {}:{}:{}", info.file, info.line, info.column);
                        let caller = info
                            .opencrabs_frame
                            .map(|f| format!(" — caller: {}", f))
                            .unwrap_or_default();
                        (loc, caller)
                    })
                    .unwrap_or_default();
                tracing::error!("[TUI] render panic caught{}{}: {}", loc, caller, msg);
                // #1719: what the terminal was actually fed right before the
                // panic. Escape junk in the tail = unsanitized content was
                // executed on-screen; clean tail = the panic itself is the
                // corruption source.
                super::capture::dump_tail_to_log("before caught render panic");
                app.error_message = Some(format!("render panic{}{}: {}", loc, caller, msg));
                // Try to recover the terminal state for the next frame.
                let _ = terminal.clear();
            }
        }

        let _ = execute!(
            terminal.backend_mut(),
            crossterm::terminal::EndSynchronizedUpdate
        );

        // Check for quit
        if app.should_quit {
            break;
        }

        // Wait for at least one event (with timeout for animation refresh)
        let event =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), app.next_event()).await;

        if let Ok(Some(event)) = event {
            // Disable mouse capture when losing focus so the terminal stops
            // queuing SGR mouse sequences that pile up while unfocused.
            // Re-enable on focus regain and clear any garbage from input.
            match &event {
                TuiEvent::FocusLost => {
                    execute!(terminal.backend_mut(), DisableMouseCapture)?;
                    mouse_capture_applied = false;
                }
                TuiEvent::FocusGained => {
                    // Only re-enable mouse capture if the user hasn't
                    // explicitly turned it off via F12 (selection mode).
                    if app.mouse_capture_enabled {
                        execute!(terminal.backend_mut(), EnableMouseCapture)?;
                        mouse_capture_applied = true;
                    }
                    // Clear any garbage that leaked into the input buffer
                    // while mouse capture was active in another tmux pane.
                    app.clear_escape_garbage();
                }
                _ => {}
            }

            // Intercept MouseScroll events — don't process through state handler.
            // Instead, add to pending_scroll for coalesced processing to avoid
            // double-counting and excessive load_more_history() calls.
            let mut pending_scroll: i32 = 0;
            if let TuiEvent::MouseScroll(dir) = &event {
                pending_scroll += *dir as i32;
            } else if let Err(e) = app.handle_event(event).await {
                app.error_message = Some(e.to_string());
            }

            // Drain all remaining queued events before re-rendering.
            // Coalesce Ticks, Scrolls, and streaming chunks to avoid redundant
            // re-renders. Streaming chunks are batched with a time budget so the
            // TUI stays responsive to keyboard/mouse input during long streams.
            let drain_start = std::time::Instant::now();
            loop {
                match app.try_next_event() {
                    Some(TuiEvent::Tick) => continue,
                    Some(TuiEvent::MouseScroll(dir)) => {
                        pending_scroll += dir as i32;
                    }
                    Some(event) => {
                        let is_chunk = matches!(
                            event,
                            TuiEvent::ResponseChunk { .. } | TuiEvent::ReasoningChunk { .. }
                        );
                        if let Err(e) = app.handle_event(event).await {
                            app.error_message = Some(e.to_string());
                        }
                        // Batch streaming chunks for up to 30ms before yielding
                        // to render. This lets multiple chunks coalesce into one
                        // re-render instead of O(n) renders per second.
                        if is_chunk && drain_start.elapsed() >= std::time::Duration::from_millis(30)
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
            // Apply coalesced scroll as a single operation.
            // Cap to prevent macOS smooth-scroll flicks (20-50 events) from
            // launching the user into orbit. Keep it small for smooth scrolling.
            const MAX_SCROLL_PER_FRAME: i32 = 3;
            let capped = pending_scroll.clamp(-MAX_SCROLL_PER_FRAME, MAX_SCROLL_PER_FRAME);
            if capped > 0 {
                app.scroll_offset = app.scroll_offset.saturating_add(capped as usize);
                app.auto_scroll = false;
                // History loading removed — causes overshoot. User can manually
                // load more history via keyboard shortcut if needed.
            } else if capped < 0 {
                app.scroll_offset = app
                    .scroll_offset
                    .saturating_sub(capped.unsigned_abs() as usize);
                app.auto_scroll = false;
                if app.scroll_offset == 0 {
                    app.auto_scroll = true;
                }
            }
        }
    }

    Ok(())
}
