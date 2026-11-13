use super::builder::AgentService;
use crate::brain::agent::context::{AgentContext, CompactionScope};
use crate::brain::agent::error::{AgentError, Result};
use crate::brain::provider::{ContentBlock, LLMRequest, Message, Provider};
use crate::services::{MessageService, SessionService};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ContextManifest {
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default)]
    pub discard_skills: Vec<String>,
    #[serde(default)]
    pub required_tools: Vec<String>,
}

/// Extract and parse a machine-readable `ContextManifest` from a compaction summary.
///
/// Searches for a fenced code block tagged `context-manifest` (or `context_manifest`),
/// and parses it either as YAML or JSON. Fails soft (returns `None`) if omitted or malformed.
pub fn parse_context_manifest(summary: &str) -> Option<ContextManifest> {
    let block = extract_manifest_block(summary)?;
    parse_manifest_text(&block)
}

/// #1960: does discarding `discarded` unload something the session still holds
/// as `active`?
///
/// True for the same name, and for a parent/child pair in either direction: an
/// auxiliary document `<skill>/<file.md>` is unreachable once the bare `<skill>`
/// is pruned, and pruning the child out from under an active parent is the same
/// self-contradiction one level down. Two unrelated slugs that merely share a
/// text prefix (`dev` vs `dev-tools`) do NOT collide, hence the `/` boundary.
fn skill_names_collide(active: &str, discarded: &str) -> bool {
    // `parent` covers `child` when `child` is one of its auxiliary documents.
    let parent_covers = |parent: &str, child: &str| {
        child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    active == discarded || parent_covers(active, discarded) || parent_covers(discarded, active)
}

fn extract_manifest_block(summary: &str) -> Option<String> {
    let mut in_block = false;
    let mut fence_char = '`';
    let mut fence_len = 3;
    let mut content = String::new();

    for line in summary.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                let f_char = if trimmed.starts_with('`') { '`' } else { '~' };
                let flen = trimmed.chars().take_while(|&c| c == f_char).count();
                let tag = trimmed[flen..].trim().to_lowercase();
                if tag == "context-manifest" || tag == "context_manifest" {
                    in_block = true;
                    fence_char = f_char;
                    fence_len = flen;
                    continue;
                }
            }
        } else {
            let chars_count = trimmed.chars().take_while(|&c| c == fence_char).count();
            if chars_count >= fence_len && trimmed[chars_count..].trim().is_empty() {
                return Some(content);
            }
            content.push_str(line);
            content.push('\n');
        }
    }
    if in_block && !content.trim().is_empty() {
        return Some(content);
    }
    None
}

pub fn parse_manifest_text(text: &str) -> Option<ContextManifest> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(manifest) = serde_json::from_str::<ContextManifest>(trimmed) {
        return Some(manifest);
    }

    let mut manifest = ContextManifest::default();
    let mut current_section: Option<&str> = None;
    let mut found_any_key = false;

    for line in trimmed.lines() {
        let line_trimmed = line.trim();
        if line_trimmed.is_empty() || line_trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = line_trimmed.strip_prefix("active_skills:") {
            current_section = Some("active_skills");
            found_any_key = true;
            let rest_trimmed = rest.trim();
            if !rest_trimmed.is_empty() {
                parse_inline_items(rest_trimmed, &mut manifest.active_skills);
            }
        } else if let Some(rest) = line_trimmed.strip_prefix("discard_skills:") {
            current_section = Some("discard_skills");
            found_any_key = true;
            let rest_trimmed = rest.trim();
            if !rest_trimmed.is_empty() {
                parse_inline_items(rest_trimmed, &mut manifest.discard_skills);
            }
        } else if let Some(rest) = line_trimmed.strip_prefix("required_tools:") {
            current_section = Some("required_tools");
            found_any_key = true;
            let rest_trimmed = rest.trim();
            if !rest_trimmed.is_empty() {
                parse_inline_items(rest_trimmed, &mut manifest.required_tools);
            }
        } else if let Some(item) = line_trimmed.strip_prefix('-') {
            let item_clean = item.trim().trim_matches('"').trim_matches('\'').trim();
            if !item_clean.is_empty() {
                match current_section {
                    Some("active_skills") => manifest.active_skills.push(item_clean.to_string()),
                    Some("discard_skills") => manifest.discard_skills.push(item_clean.to_string()),
                    Some("required_tools") => manifest.required_tools.push(item_clean.to_string()),
                    _ => {}
                }
            }
        }
    }

    if found_any_key { Some(manifest) } else { None }
}

fn parse_inline_items(s: &str, target: &mut Vec<String>) {
    let s = s.trim();
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        for part in inner.split(',') {
            let clean = part.trim().trim_matches('"').trim_matches('\'').trim();
            if !clean.is_empty() {
                target.push(clean.to_string());
            }
        }
    } else {
        let clean = s.trim_matches('"').trim_matches('\'').trim();
        if !clean.is_empty() && clean != "[]" {
            target.push(clean.to_string());
        }
    }
}

/// One `## N.` section of a continuation document, heading and body verbatim.
struct SummarySection {
    /// The heading line as written, e.g. `## 1. Chronological Analysis`.
    heading: String,
    /// Heading + body, byte-for-byte.
    text: String,
    /// True for blocks a woken agent cannot recover from the document alone.
    must_keep: bool,
}

/// The sections a trimmed document must never lose: the obligation (0), the
/// recovery playbook (7), the next step (8) and the manifest (10) — plus the
/// preamble before the first heading, kept rather than guessed at.
fn is_must_keep_section(number: Option<u32>) -> bool {
    matches!(number, None | Some(0) | Some(7) | Some(8) | Some(10))
}

/// Split a continuation document at its `## N.` headings. Lossless: the
/// concatenation of the returned sections is the input, byte for byte.
/// Fence-aware — a `##` line inside a code block is content, not a heading.
fn split_summary_sections(summary: &str) -> Vec<SummarySection> {
    let mut sections: Vec<SummarySection> = Vec::new();
    let mut heading = String::new();
    let mut number: Option<u32> = None;
    let mut cur = String::new();
    let mut open: Option<(char, usize)> = None;

    for line in summary.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let fence = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        let in_fence_before = open.is_some();
        match (open, fence) {
            (None, Some(c)) => {
                open = Some((c, trimmed.chars().take_while(|&x| x == c).count()));
            }
            (Some((c, len)), Some(c2))
                if c == c2 && trimmed.chars().take_while(|&x| x == c).count() >= len =>
            {
                open = None;
            }
            _ => {}
        }

        if !in_fence_before
            && fence.is_none()
            && let Some(n) = parse_section_number(line)
        {
            if !cur.is_empty() {
                sections.push(SummarySection {
                    heading: std::mem::take(&mut heading),
                    text: std::mem::take(&mut cur),
                    must_keep: is_must_keep_section(number),
                });
            }
            number = Some(n);
            heading = line.trim_end().to_string();
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        sections.push(SummarySection {
            heading,
            text: cur,
            must_keep: is_must_keep_section(number),
        });
    }
    sections
}

/// `## 3. Title` → `Some(3)`. Any other line, `### 3.` included, is `None`.
fn parse_section_number(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("## ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('.') {
        return None;
    }
    digits.parse().ok()
}

/// A dropped section's replacement: its heading, so the document still reads
/// as a document, and a pointer to where the content really lives.
fn section_stub(section: &SummarySection) -> String {
    if section.heading.is_empty() {
        return String::new();
    }
    format!(
        "{}\n[section omitted to fit the compaction budget — recover it from the session \
         store with `session_search`]\n\n",
        section.heading
    )
}

/// Remove the largest fenced code block from `text`, replacing it with a
/// one-line pointer. A fence is removed whole, never cut in half. `None`
/// when no fenced block remains.
fn remove_largest_fenced_block(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<(usize, char, usize)> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let fence = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        match (open, fence) {
            (None, Some(c)) => {
                open = Some((i, c, trimmed.chars().take_while(|&x| x == c).count()));
            }
            (Some((start, c, len)), Some(c2))
                if c == c2 && trimmed.chars().take_while(|&x| x == c).count() >= len =>
            {
                blocks.push((start, i + 1));
                open = None;
            }
            _ => {}
        }
    }
    if let Some((start, _, _)) = open {
        blocks.push((start, lines.len()));
    }

    let (start, end) = blocks
        .into_iter()
        .max_by_key(|&(s, e)| lines[s..e].iter().map(|l| l.len()).sum::<usize>())?;

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == start {
            out.push_str(
                "[fenced block omitted to fit the compaction budget — read it from the file \
                 named above]\n",
            );
        } else if i < start || i >= end {
            out.push_str(line);
        }
    }
    Some(out)
}

/// The §0 status line the prompt mandates, matched case-insensitively so a
/// capitalisation slip is not read as a lost obligation.
fn has_obligation_status(summary: &str) -> bool {
    summary.to_lowercase().contains("obligation status")
}

impl AgentService {
    /// The per-turn plan reminder pinned at the end of the prompt, keyed to
    /// the session's plan-mode state: an Active checklist gets the task
    /// nudge (`format_plan_reminder`), an Editing session (pre-init or
    /// design prose) gets the Editing rules (`format_editing_reminder`),
    /// and NoPlan gets nothing. Loaded through the shared plan store so
    /// legacy statuses map (and terminal ones resolve) first.
    /// The user message as the model should see it: the raw text plus the
    /// per-turn plan reminder and any relevant MEMORY.md recall.
    ///
    /// Both additions are context-only. The DB always stores the clean
    /// `user_message`, so neither can pollute chat history, and neither piles
    /// up across turns.
    ///
    /// Shared because the two message paths had drifted (#995): the tool loop
    /// appended plan reminder AND recall, `prepare_message_context` appended
    /// only the reminder, and the two blocks were otherwise identical. Whether
    /// memory surfaced therefore depended on which code path a session took,
    /// which is not a property of the memory.
    /// Augment the user message for LLM context:
    /// 1. Temporal grounding: inject [Current time: YYYY-MM-DD HH:MM:SS UTC (user: ...)] (#153).
    /// 2. Active plan reminder.
    /// 3. Memory recall.
    pub(crate) async fn augment_user_message(
        session_id: Uuid,
        user_message: &str,
        brain_dir: Option<&std::path::Path>,
    ) -> String {
        // Temporal grounding (#153): inject context-only time marker at turn start.
        let now = chrono::Utc::now();
        let tz_info = brain_dir
            .and_then(|dir| crate::brain::timezone::GLOBAL_TZ_CACHE.resolve_from_brain_dir(dir));
        let time_marker = match tz_info {
            Some(ref info) => format!("[Current time: {}]", info.format_dual_time(&now)),
            None => format!(
                "[Current time: {}]",
                crate::brain::timezone::format_utc_time(&now)
            ),
        };

        let mut out = format!("{time_marker}\n\n{user_message}");

        if let Some(reminder) = Self::active_plan_reminder(session_id).await {
            out.push_str("\n\n");
            out.push_str(&reminder);
        }

        // Ride relevant memory along with the message (#799). MEMORY.md was
        // written constantly and read almost never; #800 made reading cheap,
        // but a cheap read still has to be chosen, and the model cannot decide
        // to recall a correction it has forgotten exists.
        // #1957: MEMORY.md is the owner's personal context and this recall
        // rides into transcripts a shared/group session can read, so shared
        // sessions skip it like the search surfaces do.
        if !crate::memory::internal_content_blocked(session_id)
            && let Some(recall) = crate::brain::memory_recall::recall_for(user_message).await
        {
            tracing::info!(
                "Recalled {} chars from MEMORY.md for session {session_id}",
                recall.len()
            );
            out.push_str("\n\n");
            out.push_str(&recall);
        }
        out
    }

