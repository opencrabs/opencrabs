//! The hint card on top of the composer (#2279, #3102).
//!
//! It sits flush against the composer, under the conversation: the terminal
//! gives it the row directly above its input box, and the desktop app stacks a
//! thin strip on top of the composer card. Both read these functions, so the
//! two surfaces cannot recommend different things about the same conversation,
//! which is the drift the shared command table already exists to prevent.
//!
//! The card has an intent. It names what the screen is asking for at this
//! moment, one line, and nothing else: the rail holding the keyboard, an offer
//! waiting above the field, a queue on its way out, or a rung of the ladder. It
//! does not rotate. A line that changes on a timer reads as noise, because the
//! reader cannot tell a new fact from the same fact coming round again.
//!
//! The keys that act on the field itself are not here. Esc stops the turn and
//! the chord records a message, and both are painted inside the field where the
//! caret is, so repeating them on a card directly above it would be the same
//! sentence twice. The card speaks about the work; the field speaks about
//! itself.
//!
//! Nothing here knows what a terminal or a window is. A surface passes the
//! facts it already holds, and filters the answer through the commands it can
//! actually run: a hint naming a command that surface does not answer is worse
//! than no hint at all, because it teaches a habit that bounces.

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
    /// Messages waiting on the running turn.
    ///
    /// A queue changes what the keys do: while one is waiting, Up takes the
    /// message back instead of the offer, and once the turn is over the queue
    /// is what sends next. Zero means nothing is waiting.
    pub queued: usize,
    /// The session list holds the keyboard.
    ///
    /// While it does, the composer is not where a typed character lands: the
    /// character filters the list and Enter opens the row it stands on, so the
    /// line names those keys instead of the field's own (#3102).
    pub rail_focused: bool,
    /// The list is open to choose the session to split with, not to browse.
    ///
    /// The same keys, a different promise: Enter splits the layout rather than
    /// opening the conversation, and Esc answers the dialog's N. One word apart
    /// is the difference between a shortcut and a surprise (#3102).
    pub rail_split: bool,
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

    // Two ways out, because they cost different things: `/new` starts a fresh
    // session and takes the conversation with it, which is what a new use case
    // wants, while `/clear` only drops the context and leaves the history and
    // the title in place. Naming one of them is how the cheaper option stays a
    // secret (#3113).
    if state.turns >= LONG_SESSION_TURNS && percent < CONTEXT_ROOMY {
        hints.push(Hint {
            text: "/new for a fresh start or new use case · /clear wipes context, keeps history"
                .to_string(),
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

/// The task-related line the card above the composer paints right now (#3102).
///
/// The card has an intent: it names what the screen is asking for at this
/// moment, and nothing else. The keys that act on the field itself, Esc
/// stopping the turn and the chord that records a message, are painted inside
/// the field where the caret is, so this line only speaks when the work has
/// earned something.
///
/// One line, not a rotation. The card used to cycle through everything true on
/// a timer, and a line that changes on its own reads as noise: the reader
/// cannot tell a new fact from the same fact coming round again. It changes
/// when the situation changes, which is what makes it worth reading at all.
pub fn task_line(state: &HintState, supported: impl Fn(&str) -> bool) -> Option<String> {
    // The rail outranks everything, because while it holds the keyboard the
    // composer receives nothing at all and the arrows walk the list, not the
    // field: naming a command here would teach a habit that bounces.
    if state.rail_split {
        return Some("type to find · enter splits · esc cancels".to_string());
    }
    if state.rail_focused {
        return Some("type to filter · enter opens · esc back".to_string());
    }
    // An offer waiting is the most actionable thing on screen, and the arrows
    // reach it instead of the composer history, so the card has to say so or
    // the keys stay a secret (#3101).
    if state.offers > 0 && !state.processing {
        return Some(if state.offers == 1 {
            "↑ picks the suggested option · enter sends".to_string()
        } else {
            format!("↑↓ walk the {} suggested options · enter sends", state.offers)
        });
    }
    // A queue is the only rung that is also true while a turn runs.
    if state.queued > 0 {
        return Some(if state.processing {
            format!("{} queued · sent when the turn ends", state.queued)
        } else {
            format!("{} queued · sending now", state.queued)
        });
    }
    // Nothing is suggested while the answer is still being written: the reader
    // is waiting, not choosing, and the field already says Esc stops it.
    hint(state, supported).map(|hint| hint.text)
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
