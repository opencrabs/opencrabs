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

/// Where the compaction tip starts talking.
///
/// Below [`CONTEXT_TIGHT`] on purpose. The tight rung tells the reader to
/// `/clear`, and by then the automatic compaction has already begun: a line
/// that explains the machine AFTER it acted reads as a surprise, not a tip. At
/// this mark the conversation is long enough to be worth explaining and still
/// short of the soft trigger, which is the only window where the explanation
/// arrives before the event.
pub const COMPACTION_TIP_AT: f64 = 55.0;

/// Exchanges the Seeker introduction is still worth making.
///
/// The first few, because it is an introduction: a reader three turns in has a
/// screen full of unfamiliar machinery and no idea the search exists, while a
/// reader thirty turns in has either found it or built a workflow without it.
/// Repeating it later would be nagging, and the dismissal store is for the tips
/// that keep earning their line.
pub const SEEKER_TIP_TURNS: usize = 3;

/// Exchanges after which the per-session isolation tip is worth a line.
///
/// The same dozen as [`LONG_SESSION_TURNS`]: a reader with that much history in
/// one conversation is the reader who would benefit from splitting the next
/// use case out of it.
pub const ISOLATION_TIP_TURNS: usize = LONG_SESSION_TURNS;

/// Exchanges after which `/usage` is worth pointing at.
pub const USAGE_TIP_TURNS: usize = 20;

/// Exchanges after which asking for browser automation is worth a line.
///
/// The latest rung, because it asks the reader to hand over a job rather than
/// press a key: only a conversation that has been running this long has shown
/// the kind of repeated manual work the offer is about.
pub const BROWSER_TIP_TURNS: usize = 30;

/// Exchanges a tip stays quiet after the reader dismisses it.
///
/// Seven days, not forever. A permanent dismissal chosen before the feature has
/// had a chance to be useful is a lost reader: the point of the tips is to be
/// discovered, and a week is long enough that the line is not nagging and short
/// enough that it comes back to someone who never read it.
pub const DISMISS_DAYS: i64 = 7;

/// Whether a line can be dismissed at all.
///
/// Two classes share the card and only one of them is advice. A functional line
/// names what the keys do RIGHT NOW (the rail holding the keyboard, an offer
/// waiting, a queue on its way out) or states a fact about the machine (the
/// context is nearly full, a release is waiting). Hiding any of those for a
/// week would leave the reader guessing about a screen that is behaving in a
/// way they cannot see the reason for, so they stay.
///
/// The allowlist is inverted: a line is dismissible unless it is named here, so
/// a new tip is dismissible by default instead of silently joining the class
/// that can never be hidden.
pub fn dismissible(id: &str) -> bool {
    !matches!(id, "offers" | "queue" | "rail" | "rail-split" | "context" | "update")
}

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
    /// Hint ids the reader has dismissed, while still inside their window.
    ///
    /// Ids, not lines. The text is exactly the part that gets reworded, so a
    /// store keyed by the sentence would let a dismissed tip come back the
    /// moment a word changed, which is the failure mode a dismissal is supposed
    /// to end.
    pub dismissed: Vec<String>,
    /// A speech engine is configured, so `/whisper` has something to run.
    ///
    /// Read from the live config rather than assumed: a tip pointing at a
    /// command with no engine behind it is the same bounce as a command the
    /// surface does not answer.
    pub voice: bool,
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