    pub(super) async fn active_plan_reminder(session_id: Uuid) -> Option<String> {
        use crate::utils::plan_files::{self, PlanModeState};
        match plan_files::plan_mode_state(session_id).await {
            PlanModeState::NoPlan => None,
            PlanModeState::PreInitEditing => Some(format_editing_reminder(None)),
            PlanModeState::PostInitEditing => {
                let md = plan_files::plan_md_path(session_id).await;
                // Checklist plans have no design document (#1145): don't
                // point the reminder at a file that does not exist.
                Some(if md.exists() {
                    format_editing_reminder(Some(md))
                } else {
                    format_checklist_editing_reminder().to_string()
                })
            }
            PlanModeState::Active => {
                let plan = plan_files::load_plan(session_id).await?;
                format_plan_reminder(&plan)
            }
        }
    }

    /// Helper to prepare message context for LLM requests
    ///
    /// This extracts the common setup logic shared between send_message() and
    /// send_message_streaming() to reduce code duplication.
    pub(super) async fn prepare_message_context(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
    ) -> Result<(String, LLMRequest, MessageService, SessionService)> {
        self.prepare_message_context_with_display(session_id, user_message, None, model)
            .await
    }

    /// Like [`prepare_message_context`](Self::prepare_message_context) but
    /// persists `display_text` (when set) to the DB instead of the full
    /// `user_message`. The LLM context still receives `user_message`, so
    /// turn-scoped scaffolding (reaction guidance, steering prefaces) reaches
    /// the model without polluting session history or future context.
    pub(super) async fn prepare_message_context_with_display(
        &self,
        session_id: Uuid,
        user_message: String,
        display_text: Option<String>,
        model: Option<String>,
    ) -> Result<(String, LLMRequest, MessageService, SessionService)> {
        // Get or create session
        let session_service = SessionService::new(self.context.clone());
        let _session = session_service
            .get_session(session_id)
            .await
            .map_err(AgentError::db)?
            .ok_or(AgentError::SessionNotFound(session_id))?;

        // Load conversation context with budget-aware message trimming
        let message_service = MessageService::new(self.context.clone());
        let all_db_messages = message_service
            .list_messages_for_session(session_id)
            .await
            .map_err(AgentError::db)?;

        let model_name = model.unwrap_or_else(|| {
            self.provider_for_session(session_id)
                .default_model()
                .to_string()
        });
        let context_window = self.context_limit_for_session(session_id);

        // Load from last compaction point — no arbitrary trimming
        let db_messages = Self::messages_from_last_compaction(all_db_messages);

        let mut context =
            AgentContext::from_db_messages(session_id, db_messages, context_window as usize);

        // Add system brain if available (count its tokens for accurate tracking).
        // `live_system_brain` rebuilds from disk when a brain file changed so
        // edits take effect on the next turn without a restart (#213). The
        // session-aware variant patches the Runtime Info Model/Provider lines
        // to the session's resolved pair, not the startup default.
        if let Some(brain) = self.live_system_brain_for_session(session_id) {
            context.token_count += AgentContext::estimate_tokens(&brain);
            context.system_brain = Some(brain);
        }

        // Add user message. If a plan is actively executing, append a compact
        // reminder of its incomplete tasks so it rides at the END of the prompt
        // (best recall) every turn — without it the plan scrolls out of the
        // recency window in a long conversation and the model forgets it was
        // mid-plan (discussion #177). Regenerated each turn from the plan file;
        // the DB only ever stores the clean user message, so it never piles up.
        let brain_dir = self.brain_workspace_path();
        let context_user_message =
            Self::augment_user_message(session_id, &user_message, brain_dir.as_deref()).await;
        let user_msg = Message::user(context_user_message);
        context.add_message(user_msg);

        // Save user message to database. When a display override is set,
        // history records the compact form; the full text was context-only.
        message_service
            .create_message(
                session_id,
                "user".to_string(),
                display_text.unwrap_or(user_message),
            )
            .await
            .map_err(AgentError::db)?;

        // Build base LLM request. The output reservation is bounded by this
        // session's active context window so a 200K route keeps input headroom.
        let request = LLMRequest::new(model_name.clone(), context.messages.clone())
            .with_max_tokens(self.request_max_tokens_for_session(session_id));

        // Surface a small "Recently accessed" anchor section so the
        // agent re-uses real paths from prior sessions / pre-compaction
        // turns instead of hallucinating directory layouts. Filtered
        // against the live messages so we don't double-list paths the
        // agent just touched in this same session.
        let working_directory = self.get_working_directory_for_session(session_id);
        let recent_paths = self.recent_paths_for_dir(&working_directory).await;
        let augmented_system = Self::augment_system_with_recent_paths(
            context.system_brain,
            &recent_paths,
            &context.messages,
        );

        let mut request = if let Some(system) = augmented_system {
            request.with_system(system)
        } else {
            request
        };

        // Pass working directory so proxy-aware providers can forward it
        request.working_directory = Some(working_directory.to_string_lossy().to_string());
        request.session_id = Some(session_id);

        Ok((model_name, request, message_service, session_service))
    }

    /// Append a "Recently accessed" anchor section to `system_brain`,
    /// listing only the paths from the persistent recent-paths store
    /// that don't already appear verbatim in any of the live messages.
    /// Returns `base` unchanged when there's nothing to surface — keeps
    /// the prompt clean during normal uncompacted runs where the literal
    /// tool_call/tool_result blocks already mention the path.
    pub(crate) fn augment_system_with_recent_paths(
        base: Option<String>,
        recent_paths: &[String],
        messages: &[Message],
    ) -> Option<String> {
        if recent_paths.is_empty() {
            return base;
        }
        let context_blob: String = messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolUse { input, .. } => Some(input.to_string()),
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let context_for_match = context_blob.to_lowercase();

        let surviving: Vec<&String> = recent_paths
            .iter()
            .filter(|p| !context_for_match.contains(&p.to_lowercase()))
            .collect();
        if surviving.is_empty() {
            return base;
        }
        let mut out = base.unwrap_or_default();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(
            "\n--- Recently accessed in this project ---\n\
             (Real paths previously confirmed by read/edit/grep/ls. Prefer these as anchors \
             over guessing from naming conventions.)\n",
        );
        for p in surviving {
            out.push_str("  - ");
            out.push_str(p);
            out.push('\n');
        }
        Some(out)
    }

    /// Load messages from the compaction boundary forward.
    ///
    /// #1649 frozen segments: delta-segment markers EXTEND the window (the
    /// earlier segments stay in force), so they are not boundaries. The
    /// boundary is the LAST marker whose banner does NOT carry the delta
    /// sentinel — a full-window compaction, a hard truncate, a consolidation,
    /// the RSI seal, a cron boundary or a user clear, all of which restart
    /// history. The sentinel is matched ANCHORED at the row start, never as a
    /// substring (#1928): a summary whose BODY quotes the sentinel is still a
    /// boundary, or the reload would skip a real restart and anchor on the
    /// newest segment instead.
    /// Result: `[boundary][segments…][tail]` reconstructs the live window
    /// exactly; a session with no segment markers reloads byte-identically
    /// to the classic last-marker behaviour.
    ///
    /// Anchored on `role == "user" && starts_with(..)`, NOT a substring match
    /// (#175). Every marker is written as a `user` row whose content BEGINS
    /// with the prefix: `CompactionOutcome::marker` (compaction.rs:43),
    /// `AgentContext::hard_truncate_to` (context.rs:346), the tool-loop
    /// persist path (tool_loop.rs:3791), the RSI cycle seal (rsi.rs:1172) and
    /// the cron run boundary (`CRON_RUN_BOUNDARY`, written at run start by
    /// `open_run_boundary` in `cron/scheduler.rs`). A plain `contains` instead
    /// re-anchored the window on any message that merely QUOTED the prefix —
    /// an assistant row echoing the banner, or a lane reading it out of a log
    /// or a compaction summary. Audit over four live DBs: 1813 `user` marker
    /// rows, all 1813 starting with the prefix; 243 `assistant` rows
    /// containing it, none of them a marker.
    pub fn messages_from_last_compaction(
        all_messages: Vec<crate::db::models::Message>,
    ) -> Vec<crate::db::models::Message> {
        use crate::brain::agent::context::{COMPACTION_MARKER_PREFIX, SEGMENT_SENTINEL};

        // The DELTA banner is the sentinel ANCHORED at the row start, not
        // merely present anywhere in it (#1928). A full-window summary whose
        // body quotes "DELTA SEGMENT." must still count as a boundary; a bare
        // `contains` skipped it, so the reload anchored on the newest segment
        // instead of the restart.
        let delta_banner = format!("{COMPACTION_MARKER_PREFIX} — {SEGMENT_SENTINEL}");

        // Walk backward to the last marker that RESTARTS history. Segment
        // markers (delta-bannered, #175 anchored prefix) are skipped: the
        // boundary they extend from is what the reload anchors on.
        let boundary_idx = all_messages.iter().rposition(|msg| {
            msg.role == "user"
                && msg.content.starts_with(COMPACTION_MARKER_PREFIX)
                && !msg.content.starts_with(&delta_banner)
        });

        if let Some(idx) = boundary_idx {
            let kept = all_messages.len() - idx;
            tracing::info!(
                "Found compaction boundary at message {}/{} — loading {} messages \
                 (boundary + segments + tail)",
                idx,
                all_messages.len(),
                kept,
            );
            let mut loaded = all_messages[idx..].to_vec();
            // Every summary in the window is replayed on the way in, not just
            // the boundary: the delta segments carry the same execution
            // sections, written for their own moment (#2883).
            let now = chrono::Utc::now();
            for msg in loaded.iter_mut() {
                if msg.role == "user" && msg.content.starts_with(COMPACTION_MARKER_PREFIX) {
                    msg.content = reinjectable_summary(&msg.content, msg.created_at, now);
                }
            }
            loaded
        } else {
            all_messages
        }
    }

