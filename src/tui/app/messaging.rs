//! Messaging — session CRUD, slash commands, message expansion, streaming.

use super::answer_placement::{
    answer_already_on_screen, answer_extends_row, last_text_precedes_tool_group,
};
use super::dialogs::ensure_whispercrabs;
use super::events::{AppMode, ToolApprovalResponse, TuiEvent};
use super::onboarding::OnboardingWizard;
use super::*;
use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Return the byte length of a UTF-8 character from its leading byte.
#[inline]
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1, // continuation byte — shouldn't happen on valid str, advance 1
    }
}

/// Text to hand back to the user when a turn ends before the tool loop drained
/// the mid-turn queue.
///
/// Only user-typed entries qualify. A synthetic push (background task,
/// sub-agent, session notify, recovery) carries `[System: ...]` framing in
/// `context_text`, so pasting its `display_text` into the input box would put
/// scaffolding in front of the user as if they had typed it.
pub(crate) fn unconsumed_user_text(
    msgs: &[crate::brain::agent::QueuedUserMessage],
) -> Option<String> {
    let typed: Vec<&str> = msgs
        .iter()
        .filter(|m| m.bg_meta.is_none() && m.context_text == m.display_text)
        .map(|m| m.display_text.as_str())
        .collect();
    if typed.is_empty() {
        None
    } else {
        Some(typed.join("\n"))
    }
}

/// Extract the path argument of a `/cd` slash input (#1574).
///
/// The dispatcher matched only the first token, and the `/cd` arm ignored
/// the rest, so `/cd /path` silently opened the picker. This reads the
/// argument back: `Some(path)` when one was typed, `None` for a bare `/cd`
/// (which keeps the picker). Pure, so the extraction contract is testable
/// without an `App`.
pub(crate) fn cd_args_from_input(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("/cd")?;
    // "/cdutils" is not "/cd" plus an argument — require a whitespace
    // boundary (or end of input) between the command and what follows.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// A file pulled across the drop tunnel and written locally (#1311).
pub(crate) struct PulledDrop {
    /// The client's filename, for display.
    pub name: String,
    /// Where it landed on this machine.
    pub path: String,
    /// Size of the copy.
    pub bytes: usize,
}

/// What scanning a message for dropped paths produced (#1311).
pub(crate) struct Extraction {
    /// The message with the attached paths removed.
    pub text: String,
    pub attachments: Vec<ImageAttachment>,
    /// Receipts to show in the chat, one per file that was copied onto this
    /// machine, so a pull over the drop tunnel is never silent.
    pub notices: Vec<String>,
}

/// What a resolved local file classifies as for attachment purposes
/// (#1740, #1743). Pure extension lookup against the state tables: callers
/// check existence separately (`resolve_dropped_path`), so a sentence that
/// merely ends in ".zip" can never classify as a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachmentClass {
    Image,
    Video,
    /// Sound file: pointer-surfaced with a transcription hint, never
    /// inlined.
    Audio,
    /// Archive or disk image: pointer-surfaced, never extracted here.
    Archive,
    Document,
    TextFile,
    /// A real file whose extension matches no table (#1740). Surfaced as a
    /// path so the agent sees it instead of the paste silently staying
    /// prose.
    UnknownBinary,
}

/// Classify by extension. The tables are disjoint by construction, so a
/// single first-match walk is enough; `UnknownBinary` is the deliberate
/// fall-through (#1740), not an error.
pub(crate) fn attachment_class_for_path(path: &str) -> AttachmentClass {
    let lower = path.to_lowercase();
    let in_table = |table: &[&str]| table.iter().any(|ext| lower.ends_with(ext));
    if in_table(IMAGE_EXTENSIONS) {
        AttachmentClass::Image
    } else if in_table(VIDEO_EXTENSIONS) {
        AttachmentClass::Video
    } else if in_table(AUDIO_EXTENSIONS) {
        AttachmentClass::Audio
    } else if in_table(ARCHIVE_EXTENSIONS) {
        AttachmentClass::Archive
    } else if in_table(DOC_EXTENSIONS) {
        AttachmentClass::Document
    } else if in_table(TEXT_EXTENSIONS) {
        AttachmentClass::TextFile
    } else {
        AttachmentClass::UnknownBinary
    }
}

/// Stamp the settled per-turn metadata (tokens, cost, elapsed time, reasoning)
/// onto an assistant row that already carries this turn's text.
///
/// Two paths land here: the turn's text arrived as an IntermediateText and the
/// response replaced it, or the response text is already on screen and
/// appending it would render the same answer twice. `duration_secs` is written
/// only when the turn clock is still known, so a missing clock never clears a
/// value that is already on the row.
fn stamp_settled_metadata(
    msg: &mut DisplayMessage,
    response: &crate::brain::agent::AgentResponse,
    reasoning_details: &Option<String>,
    duration_secs: Option<i64>,
) {
    msg.token_count = Some(response.usage.output_tokens as i64);
    msg.cost = Some(response.cost);
    if let Some(secs) = duration_secs {
        msg.duration_secs = Some(secs);
    }
    if reasoning_details.is_some() {
        msg.details = reasoning_details.clone();
    }
}

impl App {
    /// Read the persisted approval policy from config.toml.
    /// Returns `(auto_session, auto_always)` flags.
    pub(crate) fn read_approval_policy_from_config() -> (bool, bool) {
        match crate::config::Config::load() {
            Ok(cfg) => match cfg.agent.approval_policy.as_str() {
                "auto-session" => (true, false),
                "auto-always" => (false, true),
                _ => (false, false),
            },
            Err(_) => (false, false),
        }
    }

    /// Queue a context hint for one session (#2006). Keyed by session id the
    /// same way `queued_messages` is, so a hint produced in one pane can never
    /// ride into another session's prompt.
    pub(crate) fn push_pending_context_for(&mut self, session_id: Uuid, hint: String) {
        self.pending_context
            .entry(session_id)
            .or_default()
            .push(hint);
    }

    /// Take every context hint queued for one session and leave the other
    /// sessions' queues untouched. Draining is one-shot: a hint reaches exactly
    /// one turn, and the map keeps no empty entries behind.
    pub(crate) fn take_pending_context_for(&mut self, session_id: Uuid) -> Vec<String> {
        self.pending_context.remove(&session_id).unwrap_or_default()
    }

    /// Apply a `/cd <path>` typed into the TUI (#1574). Validation mirrors
    /// the agent-tool dispatcher (`slash_command::handle_cd`): tilde
    /// expansion, then an is_dir check whose failure is reported, never
    /// silently swallowed.
    pub(crate) async fn apply_cd_target(&mut self, path_arg: &str) {
        let expanded = crate::brain::tools::error::expand_tilde(path_arg);
        if !expanded.is_dir() {
            self.push_system_message(format!("❌ Not a directory: {path_arg}"));
            return;
        }
        self.apply_working_directory_change(&expanded).await;
    }

    /// The single applier for a confirmed working-directory change: used by
    /// the `/cd` picker's Tab/Space confirmation and by typed `/cd <path>`
    /// (#1574), so the two paths cannot drift.
    pub(crate) async fn apply_working_directory_change(&mut self, dir: &std::path::Path) {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

        // Update App working directory
        self.working_directory = canonical.clone();

        // Update AgentService working directory (runtime). Per-session (#703)
        // so `/cd` in this pane never moves another session's cwd.
        if let Some(ref session) = self.current_session {
            self.agent_service
                .set_working_directory_for_session(session.id, canonical.clone());
        } else {
            self.agent_service.set_working_directory(canonical.clone());
        }

        // Persist to session DB — that's the source of truth for per-session WD.
        if let Some(ref session) = self.current_session
            && let Err(e) = self
                .session_service
                .update_session_working_directory(
                    session.id,
                    Some(canonical.to_string_lossy().to_string()),
                )
                .await
        {
            tracing::warn!("failed to persist session working directory: {e}");
        }

        self.push_system_message(format!(
            "Working directory changed to: {}",
            canonical.display()
        ));

        // Queue the context hint for the session this change actually targeted,
        // so its next message to the LLM knows about the cd (#2006).
        if let Some(session) = &self.current_session {
            let sid = session.id;
            self.push_pending_context_for(
                sid,
                format!(
                    "[User changed working directory to: {}]",
                    canonical.display()
                ),
            );
        }
    }

    /// Create a new session
    pub(crate) async fn create_new_session(&mut self) -> Result<()> {
        // Inherit provider and model from the global agent service default.
        // Do NOT use self.default_model_name — that field gets overwritten
        // when loading a session to the session's saved model, so it may
        // be stale (e.g. last session was on a different provider).
        let provider_name = Some(self.agent_service.provider_name());
        let model = Some(self.agent_service.provider_model());
        let session = self
            .session_service
            .create_session_with_provider(Some("New Chat".to_string()), provider_name, model, None)
            .await?;

        // Capture the launching CWD onto the session row so crash-recovery
        // resumes can restore it. Without this, only sessions where the user
        // explicitly /cd'd carry a WD; fresh ones are NULL and resume in
        // whatever current_dir() the next process happens to start in.
        let _ = self
            .session_service
            .update_session_working_directory(
                session.id,
                Some(self.working_directory.to_string_lossy().to_string()),
            )
            .await;

        self.current_session = Some(session.clone());
        // Bind the focused pane to the new session id. Without this, the
        // pane's `session_id` field still points at whatever was loaded
        // before Ctrl+N, so Tab-away + Tab-back loads the previous (just-
        // abandoned) session via the `is_focus_next_pane` handler in
        // state.rs — user sees the new chat "disappear" and has to
        // /sessions back to find it. Mirrors the same sync in
        // `load_session` further down this file. Persisting the layout
        // makes the binding survive restarts when the pane is split.
        if let Some(pane) = self.pane_manager.focused_pane_mut() {
            pane.session_id = Some(session.id);
        }
        if self.pane_manager.is_split() {
            self.pane_manager.save_layout();
        }
        // Persist as last active so the next startup resumes this session.
        // Without this, `last_session` kept pointing at whatever chat the
        // user was on BEFORE creating the new one — restarts loaded the
        // old session and the user saw their fresh work as "gone".
        Self::save_last_session_id(session.id);
        self.set_plan_file_for_session(session.id).await;
        self.is_processing = false; // New session is never processing
        self.messages.clear();
        self.turn_start_len = 0;
        self.auto_scroll = true;
        self.scroll_offset = 0;
        self.mode = AppMode::Chat;
        // Clear streaming state from any previous session
        self.streaming_response = None;
        self.streaming_reasoning = None;
        self.active_tool_group = None;
        self.streaming_output_tokens = 0;
        self.intermediate_text_received = false;
        // Re-read approval policy from config (persisted by /approve)
        (self.approval_auto_session, self.approval_auto_always) =
            Self::read_approval_policy_from_config();
        // Show the system prompt + tools baseline immediately — new sessions are never 0
        let base = self.agent_service.base_context_tokens();
        self.session_input_tokens.insert(session.id, base);
        self.last_input_tokens = Some(base);

        // Sync shared session ID for channels (Telegram, WhatsApp)
        *self.shared_session_id.lock().await = Some(session.id);

        // Reload sessions list
        self.load_sessions().await?;

        Ok(())
    }

