//! spawn_agent tool — creates a child agent with forked context.
//!
//! Sub-agent progress is streamed to `~/.opencrabs/tmp/detached/<agent_id>.json`
//! so the main orchestrator can track status without session_search.

use super::manager::{SubAgent, SubAgentManager};
use crate::brain::agent::service::work_status::{WorkState, WorkStatus};
use crate::brain::tools::error::{Result, ToolError};
use crate::brain::tools::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How much of a sub-agent's output the pushed message carries. Enough to act
/// on, short of pasting a long transcript into the parent's context.
const PUSHED_OUTPUT_LIMIT: usize = 4000;

/// Build the message a finished sub-agent injects into the session that
/// spawned it. Pure, so the framing is testable without spawning anything.
///
/// `outcome` is the output on success or the error on failure.
pub(crate) fn completion_message(
    label: &str,
    agent_id: &str,
    outcome: std::result::Result<&str, &str>,
) -> crate::brain::agent::QueuedUserMessage {
    let (context_text, display_text) = match outcome {
        Ok(output) => {
            let full_report_hint = if output.chars().count() > PUSHED_OUTPUT_LIMIT {
                let path = crate::brain::agent::service::work_status::status_path(agent_id);
                format!(
                    "Preview truncated - the FULL untruncated report is persisted in {} \
                     (field output_full).\n",
                    path.display()
                )
            } else {
                String::new()
            };
            (
                format!(
                    "[System: the sub-agent you spawned has finished.\n\
                     Agent: {label} (id {agent_id})\n\
                     Status: completed\n\
                     Output:\n{}\n\n\
                     {full_report_hint}\
                     Report the result to the user and continue anything that was waiting on it. \
                     Do not re-spawn the agent — this IS its result.]",
                    truncate_output(output)
                ),
                format!("🤖 sub-agent finished: {label}"),
            )
        }
        Err(error) => (
            format!(
                "[System: the sub-agent you spawned has failed.\n\
                 Agent: {label} (id {agent_id})\n\
                 Status: failed\n\
                 Error: {error}\n\n\
                 Report the failure to the user and decide what to do about it. Do not assume the \
                 work was completed.]"
            ),
            format!("🤖 sub-agent failed: {label}"),
        ),
    };
    crate::brain::agent::QueuedUserMessage {
        context_text,
        display_text,
        origin: crate::brain::agent::PushOrigin::SubAgent,
        bg_meta: None,
    }
}

/// Keep the tail of a long output: the conclusion matters more than the
/// opening, same as the detached-command completion path.
fn truncate_output(output: &str) -> String {
    if output.chars().count() <= PUSHED_OUTPUT_LIMIT {
        return output.to_string();
    }
    let skip = output.chars().count() - PUSHED_OUTPUT_LIMIT;
    let tail: String = output.chars().skip(skip).collect();
    format!("…(truncated)\n{tail}")
}

/// Deliver a finished sub-agent's outcome to the session that spawned it.
pub(crate) fn push_result(
    parent_session_id: uuid::Uuid,
    label: &str,
    agent_id: &str,
    outcome: std::result::Result<&str, &str>,
) {
    use crate::brain::agent::service::session_routes::{Delivery, deliver_to_session};

    let msg = completion_message(label, agent_id, outcome);
    // interrupt=true (fork #13): a sub-agent completion is the parent's own
    // awaited work — it must reach the parent even mid-turn, exactly as
    // before the gate existed.
    match deliver_to_session(parent_session_id, msg, true) {
        Delivery::Delivered => {
            tracing::info!(
                "Sub-agent {agent_id} reported its result to session {parent_session_id}"
            );
        }
        Delivery::Parked => {
            // Not lost: it leaves when the owning channel claims the session.
            tracing::info!(
                "Sub-agent {agent_id}'s result is parked for session {parent_session_id} until \
                 its channel claims it"
            );
        }
        Delivery::NoRoute => {
            // The parent is waiting on this either way, so say so rather than
            // returning quietly.
            tracing::warn!(
                "Sub-agent {agent_id}'s result had nowhere to go for session \
                 {parent_session_id}; the parent will not hear about it"
            );
        }
        Delivery::RefusedInFlight { redirected_to } => {
            // Unreachable by construction: interrupt=true is passed above and
            // the fork #13 gate refuses only when interrupt is unset. Arm kept
            // explicit so a future call-site change cannot drop the outcome
            // silently (port seam: upstream's match has no catch-all).
            tracing::warn!(
                "Sub-agent {agent_id}'s result was refused by the interrupt gate \
                 for session {parent_session_id} (redirected to {redirected_to:?}); the \
                 parent will not hear about it"
            );
        }
        Delivery::Redirected { to } => {
            // The parent was replaced on its channel while the sub-agent ran
            // (fork #17), so the result was REDIRECTED to the session that
            // owns the channel now (fork #19) — delivered, not dropped.
            tracing::info!(
                "Sub-agent {agent_id}'s result was redirected from session \
                 {parent_session_id} to session {to}, which now owns its channel"
            );
        }
    }
}

