//! Announcement loop guard (#957, extended by #961) — the text layer.
//!
//! Second half of the Luna fix: a per-session ring buffer of outgoing
//! assistant texts. The tool layer catches the near-identical `bash`
//! echoes; this layer catches the reworded announcements that each land
//! as a separate, internally clean turn, so every within-turn guard
//! (#507, phantom intent-loop, #740, #788) misses them. Since #961 the
//! ring also sees intermediate (between-tool-calls) text, catching the
//! DeepSeek v4 flash pattern of re-announcing the same pending action
//! inside a single turn.
//!
//! Detect and surface, don't prevent (the #954 guard philosophy): the
//! first trip still delivers the text plus a system nudge; only a second
//! trip with the nudge ignored escalates to an abort through the existing
//! repetition -> loop-message path (`format_user_error`).

use std::collections::{HashSet, VecDeque};

use super::helpers::normalize_loop_text;

/// Outgoing texts remembered per session (#957, raised 5 -> 8 for #961:
/// cap 5 evicted the early duplicates of the DeepSeek v4 flash zip-send
/// pattern before the ring could ever trip).
pub const TEXT_LOOP_RING_CAP: usize = 8;
/// Near-duplicate hits within the ring that trip the guard (#957).
pub const TEXT_LOOP_TRIP_AT: usize = 3;
/// Jaccard similarity threshold on normalized word sets (#957).
const NEAR_DUPLICATE_JACCARD: f64 = 0.8;
/// Overlap-coefficient threshold on normalized word sets (#961): a short
/// reworded announcement whose words are almost all contained in a
/// longer one still counts as a near-duplicate even when Jaccard is
/// dragged down by the length difference ("Sending the zip:" vs "Sending
/// the zip to this thread now:").
const NEAR_DUPLICATE_OVERLAP: f64 = 0.8;
/// Below this many normalized words, word-set ratios are too coarse —
/// only exact normalized equality counts.
const NEAR_DUPLICATE_MIN_WORDS: usize = 3;

