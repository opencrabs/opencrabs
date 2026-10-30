//! resume_agent tool — resumes a completed/failed child agent with new input.

use super::manager::{SubAgentManager, SubAgentState};
use crate::brain::tools::error::{Result, ToolError};
use crate::brain::tools::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Tool that resumes a previously completed or failed sub-agent.
pub struct ResumeAgentTool {
    manager: Arc<SubAgentManager>,
    parent_registry: Arc<crate::brain::tools::ToolRegistry>,
}

impl ResumeAgentTool {
    pub fn new(
        manager: Arc<SubAgentManager>,
        parent_registry: Arc<crate::brain::tools::ToolRegistry>,
    ) -> Self {
        Self {
            manager,
            parent_registry,
        }
    }
}

#[async_trait]
impl Tool for ResumeAgentTool {
    fn name(&self) -> &str {
        "resume_agent"
    }

    fn description(&self) -> &str {
        "Resume a completed or failed sub-agent with a new prompt. \
         The agent continues in the same session, preserving its prior context. \
         \n\nProvider and model resolution follows the same precedence as spawn_agent: \
         (1) the optional `provider` / `model` parameters on THIS call, \
         (2) the user's config.toml `[agent]` keys `subagent_provider` / `subagent_model`, \
         (3) the parent session's provider. Resuming with a different model is useful when \
         the original spawn used a cheap/fast model for a draft and the resume should \
         escalate to a stronger model for a fix-up pass."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The ID of the sub-agent to resume"
                },
                "prompt": {
                    "type": "string",
                    "description": "New instruction/prompt for the resumed agent"
                },
                "provider": {
                    "type": "string",
                    "description": "Optional provider override for THIS resume (e.g., 'zhipu', 'openrouter', 'custom:my-provider'). Highest precedence — overrides config.agent.subagent_provider."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for THIS resume (model id as the chosen provider accepts it). Highest precedence — overrides config.agent.subagent_model."
                }
            },
            "required": ["agent_id", "prompt"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::SystemModification]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let agent_id = input
            .get("agent_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'agent_id' is required".into()))?;

        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'prompt' is required".into()))?
            .to_string();
        // #129: a resumed child is still a headless session — re-assert the
        // final-message law on every resume (the original spawn's preamble
        // was consumed by the first run's context).
        let prompt = format!(
            "{}\n\n{prompt}",
            crate::cli::tool_setup::HEADLESS_PREAMBLE.trim_start_matches('\n')
        );

        // Check agent exists and is in a resumable state
        match self.manager.get_state(agent_id) {
            None => {
                return Ok(ToolResult::error(format!(
                    "No sub-agent found with id: {}",
                    agent_id
                )));
            }
            Some(SubAgentState::Running) | Some(SubAgentState::AwaitingInput) => {
                return Ok(ToolResult::error(format!(
                    "Sub-agent {} is still running. Use wait_agent first or close_agent to cancel.",
                    agent_id
                )));
            }
            Some(SubAgentState::Completed) | Some(SubAgentState::Failed(_)) => {}
            Some(SubAgentState::Cancelled) => {
                return Ok(ToolResult::error(format!(
                    "Sub-agent {} was cancelled and cannot be resumed.",
                    agent_id
                )));
            }
        }

        let session_id = self.manager.get_session_id(agent_id).ok_or_else(|| {
            ToolError::Execution(format!("No session found for sub-agent {}", agent_id))
        })?;

        let service_context = context
            .service_context
            .as_ref()
            .ok_or_else(|| ToolError::Execution("No service context available".into()))?
            .clone();

        // Create new cancel token and input channel
        let cancel_token = CancellationToken::new();
        let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();

        // Prepare the agent for resumption
        let agent_id_str = agent_id.to_string();
        if !self
            .manager
            .prepare_resume(&agent_id_str, cancel_token.clone(), input_tx)
        {
            return Ok(ToolResult::error(format!(
                "Failed to prepare sub-agent {} for resumption",
                agent_id
            )));
        }

        // Per-call provider / model overrides (issue #152). Same
        // precedence as spawn_agent: per-call > config > parent.
        let call_provider = input
            .get("provider")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let call_model = input
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Build a new AgentService for the resumed run
        let config = crate::config::Config::load()
            .map_err(|e| ToolError::Execution(format!("Config load failed: {}", e)))?;
        // Per-call > config, normalised the same way spawn resolves it (#1316).
        let child = super::provider_pair::child_pair(
            &config,
            call_provider.as_deref(),
            call_model.as_deref(),
        );
        let subagent_model = child.model.clone();
        let effective_provider_name = child.provider.clone();

        let child_service = {
            let provider = if let Some(ref provider_name) = effective_provider_name {
                match crate::brain::provider::create_provider_by_name(&config, provider_name).await
                {
                    Ok(p) => {
                        let source = match child.source {
                            super::provider_pair::ProviderSource::PerCall => "per-call",
                            super::provider_pair::ProviderSource::Config => "config",
                        };
                        tracing::info!(
                            "Resumed sub-agent using {source} provider '{provider_name}'"
                        );
                        p
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Sub-agent provider '{}' failed: {e}, falling back to parent",
                            provider_name
                        );
                        crate::brain::provider::create_provider(&config)
                            .await
                            .map_err(|e| {
                                ToolError::Execution(format!("Failed to create provider: {}", e))
                            })?
                    }
                }
            } else {
                crate::brain::provider::create_provider(&config)
                    .await
                    .map_err(|e| {
                        ToolError::Execution(format!("Failed to create provider: {}", e))
                    })?
            };

            // Rebuild the child's registry from its FROZEN grant (#1173,
            // fixes F2): resume used to hand back a full General registry
            // regardless of how the child was originally spawned, silently
            // widening an explore-style child into full write access. The
            // stored read_only flag is the single source of truth for the
            // agent's whole life.
            let child_registry = super::build_child_registry(&self.parent_registry);
            if self.manager.get_read_only(agent_id).unwrap_or(false) {
                crate::brain::tools::plan_gate::restrict_registry_to_read_only(&child_registry);
                tracing::info!(
                    "Resumed sub-agent {agent_id} holds a read-only grant: \
                     registry rebuilt restricted (#1173)"
                );
            }

            let child_dir = context.working_dir();
            let include_brain = self.manager.get_include_brain(agent_id).unwrap_or(false);
            let system_brain = super::brain::child_system_brain(
                include_brain,
                &child_dir,
                subagent_model.as_deref(),
                effective_provider_name.as_deref(),
            );

            let mut builder =
                crate::brain::agent::AgentService::new(provider, service_context, &config)
                    .await
                    .with_tool_registry(child_registry)
                    .with_auto_approve_tools(true)
                    .with_working_directory(context.working_dir())
                    // #129: a resumed child is still headless (owner ruling C).
                    .with_headless(true)
                    // The parent already planned this work (see spawn.rs).
                    .with_plan_gate_exempt(true);

            if let Some(brain) = system_brain {
                builder = builder.with_system_brain(brain);
            }

            Arc::new(builder)
        };

        // Spawn resumed task with input loop
        let cancel_clone = cancel_token.clone();
        let manager = self.manager.clone();
        let agent_id_clone = agent_id_str.clone();
        let prompt_clone = prompt.clone();
        // Delivery identity (#1197): on natural completion the parent must
        // be woken even though nobody registered a waiter for this resume.
        let parent_of_child = self.manager.get_parent_session_id(agent_id);
        let child_label = self.manager.get_label(agent_id);
        let model_override = subagent_model;
        let mut input_rx = input_rx;

        let handle = tokio::spawn(async move {
            tracing::info!("Sub-agent {} resuming: {}", agent_id_clone, prompt_clone);

            let mut current_prompt = prompt_clone;

            // Run prompt → wait for input → run again loop
            let final_output = loop {
                let result = child_service
                    .send_message_with_tools_and_mode(
                        session_id,
                        current_prompt,
                        model_override.clone(),
                        Some(cancel_clone.clone()),
                    )
                    .await;

                match result {
                    Ok(response) => {
                        manager.update_output(&agent_id_clone, response.content.clone());
                        // Natural completion (#1184), same rule as spawn.rs:
                        // only a genuinely gated round keeps waiting; a
                        // finished answer completes the resumed run instead of
                        // re-parking it forever.
                        if response.stop_reason
                            != Some(crate::brain::provider::types::StopReason::ToolUse)
                        {
                            tracing::info!(
                                "Sub-agent {} resumed round complete naturally, delivering result",
                                agent_id_clone
                            );
                            break response.content;
                        }
                        // Genuinely gated: park for the next input round.
                        manager.mark_awaiting_input(&agent_id_clone);
                        tracing::info!(
                            "Sub-agent {} round complete, waiting for input",
                            agent_id_clone
                        );

                        let next = tokio::select! {
                            msg = input_rx.recv() => msg,
                            _ = cancel_clone.cancelled() => {
                                tracing::info!(
                                    "Sub-agent {} cancelled while waiting for input",
                                    agent_id_clone
                                );
                                None
                            }
                        };

                        match next {
                            Some(text) => {
                                // Flip back to Running so the in-memory state
                                // matches the round now in flight — same as
                                // spawn.rs / team-create.rs (#1183).
                                manager.mark_running_again(&agent_id_clone);
                                tracing::info!(
                                    "Sub-agent {} received follow-up input",
                                    agent_id_clone
                                );
                                current_prompt = text;
                            }
                            None => break response.content,
                        }
                    }
                    Err(e) => {
                        tracing::error!("Sub-agent {} resumed and failed: {}", agent_id_clone, e);
                        manager.mark_failed(&agent_id_clone, e.to_string());
                        return;
                    }
                }
            };

            match (parent_of_child, child_label) {
                (Some(parent), Some(label)) => {
                    manager.complete_and_deliver(&agent_id_clone, final_output, parent, &label);
                }
                _ => {
                    // Legacy entry with no recorded parent: mark completed,
                    // nowhere to deliver.
                    manager.mark_completed(&agent_id_clone, final_output);
                }
            }
        });

        self.manager.set_join_handle(&agent_id_str, handle);

        Ok(ToolResult::success(format!(
            "Resumed sub-agent {} with new prompt:\n{}",
            agent_id, prompt
        )))
    }
}
