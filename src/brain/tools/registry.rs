//! Tool Registry
//!
//! Manages the collection of available tools that can be invoked by agents.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolExecutionContext, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Per-tool parameter aliases that LLMs commonly confuse.
/// Format: (tool_name, wrong_param, correct_param).
/// Applied before validation so models that send slight variations still work.
const PARAM_ALIASES: &[(&str, &str, &str)] = &[
    // grep/glob: LLMs often send "query" instead of "pattern"
    ("grep", "query", "pattern"),
    ("glob", "query", "pattern"),
    // file tools: "file", "file_path", "filepath" → "path"
    ("read_file", "file", "path"),
    ("read_file", "file_path", "path"),
    ("read_file", "filepath", "path"),
    ("write_file", "file", "path"),
    ("write_file", "file_path", "path"),
    ("write_file", "filepath", "path"),
    ("edit_file", "file", "path"),
    ("edit_file", "file_path", "path"),
    ("edit_file", "filepath", "path"),
    // edit_file: Claude Code sends old_string/new_string → old_text/new_text
    ("edit_file", "old_string", "old_text"),
    ("edit_file", "new_string", "new_text"),
    ("doc_parser", "file", "path"),
    ("doc_parser", "file_path", "path"),
    // write: "text", "body" → "content"
    ("write_file", "text", "content"),
    ("write_file", "body", "content"),
    // bash: "cmd" → "command"
    ("bash", "cmd", "command"),
    // search tools: "pattern" → "query"
    ("web_search", "pattern", "query"),
    ("exa_search", "pattern", "query"),
    ("brave_search", "pattern", "query"),
    ("memory_search", "pattern", "query"),
];

/// Normalize tool input by mapping common LLM parameter name mistakes
/// to the correct parameter name. Only remaps if the correct name is absent.
fn normalize_tool_input(tool_name: &str, mut input: Value) -> Value {
    if let Some(obj) = input.as_object_mut() {
        for &(tool, wrong, correct) in PARAM_ALIASES {
            if tool == tool_name
                && !obj.contains_key(correct)
                && let Some(val) = obj.remove(wrong)
            {
                tracing::debug!(
                    "Normalized tool param: {}.{} → {}.{}",
                    tool_name,
                    wrong,
                    tool_name,
                    correct
                );
                obj.insert(correct.to_string(), val);
            }
        }
    }
    input
}

/// Registry of available tools.
///
/// Thread-safe via internal `RwLock` — all methods take `&self`, allowing
/// runtime registration/removal through a shared `Arc<ToolRegistry>`.
/// Cap on how many EXTENDED tools stay active per session (#603). Once the
/// working set exceeds this, the least-recently-used tools are evicted so the
/// per-turn schema cost can't drift back toward the full ~95-tool baseline over
/// a long session. Evicted tools re-activate on next use (JIT-on-execute), so
/// nothing breaks — the set just stays small. Core tools are never counted here
/// (they always ship).
pub(crate) const MAX_ACTIVE_EXTENDED: usize = 24;

pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    /// EXTENDED tools each session has activated via `tool_search` or a by-name
    /// call (lazy-tools mode), mapped to a monotonic last-touch sequence for
    /// LRU eviction (#603). Lives here because both the agent loop and the
    /// `tool_search` tool already share this registry's `Arc`.
    session_active: RwLock<HashMap<uuid::Uuid, HashMap<String, u64>>>,
    /// Monotonic counter stamping each activation/touch for LRU ordering.
    activation_seq: std::sync::atomic::AtomicU64,
    /// Skill glob gate master switch (`[agent] skill_glob_gate`, #150).
    /// Default true; the AgentService wiring sets this from config.
    skill_gate_enabled: bool,
    /// Mechanical plan requirement (`[agent] plan_gate_after_tool_calls`).
    /// 0 disables it; the AgentService wiring sets this from config.
    plan_gate_after_tool_calls: usize,
}

