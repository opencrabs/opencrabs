use super::builder::AgentService;
use super::types::*;
use crate::brain::agent::error::{AgentError, Result};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl AgentService {
    /// Send a message and get a response
    ///
    /// This will:
    /// 1. Load conversation context from the database
    /// 2. Add the new user message
    /// 3. Send to the LLM provider
    /// 4. Save the response to the database
    /// 5. Update token usage
    pub async fn send_message(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
    ) -> Result<AgentResponse> {
        self.send_message_with_display(session_id, user_message, None, model)
            .await
    }

    /// Like [`send_message`](Self::send_message) but history persists
    /// `display_text` (when set) instead of the full `user_message`, which
    /// only feeds the LLM context for this turn. Use for synthetic prompts
    /// (reaction guidance, queued mid-turn feedback) so scaffolding never
    /// shows in the TUI or re-enters future context.
    #[tracing::instrument(skip_all, fields(session_id = %session_id, channel = "direct"))]
    pub async fn send_message_with_display(
        &self,
        session_id: Uuid,
        user_message: String,
        display_text: Option<String>,
        model: Option<String>,
    ) -> Result<AgentResponse> {
        // Wall clock for the turn, stamped onto the assistant row below (#964).
        let turn_started_at = std::time::Instant::now();
        // Prepare message context (common setup logic)
        let (_model_name, request, message_service, session_service) = self
            .prepare_message_context_with_display(session_id, user_message, display_text, model)
            .await?;

        // Send to provider — use session's provider so a concurrent
        // foreground swap on another pane can't hijack this turn.
        let provider = self.provider_for_session(session_id);
        let response = provider
            .complete(request)
            .await
            .map_err(AgentError::Provider)?;

        // Extract text from response
        let assistant_text = Self::extract_text_from_response(&response);

        // Save assistant response to database
        let assistant_db_msg = message_service
            .create_message(session_id, "assistant".to_string(), assistant_text.clone())
            .await
            .map_err(AgentError::db)?;

        // Calculate total tokens and cost for this message
        let billable_input = response.usage.input_tokens
            + response.usage.cache_creation_tokens
            + response.usage.cache_read_tokens;
        let total_tokens = billable_input + response.usage.output_tokens;
        let cost = self
            .provider_for_session(session_id)
            .calculate_cost_with_cache(
                &response.model,
                response.usage.input_tokens,
                response.usage.output_tokens,
                response.usage.cache_creation_tokens,
                response.usage.cache_read_tokens,
            );
        // Single-call path: where the provider reported dollars for this
        // call, that number IS the bill — the table is a guess (#1707).
        let cost = response.usage.cost_usd.unwrap_or(cost);

        // Update message with usage info, stashing the server-reported
        // prompt token count so session reload reads it directly.
        message_service
            .update_message_usage(
                assistant_db_msg.id,
                crate::services::message::MessageUsage {
                    token_count: total_tokens as i64,
                    cost,
                    input_tokens: Some(billable_input as i64),
                    cache_creation_tokens: Some(response.usage.cache_creation_tokens as i64),
                    cache_read_tokens: Some(response.usage.cache_read_tokens as i64),
                    duration_secs: Some(turn_started_at.elapsed().as_secs() as i64),
                },
            )
            .await
            .map_err(AgentError::db)?;

        // Update session token usage with the pair that served it (#807). The
        // cache-read count rides along for the By Model cache share (#2027).
        session_service
            .update_session_usage(
                session_id,
                total_tokens as i64,
                cost,
                &self.provider_name_for_session(session_id),
                &self.provider_model_for_session(session_id),
                if response.usage.cache_read_unreported {
                    None
                } else {
                    Some(response.usage.cache_read_tokens as i64)
                },
                Some(billable_input as i64),
            )
            .await
            .map_err(AgentError::db)?;

        Ok(AgentResponse {
            message_id: assistant_db_msg.id,
            content: assistant_text,
            stop_reason: response.stop_reason,
            context_tokens: response.usage.input_tokens,
            usage: response.usage,
            tokens_per_second: None,
            cost,
            model: response.model,
            provider_name: self.provider_name_for_session(session_id),
            // This simple send path is not the restart-remap vector (#705); it
            // resolves the session provider directly, so it always counts as
            // started-on-session-provider.
            started_on_session_provider: true,
        })
    }

    /// Send a message and get a streaming response
    ///
    /// Returns a stream of response chunks that can be consumed incrementally.
    #[tracing::instrument(skip_all, fields(session_id = %session_id, channel = "streaming"))]
    pub async fn send_message_streaming(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
    ) -> Result<AgentStreamResponse> {
        // Prepare message context (common setup logic)
        let (model_name, request, _message_service, _session_service) = self
            .prepare_message_context(session_id, user_message, model)
            .await?;

        // Add streaming flag to request
        let request = request.with_streaming();

        // Get streaming response from provider (session-scoped so a
        // concurrent /models pick on a different pane can't hijack).
        let provider = self.provider_for_session(session_id);
        let stream = provider
            .stream(request)
            .await
            .map_err(AgentError::Provider)?;
        // Per-call provenance (#969): which chain entry served this call.
        tracing::info!(
            "Streaming call served: session={} {} model='{}'",
            session_id,
            provider.provenance_label(),
            // The model that RAN, not the one asked for (#1254).
            provider.served_model(&model_name),
        );

        Ok(AgentStreamResponse {
            session_id,
            message_id: Uuid::new_v4(),
            stream,
            model: model_name,
        })
    }

    /// Send a message with automatic tool execution (TUI channel).
    pub async fn send_message_with_tools(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
    ) -> Result<AgentResponse> {
        self.send_message_with_tools_and_mode(session_id, user_message, model, None)
            .await
    }

    /// Shim: send with tools + optional cancellation token (TUI channel).
    /// Delegates to `run_tool_loop` with service-level callbacks.
    pub async fn send_message_with_tools_and_mode(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<AgentResponse> {
        self.run_tool_loop(
            session_id,
            user_message,
            None,
            model,
            cancel_token,
            None,
            None,
            "tui",
            None,
            None,
            Some(PendingOrigin::User),
        )
        .await
    }

    /// Send a message with per-call callback overrides and channel routing.
    ///
    /// `override_*_callback` parameters take precedence over the service-level
    /// callbacks (used by channels). Pass `None` to fall back to the
    /// service-level callback. `override_question_callback` is the
    /// per-call surface for interactive prompts — channels
    /// with native button surfaces construct one per message; non-
    /// interactive callers (CLI, RSI, A2A) pass None.
    ///
    /// `channel` and `channel_chat_id` identify the originating channel for
    /// crash recovery routing.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_message_with_tools_and_callback(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        override_approval_callback: Option<ApprovalCallback>,
        override_progress_callback: Option<ProgressCallback>,
        channel: &str,
        channel_chat_id: Option<&str>,
        channel_thread_id: Option<&str>,
    ) -> Result<AgentResponse> {
        self.run_tool_loop(
            session_id,
            user_message,
            None,
            model,
            cancel_token,
            override_approval_callback,
            override_progress_callback,
            channel,
            channel_chat_id,
            channel_thread_id,
            Some(PendingOrigin::User),
        )
        .await
    }

    /// Resume an interrupted turn WITHOUT re-tracking it as a pending request.
    ///
    /// A resume is a one-shot best-effort recovery. If the resume is itself
    /// interrupted (cancelled by a new message, process killed on another
    /// restart, or a crash), it must NOT leave a pending row behind — otherwise
    /// the same already-done session resumes on every subsequent startup and
    /// rows pile up (#729). Only genuine user-initiated turns are recoverable.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_interrupted_turn(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        override_approval_callback: Option<ApprovalCallback>,
        override_progress_callback: Option<ProgressCallback>,
        channel: &str,
        channel_chat_id: Option<&str>,
    ) -> Result<AgentResponse> {
        self.run_tool_loop(
            session_id,
            user_message,
            None,
            model,
            cancel_token,
            override_approval_callback,
            override_progress_callback,
            channel,
            channel_chat_id,
            None,
            None,
        )
        .await
    }

    /// Send a push-initiated turn (session_notify / background-task
    /// completion), tracked for restart recovery with origin `system` (#12).
    ///
    /// Unlike [`Self::resume_interrupted_turn`] this INSERTS a pending row:
    /// a push turn killed mid-tool must be visible to boot recovery, which
    /// re-delivers the original push text instead of replaying the LLM turn
    /// (re-running the interrupted tool call could double-execute side
    /// effects such as installs or binary swaps). The delete-at-exit
    /// invariant is unchanged, and a re-delivery at boot rides
    /// [`Self::resume_interrupted_turn`], so the #729 no-perpetual-rows
    /// guarantee still holds.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_push_turn(
        &self,
        session_id: Uuid,
        user_message: String,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        override_approval_callback: Option<ApprovalCallback>,
        override_progress_callback: Option<ProgressCallback>,
        channel: &str,
        channel_chat_id: Option<&str>,
        channel_thread_id: Option<&str>,
    ) -> Result<AgentResponse> {
        self.run_tool_loop(
            session_id,
            user_message,
            None,
            model,
            cancel_token,
            override_approval_callback,
            override_progress_callback,
            channel,
            channel_chat_id,
            channel_thread_id,
            Some(PendingOrigin::System),
        )
        .await
    }

    /// Send a message and provide a separate human-readable `display_text`
    /// for DB persistence and TUI/session display. The full `user_message`
    /// (typically the channel-wrapped agent input with sender metadata,
    /// reply context, group history, channel hints) still goes to the LLM
    /// context so the agent retains all the information it needs, but the
    /// chat history shown in the TUI mirrors what the user actually typed
    /// in the channel.
    ///
    /// `override_question_callback` is the per-call surface for the
    /// interactive-prompt variant — same semantics as the callback-only
    /// shim above.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_message_with_tools_and_display(
        &self,
        session_id: Uuid,
        user_message: String,
        display_text: Option<String>,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        override_approval_callback: Option<ApprovalCallback>,
        override_progress_callback: Option<ProgressCallback>,
        channel: &str,
        channel_chat_id: Option<&str>,
        channel_thread_id: Option<&str>,
    ) -> Result<AgentResponse> {
        self.run_tool_loop(
            session_id,
            user_message,
            display_text,
            model,
            cancel_token,
            override_approval_callback,
            override_progress_callback,
            channel,
            channel_chat_id,
            channel_thread_id,
            Some(PendingOrigin::User),
        )
        .await
    }
}
