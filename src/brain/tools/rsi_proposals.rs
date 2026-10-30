//! User-facing proposal review tool.
//!
//! Lists, applies, or rejects proposals filed by the autonomous RSI loop
//! (via `rsi_propose`). Apply paths reuse the same plumbing as
//! `tool_manage` `add` (DynamicToolLoader::add_tool) and `config_manager`
//! `add_command` (CommandLoader::add_command), so an applied proposal
//! is byte-for-byte equivalent to one the agent had typed in itself.
//!
//! When the user says "show me what RSI proposed" or "implement those
//! proposals", the agent calls this. No approval prompt — by design:
//! the user already triggered this verbally, and the audit trail under
//! `~/.opencrabs/rsi/applied/` and `rejected/` retains everything.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::brain::CommandLoader;
use crate::brain::rsi_proposals::{CommandProposal, ProposalsStore, SkillProposal, ToolProposal};
use crate::brain::tools::ToolRegistry;
use crate::brain::tools::dynamic::DynamicToolLoader;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub struct RsiProposalsTool {
    registry: Arc<ToolRegistry>,
    tools_path: PathBuf,
    brain_path: PathBuf,
}

impl RsiProposalsTool {
    pub fn new(registry: Arc<ToolRegistry>, tools_path: PathBuf, brain_path: PathBuf) -> Self {
        Self {
            registry,
            tools_path,
            brain_path,
        }
    }

    fn store(&self) -> ProposalsStore {
        // Always recompute against the actual rsi dir — keeps the tool
        // honest against profile switches that move opencrabs_home().
        ProposalsStore::with_dir(self.brain_path.join("rsi"))
    }

    fn render_list(&self) -> String {
        let store = self.store();
        let tools = store.list_tool_proposals();
        let cmds = store.list_command_proposals();
        let skills = store.list_skill_proposals();

        if tools.is_empty() && cmds.is_empty() && skills.is_empty() {
            return "No pending proposals.".to_string();
        }

        let mut out = String::new();
        if !tools.is_empty() {
            out.push_str(&format!("## Pending tool proposals ({})\n\n", tools.len()));
            for p in &tools {
                out.push_str(&format_tool_proposal(p));
            }
        }
        if !cmds.is_empty() {
            out.push_str(&format!(
                "\n## Pending command proposals ({})\n\n",
                cmds.len()
            ));
            for p in &cmds {
                out.push_str(&format_command_proposal(p));
            }
        }
        if !skills.is_empty() {
            out.push_str(&format!(
                "\n## Pending skill proposals ({})\n\n",
                skills.len()
            ));
            for p in &skills {
                out.push_str(&format_skill_proposal(p));
            }
        }
        out
    }

    pub(crate) fn apply_tool(&self, id: &str) -> std::result::Result<String, String> {
        let store = self.store();
        let Some(proposal) = store
            .take_tool_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        else {
            return Err(format!("No tool proposal with id '{id}'"));
        };

        // Hand off to the same loader tool_manage uses — proposals never
        // get a special install path.
        if let Err(e) =
            DynamicToolLoader::add_tool(&self.tools_path, proposal.def.clone(), &self.registry)
        {
            return Err(format!("install failed: {e}"));
        }

        if let Err(e) = store.archive_applied_tool(&proposal) {
            tracing::warn!("Tool {} installed but archive write failed: {}", id, e);
        }

        Ok(format!(
            "Installed tool '{}' (proposal {}). Live now in tools.toml.",
            proposal.def.name, id
        ))
    }

    pub(crate) fn apply_command(&self, id: &str) -> std::result::Result<String, String> {
        let store = self.store();
        let Some(proposal) = store
            .take_command_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        else {
            return Err(format!("No command proposal with id '{id}'"));
        };

        let loader = CommandLoader::from_brain_path(&self.brain_path);
        if let Err(e) = loader.add_command(proposal.command.clone()) {
            return Err(format!("install failed: {e}"));
        }

        if let Err(e) = store.archive_applied_command(&proposal) {
            tracing::warn!("Command {} installed but archive write failed: {}", id, e);
        }

        Ok(format!(
            "Installed command '{}' (proposal {}). Live now in commands.toml.",
            proposal.command.name, id
        ))
    }

