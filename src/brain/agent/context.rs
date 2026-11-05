//! Agent Context Management
//!
//! Manages conversation context including messages, system brain,
//! and token tracking.

use crate::brain::provider::{ContentBlock, Message, Role};
use crate::brain::tokenizer;
use crate::db::models::Message as DbMessage;
use std::path::PathBuf;
use uuid::Uuid;

/// Agent context for a conversation
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Session ID
    pub session_id: Uuid,

    /// System brain
    pub system_brain: Option<String>,

    /// Conversation messages
    pub messages: Vec<Message>,

    /// Tracked files in the conversation
    pub tracked_files: Vec<TrackedFile>,

    /// Current token count estimate
    pub token_count: usize,

    /// Maximum context tokens
    pub max_tokens: usize,

    /// The provider's own count of the last request it received, when it
    /// reported one.
    ///
    /// `token_count` is a tiktoken estimate of the system prompt plus the
    /// messages. It leaves out the tool schemas the provider also receives,
    /// and it disagrees with the provider's tokenizer on code and JSON: a
    /// 2.4MB Claude CLI request we estimated at ~660k came back counted as
    /// ~1.03M, over a 1M limit we believed we were at 66% of. Compaction
    /// measured against the estimate fired eight times without ever getting
    /// under the ceiling, because the ceiling was never where we thought.
    ///
    /// Once the provider tells us a real number we anchor on it and track our
    /// own estimate's movement since, which keeps the budget honest between
    /// calls without pretending we can tokenize the way they do. Stored as
    /// (their count, our estimate at that moment) so the delta works in both
    /// directions: trimming and compaction lower it, appending raises it.
    pub provider_anchor: Option<(usize, usize)>,
}

/// A file tracked in the conversation
#[derive(Debug, Clone)]
pub struct TrackedFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub content: Option<String>,
    pub token_count: usize,
}

impl AgentContext {
    /// Create a new agent context for a session
    pub fn new(session_id: Uuid, max_tokens: usize) -> Self {
        Self {
            session_id,
            system_brain: None,
            messages: Vec::new(),
            tracked_files: Vec::new(),
            token_count: 0,
            max_tokens,
            provider_anchor: None,
        }
    }

    /// Set the system brain
    pub fn with_system_brain(mut self, prompt: String) -> Self {
        self.token_count += Self::estimate_tokens(&prompt);
        self.system_brain = Some(prompt);
        self
    }

    /// Add a message to the context
    pub fn add_message(&mut self, message: Message) {
        // Estimate tokens for the message
        let tokens = self.estimate_message_tokens(&message);
        self.token_count += tokens;
        self.messages.push(message);
    }

    /// Convert database messages to LLM messages
    pub fn from_db_messages(
        session_id: Uuid,
        db_messages: Vec<DbMessage>,
        max_tokens: usize,
    ) -> Self {
        let mut context = Self::new(session_id, max_tokens);

        for db_msg in db_messages {
            if db_msg.role == crate::services::message::PLAN_COMPLETED_ROLE {
                continue;
            }
            // Skip messages with empty content AND no captured reasoning —
            // Anthropic rejects empty text blocks. A non-empty thinking
            // column alone still justifies keeping the row so downstream
            // providers (e.g. Moonshot kimi) see the reasoning context.
            let has_content = !db_msg.content.is_empty();
            let has_thinking = db_msg
                .thinking
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty());
            if !has_content && !has_thinking {
                continue;
            }

            let role = match db_msg.role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User, // Default fallback
            };

            // Rehydrate reasoning as a leading ContentBlock::Thinking so
            // the OpenAI-compatible encoder can emit it as
            // `reasoning_content` on assistant tool_call messages.
            // Without this, Moonshot kimi 400s on any resumed turn because
            // the required `reasoning_content` field is missing.
            let mut content: Vec<ContentBlock> = Vec::new();
            if role == Role::Assistant
                && has_thinking
                && let Some(thinking) = db_msg.thinking.as_deref()
            {
                content.push(ContentBlock::Thinking {
                    thinking: thinking.to_string(),
                    signature: None,
                });
            }
            if has_content {
                content.push(ContentBlock::Text {
                    text: db_msg.content,
                });
            }

