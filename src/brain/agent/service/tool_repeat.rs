//! Consecutive identical tool-call detection (#1030).
//!
//! Repeating a call with identical arguments returns an identical result, so
//! it is never productive. Left unnoticed it runs until the PROVIDER objects:
//! DashScope-style backends reject the whole conversation with a 500
//! `Repetitive tool calls detected`, after which the history is poisoned and
//! every subsequent request fails the same way. [`super::repetition`] recovers
//! from that state; this notices first, which is cheaper.
//!
//! **This never ends a turn.** OpenCrabs nudges and retries, and once the
//! retry budget is spent it walks the fallback chain. This adds one more
//! signal to that ladder and changes nothing about it. The reference
//! implementation (qwen-code's `loopDetectionService`) halts the turn instead;
//! that remedy is deliberately not copied, only the detection.
//!
//! Nor does it suppress a call. The repeated call still executes and its
//! result still reaches the model; the nudge is appended alongside. Skipping
//! the call would silently change what the agent did, and a detector is not
//! entitled to that.

use serde_json::Value;

/// Consecutive identical calls tolerated before the nudge fires.
///
/// Provider-relative, not arbitrary. qwen-code sets its equivalent to 5 and
/// documents that as deliberately below the server-side threshold that rejects
/// the conversation. Sitting one under that leaves a round of headroom for the
/// nudge to change course before the guardrail fires, which is the entire
/// point of noticing early.
pub(crate) const REPEAT_NUDGE_AT: u32 = 4;

/// What the tracker concluded about the call just observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepeatVerdict {
    /// Different from the previous call. Nothing to say.
    Fresh,
    /// Same as the previous call, still under the threshold.
    Repeating(u32),
    /// The threshold was reached on this call; nudge now.
    NudgeNow(u32),
}

/// Identity of a tool call for repeat comparison.
///
/// Canonicalizes `input` before serializing: every object's keys are sorted,
/// recursively. The order-independence this needs used to come for free from
/// `serde_json`'s default `BTreeMap`-backed map, but feature unification
/// broke that assumption: with the `gui` feature enabled, gpui's dependency
/// tree turns on `serde_json/preserve_order`, the map becomes
/// insertion-ordered, and `Display` would serialize in the order the model
/// emitted. A reordered re-send of the same call would then look like a
/// different call, which is exactly the guard evasion this detector exists
/// to close. Sorting here makes identity stable under both map backends,
/// so no future dependency can silently reopen the hole. (A test pins it.)
pub(crate) fn signature(name: &str, input: &Value) -> String {
    format!("{name}|{}", canonical(input))
}

/// Rebuild `value` with every object's keys in sorted order, recursively.
///
/// Under the default map this is an identity transformation (its iteration is
/// already sorted); under `preserve_order` it is what restores sorted order.
/// Array order is preserved: a list of arguments is positional, so reordering
/// one is a genuinely different call.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonical(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// Tracks how many times running the same call has been requested in a row.
#[derive(Debug, Default)]
pub(crate) struct ToolRepeatTracker {
    last: Option<String>,
    consecutive: u32,
    /// Whether the current run of repeats has already been nudged, so a model
    /// that keeps going is not nudged on every single round.
    nudged: bool,
}

impl ToolRepeatTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a requested call and report what it means.
    pub(crate) fn observe(&mut self, signature: &str) -> RepeatVerdict {
        if self.last.as_deref() != Some(signature) {
            self.last = Some(signature.to_string());
            self.consecutive = 1;
            self.nudged = false;
            return RepeatVerdict::Fresh;
        }
        self.consecutive += 1;
        if self.consecutive >= REPEAT_NUDGE_AT && !self.nudged {
            self.nudged = true;
            return RepeatVerdict::NudgeNow(self.consecutive);
        }
        RepeatVerdict::Repeating(self.consecutive)
    }

    /// Forget the current run.
    ///
    /// Called when a turn is replayed. A retry or a fallback re-sends the
    /// failed attempt's tool calls, so without this the replayed copies stack
    /// onto the original count and trip the threshold on calls the model only
    /// made once. qwen-code hit the same thing and rolls its counters back on
    /// every retry event for the same reason.
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.consecutive = 0;
        self.nudged = false;
    }

    /// How many times the current call has been requested in a row.
    pub(crate) fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

/// The correction injected when a call keeps repeating.
///
/// States the mechanism rather than scolding: an identical call returns an
/// identical result, so repeating it cannot make progress. Carries an explicit
/// way out that is not another tool call, for the same reason the phantom
/// nudges do — without one, a model told to stop repeating complies by calling
/// something else pointless and loops on a different axis.
pub(crate) fn repeat_nudge(tool_name: &str, count: u32) -> String {
    format!(
        "[System: you have now called `{tool_name}` {count} times in a row with identical \
         arguments. The result above is the same one you already have; calling it again with the \
         same arguments cannot return anything different. Change the arguments, use a different \
         approach, or if you are blocked, say so and explain what you need instead of repeating \
         the call.]"
    )
}

/// Observe one whole tool round and return the correction to inject, if any.
///
/// The single entry point the loop uses, so the call site stays one line and
/// the threshold logic lives here where it is testable. `first_tool_name`
/// names the offender in the correction; a round with no tools cannot repeat
/// and is ignored outright.
pub(crate) fn observe_round(
    tracker: &mut ToolRepeatTracker,
    round_signature: &str,
    first_tool_name: Option<String>,
) -> Option<String> {
    if round_signature.is_empty() {
        return None;
    }
    match tracker.observe(round_signature) {
        RepeatVerdict::NudgeNow(count) => Some(repeat_nudge(
            first_tool_name.as_deref().unwrap_or("that tool"),
            count,
        )),
        RepeatVerdict::Fresh | RepeatVerdict::Repeating(_) => None,
    }
}
