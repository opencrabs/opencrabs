//! Tests for `MouseFragGate`, the dispatcher-level mouse-report fragment
//! filter (#1983, #2233).
//!
//! A split escape read makes crossterm republish an SGR/URXVT mouse report as
//! individual `Key(Char)` events: the `\x1b` is consumed as `KeyCode::Esc` and
//! the body arrives one char at a time. The gate sits at the single key
//! dispatch choke point so onboarding fields, modal dialogs, mission control,
//! the sudo prompt and the session-rename buffer are all protected.
//!
//! Two properties matter equally here, and the second one is the regression
//! that #2233 fixed:
//!
//! 1. Burst bytes must never reach a handler (they used to flood buffers and
//!    silently pick onboarding options).
//! 2. Ordinary typing must never be swallowed - `arr[0]`, `[0; 4]`, `[u8; 32]`
//!    and `x[M` all used to lose their first element because a bare `[` was
//!    read as the head of a URXVT report.

use std::time::{Duration, Instant};

use crate::tui::app::mouse_frag::MouseFragGate;
use crossterm::event::KeyCode;

/// A burst char lands microseconds after the `Esc` that consumed its `\x1b`;
/// a human needs tens of milliseconds. Every test states which side it is on.
const BURST_GAP: Duration = Duration::from_millis(1);
const HUMAN_GAP: Duration = Duration::from_millis(200);

fn t0() -> Instant {
    Instant::now()
}

/// Type `text` one char at a time, all at `at`; return what survived.
fn typed_at(gate: &mut MouseFragGate, text: &str, at: Instant) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if !gate.absorb_at(&KeyCode::Char(c), at) {
            out.push(c);
        }
    }
    out
}

// ── Regression: a bare `[` is not a mouse report (#2233) ──

#[test]
fn bracket_index_typing_survives() {
    // `arr[0]` lost the `0` before #2233: the `[` armed the URXVT branch and
    // the digit was eaten as a `cb` byte.
    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "arr[0]", t0()), "arr[0]");
}

#[test]
fn rust_array_repeat_survives() {
    // `[0; 4]` and `[u8; 32]` are everyday Rust and both lost characters.
    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "[0; 4]", t0()), "[0; 4]");

    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "[u8; 32]", t0()), "[u8; 32]");
}

#[test]
fn lowercase_m_after_bracket_survives() {
    // `M`/`m` are in the report alphabet, so `x[M` and `x[m` lost a letter.
    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "x[M", t0()), "x[M");

    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "x[m", t0()), "x[m");
}

#[test]
fn markdown_link_and_prose_survive() {
    let mut gate = MouseFragGate::default();
    let text = "see [text](url) and [hello]";
    assert_eq!(typed_at(&mut gate, text, t0()), text);
}

#[test]
fn bracket_then_semicolon_survives() {
    // `;` right after `[` used to be treated as a URXVT separator.
    let mut gate = MouseFragGate::default();
    assert_eq!(typed_at(&mut gate, "a[;", t0()), "a[;");
}

#[test]
fn multibyte_typing_survives() {
    // The old filter sliced a 30-byte tail and had to snap back to a char
    // boundary to avoid panicking mid-codepoint. The gate classifies single
    // chars and never slices the buffer, so non-ASCII text passes untouched.
    let mut gate = MouseFragGate::default();
    let text = "🦀 café [0]";
    assert_eq!(typed_at(&mut gate, text, t0()), text);
}

// ── Burst suppression still holds ──

#[test]
fn sgr_burst_after_its_esc_is_dropped_with_no_residue() {
    // The real split-read shape: `\x1b` surfaces as KeyCode::Esc, the body
    // arrives immediately after. Nothing at all may reach a handler.
    let mut gate = MouseFragGate::default();
    let start = t0();
    assert!(
        !gate.absorb_at(&KeyCode::Esc, start),
        "Esc must stay a cancel key"
    );
    let body = typed_at(&mut gate, "[<35;98;25M", start + BURST_GAP);
    assert_eq!(body, "", "burst body leaked: {body:?}");
}

#[test]
fn urxvt_burst_after_its_esc_is_dropped_with_no_residue() {
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    let body = typed_at(&mut gate, "[35;98;25M", start + BURST_GAP);
    assert_eq!(body, "", "URXVT body leaked: {body:?}");
}

#[test]
fn cold_sgr_burst_is_still_caught_by_the_bracket_lt_prefix() {
    // Belt and braces: a burst whose `\x1b` never surfaced at all. The `[<`
    // prefix is enough on its own; only the bracket survives, as before.
    let mut gate = MouseFragGate::default();
    let body = typed_at(&mut gate, "[<35;98;25M", t0());
    assert_eq!(body, "[", "only the bracket may survive: {body:?}");
}

