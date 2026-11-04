use crate::brain::provider::{ProviderStream, StopReason};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;

use super::builder::AgentService;

/// Result type alias used by approval/sudo callbacks
pub(super) type Result<T> = super::super::error::Result<T>;

/// Tool approval request information
#[derive(Debug, Clone)]
pub struct ToolApprovalInfo {
    /// Session this tool call belongs to
    pub session_id: Uuid,
    /// Tool name
    pub tool_name: String,
    /// Tool description
    pub tool_description: String,
    /// Tool input parameters
    pub tool_input: Value,
    /// Tool capabilities
    pub capabilities: Vec<String>,
}

/// Type alias for approval callback function.
/// Returns `(approved, always_approve)`:
/// - `approved`: whether this tool call is allowed
/// - `always_approve`: if true, skip approval for all subsequent tools in this loop
pub type ApprovalCallback = Arc<
    dyn Fn(ToolApprovalInfo) -> Pin<Box<dyn Future<Output = Result<(bool, bool)>> + Send>>
        + Send
        + Sync,
>;

/// Progress event emitted during tool execution
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    Thinking,
    ToolStarted {
        tool_name: String,
        tool_input: Value,
        /// The provider's id for this call, so a completion finds its own
        /// start even when two calls of one tool run in parallel (#1880).
        /// Empty when the emitter has no id: consumers fall back to the name.
        tool_call_id: String,
    },
    ToolCompleted {
        tool_name: String,
        tool_input: Value,
        success: bool,
        summary: String,
        /// Same id as the matching `ToolStarted` (#1880). Empty when unknown.
        tool_call_id: String,
    },
    /// A detached background task started (#2145).
    ///
    /// Emitted synchronously by the spawn call, while the turn that started
    /// the task is still running, so a surface can draw the task as ONE row
    /// inside that turn's existing tool block. Before this, a detach produced
    /// no event at all: the turn ended, and the finish arrived later as a
    /// separate push that opened its own block. `task_id` is the identity the
    /// finish carries back, so the row is updated rather than duplicated.
    DetachedTaskStarted {
        /// Stable id for this task, matching the finish event.
        task_id: String,
        /// Short label the surface shows (`cargo test`, `npm run build`).
        label: String,
    },
    /// A detached background task finished (#2145).
    ///
    /// Same `task_id` as the matching start, so the surface finds the row it
    /// already drew and edits it in place. A finish with no matching row (the
    /// process restarted, the callback was gone, an older binary parked the
    /// push) is not an error: the surface falls back to its existing
    /// announcement path.
    DetachedTaskFinished {
        /// Same id as the matching `DetachedTaskStarted`.
        task_id: String,
        /// Short label, repeated so a fallback can render without the start.
        label: String,
        /// Exit 0. The same outcome signal the receipt card shows.
        success: bool,
        /// Wall-clock runtime of the detached command, seconds.
        elapsed_secs: f32,
    },
    /// Intermediate text the agent sends between tool call batches
    IntermediateText {
        text: String,
        reasoning: Option<String>,
    },
    /// A queued user message was injected between tool iterations
    QueuedUserMessage {
        text: String,
    },
    /// Real-time streaming chunk from the LLM (word-by-word)
    StreamingChunk {
        text: String,
    },
    /// Auto-compaction is about to run — the silent window produces zero
    /// streaming chunks. Carries the context FILL LEVEL at trigger time so
    /// channels can surface it (a level, not compaction progress — the
    /// summarizer call has no intermediate state, #29), plus the wall-clock
    /// duration the session's PREVIOUS successful compaction actually took,
    /// so channels can show a grounded ETA hint. `None` on a session's first
    /// compaction — a static guess is never right (observed live range:
    /// 10s → 29 min), so no history means no prediction (#29 E2).
    Compacting {
        usage_pct: f64,
        predicted: Option<std::time::Duration>,
    },
    /// Compaction finished — carry the summary so the TUI can display it,
    /// plus before/after fill levels and the wall-clock duration of the
    /// silent window so channels can render a completion line (#29).
    CompactionSummary {
        summary: String,
        before_pct: f64,
        after_pct: f64,
        elapsed: std::time::Duration,
    },
    /// A single build-output line (e.g. "Compiling foo v1.0"). The TUI keeps a
    /// rolling window of the last few lines and clears them on RestartReady.
    BuildLine(String),
    /// Build completed — TUI should restart into the new binary.
    RestartReady {
        status: String,
        /// The binary to exec on restart. `Some` when the producer knows the
        /// exact path (e.g. a source build returns the freshly-built binary,
        /// which is NOT the running exe on a pre-built install). `None` when
        /// the running exe was replaced in place (e.g. `/evolve`) — the
        /// handler resolves it via `current_exe()`. Without this, a source
        /// build on a pre-built binary restarted into the stale exe instead of
        /// the binary it just built (#179 follow-up).
        binary_path: Option<std::path::PathBuf>,
    },
    /// Real-time token count update — fire after every API response and tool execution
    TokenCount(usize),
    /// Reasoning/thinking content from providers like MiniMax (display-only)
    ReasoningChunk {
        text: String,
    },
    /// Self-healing action was taken (config recovery, emergency compaction, truncation, etc.)
    /// Renderers normalize dashes on display (#1745), so producers need not.
    SelfHealingAlert {
        message: String,
    },
    /// The just-streamed assistant text has been detected as a gaslighting
    /// refusal preamble (e.g. "tools aren't responding") emitted alongside a
    /// valid tool_use block. The UI should wipe its in-progress streaming
    /// buffer so the lie doesn't stay on screen.
    StripStreamedContent {
        /// Number of bytes to strip from the START of the streaming buffer.
        /// The gaslighting preamble is always leading, so consumers should
        /// drain exactly this many bytes (at a char boundary) rather than
        /// wiping the whole buffer — otherwise any legitimate draft that
        /// followed the preamble in the same text block is destroyed.
        bytes: usize,
        reason: String,
    },
    /// Sticky fallback promoted a new provider/model. Carries structured data
    /// so UIs can update the session + footer without parsing text.
    ProviderSwitched {
        from_name: String,
        from_model: String,
        to_name: String,
        to_model: String,
        reason: String,
    },
    /// A retry attempt is in progress (stream drop, network error, etc.).
    /// Transient notification — shows attempt count and reason.
    RetryAttempt {
        attempt: u32,
        max: u32,
        reason: String,
    },
    /// Optional follow-up suggestions the user MAY accept or ignore, surfaced
    /// by the `suggest_options` tool (non-blocking). The TUI renders one
    /// option as ghost text in the input (Tab fills it) and several as a
    /// pick-list; accepting fills the input as editable text, never submits.
    SuggestedOptions(Vec<crate::brain::tools::suggest_options::SuggestionItem>),
    /// A live plan reached 100% at turn settle (#2255).
    ///
    /// Emitted by the surface-agnostic settle hook immediately before the
    /// plan archives, so every surface learns the fact at the same moment
    /// instead of the fact being TUI-only knowledge. `tasks` is the number
    /// of resolved tasks the plan closed with, `title` names what finished.
    /// The TUI plays the opt-in completion sound on it; no other surface
    /// has to react.
    PlanCompleted {
        /// Plan title, empty when the plan never carried one.
        title: String,
        /// Number of resolved tasks at completion.
        tasks: usize,
    },
}