            let message = Message { role, content };

            context.add_message(message);
        }

        context
    }

    /// Track a file in the conversation
    pub fn track_file(&mut self, file: TrackedFile) {
        self.token_count += file.token_count;
        self.tracked_files.push(file);
    }

    /// Check if context would exceed limit with additional tokens
    pub fn would_exceed_limit(&self, additional_tokens: usize) -> bool {
        self.token_count + additional_tokens > self.max_tokens
    }

    /// Estimate tokens for a message
    pub(crate) fn estimate_message_tokens(&self, message: &Message) -> usize {
        let mut tokens = 0;

        for content in &message.content {
            match content {
                ContentBlock::Text { text } => {
                    tokens += Self::estimate_tokens(text);
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    tokens += Self::estimate_tokens(name);
                    tokens += Self::estimate_tokens(&input.to_string());
                }
                ContentBlock::ToolResult { content, .. } => {
                    tokens += Self::estimate_tokens(content);
                }
                ContentBlock::Image { .. } => {
                    // Images use a fixed token count (approximate)
                    tokens += 1000;
                }
                ContentBlock::Thinking { thinking, .. } => {
                    tokens += Self::estimate_tokens(thinking);
                }
            }
        }

        // Add overhead for message structure
        tokens + 4
    }

    /// Token estimation using tiktoken cl100k_base BPE encoding.
    /// No more chars/N guessing — this gives real token counts.
    pub fn estimate_tokens(text: &str) -> usize {
        tokenizer::count_tokens(text)
    }

    /// Static version of estimate_message_tokens — usable without a &self reference.
    pub fn estimate_tokens_static(message: &Message) -> usize {
        let mut tokens = 0;
        for content in &message.content {
            match content {
                ContentBlock::Text { text } => {
                    tokens += Self::estimate_tokens(text);
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    tokens += Self::estimate_tokens(name);
                    tokens += Self::estimate_tokens(&input.to_string());
                }
                ContentBlock::ToolResult { content, .. } => {
                    tokens += Self::estimate_tokens(content);
                }
                ContentBlock::Image { .. } => {
                    tokens += 1000;
                }
                ContentBlock::Thinking { thinking, .. } => {
                    tokens += Self::estimate_tokens(thinking);
                }
            }
        }
        tokens + 4
    }

    /// The size of the next request, preferring the provider's count.
    ///
    /// Falls back to the local estimate until a provider has reported one, so
    /// a first turn behaves exactly as before.
    pub fn effective_token_count(&self) -> usize {
        match self.provider_anchor {
            // Their count, moved by however much our own estimate has shifted
            // since we took it. Every path that trims or appends already
            // maintains `token_count`, so none of them need to know about the
            // anchor for the budget to follow along.
            Some((reported, estimated_at_anchor)) => reported
                .saturating_add(self.token_count)
                .saturating_sub(estimated_at_anchor),
            None => self.token_count,
        }
    }

    /// Anchor the budget on a size the provider actually reported.
    ///
    /// Callers must reject implausible reports first: an endpoint that adds a
    /// flat overhead to every call would otherwise drag the budget up and
    /// compact a context that never needed it.
    pub fn record_provider_reported_tokens(&mut self, reported: usize) {
        self.provider_anchor = Some((reported, self.token_count));
    }

    /// Get the current token usage percentage
    pub fn usage_percentage(&self) -> f64 {
        (self.effective_token_count() as f64 / self.max_tokens as f64) * 100.0
    }

    /// Returns true if a message consists entirely of ToolResult blocks.
    /// Such a message is "orphaned" if the preceding assistant(ToolUse) message
    /// was removed, and will cause the API to reject the conversation.
    pub(crate) fn is_orphaned_tool_result_msg(msg: &Message) -> bool {
        msg.role == Role::User
            && !msg.content.is_empty()
            && msg
                .content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    }

    /// Remove any leading user messages that consist solely of ToolResult blocks.
    /// Called after trimming to prevent orphaned tool results at the start of history.
    fn drop_leading_orphan_tool_results(&mut self) {
        while self
            .messages
            .first()
            .is_some_and(Self::is_orphaned_tool_result_msg)
        {
            let tokens = self.estimate_message_tokens(&self.messages[0]);
            self.token_count = self.token_count.saturating_sub(tokens);
            self.messages.remove(0);
        }
    }

    /// Trim old messages if context is too large
    pub fn trim_to_fit(&mut self, required_space: usize) {
        while self.would_exceed_limit(required_space) && !self.messages.is_empty() {
            // Remove the oldest user/assistant message pair
            if let Some(first_msg) = self.messages.first() {
                let tokens = self.estimate_message_tokens(first_msg);
                self.token_count = self.token_count.saturating_sub(tokens);
                self.messages.remove(0);
            }
        }
        // Removing an assistant(tool_use) exposes an orphaned user(tool_result) — drop it
        self.drop_leading_orphan_tool_results();
    }

    /// Hard-truncate old messages until token count is at or below `target_tokens`.
    /// Keeps at least 2 messages (the most recent pair) to maintain conversation validity.
    pub fn hard_truncate_to(&mut self, target_tokens: usize) {
        while self.token_count > target_tokens && self.messages.len() > 2 {
            let tokens = self.estimate_message_tokens(&self.messages[0]);
            self.token_count = self.token_count.saturating_sub(tokens);
            self.messages.remove(0);
        }
        self.drop_leading_orphan_tool_results();
    }

    /// #1649 delta compaction: the messages a delta summariser may see.
    ///
    /// Everything AFTER the last compaction marker. The frozen segments
    /// (markers) never re-enter the summariser input — that re-derivation
    /// was the cumulative-growth mechanism: every compaction re-billed the
    /// previous summary into the new one (x8.6 over 5 compactions, 97.8% of
    /// byte growth cumulative).
    pub(crate) fn delta_since_last_marker(&self) -> Vec<Message> {
        match self.last_marker_index() {
            Some(idx) => self.messages[idx + 1..].to_vec(),
            None => self.messages.clone(),
        }
    }

    /// #1649: decide what a new compaction summarises.
    ///
    /// - No marker yet → full window (byte-identical to the classic path).
    /// - Segments alone over half the window → consolidate the SEGMENTS only
    ///   into one fresh summary; otherwise they crowd the window and no delta
    ///   compaction can drop usage below the trigger (the 66dc5151 loop:
    ///   20+ triggers in 3 minutes, none escaped).
    /// - Otherwise → delta since the last marker.
    pub(crate) fn compaction_scope(&self) -> CompactionScope {
        if self.last_marker_index().is_none() {
            return CompactionScope::FullWindow;
        }
        if self.segments_token_count() > self.max_tokens / 2 {
            CompactionScope::SegmentConsolidation
        } else {
            CompactionScope::DeltaSinceMarker
        }
    }

    /// Tokens held by the frozen segment block: everything through the last
    /// marker, brain excluded.
    pub(crate) fn segments_token_count(&self) -> usize {
        match self.last_marker_index() {
            Some(idx) => self.messages[..=idx]
                .iter()
                .map(|m| self.estimate_message_tokens(m))
                .sum(),
            None => 0,
        }
    }

    /// Index of the first in-memory compaction marker, if any.
    pub(crate) fn first_marker_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .position(Self::is_compaction_marker_msg)
    }

    /// Index of the last in-memory compaction marker, if any.
    pub(crate) fn last_marker_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .rposition(Self::is_compaction_marker_msg)
    }

    /// An in-memory marker: a user message whose FIRST text block starts
    /// with the canonical prefix — the in-memory twin of the #175
    /// anchored-prefix rule the DB loader uses.
    pub(crate) fn is_compaction_marker_msg(m: &Message) -> bool {
        m.role == Role::User
            && matches!(m.content.first(), Some(ContentBlock::Text { text })
                if text.starts_with(COMPACTION_MARKER_PREFIX))
    }

    /// #1649: apply a DELTA summary — append a new frozen segment.
    ///
    /// Previous segments survive verbatim; everything after the last marker
    /// (the delta the summary describes) is replaced by the new marker. No
    /// prior summary text is re-derived or re-billed into the result.
    pub(crate) fn compact_with_delta_summary(&mut self, summary: String) -> String {
        let Some(idx) = self.last_marker_index() else {
            // No prior marker: this is the session's first compaction —
            // identical to the classic full-window swap.
            return self.compact_with_summary(summary, 0);
        };
        // Keep the frozen segments; drop the delta the summary covers.
        self.messages.truncate(idx + 1);
        let marker_text = format!(
            "[CONTEXT COMPACTION — {SEGMENT_SENTINEL} This block summarises only the \
             messages since the previous compaction marker. The earlier frozen \
             segments above remain in force unchanged; do not re-derive or merge \
             them.]\n\n{summary}"
        );
        let summary_msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: marker_text.clone(),
            }],
        };
        self.messages.push(summary_msg);
        self.recount_tokens_after_compaction();
        self.provider_anchor = None;
        marker_text
    }

    /// #1649: replace every frozen segment with ONE consolidated summary.
    ///
    /// The only sanctioned re-derivation: the inputs are summaries already
    /// (bounded by the summariser's output budget), never raw history, and
    /// the result is a single marker again. The consolidated marker carries
    /// no segment sentinel, so the DB loader treats it as a boundary and
    /// drops every superseded marker row on reload.
    pub(crate) fn consolidate_segments(&mut self, summary: String) -> String {
        let Some(first) = self.first_marker_index() else {
            return self.compact_with_summary(summary, 0);
        };
        let last = self.last_marker_index().unwrap_or(first);
        let tail: Vec<Message> = self.messages.split_off(last + 1);
        self.messages.truncate(first);
        let marker_text = format!(
            "[CONTEXT COMPACTION — The session's accumulated compaction summaries \
             were consolidated into this single self-contained summary. It \
             replaces every earlier one; nothing before this point survives \
             except this marker.]\n\n{summary}"
        );
        let summary_msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: marker_text.clone(),
            }],
        };
        self.messages.push(summary_msg);
        self.messages.extend(tail);
        self.recount_tokens_after_compaction();
        self.provider_anchor = None;
        marker_text
    }

    /// Text of every frozen segment marker, in order (consolidation input).
    pub(crate) fn segment_marker_texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|m| Self::is_compaction_marker_msg(m))
            .filter_map(|m| match m.content.first() {
                Some(ContentBlock::Text { text }) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// #1649: assemble the summariser input for this context's scope.
    ///
    /// One helper for BOTH the synchronous and background paths — parity by
    /// construction, not by duplication.
    pub(crate) fn compaction_input(&self) -> (CompactionScope, Vec<Message>, usize) {
        let scope = self.compaction_scope();
        match scope {
            CompactionScope::FullWindow => (scope, self.messages.clone(), self.token_count),
            CompactionScope::DeltaSinceMarker => {
                let delta = self.delta_since_last_marker();
                let tokens = delta.iter().map(AgentContext::estimate_tokens_static).sum();
                (scope, delta, tokens)
            }
            CompactionScope::SegmentConsolidation => {
                let joined = self.segment_marker_texts().join("\n\n---\n\n");
                let messages = vec![Message::user(joined)];
                let tokens = messages
                    .iter()
                    .map(AgentContext::estimate_tokens_static)
                    .sum();
                (scope, messages, tokens)
            }
        }
    }

    /// Recalculate `token_count` after a compaction swap (brain + messages).
    fn recount_tokens_after_compaction(&mut self) {
        self.token_count = 0;
        if let Some(brain) = &self.system_brain {
            self.token_count += Self::estimate_tokens(brain);
        }
        for msg in &self.messages {
            self.token_count += self.estimate_message_tokens(msg);
        }
    }

    /// Compact the context by replacing old messages with a summary.
    ///
    /// Keeps the most recent messages that fit within the token budget
    /// and prepends a summary of everything that was trimmed.
    /// `keep_token_budget` is the max tokens for kept messages (excluding the summary).
    /// Returns the exact marker text it pushed into the context (banner +
    /// summary). A caller that persists a compaction row MUST write THIS
    /// string rather than rebuild a banner of its own: two construction sites
    /// drifting apart is #1928 — the in-memory apply wrote a delta segment's
    /// sentinel banner that the DB row never carried, so the loader counted
    /// every segment as a fresh boundary and kept only the newest.
    pub fn compact_with_summary(&mut self, summary: String, keep_token_budget: usize) -> String {
        // Walk backwards from end, keeping messages until we hit the budget
        let summary_tokens = Self::estimate_tokens(&summary) + 50; // +50 for the marker text
        let available = keep_token_budget.saturating_sub(summary_tokens);
        let mut running = 0usize;
        let mut keep_count = 0usize;
        for msg in self.messages.iter().rev() {
            let t = self.estimate_message_tokens(msg);
            if running + t > available {
                break;
            }
            running += t;
            keep_count += 1;
        }
        // Caller can request a clean compaction (only the summary survives)
        // by passing `keep_token_budget == 0` — in that case we honour zero
        // kept messages. Otherwise keep at least the most recent pair so
        // valid API request structure is preserved.
        if keep_token_budget > 0 {
            keep_count = keep_count.max(2.min(self.messages.len()));
        }
        let mut keep_start = self.messages.len().saturating_sub(keep_count);

        // Advance past any leading orphaned tool_result messages in the kept slice.
        // If the assistant(tool_use) that precedes them is being dropped, they'd be invalid.
        while keep_start < self.messages.len()
            && Self::is_orphaned_tool_result_msg(&self.messages[keep_start])
        {
            keep_start += 1;
        }

        let kept_messages: Vec<Message> = self.messages.drain(keep_start..).collect();

        // Clear all old messages
        self.messages.clear();

        // Prepend the compaction summary as a user message (so the LLM sees the context)
        let marker_text = format!(
            "[CONTEXT COMPACTION — The conversation was automatically compacted. \
             Below is a structured summary of everything before this point.]\n\n{}",
            summary
        );
        let summary_msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: marker_text.clone(),
            }],
        };
        self.messages.push(summary_msg);

        // Re-add kept messages
        self.messages.extend(kept_messages);

        // Recalculate token count
        self.token_count = 0;
        if let Some(brain) = &self.system_brain {
            self.token_count += Self::estimate_tokens(brain);
        }
        for msg in &self.messages {
            self.token_count += self.estimate_message_tokens(msg);
        }

        // Drop the pre-compaction provider anchor: the anchor was taken against
        // the uncompacted prompt and would inflate the post-compaction budget
        // with a stale delta (#211).
        self.provider_anchor = None;
        marker_text
    }
}

