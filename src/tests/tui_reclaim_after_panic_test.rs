//! #2238: a caught render panic must not leave the terminal in canonical mode.
//!
//! The panic hook calls `force_restore_terminal()` for every panic, including
//! the ones `catch_unwind` then swallows. When the render loop survives a
//! panic it keeps drawing, so it has to take the terminal back: raw mode,
//! alternate screen, capture modes. It used to call `terminal.clear()` and
//! nothing else, which is why a caught render panic left keystrokes echoing
//! into the status bar row and only Enter reaching the app.
//!
//! These are source-level sentinels, not behavioural tests, and deliberately
//! so: `reclaim_terminal` needs a real tty (`enable_raw_mode` on a captured
//! stdout fails), so it cannot be executed under `cargo test`. What can be
//! pinned is the wiring: that the recovery path calls the reclaim at all, that
//! the editor handoff and the panic recovery share one body, and that the
//! panic hook still restores for a genuine unwind. The byte-level restore
//! sequence itself is already pinned by `tui_restore_test`.

const RUNNER_SRC: &str = include_str!("../tui/runner.rs");

/// `src[start..]`, with the char-boundary check made explicit. Every offset
/// here comes from a `find`, so a miss is a real bug worth a panic, but going
/// through `get` keeps this file off the `clippy::string_slice` ratchet (#2221).
fn tail(src: &str, start: usize) -> &str {
    src.get(start..)
        .unwrap_or_else(|| panic!("slice from {start} is not a char boundary"))
}

/// `src[start..end]`, same contract as [`tail`].
fn window(src: &str, start: usize, end: usize) -> &str {
    src.get(start..end)
        .unwrap_or_else(|| panic!("slice {start}..{end} is not a char boundary"))
}

/// The source between the first `from` marker and the next `to` marker, so a
/// renamed marker fails loudly instead of quietly matching nothing.
fn section<'a>(src: &'a str, from: &str, to: &str, what: &str) -> &'a str {
    let start = src
        .find(from)
        .unwrap_or_else(|| panic!("{what} is gone; re-point this test"));
    let end = tail(src, start)
        .find(to)
        .map(|i| start + i)
        .unwrap_or(src.len());
    window(src, start, end)
}

/// Slice out the body of `fn <name>` by matching braces from its opening `{`.
/// Panics if the function is not there, so a rename fails loudly instead of
/// quietly matching nothing.
fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let needle = format!("fn {name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("`fn {name}` not found in src/tui/runner.rs"));
    let open = tail(src, start)
        .find('{')
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("`fn {name}` has no body"));
    let mut depth = 0usize;
    for (offset, ch) in tail(src, open).char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return window(src, open, open + offset + 1);
                }
            }
            _ => {}
        }
    }
    panic!("`fn {name}` body is unbalanced");
}

#[test]
fn the_caught_panic_arm_reclaims_the_terminal_instead_of_only_clearing_it() {
    let arm = section(
        RUNNER_SRC,
        "Err(panic_payload) =>",
        "Ok(Err(e)) => return Err(e.into())",
        "the caught render-panic arm",
    );

    assert!(
        arm.contains("reclaim_terminal(terminal)"),
        "the caught-panic arm must reclaim the terminal (#2238); it survived the \
         panic but left raw mode off, so keystrokes echo into the status bar"
    );
    assert!(
        !arm.contains("let _ = terminal.clear();"),
        "the caught-panic arm still clears instead of reclaiming (#2238); \
         clear() cannot turn raw mode back on"
    );
}

#[test]
fn the_reclaim_sequence_re_enters_every_mode_the_panic_hook_dropped() {
    let body = fn_body(RUNNER_SRC, "reclaim_terminal");

    // `force_restore_terminal` turns each of these off. Raw mode and the
    // alternate screen are the two that produce the reported symptoms; the
    // capture modes come back too so the session is not subtly degraded.
    for required in [
        "enable_raw_mode()",
        "EnterAlternateScreen",
        "EnableBracketedPaste",
        "EnableFocusChange",
        "EnableMouseCapture",
    ] {
        assert!(
            body.contains(required),
            "`reclaim_terminal` must re-establish `{required}`; the panic hook \
             dropped it and the loop keeps running (#2238)"
        );
    }
}

