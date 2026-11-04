//! `tool_search` — the agent's on-demand tool discovery.
//!
//! In lazy-tools mode the request carries only the CORE schemas plus this
//! tool. When the agent needs anything else (browser, channels, sub-agents,
//! media, system, …) it calls `tool_search("what I need")`; the matching
//! tools are activated for the session (so their schemas ride on subsequent
//! requests) and their full schemas are returned immediately so the agent can
//! act right away. Mirrors `load_brain_file` for contextual brain files.

use super::error::Result;
use super::registry::ToolRegistry;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Weak};

/// Max tools returned (and activated) per search — keeps a single discovery
/// from re-injecting a huge chunk of the schemas it was meant to avoid.
const MAX_RESULTS: usize = 8;

/// Bound to the registry whose ACTIVE SET the request builder reads.
///
/// That binding is the whole tool (#1210). A sub-agent's registry is a copy
/// of its parent's holding the same `Arc<dyn Tool>` instances, so a
/// `ToolSearchTool` built against the parent kept pointing there after the
/// copy: in a child session it activated into the parent's registry under the
/// child's session id, while the child's request builder read the child's.
/// The searched tool's schema never rode on the child's next request, so the
/// model could not call what it had just been told was callable, and searched
/// again until the loop-breaker killed the turn.
///
/// `Weak` rather than `Arc` because the registry owns this tool: an `Arc`
/// closes a registry -> tool -> registry cycle, which leaks the registry.
/// That was survivable for the one long-lived parent and would not be for a
/// child per spawn.
pub struct ToolSearchTool {
    registry: Weak<ToolRegistry>,
}

impl ToolSearchTool {
    pub fn new(registry: &Arc<ToolRegistry>) -> Self {
        Self {
            registry: Arc::downgrade(registry),
        }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        super::catalog::TOOL_SEARCH_NAME
    }

    fn description(&self) -> &str {
        "Discover and activate tools beyond your always-available core set. You start each turn \
         with core tools (file I/O, shell, search, task/plan/context, http, the brain-file loader, \
         config/session basics). For ANYTHING else — browsing/clicking web pages, sending channel \
         messages (Telegram/Discord/Slack/WhatsApp), spawning sub-agents or teams, generating or \
         analyzing images/video, cron jobs, self-improvement/evolve — call this FIRST with \
         a short description of what you need. It returns the matching tools' exact schemas and \
         makes them callable for the rest of this session. If a task needs a tool you don't see, \
         search for it here before saying you can't do it."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Plain-words description of what you need to do (e.g. 'send a telegram photo', 'click a button on a web page', 'spawn a sub-agent', 'generate an image', 'create a cron job'). May also be a category: browser, channels, agents, media, system, utility."
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Read-only discovery: searches the registry, activates schemas. No
        // file/shell/network side effects, so no approval gate.
        vec![]
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Ok(ToolResult::error(
                "tool_search needs a 'query' — describe what you need to do (e.g. 'send a telegram message').".to_string(),
            ));
        }

        // The registry outlives every tool it holds in practice, so this is a
        // "cannot happen" — say so rather than returning an empty result set,
        // which the model would read as "no such tool exists".
        let Some(registry) = self.registry.upgrade() else {
            tracing::error!(
                "tool_search: its registry is gone, so nothing can be discovered or activated"
            );
            return Ok(ToolResult::error(
                "Tool discovery is unavailable: the tool registry is no longer live. This is a                  bug, not a missing tool — do not conclude the tool you searched for does not                  exist."
                    .to_string(),
            ));
        };

        let matches = registry.search_tools(query, MAX_RESULTS);
        if matches.is_empty() {
            return Ok(ToolResult::success(format!(
                "No additional tools matched \"{query}\". Your core tools may already cover it; \
                 otherwise try different words or a category (browser, channels, agents, media, \
                 system, utility)."
            )));
        }

        // Guest scope (#2134): a stranger's turn may only reach the guest
        // allowlist, so discovery must not advertise what would be refused a
        // moment later. Filtered before activation as well as before display,
        // because an activated-but-refused tool would sit in the request as a
        // dead schema the model keeps retrying.
        let matches: Vec<_> = if context.is_guest() {
            matches
                .into_iter()
                .filter(|(name, ..)| super::scope::guest_allows(name))
                .collect()
        } else {
            matches
        };
        if matches.is_empty() {
            return Ok(ToolResult::success(format!(
                "Nothing outside your current tools matches \"{query}\" on this turn. A guest \
                 turn is scoped to: {}. Answer with those.",
                super::scope::GUEST_ALLOWED_TOOLS.join(", ")
            )));
        }

        // Hand back the full schemas as text so the model can call the ONE it
        // needs immediately with correct params.
        let names: std::collections::HashSet<String> =
            matches.iter().map(|(n, ..)| n.clone()).collect();

        // ...and ACTIVATE them, so their schemas ride on the next request
        // (#1025).
        //
        // #604 withheld activation to keep a search for "send a photo" from
        // ballooning the request with eight unused schemas, leaving activation
        // to the JIT-on-execute path. That path only fires when a non-core tool
        // is actually USED, which is circular: a model that will not emit a
        // call for a function absent from its tool list can never trigger the
        // activation that would list it.
        //
        // Capable models escape by calling from the text schema above. Weaker
        // ones substitute whatever IS active — observed as a model searching
        // for spawn_agent, reporting "Spawn tool active", then emitting
        // `bash(echo "spawning subagent")` and `read_file` instead, across 30
        // tool calls in two runs, until the announcement-loop detector killed
        // both turns. Removing the spawn from the task made it complete
        // immediately.
        //
        // #604's concern is still handled, by the caps that already exist:
        // matches are capped per search (MAX_RESULTS) and `activate_tools`
        // evicts LRU past MAX_ACTIVE_EXTENDED. The request cannot balloon
        // without bound; it just stops lying about what is callable.
        registry.activate_tools(context.session_id, names.iter().cloned());
        let defs = registry.definitions_for(&names);

        let mut out = format!(
            "Found {} tool(s) — call the one you need directly (params below); it activates on use:\n\n",
            defs.len()
        );
        for def in &defs {
            out.push_str(&format!(
                "### {} [{}]\n{}\nschema: {}\n\n",
                def.name,
                super::catalog::tool_category(&def.name),
                def.description,
                serde_json::to_string(&def.input_schema).unwrap_or_default(),
            ));
        }
        // Surface relevant brain guidance alongside the discovered schemas so
        // routing rules (canonical names, gotchas) ride the discovery (#767).
        if let Some(hints) = crate::brain::hints::hints_for(query).await {
            out.push_str(&hints);
        }
        Ok(ToolResult::success(out))
    }
}