    /// Synchronous compaction: compute a summary and apply it to `context` in place.
    /// Used by the manual `/compact` command and the two emergency callsites that
    /// recover from "context too large" provider errors. The async path used by
    /// `enforce_context_budget` does NOT go through here — it spawns
    /// `compute_compaction_summary` directly and applies the result via
    /// `apply_compaction_summary` once the LLM call finishes.
    pub(super) async fn compact_context(
        &self,
        session_id: Uuid,
        context: &mut AgentContext,
        model_name: &str,
        cancel_token: Option<&CancellationToken>,
        notifier: Option<super::compaction_notice::CompactionNotifier>,
    ) -> Result<(String, String)> {
        // This call replaces the whole context, so a background summariser
        // still describing the pre-call conversation is describing something
        // that will not exist by the time it returns. Applying its result
        // later would overwrite the compaction happening right here.
        if let Some(superseded) = self.take_pending_compaction(session_id) {
            tracing::info!("Background compaction superseded by a synchronous one — aborting it");
            superseded.abort();
        }

        let provider = self.provider_for_session(session_id);
        let cancel = cancel_token.cloned().unwrap_or_default();

        let summary = Self::compute_compaction_summary(
            provider,
            self.fallback_chain_snapshot(),
            session_id,
            // Manual `/compact` and emergency recovery are full-window by
            // definition: the caller wants everything re-derived into one
            // marker now (#1649 scoping only applies to the background path).
            CompactionScope::FullWindow,
            context.messages.clone(),
            context.token_count,
            context.max_tokens,
            context.usage_percentage(),
            model_name.to_string(),
            super::request_budget::compaction_summary_request_allowance(),
            self.get_working_directory_for_session(session_id),
            self.auto_approve_tools.load(Ordering::Relaxed),
            self.skill_inventory_for_session(session_id),
            cancel,
            self.compaction_attempt_deadline(session_id),
            notifier,
        )
        .await?;

        let summary =
            Self::decorate_compaction_summary(summary, session_id, self.subagent_manager.clone())
                .await;

        let applied_marker = Self::apply_compaction_summary(context, &summary);
        Ok((summary, applied_marker))
    }

    /// Attach the state a model's prose summary cannot be trusted to carry.
    ///
    /// Both blocks are harness-written so they survive a summary that forgot
    /// them, and both ride INSIDE the persisted marker rather than arriving as
    /// separate messages that can scroll away:
    ///
    /// - session plan artifacts, so the post-compaction agent resumes the plan
    ///   instead of rediscovering it;
    /// - live sub-agent IDs, so `wait_agent` / `send_input` / `resume_agent` /
    ///   `close_agent` still have something to address (#936).
    ///
    /// Owns no `&self` and takes the manager by value: the background
    /// summariser calls it from a spawned task with nothing but a snapshot.
    pub(super) async fn decorate_compaction_summary(
        summary: String,
        session_id: Uuid,
        subagents: Option<Arc<crate::brain::tools::subagent::SubAgentManager>>,
    ) -> String {
        let summary = match plan_state_block(session_id).await {
            Some(block) => format!("{summary}\n\n{block}"),
            None => summary,
        };
        match subagents.and_then(|m| m.format_running_for_compaction()) {
            Some(block) => format!("{summary}\n\n{block}"),
            None => summary,
        }
    }

    /// Send the summariser request, walking `[providers.fallback]` when the
    /// session's provider fails (#1247).
    ///
    /// Compaction used to call `provider.complete()` once and surface whatever
    /// came back. Every other request path in the process walks the chain, so
    /// a session whose primary was rate-limited or out of credit kept chatting
    /// happily via a fallback while `/compact` died on the dead primary — and
    /// with the context window full, a session that cannot compact cannot
    /// recover at all.
    ///
    /// Mirrors the tool loop's walk deliberately: skip the primary's own name,
    /// try every remaining entry in configured order (#1251 — no provider is
    /// ever dropped from the walk for having failed before), remap the model
    /// to each fallback's default when it doesn't carry the requested one,
    /// and report the whole ledger if everything dies.
    /// `pub(crate)` for the regression tests in `src/tests` — no caller outside
    /// Bound on a single summariser attempt for a session with no compaction
    /// history to scale from. Not a guess: it is the 300s every HTTP provider
    /// in this codebase already enforces per request
    /// (`anthropic.rs::DEFAULT_TIMEOUT`), extended to the CLI providers that
    /// ship no timeout at all.
    pub(crate) const COMPACTION_ATTEMPT_FLOOR: std::time::Duration =
        std::time::Duration::from_secs(300);