/// Callback for reporting progress during agent execution.
/// The first parameter is the `session_id` the event belongs to.
pub type ProgressCallback = Arc<dyn Fn(Uuid, ProgressEvent) + Send + Sync>;

/// Events sent through `session_updated_tx` to notify the TUI about remote channel
/// session activity (Telegram, WhatsApp, Discord, Slack).
#[derive(Debug, Clone)]
pub enum ChannelSessionEvent {
    /// A remote channel started processing a session
    ProcessingStarted(uuid::Uuid),
    /// Session content was updated (tool result persisted, response complete, etc.)
    Updated(uuid::Uuid),
    /// A remote channel finished processing a session
    ProcessingFinished(uuid::Uuid),
    /// Session title was generated or changed (e.g. by the auto-title path).
    /// Carries the new title so the TUI can update its in-memory copy
    /// without a DB roundtrip. Cheap counterpart to `Updated` for the
    /// specific case of "the title changed and nothing else needs to
    /// reload" — see TuiEvent::SessionTitleUpdated for the consumer side.
    TitleUpdated(uuid::Uuid, String),
}

/// Callback for requesting sudo password from the user.
/// Takes the command string, returns Ok(Some(password)) or Ok(None) if cancelled.
pub type SudoCallback = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send>> + Send + Sync,
>;