    /// Load a session and its messages
    pub(crate) async fn load_session(&mut self, session_id: Uuid) -> Result<()> {
        let session = self
            .session_service
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        // Only the newest display window is read from SQL. Reading the full
        // history to show its tail cost a 57 MB allocation on a large session
        // (#2281); PageUp pages older rows in on demand.
        let (display, hidden) = self.load_display_window(session_id).await?;

        // The agent applies its own compaction window when building the LLM
        // prompt — that's independent of what the human sees. Trimming here was wiping the
        // visible chat after every auto-compaction.

        // Stash old session's cancel token before switching so background
        // processing can still be cancelled from the sessions screen.
        if let Some(old_token) = self.cancel_token.take()
            && let Some(ref old_session) = self.current_session
            && self.processing_sessions.contains(&old_session.id)
        {
            self.session_cancel_tokens.insert(old_session.id, old_token);
        }

        // Cache outgoing session's messages for inactive pane rendering
        if self.pane_manager.is_split()
            && let Some(ref old_session) = self.current_session
        {
            self.pane_message_cache
                .insert(old_session.id, self.messages.clone());
        }

        // Demote the outgoing session's live-turn state into the
        // background-sessions sidecar so a background turn that's
        // still in flight keeps accumulating events (streaming
        // chunks, tool calls, thinking) while the user looks at
        // the new session. Without this snapshot the routing
        // helper has no anchor for events that arrive between now
        // and the next focus-switch back.
        let old_session_id = self.current_session.as_ref().map(|s| s.id);
        if let Some(old_sid) = old_session_id {
            // Persist before demoting, so leaving a live turn is as safe as
            // cancelling one. The sidecar is memory: when that session later
            // finishes while it is not focused, `ResponseComplete` drops the
            // entry on the grounds that the DB has the content, which is only
            // true if something wrote it. Abort and /stop already flush here;
            // the switch did not, so the same output was durable if you
            // cancelled and lost if you merely looked elsewhere (#1421).
            if self.is_session_processing(old_sid) {
                self.persist_streaming_state(old_sid).await;
            }
            self.demote_to_background(old_sid);
        }

        // Clear streaming state from previous session so it doesn't
        // bleed into the newly loaded session's chat view.
        self.streaming_response = None;
        self.streaming_reasoning = None;
        self.active_tool_group = None;
        self.streaming_output_tokens = 0;
        self.intermediate_text_received = false;

        // Restore session's working directory if persisted. Source of truth is
        // the session DB — config.toml does NOT carry per-session state.
        //
        // The agent sync is UNCONDITIONAL (#460): the agent's wd is a shared
        // runtime value that tool-driven cd mutates without updating the
        // TUI-tracked path, so the old `path != self.working_directory`
        // shortcut skipped the sync exactly when the agent sat on ANOTHER
        // session's directory that happened to differ from the TUI's — the
        // footer said one repo while the prompt and tools ran in another.
        // Seed the newly focused session's OWN cwd handle (#703) so the prompt
        // and tools resolve THIS session's directory, not whatever the global
        // last held. Switching panes is not a directory change: the global
        // stays where the operator put it, so no other session inherits this
        // one's repo (#2007).
        if let Some(ref dir_str) = session.working_directory {
            let path = std::path::PathBuf::from(dir_str);
            if path.is_dir() {
                self.working_directory = path.clone();
                self.agent_service
                    .set_session_only_working_directory(session.id, path);
            }
        } else {
            // No persisted wd: pin this session to the TUI-tracked directory so
            // the previous session's wd can never leak into this one (#460).
            self.agent_service
                .set_session_only_working_directory(session.id, self.working_directory.clone());
        }

        self.current_session = Some(session.clone());
        self.set_plan_file_for_session(session.id).await;
        // Sync is_processing flag with per-session state
        self.is_processing = self.processing_sessions.contains(&session.id);

        self.hidden_older_messages = hidden;
        self.oldest_displayed_sequence = display.first().map(|m| m.sequence).unwrap_or(0);
        self.display_token_count = display
            .iter()
            .map(|m| crate::brain::tokenizer::count_tokens(&m.content))
            .sum();
        let mut expanded: Vec<DisplayMessage> =
            display.into_iter().flat_map(Self::expand_message).collect();
        if hidden > 0 {
            expanded.insert(0, Self::make_history_marker(hidden));
        }
        self.messages = expanded;
        // Everything loaded here is durable. A turn that starts (or resumes)
        // from this point on begins at this index, so cancel-persist can never
        // re-collect history into a fresh placeholder row (#2127).
        self.turn_start_len = self.messages.len();
        self.auto_scroll = true;
        self.scroll_offset = 0;

        // Promote any background sidecar entry for the newly
        // focused session back into the AppState live fields, so
        // anything that accumulated while the user looked at the
        // other pane is immediately visible without waiting for the
        // next event. No-op when there's no sidecar entry (typical
        // first-load case); the DB reload above already populated
        // the static message list.
        // Called for its side effect (promote the session to the foreground);
        // the returned "was it present" bool isn't needed here.
        self.promote_to_foreground(session_id);

        // Re-read approval policy from config (persisted by /approve)
        (self.approval_auto_session, self.approval_auto_always) =
            Self::read_approval_policy_from_config();

        // Sync shared session ID for channels (Telegram, WhatsApp)
        *self.shared_session_id.lock().await = Some(session.id);

        // Restore last known context size, preferring the in-memory
        // per-session cache over the DB. The cache is fed by live
        // TokenCountUpdated events for this session's turns and is the
        // most accurate "last observed prompt size" — the DB column
        // still holds the cumulative `billable_input` from the
        // multi-iter fix, which over-reports context on session reload.
        let base = self.agent_service.base_context_tokens();
        let cached_tokens = self.session_input_tokens.get(&session_id).copied();
        let restored_tokens = if let Some(c) = cached_tokens {
            Some(c)
        } else {
            self.message_service
                .last_assistant_input_tokens(session_id)
                .await
                .ok()
                .flatten()
                .map(|t| t as u32)
                .or(Some(base))
        };
        if let Some(t) = restored_tokens {
            self.session_input_tokens.insert(session_id, t);
        }
        self.last_input_tokens = restored_tokens;

        // Clear unread indicator for this session
        self.sessions_with_unread.remove(&session_id);

        // Persist as last active session so startup restores it
        Self::save_last_session_id(session_id);

        // Capture the OLD session's provider BEFORE the switch, so we can
        // use it as the "current provider" for the new session if it has
        // no saved provider. Without this, the else branch below calls
        // `provider_name_for_session(session.id)` which looks up the NEW
        // session's provider (not yet in HashMap), falls back to global
        // default (from pane 1), and contaminates the new session with
        // the wrong provider. 2026-06-05 user report: multi-pane mode
        // shows pane 1's provider + pane 2's model in footer because
        // the provider lookup fell back to global instead of using the
        // provider that was active before the pane switch.
        let old_provider_info = old_session_id.map(|old_sid| {
            let prov = self.agent_service.provider_name_for_session(old_sid);
            let model = self.agent_service.provider_model_for_session(old_sid);
            (prov, model)
        });

        // Populate the session's provider entry in the agent service so
        // future turns for this session use the right provider. Crucially
        // we do NOT call `swap_provider` (the GLOBAL default) — another
        // pane's session is allowed to sit on a different provider, and
        // mutating global mid-turn is exactly the bug that routed a
        // background qwen-plus turn at localhost:8891 when the other
        // pane switched to qwenlocal (logs 2026-04-17 17:01).
        if let Some(ref saved_provider) = session.provider_name {
            let current_prov = self.agent_service.provider_name_for_session(session.id);
            if saved_provider != &current_prov {
                // Track whether the swap actually succeeded. If it
                // fails we MUST NOT fall through to the canonical-
                // rename sync below — without this, any swap failure
                // (stale base_url, transient config read error, etc.)
                // caused the session to silently adopt the current
                // global provider as its "live name" and overwrite
                // session.provider_name in memory (and, on next save,
                // in DB). 2026-04-18 incident: a session saved on
                // "customprovider" came back as "opencode" after restart
                // because the global default at load time was
                // "opencode" (first custom alphabetically) and this
                // code stamped it over the user's saved choice.
                let mut swap_ok = false;
                if let Some(cached) = self.provider_cache.get(saved_provider).cloned() {
                    tracing::info!(
                        "Restoring cached provider '{}' for session {}",
                        saved_provider,
                        session.id
                    );
                    // Restore the session's saved provider+model pair.
                    let model = session
                        .model
                        .clone()
                        .unwrap_or_else(|| cached.default_model().to_string());
                    self.agent_service
                        .swap_provider_for_session(session.id, cached, model);
                    swap_ok = true;
                } else if let Ok(config) = crate::config::Config::load() {
                    match crate::brain::provider::create_provider_by_name(&config, saved_provider)
                        .await
                    {
                        Ok(new_provider) => {
                            tracing::info!(
                                "Created provider '{}' for session {}",
                                saved_provider,
                                session.id
                            );
                            self.provider_cache
                                .insert(saved_provider.clone(), new_provider.clone());
                            let model = session
                                .model
                                .clone()
                                .unwrap_or_else(|| new_provider.default_model().to_string());
                            self.agent_service.swap_provider_for_session(
                                session.id,
                                new_provider,
                                model,
                            );
                            swap_ok = true;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to restore provider '{}' for session {}: {e} — keeping saved name; user will see the real error on next turn",
                                saved_provider,
                                session.id,
                            );
                        }
                    }
                }
                // Canonical-rename sync: ONLY after a successful swap.
                // Handles legacy renames like "qwen-code" → "qwen-cli"
                // where the newly-created provider reports a different
                // canonical id than what was stored. Never runs on
                // failure, so a failed swap never mutates the session's
                // saved provider/model to the global default.
                if swap_ok {
                    // The swap above already set the session's saved model as
                    // the paired model — no separate pin needed.
                    let live_name = self.agent_service.provider_name_for_session(session.id);
                    if live_name != *saved_provider
                        && let Some(ref mut s) = self.current_session
                    {
                        tracing::info!(
                            "Canonical rename on load: '{}' → '{}' for session {}",
                            saved_provider,
                            live_name,
                            session.id
                        );
                        s.provider_name = Some(live_name);
                        s.model = Some(self.agent_service.provider_model_for_session(session.id));
                    }
                }
            }
        } else {
            // Legacy session with no saved provider — stamp the provider
            // that was active BEFORE the switch (from the old pane) onto
            // this session. This ensures the new session gets its own
            // entry in session_providers HashMap with the correct
            // provider, rather than falling back to global default
            // (which belongs to whichever pane was loaded first).
            if let Some(ref mut s) = self.current_session {
                if let Some((prov, model)) = old_provider_info {
                    s.provider_name = Some(prov.clone());
                    s.model = Some(model.clone());
                    // Populate the session's provider entry in HashMap
                    // so future lookups for this session return the
                    // correct provider, not the global default.
                    if let Some(cached) = self.provider_cache.get(&prov).cloned() {
                        self.agent_service.swap_provider_for_session(
                            session.id,
                            cached,
                            model.clone(),
                        );
                    } else if let Ok(config) = crate::config::Config::load()
                        && let Ok(new_provider) =
                            crate::brain::provider::create_provider_by_name(&config, &prov).await
                    {
                        self.provider_cache
                            .insert(prov.clone(), new_provider.clone());
                        self.agent_service.swap_provider_for_session(
                            session.id,
                            new_provider,
                            model.clone(),
                        );
                    }
                } else {
                    // No old session (first load) — use global default
                    s.provider_name =
                        Some(self.agent_service.provider_name_for_session(session.id));
                    s.model = Some(self.agent_service.provider_model_for_session(session.id));
                }
            }
        }

        // Display model comes from the session record, falling back to the session's provider
        self.default_model_name = session
            .model
            .clone()
            .unwrap_or_else(|| self.agent_service.provider_model_for_session(session.id));
        // context_window is session-specific (different providers have
        // different windows). Use per-session limit so compaction and
        // TUI indicator stay correct even when global provider changes.
        let max_ctx = self.agent_service.context_limit_for_session(session_id);
        self.context_max_tokens = max_ctx;
        self.session_context_max.insert(session_id, max_ctx);

        // Keep focused pane in sync with loaded session
        if let Some(pane) = self.pane_manager.focused_pane_mut() {
            pane.session_id = Some(session_id);
        }
        // Persist layout so pane-to-session mapping survives restarts
        if self.pane_manager.is_split() {
            self.pane_manager.save_layout();
        }

