//! The one line under the composer that says what to do next (#2279).
//!
//! The terminal draws it between its input box and its status bar; the desktop
//! app draws it between the composer and the footer. Both read this function,
//! so the two surfaces cannot recommend different things about the same
//! conversation, which is the drift the shared command table already exists to
//! prevent.
//!
//! Nothing here knows what a terminal or a window is. A surface passes the
//! facts it already holds, and filters the answer through the commands it can
//! actually run: a hint naming a command that surface does not answer is worse
//! than no hint at all, because it teaches a habit that bounces.
//!
//! The row is reserved whether or not it has something to say. A row that
//! appears and disappears moves everything above it, and the transcript is
//! exactly the thing nobody wants moving under their eyes while they read.

use crate::ui_core::commands::{Support, support_gui};

/// A context this full is worth saying out loud.
///
/// The same line compaction enforces, because that is where the cost lands: the
/// reader loses the conversation either way, and the only question is whether
/// they chose the moment.
pub const CONTEXT_TIGHT: f64 = 80.0;

/// Below this much context a long conversation still has room to keep going,
/// which is when a clean slate is worth mentioning as an option rather than a
/// warning.
pub const CONTEXT_ROOMY: f64 = 40.0;

/// Exchanges after which a fresh session is worth mentioning.
///
/// A dozen turns is the point where the ledger is long enough to scroll and the
/// context is usually past a third: long enough that the reader is working, not
/// just starting.
pub const LONG_SESSION_TURNS: usize = 12;

/// What a surface knows about the conversation when it draws the row.
///
/// Every field is something both frontends already track for their own chrome,
/// so nothing here is state kept only for the hint. `Default` is the state
/// before the first answer, which produces no hint at all.
#[derive(Clone, Debug, Default)]
pub struct HintState {
    /// A turn is running. Nothing is suggested while the answer is still being
    /// written: the reader is waiting, not choosing.
    pub processing: bool,
    /// Prompt tokens the provider last reported for this conversation.
    pub context_used: u64,
    /// The window those tokens are measured against. Zero means unknown, which
    /// reads as "nothing to warn about".
    pub context_max: u64,
    /// The tokens a `/clear` leaves behind: the system prompt and tool schema.
    pub clear_base: u64,
    /// A newer release is waiting, when the surface has checked.
    pub update_version: Option<String>,
    /// Exchanges in the conversation on screen.
    pub turns: usize,
    /// Follow-up options waiting above the composer.
    ///
    /// A count rather than a flag, because the row says what the arrows do and
    /// the reader needs to know there is more than one to walk. Zero means the
    /// turn offered nothing, which is the common case.
    pub offers: usize,
}

impl HintState {
    /// How full the context is, as a percentage.
    pub fn context_percent(&self) -> f64 {
        if self.context_max == 0 {
            return 0.0;
        }
        (self.context_used as f64 / self.context_max as f64) * 100.0
    }

    /// The tokens a `/clear` would free.
    ///
    /// Saturating: a base larger than what is in use means the two numbers came
    /// from different moments, and a negative "frees -3k tokens" would be worse
    /// than saying nothing.
    pub fn clearable(&self) -> u64 {
        self.context_used.saturating_sub(self.clear_base)
    }
}

/// One suggestion: the line to print, and the command it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub text: String,
    /// The command the text points at, so a surface can drop a hint it cannot
    /// honour instead of printing a name that bounces.
    pub command: &'static str,
}

/// Every hint this conversation has earned, most important first.
///
/// Split from [`hint`] so a test can assert the whole ladder rather than only
/// the winner, which is how a rule that never fires goes unnoticed.
pub fn candidates(state: &HintState) -> Vec<Hint> {
    let mut hints = Vec::new();
    let percent = state.context_percent();

    // First, because it is the only rung about something already on screen and
    // one keystroke away: an offer the turn just made, waiting to be sent. No
    // command to point at, so the text carries its own keys.
    if state.offers > 0 {
        hints.push(Hint {
            text: if state.offers == 1 {
                "↑ picks the option · enter sends".to_string()
            } else {
                "↑↓ pick an option · enter sends".to_string()
            },
            command: "",
        });
    }

    if percent >= CONTEXT_TIGHT {
        let freed = compact_tokens(state.clearable());
        hints.push(Hint {
            text: format!("new task? /clear frees {freed} tokens"),
            command: "/clear",
        });
    }

    if let Some(version) = state.update_version.as_deref().filter(|v| !v.is_empty()) {
        hints.push(Hint {
            text: format!("v{version} is out · /evolve"),
            command: "/evolve",
        });
    }

    if state.turns >= LONG_SESSION_TURNS && percent < CONTEXT_ROOMY {
        hints.push(Hint {
            text: "keep going, or /new for a clean slate".to_string(),
            command: "/new",
        });
    }

    hints
}

/// The line to draw, or `None` for a reserved but empty row.
///
/// `supported` decides whether a command runs on the surface asking, so the
/// first hint it cannot honour gives way to the next one rather than blanking
/// the row.
pub fn hint(state: &HintState, supported: impl Fn(&str) -> bool) -> Option<Hint> {
    if state.processing {
        return None;
    }
    candidates(state)
        .into_iter()
        // A hint with no command names keys instead, and every surface has
        // keys, so there is nothing to filter it against.
        .find(|hint| hint.command.is_empty() || supported(hint.command))
}

/// The same line for the desktop app, which answers the commands in its own
/// support list (#2878).
pub fn gui_hint(state: &HintState) -> Option<Hint> {
    hint(state, |command| support_gui(command) == Support::Runs)
}

/// A token count the way a person reads it: `840`, `104.4k`, `1.2M`.
///
/// One decimal is enough to feel the size without pretending to a precision the
/// provider's own count does not have.
pub fn compact_tokens(tokens: u64) -> String {
    const THOUSAND: u64 = 1_000;
    const MILLION: u64 = 1_000_000;
    if tokens < THOUSAND {
        return tokens.to_string();
    }
    if tokens < MILLION {
        return format!("{:.1}k", tokens as f64 / THOUSAND as f64);
    }
    format!("{:.1}M", tokens as f64 / MILLION as f64)
}