    /// Install a proposed skill by writing
    /// `<brain_path>/skills/<name>/SKILL.md` with YAML frontmatter
    /// (name + description) wrapping the proposed body. Refuses to
    /// overwrite an existing skill so a user who already wrote a
    /// `<name>` skill manually doesn't have it silently replaced by
    /// an RSI proposal that picked the same slug — they get an error
    /// and can reject the proposal instead.
    pub(crate) fn apply_skill(&self, id: &str) -> std::result::Result<String, String> {
        let store = self.store();
        let Some(proposal) = store
            .take_skill_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        else {
            return Err(format!("No skill proposal with id '{id}'"));
        };

        let skill_dir = self.brain_path.join("skills").join(&proposal.skill.name);
        let skill_path = skill_dir.join("SKILL.md");
        if skill_path.exists() {
            return Err(format!(
                "skill '{}' already exists at {} — reject the proposal or remove the existing skill first",
                proposal.skill.name,
                skill_path.display(),
            ));
        }

        if let Err(e) = std::fs::create_dir_all(&skill_dir) {
            return Err(format!("create skill dir {}: {e}", skill_dir.display()));
        }

        // Mirror the format documented in TOOLS.md: YAML frontmatter
        // with `name` + `description`, then the multi-line body.
        let contents = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            proposal.skill.name,
            proposal.skill.description.replace('\n', " ").trim(),
            proposal.skill.body.trim_end(),
        );
        if let Err(e) = std::fs::write(&skill_path, contents) {
            return Err(format!("write {}: {e}", skill_path.display()));
        }

        if let Err(e) = store.archive_applied_skill(&proposal) {
            tracing::warn!("Skill {} installed but archive write failed: {}", id, e);
        }

        Ok(format!(
            "Installed skill '{}' (proposal {}). Live at {}.",
            proposal.skill.name,
            id,
            skill_path.display(),
        ))
    }

    pub(crate) fn reject(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> std::result::Result<String, String> {
        let store = self.store();

        if let Some(p) = store
            .take_tool_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        {
            if let Err(e) = store.archive_rejected_tool(&p, reason) {
                return Err(format!("archive failed: {e}"));
            }
            return Ok(format!("Rejected tool proposal '{}'.", id));
        }

        if let Some(p) = store
            .take_command_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        {
            if let Err(e) = store.archive_rejected_command(&p, reason) {
                return Err(format!("archive failed: {e}"));
            }
            return Ok(format!("Rejected command proposal '{}'.", id));
        }

        if let Some(p) = store
            .take_skill_proposal(id)
            .map_err(|e| format!("read inbox: {e}"))?
        {
            if let Err(e) = store.archive_rejected_skill(&p, reason) {
                return Err(format!("archive failed: {e}"));
            }
            return Ok(format!("Rejected skill proposal '{}'.", id));
        }

        Err(format!("No proposal with id '{id}'"))
    }
}

fn format_tool_proposal(p: &ToolProposal) -> String {
    let cmd_or_url = match (&p.def.command, &p.def.url) {
        (Some(c), _) => format!("shell: `{}`", c),
        (_, Some(u)) => format!("{} {}", p.def.method.as_deref().unwrap_or("GET"), u),
        _ => "(no command/url)".to_string(),
    };
    format!(
        "- **{id}** — `{name}`\n  {desc}\n  {payload}\n  Why: {why}\n  Filed: {when}\n\n",
        id = p.id,
        name = p.def.name,
        desc = p.def.description,
        payload = cmd_or_url,
        why = p.rationale,
        when = p.created_at.format("%Y-%m-%d %H:%M UTC"),
    )
}

fn format_command_proposal(p: &CommandProposal) -> String {
    format!(
        "- **{id}** — `{name}`\n  {desc}\n  Prompt: \"{prompt}\"\n  Why: {why}\n  Filed: {when}\n\n",
        id = p.id,
        name = p.command.name,
        desc = p.command.description,
        prompt = if p.command.prompt.len() > 80 {
            format!(
                "{}...",
                crate::utils::string::truncate_str(&p.command.prompt, 77)
            )
        } else {
            p.command.prompt.clone()
        },
        why = p.rationale,
        when = p.created_at.format("%Y-%m-%d %H:%M UTC"),
    )
}

fn format_skill_proposal(p: &SkillProposal) -> String {
    let body_lines = p.skill.body.lines().count();
    format!(
        "- **{id}** — `{name}`\n  {desc}\n  Body: {lines} lines (lands at skills/{name}/SKILL.md in your OpenCrabs home)\n  Why: {why}\n  Filed: {when}\n\n",
        id = p.id,
        name = p.skill.name,
        desc = p.skill.description,
        lines = body_lines,
        why = p.rationale,
        when = p.created_at.format("%Y-%m-%d %H:%M UTC"),
    )
}

#[async_trait]
impl Tool for RsiProposalsTool {
    fn name(&self) -> &str {
        "rsi_proposals"
    }