        Ok(())
    }

    /// Persist the current session ID to `~/.opencrabs/last_session` so
    /// the next startup can restore the correct session.
    fn save_last_session_id(session_id: Uuid) {
        let base = crate::config::opencrabs_home();
        if let Err(e) = std::fs::write(base.join("last_session"), session_id.to_string()) {
            tracing::warn!("Failed to persist last_session: {}", e);
        }
    }

    /// Read the last active session ID from disk.
    pub(crate) fn read_last_session_id() -> Option<Uuid> {
        let base = crate::config::opencrabs_home();
        let content = std::fs::read_to_string(base.join("last_session")).ok()?;
        Uuid::parse_str(content.trim()).ok()
    }

    /// Pre-load a session's messages into the pane cache (for restored split panes).
    pub(crate) async fn preload_pane_session(&mut self, session_id: Uuid) {
        let display = match self.load_display_window(session_id).await {
            Ok((display, _)) => display,
            Err(e) => {
                tracing::warn!(error = %e, %session_id, "failed to preload pane session");
                Vec::new()
            }
        };
        let expanded: Vec<DisplayMessage> =
            display.into_iter().flat_map(Self::expand_message).collect();
        self.pane_message_cache.insert(session_id, expanded);
    }

    /// The newest `DISPLAY_MESSAGE_WINDOW` rows of a session plus how many
    /// older rows sit behind them. Reads a count and a LIMIT-bounded tail, so
    /// the cost is independent of how long the session has grown (#2281).
    async fn load_display_window(
        &self,
        session_id: Uuid,
    ) -> Result<(Vec<crate::db::models::Message>, usize)> {
        let total = self
            .message_service
            .count_messages_in_session(session_id)
            .await?;
        let rows = self
            .message_service
            .list_recent_messages_for_session(session_id, Self::DISPLAY_MESSAGE_WINDOW)
            .await?;
        let hidden = usize::try_from(total)
            .unwrap_or(0)
            .saturating_sub(rows.len());
        Ok((rows, hidden))
    }

    /// How many of the newest database rows a reloaded pane displays.
    ///
    /// A count, not a token budget. The budget was measured over raw DB rows,
    /// which include the persisted tool-ledger JSON: one such row can carry
    /// hundreds of thousands of tokens on its own, so a couple of oversized
    /// rows collapsed the visible history to two or three messages and a
    /// session switch looked like it had wiped the transcript (#2155). No
    /// single row can eat a count window.
    ///
    /// Anything older stays behind the "N older messages hidden" marker and
    /// remains reachable through PageUp paging.
    pub(crate) const DISPLAY_MESSAGE_WINDOW: usize = 200;

    /// Upper bound on how many rows PageUp paging will load in total. A count
    /// for the same reason the window is one: the old guard stopped paging
    /// once `display_token_count` passed 300k, and a single tool-ledger row can
    /// pass that on its own, which left every older message unreachable
    /// (#2155). This bound still keeps a very long session from loading itself
    /// entirely into memory.
    pub(crate) const MAX_DISPLAYED_MESSAGES: usize = 2_000;

    /// Build the dim italic history marker shown at the top of the message list.
    fn make_history_marker(count: usize) -> DisplayMessage {
        DisplayMessage {
            id: Uuid::new_v4(),
            role: "history_marker".to_string(),
            content: format!("↑ {} older messages hidden · PageUp to load more", count),
            timestamp: chrono::Utc::now(),
            token_count: None,
            cost: None,
            approval: None,
            approve_menu: None,
            details: None,
            expanded: false,
            expanded_full: false,
            tool_group: None,
            duration_secs: None,
        }
    }

    /// Load one more window of older messages from the DB and prepend them to
    /// the current display list. Called by PageUp when `hidden_older_messages`
    /// is above zero.
    ///
    /// A count, matching the display window: a token budget here measured raw
    /// DB rows, and one persisted tool-ledger row could fill it on its own, so
    /// every PageUp added exactly one row (#2155).
    pub(crate) async fn load_more_history(&mut self) -> Result<()> {
        tracing::debug!(
            "[SCROLL] load_more_history called: hidden={}, display_tokens={}",
            self.hidden_older_messages,
            self.display_token_count
        );
        let session_id = match self.current_session.as_ref().map(|s| s.id) {
            Some(id) => id,
            None => return Ok(()),
        };
        let msgs_before = self.messages.len();
        // One more window of rows strictly older than the oldest displayed
        // one, read from SQL with LIMIT (#2281). Rows still hidden after this
        // page come from the running count, not from a full reload.
        let to_add = self
            .message_service
            .list_messages_before_sequence(
                session_id,
                self.oldest_displayed_sequence,
                Self::DISPLAY_MESSAGE_WINDOW,
            )
            .await?;
        let tokens: usize = to_add
            .iter()
            .map(|m| crate::brain::tokenizer::count_tokens(&m.content))
            .sum();
        let hidden_still = self.hidden_older_messages.saturating_sub(to_add.len());

        // Remove existing history_marker at front
        let removed_marker = if self
            .messages
            .first()
            .map(|m| m.role == "history_marker")
            .unwrap_or(false)
        {
            self.messages.remove(0);
            true
        } else {
            false
        };

        let mut new_msgs: Vec<DisplayMessage> = to_add
            .iter()
            .cloned()
            .flat_map(Self::expand_message)
            .collect();
        if hidden_still > 0 {
            new_msgs.insert(0, Self::make_history_marker(hidden_still));
        }
        // Count the front block BEFORE `append` drains `self.messages`: `append`
        // leaves the source empty, so measuring afterwards reports the whole
        // history as prepended and throws the anchor past the end (#2127).
        let prepended = new_msgs.len();
        new_msgs.append(&mut self.messages);
        self.messages = new_msgs;
        self.turn_start_len = shift_turn_start(
            self.turn_start_len,
            prepended,
            usize::from(removed_marker),
            self.messages.len(),
        );
        self.hidden_older_messages = hidden_still;
        self.oldest_displayed_sequence = to_add.first().map(|m| m.sequence).unwrap_or(0);
        self.display_token_count += tokens;
        self.render_cache.clear();
        tracing::debug!(
            "[SCROLL] load_more_history done: msgs {} -> {}, hidden_remaining={}, tokens_added={}",
            msgs_before,
            self.messages.len(),
            hidden_still,
            tokens
        );
        Ok(())
    }

    /// Load all sessions
    pub(crate) async fn load_sessions(&mut self) -> Result<()> {
        use crate::db::repository::{SessionListOptions, UsageLedgerRepository};

        self.sessions = self
            .session_service
            .list_sessions(SessionListOptions {
                include_archived: false,
                limit: Some(100),
                offset: 0,
                query: None,
                include_subagents: false,
            })
            .await?;

        // Hydrate the pin set, then float pinned sessions to the top of the
        // dialog WITHOUT disturbing the recency order inside each group:
        // `sort_by_key` is stable, so favorites keep their updated_at order
        // and unpinned sessions keep theirs. The sort lives in this TUI
        // loader only, so the CLI and channel session lists stay pure
        // recency.
        self.session_favorites = self
            .session_service
            .session_favorites()
            .await
            .unwrap_or_default()
            .into_iter()
            // The table stores TEXT ids; parse to `Uuid` so every consumer
            // compares native session ids with zero per-row allocation. A
            // malformed row simply never matches and is skipped.
            .filter_map(|id| Uuid::parse_str(&id).ok())
            .collect();
        let pins = &self.session_favorites;
        self.sessions.sort_by_key(|s| !pins.contains(&s.id));

        // Populate project name cache for session list display
        if let Ok(projects) = self.project_service.list_projects().await {
            for p in projects {
                self.project_name_cache.entry(p.id).or_insert(p.name);
            }
        }

        // Load all-time usage from the ledger (survives session deletes)
        let ledger = UsageLedgerRepository::new(self.session_service.pool());
        self.usage_ledger_stats = ledger.stats_by_model().await.unwrap_or_default();

        Ok(())
    }

    /// Clear all messages from the current session
    pub(crate) async fn clear_session(&mut self) -> Result<()> {
        if let Some(session) = &self.current_session {
            // Delete all messages from the database
            self.message_service
                .delete_messages_for_session(session.id)
                .await?;

            // Clear messages from UI
            self.messages.clear();
            self.scroll_offset = 0;
            self.streaming_response = None;
            self.streaming_reasoning = None;
            self.error_message = None;
            self.error_message_shown_at = None;
        }

        Ok(())
    }

    /// Handle slash commands locally (returns true if handled)
    pub(crate) async fn handle_slash_command(&mut self, input: &str) -> bool {
        let cmd = input.split_whitespace().next().unwrap_or("");
        match cmd {
            "/usage" => {
                self.open_usage_dashboard().await;
                self.mode = AppMode::UsageDashboard;
                true
            }
            "/cowork" => {
                // Cowork is a Telegram-group feature but launchable from here:
                // hand it to the agent so it calls the cowork_connect tool,
                // which builds the deep link + QR and registers the session.
                let name = input.strip_prefix("/cowork").unwrap_or("").trim();
                let prompt = if name.is_empty() {
                    "[SYSTEM: The user wants to create a Telegram cowork workspace. Ask them \
                     for a workspace name, then call the cowork_connect tool with it and show \
                     them the deep link it returns.]"
                        .to_string()
                } else {
                    format!(
                        "[SYSTEM: Create a Telegram cowork workspace named '{name}': call the \
                         cowork_connect tool with workspace_name='{name}', then show the user \
                         the deep link it returns so they can tap it to create the group.]"
                    )
                };
                let sender = self.event_sender();
                let _ = sender.send(TuiEvent::CommandSubmitted(prompt));
                true
            }
            // `/models <provider/model>`: direct switch for the current session,
            // no picker (#467, #2064). Matched on the full input, because `cmd`
            // is only the first word. The arm sits before the setup-dialog arm,
            // which also claims `/models`. Same switch path as the keyboard flow
            // (per-session swap + manual pin + DB persist).
            "/models" if crate::cli::session_set_model::direct_switch_arg(input).is_some() => {
                let arg =
                    crate::cli::session_set_model::direct_switch_arg(input).unwrap_or_default();
                let Some(sid) = self.current_session.as_ref().map(|cs| cs.id) else {
                    self.push_system_message("No active session to switch.".to_string());
                    return true;
                };
                let reply =
                    crate::channels::commands::direct_model_switch(&self.agent_service, sid, arg)
                        .await;
                // Reflect the switch in the in-memory session so the footer
                // updates immediately instead of on the next session load.
                let pair = arg.strip_suffix(" all").unwrap_or(arg);
                if !reply.starts_with('⚠')
                    && let Ok((provider, model)) = crate::utils::provider_pair::parse_pair(pair)
                    && let Some(cs) = self.current_session.as_mut()
                {
                    cs.provider_name = Some(provider);
                    cs.model = Some(model);
                }
                self.push_system_message(reply);
                true
            }
            s if s.starts_with("/onboard")
                || crate::tui::onboarding::deep_link::is_setup_command(s) =>
            {
                use crate::tui::onboarding::OnboardingStep;
                use crate::tui::onboarding::deep_link::{self, DeepLink};
                // Resolution reads the full input, not just the first word, so
                // arguments like `/channels whatsapp` survive: `cmd`
                // holds only the first word, which drops the channel name and
                // sends the deep-link back to the menu.
                let (link, channel_arg) = deep_link::resolve(s, input);
                let step = match link {
                    DeepLink::Step(step) => step,
                    DeepLink::FullWizard => OnboardingStep::ModeSelect,
                    // An unrecognised suffix used to fall through a catch-all
                    // into the full wizard, so a typo and a doc row that had
                    // outlived its arm both looked exactly like bare
                    // `/onboard` (#1664). Name it back instead.
                    DeepLink::Unknown(suffix) => {
                        self.push_system_message(deep_link::unknown_suffix_message(&suffix));
                        return true;
                    }
                };
                let config = match crate::config::Config::load() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Failed to load config for onboarding: {}", e);
                        self.push_system_message(format!(
                            "⚠️ Could not load config: {}. Cannot open onboarding.",
                            e
                        ));
                        return false;
                    }
                };
                let mut wizard = OnboardingWizard::from_config(&config);
                wizard.step = step;
                // `/models` opens on the provider and model this session is using
                // (#2060), not on whatever config happens to list first.
                if step == OnboardingStep::ProviderAuth
                    && let Some(session) = self.current_session.as_ref()
                {
                    let provider = self.agent_service.provider_name_for_session(session.id);
                    if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
                        wizard.select_session_pair(&provider, model, &config);
                    }
                }
                // Deep-link a named channel directly to its setup dialog. Seeds
                // the channel's fields just like selecting it in the menu; on an
                // unknown name `open_channel_setup` leaves the menu step intact.
                let step = if step == OnboardingStep::Channels
                    && !channel_arg.is_empty()
                    && wizard.open_channel_setup(channel_arg)
                {
                    wizard.step
                } else {
                    step
                };
                // Deep-link to a specific step: lock to that step only
                // (no progress dots, no navigation, Enter/Esc exit to chat)
                // Only bare /onboard runs the full wizard flow
                if step != OnboardingStep::ModeSelect {
                    wizard.quick_jump = true;
                }
                if step == OnboardingStep::HealthCheck {
                    wizard.start_health_check();
                }
                if step == OnboardingStep::ImageSetup {
                    wizard.detect_existing_image_key();
                }
                self.onboarding = Some(wizard);
                self.mode = AppMode::Onboarding;
                true
            }
            "/new" => {
                let _ = self.event_sender().send(TuiEvent::NewSession);
                true
            }
            "/sessions" => {
                self.mode = AppMode::Sessions;
                let _ = self
                    .event_sender()
                    .send(TuiEvent::SwitchMode(AppMode::Sessions));
                true
            }
            "/approve" => {
                self.messages.push(DisplayMessage {
                    id: Uuid::new_v4(),
                    role: "system".to_string(),
                    content: String::new(),
                    timestamp: chrono::Utc::now(),
                    token_count: None,
                    cost: None,
                    approval: None,
                    approve_menu: Some(ApproveMenu {
                        selected_option: 0,
                        state: ApproveMenuState::Pending,
                    }),
                    details: None,
                    expanded: false,
                    expanded_full: false,
                    tool_group: None,
                    duration_secs: None,
                });
                self.scroll_offset = 0;
                true
            }
            "/compact" => {
                let pct = self.context_usage_percent();
                // Send first, then report (#1375): a note printed before the
                // trigger leaves claims a compaction that may never start,
                // and a dropped send must never be silent.
                let sender = self.event_sender();
                match sender.send(TuiEvent::CommandSubmitted(
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string(),
                )) {
                    Ok(()) => {
                        self.push_system_message(crate::tui::compact_notice::requested(pct));
                    }
                    Err(e) => {
                        tracing::error!("/compact: compaction trigger not dispatched: {e}");
                        self.error_message = Some(crate::tui::compact_notice::dispatch_failed(&e));
                    }
                }
                true
            }
            "/clear" => {
                // Cut the agent's context here at no cost (#1585). History
                // stays on screen; the footer drops to the baseline a new
                // session starts at.
                if self.is_processing {
                    self.push_system_message(crate::tui::clear_notice::busy());
                    return true;
                }
                let Some(session_id) = self.current_session.as_ref().map(|s| s.id) else {
                    self.push_system_message(crate::tui::clear_notice::no_session());
                    return true;
                };
                match self.agent_service.clear_context(session_id).await {
                    Ok(receipt) => {
                        let base = self.agent_service.base_context_tokens();
                        self.session_input_tokens.insert(session_id, base);
                        self.last_input_tokens = Some(base);
                        self.push_system_message(crate::tui::clear_notice::cleared(&receipt));
                    }
                    Err(e) => {
                        tracing::error!("/clear failed: {e}");
                        self.error_message = Some(crate::tui::clear_notice::failed(&e));
                    }
                }
                true
            }
            "/theme" => {
                use crate::tui::render::presets;
                use crate::tui::render::theme;
                let rest = input.split_once(' ').map(|x| x.1.trim()).unwrap_or("");
                let mut parts = rest.splitn(2, ' ');
                let sub = parts.next().unwrap_or("");
                let arg = parts.next().map(str::trim).unwrap_or("");
                match sub {
                    "" => {
                        // Bare /theme opens the interactive picker (#1371);
                        // `list` keeps the text surface for habits/scripts.
                        self.theme_picker =
                            Some(crate::tui::render::theme_picker::ThemePickerState::open());
                    }
                    "list" | "ls" => {
                        let active_name = theme::active().name;
                        let mut lines: Vec<String> = Vec::new();
                        lines.push("Built-in themes:".to_string());
                        for t in presets::built_ins() {
                            let marker = if t.name == active_name { " *" } else { "" };
                            lines.push(format!("  • {}{}", t.name, marker));
                        }
                        // User presets (S3): rescan on every list so file edits
                        // hot-load; rejected files surface with their reason.
                        let report = crate::tui::render::user_themes::reload();
                        if !report.themes.is_empty() {
                            lines.push("User presets (~/.opencrabs/themes/):".to_string());
                            for t in &report.themes {
                                let marker = if t.name == active_name { " *" } else { "" };
                                lines.push(format!("  • {}{}", t.name, marker));
                            }
                        }
                        for r in &report.rejected {
                            lines.push(format!("  ✗ {} — {}", r.file, r.reason));
                        }
                        lines.push(format!("\nActive: {}", active_name));
                        lines.push("Use: /theme set <name> · /theme reset".to_string());
                        self.push_system_message(lines.join("\n"));
                    }
                    "set" => {
                        if arg.is_empty() {
                            self.push_system_message("Usage: /theme set <name>".to_string());
                        } else if let Some(t) = presets::by_name(arg)
                            .or_else(|| crate::tui::render::user_themes::find(arg))
                        {
                            theme::set(t);
                            // Persist to config.toml; ConfigWatcher reload
                            // re-applies on next boot / config change.
                            // The bare name: `write_key_string` produces the
                            // TOML string itself, so quoting here stored the
                            // quote characters inside the value and the boot
                            // lookup could never match it (#1428).
                            if let Err(e) =
                                crate::config::Config::write_key_string("tui", "theme", t.name)
                            {
                                self.push_system_message(format!(
                                    "Applied '{}' (live). Persist failed: {e}",
                                    t.name
                                ));
                            } else {
                                self.push_system_message(format!(
                                    "Theme switched to '{}' — applied live and persisted.",
                                    t.name
                                ));
                            }
                        } else {
                            self.push_system_message(format!(
                                "Unknown theme '{}'. Run /theme list for available names.",
                                arg
                            ));
                        }
                    }
                    "reset" => {
                        theme::reset();
                        // Remove the key so boot falls through to CRAB_DARK.
                        if let Err(e) = crate::config::Config::write_key_string("tui", "theme", "")
                        {
                            self.push_system_message(format!(
                                "Reset to default. Persist failed: {e}"
                            ));
                        } else {
                            self.push_system_message(
                                "Theme reset to crab-dark (default).".to_string(),
                            );
                        }
                    }
                    other => {
                        self.push_system_message(format!(
                            "Unknown /theme subcommand '{}'. Try: list | set <name> | reset",
                            other
                        ));
                    }
                }
                true
            }
            "/plan" => {
                if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
                    let query = input
                        .split_once(' ')
                        .map(|x| x.1.trim().to_string())
                        .unwrap_or_default();
                    let reply = crate::utils::plan_mode::enter_plan_mode(sid).await;
                    self.reload_plan();
                    if query.is_empty() {
                        self.push_system_message(reply);
                    } else {
                        // `/plan <query>`: Plan mode is armed; run the query as
                        // the planning turn so the agent drafts the design from
                        // it in one step (#579).
                        let sender = self.event_sender();
                        let _ = sender.send(TuiEvent::CommandSubmitted(query));
                    }
                } else {
                    self.push_system_message("No active session.".to_string());
                }
                true
            }
            "/show-plan" | "/showplan" | "/show_plan" => {
                if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
                    self.reload_plan();
                    use crate::utils::plan_files::{PlanModeState, plan_mode_state};
                    match plan_mode_state(sid).await {
                        // A live plan opens the overlay (scrollable design
                        // .md with the Approve/Discard footer, or the Active
                        // checklist view).
                        PlanModeState::PostInitEditing
                        | PlanModeState::Active
                        | PlanModeState::PreInitEditing => {
                            self.plan_overlay_scroll = 0;
                            if let Err(e) = self.switch_mode(AppMode::PlanOverlay).await {
                                tracing::warn!("Failed to open plan overlay: {e}");
                            }
                        }
                        PlanModeState::NoPlan => {
                            let reply = crate::utils::plan_mode::show_plan(sid).await;
                            self.push_system_message(reply);
                        }
                    }
                } else {
                    self.push_system_message("No active session.".to_string());
                }
                true
            }
            "/execute" => {
                // Approve and /execute are FORBIDDEN while a turn is running:
                // refuse immediately, never queue (locked).
                if self.is_processing {
                    self.push_system_message(
                        "⛔ A turn is running. /execute and Approve are refused while \
                         busy; try again when the turn finishes."
                            .to_string(),
                    );
                    return true;
                }
                let Some(sid) = self.current_session.as_ref().map(|s| s.id) else {
                    self.push_system_message("No active session.".to_string());
                    return true;
                };
                match crate::utils::plan_mode::try_approve(
                    sid,
                    crate::tui::plan::ApprovalSource::User,
                )
                .await
                {
                    crate::utils::plan_mode::ApproveOutcome::Refused(reply) => {
                        self.push_system_message(reply);
                    }
                    crate::utils::plan_mode::ApproveOutcome::SeedTurn { prompt } => {
                        self.push_system_message("✅ Plan approved — starting now…".to_string());
                        self.reload_plan();
                        // Visible seed turn: dispatch the locked implement-turn
                        // prompt as a command-sourced message (analyzer skipped).
                        let sender = self.event_sender();
                        if let Err(e) = sender.send(TuiEvent::CommandSubmitted(prompt)) {
                            tracing::error!("Failed to dispatch seed turn: {e}");
                        }
                    }
                }
                true
            }
            "/discard" => {
                // /discard cancels the in-flight turn first, then cleans up.
                let mut cancelled = false;
                if self.is_processing
                    && let Some(token) = self.cancel_token.take()
                {
                    token.cancel();
                    cancelled = true;
                }
                let Some(sid) = self.current_session.as_ref().map(|s| s.id) else {
                    self.push_system_message("No active session.".to_string());
                    return true;
                };
                let mut reply =
                    crate::utils::plan_mode::discard(sid, self.agent_service.context()).await;
                if cancelled {
                    reply = format!("⏹️ Cancelled the running turn. {reply}");
                }
                self.push_system_message(reply);
                self.plan_document = None;
                true
            }
            s if s == "/architecture" || s.starts_with("/architecture ") => {
                // Mechanical directory tree (#933): pure std::fs walk, no
                // LLM, zero per-use cost. Same surface on channels.
                let arg = input.strip_prefix("/architecture").unwrap_or("").trim();
                let reply = crate::channels::commands::run_architecture(if arg.is_empty() {
                    None
                } else {
                    Some(arg)
                });
                self.push_system_message(reply);
                true
            }
            s if s == "/attach" || s.starts_with("/attach ") => {
                // Mechanical file attach (#933): validate each path against
                // the compiled confidential gate, ride the #1740 pipeline for
                // image attachments, and leave a bracketed note in the input
                // buffer so the next sent message carries every file to the
                // model. No LLM calls, no content dumping.
                let args = input.strip_prefix("/attach").unwrap_or("").trim();
                if args.is_empty() {
                    self.push_system_message(
                        "attach: usage /attach <path> [more paths...]".to_string(),
                    );
                    return true;
                }
                // One verdict pass shared with the channel surface (#933):
                // identical status lines, identical gates.
                let (lines, attached_paths) = crate::channels::commands::attach_status_lines(args);
                for raw in &attached_paths {
                    let extraction = Self::extract_attachments(raw);
                    for notice in extraction.notices {
                        self.push_system_message(notice);
                    }
                    if !extraction.attachments.is_empty() {
                        if let Some(session) = &self.current_session {
                            for att in &extraction.attachments {
                                let file_svc = self.file_service.clone();
                                let sid = session.id;
                                let path = std::path::PathBuf::from(&att.path);
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        file_svc.get_or_create_file(sid, path, None).await
                                    {
                                        tracing::warn!("Failed to track /attach file: {e}");
                                    }
                                });
                            }
                        }
                        self.attachments.extend(extraction.attachments);
                    }
                }
                if !attached_paths.is_empty() {
                    let notes = attached_paths
                        .iter()
                        .map(|p| format!("[User attached file: {p}]"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.insert_text_at_cursor(&format!("{notes}\n"));
                    self.notification =
                        Some(format!("📎 Attached {} file(s)", attached_paths.len()));
                    self.notification_shown_at = Some(std::time::Instant::now());
                }
                self.push_system_message(lines.join("\n"));
                true
            }
            s if s == "/audit" || s.starts_with("/audit ") => {
                // Mechanical audit viewer (#1705): the same renderer the
                // channels use, straight into the transcript. No LLM calls.
                let args = input.strip_prefix("/audit").unwrap_or("").trim();
                let n = args
                    .split_whitespace()
                    .next()
                    .and_then(|a| a.parse::<u32>().ok());
                let reply =
                    crate::channels::commands::run_audit_channel(n, &self.agent_service).await;
                self.push_system_message(reply);
                true
            }
            "/exit" | "/quit" => {
                // Same shutdown the runner already drives for Ctrl+C twice,
                // just reachable by typing it (#923).
                crate::brain::agent::service::shutdown::mark_shutting_down();
                self.should_quit = true;
                true
            }
            "/restart" => {
                // Routed through SelfUpdater so the session is resumed on the
                // way back up, rather than a second exec implementation that
                // would come back to an empty chat. Only returns on failure.
                let sid = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id)
                    .unwrap_or(Uuid::nil());
                self.push_system_message("♻️ Restarting OpenCrabs...".to_string());
                match crate::brain::SelfUpdater::auto_detect().and_then(|u| u.restart(sid)) {
                    Ok(()) => {}
                    // Still running, and the user was already told it was going
                    // down, so the correction has to be visible in-session.
                    Err(e) => self.show_error(format!("Restart failed: {e}")),
                }
                true
            }
            "/evolve" => {
                self.push_system_message("Checking for updates...".to_string());
                let sender = self.event_sender();
                let sid = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id)
                    .unwrap_or(Uuid::nil());
                tokio::spawn(async move {
                    run_evolve_directly(sid, sender).await;
                });
                true
            }
            "/whisper" => {
                self.push_system_message("Setting up WhisperCrabs...".to_string());
                let sender = self.event_sender();
                let sid = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id)
                    .unwrap_or(Uuid::nil());
                tokio::spawn(async move {
                    match ensure_whispercrabs().await {
                        Ok(binary_path) => {
                            // Launch the binary (GTK handles if already running)
                            match tokio::process::Command::new(&binary_path)
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                            {
                                Ok(_) => {
                                    let _ = sender.send(TuiEvent::SystemMessage {
                                        session_id: sid,
                                        text: "WhisperCrabs is running! A floating mic button is now on your screen.\n\n\
                                            Speak from any app — transcription is auto-copied to your clipboard. Just paste wherever you need.\n\n\
                                            To change settings, right-click the button or just ask me here.".to_string(),
                                    });
                                }
                                Err(e) => {
                                    let _ = sender.send(TuiEvent::Error {
                                        session_id: sid,
                                        message: format!("Failed to launch WhisperCrabs: {}", e),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = sender.send(TuiEvent::Error {
                                session_id: sid,
                                message: format!("WhisperCrabs setup failed: {}", e),
                            });
                        }
                    }
                });
                true
            }
            "/help" => {
                // Through `switch_mode`, never a bare `self.mode` assignment:
                // entering help is what loads the command catalog (#1586).
                if let Err(e) = self.switch_mode(AppMode::Help).await {
                    tracing::warn!("Failed to open help screen: {e}");
                }
                true
            }
            "/mission-control" => {
                crate::tui::app::mission_control::actions::open(self).await;
                true
            }
            "/skills" => {
                crate::tui::app::skills_dialog::actions::open(self);
                true
            }
            "/profiles" => {
                crate::tui::app::profiles_dialog::actions::open(self);
                true
            }
            "/rtk" => {
                #[cfg(feature = "rtk")]
                {
                    if !crate::rtk::is_rtk_available().await {
                        self.push_system_message(crate::rtk::RTK_NOT_INSTALLED_HELP.to_string());
                    } else {
                        self.push_system_message(
                            "🔍 Fetching RTK token savings statistics...".to_string(),
                        );
                        let sender = self.event_sender();
                        let sid = self
                            .current_session
                            .as_ref()
                            .map(|s| s.id)
                            .unwrap_or(Uuid::nil());
                        tokio::spawn(async move {
                            match tokio::process::Command::new("rtk")
                                .arg("gain")
                                .output()
                                .await
                            {
                                Ok(output) => {
                                    let stdout = String::from_utf8_lossy(&output.stdout);
                                    let stderr = String::from_utf8_lossy(&output.stderr);

                                    let message = if output.status.success() {
                                        format!(
                                            "📊 **RTK Token Savings:**\n\n```\n{}\n```",
                                            stdout.trim()
                                        )
                                    } else {
                                        format!(
                                            "⚠️ RTK gain command failed:\n\n```\n{}\n```",
                                            stderr.trim()
                                        )
                                    };

                                    let _ = sender.send(TuiEvent::SystemMessage {
                                        session_id: sid,
                                        text: message,
                                    });
                                }
                                Err(e) => {
                                    let _ = sender.send(TuiEvent::Error {
                                        session_id: sid,
                                        message: format!(
                                            "Failed to run rtk gain: {}. Is RTK installed?",
                                            e
                                        ),
                                    });
                                }
                            }
                        });
                    }
                }
                #[cfg(not(feature = "rtk"))]
                {
                    self.push_system_message(
                        "⚠️ RTK feature is not enabled. Rebuild with --features rtk to enable token savings tracking.".to_string()
                    );
                }
                true
            }
            "/cd" => {
                // #1574: a bare `/cd` opens the picker; `/cd <path>` must
                // behave like the channel and agent-tool dispatchers, which
                // both honor the argument. Only the TUI used to drop it.
                if let Some(arg) = cd_args_from_input(input) {
                    self.apply_cd_target(arg).await;
                } else if let Err(e) = self.open_directory_picker().await {
                    tracing::warn!(error = %e, "failed to open directory picker");
                }
                true
            }
            "/goal" => {
                let args = input.strip_prefix("/goal").unwrap_or("");
                if crate::brain::goal::is_bare(args) {
                    // Bare `/goal`: DENY it. Show the usage warning directly and
                    // send NOTHING to the LLM — an abandoned/mistyped /goal leaves
                    // the conversation exactly as it was, zero context interference.
                    self.push_system_message(crate::brain::goal::goal_usage_warning());
                } else {
                    let prompt = crate::brain::goal::goal_command_prompt(args);
                    let sender = self.event_sender();
                    let _ = sender.send(TuiEvent::CommandSubmitted(prompt));
                }
                true
            }
            _ if input.starts_with('/') && !crate::utils::string::looks_like_file_path(input) => {
                // Check user-defined commands first — explicit user definitions
                // win over auto-registered skills of the same `/<name>`.
                if let Some(user_cmd) = self.user_commands.iter().find(|c| c.name == cmd) {
                    let prompt = user_cmd.prompt.clone();
                    let action = user_cmd.action.clone();
                    match action.as_str() {
                        "system" => {
                            self.push_system_message(prompt);
                        }
                        _ => {
                            // "prompt" action — send to LLM
                            let sender = self.event_sender();
                            let _ = sender.send(TuiEvent::CommandSubmitted(prompt));
                        }
                    }
                    return true;
                }
                // Fall back to skills: any /<name> that matches a loaded skill's
                // slash_name dispatches its body as a prompt. Auto-registration
                // means a user dropping a SKILL.md gets a working slash command
                // immediately, no commands.toml entry required.
                if let Some(skill) = self.skills.iter().find(|s| s.slash_name == cmd) {
                    // `prompt_body()` prepends the review-gate reminder when
                    // the skill declares `review_gate: true`.
                    let prompt = skill.prompt_body();
                    let sender = self.event_sender();
                    let _ = sender.send(TuiEvent::CommandSubmitted(prompt));
                    return true;
                }
                // Unknown slash command — show warning inline, keep input, don't add to chat
                self.error_message = Some(format!(
                    "⚡ Unknown command: {}. Type /help for available commands.",
                    cmd
                ));
                self.error_message_shown_at = Some(std::time::Instant::now());
                true
            }
            _ => false,
        }
    }

    /// Format a human-readable description of a tool call from its name and input
    /// Case-insensitive key lookup on a JSON object.
    /// Handles camelCase, snake_case, or whatever the model sends.
    fn get_input_ci<'a>(input: &'a Value, key: &str) -> Option<&'a Value> {
        input.get(key).or_else(|| {
            let lower = key.to_lowercase();
            input
                .as_object()
                .and_then(|obj| obj.iter().find(|(k, _)| k.to_lowercase() == lower))
                .map(|(_, v)| v)
        })
    }

    pub fn format_tool_description(tool_name: &str, tool_input: &Value) -> String {
        let ci = Self::get_input_ci;
        let raw = match tool_name {
            "bash" => {
                let cmd = ci(tool_input, "command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                // Drop the leading `cd` and flatten newlines (#790, #791): raw,
                // this row shows a path and can fuse tokens across a line break.
                let label = crate::utils::command_label::command_label(cmd);
                format!("bash: {}", if label.is_empty() { "?" } else { &label })
            }
            "read_file" | "read" => {
                let path = ci(tool_input, "path")
                    .or_else(|| ci(tool_input, "file_path"))
                    .or_else(|| ci(tool_input, "filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Read {}", path)
            }
            "write_file" | "write" => {
                let path = ci(tool_input, "path")
                    .or_else(|| ci(tool_input, "file_path"))
                    .or_else(|| ci(tool_input, "filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Write {}", path)
            }
            "edit_file" | "edit" => {
                let path = ci(tool_input, "path")
                    .or_else(|| ci(tool_input, "file_path"))
                    .or_else(|| ci(tool_input, "filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Edit {}", path)
            }
            "ls" => {
                let path = ci(tool_input, "path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                format!("ls {}", path)
            }
            "glob" => {
                let pattern = ci(tool_input, "pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Glob {}", pattern)
            }
            "grep" => {
                let pattern = ci(tool_input, "pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let path = ci(tool_input, "path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if path.is_empty() {
                    format!("Grep '{}'", pattern)
                } else {
                    format!("Grep '{}' in {}", pattern, path)
                }
            }
            "lsp" => {
                let op = ci(tool_input, "operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("LSP {}", op)
            }
            "web_search" => {
                let query = ci(tool_input, "query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Search: {}", query)
            }
            "exa_search" => {
                let query = ci(tool_input, "query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("EXA search: {}", query)
            }
            "brave_search" => {
                let query = ci(tool_input, "query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Brave search: {}", query)
            }
            "http_request" => {
                let url = ci(tool_input, "url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let method = ci(tool_input, "method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET");
                format!("{} {}", method, url)
            }
            "execute_code" => {
                let lang = ci(tool_input, "language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let code = ci(tool_input, "code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if code.is_empty() {
                    format!("Execute {}", lang)
                } else {
                    // Show first line of code for context
                    let first_line = code.lines().next().unwrap_or(code);
                    let truncated = if first_line.len() > 80 {
                        format!("{}…", crate::utils::string::truncate_str(first_line, 80))
                    } else {
                        first_line.to_string()
                    };
                    format!("{}: {}", lang, truncated)
                }
            }
            "notebook_edit" => {
                let path = ci(tool_input, "notebook_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Notebook {}", path)
            }
            "parse_document" => {
                let path = ci(tool_input, "path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Parse {}", path)
            }
            "task_manager" => {
                let op = ci(tool_input, "operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Task: {}", op)
            }
            "plan" => {
                let op = ci(tool_input, "operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                match op {
                    "init" => {
                        if let Some(path) = ci(tool_input, "file_path").and_then(|v| v.as_str()) {
                            let name = path.rsplit('/').next().unwrap_or(path);
                            format!("Plan: import '{}' — awaiting approval", name)
                        } else {
                            let name = ci(tool_input, "title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("plan");
                            format!("Plan: create '{}' — awaiting approval", name)
                        }
                    }
                    "add_task" => {
                        let title = ci(tool_input, "title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("task");
                        format!("Plan: add task '{}'", title)
                    }
                    "start" => {
                        let id = ci(tool_input, "task_order")
                            .and_then(|v| v.as_u64())
                            .map(|n| format!("#{n}"))
                            .unwrap_or_else(|| "next".to_string());
                        format!("Plan: start task {}", id)
                    }
                    "complete" => {
                        let id = ci(tool_input, "task_order")
                            .and_then(|v| v.as_u64())
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let action = ci(tool_input, "action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("success");
                        format!("Plan: complete task #{} ({})", id, action)
                    }
                    _ => format!("Plan: {}", op),
                }
            }
            "session_context" => "Session context".to_string(),
            "Agent" | "agent" => {
                let desc = tool_input
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent task");
                format!("Agent: {}", desc)
            }
            other => other.to_string(),
        };
        // Redact any inline secrets (Bearer tokens, api_key=, URL passwords)
        // from the one-line summary so a command like
        // `curl -H "Authorization: Bearer dgr_live_…"` never shows the key
        // in the collapsed tool-call display. The expanded view already
        // redacts via `redact_tool_input`; this closes the same leak in the
        // summary line (2026-06-07 TUI exposure). The agent still runs the
        // real command — only the display is redacted.
        crate::utils::sanitize::redact_command(&raw)
    }

    /// Expand a DB message into one or more DisplayMessages.
    /// Assistant messages may contain tool markers that get reconstructed into ToolCallGroup display messages.
    /// Supports both v1 (`<!-- tools: desc1 | desc2 -->`) and v2 (`<!-- tools-v2: [JSON] -->`) formats.
    /// Extract reasoning blocks from text. Handles:
    /// - `<!-- reasoning -->...<!-- /reasoning -->`
    /// - `<antThinking>...</antThinking>` (Qwen, case-insensitive)
    /// - `<think>...</think>` (DeepSeek)
    ///
    /// Returns (reasoning_text, remaining_text_without_markers).
    fn extract_reasoning(text: &str) -> (Option<String>, String) {
        let mut reasoning_parts = Vec::new();
        let mut remaining = text.to_string();

        // Extract <!-- reasoning --> blocks
        let open_tag = "<!-- reasoning -->";
        let close_tag = "<!-- /reasoning -->";
        while let Some(start) = remaining.find(open_tag) {
            let before = remaining[..start].to_string();
            let after_tag = &remaining[start + open_tag.len()..];
            if let Some(end) = after_tag.find(close_tag) {
                let part = after_tag[..end].trim();
                if !part.is_empty() {
                    reasoning_parts.push(part.to_string());
                }
                remaining = format!("{}{}", before, &after_tag[end + close_tag.len()..]);
            } else {
                let part = after_tag.trim();
                if !part.is_empty() {
                    reasoning_parts.push(part.to_string());
                }
                remaining = before;
                break;
            }
        }

        // Extract <antThinking> blocks (case-insensitive, handles <anTthinking> typo)
        remaining =
            Self::extract_tag_case_insensitive(&remaining, "antthinking", &mut reasoning_parts);

        // Extract <think> blocks (DeepSeek)
        remaining = Self::extract_tag_case_insensitive(&remaining, "think", &mut reasoning_parts);

        // Extract <mm:think> blocks (MiniMax/mimo namespaced think tag)
        remaining =
            Self::extract_tag_case_insensitive(&remaining, "mm:think", &mut reasoning_parts);

        // Extract self-closing tags: <think/>, <antThinking/>, etc.
        // These appear when the model emits thinking with no content or as a flush marker.
        for tag_name in ["think", "antthinking", "mm:think"] {
            let mut cleaned = String::new();
            let mut rest = remaining.as_str();
            let open_self = format!("<{}", tag_name);
            loop {
                let rest_lower = rest.to_lowercase();
                if let Some(start) = rest_lower.find(&open_self) {
                    cleaned.push_str(&rest[..start]);
                    let after = &rest[start + open_self.len()..];
                    // Find the closing />
                    if let Some(end) = after.find("/>") {
                        let inner = after[..end].trim();
                        // If the self-closing tag has content attributes, capture them
                        if !inner.is_empty() && !inner.starts_with('/') {
                            reasoning_parts.push(inner.to_string());
                        }
                        rest = &after[end + 2..];
                    } else {
                        // Malformed — keep the rest as-is
                        cleaned.push_str(&rest[start..]);
                        rest = "";
                        break;
                    }
                } else {
                    break;
                }
            }
            cleaned.push_str(rest);
            remaining = cleaned;
        }

        // Strip orphan closing tags (</think>, </antThinking>, </mm:think>)
        // that leaked when a stream error dropped mid-thinking-block, or when
        // the model emits a bare closer with no open tag. These have no
        // matching open tag so extract_tag_case_insensitive never catches them.
        for tag_name in ["think", "antthinking", "mm:think"] {
            let close = format!("</{}>", tag_name);
            let mut cleaned = String::new();
            let mut rest = remaining.as_str();
            loop {
                let rest_lower = rest.to_lowercase();
                if let Some(start) = rest_lower.find(&close) {
                    cleaned.push_str(&rest[..start]);
                    rest = &rest[start + close.len()..];
                } else {
                    break;
                }
            }
            cleaned.push_str(rest);
            remaining = cleaned;
        }

        let remaining = remaining.trim().to_string();
        if reasoning_parts.is_empty() {
            (None, remaining)
        } else {
            (Some(reasoning_parts.join("\n\n")), remaining)
        }
    }

    /// Extract content between <tag>...</tag> pairs, case-insensitive.
    fn extract_tag_case_insensitive(
        text: &str,
        tag_name: &str,
        reasoning_parts: &mut Vec<String>,
    ) -> String {
        let mut result = text.to_string();
        loop {
            let lower = result.to_lowercase();
            let open = format!("<{}>", tag_name);
            let close = format!("</{}>", tag_name);
            let start_opt = lower.find(&open);
            if let Some(start) = start_opt {
                let before = result[..start].to_string();
                let after_open = &result[start + open.len()..];
                let after_open_lower = &lower[start + open.len()..];
                if let Some(end) = after_open_lower.find(&close) {
                    let part = after_open[..end].trim();
                    if !part.is_empty() {
                        reasoning_parts.push(part.to_string());
                    }
                    result = format!("{}{}", before, &after_open[end + close.len()..]);
                } else {
                    let part = after_open.trim();
                    if !part.is_empty() {
                        reasoning_parts.push(part.to_string());
                    }
                    result = before;
                    break;
                }
            } else {
                break;
            }
        }
        result
    }

    /// Push one row per chronological segment of `region`, preserving the
    /// think → text → think → text layout the live view showed.
    ///
    /// Reload used to emit a SINGLE row per text region, which made a
    /// tool-less turn collapse into one giant message. That message is then
    /// the turn's final answer, and the per-turn fold must always show the
    /// final answer, so a reasoning-heavy turn rendered as a wall after a
    /// restart even though the live view had folded it away (#764).
    ///
    /// The first row emitted keeps the DB message's id, token count and cost;
    /// later rows get fresh ids because they correspond to no row of their own.
    fn push_segments(
        result: &mut Vec<DisplayMessage>,
        region: &str,
        id: Uuid,
        timestamp: chrono::DateTime<chrono::Utc>,
        token_count: Option<i64>,
        cost: Option<f64>,
        first_text: &mut bool,
    ) {
        use crate::tui::app::reasoning_split::{
            BLOCKED_LABEL, Segment, is_intermediate, split_segments,
        };
        let segments = split_segments(region);
        for (i, seg) in segments.iter().enumerate() {
            let (content, details) = match seg {
                // A text segment can still carry inline model tags
                // (`<think>`, `<antThinking>`, `<mm:think>`), which the
                // persist path never wrapped, so it stays on the old lifter.
                Segment::Text(t) => {
                    let (reasoning, clean) = Self::extract_reasoning(t);
                    if is_intermediate(&segments, i) {
                        // Intermediate narration: keep it, collapsed. Merged
                        // with any inline reasoning it carried so one row holds
                        // the whole step.
                        let merged = match reasoning {
                            Some(r) if !clean.is_empty() => format!("{clean}\n\n{r}"),
                            Some(r) => r,
                            None => clean,
                        };
                        if merged.trim().is_empty() {
                            continue;
                        }
                        (String::new(), Some(merged))
                    } else {
                        (clean, reasoning)
                    }
                }
                Segment::Reasoning(r) => (String::new(), Some(r.clone())),
                // Narration the phantom detector refused to deliver (#1172).
                // Collapsed and labelled, never answer text (#1584).
                Segment::Blocked(b) => (String::new(), Some(format!("{BLOCKED_LABEL}\n\n{b}"))),
            };
            if content.is_empty() && details.is_none() {
                continue;
            }
            result.push(DisplayMessage {
                id: if *first_text { id } else { Uuid::new_v4() },
                role: "assistant".to_string(),
                content,
                timestamp,
                token_count: if *first_text { token_count } else { None },
                cost: if *first_text { cost } else { None },
                approval: None,
                approve_menu: None,
                details,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
            *first_text = false;
        }
    }

    fn expand_message(msg: crate::db::models::Message) -> Vec<DisplayMessage> {
        // Compaction markers are stored as a user message containing the full
        // structured summary the LLM uses to recover context. They must stay
        // fully transparent in the TUI — don't render them at all on reload.
        if msg.content.starts_with("[CONTEXT COMPACTION") {
            return vec![];
        }

        let content_lower = msg.content.to_lowercase();
        let has_db_thinking = msg.thinking.as_ref().is_some_and(|t| !t.trim().is_empty());
        let has_tool_call_tag =
            msg.content.contains("\u{fe0f}\u{20e3}") || msg.content.contains("<tool_call>");
        if msg.role != "assistant"
            || (!msg.content.contains("<!-- tools")
                && !msg.content.contains("<!-- reasoning -->")
                && !content_lower.contains("<antthinking")
                && !content_lower.contains("<think")
                && !content_lower.contains("<mm:think")
                && !has_db_thinking
                && !has_tool_call_tag)
        {
            return vec![DisplayMessage::from(msg)];
        }

        // Extract owned values before borrowing content
        let id = msg.id;
        let timestamp = msg.created_at;
        let token_count = msg.token_count;
        let cost = msg.cost;
        let content = msg.content;
        let db_thinking = msg.thinking;

        let mut result = Vec::new();

        // A ledger counts only at the start of a line and only if it parses;
        // a quoted or truncated opener stays text of the block it sits in
        // (#1587).
        let mut remaining = content.as_str();
        let mut first_text = true;
        while let Some(ledger) = super::ledger_scan::next_ledger(remaining) {
            // Text before marker
            let text_before = remaining[..ledger.start].trim();
            if !text_before.is_empty() {
                Self::push_segments(
                    &mut result,
                    text_before,
                    id,
                    timestamp,
                    token_count,
                    cost,
                    &mut first_text,
                );
            }
            let is_v2 = ledger.is_v2;
            let tools_str = ledger.body;
            let close_end = ledger.end;

            let calls: Vec<ToolCallEntry> = if is_v2 {
                // v2: parse JSON array with descriptions, success, output, and tool input
                serde_json::from_str::<Vec<serde_json::Value>>(tools_str)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|entry| {
                        let desc = entry["d"].as_str().unwrap_or("?").to_string();
                        let success = entry["s"].as_bool().unwrap_or(true);
                        let output = entry["o"]
                            .as_str()
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty());
                        let tool_input = entry.get("i").cloned().unwrap_or(serde_json::Value::Null);
                        ToolCallEntry {
                            task_id: None,
                            description: desc,
                            success,
                            details: output,
                            completed: true,
                            tool_input,
                        }
                    })
                    .collect()
            } else {
                // v1: plain descriptions, no output
                tools_str
                    .split(" | ")
                    .map(|desc| ToolCallEntry {
                        task_id: None,
                        description: desc.to_string(),
                        success: true,
                        details: None,
                        completed: true,
                        tool_input: serde_json::Value::Null,
                    })
                    .collect()
            };

            if !calls.is_empty() {
                let count = calls.len();
                result.push(DisplayMessage {
                    id: Uuid::new_v4(),
                    role: "tool_group".to_string(),
                    content: format!("{} tool call{}", count, if count == 1 { "" } else { "s" }),
                    timestamp,
                    token_count: None,
                    cost: None,
                    approval: None,
                    approve_menu: None,
                    details: None,
                    expanded: false,
                    expanded_full: false,
                    tool_group: Some(ToolCallGroup {
                        calls,
                        expanded: false,
                    }),
                    duration_secs: None,
                });
            }
            remaining = &remaining[close_end..];
        }

        // Any remaining text after the last marker
        let trailing = remaining.trim();
        if !trailing.is_empty() {
            Self::push_segments(
                &mut result,
                trailing,
                id,
                timestamp,
                token_count,
                cost,
                &mut first_text,
            );
        }

        if result.is_empty() {
            // Content was only tool markers with no text — show a placeholder
            result.push(DisplayMessage {
                id,
                role: "assistant".to_string(),
                content: String::new(),
                timestamp,
                token_count,
                cost,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
        }

        // Apply thinking from DB `thinking` column (non-CLI providers).
        // If no DisplayMessage has `details` set yet but we have persisted
        // thinking, attach it to the first text-based assistant message.
        if let Some(ref thinking) = db_thinking
            && !thinking.trim().is_empty()
        {
            let has_details = result.iter().any(|dm| dm.details.is_some());
            if !has_details {
                // Find the first assistant text message to attach thinking to
                if let Some(first_text) = result
                    .iter_mut()
                    .find(|dm| dm.role == "assistant" && !dm.content.is_empty())
                {
                    first_text.details = Some(thinking.clone());
                } else if let Some(first) = result.first_mut() {
                    // Even if it's a tool-only message, attach thinking
                    first.details = Some(thinking.clone());
                }
            }
        }

        result
    }

    /// Extract image file paths from text and return (remaining_text, attachments).
    /// Handles paths with spaces (e.g. `/home/user/My Screenshots/photo.png`)
    /// and image URLs.
    ///
    /// Strip terminal escape sequences (CSI, SGR mouse, OSC, etc.), raw ETX
    /// bytes and ESC-less mouse-report bursts from text.
    ///
    /// These leak into paste events when switching terminal focus while mouse
    /// capture is active (e.g. tmux pane switch, alt-tab with iTerm2). When a
    /// burst is split across reads the leading `\x1b` is consumed elsewhere
    /// and the remainder arrives as plain text (`[<35;10;102M[<35;11;102M`);
    /// the ESC-anchored walk alone would pass those through verbatim, so the
    /// burst pass below catches them (#1943).
    pub(crate) fn strip_terminal_escapes(text: &str) -> String {
        let bytes = text.as_bytes();
        // Fast path: no ESC, no ETX and no bracket means nothing to strip
        if !bytes.contains(&0x1b) && !bytes.contains(&0x03) && !bytes.contains(&b'[') {
            return text.to_string();
        }

        let mut out = String::with_capacity(text.len());
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if bytes[i] == 0x03 {
                // ETX (Ctrl+C) leaked as a raw byte: a split burst leaves
                // some terminals to hand it through as a typed char and it
                // is never legitimate input text (#1943).
                i += 1;
                continue;
            }
            if bytes[i] == 0x1b {
                // ESC — start of escape sequence (always single-byte ASCII)
                i += 1;
                if i >= len {
                    break;
                }
                match bytes[i] {
                    b'[' => {
                        // CSI sequence: ESC [ ... (final byte in 0x40-0x7E)
                        i += 1;
                        while i < len && !(0x40..=0x7E).contains(&bytes[i]) {
                            i += 1;
                        }
                        if i < len {
                            i += 1; // skip the final byte
                        }
                    }
                    b']' => {
                        // OSC sequence: ESC ] ... ST (ST = ESC \ or BEL 0x07)
                        i += 1;
                        while i < len {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b && i + 1 < len && bytes[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => {
                        // Two-byte escape (e.g. ESC =, ESC >). Only an ASCII
                        // byte is consumed: a multi-byte char has to stay at
                        // `i` so the next iteration copies it whole. Stepping
                        // into the middle of one made the copy below panic
                        // (#2247).
                        if bytes[i].is_ascii() {
                            i += 1;
                        }
                    }
                }
            } else {
                // Normal byte — figure out how many bytes this UTF-8 char is
                // and copy the whole character to preserve multi-byte (emoji, CJK, etc.)
                let ch_len = utf8_char_len(bytes[i]);
                // The boundary check is belt and braces: an escape arm can
                // leave `i` inside a character, and slicing there panics
                // (#2247).
                if i + ch_len <= len
                    && text.is_char_boundary(i)
                    && text.is_char_boundary(i + ch_len)
                {
                    out.push_str(&text[i..i + ch_len]);
                }
                i += ch_len;
            }
        }
        Self::strip_mouse_report_bursts(&out)
    }

    /// Drop SGR (1006) and URXVT (1015) mouse-report bursts that arrive
    /// without their leading `\x1b`.
    ///
    /// When an escape read is split mid-burst, crossterm never sees the ESC
    /// and each report lands as ordinary characters: `[<35;10;102M` (SGR)
    /// or `[35;10;102M` (URXVT). Those strings are exactly what poisoned
    /// the input buffer for ~22 minutes in the #1943 incident: the typed
    /// `/cmd` ended up buried behind a wall of bursts, so slash dispatch
    /// (`input.starts_with('/')`) never matched and no command could run.
    ///
    /// Matching is deliberately strict so bracketed text stays intact:
    /// - SGR: `[<` + at least one digit and two `;` segments + final `M`/`m`
    /// - URXVT: same coordinate shape without the `<`
    /// - no terminator (`[<35;10;102`) or too few segments (`[3;4]`) is
    ///   ordinary text and passes through untouched.
    pub(crate) fn strip_mouse_report_bursts(text: &str) -> String {
        let bytes = text.as_bytes();
        if !bytes.contains(&b'[') {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'['
                && let Some(len) = Self::mouse_report_len(bytes, i)
            {
                i += len;
                continue;
            }
            let ch_len = utf8_char_len(bytes[i]);
            if i + ch_len <= bytes.len()
                && text.is_char_boundary(i)
                && text.is_char_boundary(i + ch_len)
            {
                out.push_str(&text[i..i + ch_len]);
            }
            i += ch_len;
        }
        out
    }

    /// Byte length of a mouse-report burst starting at `start` (which must
    /// point at `[`), or None when the text there is not a complete report.
    fn mouse_report_len(bytes: &[u8], start: usize) -> Option<usize> {
        let mut j = start + 1;
        let sgr = bytes.get(j) == Some(&b'<');
        if sgr {
            j += 1;
        }
        let mut digits = 0usize;
        let mut separators = 0usize;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            if bytes[j].is_ascii_digit() {
                digits += 1;
            } else {
                separators += 1;
            }
            j += 1;
        }
        // Real reports carry button;col;row: digits and both `;` segments,
        // closed by a press/release final byte. Anything shorter is far more
        // likely bracketed text than a burst, so it stays.
        if !matches!(bytes.get(j), Some(b'M') | Some(b'm')) {
            return None;
        }
        if digits > 0 && separators >= 2 {
            return Some(j + 1 - start);
        }
        None
    }

    /// Normalize invisible/zero-width Unicode characters that web pages use
    /// for table formatting. These paste as invisible bytes in terminals,
    /// gluing columns together.
    pub(crate) fn normalize_unicode_whitespace(text: &str) -> String {
        text.chars()
            .map(|ch| match ch {
                // Non-breaking spaces (common in HTML tables)
                '\u{00A0}' | '\u{202F}' | '\u{2007}' => ' ',
                // Zero-width spaces / joiners — remove entirely
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => '\0',
                // Various width spaces — normalize to regular space
                '\u{2000}'..='\u{200A}' | '\u{205F}' | '\u{3000}' => ' ',
                // Soft hyphen — remove
                '\u{00AD}' => '\0',
                _ => ch,
            })
            .filter(|&ch| ch != '\0')
            .collect()
    }

    /// Expand tab characters to spaces (8 spaces per tab stop — standard terminal width).
    /// Terminal TUIs can't render tab stops, so pasted table data with `\t`
    /// collapses to zero-width, gluing columns together. This preserves visual
    /// alignment from web pages and spreadsheets.
    pub(crate) fn expand_tabs(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut col = 0usize;
        for ch in text.chars() {
            if ch == '\t' {
                // Advance to next tab stop (every 8 columns — standard terminal)
                let spaces = 8 - (col % 8);
                for _ in 0..spaces {
                    result.push(' ');
                }
                col += spaces;
            } else {
                result.push(ch);
                if ch == '\n' {
                    col = 0;
                } else {
                    col += 1;
                }
            }
        }
        result
    }

    /// Text file paths (`.txt`, `.md`, `.json`, source code, etc.) are read from
    /// disk and inlined into the returned text — no attachment needed.
    /// Resolve a drag-dropped path to its real on-disk form.
    ///
    /// Terminals shell-escape dropped paths: spaces become `\ `, and the
    /// whole thing may be wrapped in quotes. The raw string therefore fails
    /// `Path::exists()` even though the file is right there — which is why a
    /// screenshot like `Screenshot\ 2026.png` silently never attached.
    ///
    /// We try the raw string first (so Windows backslash *separators* are
    /// never mangled), then an unquoted form, then a POSIX-unescaped form,
    /// returning the first that actually exists. `None` means no real file.
    pub(crate) fn resolve_dropped_path(raw: &str) -> Option<String> {
        if raw.is_empty() {
            return None;
        }
        if std::path::Path::new(raw).exists() {
            return Some(raw.to_string());
        }
        // Strip one layer of matching surrounding quotes.
        let unquoted = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw);
        if unquoted != raw && std::path::Path::new(unquoted).exists() {
            return Some(unquoted.to_string());
        }
        // POSIX shell unescape: drop the backslash before any escaped char
        // (`\ `, `\(`, `\&`, …). Only used when the result actually exists,
        // so a genuine backslash in a path is never wrongly stripped.
        if unquoted.contains('\\') {
            let mut unescaped = String::with_capacity(unquoted.len());
            let mut chars = unquoted.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        unescaped.push(next);
                    }
                } else {
                    unescaped.push(c);
                }
            }
            if std::path::Path::new(&unescaped).exists() {
                return Some(unescaped);
            }
        }
        None
    }

    /// Pull a file from the client over the drop tunnel and store it where
    /// the rest of the attachment pipeline expects a path (#1289).
    ///
    /// Lands in `<home>/tmp/`, beside pasted clipboard images, because that is
    /// what this is: a TUI-side attachment materialised locally. NOT in
    /// `channel_attachments/`, which the system prompt defines as files sent
    /// or forwarded in a chat channel, and which is keyed by platform. A drop
    /// is neither, and filing it there would tell the agent something untrue.
    ///
    /// Named after the client's file so the directory stays readable, with a
    /// timestamp spliced in only on collision (#1311): two screenshots called
    /// `Screenshot.png` must not clobber each other, but the ordinary case
    /// should be a file you can recognise.
    fn pull_dropped_file(port: u16, client_path: &str) -> anyhow::Result<PulledDrop> {
        let bytes = crate::utils::drop_agent::fetch(port, client_path)?;
        let dir = crate::config::opencrabs_home().join("tmp");
        std::fs::create_dir_all(&dir)?;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dest =
            crate::utils::drop_landing::landing_path(&dir, client_path, stamp, |p| p.exists());
        std::fs::write(&dest, &bytes)?;
        tracing::info!(
            "drop tunnel: pulled {} ({} bytes) to {}",
            client_path,
            bytes.len(),
            dest.display()
        );
        Ok(PulledDrop {
            name: crate::utils::drop_landing::client_file_name(client_path),
            path: dest.to_string_lossy().to_string(),
            bytes: bytes.len(),
        })
    }

    /// Scan `text` for dropped media paths and turn them into attachments,
    /// with a receipt for every file that had to be copied onto this machine.
    pub(crate) fn extract_attachments(text: &str) -> Extraction {
        let mut notices: Vec<String> = Vec::new();
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // Case 1: Entire pasted text is a single image OR video path
        // (handles spaces in path)
        let is_image_single = IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
        let is_video_single = VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
        if is_image_single || is_video_single {
            // Local path — resolve the shell-escaped drag-drop form (e.g.
            // `Screenshot\ 1.png`) to the real file before attaching.
            if let Some(real) = Self::resolve_dropped_path(trimmed) {
                let name = std::path::Path::new(&real)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| real.clone());
                return Extraction {
                    text: String::new(),
                    attachments: vec![ImageAttachment {
                        name,
                        path: real,
                        is_video: is_video_single,
                    }],
                    notices,
                };
            }
            // URL (no spaces — just check prefix)
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                let name = trimmed.rsplit('/').next().unwrap_or(trimmed).to_string();
                return Extraction {
                    text: String::new(),
                    attachments: vec![ImageAttachment {
                        name,
                        path: trimmed.to_string(),
                        is_video: is_video_single,
                    }],
                    notices,
                };
            }
        }

        // Case 1b: Entire pasted text is a single real file path that is not
        // an image or video. Route through the shared classifier (#1740, #1743)
        // so paste, drag-drop and Ctrl+V all agree on what a file is, and any
        // extension surfaces as an attachment instead of silently staying
        // prose (#1740).
        if let Some(real) = Self::resolve_dropped_path(trimmed) {
            let class = attachment_class_for_path(&real);
            let name = std::path::Path::new(&real)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| real.clone());
            match class {
                // Defensive: Case 1 above already claimed image/video paths,
                // but if the two ever drift, honor the classifier rather than
                // dropping the file on the floor.
                AttachmentClass::Image | AttachmentClass::Video => {
                    return Extraction {
                        text: String::new(),
                        attachments: vec![ImageAttachment {
                            name,
                            path: real,
                            is_video: class == AttachmentClass::Video,
                        }],
                        notices,
                    };
                }
                AttachmentClass::Document => {
                    // Binary formats — never inline bytes. Surface the real
                    // path and point the agent at the parsing tools so it can
                    // actually read/see it.
                    let is_pdf = real.to_lowercase().ends_with(".pdf");
                    let how = if is_pdf {
                        format!(
                            "Call `parse_document(path='{real}')` to read its text, or \
                             `pdf_to_images(path='{real}', page_range='N')` then `analyze_image` \
                             for figures/screenshots/scanned pages."
                        )
                    } else {
                        format!("Call `parse_document(path='{real}')` to read it.")
                    };
                    return Extraction {
                        text: format!("[User attached a document: {name} ({real}). {how}]"),
                        attachments: vec![],
                        notices,
                    };
                }
                AttachmentClass::TextFile => {
                    let path = std::path::Path::new(&real);
                    if let Ok(content) = std::fs::read_to_string(path) {
                        const LIMIT: usize = 8_000;
                        let truncated = if content.len() > LIMIT {
                            let safe: String = content.chars().take(LIMIT).collect();
                            format!("{}…[truncated]", safe)
                        } else {
                            content
                        };
                        return Extraction {
                            text: format!("[File: {}]\n```\n{}\n```", name, truncated),
                            attachments: vec![],
                            notices,
                        };
                    }
                    // Read failed (permissions, invalid UTF-8): fall through
                    // to the mixed-text scan, same as before the router.
                }
                AttachmentClass::Audio => {
                    return Extraction {
                        text: format!(
                            "[User attached an audio file: {name} ({real}). \
                             It is not transcribed; transcribe it if the request \
                             needs its content.]"
                        ),
                        attachments: vec![],
                        notices,
                    };
                }
                AttachmentClass::Archive => {
                    return Extraction {
                        text: format!(
                            "[User attached an archive: {name} ({real}). \
                             It is not extracted; unzip/untar it if the request \
                             needs its contents.]"
                        ),
                        attachments: vec![],
                        notices,
                    };
                }
                AttachmentClass::UnknownBinary => {
                    return Extraction {
                        text: format!(
                            "[User attached a file: {name} ({real}). \
                             Unknown type; inspect it with the tools that fit.]"
                        ),
                        attachments: vec![],
                        notices,
                    };
                }
            }
        }

        // #1740: a path-shaped whole message that failed to resolve means the
        // user pasted or dropped a file that is not on this machine (moved,
        // deleted, not synced). Say so instead of letting it flow as prose.
        // Media extensions are excluded: Case 2's scan below already emits
        // its own receipt for those, and two notices for one path is noise.
        let media_ext = IMAGE_EXTENSIONS
            .iter()
            .chain(VIDEO_EXTENSIONS.iter())
            .any(|ext| lower.ends_with(ext));
        let looks_like_path = trimmed.contains('/')
            && std::path::Path::new(trimmed)
                .extension()
                .is_some_and(|e| !e.is_empty());
        if !media_ext && looks_like_path {
            tracing::warn!("Paste/drop path did not resolve on this machine: {trimmed}");
            notices.push(format!("[Path not found on this machine: {trimmed}]"));
        }

        // Case 2: Mixed text.
        //
        // A dropped path can contain spaces, and macOS names every screenshot
        // that way, so the whitespace scan below can never see one: it
        // shatters into fragments and the only one carrying the extension is
        // tested as a relative path and fails (#1288). Anchor on the
        // extension instead and pull those out first, longest-match wins.
        let mut attachments = Vec::new();
        let mut text = std::borrow::Cow::Borrowed(text);
        {
            use super::dropped_path::{self, Dropped};
            let media: Vec<&str> = IMAGE_EXTENSIONS
                .iter()
                .chain(VIDEO_EXTENSIONS.iter())
                .copied()
                .collect();

            // Collect over the ORIGINAL text, then rewrite right-to-left so
            // earlier byte ranges stay valid. Rewriting as we go would also
            // let an "unavailable" marker match itself on the next pass.
            let mut hits: Vec<(usize, usize, Dropped)> = Vec::new();
            let mut from = 0usize;
            while from < text.len() {
                let Some(hit) = dropped_path::find(&text[from..], &media) else {
                    break;
                };
                let (s, e) = hit.range();
                hits.push((from + s, from + e, hit));
                from += e;
            }

            if !hits.is_empty() {
                let mut rewritten = text.to_string();
                for (start, end, hit) in hits.into_iter().rev() {
                    match hit {
                        Dropped::Here { path, .. } => {
                            let lower = path.to_lowercase();
                            let is_video = VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.clone());
                            attachments.push(ImageAttachment {
                                name,
                                path,
                                is_video,
                            });
                            rewritten.replace_range(start..end, "");
                        }
                        // The path is real, just not on this machine: a drop
                        // from a laptop into a session running over SSH. Say
                        // so in the message itself: forwarding it as prose
                        // sent the agent hunting through the attachments dir
                        // and cost a whole turn before it worked that out.
                        //
                        // If the user opened the reverse tunnel, pull the file
                        // across the SSH connection they already made and
                        // attach the local copy like any other drop (#1289).
                        // Over SSH the default port is probed even without
                        // OPENCRABS_DROP_PORT, so the documented `ssh -R`
                        // alias is enough on its own; only a DECLARED tunnel
                        // that fails is reported as an error (#1311).
                        Dropped::Elsewhere { path, .. } => {
                            let attempt = crate::utils::drop_transfer::tunnel()
                                .map(|t| (t, Self::pull_dropped_file(t.port, &path)));
                            match attempt {
                                Some((_, Ok(pulled))) => {
                                    let lower = pulled.path.to_lowercase();
                                    let is_video =
                                        VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
                                    notices.push(crate::utils::drop_landing::pulled_notice(
                                        &pulled.name,
                                        &pulled.path,
                                        pulled.bytes,
                                    ));
                                    attachments.push(ImageAttachment {
                                        name: pulled.name,
                                        path: pulled.path,
                                        is_video,
                                    });
                                    rewritten.replace_range(start..end, "");
                                }
                                Some((tunnel, Err(e))) if tunnel.declared => {
                                    // Never silently claim an attachment that
                                    // did not arrive.
                                    tracing::warn!("drop tunnel: {path}: {e:#}");
                                    rewritten.replace_range(
                                        start..end,
                                        &format!(
                                            "[attachment unavailable: could not pull {path} \
                                             over the drop tunnel: {e}]"
                                        ),
                                    );
                                }
                                other => {
                                    if let Some((tunnel, Err(e))) = other {
                                        tracing::debug!(
                                            "drop tunnel: nothing answering the probe on port {}: \
                                             {e:#}; falling back to copy guidance",
                                            tunnel.port
                                        );
                                    }
                                    // Name the transfer that actually works for
                                    // this terminal (#1289) rather than only
                                    // reporting the absence: over SSH the file
                                    // IS reachable, just not from here.
                                    let env = crate::tui::remote_upload::Env::current();
                                    // Same destination a tunnel-pulled file
                                    // lands in, so copying by hand and copying
                                    // automatically put the file in the same
                                    // place.
                                    let dest = crate::config::opencrabs_home().join("tmp");
                                    let advice = crate::tui::remote_upload::guidance(
                                        &env,
                                        &path,
                                        &dest.to_string_lossy(),
                                    );
                                    rewritten.replace_range(
                                        start..end,
                                        &format!("[attachment unavailable: {advice}]"),
                                    );
                                }
                            }
                        }
                    }
                }
                text = std::borrow::Cow::Owned(rewritten);
            }
        }
        let text: &str = &text;

        let mut remaining_parts = Vec::new();
        let mut inlined_files: Vec<String> = Vec::new();

        for word in text.split_whitespace() {
            let word_lower = word.to_lowercase();
            let is_image = IMAGE_EXTENSIONS.iter().any(|ext| word_lower.ends_with(ext));
            let is_video = VIDEO_EXTENSIONS.iter().any(|ext| word_lower.ends_with(ext));

            if is_image || is_video {
                let path = std::path::Path::new(word);
                if path.exists() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| word.to_string());
                    attachments.push(ImageAttachment {
                        name,
                        path: word.to_string(),
                        is_video,
                    });
                    continue;
                }
                if word.starts_with("http://") || word.starts_with("https://") {
                    let name = word.rsplit('/').next().unwrap_or(word).to_string();
                    attachments.push(ImageAttachment {
                        name,
                        path: word.to_string(),
                        is_video,
                    });
                    continue;
                }
            }

            // Document paths (space-free in mixed-text mode): surface the
            // path + parsing hint rather than inlining binary bytes.
            if DOC_EXTENSIONS.iter().any(|ext| word_lower.ends_with(ext))
                && std::path::Path::new(word).exists()
            {
                let name = std::path::Path::new(word)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| word.to_string());
                let how = if word_lower.ends_with(".pdf") {
                    format!(
                        "Call `parse_document(path='{word}')` for text, or \
                         `pdf_to_images(path='{word}', page_range='N')` + `analyze_image` for visuals."
                    )
                } else {
                    format!("Call `parse_document(path='{word}')` to read it.")
                };
                inlined_files.push(format!(
                    "[User attached a document: {name} ({word}). {how}]"
                ));
                continue;
            }

            // Check for text file paths (space-free paths only in mixed-text mode)
            let is_text = TEXT_EXTENSIONS.iter().any(|ext| word_lower.ends_with(ext));
            if is_text {
                let path = std::path::Path::new(word);
                if path.exists()
                    && let Ok(content) = std::fs::read_to_string(path)
                {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| word.to_string());
                    const LIMIT: usize = 8_000;
                    let truncated = if content.len() > LIMIT {
                        let safe: String = content.chars().take(LIMIT).collect();
                        format!("{}…[truncated]", safe)
                    } else {
                        content
                    };
                    inlined_files.push(format!("[File: {}]\n```\n{}\n```", name, truncated));
                    continue;
                }
            }

            remaining_parts.push(word);
        }

        let mut result = remaining_parts.join(" ");
        for file_content in inlined_files {
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(&file_content);
        }
        Extraction {
            text: result,
            attachments,
            notices,
        }
    }

    /// Replace `<<IMG:/path>>` and `<<VID:/path>>` markers with readable
    /// `[IMG: file.png]` / `[VID: clip.mp4]` for display.
    pub(crate) fn humanize_image_markers(text: &str) -> String {
        Self::humanize_marker(
            &Self::humanize_marker(text, "<<IMG:", "IMG"),
            "<<VID:",
            "VID",
        )
    }

    /// Generic marker humanizer used by `humanize_image_markers` for both
    /// image and video markers — same behaviour, different prefix/label.
    fn humanize_marker(text: &str, marker_open: &str, label: &str) -> String {
        let mut result = text.to_string();
        let prefix_len = marker_open.len();
        while let Some(start) = result.find(marker_open) {
            if let Some(end) = result[start..].find(">>") {
                let path = &result[start + prefix_len..start + end];
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string());
                let replacement = format!("[{}: {}]", label, name);
                result = format!(
                    "{}{}{}",
                    &result[..start],
                    replacement,
                    &result[start + end + 2..]
                );
            } else {
                break;
            }
        }
        result.trim().to_string()
    }

    /// Push a system message into the chat display
    pub(crate) fn push_system_message(&mut self, content: String) {
        self.messages.push(DisplayMessage {
            id: Uuid::new_v4(),
            role: "system".to_string(),
            content,
            timestamp: chrono::Utc::now(),
            token_count: None,
            cost: None,
            approval: None,
            approve_menu: None,
            details: None,
            expanded: false,
            expanded_full: false,
            tool_group: None,
            duration_secs: None,
        });
        // Only scroll to bottom if user hasn't scrolled up manually
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }

    /// Send a message to the agent
    pub(crate) async fn send_message(&mut self, content: String) -> Result<()> {
        self.send_message_inner(content, false).await
    }

    /// Send a prompt that came from a slash command, skill, or user-command
    /// expansion. Skips the prompt analyzer: soft-nudge is for natural-language
    /// user chat only, not for prose a command or skill author wrote.
    pub(crate) async fn send_command_message(&mut self, content: String) -> Result<()> {
        self.send_message_inner(content, true).await
    }

    async fn send_message_inner(&mut self, content: String, command_sourced: bool) -> Result<()> {
        // A new turn is starting — drop any follow-up suggestions from the
        // previous turn so they can't reappear on the next empty input (#596).
        self.followup_suggestions.clear();
        self.followup_selected_index = 0;
        tracing::info!(
            "[send_message] START is_processing={} has_session={} content_len={}",
            self.is_processing,
            self.current_session.is_some(),
            content.len()
        );

        // On every new user message, refresh the in-memory plan from disk.
        // Editing and Active plans both persist across user messages (an
        // Editing design doc must survive follow-up chat; an idle Active
        // checklist, including a seed-failed empty one, is still live).
        // Terminal plans no longer linger on disk — completing archives and
        // discarding deletes — so a vanished file is the only "moved on"
        // signal, and the shared loader resolves legacy terminal statuses.
        if self.plan_document.is_some() {
            self.reload_plan();
        }

        // Deny stale pending approvals so they don't block streaming
        let stale_count = self
            .messages
            .iter()
            .filter(|m| {
                m.approval
                    .as_ref()
                    .is_some_and(|a| a.state == ApprovalState::Pending)
            })
            .count();
        if stale_count > 0 {
            tracing::warn!(
                "[send_message] Clearing {} stale pending approvals",
                stale_count
            );
        }
        for msg in &mut self.messages {
            if let Some(ref mut approval) = msg.approval
                && approval.state == ApprovalState::Pending
            {
                let _ = approval.response_tx.send(ToolApprovalResponse {
                    request_id: approval.request_id,
                    approved: false,
                    reason: Some("Superseded".to_string()),
                });
                approval.state = ApprovalState::Denied("Superseded".to_string());
            }
        }

        if let Some(session) = &self.current_session
            && self.processing_sessions.contains(&session.id)
        {
            tracing::warn!(
                "[send_message] QUEUED — session {} still processing",
                session.id
            );

            // Re-submitting the message already being answered produces a
            // second identical turn whose cost is paid on every later turn of
            // the session, since both copies reload into context (#798). The
            // usual cause is Arrow Up then Enter while the first is still
            // running. Compare only against the turn in flight and the pending
            // queue, never further back, so a deliberate repeat later in the
            // session is untouched.
            let sid = session.id;
            let in_flight = self
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone());
            let already_queued: Vec<String> = self
                .queued_messages
                .lock()
                .ok()
                .and_then(|q| {
                    q.get(&sid)
                        .map(|msgs| msgs.iter().map(|m| m.display_text.clone()).collect())
                })
                .unwrap_or_default();
            let verdict =
                super::duplicate_submit::classify(&content, in_flight.as_deref(), &already_queued);
            if verdict.is_duplicate() {
                tracing::info!(
                    "[send_message] DROPPED duplicate submission for session {sid} ({verdict:?}) \
                     — the first copy is still being answered"
                );
                self.cursor_position = self.input_buffer.len();
                return Ok(());
            }

            // Stack queued messages — multiple sends accumulate. The same
            // map serves the UI's "Queued" indicator AND the agent's
            // mid-tool inject callback (keyed by session id), so we only
            // need to write here once.
            if let Some(sid) = self.current_session.as_ref().map(|s| s.id)
                && let Ok(mut q) = self.queued_messages.lock()
            {
                // Typed by the user, so there is no synthetic framing to hide:
                // context and display are the same words.
                q.entry(sid)
                    .or_default()
                    .push(crate::brain::agent::QueuedUserMessage::plain(content));
            }
            self.cursor_position = self.input_buffer.len();
            return Ok(());
        }
        // Drain this session's pending context hints (/cd and friends) and
        // prepend them. Per-session (#2006): a hint queued in another pane
        // must not ride along on this turn. Hoisted above the `session` borrow
        // below, which stays live until the turn is dispatched, so the drain
        // cannot take `&mut self` inside it.
        let mut transformed_content = content.clone();
        if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
            let hints = self.take_pending_context_for(sid);
            if !hints.is_empty() {
                transformed_content = format!("{}\n\n{}", hints.join("\n"), transformed_content);
            }
        }

        if let Some(session) = &self.current_session {
            self.processing_sessions.insert(session.id);
            self.is_processing = true;
            self.processing_started_at = Some(std::time::Instant::now());
            self.streaming_output_tokens = 0;
            self.error_message = None;
            self.error_message_shown_at = None;
            self.intermediate_text_received = false;

            // Soft-nudge: append LLM-only tool hints when the USER's own text
            // matches keyword families. Natural-language chat only: command,
            // skill, and user-command expansions plus system triggers are
            // never analyzed. Hints go to the agent string; the chat bubble
            // below renders `content` untouched.
            if !command_sourced && crate::utils::prompt_analyzer::is_natural_chat(&content) {
                // Plan keywords enter Plan mode durably: the pre-init
                // Editing flag survives restarts and arms the write gate
                // until `plan init` (or /discard). A live plan refuses the
                // flag; that's expected.
                if self.prompt_analyzer.plan_intent(&content)
                    && let Some(sid) = self.current_session.as_ref().map(|s| s.id)
                    && let Err(e) = crate::utils::plan_files::set_pre_init_editing(sid).await
                {
                    tracing::debug!("Plan-keyword pre-init skipped (plan already live): {e}");
                }
                if let Some(hints) = self.prompt_analyzer.hints_for(&content) {
                    tracing::info!("✨ Prompt transformed with tool hints");
                    transformed_content.push_str(&hints);
                }
            }

            // Add user message to UI — skip internal system triggers (e.g. /compact)
            let is_system_trigger = content.starts_with("[SYSTEM:");
            if !is_system_trigger {
                let display_content = Self::humanize_image_markers(&content);

                // Dedup: if the tail of the conversation is an unpaired user
                // message with identical content (previous request failed or
                // was cancelled with no assistant response), remove the stale
                // one before pushing.  We only need to check the LAST message:
                // if it's still a user message, it was never responded to.
                let prev_is_duplicate = self
                    .messages
                    .last()
                    .is_some_and(|last| last.role == "user" && last.content == display_content);
                if prev_is_duplicate {
                    self.messages.pop();
                }

                let user_msg = DisplayMessage {
                    id: Uuid::new_v4(),
                    role: "user".to_string(),
                    content: display_content,
                    timestamp: chrono::Utc::now(),
                    token_count: None,
                    cost: None,
                    approval: None,
                    approve_menu: None,
                    details: None,
                    expanded: false,
                    expanded_full: false,
                    tool_group: None,
                    duration_secs: None,
                };
                self.messages.push(user_msg);
            }

            // This turn's rows begin here. Anything above is durable history,
            // and cancel-persist must not re-collect it (#2127). Set for system
            // triggers too: they skip the user row, so without an explicit
            // anchor the reconstruction would reach back into earlier turns.
            self.turn_start_len = self.messages.len();

            // Auto-scroll to show the new user message and re-enable auto-scroll
            self.auto_scroll = true;
            self.scroll_offset = 0;

            // ── Fast-cancel: "stop" / "/stop" exact match kills active task immediately ──
            // Same logic as Esc×2 but triggered by the user typing stop.
            // Runs BEFORE spawning a new agent task so nothing else fires.
            {
                let trimmed = transformed_content.trim();
                if self.is_processing
                    && (trimmed.eq_ignore_ascii_case("stop") || trimmed == "/stop")
                {
                    if let Some(ref session) = self.current_session {
                        self.persist_streaming_state(session.id).await;
                    }
                    if let Some(token) = self.cancel_token.take() {
                        token.cancel();
                    }
                    if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
                        // Only this session's task: the handle used to be a
                        // single app-level slot, so "stop" in one session could
                        // abort a turn running in another (#2155).
                        self.abort_session_task(sid);
                        if let Some(stashed) = self.session_cancel_tokens.remove(&sid) {
                            stashed.cancel();
                        }
                        self.processing_sessions.remove(&sid);
                    }
                    self.is_processing = false;
                    self.processing_started_at = None;
                    self.streaming_response = None;
                    self.streaming_reasoning = None;
                    self.streaming_render_cache = None;
                    self.cancel_token = None;
                    tracing::info!("Fast-cancel: active task killed by stop command");
                    return Ok(());
                }
            }

            // Create cancellation token for this request
            let token = CancellationToken::new();
            self.cancel_token = Some(token.clone());

            // Send transformed content to agent in background
            let agent_service = self.agent_service.clone();
            let session_id = session.id;
            let event_sender = self.event_sender();

            tracing::info!(
                "[send_message] Spawning agent task for session {}",
                session_id
            );
            let abort_event_sender = event_sender.clone();
            let handle = tokio::spawn(async move {
                tracing::info!("[agent_task] START calling send_message_with_tools_and_mode");
                let result = agent_service
                    .send_message_with_tools_and_mode(
                        session_id,
                        transformed_content,
                        None,
                        Some(token),
                    )
                    .await;

                match result {
                    Ok(response) => {
                        tracing::info!("[agent_task] OK — sending ResponseComplete");
                        if let Err(e) = event_sender.send(TuiEvent::ResponseComplete {
                            session_id,
                            response,
                        }) {
                            tracing::error!("[agent_task] FAILED to send ResponseComplete: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::error!("[agent_task] ERROR: {}", e);
                        // Translate the raw error into a user-facing message
                        // via the shared helper (`brain::agent::format_user_error`).
                        // It handles 5xx-exhaustion, rate limits, context-too-large,
                        // stream-broken, repetition-loop, etc. Both this TUI
                        // path and the channel handlers use the same helper, so
                        // a user on Telegram sees the same wording as a user on
                        // the TUI when self-heal exhausts and the turn dies.
                        let user_message = crate::brain::agent::format_user_error(&e);
                        if let Err(e2) = event_sender.send(TuiEvent::Error {
                            session_id,
                            message: user_message,
                        }) {
                            tracing::error!("[agent_task] FAILED to send Error event: {}", e2);
                        }
                    }
                }
            });
            // Store this session's abort handle so double-Escape hard-kills
            // only the turn it is meant to kill (#2155).
            self.task_abort_handles
                .insert(session_id, handle.abort_handle());

            // Watch for panics — surface them in the UI instead of silent hang
            tokio::spawn(async move {
                match handle.await {
                    Err(e) if e.is_panic() => {
                        tracing::error!("[agent_task] PANICKED: {}", e);
                        let _ = abort_event_sender.send(TuiEvent::Error {
                            session_id,
                            message: format!(
                                "Agent task crashed unexpectedly: {e}. You can continue chatting."
                            ),
                        });
                    }
                    // Cancelled or completed — no action needed
                    _ => {}
                }
            });
        }

        Ok(())
    }

    /// Resume a session from a finished background task (#722): start a fresh
    /// turn with the completion injected. `context_text` is what the LLM sees;
    /// `display_text` persists to history. Uses the service-level callbacks so
    /// streaming / tools / the final response route to the TUI like any turn.
    pub(crate) async fn resume_background_turn(
        &mut self,
        session_id: Uuid,
        context_text: String,
        display_text: String,
    ) {
        let token = CancellationToken::new();
        self.processing_sessions.insert(session_id);
        if self.is_current_session(session_id) {
            self.is_processing = true;
            self.processing_started_at = Some(std::time::Instant::now());
            self.cancel_token = Some(token.clone());
            // Background-task turns carry their own system prompt and push no
            // user row, so the anchor has to be explicit (#2127).
            self.turn_start_len = self.messages.len();
        } else {
            self.session_cancel_tokens.insert(session_id, token.clone());
        }

        let agent_service = self.agent_service.clone();
        let event_sender = self.event_sender();
        tracing::info!(
            "[background_resume] starting resume turn for session {}",
            session_id
        );
        tokio::spawn(async move {
            let result = agent_service
                .send_message_with_tools_and_display(
                    session_id,
                    context_text,
                    Some(display_text),
                    None,
                    Some(token),
                    None,
                    None,
                    "tui",
                    None,
                    None,
                )
                .await;
            match result {
                Ok(response) => {
                    let _ = event_sender.send(TuiEvent::ResponseComplete {
                        session_id,
                        response,
                    });
                }
                Err(e) => {
                    let user_message = crate::brain::agent::format_user_error(&e);
                    let _ = event_sender.send(TuiEvent::Error {
                        session_id,
                        message: user_message,
                    });
                }
            }
        });
    }

    /// Append a streaming chunk
    pub(crate) fn append_streaming_chunk(&mut self, chunk: String) {
        if let Some(ref mut response) = self.streaming_response {
            response.push_str(&chunk);
        } else {
            self.streaming_response = Some(chunk);
            // Auto-scroll when response starts streaming (only if user hasn't scrolled up)
            if self.auto_scroll {
                self.scroll_offset = 0;
            }
        }
    }

    /// Complete the streaming response
    pub(crate) async fn complete_response(
        &mut self,
        response: crate::brain::agent::AgentResponse,
    ) -> Result<()> {
        if let Some(ref session) = self.current_session {
            self.processing_sessions.remove(&session.id);
            self.session_cancel_tokens.remove(&session.id);
        }
        self.is_processing = false;
        self.processing_started_at = None;
        // A tool-driven cd during the turn changes the agent's per-session wd
        // without touching the TUI-tracked path; resync here so the footer
        // shows where the agent actually is (#460). Read THIS session's handle
        // (#703), not the global, so a background session's cd never moves the
        // footer.
        let agent_wd = match self.current_session {
            Some(ref session) => self
                .agent_service
                .get_working_directory_for_session(session.id),
            None => self.agent_service.get_working_directory(),
        };
        if agent_wd != self.working_directory {
            tracing::info!(
                "TUI: working directory changed during turn: {} → {}",
                self.working_directory.display(),
                agent_wd.display()
            );
            self.working_directory = agent_wd;
        }
        tracing::debug!(
            "[TUI] complete_response: clearing streaming_response (was {} chars), intermediate_text_received={}",
            self.streaming_response
                .as_ref()
                .map(|s| s.len())
                .unwrap_or(0),
            self.intermediate_text_received
        );
        self.streaming_response = None;
        // Stash the just-finished turn's tok/s into `last_tps` BEFORE we
        // zero the counters, so the footer keeps showing the rate until
        // the next turn produces its first token. Pass the provider-
        // reported tok/s from AgentResponse so the displayed rate
        // matches the channel footer (and is correct for non-OpenAI
        // models where tiktoken over-counts the local estimate).
        self.finalize_tps(response.tokens_per_second);
        self.streaming_output_tokens = 0;
        let reasoning_details = self.streaming_reasoning.take();
        self.cancel_token = None;
        if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
            self.task_abort_handles.remove(&sid);
        }
        self.escape_pending_at = None; // Reset so abort hint doesn't leak to input clear

        // Clean up stale pending approvals — send deny so agent callbacks don't hang
        for msg in &mut self.messages {
            if let Some(ref mut approval) = msg.approval
                && approval.state == ApprovalState::Pending
            {
                tracing::warn!(
                    "Cleaning up stale pending approval for tool '{}'",
                    approval.tool_name
                );
                let _ = approval.response_tx.send(ToolApprovalResponse {
                    request_id: approval.request_id,
                    approved: false,
                    reason: Some("Agent completed without resolution".to_string()),
                });
                approval.state =
                    ApprovalState::Denied("Agent completed without resolution".to_string());
            }
        }

        // Finalize active tool group as a quick_jump message BEFORE the response.
        // Matches DB reload order from expand_message.
        if let Some(group) = self.active_tool_group.take() {
            let count = group.calls.len();
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "tool_group".to_string(),
                content: format!("{} tool call{}", count, if count == 1 { "" } else { "s" }),
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: Some(group),
                duration_secs: None,
            });
        }

        // Hand back any queued message the turn ended without consuming. The
        // tool loop drains the queue at a tool-call boundary; when the model
        // answers straight after a queue there is no boundary left, and
        // dropping the entry here lost the message outright: the user watched
        // it listed as queued, then it never sent and never came back (#2236).
        // Restore it to the input box the same way Arrow-Up dequeues, so
        // nothing is auto-sent and the duplicate-submit guard (#798) stays
        // untouched.
        let unconsumed = self.current_session.as_ref().map(|s| s.id).and_then(|sid| {
            self.queued_messages
                .lock()
                .ok()
                .and_then(|mut q| q.remove(&sid))
        });
        if let Some(restored) = unconsumed.as_deref().and_then(unconsumed_user_text) {
            if self.input_buffer.is_empty() {
                self.input_buffer = restored;
            } else {
                self.input_buffer = format!("{restored}\n{}", self.input_buffer);
            }
            self.cursor_position = self.input_buffer.len();
            tracing::info!("[TUI] Restored unconsumed queued message to the input buffer");
        }

        // Track context usage from latest response. Write to the
        // per-session cache first; only mirror into the focused-view
        // fields if THIS response belongs to the focused session,
        // otherwise a background turn's completion would overwrite the
        // pane the user is actually looking at.
        if let Some(ref session) = self.current_session {
            self.session_input_tokens
                .insert(session.id, response.context_tokens);
            if self.is_current_session(session.id) {
                self.last_input_tokens = Some(response.context_tokens);
            }
        } else {
            self.last_input_tokens = Some(response.context_tokens);
        }

        // Strip LLM artifacts (<!-- reasoning -->, </invoke>, XML tool blocks)
        // before displaying in TUI — same sanitization as Telegram/external channels.
        let mut response = response;
        response.content = crate::utils::sanitize::strip_llm_artifacts(&response.content);

        // Debug: log response content length
        tracing::debug!(
            "Response complete: content_len={}, output_tokens={}",
            response.content.len(),
            response.usage.output_tokens
        );

        // A final answer that follows tool groups must become its own row.
        // Overwriting an earlier text row would move it above those tools,
        // where the fold hides it (see `answer_placement`).
        let answer_follows_tools =
            !response.content.is_empty() && last_text_precedes_tool_group(&self.messages);
        // This turn's rows only, so a stamp can never land on an earlier turn.
        let turn_from = self.turn_start_len.min(self.messages.len());
        // The row an answer may be written into: the last row of this turn that
        // already carries text. Reasoning-only rows are skipped on purpose:
        // their content is empty by design, so they are never the answer's home,
        // and picking one left the answer sitting inside a thinking row.
        let text_row = self.messages[turn_from..]
            .iter()
            .rposition(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|i| i + turn_from);
        // ... and the write only happens when the response EXTENDS what that row
        // holds. Replacing a delivered report with a later, shorter closing line
        // is how a report the user had already read came back as vanished while
        // the database still held every character of it (#2165).
        let write_row = if self.intermediate_text_received && !answer_follows_tools {
            text_row.filter(|&i| answer_extends_row(&self.messages[i].content, &response.content))
        } else {
            None
        };
        if let Some(idx) = write_row {
            // IntermediateText only contained text BEFORE the last tool block.
            // The full response may have trailing text AFTER the last tool.
            // Update the intermediate assistant message with the full content
            // so trailing text is not lost, and add cost/token metadata.
            let last_assistant = &mut self.messages[idx];
            let old_len = last_assistant.content.len();
            last_assistant.content = response.content.clone();
            // Stamp the elapsed time before `processing_started_at` is
            // cleared below (#964). Without this the settled header has
            // nothing to show until the session is reloaded from the DB.
            stamp_settled_metadata(
                last_assistant,
                &response,
                &reasoning_details,
                self.processing_started_at
                    .map(|t| t.elapsed().as_secs() as i64),
            );
            tracing::debug!(
                "Updated intermediate assistant message: old_len={}, new_len={}",
                old_len,
                last_assistant.content.len()
            );
        } else if let Some(idx) = text_row.filter(|_| response.content.is_empty()) {
            // CLI providers (qwen-cli, opencode-cli) stream their full text
            // through IntermediateText progress events and return an empty
            // `response.content` at the end. Stamp the row that already holds
            // the reply instead of adding a blank one.
            stamp_settled_metadata(
                &mut self.messages[idx],
                &response,
                &reasoning_details,
                self.processing_started_at
                    .map(|t| t.elapsed().as_secs() as i64),
            );
        } else if answer_already_on_screen(&self.messages, &response.content) {
            // The answer text is already on screen as this turn's assistant row.
            // A turn that halts on an option-surface tool streams its answer and
            // then calls the tool, so the text row sits ABOVE the tool group and
            // the response the loop synthesizes on that halt carries the same
            // text again. Appending it would render the answer twice; the DB
            // keeps one row per turn, so a reload drops the second copy anyway.
            // Stamp the settled metadata onto the row that exists instead.
            tracing::info!(
                "[TUI] Duplicate answer row skipped: text already on screen (len={})",
                response.content.len()
            );
            if let Some(idx) =
                text_row.or_else(|| self.messages.iter().rposition(|m| m.role == "assistant"))
            {
                stamp_settled_metadata(
                    &mut self.messages[idx],
                    &response,
                    &reasoning_details,
                    self.processing_started_at
                        .map(|t| t.elapsed().as_secs() as i64),
                );
            }
        } else {
            // Add assistant message to UI
            let assistant_msg = DisplayMessage {
                id: response.message_id,
                role: "assistant".to_string(),
                content: response.content,
                timestamp: chrono::Utc::now(),
                token_count: Some(response.usage.output_tokens as i64),
                cost: Some(response.cost),
                approval: None,
                approve_menu: None,
                details: reasoning_details.clone(),
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: self
                    .processing_started_at
                    .map(|t| t.elapsed().as_secs() as i64),
            };
            self.messages.push(assistant_msg);
        }

        // Auto-scroll to bottom
        self.scroll_offset = 0;

        // Update pane message cache so inactive panes reflect latest content
        if self.pane_manager.is_split()
            && let Some(ref session) = self.current_session
        {
            self.pane_message_cache
                .insert(session.id, self.messages.clone());
        }

        // Keep the live footer aligned with the model the turn actually ran
        // on. When Anthropic ships a new Claude, the CLI resolves `opus` to
        // the new version and reports it back in `response.model`; refreshing
        // the in-memory override here makes the footer show it immediately
        // instead of waiting for a restart. Only touch it when it actually
        // changed so we don't churn the override on every message.
        // Only a turn that stayed on the session's provider may do this. The
        // persisted pair already obeys that rule (#705); this override did not,
        // so a turn that fell elsewhere reported that provider's model and
        // silently replaced the user's pick, leaving `/models` looking ignored.
        if let Some(ref session) = self.current_session
            && crate::brain::agent::service::should_refresh_session_model(
                response.started_on_session_provider,
                &self.agent_service.provider_model_for_session(session.id),
                &response.model,
            )
        {
            self.agent_service
                .set_session_model(session.id, response.model.clone());
        }

        // Spawn slow post-completion work in the background so the render loop
        // can draw the next frame immediately (is_processing is already false).
        // DB writes and file reads here were causing 5+ second spinner delays.
        let session_service = self.session_service.clone();
        let session_for_bg = self.current_session.clone();
        let response_model = response.model.clone();
        let response_provider = response.provider_name.clone();
        // Only a turn that STARTED on the session's own provider may rewrite the
        // saved pair (#705). A turn that started on the wrong provider (a #704
        // restore gap) carries an involuntarily-remapped pair that must never
        // overwrite the session's saved choice — that was the silent-switch bug.
        let response_started_on_session_provider = response.started_on_session_provider;
        let plan_path = self.plan_file_path.clone();

        tokio::spawn(async move {
            // Persist the {provider, model} pair from the response onto the
            // session row whenever it differs from what's stored. The two
            // fields are a LOCKED unit — writing only one (the prior code
            // wrote `model` alone behind an `is_none()` gate) lets a sticky
            // fallback's model land on a row whose provider_name still
            // points at the original. The next turn then ships e.g.
            // dialagram/glm-5.1 — a pair that exists in no provider's
            // catalogue. ProviderSwitched event handler also keeps the row
            // in sync; this is the second writer that closes the same gap
            // for paths that produce a response without firing
            // ProviderSwitched (e.g. resume after restart, channel-driven
            // turns whose progress events miss the TUI loop).
            if let Some(mut session) = session_for_bg {
                let pair_changed = session.model.as_deref() != Some(response_model.as_str())
                    || session.provider_name.as_deref() != Some(response_provider.as_str());
                // A changed pair from a turn that started on the WRONG provider
                // is an involuntary remap (#705) — skip it so the session's
                // saved choice survives. Genuine fallbacks start matched and are
                // persisted here (and by the ProviderSwitched handler).
                if pair_changed && !response_started_on_session_provider {
                    tracing::warn!(
                        "Skipping provider/model persist for session {}: turn ran on '{}' \
                         which is not the session's saved provider (involuntary remap, #705)",
                        session.id,
                        response_provider
                    );
                } else if pair_changed {
                    session.model = Some(response_model);
                    session.provider_name = Some(response_provider);
                    if let Err(e) = session_service.update_session(&session).await {
                        tracing::warn!("Failed to update session provider/model pair: {}", e);
                    }
                }
            }

            // Thinking persistence is handled by tool_loop's per-iteration
            // append_content with `<!-- reasoning -->` markers in `content`.
            // No TUI-side write here — the prior fake-UUID `set_thinking` call
            // silently no-op'd (UPDATE affected 0 rows) and corrupted the
            // post-restart layout by losing the per-iteration chronological
            // position. Reload now reconstructs the exact streamed view via
            // `expand_message` + `extract_reasoning`.

            // Reload user commands (agent may have written new ones to commands.json)
            // This is synchronous but fast — just a file read + parse.
            // We can't call self.reload_user_commands() from a spawned task,
            // so we skip it here. The next send_message will pick up new commands.

            // Resolve any legacy terminal plan statuses on disk (Completed
            // archives, Cancelled deletes) via the shared loader. Live plans
            // are never deleted here: an idle Active plan (including a
            // seed-failed empty-task one) stays intact for retry, and
            // Editing survives across turns by design.
            if let Some(ref path) = plan_path
                && path.exists()
                && crate::utils::plan_files::load_plan_from_path(path).is_none()
            {
                tracing::debug!("Plan at {} resolved to NoPlan on reload", path.display());
            }
        });

        // Re-read the plan file so the widget reflects the post-turn state.
        // When the plan completed and was archived (JSON renamed to archive/),
        // the file no longer exists and reload_plan() clears plan_document,
        // making the widget vanish. Without this, the TUI kept rendering a
        // stale in-memory copy of the plan forever after completion.
        self.reload_plan();

        Ok(())
    }

    /// Persist current in-memory streaming state to DB so cancel never loses visible content.
    ///
    /// Finds the last assistant message (created by tool_loop at start) and appends
    /// any streaming text + tool call markers that are currently displayed on screen.
    pub(crate) async fn persist_streaming_state(&self, session_id: Uuid) {
        // Reconstruct the current turn's assistant text that lives only in
        // `self.messages`. For HTTP/streaming providers the real-time live
        // persister (#269) is CLI-only, so text promoted out of
        // `streaming_response` into the message list is never written to the
        // DB until turn-end; a cancel would lose it entirely (#1468).
        let memory_turn_text = collect_turn_assistant_text(&self.messages, self.turn_start_len);

        // Build the live tail: in-flight tool group + not-yet-promoted text.
        let mut content = String::new();

        // Active tool group (tool calls shown on screen but not yet flushed to a message)
        if let Some(ref group) = self.active_tool_group {
            let entries: Vec<serde_json::Value> = group
                .calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "d": call.description,
                        "s": call.success,
                        "i": call.tool_input,
                    })
                })
                .collect();
            if !entries.is_empty() {
                content.push_str(&format!(
                    "\n<!-- tools-v2: {} -->\n",
                    serde_json::to_string(&entries).unwrap_or_default()
                ));
            }
        }

        // Streaming response text (currently being typed out, not yet committed)
        if let Some(ref text) = self.streaming_response
            && !text.trim().is_empty()
        {
            content.push_str(text);
            content.push_str("\n\n");
        }

        // Nothing visible at all: no in-memory turn text and no live tail.
        if memory_turn_text.trim().is_empty() && content.trim().is_empty() {
            return;
        }

        // Find the last assistant message in this session and persist what's missing.
        match self.message_service.get_last_message(session_id).await {
            Ok(Some(msg)) if msg.role == "assistant" => {
                // If the DB row is still empty (the non-CLI placeholder the live
                // persister never filled), write the FULL visible turn so the
                // completion survives the cancel. If the row already has content
                // (CLI live-persister wrote it as it streamed), append only the
                // live tail, exactly as before, to avoid duplicating what is
                // already durable.
                let db_empty = msg.content.trim().is_empty();
                let to_write = if db_empty && !memory_turn_text.trim().is_empty() {
                    let mut full = memory_turn_text.clone();
                    if !content.trim().is_empty() {
                        full.push_str("\n\n");
                        full.push_str(&content);
                    }
                    full
                } else {
                    content.clone()
                };
                if to_write.trim().is_empty() {
                    return;
                }
                if let Err(e) = self.message_service.append_content(msg.id, &to_write).await {
                    tracing::error!("Failed to persist streaming state on cancel: {}", e);
                } else {
                    tracing::info!(
                        "Persisted {} chars of streaming state to DB on cancel (db_empty={})",
                        to_write.len(),
                        db_empty
                    );
                }
            }
            Ok(_) => {
                // Last message isn't assistant — create one to hold the visible turn.
                let mut full = memory_turn_text.clone();
                if !content.trim().is_empty() {
                    if !full.is_empty() {
                        full.push_str("\n\n");
                    }
                    full.push_str(&content);
                }
                if full.trim().is_empty() {
                    return;
                }
                match self
                    .message_service
                    .create_message(session_id, "assistant".to_string(), full.clone())
                    .await
                {
                    Ok(_) => {
                        tracing::info!(
                            "Created new assistant message with {} chars of streaming state on cancel",
                            full.len()
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to create assistant message for streaming state: {}",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to query last message on cancel: {}", e);
            }
        }
    }
}