    /// One summariser attempt, bounded.
    ///
    /// HTTP providers already cap a single request at
    /// `anthropic.rs::DEFAULT_TIMEOUT` (300s). CLI providers carry no timeout
    /// at all, so a summariser that stopped answering held the session for as
    /// long as the process felt like living, and the fallback chain below was
    /// never reached because the first attempt never returned (#1255). This
    /// extends the bound every HTTP provider already honours to the ones that
    /// do not, and a provider that blows it is handed on rather than waited
    /// out: `Timeout` is retryable, so `should_try_next_provider` walks.
    ///
    /// HTTP providers are asked through `stream()` and the stream is folded
    /// into one response by [`super::compaction_stream::collect_stream`]
    /// (#1519). The non-streaming request held the gateway's connection open
    /// until the whole summary existed, and a summary of a full window does
    /// not finish inside a gateway idle limit of about two minutes, so the
    /// request came back 504 on every retry while chat turns on the same
    /// model streamed fine. CLI providers keep `complete()`: there is no
    /// gateway in front of a subprocess, and their stream path is the one
    /// that runs tools.
    async fn compaction_attempt(
        provider: &Arc<dyn Provider>,
        request: LLMRequest,
        deadline: std::time::Duration,
    ) -> std::result::Result<
        crate::brain::provider::LLMResponse,
        crate::brain::provider::ProviderError,
    > {
        let attempt = async {
            if provider.cli_handles_tools() {
                provider.complete(request).await
            } else {
                let stream = provider.stream(request).await?;
                super::compaction_stream::collect_stream(stream).await
            }
        };
        match tokio::time::timeout(deadline, attempt).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    "Compaction: provider '{}' produced nothing in {:?} — moving on",
                    provider.name(),
                    deadline,
                );
                Err(crate::brain::provider::ProviderError::Timeout(
                    deadline.as_secs(),
                ))
            }
        }
    }

    /// compaction should send a summariser request.
    pub(crate) async fn complete_compaction_request(
        primary: &Arc<dyn Provider>,
        fallbacks: &[Arc<dyn Provider>],
        request: LLMRequest,
        cancel: &CancellationToken,
        attempt_deadline: std::time::Duration,
        notifier: Option<&super::compaction_notice::CompactionNotifier>,
    ) -> Result<crate::brain::provider::LLMResponse> {
        use super::compaction_notice::CompactionStep;
        use crate::brain::provider::error as provider_error;

        let primary_name = primary.name().to_string();
        let first_err = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::info!("Compaction cancelled before completion");
                return Err(AgentError::Cancelled);
            }
            r = Self::compaction_attempt(primary, request.clone(), attempt_deadline) => match r {
                Ok(response) => return Ok(response),
                Err(e) => e,
            },
        };

        if fallbacks.is_empty() || !provider_error::should_try_next_provider(&first_err) {
            if let Some(n) = notifier {
                n.step(CompactionStep::Failed {
                    detail: format!(
                        "'{}': {}",
                        primary_name,
                        provider_error::short_error_reason(&first_err)
                    ),
                });
            }
            return Err(AgentError::Provider(first_err));
        }

        tracing::warn!(
            "Compaction: primary '{}' failed ({}) — walking fallback chain",
            primary_name,
            provider_error::short_error_reason(&first_err),
        );
        if let Some(n) = notifier {
            n.step(CompactionStep::AttemptFailed {
                provider: primary_name.clone(),
                reason: provider_error::short_error_reason(&first_err),
            });
        }

        // Size is the one failure where the order matters: a narrower window
        // than the one that just refused is a refusal spent before the
        // answer, so an overflow walks widest first (#1379). Everything else
        // keeps the configured order.
        let ordered: Vec<Arc<dyn Provider>> = if matches!(
            first_err,
            crate::brain::provider::ProviderError::ContextLengthExceeded(_)
        ) {
            crate::brain::provider::chain_order::widest_first(fallbacks)
        } else {
            fallbacks.to_vec()
        };

        let mut tried: Vec<String> = Vec::new();
        let mut last_err = first_err;

        for fallback in &ordered {
            let name = fallback.name().to_string();
            if name == primary_name {
                continue;
            }
            // A substitute runs its own configured model, not the one the
            // failed request carried; the same rule the chat chain applies,
            // through the same function (#1374).
            let fb_request =
                crate::brain::provider::fallback::substitute_request(fallback.as_ref(), &request);
            tracing::info!(
                "Compaction: trying fallback provider '{}' (model '{}')",
                name,
                fb_request.model
            );
            if let Some(n) = notifier {
                n.step(CompactionStep::TryingFallback {
                    provider: name.clone(),
                    model: fb_request.model.clone(),
                });
            }

            let err = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::info!("Compaction cancelled while walking fallback chain");
                    return Err(AgentError::Cancelled);
                }
                r = Self::compaction_attempt(fallback, fb_request, attempt_deadline) => match r {
                    Ok(response) => {
                        tracing::info!("Compaction served by fallback '{}'", name);
                        return Ok(response);
                    }
                    Err(e) => e,
                },
            };

            tried.push(format!(
                "{}: {}",
                name,
                provider_error::short_error_reason(&err)
            ));
            if let Some(n) = notifier {
                n.step(CompactionStep::AttemptFailed {
                    provider: name.clone(),
                    reason: provider_error::short_error_reason(&err),
                });
            }
            last_err = err;
        }

        let summary = provider_error::chain_exhausted_summary(
            &primary_name,
            &provider_error::short_error_reason(&last_err),
            &tried,
        );
        tracing::error!("Compaction: fallback chain exhausted: {summary}");
        if let Some(n) = notifier {
            n.step(CompactionStep::Failed {
                detail: summary.clone(),
            });
        }
        Err(AgentError::Provider(provider_error::with_chain_summary(
            last_err, summary,
        )))
    }

    /// #1930: the summariser's system prompt. Dense knowledge transfer under a
    /// hard budget — the document is trimmed mechanically if it overruns, so
    /// the instruction is completeness of FACTS, not volume of prose.
    pub(crate) fn compaction_system_prompt() -> String {
        "You are a continuation document generator. Your job is to create a dense, \
         complete knowledge transfer document from a conversation so that a fresh AI agent can \
         continue the work seamlessly. You must capture every file path, command, identifier, \
         user preference, error and pending task. The agent reading your output will have ZERO \
         prior context — your document is its entire memory. \
         The document has a hard budget, stated in the user message, and is trimmed \
         mechanically when it overruns — so write densely: every load-bearing fact, no filler, \
         and name files and line numbers instead of pasting long code blocks. \
         Missing a single fact could cause the agent to repeat mistakes or violate user preferences."
            .to_string()
    }

    /// #1930: the closing directive of the continuation-document prompt — the
    /// hard budget and the order in which it is spent.
    ///
    /// The prompt used to close by ordering maximum verbosity, with no stated
    /// size and nothing trimming the result: measured 2026-10-04 that produced a
    /// 27.8 KB mean (max 69.2 KB) against a 3 000-token budget. Retention is
    /// ordered so the blocks a woken agent cannot work without (obligation
    /// status, the `context-manifest` fence, the next step, the Recovery
    /// Playbook) are named MUST, and the cheapest content to drop (long
    /// snippets, long quotes) is named FIRST TO GO.
    pub(crate) fn compaction_budget_directive(summary_budget_tokens: u32) -> String {
        format!(
            "BUDGET: the whole document MUST fit ~{summary_budget_tokens} tokens. A document \
             that overruns is trimmed mechanically and loses whatever was written last, so spend \
             the budget deliberately, in this order:\n\
             1. MUST be complete — section 0 (obligation status + next action), the \
             `context-manifest` fence in section 10, section 8 (next step), section 7 (recovery \
             playbook).\n\
             2. THEN exact paths, commands, identifiers and error strings — cheap and load-bearing.\n\
             3. THEN sections 1-6 compressed to the shortest form that still carries the facts.\n\
             4. FIRST TO GO when the budget is tight — long code snippets (name the file and line \
             instead of pasting the block) and long quotes (keep only the decisive phrase).\n\
             This is not a summary — it is a complete knowledge transfer, and the fresh agent has \
             ZERO context beyond what you write here. Completeness means every load-bearing FACT \
             survives, not every sentence."
        )
    }

    /// #1960: the live skill set stamped into the summariser's §10 manifest
    /// instructions.
    ///
    /// The compactor authors `active_skills` / `discard_skills` from conversation
    /// memory alone: nothing in its input says which skills the session holds
    /// RIGHT NOW, so it can prune a load-bearing one and revoke the #219
    /// re-injection with a well-formed fence and no warning. The harness owns that
    /// registry (the same inventory `append_skill_stamp` reads), so the names go
    /// in as measured fact instead of guesswork. An empty inventory stamps nothing:
    /// zero marginal tokens for sessions that never touched a skill.
    pub(crate) fn compaction_active_skill_stamp(active_skills: &[String]) -> String {
        if active_skills.is_empty() {
            return String::new();
        }
        let mut names: Vec<&str> = active_skills.iter().map(|s| s.as_str()).collect();
        names.sort();
        names.dedup();
        let list = names
            .iter()
            .map(|n| format!("- `{n}`"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "### LIVE SKILL SET (harness-measured at compaction time, not your guess):\n\
             {list}\n\
             Every name above MUST be listed under `active_skills` and MUST NOT appear under \
             `discard_skills`. If you believe one is spent, keep it anyway: pruning it unloads a \
             body the next turn cannot recover, and its tools get refused by the skill gate.\n\n"
        )
    }

    /// #1649: the scope header prepended to the summariser prompt. Empty for
    /// `FullWindow` — the first-compaction prompt stays byte-identical to the
    /// classic one (the parity probe pins this). Delta and consolidation get
    /// a short prelude that re-aims the same exhaustive body at their
    /// narrower input instead of duplicating the whole prompt per scope.
    pub(crate) fn compaction_scope_prelude(scope: CompactionScope) -> String {
        match scope {
            CompactionScope::FullWindow => String::new(),
            CompactionScope::DeltaSinceMarker => concat!(
                "SCOPE: The messages below are ONLY the conversation since the last ",
                "compaction marker. Earlier history is already preserved in frozen ",
                "summary segments that are NOT part of this input — do not restate, ",
                "re-derive, or merge them; document only what is present below.\n\n"
            )
            .to_string(),
            CompactionScope::SegmentConsolidation => concat!(
                "SCOPE: The input below is a set of prior compaction segment summaries ",
                "separated by --- markers. Merge them into ONE comprehensive continuation ",
                "document that supersedes them all. RE-DERIVE section 0 (IMMEDIATE TASK) ",
                "from the evidence in these segments — never copy it forward verbatim ",
                "from the most recent one. Set its obligation status to OPEN, DONE or ",
                "UNKNOWN according to whether the work it names is still owed at wake-up; ",
                "an obligation already completed must NOT be presented as the live task. ",
                "Do not invent content that is not in the segments.\n\n"
            )
            .to_string(),
        }
    }

    /// Compute a compaction summary from a snapshot of messages.
    ///
    /// This is the LLM-facing half of compaction. It does not touch any live
    /// session state — it operates entirely on the cloned snapshot — so it
    /// is safe to call from a background `tokio::spawn` task while the agent
    /// keeps appending new messages to the live context.
    ///
    /// Returns the raw summary text. Callers that want to apply it to a live
    /// context should call `apply_compaction_summary` once the future resolves.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn compute_compaction_summary(
        provider: Arc<dyn Provider>,
        fallbacks: Vec<Arc<dyn Provider>>,
        session_id: Uuid,
        scope: CompactionScope,
        snapshot_messages: Vec<Message>,
        snapshot_token_count: usize,
        snapshot_max_tokens: usize,
        snapshot_usage_pct: f64,
        model_name: String,
        max_output_tokens: u32,
        working_directory: PathBuf,
        auto_approve_tools: bool,
        active_skills: Vec<String>,
        cancel: CancellationToken,
        attempt_deadline: std::time::Duration,
        notifier: Option<super::compaction_notice::CompactionNotifier>,
    ) -> Result<String> {
        let remaining_budget = snapshot_max_tokens.saturating_sub(snapshot_token_count);

        // Skip any leading user messages that consist only of ToolResult blocks —
        // they are orphaned (their tool_use was removed by a prior trim) and would
        // cause the API to reject the request with a 400.
        let start = snapshot_messages
            .iter()
            .position(|m| {
                !(m.role == crate::brain::provider::Role::User
                    && !m.content.is_empty()
                    && m.content.iter().all(|b| {
                        matches!(b, crate::brain::provider::ContentBlock::ToolResult { .. })
                    }))
            })
            .unwrap_or(snapshot_messages.len());

        // Reserve room for the summariser's OUTPUT budget + prompt headroom.
        // Both derive from the SAME constant as the request's
        // `max_output_tokens` (#1930): a reserve that disagrees with the
        // allowance hands the summariser more input than the window can hold.
        let output_reserve = super::request_budget::compaction_summary_input_reserve();
        let max_input_budget = snapshot_max_tokens.saturating_sub(output_reserve);
        let all_msgs = &snapshot_messages[start..];
        let mut running_tokens = 0usize;
        let msgs_to_include: Vec<&Message> = all_msgs
            .iter()
            .rev()
            .take_while(|m| {
                let t = AgentContext::estimate_tokens_static(m);
                if running_tokens + t <= max_input_budget {
                    running_tokens += t;
                    true
                } else {
                    tracing::warn!(
                        "Compaction: dropping oldest messages to fit input budget ({}/{} tokens used)",
                        running_tokens,
                        max_input_budget,
                    );
                    false
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        tracing::info!(
            "Compaction: sending {} / {} messages to summarizer ({} / {} input tokens, reserving {} for output)",
            msgs_to_include.len(),
            all_msgs.len(),
            running_tokens,
            snapshot_max_tokens,
            output_reserve,
        );

        let mut summary_messages: Vec<Message> = msgs_to_include.into_iter().cloned().collect();

        let base_prompt = format!(
            "CRITICAL: The context window is at {:.0}% capacity ({} / {} tokens, {} tokens remaining). \
             The conversation must be compacted NOW.\n\n\
             You are creating a COMPREHENSIVE CONTINUATION DOCUMENT. After compaction, a fresh agent \
             instance will wake up with ONLY this summary as context. It must be able to continue \
             working immediately without asking the user what to do.\n\n\
             Analyze the ENTIRE conversation chronologically and produce the following.\n\n\
             OUTPUT ORDER — CRITICAL: write the sections below in THIS order, NOT in numeric \
             order. The document can be CUT at the tail (the summariser may hit its output \
             limit) and a cut removes whatever was written LAST — so the load-bearing blocks \
             go FIRST: section 0 (obligation) -> the section 10 `context-manifest` fence -> \
             section 7 (recovery playbook) -> section 8 (next step) -> sections 1-6 (the prose, \
             compressed) -> section 9 (continuation message) LAST. Never place the manifest \
             fence below the prose.\n\n\
             ## 0. IMMEDIATE TASK (CRITICAL — MOST IMPORTANT SECTION)\n\
             Look at the LAST 6-8 message pairs in the conversation. Extract EXACTLY:\n\
             - What was the user's LAST instruction or request? (quote their exact words)\n\
             - What was the agent doing in response? (exact tool calls, file edits, investigations in progress)\n\
             - What is the EXACT next action the agent should take?\n\n\
             FIRST LINE OF THIS SECTION, always, before any prose:\n\
             **Obligation status: OPEN** or **Obligation status: DONE** or **Obligation status: UNKNOWN**\n\
             Determine it from the completion evidence you record in section 1 below \
             (\"Whether it was completed, committed, pushed, or still pending\") — do NOT \
             guess, and do NOT carry a status forward from an earlier summary:\n\
             - OPEN — the work is still owed. Write the DIRECTIVE exactly as specified below.\n\
             - DONE — the work is already complete. Say so in one line and name the NEXT \
             actual work instead. Do NOT restate the completed obligation as a directive.\n\
             - UNKNOWN — you cannot tell from the conversation. State what must be checked \
             before acting. Do NOT phrase it as a directive to continue.\n\n\
             Write this as a DIRECTIVE, not a description. Use this format:\n\
             \"CONTINUE THIS TASK: The user asked you to [exact instruction]. \
             You were [exact action in progress — e.g. 'editing file X at line Y', 'running command Z']. \
             Your next step is [specific next action]. \
             Do NOT deviate to any other topic.\"\n\n\
             This is the MOST IMPORTANT section. If nothing else survives compaction, this must. \
             The fresh agent will read this section FIRST and act on it IMMEDIATELY.\n\n\
             ## 10. Context Manifest (MANDATORY MACHINE-READABLE BLOCK)\n\
             Write this block EARLY — immediately after section 0, ahead of all prose — NOT at the end: \
             the document can be CUT at the tail, so this block must survive any cut. You MUST \
             include a fenced YAML block labeled \
             ` ```context-manifest `.\n\
             This manifest directly instructs the harness which skills to keep active vs discard, \
             and which lazy tools to pre-activate for turn 1.\n\n\
             ### Manifest Rules:\n\
             - `active_skills`: Skills that MUST remain active for pending work / ongoing tasks. \
             You may specify the bare `<skill-slug>` (e.g. `opencrabs-dev`) or a specific in-skill document \
             `<skill-slug>/<file.md>` (e.g. `opencrabs-dev/editor.md`, `opencrabs-dev/fleet-directives.md`) \
             if only specific auxiliary procedures are needed.\n\
             - `discard_skills`: Skills or specific auxiliary documents whose tasks are complete and should be pruned to save budget.\n\
             - NEVER discard an ACTIVE skill (#1960). Anything under `active_skills`, or named in the \
             LIVE SKILL SET below, is load-bearing right now: discarding it unloads the skill body at \
             wake-up and silently revokes the re-injection the harness already guarantees. Discard a \
             skill only once the task it serves is finished. And never discard the bare `<skill>` while \
             keeping `<skill>/<file.md>`: the auxiliary document cannot load without its parent.\n\
             - `required_tools`: Extended lazy tools (e.g. telegram_send, browser_navigate, cron_manage, pg_query) \
             that the agent will need immediately on turn 1.\n\
             {active_skill_stamp}\
             Format as YAML:\n\
             ```context-manifest\n\
             active_skills:\n\
               - <skill-slug>\n\
               - <skill-slug>/<file.md>\n\
             discard_skills:\n\
               - <skill-slug>\n\
             required_tools:\n\
               - <tool-name>\n\
             ```\n\n\
             ## 7. Recovery Playbook\n\
             The fresh agent has these tools available to recover any missing context:\n\
             - `session_search` — search past conversation messages in this session by keyword\n\
             - `memory_search` — search daily memory logs and indexed knowledge\n\
             - `load_brain_file` — reload brain files (SOUL.md, TOOLS.md, USER.md, etc.) for identity/preferences\n\
             - `read_file` / `glob` / `grep` — read any file, search by pattern, search file contents\n\
             - `bash` — run shell commands (git status, git log, git diff, etc.)\n\
             - `ls` — list directory contents\n\
             - `gh` — GitHub CLI for ALL GitHub operations (repos, releases, issues, PRs). \
             NEVER use HTTP requests to GitHub — always use `gh` CLI.\n\n\
             Write a SPECIFIC recovery plan: which tools to call with which arguments to get back \
             up to speed. Example: \"Run `git status` and `git diff` to see uncommitted changes, \
             then `read_file src/main.rs` to verify the current state of the fix, then \
             `session_search 'vision fallback'` to recover details from the investigation.\"\n\
             Be concrete — include actual file paths, search queries, and commands.\n\n\
             ## 8. Next Step\n\
             State the single most important thing the agent should do when it wakes up. \
             If the task is clear, continue immediately. If ambiguous, ask the user ONE focused \
             follow-up question.\n\n\
             ## 1. Chronological Analysis\n\
             Walk through every task the user requested, in order. For each task include:\n\
             - What was requested\n\
             - What was done (with exact file paths and line numbers where relevant)\n\
             - Exact code snippets for any changes made (show before/after when applicable)\n\
             - Whether it was completed, committed, pushed, or still pending\n\n\
             ## 2. Files Modified\n\
             List EVERY file that was created, edited, read, or discussed. For each file include:\n\
             - Full file path\n\
             - What was changed and why\n\
             - Key code snippets showing the current state of changes\n\
             - Whether the change is committed or uncommitted\n\n\
             ## 3. User Preferences & Constraints\n\
             List EVERY preference, constraint, or strong reaction from the user. Include:\n\
             - Things the user explicitly said to NEVER do (with their exact words if they were emphatic)\n\
             - Workflow preferences (commit style, release process, tool choices)\n\
             - Technical constraints or architectural decisions\n\
             - Any corrections the user made to your work\n\n\
             ## 4. Errors & Corrections\n\
             Every error encountered, every mistake made, and how each was resolved. Include:\n\
             - Exact error messages when available\n\
             - What caused the error\n\
             - The fix applied\n\
             - User reactions to mistakes (so the agent avoids repeating them)\n\n\
             ## 5. All User Messages\n\
             Summarize every user message in order, capturing their intent and exact wording \
             for important instructions. This is critical for understanding the user's communication \
             style and expectations.\n\n\
             ## 6. Pending Tasks\n\
             List everything that is NOT yet done:\n\
             - Uncommitted changes\n\
             - Tasks mentioned but not started\n\
             - Investigations in progress\n\
             - Next steps the user expects\n\n\
             ## 9. Continuation Message\n\
             Write a SHORT, punchy message (2-4 sentences) that the agent will say to the user \
             right after waking up from compaction. This message MUST:\n\
             - Reference SPECIFIC things from the conversation (file names, user quotes, inside jokes, \
             frustrations, wins) — prove the agent remembers everything\n\
             - Mention what was just accomplished and what's next in a way that feels alive and engaged\n\
             - Match the user's energy and communication style from the conversation\n\
             - Be creative, surprising, maybe funny — make the user think \"holy shit it remembers\"\n\
             - End with a clear action: what the agent is about to do next or a specific question\n\
             DO NOT be generic. DO NOT say \"I'm ready to continue.\" Reference actual conversation details \
             that only someone who was there would know.\n\n\
             Tool approval status: {}\n\n\
             {budget_directive}",
            snapshot_usage_pct,
            snapshot_token_count,
            snapshot_max_tokens,
            remaining_budget,
            if auto_approve_tools {
                "AUTO-APPROVE ON (tools run freely)"
            } else {
                "AUTO-APPROVE OFF — tool approval is REQUIRED for every tool call"
            },
            budget_directive = Self::compaction_budget_directive(
                super::request_budget::COMPACTION_SUMMARY_MAX_TOKENS,
            ),
            active_skill_stamp = Self::compaction_active_skill_stamp(&active_skills),
        );

        // #1649: the scope prelude rides in front of the unchanged body;
        // FullWindow gets an empty prelude, so the first-compaction prompt is
        // byte-identical to the classic one.
        let compaction_prompt = format!("{}{}", Self::compaction_scope_prelude(scope), base_prompt);
        summary_messages.push(Message::user(compaction_prompt));

        // Never send a {provider, model} pair the user didn't configure.
        // If the requested model isn't supported by this provider, remap to
        // the provider's own default — same invariant `stream_complete` enforces.
        let mut effective_model = model_name;
        let supported = provider.supported_models();
        if !supported.is_empty() && !supported.iter().any(|m| m == &effective_model) {
            let remapped = provider.default_model().to_string();
            tracing::warn!(
                "compute_compaction_summary: provider '{}' does not support model '{}' — remapping to '{}'",
                provider.name(),
                effective_model,
                remapped,
            );
            effective_model = remapped;
        }

        let mut request = LLMRequest::new(effective_model, summary_messages)
            .with_max_tokens(max_output_tokens)
            .with_system(Self::compaction_system_prompt());
        request.working_directory = Some(working_directory.to_string_lossy().to_string());
        request.session_id = Some(session_id);

        // The summariser's output never reaches a UI: the stream is folded
        // into one response inside `compaction_attempt`, no progress callback
        // is attached (#1519). `cancel` aborts the request mid-flight if the
        // caller signals (e.g. 90% hard-truncate firing on the same session).
        let response = Self::complete_compaction_request(
            &provider,
            &fallbacks,
            request,
            &cancel,
            attempt_deadline,
            notifier.as_ref(),
        )
        .await?;

        // #1933: a `MaxTokens` stop means the request hit its output allowance
        // and the document is CUT, whatever its token count reads. The trim
        // guard cannot see this — a capped document sits exactly at the budget,
        // which the guard's `before > budget` test never inspects — so the
        // response's own verdict is read here, at the source.
        Self::warn_if_summary_truncated(response.stop_reason.clone(), &response.usage);

        let summary = Self::extract_text_from_response(&response);

        if let Err(e) = Self::save_compaction_summary_to_memory(session_id, &summary).await {
            tracing::warn!("Failed to save compaction summary to daily log: {}", e);
        }

        // Index the updated memory file in the background so memory_search picks it up.
        let memory_path = crate::config::opencrabs_home()
            .join("memory")
            .join(format!("{}.md", chrono::Local::now().format("%Y-%m-%d")));
        tokio::spawn(async move {
            match crate::memory::get_store() {
                Ok(store) => {
                    if let Err(e) = crate::memory::index_file(store, &memory_path).await {
                        tracing::warn!("Failed to index daily note after compaction: {e}");
                    }
                }
                Err(e) => tracing::warn!("Memory store unavailable, daily note not indexed: {e}"),
            }
        });

        // #1930: the prompt ASKS for a ~3 000-token document, but asking is not
        // enforcing — measured 2026-10-04, 208 of the 210 live markers exceeded
        // the budget (mean 27.8 KB, max 69.2 KB) because a 40 000-token output
        // allowance sat against a 9 000-token input reserve. Trim structurally
        // HERE, after generation and BEFORE the #482 correction below, so the
        // correction always survives into the persisted marker.
        let summary = Self::enforce_summary_budget(
            summary,
            super::request_budget::compaction_summary_output_tokens() as usize,
            &active_skills,
        );

        // #482: the summary is obeyed as the continuation document, so an
        // artifact it reports as complete is acted on — the incident claimed a
        // finished transcription, quoted the "exact text" of files that had
        // never been written, and ordered the woken agent to transcribe it.
        // The detectors that guard a live turn are not wired onto this path,
        // and this is the one surface whose output becomes durable context, so
        // the artifact-existence check runs against the very messages being
        // summarised.
        let summary = Self::flag_unbacked_artifacts(summary, &snapshot_messages);

        Ok(summary)
    }

    /// Enforce the continuation document's token budget, structurally (#1930).
    ///
    /// The summariser prompt *asks* for a document of `budget_tokens`, but
    /// asking is not enforcing: on 2026-10-04 the live markers read a mean of
    /// 27 831 B against an ~11.7 KB budget (max 69 154 B; 208 of 210 over), and
    /// a 40 000-token output allowance against a 9 000-token input reserve
    /// guaranteed a model could produce one. A summary that is not trimmed
    /// becomes the woken agent's ENTIRE context and is re-loaded every turn, so
    /// its cost compounds with the session's own length.
    ///
    /// Trimming is at SECTION granularity and never inside a fence: a fenced
    /// block is dropped whole or not at all. Four blocks are never candidates,
    /// because a woken agent cannot recover them from the document alone —
    /// section 0 (the obligation), section 7 (recovery), section 8 (next step)
    /// and the `context-manifest` fence — and the preamble before the first
    /// heading is kept rather than guessed at. If the must-keep set alone
    /// exceeds the budget the guard WARNs and ships it: that is a prompt
    /// defect, not a trimming one, and silently cutting the obligation out
    /// would be worse than an over-budget document.
    pub(crate) fn enforce_summary_budget(
        summary: String,
        budget_tokens: usize,
        active_skills: &[String],
    ) -> String {
        let before = crate::brain::tokenizer::count_tokens(&summary);
        if before <= budget_tokens {
            // #1933: an under-budget document can still be INCOMPLETE. When the
            // summariser is cut at source (`stop_reason = MaxTokens`) the visible
            // document is short — it FITS the budget — while missing the §10
            // fence entirely. This early return is exactly why the one failure
            // that removes the fence was the one the guard never inspected, so
            // the invariants are checked on BOTH paths.
            Self::warn_on_summary_invariants(&summary, active_skills);
            return summary;
        }

        let sections = split_summary_sections(&summary);
        let manifest = extract_manifest_block(&summary);

        // Largest trimmable section first; must-keep sections are never in this
        // list, so neither pass can reach the four load-bearing blocks.
        let mut trimmable: Vec<usize> = sections
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.must_keep)
            .map(|(i, _)| i)
            .collect();
        trimmable.sort_by_key(|&i| std::cmp::Reverse(sections[i].text.len()));

        let mut rendered: Vec<String> = sections.iter().map(|s| s.text.clone()).collect();
        let mut total = before;

        // Pass 1 — fenced code blocks go first, the directive's own order
        // ("name the file and line instead of pasting the block").
        for &i in &trimmable {
            while total > budget_tokens {
                let Some(next) = remove_largest_fenced_block(&rendered[i]) else {
                    break;
                };
                let next_total = total - crate::brain::tokenizer::count_tokens(&rendered[i])
                    + crate::brain::tokenizer::count_tokens(&next);
                if next_total >= total {
                    break;
                }
                rendered[i] = next;
                total = next_total;
            }
            if total <= budget_tokens {
                break;
            }
        }

        // Pass 2 — drop whole trimmable sections, largest first, leaving the
        // heading plus a one-line pointer so the document stays navigable.
        for &i in &trimmable {
            if total <= budget_tokens {
                break;
            }
            let stub = section_stub(&sections[i]);
            let next_total = total - crate::brain::tokenizer::count_tokens(&rendered[i])
                + crate::brain::tokenizer::count_tokens(&stub);
            if next_total >= total {
                continue;
            }
            rendered[i] = stub;
            total = next_total;
        }

        let mut result = rendered.concat();

        // Section 10 keeps the manifest fence in the normal shape, but a sloppy
        // model may place it anywhere; re-attach it if trimming reached it.
        if let Some(block) = manifest
            && parse_context_manifest(&result).is_none()
        {
            result.push_str("\n```context-manifest\n");
            result.push_str(&block);
            if !block.ends_with('\n') {
                result.push('\n');
            }
            result.push_str("```\n");
        }

        let after = crate::brain::tokenizer::count_tokens(&result);

        // The two invariants this guard exists to protect. WARN rather than
        // ship a document that violates them.
        Self::warn_on_summary_invariants(&result, active_skills);
        if after > budget_tokens {
            tracing::warn!(
                "enforce_summary_budget: must-keep sections alone are {} tokens against a {} \
                 budget — the summariser prompt is asking for more than it can keep (#1930)",
                after,
                budget_tokens
            );
        }

        tracing::info!(
            "enforce_summary_budget: trimmed: {} -> {} tokens (budget {})",
            before,
            after,
            budget_tokens
        );
        result
    }

    /// WARN when a continuation document is missing a block a woken agent cannot
    /// recover from anywhere else (#1930, #1933), or when its manifest prunes a
    /// skill the harness still holds active (#1960).
    ///
    /// Runs on the UNDER-budget path too, not only after a trim: a document cut
    /// at source is short AND incomplete at once, and the trim-only check could
    /// never see that combination. `parse_context_manifest` is
    /// position-independent, so this is a real presence test rather than a shape
    /// guess.
    ///
    /// The retention leg is the content half of the same problem: a manifest with
    /// a well-formed fence that lists an active skill under `discard_skills`
    /// passes every presence check and still revokes the #219 re-injection, which
    /// is exactly how the 2026-10-06 specimen lost `opencrabs-dev` while keeping
    /// `opencrabs-dev/harvest.md`. WARN only, never rewrite: the guard cannot know
    /// whether the summariser's judgement or the registry is the stale side, and a
    /// silent edit would hide the disagreement instead of surfacing it.
    fn warn_on_summary_invariants(document: &str, active_skills: &[String]) {
        let manifest = parse_context_manifest(document);
        if manifest.is_none() {
            tracing::warn!(
                "enforce_summary_budget: continuation document carries no context-manifest fence (#1933)"
            );
        }
        if !has_obligation_status(document) {
            tracing::warn!(
                "enforce_summary_budget: continuation document carries no §0 obligation-status line (#1933)"
            );
        }
        let conflicts = Self::manifest_discard_conflicts(document, active_skills);
        if !conflicts.is_empty() {
            tracing::warn!(
                "enforce_summary_budget: the manifest discards {} which the harness still holds \
                 active (#1960): that skill body is gone at wake-up and the next turn's tools \
                 matching it get refused by the skill gate",
                conflicts.join(", ")
            );
        }
    }

    /// #1960: entries of `discard_skills` that collide with a skill the harness
    /// still holds active.
    ///
    /// A collision is an exact name match or a parent/child pair in either
    /// direction, because pruning one silently unloads the other: the live
    /// specimen discarded `opencrabs-dev` while keeping
    /// `opencrabs-dev/harvest.md`, and an auxiliary document cannot be reached
    /// once its parent body is gone. Returns an empty Vec for a document with no
    /// fence (the presence check owns that) or for a session with no active
    /// skills, so this is silent exactly when there is nothing to protect.
    pub(crate) fn manifest_discard_conflicts(
        document: &str,
        active_skills: &[String],
    ) -> Vec<String> {
        if active_skills.is_empty() {
            return Vec::new();
        }
        let Some(manifest) = parse_context_manifest(document) else {
            return Vec::new();
        };
        manifest
            .discard_skills
            .iter()
            .filter(|d| active_skills.iter().any(|a| skill_names_collide(a, d)))
            .cloned()
            .collect()
    }

    /// WARN when the summariser's own stop reason says the document is
    /// incomplete (#1933).
    ///
    /// A `MaxTokens` stop means the request hit its output allowance: the
    /// document is CUT, whatever its token count reads. On 2026-10-04 the cut
    /// landed before §10, so the persisted marker carried no `context-manifest`
    /// fence — and the trim guard never saw it, because a capped document sits
    /// exactly at the budget and the guard only inspected over-budget ones.
    /// This is the visibility leg: read the response's own verdict, not the
    /// document's size.
    pub(crate) fn warn_if_summary_truncated(
        stop_reason: Option<crate::brain::provider::StopReason>,
        usage: &crate::brain::provider::TokenUsage,
    ) {
        if matches!(
            stop_reason,
            Some(crate::brain::provider::StopReason::MaxTokens)
        ) {
            tracing::warn!(
                "compute_compaction_summary: summariser stopped at MaxTokens (output {} / reasoning {}) \
                 — the continuation document is TRUNCATED and may have lost its load-bearing blocks (#1933)",
                usage.output_tokens,
                usage.reasoning_tokens,
            );
        }
    }

    /// Append a harness-written correction for artifacts the summary reports as
    /// complete but which no tool call in the summarised conversation produced
    /// (#482).
    ///
    /// Rides inside the persisted marker, like `decorate_compaction_summary`'s
    /// blocks: a correction a summary cannot be trusted to carry has to arrive
    /// WITH it, not as a later message that can scroll away. The correction
    /// quotes each claim back, the posture of `nudge::unbacked_facts_nudge` —
    /// the model cannot reword its way past a name that appears nowhere.
    fn flag_unbacked_artifacts(summary: String, messages: &[Message]) -> String {
        let claims = crate::brain::agent::service::phantom::claimed_artifacts(&summary);
        if claims.is_empty() {
            return summary;
        }
        let evidence = Self::compaction_evidence(messages);
        let unbacked: Vec<String> = claims
            .into_iter()
            .filter(|claim| {
                crate::brain::agent::service::phantom::claimed_artifact_unbacked(claim, &evidence)
            })
            .collect();
        if unbacked.is_empty() {
            return summary;
        }
        let quoted = unbacked
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            "Compaction summary reports artifacts with no producing call: {}",
            quoted
        );
        format!(
            "{summary}\n\n[System: This summary reports {quoted} as completed, but no tool call in \
             the summarised conversation produced it — no `write_file`/`edit_file` names it and no \
             tool result contains it. Treat it as NOT produced and re-derive the work from the \
             conversation above; do not transcribe or extend anything the summary describes as \
             finished. An artifact is real only when a tool call in this conversation created it.]"
        )
    }

    /// Everything the summarised conversation observed: tool results, tool
    /// inputs (what was actually run — a path in a `write_file` argument is as
    /// real as one in its result) and the user's own text.
    ///
    /// The summariser's own output is deliberately absent: the summary is the
    /// claim under test, and letting it vouch for itself is the defect.
    fn compaction_evidence(messages: &[Message]) -> String {
        let mut evidence = String::new();
        for msg in messages {
            for block in &msg.content {
                match block {
                    ContentBlock::ToolResult { content, .. } => {
                        evidence.push_str(content);
                        evidence.push('\n');
                    }
                    ContentBlock::ToolUse { input, .. } => {
                        evidence.push_str(&input.to_string());
                        evidence.push('\n');
                    }
                    ContentBlock::Text { text }
                        if msg.role == crate::brain::provider::Role::User =>
                    {
                        evidence.push_str(text);
                        evidence.push('\n');
                    }
                    _ => {}
                }
            }
        }
        evidence
    }

    /// Apply a previously-computed compaction summary to a live `AgentContext`.
    ///
    /// Builds the recovered-brain preamble plus a snapshot of the most recent
    /// messages, then calls `AgentContext::compact_with_summary` to do the
    /// in-place swap (replace older messages with the summary, keep the recent
    /// tail within 55% of the window).
    /// Apply a summary that was computed in the background, keeping whatever
    /// the turn appended while the summariser was still thinking.
    ///
    /// `apply_compaction_summary` clears the whole message vector, which is
    /// right when the summariser blocked the turn: nothing could arrive in the
    /// meantime. A background summariser leaves a gap, and everything in that
    /// gap (the tool calls and results of the turn still running) is work the
    /// summary never saw and cannot describe. Clearing it would silently
    /// delete the most recent thing the agent did.
    pub(crate) fn apply_compaction_summary_after(
        context: &mut AgentContext,
        scope: CompactionScope,
        summary: &str,
        snapshot_len: usize,
    ) -> String {
        // The index addresses the message vector this turn is appending to.
        // A vector shorter than the snapshot cannot be that one: the context
        // is rebuilt from the database at the start of every turn, so this
        // means the pending entry outlived its turn. There is no delta to
        // keep, only a summary to apply.
        if snapshot_len > context.messages.len() {
            tracing::warn!(
                "Compaction snapshot ({snapshot_len} messages) outlived its context ({}) — \
                 applying the summary without a delta",
                context.messages.len(),
            );
            return Self::apply_scoped_compaction_summary(context, scope, summary);
        }

        let mut delta = context.messages.split_off(snapshot_len);
        // The summary replaces everything the snapshot covered, so it is
        // computed against exactly what it summarises — scoped (#1649): a
        // delta segment replaces only what followed the last marker, a
        // consolidation replaces the segment run, full window clears all.
        let applied_marker = Self::apply_scoped_compaction_summary(context, scope, summary);

        // The summary lands as a user message. A delta opening with tool
        // results has lost the assistant tool_use that authorised them, and
        // the provider rejects that shape outright.
        let orphans = delta
            .iter()
            .position(|m| !AgentContext::is_orphaned_tool_result_msg(m))
            .unwrap_or(delta.len());
        if orphans > 0 {
            tracing::debug!("Compaction delta: dropping {orphans} orphaned tool results");
            delta.drain(..orphans);
        }

        let kept = delta.len();
        for msg in delta {
            context.add_message(msg);
        }
        tracing::info!("Compaction: kept {kept} messages appended during the summariser call");
        applied_marker
    }

    /// Returns the marker text the apply welded into the context (#1928).
    pub(super) fn apply_compaction_summary(context: &mut AgentContext, summary: &str) -> String {
        // The synchronous path (`/compact`, the two emergency recoveries):
        // full-window by definition — the summary was computed against the
        // whole window, so the apply clears everything and prepends one
        // marker. The background path goes through
        // `apply_compaction_summary_after` with the scope it spawned with.
        Self::apply_scoped_compaction_summary(context, CompactionScope::FullWindow, summary)
    }

    /// Scope-aware apply (#1649). Welds the recovered brain context onto the
    /// summary, then: full window clears all and prepends one marker
    /// (classic behaviour); delta appends a frozen segment after the existing
    /// markers; consolidation replaces the segment run with one superseding
    /// marker.
    pub(crate) fn apply_scoped_compaction_summary(
        context: &mut AgentContext,
        scope: CompactionScope,
        summary: &str,
    ) -> String {
        // No verbatim recent-pairs snapshot is welded onto the marker: the
        // summary prompt's "IMMEDIATE TASK" section already quotes the last
        // exchange from the history, so a second copy duplicated it inside
        // the marker and re-billed it on every subsequent compaction (#1649).
        // The raw tail a background compaction keeps
        // (`apply_compaction_summary_after`) covers continuation mechanically.
        // No brain files are welded on either: SOUL.md, USER.md and AGENTS.md
        // are injected into the system prompt on EVERY turn
        // (`prompt_builder::CORE_BRAIN_FILES` / `ALWAYS_LOADED_FILES`), and
        // `system_brain` survives compaction untouched. Welding them here
        // duplicated ~16k tokens onto the in-memory marker while the marker
        // persisted to the DB carried none, so the confirmation reported a
        // size the next request never had, and the background path re-sent
        // the duplicate on every turn until the next reload (#1676).
        let marker_body = summary.to_string();

        let applied_marker = match scope {
            CompactionScope::FullWindow => {
                // After compaction, the summary IS the conversation — it's
                // prepended as a single user message and the agent picks up
                // from there. We do NOT preserve a raw pre-compaction tail:
                // the summary already embeds the recent-snapshot prose, so
                // keeping the raw tail on top would just duplicate ~half the
                // window and defeat the whole purpose of compacting. Pass 0
                // so `compact_with_summary` clears everything and prepends
                // just the summary.
                context.compact_with_summary(marker_body, 0)
            }
            CompactionScope::DeltaSinceMarker => {
                // The prior frozen segments stay in force verbatim; only the
                // messages the delta summary described are replaced by this
                // new segment marker.
                context.compact_with_delta_summary(marker_body)
            }
            CompactionScope::SegmentConsolidation => {
                // The segments merge into ONE superseding marker; the tail
                // after the segment run is untouched.
                context.consolidate_segments(marker_body)
            }
        };

        tracing::info!(
            "Context compacted ({scope:?}): now at {:.0}% ({} tokens)",
            context.usage_percentage(),
            context.token_count
        );
        applied_marker
    }

    /// Save a compaction summary to a daily memory log at `~/.opencrabs/memory/YYYY-MM-DD.md`.
    ///
    /// Multiple compactions per day append to the same file. The brain workspace's
    /// `MEMORY.md` is left untouched — it stays as user-curated durable memory.
    /// Save a compaction summary to a daily memory log at `~/.opencrabs/memory/YYYY-MM-DD.md`.
    ///
    /// The daily file is shared by every session in the profile, so only the
    /// durable half of the summary is written: the continuation prompt's
    /// session-local sections (immediate task, pending tasks, recovery
    /// playbook, next step, continuation message, harness plan state) belong to
    /// one session and would read as instructions to the next one. One entry
    /// per session per day: a second compaction replaces the earlier entry
    /// instead of stacking a copy, and one entry is capped.
    pub(super) async fn save_compaction_summary_to_memory(
        session_id: Uuid,
        summary: &str,
    ) -> std::result::Result<(), String> {
        let memory_dir = crate::config::opencrabs_home().join("memory");

        std::fs::create_dir_all(&memory_dir)
            .map_err(|e| format!("Failed to create memory directory: {}", e))?;

        let date = chrono::Local::now().format("%Y-%m-%d");
        let memory_path = memory_dir.join(format!("{}.md", date));

        let existing = std::fs::read_to_string(&memory_path).unwrap_or_default();

        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let entry = compaction_memory_entry(session_id, &timestamp.to_string(), summary);
        let new_content = replace_session_entry(&existing, session_id, &entry);

        std::fs::write(&memory_path, new_content.trim_start())
            .map_err(|e| format!("Failed to write daily memory log: {}", e))?;

        tracing::info!("Saved compaction summary to {}", memory_path.display());
        Ok(())
    }
}