/// Callback for requesting an SSH password from the user.
///
/// Same signature as `SudoCallback`; the input is a human-readable target
/// label (e.g. `"root@1.2.3.4 (ssh)"`) rather than a command string. Wired
/// up by the TUI to a password dialog and by channels (future) to an
/// approval card. When `None`, ssh commands that need a password fall back
/// to returning the raw probe stderr to the agent.
pub type SshPasswordCallback = SudoCallback;

/// Callback for checking if a user message has been queued for THIS session
/// during tool execution. Returns Some(message) if one is waiting for the
/// given `session_id`, None otherwise. Must not block.
///
/// The `session_id` parameter is required: prior to 2026-04-27 this callback
/// was nullary and read from a single process-wide slot, so when two panes
/// (or two channels) had concurrent agent loops, a message queued in pane A
/// could be drained by pane B's agent and injected into the wrong session.
pub type MessageQueueCallback = Arc<
    dyn Fn(Uuid) -> Pin<Box<dyn Future<Output = Option<QueuedUserMessage>> + Send>> + Send + Sync,
>;

/// Symmetric producer for the per-session message queue (#722): pushes a
/// `QueuedUserMessage` into `session_id`'s queue so it is drained at the next
/// tool-loop iteration boundary (or starts a fresh turn if the session is idle).
/// Each surface (TUI, channels) provides one that targets its own queue, so a
/// background-task watcher can resume any session without knowing the surface.
pub type MessageEnqueueCallback = Arc<dyn Fn(Uuid, QueuedUserMessage) + Send + Sync>;

/// A user message queued for injection between tool rounds, split into what
/// the LLM sees and what persists. Synthetic steering prefaces and reaction
/// guidance belong in `context_text` for the live turn ONLY; the DB and the
/// TUI history get `display_text` (the user's actual words, or a compact
/// "[System: ...]" tag), so prompt scaffolding never pollutes the session.
/// What produced a queued/injected message (#1221). The Telegram resume
/// callback uses it to decide whether the delivery earns a collapsible echo
/// bubble: background-task completions and cross-session session_notify
/// pushes do (the user must see WHAT woke the session), while sub-agent
/// results, ingress queueing and recovery replays do not render one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOrigin {
    /// A detached bash task finished (background_tasks completion path).
    BackgroundTask,
    /// A cross-session push via the session_notify tool (#1203). Echoes into
    /// the recipient's topic like background-task completions (#1221).
    SessionNotify,
    /// A spawned sub-agent reported back (spawn.rs push_result / notify).
    SubAgent,
    /// Startup crash-recovery replaying an interrupted turn.
    Recovery,
    /// A user message queued mid-turn (ingress or reaction path).
    Ingress,
    /// Anything else — safe default, never renders a push echo.
    Other,
}

/// Typed receipt payload for a background-task completion push (#15).
///
/// The rendering surface (Telegram resume echo) builds the collapsible
/// receipt card from these fields instead of parsing `context_text` — the
/// `[System: ...]` shape belongs to the LLM and must stay free to evolve
/// without breaking the bubble.
/// Receipt payload for BackgroundTask-origin pushes (#111). Derives serde so
/// the durable notify queue (#111) can round-trip it through its JSON
/// column.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BgTaskMeta {
    /// Identity of the task this receipt belongs to (#2145).
    ///
    /// Lets a surface match the receipt to the row it already drew from
    /// `DetachedTaskStarted` and edit that row, instead of posting the
    /// outcome as a second message. Empty when unknown (a receipt persisted
    /// by an older binary, or a task started without a progress callback), so
    /// surfaces fall back to their announcement path rather than dropping the
    /// result.
    #[serde(default)]
    pub task_id: String,
    /// Exit 0 — the sole outcome signal the receipt card shows (the exit
    /// code itself stays in the context text, for the model only).
    pub success: bool,
    /// Short roster label (`short_label` form: ≤60 chars + ellipsis).
    pub label: String,
    /// Wall-clock runtime of the detached command, seconds.
    pub elapsed_secs: f32,
    /// Last 50 lines of merged stdout+stderr, verbatim.
    pub tail: String,
}

/// Origin recorded on a `pending_requests` row for restart recovery (#12).
///
/// Distinct from [`PushOrigin`]: that one decides echo-bubble rendering for a
/// queued message; this one decides what BOOT does with a turn that was
/// killed mid-tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOrigin {
    /// A user-initiated turn (channel ingress / TUI / button tap). Boot
    /// replays it with the continuation prompt — the behavior that predates
    /// #12, unchanged.
    User,
    /// A push-initiated turn (session_notify / background-task completion).
    /// Boot must NOT replay the LLM turn: re-running an interrupted tool
    /// call can double-execute side effects such as installs or binary
    /// swaps. Boot re-delivers the original push text to the owning
    /// surface instead, which re-drives the intent through the normal wake
    /// path.
    System,
}