/// New start index for the in-flight turn after `prepended` rows are inserted
/// at the front of the message list and `removed_front` rows (a history marker)
/// are dropped from that front. Pure so the shift arithmetic is testable: the
/// earlier version measured the prepended count after `append` had already
/// drained the source, which reported the whole history and clamped the anchor
/// past the end, so a cancel after PageUp paging persisted nothing (#2127).
pub(crate) fn shift_turn_start(
    turn_start: usize,
    prepended: usize,
    removed_front: usize,
    total: usize,
) -> usize {
    turn_start
        .saturating_add(prepended)
        .saturating_sub(removed_front)
        .min(total)
}

/// Reconstruct the assistant text of the in-flight turn from the in-memory
/// message list: every `assistant` DisplayMessage after the last `user`
/// message, joined in order. For HTTP/streaming providers the real-time live
/// persister (#269) is CLI-only, so completion text promoted out of
/// `streaming_response` into `self.messages` is never written to the DB until
/// turn-end. On cancel that text would be lost entirely (the placeholder row
/// stays empty, then `load_session` rebuilds the view from it and wipes the
/// in-memory copy), so `persist_streaming_state` reconstructs it from here and
/// writes it before the task is killed (#1468). Pure over the slice so it is
/// unit-testable without a live terminal.
pub(crate) fn collect_turn_assistant_text(
    messages: &[DisplayMessage],
    turn_start: usize,
) -> String {
    // Never read above the in-flight turn's start. Rows before it are already
    // durable: loaded from the DB by `load_session`, or written by an earlier
    // turn in this process. Re-collecting them wrote the whole previous turn
    // into a fresh placeholder row, which then rendered as a second,
    // marker-less copy of history that no fold could hide (#2127).
    //
    // The last user message stays a second floor so a caller that could not
    // set an anchor still cannot reach into a settled turn.
    let after_user = messages
        .iter()
        .rposition(|m| m.role == "user")
        .map(|i| i + 1)
        .unwrap_or(0);
    let start = turn_start.max(after_user).min(messages.len());
    let mut out = String::new();
    for m in &messages[start..] {
        if m.role == "assistant" && !m.content.trim().is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&m.content);
        }
    }
    out
}