/// Tool that spawns a child agent to handle a sub-task.
pub struct SpawnAgentTool {
    manager: Arc<SubAgentManager>,
    parent_registry: Arc<crate::brain::tools::ToolRegistry>,
}

impl SpawnAgentTool {
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
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a child agent to handle a sub-task autonomously. The child gets its own session \
         and runs in the background, completing naturally when its task is done (its result is \
         delivered to you automatically). Returns an agent_id you can use with wait_agent, \
         send_input (only while Running/AwaitingInput), close_agent, or resume_agent (to \
         continue a completed agent). Use this to delegate independent work items. \
         \n\nProvider and model resolution (highest priority first): \
         (1) the optional `provider` / `model` parameters on THIS call, \
         (2) the user's config.toml `[agent]` keys `subagent_provider` / `subagent_model`, \
         (3) the parent session's provider with that provider's default model. \
         Use the per-call params when a single skill orchestrates multiple steps that each \
         want a different model (for example: plan with one model, code with another, review \
         with a third). Use the config keys when every sub-agent in the session should share \
         the same routing. Use no override to let the child inherit the parent."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task/instruction for the child agent to execute"
                },
                "label": {
                    "type": "string",
                    "description": "Short human-readable label for this sub-agent (e.g., 'refactor-auth', 'test-runner')"
                },
                "allow_nested": {
                    "type": "boolean",
                    "description": "Whether THIS CHILD may spawn further sub-agents or background tasks (#1195). Default true. Set false for pure workers: any spawn_agent/team_create call it makes is refused, and its long bash commands run attached instead of detached. Plan workers are always spawned with false."
                },
                "read_only": {
                    "type": "boolean",
                    "description": "Spawn this child with a read-restricted tool registry (#1173): file reads, glob/grep/ls and web research only — no writes, no bash, no spawning. Use for exploration, code review, and research children. Omit or false for a full-capability worker (still minus recursive/dangerous tools)."
                },
                "include_brain": {
                    "type": "boolean",
                    "description": "Whether to pre-inject workspace brain files (SOUL.md, USER.md, AGENTS.md) into the child agent (#145). Default false (lean execution — sub-agents query workspace files on demand via load_brain_file). Set true when delegating full co-worker tasks that need immediate ambient access to all workspace rules and personality."
                },
                "provider": {
                    "type": "string",
                    "description": "Optional provider override for THIS spawn (e.g., 'zhipu', 'openrouter', 'custom:my-provider'). Highest precedence — overrides config.agent.subagent_provider and parent inheritance. Use to route this single sub-agent differently from the global subagent config."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for THIS spawn (model id as the chosen provider accepts it, e.g., 'glm-5', 'deepseek-coder'). Highest precedence — overrides config.agent.subagent_model. Pair with `provider` when the model lives on a provider other than the parent session's."
                },
                "plan_session": {
                    "type": "string",
                    "description": "Optional session UUID whose plan state this child operates on (#908). When set, the child's plan tool resolves that session's plan (JSON, design .md, markers, task goal) instead of its own. Plan-driven execution passes the parent session id here so a task worker sees the parent's checklist; the child's own session stays fresh. Omit for normal sub-agents."
                }
            },
            "required": ["prompt"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::SystemModification]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        // Nesting enforcement (#1195): children spawned with allow_nested=false
        // are pure workers. Root / unmanaged sessions pass freely.
        if let Some(ref mgr) = context.subagent_manager
            && !mgr.nesting_allowed_for_session(context.session_id)
        {
            return Ok(ToolResult::error(
                "refused: this agent was spawned without nesting permissions \
                 (allow_nested=false) and may not spawn further sub-agents"
                    .to_string(),
            ));
        }
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'prompt' is required".into()))?
            .to_string();

        let label = input
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("sub-agent")
            .to_string();

        // Resolve the child's capability grant (#1173). Explicit `read_only`
        // wins; a deprecated typed `agent_type` maps to its historical
        // effective grant (loudly); anything else defaults to full access.
        // Nesting grant (#1195): absent = true (backward compatible). A
        // non-boolean is a hard error, mirroring read_only above.
        let allow_nested = match input.get("allow_nested") {
            None | Some(serde_json::Value::Bool(_)) => input
                .get("allow_nested")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            Some(_) => {
                return Ok(ToolResult::error(
                    "'allow_nested' must be a boolean (true = child may nest spawns/detach, false = pure worker)"
                        .to_string(),
                ));
            }
        };

        // A non-boolean `read_only` is a hard error, not a silent default —
        // the caller asked for a specific capability and must not get a
        // different one because of a type mistake.
        let explicit_read_only = match input.get("read_only") {
            Some(v) => Some(v.as_bool().ok_or_else(|| {
                ToolError::InvalidInput(
                    "'read_only' must be a boolean (true = restricted registry, false = full)"
                        .into(),
                )
            })?),
            None => None,
        };

        // Brain pre-injection flag (#145): absent = false (lean default).
        // Non-boolean is a hard error.
        let include_brain = match input.get("include_brain") {
            None | Some(serde_json::Value::Bool(_)) => input
                .get("include_brain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            Some(_) => {
                return Ok(ToolResult::error(
                    "'include_brain' must be a boolean (true = pre-inject workspace brain, false = lean)"
                        .to_string(),
                ));
            }
        };
        let deprecated_raw = input
            .get("agent_type")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let (read_only, deprecation_note) = match (explicit_read_only, deprecated_raw) {
            (Some(ro), _) => (ro, None),
            (None, Some(raw)) => {
                let grant =
                    super::map_deprecated_agent_type(&raw).map_err(ToolError::InvalidInput)?;
                tracing::warn!(
                    "spawn_agent called with deprecated agent_type='{raw}'; \
                     mapped to read_only={grant}. Pass read_only explicitly (#1173)."
                );
                (
                    grant,
                    Some(format!(
                        "Deprecated agent_type '{raw}' resolved to read_only={grant}; pass read_only explicitly."
                    )),
                )
            }
            (None, None) => (false, None),
        };

        // Optional plan-state override (#908 option A): plan-driven
        // execution hands the child the PARENT's session id so the worker's
        // plan tool resolves the parent's checklist while the worker session
        // itself stays fresh. A malformed UUID is a hard error — silently
        // falling back to the child's own session would let the worker run
        // against an empty plan and report success on nothing.
        let plan_session_override = match input
            .get("plan_session")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(uuid::Uuid::parse_str(raw).map_err(|e| {
                ToolError::InvalidInput(format!("'plan_session' must be a valid UUID: {e}"))
            })?),
            None => None,
        };

        // We need a ServiceContext to create a session for the child
        let service_context = context
            .service_context
            .as_ref()
            .ok_or_else(|| ToolError::Execution("No service context available".into()))?
            .clone();

        // Create a new session for the child agent
        let session_service = crate::services::SessionService::new(service_context.clone());
        let child_session = session_service
            .create_session(Some(format!("subagent: {}", label)))
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to create child session: {}", e)))?;

        let child_session_id = child_session.id;
        let agent_id = SubAgentManager::generate_id();
        // Cut before the child is built so its working directory can point at
        // the new tree from the first turn.
        let worktree = super::worktree::create(&context.working_dir(), &agent_id);

        // Create cancel token and input channel for the child
        let cancel_token = CancellationToken::new();
        let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();

        // Per-call provider / model overrides, read from the tool
        // call's input. Precedence (issue #152): per-call > config >
        // parent inheritance. Empty strings are treated as unset so an
        // optional schema field passed as "" doesn't accidentally
        // resolve to an invalid provider name.
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

        // Load config and extract model override before entering block scope
        let config = crate::config::Config::load()
            .map_err(|e| ToolError::Execution(format!("Config load failed: {}", e)))?;
        // Precedence: per-call > config > parent default, for provider and
        // model alike; the winning value is normalised so "custom:<name>"
        // or "<provider>/<model>" resolves to the provider it names (#1316).
        // When the model is None the child uses its provider's default.
        let child = super::provider_pair::child_pair(
            &config,
            call_provider.as_deref(),
            call_model.as_deref(),
        );
        let model_override = child.model.clone();
        let effective_provider_name = child.provider.clone();

        // Build a minimal AgentService for the child
        let child_service = {
            // Use the resolved per-call/config provider if any,
            // otherwise inherit parent's. The fallback-on-failure
            // path keeps a typo in the override from breaking the
            // spawn entirely — same shape as the prior config-only
            // resolution.
            let provider = if let Some(ref provider_name) = effective_provider_name {
                match crate::brain::provider::create_provider_by_name(&config, provider_name).await
                {
                    Ok(p) => {
                        let source = match child.source {
                            super::provider_pair::ProviderSource::PerCall => "per-call",
                            super::provider_pair::ProviderSource::Config => "config",
                        };
                        tracing::info!("Sub-agent using {source} provider '{provider_name}'");
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

            // Build the child's tool registry (#1173): parent's tools minus
            // recursive/dangerous ones, then restricted to read-only when the
            // parent granted only read access. The #649 Editing-parent
            // restriction below applies on top of either grant.
            let child_registry = super::build_child_registry(&self.parent_registry);
            // Explicit read_only grant from the spawn call (#1173): strip
            // mutating tools before any other consideration.
            if read_only {
                crate::brain::tools::plan_gate::restrict_registry_to_read_only(&child_registry);
                tracing::info!(
                    "Sub-agent spawned with read_only=true: \
                     child registry restricted to read-only (#1173)"
                );
            }

            // #649: a child spawned while the PARENT session is in Plan-mode
            // Editing must be read-only. The child runs under a fresh session
            // that resolves to NoPlan, so the per-call plan gate never fires
            // for it; strip the mutating tools from its registry instead so it
            // can read and review the design but cannot write the project, run
            // bash, or spawn further agents (which would escape the parent's
            // write-freeze). Currently reachable only after spawn_agent leaves
            // EDITING_DENIED_NAMES; landing this filter first keeps that
            // removal safe.
            if matches!(
                crate::utils::plan_files::plan_mode_state(context.session_id).await,
                crate::utils::plan_files::PlanModeState::PreInitEditing
                    | crate::utils::plan_files::PlanModeState::PostInitEditing
            ) {
                crate::brain::tools::plan_gate::restrict_registry_to_read_only(&child_registry);
                tracing::info!(
                    "Sub-agent spawned under a Plan-mode Editing parent: \
                     child registry restricted to read-only (#649)"
                );
            }

            // A private checkout for this child (#1151). A fan-out spawns
            // several at once against one tree, so they collide by
            // construction rather than by coincidence. `None` is a normal
            // outcome — outside a repository, or if git refuses — and means
            // the child works where it always did.
            let child_dir = worktree
                .as_ref()
                .map(|w| w.path.clone())
                .unwrap_or_else(|| context.working_dir());

            let system_brain = super::brain::child_system_brain(
                include_brain,
                &child_dir,
                model_override.as_deref(),
                effective_provider_name.as_deref(),
            );

            let mut agent =
                crate::brain::agent::AgentService::new(provider, service_context.clone(), &config)
                    .await
                    .with_tool_registry(child_registry)
                    .with_auto_approve_tools(true) // children auto-approve (parent already approved spawn)
                    .with_working_directory(child_dir)
                    // #129: a sub-agent is a headless session (owner ruling C)
                    // — backstop flag so interactive-only tools hard-error.
                    .with_headless(true)
                    // The parent already planned this work; a throwaway child
                    // plan nobody reviews would be worse than no plan.
                    .with_plan_gate_exempt(true)
                    .with_plan_session_override(plan_session_override);

            if let Some(brain) = system_brain {
                agent = agent.with_system_brain(brain);
            }

            Arc::new(agent)
        };

        // Typed preambles are gone (#1173, Proposal B): a restricted child
        // gets one factual capability line derived from its actual grant —
        // no role-play text that could drift from what the registry truly
        // allows. Full-access children receive just the task.
        // #145: stacked capability + lean context note via child_prompt.
        // #129 (owner ruling A): a sub-agent is a headless session — the
        // self-containedness preamble rides on EVERY spawned child prompt,
        // below the capability note and the context note, above the task
        // (child_prompt emits notes-then-task, so the preamble slots
        // between them; byte-exact task preserved).
        let stacked = super::brain::child_prompt(read_only, include_brain, &prompt);
        let notes_len = stacked.len() - prompt.len();
        let full_prompt = format!(
            "{}{}{}",
            &stacked[..notes_len],
            crate::cli::tool_setup::HEADLESS_PREAMBLE.trim_start_matches('\n'),
            &stacked[notes_len..]
        );

        // Create the status file in Pending state before spawning. new()
        // writes the file; we don't need the returned handle, but we do
        // propagate any write error. The parent session is captured before
        // the first write so the file is born with the binding (#110): if
        // the daemon restarts mid-run, boot-resume can route the revived
        // result back to the session that is waiting on it.
        let parent_session_for_status = context.session_id.to_string();
        let _ = WorkStatus::new_agent(
            &agent_id,
            &label,
            &child_session_id.to_string(),
            &full_prompt,
            Some(&parent_session_for_status),
        )
        .map_err(|e| ToolError::Execution(format!("Failed to create status file: {e}")))?;

        // Spawn background task with input loop
        let cancel_clone = cancel_token.clone();
        let manager = self.manager.clone();
        let agent_id_clone = agent_id.clone();
        let worktree_for_cleanup = worktree.clone();
        let prompt_clone = full_prompt;
        let label_clone = label.clone();
        let mut input_rx = input_rx;
        // The session that asked for this agent, so a result nobody is waiting
        // on still reaches the caller instead of sitting in the manager map
        // (#1036). Not the child's session, which nothing is listening to.
        let parent_session_id = context.session_id;

        let handle = tokio::spawn(async move {
            tracing::info!("Sub-agent {} starting: {}", agent_id_clone, prompt_clone);

            // Transition to Running state.
            let mut status = WorkStatus::read(&agent_id_clone).unwrap_or_else(|| {
                WorkStatus::new_agent(
                    &agent_id_clone,
                    &label_clone,
                    &child_session_id.to_string(),
                    &prompt_clone,
                    None,
                )
                .expect("status file")
            });
            if !matches!(status.state, WorkState::Completed | WorkState::Failed)
                && let Err(e) = status.mark_running()
            {
                tracing::warn!("Failed to write running status: {e}");
            }

            // Reload with correct state.

            let mut current_prompt = prompt_clone;
            let mut iteration: usize = 0;

            // Run prompt → wait for input → run again loop
            let final_output = loop {
                iteration += 1;
                let result = child_service
                    .send_message_with_tools_and_mode(
                        child_session_id,
                        current_prompt,
                        model_override.clone(),
                        Some(cancel_clone.clone()),
                    )
                    .await;

                match result {
                    Ok(response) => {
                        // Extract a short summary of what the agent did this turn.
                        let summary = if response.stop_reason
                            == Some(crate::brain::provider::types::StopReason::ToolUse)
                        {
                            "tool call(s) completed".to_string()
                        } else {
                            response.content.chars().take(120).collect::<String>()
                        };

                        status
                            .update_progress(iteration, None, Some(summary))
                            .unwrap_or_else(|e| tracing::warn!("status write failed: {e}"));

                        manager.update_output(&agent_id_clone, response.content.clone());
                        // Natural completion (#1184): the internal tool loop
                        // only returns with a pending tool call when the round
                        // is genuinely gated (approval prompt or iteration
                        // cap). Any other stop reason means the model finished
                        // its answer — completing here delivers the result to
                        // the parent instead of parking the agent as
                        // phantom-"Running" forever. Interactive follow-ups
                        // remain available via `resume_agent`, which accepts
                        // Completed agents.
                        if response.stop_reason
                            != Some(crate::brain::provider::types::StopReason::ToolUse)
                        {
                            tracing::info!(
                                "Sub-agent {} round {} complete naturally, delivering result",
                                agent_id_clone,
                                iteration
                            );
                            break response.content;
                        }
                        // Genuinely gated (pending approval / cap): park so
                        // wait_agent can observe round-boundary progress and
                        // the parent can nudge with input. The status file
                        // parks too (#1183): a parked agent reading
                        // `state: "Running"` with `completed_at: null` misled
                        // every consumer into waiting on finished work.
                        manager.mark_awaiting_input(&agent_id_clone);
                        if let Err(e) = status.mark_awaiting_input() {
                            tracing::warn!(
                                "Failed to write awaiting-input status for {}: {e}",
                                agent_id_clone
                            );
                        }
                        tracing::info!(
                            "Sub-agent {} round {} complete, waiting for input",
                            agent_id_clone,
                            iteration
                        );

                        // Wait for follow-up input or shutdown
                        let next = tokio::select! {
                            msg = input_rx.recv() => msg,
                            _ = cancel_clone.cancelled() => {
                                tracing::info!("Sub-agent {} cancelled while waiting for input", agent_id_clone);
                                None
                            }
                        };

                        match next {
                            Some(text) => {
                                manager.mark_running_again(&agent_id_clone);
                                if let Err(e) = status.mark_running() {
                                    tracing::warn!(
                                        "Failed to write running status for {}: {e}",
                                        agent_id_clone
                                    );
                                }
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
                        tracing::error!("Sub-agent {} failed: {}", agent_id_clone, e);
                        // A dropped write here loses the record of a failure
                        // that already happened, and leaves the file reading
                        // `Running` forever. Proceed either way, but say so.
                        if let Err(write_err) = status.mark_failed(e.to_string()) {
                            tracing::error!(
                                "Sub-agent {} failed and its failure status could not be written, \
                                 so it will keep reading as running: {write_err}",
                                agent_id_clone
                            );
                        }
                        if manager.mark_failed(&agent_id_clone, e.to_string()) {
                            push_result(
                                parent_session_id,
                                &label_clone,
                                &agent_id_clone,
                                Err(&e.to_string()),
                            );
                        }
                        return;
                    }
                }
            };

            // #147: persist the COMPLETE output — the 200-char stub lost
            // every full report to restarts/compaction, and nothing in
            // production ever read the summary's content.
            if let Err(write_err) = status.mark_completed(final_output.clone()) {
                tracing::error!(
                    "Sub-agent {} completed but its status could not be written, so it will keep \
                     reading as running: {write_err}",
                    agent_id_clone
                );
            }
            // Return the tree, unless the child left work in it. A tree with
            // a commit or an uncommitted edit survives: discarding a child's
            // work silently would be worse than the collisions the isolation
            // exists to prevent. Settle this BEFORE reporting, so a kept tree
            // is named in the result the parent reads rather than only in the
            // log, where nobody would find it.
            let final_output = match &worktree_for_cleanup {
                Some(wt) => {
                    let removed = wt.cleanup();
                    match wt.parent_notice(removed) {
                        Some(note) => format!("{final_output}{note}"),
                        None => final_output,
                    }
                }
                None => final_output,
            };
            manager.complete_and_deliver(
                &agent_id_clone,
                final_output,
                parent_session_id,
                &label_clone,
            );
        });

        // Register in manager
        self.manager.insert(SubAgent {
            read_only,
            include_brain,
            allow_nested,
            cancel_token,
            join_handle: Some(handle),
            input_tx: Some(input_tx),
            ..SubAgent::new(
                agent_id.clone(),
                label.clone(),
                child_session_id,
                context.session_id,
            )
        });

        Ok(ToolResult::success(format!(
            "Spawned sub-agent '{}' with id: {}\nSession: {}\nAccess: {}\nBrain: {}\nPrompt: {}{}",
            label,
            agent_id,
            child_session_id,
            if read_only {
                "read-only (#1173): reads/search/research only"
            } else {
                "full (minus recursive/dangerous tools)"
            },
            super::brain::brain_status_label(include_brain),
            prompt,
            deprecation_note
                .as_deref()
                .map(|n| format!("\nNote: {n}"))
                .unwrap_or_default()
        )))
    }
}