impl PendingOrigin {
    /// The value persisted in `pending_requests.origin`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            PendingOrigin::User => "user",
            PendingOrigin::System => "system",
        }
    }

    /// Parse a persisted `pending_requests.origin` value. Unknown or legacy
    /// values map to [`PendingOrigin::User`] so pre-#12 rows keep their
    /// historical replay behavior.
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "system" => PendingOrigin::System,
            _ => PendingOrigin::User,
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueuedUserMessage {
    /// Full text injected into the LLM context for the live turn.
    pub context_text: String,
    /// What persists to the DB and shows in the session history.
    pub display_text: String,
    /// Which producer built this message (#1221).
    pub origin: PushOrigin,
    /// Receipt payload for BackgroundTask-origin pushes (#15); `None` for
    /// every other producer.
    pub bg_meta: Option<BgTaskMeta>,
}

impl QueuedUserMessage {
    /// A plain message with no synthetic framing: context and display match.
    pub fn plain(text: String) -> Self {
        Self {
            context_text: text.clone(),
            display_text: text,
            origin: PushOrigin::Other,
            bg_meta: None,
        }
    }

    /// A synthetic system message (#722): the LLM sees `context`, but history
    /// and the UI show the compact `display` tag so scaffolding never pollutes
    /// the session transcript.
    pub fn system(context: String, display: String) -> Self {
        Self {
            context_text: context,
            display_text: display,
            origin: PushOrigin::Other,
            bg_meta: None,
        }
    }

    /// Fold a drained queue into one message, joining the two halves
    /// SEPARATELY.
    ///
    /// Joining the queue into a single string and calling [`Self::plain`] on it
    /// looks equivalent, and is for typed messages where both halves match. It
    /// is not for a synthetic entry: collapsing there promotes the whole
    /// context into the display text, which published a background task's
    /// entire `[System: ...]` block, command and output included, as a user
    /// turn in the transcript (#765).
    ///
    /// `None` for an empty queue, so the caller can skip the injection.
    pub fn join(msgs: &[Self]) -> Option<Self> {
        if msgs.is_empty() {
            return None;
        }
        let join_on = |f: fn(&Self) -> &str| msgs.iter().map(f).collect::<Vec<_>>().join("\n");
        Some(Self {
            context_text: join_on(|m| m.context_text.as_str()),
            display_text: join_on(|m| m.display_text.as_str()),
            // #1221: a joined batch inherits the head message's origin — the
            // first-completed task dominates what the echo announces.
            origin: msgs[0].origin,
            // #15: same dominance rule for the receipt payload — the head
            // task's card is the one the echo renders.
            bg_meta: msgs[0].bg_meta.clone(),
        })
    }
}

/// Response from the agent
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// Message ID in database
    pub message_id: Uuid,

    /// Response content
    pub content: String,

    /// Stop reason
    pub stop_reason: Option<StopReason>,

    /// Token usage (accumulated across all tool-loop iterations — for billing)
    pub usage: crate::brain::provider::TokenUsage,

    /// Actual context window usage from the last API call (for display)
    pub context_tokens: u32,

    /// Tokens per second for this turn (for display in channel footers)
    pub tokens_per_second: Option<f64>,

    /// Cost in USD
    pub cost: f64,

    /// Model used
    pub model: String,

    /// Provider that produced this response. Set from the per-session active
    /// provider at the moment of construction, so it reflects sticky-fallback
    /// targets too. Callers who persist `model` MUST persist `provider_name`
    /// from the same response in the same write — the {provider, model} pair
    /// is a locked unit, splitting them across writes lets a fallback's
    /// model leak onto a different provider's session row and produces
    /// cross-provider routing on the next turn (e.g. dialagram/glm-5.1
    /// where glm-5.1 belongs to zhipu's catalogue).
    pub provider_name: String,

    /// Whether this turn STARTED on the session's own saved provider (#705).
    /// `false` means the turn ran on a provider that did not match the session's
    /// saved `provider_name` — an involuntary remap, not a user pick or a real
    /// fallback. Callers that persist the {provider, model} pair back to the
    /// session row MUST skip the write when this is `false`, or a wrong pair
    /// silently overwrites the session's saved choice and survives restarts. A
    /// genuine fallback starts matched (`true`) and diverges later, and is
    /// persisted by the ProviderSwitched handler.
    pub started_on_session_provider: bool,
}