/// One compaction entry may not exceed this. A continuation document measures
/// 6 to 15 KB, and the daily file is read whole by every session in the
/// profile, so a handful of uncapped entries cost more context than they
/// return.
const MAX_COMPACTION_ENTRY_BYTES: usize = 8 * 1024;

/// Build the daily-memory entry for one compaction: the durable half of the
/// summary, wrapped in markers so a later compaction in the same session can
/// replace it instead of appending a second copy.
pub(crate) fn compaction_memory_entry(session_id: Uuid, timestamp: &str, summary: &str) -> String {
    let header = format!(
        "<!-- compaction:session:{session_id} -->\n\
         ## Auto-Compaction Summary ({timestamp})\n\n"
    );
    let footer = format!("\n<!-- /compaction:session:{session_id} -->");
    let budget = MAX_COMPACTION_ENTRY_BYTES.saturating_sub(header.len() + footer.len());
    let stripped = strip_session_local_sections(summary);
    let body = if stripped.trim().is_empty() {
        "[the summary carried no durable sections]".to_string()
    } else {
        stripped
    };
    format!("{header}{}{footer}", cap_entry_body(&body, budget))
}

/// Cut `body` to `budget` bytes on a character boundary, and say so when it
/// had to: silent truncation would leave a summary that looks complete.
pub(crate) fn cap_entry_body(body: &str, budget: usize) -> String {
    if body.len() <= budget {
        return body.to_string();
    }
    const NOTE: &str = "\n\n[truncated at the daily-memory entry cap]";
    let mut end = budget.saturating_sub(NOTE.len());
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{NOTE}", &body[..end])
}