/// The canonical compaction-marker prefix. Every marker row — DB or
/// in-memory — starts with this (the #175 anchored-prefix invariant).
pub(crate) const COMPACTION_MARKER_PREFIX: &str = "[CONTEXT COMPACTION";

/// Rides in a DELTA-SEGMENT marker's banner (#1649). Segments extend the
/// window instead of restarting it, so the DB loader skips them when it
/// looks for the reload boundary: it anchors on the LAST marker WITHOUT
/// this sentinel — the full-window, hard-truncate, consolidation, RSI and
/// cron markers all restart history, and only they bound the reload.
/// Inverted on purpose: tagging the one new marker type keeps every legacy
/// marker (and every legacy DB stream) behaving exactly as before.
pub(crate) const SEGMENT_SENTINEL: &str = "DELTA SEGMENT.";

/// What a compaction summarises (#1649).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CompactionScope {
    /// No marker in the window yet — summarise everything (the classic
    /// full-window behaviour, byte-identical prompt and apply).
    FullWindow,
    /// Prior markers exist — summarise ONLY the messages since the last one
    /// and append the result as a new frozen segment.
    DeltaSinceMarker,
    /// The frozen segments alone exceed half the window — consolidate the
    /// segments into one fresh summary that supersedes them.
    SegmentConsolidation,
}