#[test]
fn burst_digits_never_reach_a_handler() {
    // The wizard-selection hazard: digits at onboarding/input.rs pick options,
    // so a burst must deliver ZERO digits to any handler.
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    let leaked = typed_at(&mut gate, "[<35;98;25M[<35;98;25M", start + BURST_GAP);
    assert!(
        !leaked.chars().any(|c| c.is_ascii_digit()),
        "burst digits leaked: {leaked:?}"
    );
}

#[test]
fn repeated_bursts_do_not_accumulate_garbage() {
    // Twenty consecutive motion reports (the screenshot showed dozens), each
    // with its own consumed Esc.
    let mut gate = MouseFragGate::default();
    let start = t0();
    let mut leaked = String::new();
    for _ in 0..20 {
        gate.absorb_at(&KeyCode::Esc, start);
        leaked.push_str(&typed_at(&mut gate, "[<35;98;25M", start + BURST_GAP));
    }
    assert_eq!(leaked, "", "burst residue accumulated: {leaked:?}");
}

// ── Esc must not eat the next real keystroke (#2233) ──

#[test]
fn bracket_typed_after_esc_survives() {
    // `esc_key_pending` used to swallow a `[` typed after any Esc.
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    let typed = typed_at(&mut gate, "[hello]", start + HUMAN_GAP);
    assert_eq!(typed, "[hello]", "a human bracket must not be eaten");
}

#[test]
fn typing_after_esc_survives_at_human_speed() {
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    assert_eq!(
        typed_at(&mut gate, "arr[0] = 1", start + HUMAN_GAP),
        "arr[0] = 1"
    );
}

#[test]
fn non_char_key_clears_the_esc_arm() {
    // Enter/arrows break adjacency, so a later `[3` is typing, not a burst.
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    assert!(!gate.absorb_at(&KeyCode::Enter, start + BURST_GAP));
    assert_eq!(
        typed_at(&mut gate, "[3;4]", start + BURST_GAP),
        "[3;4]",
        "a cleared arm must not suppress anything"
    );
}

#[test]
fn state_is_released_after_a_burst() {
    let mut gate = MouseFragGate::default();
    let start = t0();
    gate.absorb_at(&KeyCode::Esc, start);
    typed_at(&mut gate, "[<35;98;25M", start + BURST_GAP);
    // Typing right after a finished burst is ordinary input.
    assert_eq!(typed_at(&mut gate, "ok[1]", start + BURST_GAP), "ok[1]");
}

// ── Raw control bytes arriving as Chars ──

#[test]
fn raw_esc_and_etx_chars_are_dropped() {
    // crossterm reports Esc as KeyCode::Esc and Ctrl+C as a control event, so
    // these bytes only reach the char path when an escape read split (#1943).
    let mut gate = MouseFragGate::default();
    let start = t0();
    assert!(gate.absorb_at(&KeyCode::Char('\x1b'), start));
    assert!(gate.absorb_at(&KeyCode::Char('\u{3}'), start));
}

// ── Dispatch-level feed: burst ahead of a real mode handler ──

use crate::tui::onboarding::{BrainField, OnboardingStep, OnboardingWizard};
use crossterm::event::{KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn brain_wizard() -> OnboardingWizard {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::BrainSetup;
    w.brain_field = BrainField::AboutMe;
    // `new()` seeds `about_me` from any existing USER.md (the wizard opens on
    // the Review screen with the profile prefilled). Clear it so the
    // burst-feed assertions measure exactly what the feed typed.
    w.about_me.clear();
    w
}

#[test]
fn dispatch_level_feed_keeps_onboarding_field_free_of_burst_bytes() {
    // Model the dispatcher exactly: every key goes through `absorb_at` first;
    // only keys that pass reach the mode's own `handle_key`.
    let mut gate = MouseFragGate::default();
    let mut w = brain_wizard();
    let start = t0();
    for _ in 0..3 {
        gate.absorb_at(&KeyCode::Esc, start);
        for c in "[<35;98;25M".chars() {
            if gate.absorb_at(&KeyCode::Char(c), start + BURST_GAP) {
                continue;
            }
            w.handle_key(key(KeyCode::Char(c)));
        }
    }
    assert!(
        w.about_me.is_empty(),
        "not a single burst byte may reach the field: {:?}",
        w.about_me
    );
    assert!(
        matches!(w.step, OnboardingStep::BrainSetup),
        "burst must never navigate the wizard"
    );
}

#[test]
fn dispatch_level_feed_lets_real_typing_through() {
    // The mirror of the test above: the same wiring must deliver genuine
    // keystrokes, brackets included, into the field.
    let mut gate = MouseFragGate::default();
    let mut w = brain_wizard();
    let start = t0();
    for c in "let v = arr[0];".chars() {
        if gate.absorb_at(&KeyCode::Char(c), start + HUMAN_GAP) {
            continue;
        }
        w.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(w.about_me, "let v = arr[0];");
}