impl ToolRegistry {
    /// Create a new empty tool registry
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            session_active: RwLock::new(HashMap::new()),
            activation_seq: std::sync::atomic::AtomicU64::new(0),
            skill_gate_enabled: true,
            plan_gate_after_tool_calls: 12,
        }
    }

    /// Set the mechanical plan requirement (`[agent] plan_gate_after_tool_calls`).
    /// Called by the AgentService wiring with the config value; 0 disables it.
    pub fn set_plan_gate_after_tool_calls(&mut self, calls: usize) {
        self.plan_gate_after_tool_calls = calls;
    }

    /// Set the skill glob gate master switch (#150). Called by the
    /// AgentService wiring with the `[agent] skill_glob_gate` config value.
    pub fn set_skill_gate_enabled(&mut self, enabled: bool) {
        self.skill_gate_enabled = enabled;
    }

    /// Mark EXTENDED tools as active for a session (or refresh their recency),
    /// so subsequent requests include their schemas. Called on the JIT-on-execute
    /// path when a non-core tool is actually used — so recency tracks real use.
    /// Evicts the least-recently-used tools once the session exceeds
    /// [`MAX_ACTIVE_EXTENDED`] (#603).
    pub fn activate_tools(&self, session_id: uuid::Uuid, names: impl IntoIterator<Item = String>) {
        use std::sync::atomic::Ordering;
        let mut map = self.session_active.write().unwrap();
        let set = map.entry(session_id).or_default();
        for name in names {
            let seq = self.activation_seq.fetch_add(1, Ordering::Relaxed);
            set.insert(name, seq);
        }
        // LRU eviction: keep only the most-recently-touched MAX_ACTIVE_EXTENDED.
        if set.len() > MAX_ACTIVE_EXTENDED {
            let mut by_recency: Vec<(u64, String)> =
                set.iter().map(|(n, &s)| (s, n.clone())).collect();
            // Newest first, so `skip(MAX)` leaves the oldest to evict.
            by_recency.sort_unstable_by_key(|&(s, _)| std::cmp::Reverse(s));
            for (_, name) in by_recency.into_iter().skip(MAX_ACTIVE_EXTENDED) {
                set.remove(&name);
            }
        }
    }

    /// The EXTENDED tools currently active for a session (empty if none yet).
    pub fn active_tools(&self, session_id: uuid::Uuid) -> std::collections::HashSet<String> {
        self.session_active
            .read()
            .unwrap()
            .get(&session_id)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Register a tool (takes `&self` — safe through shared `Arc`)
    pub fn register(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        tracing::trace!("Registered tool: {}", name);
        self.tools.write().unwrap().insert(name, tool);
    }

    /// Unregister a tool by name. Returns true if it existed.
    pub fn unregister(&self, name: &str) -> bool {
        self.tools.write().unwrap().remove(name).is_some()
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().unwrap().get(name).cloned()
    }

    /// Whether the named tool's successful execution should end the agent
    /// turn after its results flush (e.g. `suggest_options`). Unknown tools
    /// never halt. The policy lives on the tool itself via
    /// [`Tool::halts_turn`]; this is how the tool loop consults it instead
    /// of string-matching names at call sites (#1178 audit finding).
    pub fn halts_turn(&self, name: &str) -> bool {
        self.get(name).is_some_and(|tool| tool.halts_turn())
    }

    /// Check if a tool is registered
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.read().unwrap().contains_key(name)
    }

    /// List all registered tool names
    pub fn list_tools(&self) -> Vec<String> {
        self.tools.read().unwrap().keys().cloned().collect()
    }

    /// Get tool definitions in LLM format
    pub fn get_tool_definitions(&self) -> Vec<crate::brain::provider::Tool> {
        self.collect_definitions(false, None)
    }

    /// The guest-scoped set (#2134): the allowlist only, regardless of the
    /// lazy-tools mode. Used when lazy tools are OFF, where the normal path
    /// ships every schema, but a guest must still see only what it may call.
    pub fn get_tool_definitions_for_guest(&self) -> Vec<crate::brain::provider::Tool> {
        self.collect_definitions(true, None)
    }

    /// Lazy-tools mode: only the schemas for the CORE set plus any EXTENDED
    /// tools the session has activated via `tool_search`. Keeps a "reply yes"
    /// turn from shipping all ~95 schemas (~20k tokens) when it needs none.
    /// A tool the agent hasn't discovered yet is simply omitted — calling
    /// `tool_search` activates it for subsequent requests.
    pub fn get_tool_definitions_filtered(
        &self,
        active_extended: &std::collections::HashSet<String>,
    ) -> Vec<crate::brain::provider::Tool> {
        self.collect_definitions(false, Some(active_extended))
    }

    /// Lazy-tools filtering plus the per-caller scope (#2134).
    ///
    /// `guest_scope` narrows the set to [`super::scope::GUEST_ALLOWED_TOOLS`]
    /// BEFORE the core/extended split, so a stranger's turn is never even shown
    /// a schema it would be refused for calling. Default-deny: a tool added
    /// later stays out of reach until it is listed.
    pub fn get_tool_definitions_scoped(
        &self,
        active_extended: &std::collections::HashSet<String>,
        guest_scope: bool,
    ) -> Vec<crate::brain::provider::Tool> {
        self.collect_definitions(guest_scope, Some(active_extended))
    }

    /// One filter, three public shapes. `lazy` is `Some(active_extended)` when
    /// lazy tools are on (core plus what the session activated) and `None` when
    /// every registered tool should ride.
    fn collect_definitions(
        &self,
        guest_scope: bool,
        lazy: Option<&std::collections::HashSet<String>>,
    ) -> Vec<crate::brain::provider::Tool> {
        use crate::brain::tools::catalog;
        self.tools
            .read()
            .unwrap()
            .values()
            .filter(|tool| {
                let name = tool.name();
                if guest_scope && !super::scope::guest_allows(name) {
                    return false;
                }
                match lazy {
                    Some(active) => catalog::is_core(name) || active.contains(name),
                    None => true,
                }
            })
            .map(|tool| crate::brain::provider::Tool {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    /// Find EXTENDED (non-core) tools matching a free-text query, ranked by
    /// how well the query terms hit the tool's name + description. Powers the
    /// `tool_search` discovery tool. Returns `(name, category, description)`,
    /// best matches first, capped at `limit`.
    pub fn search_tools(&self, query: &str, limit: usize) -> Vec<(String, String, String)> {
        use crate::brain::tools::catalog;
        let q = query.to_ascii_lowercase();
        let terms: Vec<&str> = q.split_whitespace().filter(|t| t.len() > 1).collect();
        let mut scored: Vec<(i32, String, String, String)> = self
            .tools
            .read()
            .unwrap()
            .values()
            .filter(|tool| !catalog::is_core(tool.name()))
            .map(|tool| {
                let name = tool.name().to_string();
                let desc = tool.description().to_string();
                let category = catalog::tool_category(&name).to_string();
                let hay = format!("{name} {category} {desc}").to_ascii_lowercase();
                let mut score = 0i32;
                for term in &terms {
                    if name.to_ascii_lowercase().contains(term) {
                        score += 5; // name hit is the strongest signal
                    } else if category.contains(term) {
                        score += 3;
                    } else if hay.contains(term) {
                        score += 1;
                    }
                }
                (score, name, category, desc)
            })
            .filter(|(score, ..)| *score > 0)
            .collect();
        // Highest score first; stable tie-break by name for determinism.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, name, category, desc)| (name, category, desc))
            .collect()
    }

    /// Full provider-format definitions for a specific set of tool names (used
    /// by `tool_search` to hand the agent the exact schemas it just discovered).
    pub fn definitions_for(
        &self,
        names: &std::collections::HashSet<String>,
    ) -> Vec<crate::brain::provider::Tool> {
        self.tools
            .read()
            .unwrap()
            .values()
            .filter(|tool| names.contains(tool.name()))
            .map(|tool| crate::brain::provider::Tool {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    /// Execute a tool by name
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        // Resolve the tool. On an unknown name, try the tool-name self-heal
        // (a weaker model guessing `tg_send_message` for `telegram_send`,
        // issue #176) before giving up. The healed name is used for param
        // normalization and logging so everything downstream sees the real
        // tool.
        let (tool, resolved_name) = match self.get(name) {
            Some(t) => (t, name.to_string()),
            None => {
                let registered = self.list_tools();
                match super::tool_name_heal::resolve_tool_name(name, &registered) {
                    Some(real) => {
                        tracing::warn!(
                            "Self-healed tool name: '{}' → '{}' (model called a near-miss name)",
                            name,
                            real
                        );
                        let t = self
                            .get(&real)
                            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
                        (t, real)
                    }
                    None => return Err(ToolError::NotFound(name.to_string())),
                }
            }
        };
        let name = resolved_name.as_str();

        // Per-caller scope (#2134). The schema filter already hides these from
        // a guest's tool list, so reaching this branch means the model called a
        // tool it was never shown (a remembered name, or a `tool_search` result
        // from a shared session). Refuse before validation, before activation,
        // and before any approval prompt: a guest must never learn what a
        // refused tool would have done.
        if context.is_guest() && !super::scope::guest_allows(name) {
            tracing::info!(
                "Guest scope refused tool '{}' for session {}",
                name,
                context.session_id
            );
            return Ok(ToolResult::error(super::scope::guest_refusal(name)));
        }

        // JIT discovery (#214): when lazy_tools is on, a tool the model called
        // by name but never surfaced via `tool_search` is absent from the
        // system prompt, so the model is guessing its params blind. Activate it
        // now, BEFORE validation, so even if THIS call fails on bad params the
        // next request carries the real schema and the model self-corrects
        // instead of looping. No-op for CORE tools (always present); harmless
        // when lazy_tools is off (the active set is never consulted).
        if !crate::brain::tools::catalog::is_core(name) {
            self.activate_tools(context.session_id, [name.to_string()]);
        }

        // Normalize LLM parameter name mistakes before validation
        let input = normalize_tool_input(name, input);

        // Validate input
        tool.validate_input(&input)?;

        // Plan-mode write/bash gate: while a session is Editing (pre-init
        // or design prose) or has a frozen Active design .md, some tools
        // are deterministically refused with an instructive reason. Checked
        // after validation so the model's params are sane, before approval
        // so the user is never prompted for a call that cannot run.
        //
        // #908 option A: the gate keys on the PLAN session, not the raw
        // session id. A spawned plan worker carries the parent's plan
        // override, so its mutators are gated by the PARENT's plan state
        // (write freeze during Editing, autonomy grant, seed window) —
        // matching the spawn-time #649 lock. Without an override this is
        // exactly session_id: normal sessions behave as before.
        let plan_sid = context.plan_session_override.unwrap_or(context.session_id);
        match super::plan_gate::check_plan_gate(plan_sid, name, &tool.hints(), &input).await {
            super::plan_gate::GateDecision::Allow => { /* proceed */ }
            super::plan_gate::GateDecision::Deny(reason) => {
                tracing::info!("Plan gate denied tool '{}': session is plan-gated", name);
                return Ok(ToolResult::error(reason));
            }
            super::plan_gate::GateDecision::RequireApproval(reason) => {
                if context.auto_approve {
                    // User already approved via the Telegram/Discord/Slack
                    // approval callback (or YOLO mode is active). Let the
                    // call proceed — the plan gate's intent ("require
                    // approval for mutators during Editing") is satisfied.
                    tracing::info!(
                        "Plan gate: allowing '{}' (already approved) — {}",
                        name,
                        reason
                    );
                } else {
                    tracing::info!(
                        "Plan gate requires approval for tool '{}': {}",
                        name,
                        reason
                    );
                    return Err(ToolError::ApprovalRequired(reason));
                }
            }
        }

        // Mechanical plan requirement. Distinct from the plan-MODE gate
        // above: that one governs writes while a plan is being drafted or
        // executed, this one decides that a long ad-hoc turn must have a plan
        // at all. Checked here so every dispatch path (sequential, parallel,
        // sub-agent) passes through the same choke point, and before approval
        // so the user is never prompted for a call that cannot run.
        match super::plan_gate::check_plan_requirement(
            plan_sid,
            name,
            context.turn_tool_calls,
            self.plan_gate_after_tool_calls,
            context.plan_gate_exempt,
        )
        .await
        {
            super::plan_gate::GateDecision::Allow => { /* proceed */ }
            super::plan_gate::GateDecision::Deny(reason) => {
                tracing::info!(
                    "Plan requirement denied tool '{}' at {} calls this turn",
                    name,
                    context.turn_tool_calls
                );
                return Ok(ToolResult::error(reason));
            }
            super::plan_gate::GateDecision::RequireApproval(_) => {
                // This gate never asks for approval: the requirement is
                // "create a plan", not "confirm this call".
            }
        }

        // Skill glob gate (issue #150): a tool call touching a path that
        // matches a globs-declaring skill the session hasn't loaded is
        // rejected with the skill's FULL body as the error content (the
        // plan_gate deny precedent) and `mark_seen` arms the identical
        // retry. Fires per call inside `execute` — sequential, parallel,
        // and sub-agent dispatch all pass through here, so there is no
        // batch-splice logic to get wrong. Fail-open: any internal gate
        // error is a `Pass` inside `skill_gate::check` itself.
        let verdict = super::skill_gate::check(
            context.session_id,
            name,
            &input,
            &context.working_directory,
            self.skill_gate_enabled,
        );
        if let super::skill_gate::GateVerdict::Block {
            skill,
            matched_path,
            body,
            globs,
        } = verdict
        {
            tracing::info!(
                "skill_gate: blocked '{}' — path '{}' matches skill '{}' (not loaded in context)",
                name,
                matched_path,
                skill
            );
            // Arm the retry BEFORE the agent sees the result: with the
            // epoch-carrying registry (#150) this upserts the current
            // epoch, so the identical re-issued call passes.
            super::seen_skills::mark_seen(context.session_id, &skill);
            let content = format!(
                "[SKILL GATE] This call touches '{}' which matches skill '{}' (globs: {}), \
                 not loaded in the current session context. The full skill body follows. \
                 Read it, then re-issue the identical call.\n\n---\n\n{}",
                matched_path,
                skill,
                globs.join(", "),
                body
            );
            return Ok(ToolResult::error(content));
        }

        // Check if approval is required
        if tool.requires_approval() && !context.auto_approve {
            return Err(ToolError::ApprovalRequired(format!(
                "Tool '{}' requires approval before execution",
                name
            )));
        }

        // Execute the tool
        tracing::debug!("Executing tool: {}", name);
        let is_md_write = tool
            .capabilities()
            .contains(&crate::brain::tools::r#trait::ToolCapability::WriteFiles)
            && super::plan_gate::write_targets_session_md(plan_sid, &input).await;
        let result = tool.execute(input, context).await?;

        // Editing mirror: a successful write to the session plan .md syncs
        // the full body into the JSON `description` (tasks stay empty), so
        // the .md remains the Editing source of truth and every JSON reader
        // (TUI chrome, Telegram sections) sees the fresh design. Malformed
        // template writes are refused and restored by the sync boundary.
        if result.success
            && is_md_write
            && let Err(error) = crate::utils::plan_files::sync_md_to_json(plan_sid).await
        {
            tracing::info!("Plan .md write refused by template guard");
            return Ok(ToolResult::error(error));
        }

        if result.success {
            tracing::debug!("Tool '{}' executed successfully", name);
        } else {
            tracing::warn!(
                "Tool '{}' failed: {:?}",
                name,
                result.error.as_deref().unwrap_or("unknown error")
            );
        }

        // A successful call that changed the world can invalidate a recorded
        // bash failure (#2211): the write that created the missing file is the
        // reported case, so the retry the user asked for has to run. Read-only
        // calls do not clear it, so fail -> read -> retry the identical command
        // stays blocked, which is the loop this guard exists to stop. Bash
        // itself never clears the window for the same reason.
        if result.success
            && name != "bash"
            && (tool
                .capabilities()
                .contains(&crate::brain::tools::r#trait::ToolCapability::WriteFiles)
                || tool
                    .capabilities()
                    .contains(&crate::brain::tools::r#trait::ToolCapability::SystemModification))
        {
            super::bash::note_environment_changed(context.session_id);
        }

        Ok(result)
    }

    /// Get the number of registered tools
    pub fn count(&self) -> usize {
        self.tools.read().unwrap().len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