/// Near-duplicate check for loop detection (#957, extended for #961).
///
/// Jaccard similarity >= 0.8 on normalized word sets, plus an
/// overlap-coefficient clause (>= 0.8 of the SMALLER word set shared)
/// for reworded pairs where one text is much shorter than the other.
/// Equality fallback: short texts (< 3 normalized words) only count as
/// duplicates when their normalized forms are identical, because word-set
/// ratios are too coarse below a handful of words.
pub fn near_duplicate(a: &str, b: &str) -> bool {
    // Directives are channel instructions, not prose (#2129): stripping them
    // here as well as at the recording call site means no caller can make two
    // different reactions look like the same announcement.
    let a = super::phantom::strip_inline_directives(a);
    let b = super::phantom::strip_inline_directives(b);
    let na = normalize_loop_text(&a);
    let nb = normalize_loop_text(&b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    match similarity_scores(&na, &nb) {
        Some((jaccard, overlap)) => {
            jaccard >= NEAR_DUPLICATE_JACCARD || overlap >= NEAR_DUPLICATE_OVERLAP
        }
        None => false,
    }
}

/// The two word-set scores behind [`near_duplicate`], as `(jaccard, overlap)`
/// on normalized text. `None` when either side is too short to compare.
/// Shared with the trip log so the logged scores are the decided ones (#2052).
pub(crate) fn similarity_scores(na: &str, nb: &str) -> Option<(f64, f64)> {
    let wa: HashSet<&str> = na.split(' ').collect();
    let wb: HashSet<&str> = nb.split(' ').collect();
    if wa.len() < NEAR_DUPLICATE_MIN_WORDS || wb.len() < NEAR_DUPLICATE_MIN_WORDS {
        return None;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    let min_len = wa.len().min(wb.len()) as f64;
    let jaccard = if union > 0.0 { inter / union } else { 0.0 };
    let overlap = if min_len > 0.0 { inter / min_len } else { 0.0 };
    Some((jaccard, overlap))
}

/// Longest excerpt of a text the trip log carries. The full text stays in the
/// session's messages; the log only needs enough to see what repeated.
const TRIP_LOG_EXCERPT_CHARS: usize = 240;

/// Debug-log every ring entry that matched a tripping text, with the scores
/// that decided it (#2052). Debug only: no effect on the guard's decision.
fn log_trip_pairs(text: &str, ring: &VecDeque<String>) {
    let current = normalize_loop_text(text);
    for prior in ring.iter() {
        let prior_norm = normalize_loop_text(prior);
        if !near_duplicate(text, prior) {
            continue;
        }
        let (jaccard, overlap) = if current == prior_norm {
            (1.0, 1.0)
        } else {
            similarity_scores(&current, &prior_norm).unwrap_or((0.0, 0.0))
        };
        tracing::debug!(
            current_words = current.split(' ').count(),
            prior_words = prior_norm.split(' ').count(),
            exact = current == prior_norm,
            jaccard = format!("{jaccard:.2}"),
            overlap = format!("{overlap:.2}"),
            current_text = %excerpt(text),
            prior_text = %excerpt(prior),
            "announcement_loop: near-duplicate pair at trip"
        );
    }
}

fn excerpt(text: &str) -> String {
    let mut out: String = text.chars().take(TRIP_LOG_EXCERPT_CHARS).collect();
    if text.chars().count() > TRIP_LOG_EXCERPT_CHARS {
        out.push('…');
    }
    out
}

/// Outcome of checking an outgoing text against the session ring (#957).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLoopAction {
    /// No loop signal; deliver normally.
    Continue,
    /// First trip: deliver, but surface a system nudge for the next turn.
    Nudge,
    /// The nudge was ignored [`MAX_TEXT_LOOP_NUDGES`] turns in a row: abort
    /// through the loop-message path.
    Abort,
}

/// How many nudges a session gets before the cross-turn guard gives up (#2129).
///
/// The owner's rule for this guard: "we never abandon anything we nudge, retry
/// 5x times then fallback". A single nudge followed by an immediate abort
/// punished one false positive with a provider switch, so the abort is now the
/// sixth nudged turn, not the second trip. One nudge per turn: the nudge lands
/// in the NEXT turn's prompt, so repeats inside the same turn cannot spend a
/// retry the model has not been given yet.
pub const MAX_TEXT_LOOP_NUDGES: u32 = 5;

/// Per-session ring buffer of outgoing assistant texts (#957).
///
/// Lives on `AgentService` keyed by session id — NOT in the per-turn
/// context, which is reloaded from the DB every turn, so a loop that
/// spans turns needs state that outlives a single context load. Restart
/// re-arms the guard: same accepted semantics as #507's in-loop flags.
#[derive(Debug, Default)]
pub struct OutgoingTextRing {
    ring: VecDeque<String>,
    /// Nudges delivered since the last clean turn. Counts across turns so a
    /// model that ignores every nudge still reaches the abort, while
    /// [`Self::end_turn`] clears it after a turn that did not trip (#2129).
    nudges: u32,
    /// Did this turn trip at all? Read by [`Self::end_turn`].
    tripped_this_turn: bool,
}

impl OutgoingTextRing {
    /// Close a turn: forget accumulated nudges unless this turn tripped.
    ///
    /// Consecutive tripping turns still escalate to the abort, so the #957
    /// cross-turn loop is still broken, but an occasional trip in an otherwise
    /// clean session no longer accumulates toward it. Mirrors
    /// `tool_repeat.rs`, which clears its nudge when the run of repeats ends
    /// (#2129).
    ///
    /// Called once per turn, from the cross-turn hook.
    pub fn end_turn(&mut self) {
        if !self.tripped_this_turn {
            self.nudges = 0;
        }
        self.tripped_this_turn = false;
    }

    /// Record an outgoing text, then check it against the ring.
    ///
    /// The text is stored FIRST, so it counts as one hit against itself:
    /// three near-identical outgoing texts within the ring trip the guard.
    pub fn record_and_check(&mut self, text: &str) -> TextLoopAction {
        // Channel directives are instructions to the channel layer, never
        // prose (#2129). `<<react:👀>>` and `<<react:👍>>` are different
        // actions, and normalizing them as text made every reaction look like
        // the same announcement. They are stripped before the comparison, and
        // a text that is nothing but directives is stored as the empty sentinel
        // so the ring slot exists (keeping `last_recorded` honest) while never
        // matching anything.
        let comparable = super::phantom::strip_inline_directives(text);
        let stored = if comparable.trim().is_empty() {
            String::new()
        } else {
            text.to_string()
        };
        self.ring.push_back(stored);
        while self.ring.len() > TEXT_LOOP_RING_CAP {
            self.ring.pop_front();
        }
        let hits = self
            .ring
            .iter()
            .filter(|t| near_duplicate(&comparable, t))
            .count();
        if hits >= TEXT_LOOP_TRIP_AT {
            log_trip_pairs(&comparable, &self.ring);
            // One nudge per TURN, not per recorded text. The nudge buys the
            // model its next turn to change course, so further repeats inside
            // the same turn must not burn the retry budget before that turn
            // has even ended (#2129).
            if !self.tripped_this_turn {
                self.tripped_this_turn = true;
                self.nudges += 1;
            }
            if self.nudges > MAX_TEXT_LOOP_NUDGES {
                tracing::warn!(
                    nudges = self.nudges,
                    max = MAX_TEXT_LOOP_NUDGES,
                    "announcement_loop: loop persisted after every nudge, aborting turn"
                );
                // The abort ends the turn through the loop-message path, so
                // the turn-closing hook never runs. Release the budget here or
                // the next turn would abort on its first trip: that is the
                // #2129 defect, just moved one step later. The fallback walk
                // is the escalation; the next turn starts with a fresh five.
                self.nudges = 0;
                self.tripped_this_turn = false;
                TextLoopAction::Abort
            } else {
                tracing::warn!(
                    nudges = self.nudges,
                    max = MAX_TEXT_LOOP_NUDGES,
                    "announcement_loop: near-identical outgoing text recurred, nudging"
                );
                TextLoopAction::Nudge
            }
        } else {
            TextLoopAction::Continue
        }
    }

    /// Peek at the most recently recorded text (#961). Lets a later hook
    /// skip re-recording text an earlier hook already judged, so one
    /// outgoing text can never double-count toward the trip threshold.
    pub fn last_recorded(&self) -> Option<&str> {
        self.ring.back().map(String::as_str)
    }
}