#[test]
fn the_editor_handoff_and_the_panic_recovery_share_one_reclaim_body() {
    let reinit = fn_body(RUNNER_SRC, "reinit_terminal_for_editor");

    assert!(
        reinit.contains("reclaim_terminal(terminal)"),
        "the editor handoff must go through the shared reclaim (#2238); two \
         copies of this sequence drift and one of them ends up stale"
    );
    assert!(
        !reinit.contains("enable_raw_mode()"),
        "the editor handoff still has its own inline setup (#2238); it must \
         call `reclaim_terminal` so both recovery paths stay identical"
    );
}

#[test]
fn the_panic_hook_still_hands_the_terminal_back_for_a_real_unwind() {
    let hook = section(
        RUNNER_SRC,
        "std::panic::set_hook(Box::new(move |info| {",
        "// SIGINT (Ctrl+C) handler",
        "the panic hook",
    );

    let restore = hook
        .find("force_restore_terminal();")
        .expect("the panic hook must restore the terminal before printing (#2238)");
    let print = hook
        .find("default_hook(info);")
        .expect("the panic hook must still delegate to the default hook");
    assert!(
        restore < print,
        "the terminal must be restored BEFORE the panic text is printed, or the \
         message lands in raw mode with no cursor"
    );
}

#[test]
fn the_panic_arm_resyncs_the_mouse_capture_state_it_just_forced_on() {
    let arm = section(
        RUNNER_SRC,
        "Err(panic_payload) =>",
        "Ok(Err(e)) => return Err(e.into())",
        "the caught render-panic arm",
    );

    assert!(
        arm.contains("mouse_capture_applied = true;"),
        "the reclaim re-enables mouse capture unconditionally, so the applied \
         state has to say so; otherwise the loop's sync block believes capture \
         is still off and never restores an F12 toggle (#2238)"
    );
}

#[test]
fn the_reclaim_is_reachable_from_the_module_that_owns_the_loop() {
    // Compile-time pin: the signature must stay `&mut TuiTerminal -> Result<()>`
    // so the panic arm can hand it the terminal it already holds.
    let _: fn(&mut crate::tui::runner::TuiTerminal) -> anyhow::Result<()> =
        crate::tui::runner::reclaim_terminal;
}

#[test]
fn every_release_arms_the_reclaim_flag_and_every_reclaim_clears_it() {
    let restore = fn_body(RUNNER_SRC, "force_restore_terminal");
    assert!(
        restore.contains("TERMINAL_RELEASED.store(true"),
        "`force_restore_terminal` must arm TERMINAL_RELEASED (#2238); it is the \
         single choke point every release goes through, including the \
         process-wide panic hook firing for a caught panic or a spawned task"
    );

    let reclaim = fn_body(RUNNER_SRC, "reclaim_terminal");
    assert!(
        reclaim.contains("TERMINAL_RELEASED.store(false"),
        "`reclaim_terminal` must clear TERMINAL_RELEASED (#2238), or the loop \
         reclaims on every single frame forever"
    );
}

#[test]
fn the_event_loop_reclaims_a_terminal_released_under_it() {
    let loop_start = RUNNER_SRC
        .find("    loop {\n        // Check SIGINT flag")
        .expect("the event loop is gone; re-point this test");
    let loop_head = window(
        RUNNER_SRC,
        loop_start,
        (loop_start + 2000).min(RUNNER_SRC.len()),
    );

    assert!(
        loop_head.contains("TERMINAL_RELEASED.load(Ordering::SeqCst)"),
        "the event loop must check TERMINAL_RELEASED every iteration (#2238); \
         the panic hook restores the terminal for panics that do not end the \
         process, and nothing else would ever notice"
    );
    assert!(
        loop_head.contains("reclaim_terminal(terminal)"),
        "the loop must reclaim the terminal when it finds it released (#2238)"
    );
    assert!(
        loop_head.contains("mouse_capture_applied = true"),
        "the reclaim re-enables mouse capture, so the loop's applied state must \
         follow (#2238)"
    );
}

#[test]
fn startup_disarms_the_flag_it_armed_for_the_predecessor_cleanup() {
    // `run()` calls `force_restore_terminal()` before taking over (#1964),
    // which arms the flag. If that is not disarmed, the first loop iteration
    // reclaims a terminal it already owns.
    let disarm = RUNNER_SRC
        .find("TERMINAL_RELEASED.store(false, Ordering::SeqCst);\n\n    // Fast sync init")
        .or_else(|| RUNNER_SRC.find("disarm it before the loop starts checking"));
    assert!(
        disarm.is_some(),
        "`run()` must disarm TERMINAL_RELEASED after its own terminal setup (#2238)"
    );
}
