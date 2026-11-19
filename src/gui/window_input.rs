//! The window's input edge: gpui keystrokes into the neutral
//! vocabulary, zoom steps, ctrl+c's three answers, and the wheel's line step.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// Translate a gpui keystroke into the neutral vocabulary the decision logic
/// speaks. Key names here are the macOS platform layer's spellings
/// (`platform/mac/events.rs`): `escape`, `tab` (shift+tab arrives as `tab`
/// with the shift modifier), `enter`. A printable `key_char` becomes
/// `UiKey::Char` unless a command-ish modifier is held, and everything
/// unmodelled falls to `UiKey::Other`, which `decide` always ignores.
pub(crate) fn translate_key(ev: &KeyDownEvent) -> UiKey {
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
pub(crate) fn zoom_step(key: &str, rem: f32) -> Option<f32> {
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
pub(crate) fn zoom_step_for(key: &str, rem: f32, chord: bool) -> Option<f32> {
    if !chord {
        return None;
    }
    zoom_step(key, rem)
}

/// How long the Seeker caret keeps blinking after the last keystroke. Past
/// this the field goes quiet and the caret hides, the way Spotlight's does
/// once typing stops; the next key brings it back (#2531). The window and the
/// terminal share one rule, so this aliases the engine's constant.
pub(crate) const CARET_IDLE_AFTER: std::time::Duration =
    opencrabs::ui_core::palette::CARET_IDLE_AFTER;

/// How tall one wheel "line" is when the platform reports the tick in lines
/// instead of pixels, the same 20px GPUI's own list uses (#2698).
pub(crate) const WHEEL_LINE_PX: f32 = 20.0;

/// How long a first Ctrl+C stays armed before the second press stops meaning
/// "quit": the terminal's own three seconds (`tui/app/state.rs:3730`).
pub(crate) const QUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// What one Ctrl+C press spends itself on (#2308).
///
/// The terminal has answered this key the same way for years, so the window
/// asks the same question in the same order instead of inventing a second
/// meaning for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CtrlCAction {
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
pub(crate) fn ctrl_c_action(
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
pub(crate) fn wheel_dy(delta: &ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(d) => f32::from(d.y),
        ScrollDelta::Lines(d) => d.y * WHEEL_LINE_PX,
    }
}