    fn description(&self) -> &str {
        "List, apply, or reject tools/commands proposed by the autonomous RSI loop. \
         Use 'list' to show pending proposals, 'apply' to install one (or 'all') into \
         the live tools.toml/commands.toml, 'reject' to discard with an optional reason. \
         Applied/rejected entries archive under `rsi/{applied,rejected}/` in your OpenCrabs home."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "apply", "reject"],
                    "description": "list: show pending proposals; apply: install into tools.toml/commands.toml; reject: archive without installing"
                },
                "id": {
                    "type": "string",
                    "description": "Proposal id from list output. Required for apply/reject. Pass 'all' to apply/reject every pending proposal."
                },
                "kind": {
                    "type": "string",
                    "enum": ["tool", "command", "skill", "all"],
                    "description": "(list) Filter by kind. Default: 'all'.",
                    "default": "all"
                },
                "reason": {
                    "type": "string",
                    "description": "(reject) Optional human-facing reason recorded in the rejection archive."
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Apply paths can register new shell tools — same capability surface
        // as tool_manage. We mark it but don't require approval (the user
        // already asked for "implement proposed", and individual proposed
        // tools carry their own `requires_approval` for runtime calls).
        vec![ToolCapability::ExecuteShell]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match action.as_str() {
            "list" => Ok(ToolResult::success(self.render_list())),

            "apply" => {
                let id = input
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return Ok(ToolResult::error(
                        "id is required (or 'all' to apply every pending proposal)".to_string(),
                    ));
                }

                if id == "all" {
                    let store = self.store();
                    let tool_ids: Vec<String> = store
                        .list_tool_proposals()
                        .into_iter()
                        .map(|p| p.id)
                        .collect();
                    let cmd_ids: Vec<String> = store
                        .list_command_proposals()
                        .into_iter()
                        .map(|p| p.id)
                        .collect();

                    if tool_ids.is_empty() && cmd_ids.is_empty() {
                        return Ok(ToolResult::success("No pending proposals.".to_string()));
                    }

                    let mut report = String::new();
                    let mut total_ok = 0usize;
                    let mut total_err = 0usize;
                    for tid in tool_ids {
                        match self.apply_tool(&tid) {
                            Ok(msg) => {
                                report.push_str(&format!("✓ {}\n", msg));
                                total_ok += 1;
                            }
                            Err(e) => {
                                report.push_str(&format!("✗ tool {}: {}\n", tid, e));
                                total_err += 1;
                            }
                        }
                    }
                    for cid in cmd_ids {
                        match self.apply_command(&cid) {
                            Ok(msg) => {
                                report.push_str(&format!("✓ {}\n", msg));
                                total_ok += 1;
                            }
                            Err(e) => {
                                report.push_str(&format!("✗ command {}: {}\n", cid, e));
                                total_err += 1;
                            }
                        }
                    }
                    report.push_str(&format!(
                        "\nApplied {} proposal(s); {} failed.",
                        total_ok, total_err
                    ));
                    return Ok(ToolResult::success(report));
                }

                // Specific id: try tool first, then command. The id prefix
                // (`prop_tool_...` vs `prop_cmd_...`) tells us which.
                if id.starts_with("prop_tool_") {
                    match self.apply_tool(&id) {
                        Ok(msg) => Ok(ToolResult::success(msg)),
                        Err(e) => Ok(ToolResult::error(e)),
                    }
                } else if id.starts_with("prop_cmd_") {
                    match self.apply_command(&id) {
                        Ok(msg) => Ok(ToolResult::success(msg)),
                        Err(e) => Ok(ToolResult::error(e)),
                    }
                } else {
                    // Unknown prefix — try each kind, bail if none match.
                    match self.apply_tool(&id) {
                        Ok(msg) => Ok(ToolResult::success(msg)),
                        Err(_) => match self.apply_command(&id) {
                            Ok(msg) => Ok(ToolResult::success(msg)),
                            Err(e) => Ok(ToolResult::error(e)),
                        },
                    }
                }
            }

            "reject" => {
                let id = input
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = input
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if id.is_empty() {
                    return Ok(ToolResult::error(
                        "id is required (or 'all' to reject every pending proposal)".to_string(),
                    ));
                }

                if id == "all" {
                    let store = self.store();
                    let tool_ids: Vec<String> = store
                        .list_tool_proposals()
                        .into_iter()
                        .map(|p| p.id)
                        .collect();
                    let cmd_ids: Vec<String> = store
                        .list_command_proposals()
                        .into_iter()
                        .map(|p| p.id)
                        .collect();
                    let total = tool_ids.len() + cmd_ids.len();
                    if total == 0 {
                        return Ok(ToolResult::success("No pending proposals.".to_string()));
                    }
                    for tid in tool_ids {
                        let _ = self.reject(&tid, reason.as_deref());
                    }
                    for cid in cmd_ids {
                        let _ = self.reject(&cid, reason.as_deref());
                    }
                    return Ok(ToolResult::success(format!(
                        "Rejected {total} pending proposal(s)."
                    )));
                }

                match self.reject(&id, reason.as_deref()) {
                    Ok(msg) => Ok(ToolResult::success(msg)),
                    Err(e) => Ok(ToolResult::error(e)),
                }
            }

            other => Ok(ToolResult::error(format!(
                "action must be 'list', 'apply', or 'reject', got '{other}'"
            ))),
        }
    }
}