/// Write `entry` into `existing`: replace an earlier entry for the same
/// session in place, or append a new one. The markers make the replace exact,
/// and entries written before the markers existed are left alone.
pub(crate) fn replace_session_entry(existing: &str, session_id: Uuid, entry: &str) -> String {
    let open = format!("<!-- compaction:session:{session_id} -->");
    let close = format!("<!-- /compaction:session:{session_id} -->");
    if let Some(start) = existing.find(&open)
        && let Some(rel_end) = existing[start..].find(&close)
    {
        let end = start + rel_end + close.len();
        let mut out = String::with_capacity(existing.len() + entry.len());
        out.push_str(&existing[..start]);
        out.push_str(entry);
        out.push_str(&existing[end..]);
        return out;
    }
    let trimmed = existing.trim_end();
    if trimmed.is_empty() {
        format!("{entry}\n")
    } else {
        format!("{trimmed}\n\n---\n\n{entry}\n")
    }
}

/// Rewrite a stored compaction summary on its way back into context (#2883).
///
/// A continuation document is written to steer the session that produced it.
/// Its execution sections (`## 0. IMMEDIATE TASK`, `## 6. Pending Tasks`,
/// `## PLAN STATE (harness)`) are orders addressed to one moment. Replayed
/// days later they still read as live orders, and the model has no way to tell
/// them from the harness block of the current turn, so it redoes work that
/// finished long ago. The daily-memory path has stripped these sections since
/// it was written ([`compaction_memory_entry`]); the context path never did.
///
/// The banner line survives untouched: [`AgentService::messages_from_last_compaction`]
/// anchors the reload boundary on that exact prefix, so stripping it would
/// delete the boundary along with the directive.
pub(crate) fn reinjectable_summary(
    content: &str,
    written_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use crate::brain::agent::context::COMPACTION_MARKER_PREFIX;

    if !content.starts_with(COMPACTION_MARKER_PREFIX) {
        return content.to_string();
    }
    // The banner is its own line; the document starts after the blank line
    // that follows it. A marker with no body keeps its banner and gains no
    // stamp, because there is nothing to age.
    let (banner, body) = match content.split_once("\n\n") {
        Some((banner, body)) => (banner, body),
        None => (content, ""),
    };
    if body.trim().is_empty() {
        return content.to_string();
    }
    let stripped = strip_session_local_sections(body);
    if stripped.trim().is_empty() {
        return format!(
            "{banner}\n\n[this summary carried only session-local instructions, \
             dropped when it was replayed]"
        );
    }
    format!(
        "{banner}\n\n[summary written {}, {}]\n\n{}",
        written_at.format("%Y-%m-%d %H:%M UTC"),
        relative_age(written_at, now),
        stripped.trim_end(),
    )
}