/// Streaming response from the agent
pub struct AgentStreamResponse {
    /// Session ID
    pub session_id: Uuid,

    /// Message ID that will be created
    pub message_id: Uuid,

    /// Stream of events
    pub stream: ProviderStream,

    /// Model being used
    pub model: String,
}

// Make AgentService's extract_text_from_response available to types that need it
impl AgentService {
    /// Extract text content from an LLM response (text blocks only — tool calls
    /// are displayed via the tool group UI, not as raw text).
    pub(super) fn extract_text_from_response(
        response: &crate::brain::provider::LLMResponse,
    ) -> String {
        let mut text = String::new();

        for content in &response.content {
            if let crate::brain::provider::ContentBlock::Text { text: t } = content
                && !t.trim().is_empty()
            {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(t);
            }
        }

        text
    }

    /// Extract a usable title candidate from an LLM response, falling
    /// through several shapes some providers use for short prompts.
    /// Returns the cleaned candidate (already trimmed, dequoted, capped
    /// at 60 chars) or empty string if nothing usable was found.
    ///
    /// Order:
    /// 1. Concatenated `ContentBlock::Text` blocks (the normal path).
    /// 2. `ContentBlock::Thinking` content. Reasoning models like
    ///    `qwen-3.7-max-preview-thinking` sometimes return ONLY a
    ///    Thinking block for very short prompts ("generate a title")
    ///    and never finalize a Text block. Issue #121: auto-title ran
    ///    fine in isolation but produced empty titles on the reporter's
    ///    setup, so sessions stayed stuck on the default
    ///    channel-generated name forever.
    ///
    /// For the Thinking fallback we extract the last quoted phrase if
    /// any (most likely the candidate the model settled on), otherwise
    /// take the last short sentence trimmed to title length.
    pub(crate) fn extract_title_candidate(
        response: &crate::brain::provider::LLMResponse,
    ) -> String {
        let from_text = Self::clean_auto_title(&Self::extract_text_from_response(response));
        if !from_text.is_empty() {
            return from_text;
        }
        for content in &response.content {
            if let crate::brain::provider::ContentBlock::Thinking { thinking, .. } = content {
                let cand = pluck_title_from_thinking(thinking);
                let cleaned = Self::clean_auto_title(&cand);
                if !cleaned.is_empty() {
                    return cleaned;
                }
            }
        }
        String::new()
    }

