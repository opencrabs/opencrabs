//! Dispatcher-level mouse-report fragment filter (#1983, #2233).
//!
//! When the escape read splits mid-burst, crossterm re-publishes SGR (1006)
//! and URXVT (1015) mouse reports as individual `Key(Char)` events: the
//! leading `\x1b` is consumed as `KeyCode::Esc` and the body (`[<35;116;77M`)
//! arrives one char at a time. This filter sits at the single dispatch choke
//! point (`App::handle_key_event`) so no mode has to remember to gate.
//!
//! # Why the old heuristic ate real typing
//!
//! The previous version decided from the destination buffer's tail alone, so
//! a bare `[` was treated as the head of a URXVT report and the characters
//! after it were discarded as noise. That is not a rare shape: `arr[0]`,
//! `[0; 4]`, `[u8; 32]`, `x[M` all lost their first element, and because the
//! dropped char was never echoed into the tail, the `[` stayed "open" and ate
//! the whole run. `esc_key_pending` had the same defect in a narrower form: it
//! swallowed a `[` typed right after any Esc.
//!
//! # What decides now
//!
//! Two independent signals, both narrow:
//!
//! 1. **Esc adjacency.** A real burst body is produced by the terminal in the
//!    same write that produced the `\x1b`, so its chars land in the event
//!    queue microseconds after the `KeyCode::Esc` that consumed it. A human
//!    pressing Esc and then typing takes tens of milliseconds. Only a char
//!    arriving inside [`BURST_WINDOW`] of the Esc can open a burst.
//! 2. **The `[<` prefix.** SGR is the mode crossterm itself enables, and `[<`
//!    followed by digits is not ordinary typing. This one needs no Esc, so a
//!    burst whose `\x1b` never surfaced is still caught.
//!
//! A bare `[` on its own now means nothing. `arr[0]` types exactly as written.
//!
//! # Known trade-off
//!
//! A URXVT report (no `<`) whose `\x1b` was dropped entirely by crossterm, so
//! that no `KeyCode::Esc` ever reached the gate, is no longer suppressed on a
//! cold gate: `[35;98;25M` would type through. crossterm delivers that `\x1b`
//! as `KeyCode::Esc` (the #1943 evidence), so the Esc-adjacency rule covers
//! every report this app can actually receive, and the `[<` rule covers the
//! rest. Trading that hypothetical for `arr[0]` typing correctly is the right
//! way round.

use std::time::{Duration, Instant};

use crossterm::event::KeyCode;

/// A burst char must land within this window of the `KeyCode::Esc` that
/// consumed its `\x1b` to be treated as terminal noise. Split reads are
/// delivered back-to-back (sub-millisecond); a human cannot press Esc and
/// another key this fast.
const BURST_WINDOW: Duration = Duration::from_millis(30);

/// Characters that can appear inside an SGR/URXVT report body.
fn is_report_body(c: char) -> bool {
    matches!(c, '<' | '>' | 'M' | 'm' | ';') || c.is_ascii_digit()
}

#[derive(Debug, Default)]
pub(crate) struct MouseFragGate {
    /// When a real `KeyCode::Esc` last passed, so the next char can be
    /// classified by how soon it followed.
    esc_at: Option<Instant>,
    /// The previous char was `[` - the next one decides whether we are in an
    /// SGR report (`[<…`) or typing a bracket expression.
    bracket_pending: bool,
    /// Inside an SGR report body; drop until the `M`/`m` terminator.
    in_sgr: bool,
    /// Inside a burst opened by Esc adjacency.
    in_burst: bool,
}

impl MouseFragGate {
    /// Decide one key code: `true` means the event is a mouse-report fragment
    /// and must not reach any mode handler.
    pub(crate) fn absorb(&mut self, code: &KeyCode) -> bool {
        self.absorb_at(code, Instant::now())
    }

    /// Clock-injected form for tests.
    pub(crate) fn absorb_at(&mut self, code: &KeyCode, now: Instant) -> bool {
        match code {
            KeyCode::Esc => {
                // Esc is a real cancel key: it always reaches the app. It only
                // arms the possibility that the chars right behind it are the
                // rest of a burst whose `\x1b` it consumed.
                self.esc_at = Some(now);
                self.bracket_pending = false;
                self.in_sgr = false;
                self.in_burst = false;
                false
            }
            KeyCode::Char(c) => {
                // Already inside a confirmed SGR report: swallow the body.
                if self.in_sgr {
                    if is_report_body(*c) {
                        if matches!(c, 'M' | 'm') {
                            self.in_sgr = false;
                        }
                        return true;
                    }
                    self.in_sgr = false;
                }

                // Already inside an Esc-adjacent burst.
                if self.in_burst {
                    if is_report_body(*c) {
                        if matches!(c, 'M' | 'm') {
                            self.in_burst = false;
                            self.esc_at = None;
                        }
                        return true;
                    }
                    self.in_burst = false;
                    self.esc_at = None;
                }

                // Esc adjacency: the only window in which a `[` or a bare
                // digit can be the head of a burst rather than typing.
                let adjacent = self
                    .esc_at
                    .take()
                    .is_some_and(|at| now.duration_since(at) < BURST_WINDOW);
                if adjacent && (*c == '[' || is_report_body(*c)) {
                    self.in_burst = true;
                    self.bracket_pending = false;
                    return true;
                }

                // SGR head: `[<`. Unambiguous enough to catch a burst whose
                // `\x1b` never surfaced, and `[<` is not ordinary typing.
                if self.bracket_pending {
                    self.bracket_pending = false;
                    if *c == '<' {
                        self.in_sgr = true;
                        return true;
                    }
                    // Anything else after `[` is the user's own text.
                }

                if *c == '[' {
                    self.bracket_pending = true;
                    return false;
                }

                // Raw ESC and ETX arriving as Chars are never typing: crossterm
                // reports Esc as `KeyCode::Esc` and Ctrl+C as a control event,
                // so these bytes only reach the char path when an escape read
                // was split mid-sequence (#1943).
                matches!(c, '\x1b' | '\u{3}')
            }
            _ => {
                // Any other key (Enter, arrows, F12, …) neither forms nor
                // continues a burst; reset so a stale arm can't eat later
                // typing.
                self.esc_at = None;
                self.bracket_pending = false;
                self.in_sgr = false;
                self.in_burst = false;
                false
            }
        }
    }
}