/// Run the evolve tool directly (no LLM involvement) and pipe progress
/// events into TUI system messages. Used by `/evolve`, the UpdatePrompt
/// dialog, and the auto-update path on startup.
pub(crate) async fn run_evolve_directly(
    session_id: Uuid,
    sender: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
) {
    use crate::brain::agent::ProgressEvent;
    use crate::brain::tools::evolve::EvolveTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use std::sync::Arc;

    // Translate ProgressEvent into TUI events so the user sees live status.
    // All evolve messages are tied to the session that triggered /evolve so
    // they don't leak into a parallel pane that happens to be focused when
    // a build line streams in.
    let tx = sender.clone();
    let origin = session_id;
    let progress: crate::brain::agent::ProgressCallback =
        Arc::new(move |_sid, event| match event {
            ProgressEvent::IntermediateText { text, .. } => {
                let _ = tx.send(TuiEvent::SystemMessage {
                    session_id: origin,
                    text,
                });
            }
            ProgressEvent::RestartReady {
                status,
                binary_path,
            } => {
                let _ = tx.send(TuiEvent::RestartReady {
                    status,
                    binary_path,
                });
            }
            _ => {}
        });

    let tool = EvolveTool::new(Some(progress));
    let ctx = ToolExecutionContext::new(session_id).with_session_notify_enabled(
        crate::config::Config::current()
            .agent
            .session_notify_enabled,
    );
    match tool.execute(serde_json::json!({}), &ctx).await {
        Ok(result) => {
            if !result.success {
                if let Some(err) = result.error {
                    let _ = sender.send(TuiEvent::SystemMessage {
                        session_id,
                        text: format!("Evolve failed: {}", err),
                    });
                } else {
                    let _ = sender.send(TuiEvent::SystemMessage {
                        session_id,
                        text: "Evolve failed (unknown error)".to_string(),
                    });
                }
            } else if !result.output.is_empty() {
                // Already on latest, or non-restart success path — surface message.
                let _ = sender.send(TuiEvent::SystemMessage {
                    session_id,
                    text: result.output,
                });
            }
        }
        Err(e) => {
            let _ = sender.send(TuiEvent::SystemMessage {
                session_id,
                text: format!("Evolve error: {}", e),
            });
        }
    }
}