/// How old a replayed summary is, in words.
///
/// The stamp alone makes a reader do date arithmetic. This says it outright, so
/// a summary written a month ago cannot pass for the current turn at a glance.
fn relative_age(then: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", secs / 60),
        3_600..=86_399 => format!("{} h ago", secs / 3_600),
        _ => {
            let days = secs / 86_400;
            if days == 1 {
                "1 day ago".to_string()
            } else {
                format!("{days} days ago")
            }
        }
    }
}

/// Sections of a compaction continuation document that belong to the session
/// that produced it, so they never reach the shared daily file.
fn is_session_local_heading(heading: &str) -> bool {
    let head = heading.trim().to_ascii_uppercase();
    if [
        "IMMEDIATE TASK",
        "PENDING TASKS",
        "RECOVERY PLAYBOOK",
        "NEXT STEP",
        "CONTINUATION MESSAGE",
        "PLAN STATE",
        "CONTEXT MANIFEST",
    ]
    .iter()
    .any(|needle| head.contains(needle))
    {
        return true;
    }
    // Numbered fallback for a heading whose title changed: 0 is the immediate
    // task, 6 to 10 are execution state.
    let number = head.split('.').next().unwrap_or_default();
    matches!(number, "0" | "6" | "7" | "8" | "9" | "10")
}