/// One suggestion: what it is, the line to print, and the command it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// A stable name for this suggestion, never shown.
    ///
    /// The dismissal store keys on this rather than on [`text`](Self::text),
    /// because the copy is what gets reworded: a store keyed by the sentence
    /// would resurrect a dismissed tip the first time a word changed, and the
    /// reader who dismissed it would have no way to tell that from a bug.
    pub id: &'static str,
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
            id: "offers",
            text: if state.offers == 1 {
                "↑ picks the option · enter sends · ↓ or esc back to the field · stop overthinking, pick one"
                    .to_string()
            } else {
                format!(
                    "↑↓ walk the {} options · enter sends · ↓ past the last or esc back to the field · stop overthinking, pick one",
                    state.offers
                )
            },
            command: "",
        });
    }

    if percent >= CONTEXT_TIGHT {
        let freed = compact_tokens(state.clearable());
        hints.push(Hint {
            id: "context",
            text: format!("new task? /clear frees {freed} tokens · cheaper than watching this thing choke"),
            command: "/clear",
        });
    }

    if let Some(version) = state.update_version.as_deref().filter(|v| !v.is_empty()) {
        hints.push(Hint {
            id: "update",
            text: format!("v{version} is out · /evolve · new version, same crabs"),
            command: "/evolve",
        });
    }

    // The explanation, while it can still arrive before the event. The tight
    // rung above tells the reader to clear, and the machine has usually already
    // started compacting by then: saying "this happens by itself" only after it
    // happened reads as a surprise rather than a tip, which is why this rung
    // sits BELOW the warning instead of replacing it.
    if (COMPACTION_TIP_AT..CONTEXT_TIGHT).contains(&percent) {
        hints.push(Hint {
            id: "compaction",
            text: "pro tip: I compact on my own, no babysitting needed · /clear keeps history, /new starts fresh"
                .to_string(),
            command: "/clear",
        });
    }

    // Two ways out, because they cost different things: `/new` starts a fresh
    // session and takes the conversation with it, which is what a new use case
    // wants, while `/clear` only drops the context and leaves the history and
    // the title in place. Naming one of them is how the cheaper option stays a
    // secret (#3113).
    if state.turns >= LONG_SESSION_TURNS && percent < CONTEXT_ROOMY {
        hints.push(Hint {
            id: "fresh-start",
            text: "/new for a fresh start or new use case · /clear wipes context, keeps history · both beat scrolling into the void"
                .to_string(),
            command: "/new",
        });
    }

    // The friendly rungs. They exist to be discovered rather than to warn, so
    // they are ordered by how early they are useful and each one only comes up
    // once the reader has had the chance to act on the one before it: a
    // dismissal lets the next through, and nothing ever rotates on a timer.
    // From the first exchange, not from an empty session: there is nothing to
    // search yet on a screen with no messages, and a greeting card over an
    // empty transcript is chrome, not a hint.
    if (1..=SEEKER_TIP_TURNS).contains(&state.turns) {
        hints.push(Hint {
            id: "seeker",
            text: "meet the Seeker · alt+space finds anything, so you can stop pretending you knew where it was"
                .to_string(),
            command: "",
        });
    }

    if state.turns >= ISOLATION_TIP_TURNS {
        hints.push(Hint {
            id: "isolated-sessions",
            text: "interesting fact: every session is its own agent · one for frontend, one for backend"
                .to_string(),
            command: "",
        });
    }

    if state.turns >= USAGE_TIP_TURNS {
        hints.push(Hint {
            id: "usage",
            text: "/usage shows where the tokens went · spoiler: it was that four hour debugging session"
                .to_string(),
            command: "/usage",
        });
    }

    // Only when there is an engine behind the command. `/whisper` is answered by
    // both surfaces, so the tip is about the config, not about the frontend.
    if state.voice {
        hints.push(Hint {
            id: "whisper",
            text: "/whisper lets you talk instead of type · your wrists will send a thank you card"
                .to_string(),
            command: "/whisper",
        });
    }

    if state.turns >= BROWSER_TIP_TURNS {
        hints.push(Hint {
            id: "browser-automation",
            text: "ask me to set up browser automation · social media, daily briefs, whatever you keep doing by hand"
                .to_string(),
            command: "",
        });
    }

    // A dismissal silences one line for its window, not the card: dropping the
    // dismissed id here is what lets the next rung through, which is the whole
    // reason the store exists. Functional lines are exempt, so a stale row in
    // the store can never hide what the keys are doing.
    hints.retain(|hint| {
        !dismissible(hint.id) || !state.dismissed.iter().any(|id| id == hint.id)
    });
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
    task_line_hint(state, supported).map(|hint| hint.text)
}

/// The same line, with its id, for a surface that can act on it.
///
/// A dismissal needs to know WHICH line the reader hid, and the text is not an
/// identity: it is reworded, translated and reformatted. So the id travels with
/// the line, and [`task_line`] stays the plain-text door every existing caller
/// already uses.
pub fn task_line_hint(state: &HintState, supported: impl Fn(&str) -> bool) -> Option<Hint> {
    // The rail outranks everything, because while it holds the keyboard the
    // composer receives nothing at all and the arrows walk the list, not the
    // field: naming a command here would teach a habit that bounces.
    if state.rail_split {
        return Some(Hint {
            id: "rail-split",
            text: "type to find · enter splits · esc cancels · yes, you can have two".to_string(),
            command: "",
        });
    }
    if state.rail_focused {
        return Some(Hint {
            id: "rail",
            text: "type to filter · enter opens · esc back · the list will not read itself".to_string(),
            command: "",
        });
    }
    // An offer waiting is the most actionable thing on screen, and the arrows
    // reach it instead of the composer history, so the card has to say so or
    // the keys stay a secret (#3101). It also has to say how to LEAVE: the
    // card holds the keyboard while it is focused, and a typed character is
    // dropped rather than sent, so a reader who followed the line into the
    // card was told nothing about getting back out (#3114). Down past the last
    // row and Esc both return the caret to the field.
    if state.offers > 0 && !state.processing {
        return Some(Hint {
            id: "offers",
            text: if state.offers == 1 {
                "↑ picks the option · enter sends · ↓ or esc back to the field · stop overthinking, pick one"
                    .to_string()
            } else {
                format!(
                    "↑↓ walk the {} options · enter sends · ↓ past the last or esc back to the field · stop overthinking, pick one",
                    state.offers
                )
            },
            command: "",
        });
    }
    // A queue is the only rung that is also true while a turn runs.
    if state.queued > 0 {
        return Some(Hint {
            id: "queue",
            text: if state.processing {
                format!(
                    "{} queued · sent when the turn ends · patience, they are coming",
                    state.queued
                )
            } else {
                format!("{} queued · sending now · yes, all of them", state.queued)
            },
            command: "",
        });
    }
    // Nothing is suggested while the answer is still being written: the reader
    // is waiting, not choosing, and the field already says Esc stops it.
    hint(state, supported)
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