    /// Strip injected channel preamble blocks from message text before it
    /// reaches the title-generation prompt. Channels prepend `[Channel: …]`,
    /// `[Reaction directive: …]`, `[Recent group history …]`, and similar
    /// bracket-delimited blocks for LLM context; the title LLM should only
    /// see the actual user text. Only strips leading blocks whose opening
    /// matches a known preamble prefix, so user text like `[BUG] Fix crash`
    /// is preserved. Handles nested brackets and multi-line spans. #688
    pub(crate) fn strip_channel_preamble(input: &str) -> String {
        /// Known preamble block prefixes injected by channel handlers.
        const PREAMBLE_PREFIXES: &[&str] = &[
            "[Channel:",
            "[Reaction directive:",
            "[Recent group history",
            // Discord and Slack say "channel" where Telegram and WhatsApp say
            // "group" (#1618, #1619, #1620). Without this the whole history
            // block reached the title prompt.
            "[Recent channel history",
            "[Telegram group",
            "[Telegram DM",
            "[Discord",
            "[Slack",
            "[WhatsApp",
            "[Trello",
            "[System:",
        ];

        let mut s = input.trim_start();
        loop {
            // Only strip blocks that match a known preamble prefix.
            if !PREAMBLE_PREFIXES.iter().any(|p| s.starts_with(p)) {
                break;
            }
            // Walk to the matching closing bracket, tracking depth so
            // nested brackets (e.g. group-history lines like "[13:57]")
            // don't terminate the block early.
            let mut depth = 0i32;
            let mut end: Option<usize> = None;
            for (i, ch) in s.char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + ch.len_utf8());
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(pos) => s = s[pos..].trim_start(),
                None => break, // unmatched bracket, stop stripping
            }
        }
        s.to_string()
    }

    /// Post-process an LLM-generated auto-title: trim whitespace, strip
    /// surrounding quotes, and cap at 60 characters.
    pub(crate) fn clean_auto_title(raw: &str) -> String {
        let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
        if trimmed.is_empty() {
            return String::new();
        }
        let char_count = trimmed.chars().count();
        if char_count > 60 {
            trimmed.chars().take(60).collect()
        } else {
            trimmed.to_string()
        }
    }

    /// Check if a session title is a default channel-generated title that
    /// should be replaced by auto-title. Default titles follow specific patterns:
    /// - Telegram DM: "Telegram: DM <name> (<id>) [chat:<id>]"
    /// - Discord channel: "Discord: #<channel>"
    /// - Slack channel: "Slack: #<channel>"
    /// - New Chat (exact match)
    ///
    /// Auto-titled sessions like "Telegram: Fix Bug Report [chat:456]" do NOT match
    /// these patterns, preventing auto-title from firing on every message.
    pub(crate) fn is_default_channel_title(title: &str) -> bool {
        // Exact match for "New Chat"
        if title == "New Chat" {
            return true;
        }

        // Telegram DM: "Telegram: DM <name> (<id>) [chat:<id>]"
        // After "Telegram: ", must have "DM " AND contain "(<id>)"
        if let Some(rest) = title.strip_prefix("Telegram: ") {
            return rest.starts_with("DM ") && rest.contains('(') && rest.contains(')');
        }

        // Discord channel: "Discord: #<channel>"
        // After "Discord: ", must start with "#"
        if let Some(rest) = title.strip_prefix("Discord: ") {
            return rest.starts_with('#');
        }

        // Slack channel: "Slack: #<channel>"
        // After "Slack: ", must start with "#"
        if let Some(rest) = title.strip_prefix("Slack: ") {
            return rest.starts_with('#');
        }

        // WhatsApp and Trello: no clear default pattern marker, skip auto-title
        // to prevent repeated firing. Users can manually rename if needed.

        false
    }

    /// Extract the channel prefix from a title if it exists.
    /// Returns the prefix (e.g., "Telegram: ") or empty string if none.
    pub(crate) fn extract_channel_prefix(title: &str) -> &str {
        let prefixes = [
            "Telegram: ",
            "Discord: ",
            "Slack: ",
            "WhatsApp: ",
            "Trello: ",
        ];
        for prefix in prefixes.iter() {
            if title.starts_with(prefix) {
                return prefix;
            }
        }
        ""
    }

    /// Extract the `[chat:ID]` suffix from a channel session title.
    /// This suffix is the stable identifier that `find_session_by_title_suffix`
    /// uses to resolve sessions across renames. Auto-title MUST preserve it
    /// or every subsequent message creates a new session (issue #115).
    pub(crate) fn extract_chat_id_suffix(title: &str) -> &str {
        // Find the last `[chat:` occurrence and return from there to end
        if let Some(pos) = title.rfind("[chat:") {
            let suffix = &title[pos..];
            // Validate it ends with `]`
            if suffix.ends_with(']') {
                return suffix;
            }
        }
        ""
    }
}

/// Pull a likely title out of a Thinking block. Reasoning models that
/// answer "generate a title" without producing a Text block typically
/// leave one or more candidate titles inside their thinking. Heuristic:
/// the LAST quoted phrase (single or double quotes) is usually the
/// model's settled choice; failing that, the LAST short sentence.
fn pluck_title_from_thinking(thinking: &str) -> String {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Last quoted phrase. Try double quotes first, then single.
    for delim in ['"', '\''] {
        let mut last: Option<String> = None;
        let mut chars = trimmed.char_indices();
        while let Some((start, c)) = chars.next() {
            if c != delim {
                continue;
            }
            // Find the next matching delim.
            for (end, c2) in chars.by_ref() {
                if c2 == delim {
                    let inner = &trimmed[start + delim.len_utf8()..end];
                    if !inner.trim().is_empty() && inner.chars().count() <= 60 {
                        last = Some(inner.trim().to_string());
                    }
                    break;
                }
            }
        }
        if let Some(s) = last {
            return s;
        }
    }

    // Fallback: last short sentence. Split on `. ` `! ` `? ` and take
    // the last segment under 60 chars.
    let mut sentences: Vec<&str> = trimmed
        .split(['.', '!', '?', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    while let Some(last) = sentences.pop() {
        if last.chars().count() <= 60 {
            return last.to_string();
        }
    }
    String::new()
}