/// Keep only the durable sections of a compaction continuation document.
///
/// Line based on purpose: the summary embeds fenced blocks, and a regex over
/// `## ` headings would cut the document at a heading that lives inside a
/// fence. A `context-manifest` block is dropped wherever it appears, fence and
/// all, because it is a machine snapshot of one session.
pub(crate) fn strip_session_local_sections(summary: &str) -> String {
    let mut out = String::with_capacity(summary.len());
    let mut in_fence = false;
    let mut dropping = false;
    let mut skipping_manifest = false;

    for line in summary.lines() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```");

        if skipping_manifest {
            if is_fence {
                skipping_manifest = false;
            }
            continue;
        }

        if is_fence {
            if !in_fence {
                let info = trimmed.trim_start_matches('`').trim();
                if info.starts_with("context-manifest") {
                    skipping_manifest = true;
                    continue;
                }
            }
            in_fence = !in_fence;
            if !dropping {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }

        if in_fence {
            if !dropping {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix("## ") {
            dropping = is_session_local_heading(heading);
            if dropping {
                continue;
            }
        }
        if dropping {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    out.trim_end().to_string()
}

/// Harness-written plan-state block appended to every compaction summary
/// while session plan artifacts exist: state (Editing / Active), the
/// absolute `.md` path when present, and one line on what to do next.
/// `None` when the session is NoPlan (no plan chatter in the summary).
pub(crate) async fn plan_state_block(session_id: Uuid) -> Option<String> {
    use crate::utils::plan_files::{self, PlanModeState};
    let md = plan_files::plan_md_path(session_id).await;
    match plan_files::plan_mode_state(session_id).await {
        PlanModeState::NoPlan => None,
        PlanModeState::PreInitEditing => Some(
            "[PLAN MODE — injected by the harness, not from the user]\n\
             \n\
             **State:** Pre-init (no plan yet)\n\
             \n\
             **Now:**\n\
             1. Explore with reads/search/bash\n\
             2. Call: `plan init mode='design' title='<3-8 words>'`\n\
             \n\
             **Next:** Harness creates .md with template for you to edit.\n\
             \n\
             **Constraints:**\n\
             - No project file edits\n\
             - No pasting plan in chat"
                .to_string(),
        ),
        PlanModeState::PostInitEditing => {
            // PostInitEditing either has a design `.md` to refine (design track)
            // or no `.md` at all — a checklist plan waiting on the user's
            // Approve (#1145: checklist init no longer writes the scaffold).
            // Don't point at a file that does not exist.
            if md.exists() {
                Some(format!(
                    "[PLAN MODE — injected by the harness, not from the user]\n\
                     \n\
                     **State:** Editing design prose\n\
                     **File:** {}\n\
                     \n\
                     **Now:**\n\
                     1. Re-read .md\n\
                     2. Refine design\n\
                     \n\
                     **Next:** Wait for `/execute` approval.\n\
                     \n\
                     **Constraints:**\n\
                     - No `plan start`/`complete`\n\
                     - No project file edits\n\
                     - No pasting plan in chat",
                    md.display()
                ))
            } else {
                Some(format_checklist_editing_reminder().to_string())
            }
        }
        PlanModeState::Active => {
            let (title, done, total) = plan_files::load_plan(session_id)
                .await
                .map(|p| {
                    let done = p
                        .tasks
                        .iter()
                        .filter(|t| {
                            matches!(
                                t.status,
                                crate::tui::plan::TaskStatus::Completed
                                    | crate::tui::plan::TaskStatus::Skipped
                            )
                        })
                        .count();
                    (p.title, done, p.tasks.len())
                })
                .unwrap_or_default();
            let md_line = if md.exists() {
                format!("\nPlan document (frozen): {}", md.display())
            } else {
                String::new()
            };
            Some(format!(
                "## PLAN STATE (harness)\n\
                 State: Active checklist \"{title}\" ({done}/{total} done).{md_line}\n\
                 Next: call plan start (no args) to resurface the in-progress task, \
                 then continue executing the checklist."
            ))
        }
    }
}

/// The Editing reminder for a CHECKLIST plan: the checklist is the
/// deliverable and there is no design document to refine (#1145 — checklist
/// init no longer writes the scaffold `.md`). Pure, like
/// [`format_editing_reminder`].
pub(crate) fn format_checklist_editing_reminder() -> &'static str {
    "[PLAN MODE — injected by the harness, not from the user]\n\
     \n\
     **State:** Checklist plan in Editing (no design document)\n\
     \n\
     **Now:**\n\
     1. The task list is already visible to the user in the plan card.\n\
     2. Do NOT restate the tasks in chat.\n\
     \n\
     **Next:** Wait for the user to approve (Approve button or `/execute`).\n\
     \n\
     **Constraints:**\n\
     - No `plan start`/`complete`\n\
     - No project file edits"
}

/// Build the pinned Editing reminder: the session is in Plan mode, so the
/// turn refines the SESSION PLAN instead of executing. `md_path` is the
/// design document once `plan init` created it (post-init); `None` means
/// pre-init (no document yet). Pure (no IO) so it's unit-testable.
pub(crate) fn format_editing_reminder(md_path: Option<std::path::PathBuf>) -> String {
    match md_path {
        Some(md) => format!(
            "[PLAN MODE — injected by the harness, not from the user]\n\
             \n\
             **State:** Editing design prose\n\
             **File:** {}\n\
             \n\
             **Now:**\n\
             1. Re-read .md\n\
             2. Refine design\n\
             \n\
             **Next:** Wait for `/execute` approval.\n\
             \n\
             **Constraints:**\n\
             - No `plan start`/`complete`\n\
             - No project file edits\n\
             - No pasting plan in chat",
            md.display()
        ),
        None => "[PLAN MODE — injected by the harness, not from the user]\n\
                 \n\
                 **State:** Pre-init (no plan yet)\n\
                 \n\
                 **Now:**\n\
                 1. Explore with reads/search/bash\n\
                 2. Call: `plan init mode='design' title='<3-8 words>'`\n\
                 \n\
                 **Next:** Harness creates .md with template for you to edit.\n\
                 \n\
                 **Constraints:**\n\
                 - No project file edits\n\
                 - No pasting plan in chat"
            .to_string(),
    }
}

/// Build the pinned plan reminder from a plan document. `None` unless the
/// checklist is live (Active) with unresolved tasks — Editing (pre-init flag
/// or design prose) gets no execution nagging. Pure (no IO) so it's
/// unit-testable; `active_plan_reminder` does the file load.
pub(crate) fn format_plan_reminder(plan: &crate::tui::plan::PlanDocument) -> Option<String> {
    use crate::tui::plan::{PlanStatus, TaskStatus};

    if plan.status != PlanStatus::Active || plan.pre_init_editing {
        return None;
    }
    let total = plan.tasks.len();
    if total == 0 {
        return None;
    }
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed))
        .count();
    // Skipped tasks are intentionally resolved; once every task is done or
    // skipped there's nothing left to nag about.
    let resolved = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();
    if resolved == total {
        return None;
    }

    // #1903: staleness cutoff. The reminder is written for in-flight
    // plans; when the plan file has not moved in max_age_hours, the full
    // block is permanent prompt overhead the model often cannot clear
    // (a CLI provider with no native plan tool can neither `complete`
    // nor `discard`). Shrink to a one-line stale note that names the
    // human exits instead. 0 disables the cutoff (remind forever).
    let max_age_hours = crate::config::Config::current()
        .agent
        .plan_reminder_max_age_hours;
    if max_age_hours > 0
        && chrono::Utc::now() - chrono::Duration::hours(max_age_hours as i64) > plan.updated_at
    {
        return Some(format!(
            "[ACTIVE PLAN REMINDER (stale) injected by the harness, not from the user]\n\
                 📋 Plan: \"{}\" ({done}/{total} done) has not moved in {}h; the full reminder is \
                 suppressed. If it is dead, discard it (`plan discard` or the plan card's Discard); \
                 if it lives, `plan start` re-surfaces the task details and resets this clock. Do \
                 not execute it from this note alone.\n",
            plan.title, max_age_hours
        ));
    }

    let mut out = format!(
        "[ACTIVE PLAN REMINDER — injected by the harness, not from the user]\n\
         📋 Plan: \"{}\" ({done}/{total} done). Keep executing it; do not abandon it. \
         Use the plan tool's `complete` as you finish each task (it auto-starts the next), and \
         `start` to (re)surface a task's full details.\n\
         Do NOT repeat the plan title or this reminder text in your response — the \
         plan is already displayed to the user by the surface they are reading.\n",
        plan.title
    );
    let mut tasks: Vec<&crate::tui::plan::PlanTask> = plan.tasks.iter().collect();
    tasks.sort_by_key(|t| t.order);
    // Resolve the set of completed/skipped task ids once, so we can name a
    // blocked task's specific unmet dependencies.
    let resolved_ids: std::collections::HashSet<uuid::Uuid> = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .map(|t| t.id)
        .collect();
    for t in &tasks {
        // [Type, N★] suffix gives the model the task shape at a glance.
        let meta = format!("[{}, {}★]", t.task_type, t.complexity.clamp(1, 5));
        match &t.status {
            TaskStatus::InProgress => {
                out.push_str(&format!("→ Task {}: {} {meta}\n", t.order, t.title));
                if !t.description.is_empty() {
                    let desc: String = t.description.chars().take(160).collect();
                    out.push_str(&format!("  {desc}\n"));
                }
                if !t.acceptance_criteria.is_empty() {
                    let crit = t.acceptance_criteria.join(" • ");
                    out.push_str(&format!("  Criteria: • {crit}\n"));
                }
            }
            TaskStatus::Pending => {
                // Pending but with unmet deps → call it out as blocked, naming them.
                let unmet: Vec<usize> = t
                    .dependencies
                    .iter()
                    .filter_map(|d| d.as_uuid())
                    .filter(|id| !resolved_ids.contains(id))
                    .filter_map(|id| plan.tasks.iter().find(|x| x.id == id).map(|x| x.order))
                    .collect();
                if unmet.is_empty() {
                    out.push_str(&format!("☐ Task {}: {} {meta}\n", t.order, t.title));
                } else {
                    let blockers = unmet
                        .iter()
                        .map(|o| o.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!(
                        "⊘ Task {}: {} {meta} (blocked: {blockers})\n",
                        t.order, t.title
                    ));
                }
            }
            TaskStatus::Failed => out.push_str(&format!(
                "✗ Task {}: {} {meta} (failed — retry/fix)\n",
                t.order, t.title
            )),
            TaskStatus::Blocked(reason) => out.push_str(&format!(
                "⊘ Task {}: {} {meta} (blocked: {reason})\n",
                t.order, t.title
            )),
            TaskStatus::Completed | TaskStatus::Skipped => {}
        }
    }
    Some(out)
}
