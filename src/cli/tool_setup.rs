//! Shared agent tool-registry construction.
//!
//! The interactive startup (TUI + channels) and the headless multi-profile
//! cron daemon both need an `AgentService` backed by the same core tools.
//! Keeping the registration in ONE place is what stops the daemon from running
//! cron jobs against an empty registry ("No tools registered" →
//! "Tool not found: bash") and keeps the two entry points from drifting.

use std::sync::Arc;

use crate::brain::tools::registry::ToolRegistry;
use crate::config::Config;
use crate::db::Database;

/// Register the dependency-light CORE agent tools: file ops, shell, search,
/// workflow, memory/brain, session/channel/cron/a2a/config/slash/rename,
/// follow-up, tool discovery, the config-dependent tools, sub-agent + team
/// orchestration, and RSI. These need only the DB pool + config (managers are
/// created inline and used nowhere else), so both entry points can share one
/// registry definition.
///
/// Browser, channel-send, media, and evolve tools are registered
/// separately by the interactive path — they need managers the daemon lacks.
///
/// `headless` gates the two tools that are meaningless without a live user
/// surface (#129, owner-ruled design in fork issue body, 2026-09-07):
/// - `session_notify` (parks the notification until a turn-end that never
///   comes on a one-shot run; sub-agents report via the harness's
///   final-message relay instead)
/// - `suggest_options` (renders nowhere headless — no TUI, no channel
///   handler to consume the option message)
///
/// Excluding them here is the PRIMARY gate; the tool-level context guard
/// (`ToolExecutionContext.headless` → hard `ToolError`) is the
/// belt-and-braces backstop, so a registration leak fails loudly instead
/// of silently parking a verdict. Interactive callers pass `false`.
/// Public entry for surfaces outside the lib tree (the gpui gui bin) that
/// need the exact core tool set the TUI ships. Same body, same wiring.
pub fn register_core_agent_tools_embedded(
    tool_registry: &Arc<ToolRegistry>,
    db: &Database,
    config: &Config,
) -> Arc<crate::brain::tools::subagent::SubAgentManager> {
    register_core_agent_tools(tool_registry, db, config, false)
}

pub(crate) fn register_core_agent_tools(
    tool_registry: &Arc<ToolRegistry>,
    db: &Database,
    config: &Config,
    headless: bool,
) -> Arc<crate::brain::tools::subagent::SubAgentManager> {
    use crate::brain::tools::{
        bash::BashTool, code_exec::CodeExecTool, config_tool::ConfigTool, context::ContextTool,
        doc_gen::GenerateDocumentTool, doc_parser::DocParserTool, edit::EditTool, glob::GlobTool,
        grep::GrepTool, http::HttpClientTool, load_brain_file::LoadBrainFileTool, ls::LsTool,
        memory_search::MemorySearchTool, notebook::NotebookEditTool,
        pdf_to_images::PdfToImagesTool, plan_tool::PlanTool, read::ReadTool,
        rename_session::RenameSessionTool, session_search::SessionSearchTool,
        slash_command::SlashCommandTool, suggest_options::SuggestOptionsTool,
        web_search::WebSearchTool, write::WriteTool, write_opencrabs_file::WriteOpenCrabsFileTool,
    };
    // Phase 1: Essential file operations
    tool_registry.register(Arc::new(ReadTool));
    tool_registry.register(Arc::new(WriteTool));
    tool_registry.register(Arc::new(EditTool));
    tool_registry.register(Arc::new(crate::brain::tools::hashline::HashlineEditTool));
    tool_registry.register(Arc::new(BashTool));
    tool_registry.register(Arc::new(LsTool));
    tool_registry.register(Arc::new(GlobTool));
    tool_registry.register(Arc::new(GrepTool));
    // Phase 2: Advanced features
    tool_registry.register(Arc::new(WebSearchTool::default()));
    tool_registry.register(Arc::new(CodeExecTool));
    tool_registry.register(Arc::new(NotebookEditTool));
    tool_registry.register(Arc::new(DocParserTool));
    tool_registry.register(Arc::new(GenerateDocumentTool));
    tool_registry.register(Arc::new(PdfToImagesTool));
    // Phase 3: Workflow & integration
    // task_manager is deprecated in favor of the plan tool (mode=checklist)
    // and no longer registered, so it leaves the model-facing catalog and
    // tool_search. The TaskTool code stays in place (brain::tools::task) for
    // reference and any out-of-band callers.
    tool_registry.register(Arc::new(ContextTool));
    tool_registry.register(Arc::new(HttpClientTool));
    tool_registry.register(Arc::new(PlanTool));
    // Memory search (built-in FTS5, always available)
    tool_registry.register(Arc::new(MemorySearchTool));
    // On-demand brain file loader — agent fetches USER.md, MEMORY.md etc. only when needed
    tool_registry.register(Arc::new(LoadBrainFileTool));
    // OpenCrabs file writer — agent can edit/append/overwrite any file in ~/.opencrabs/
    tool_registry.register(Arc::new(WriteOpenCrabsFileTool));
    // Session search — hybrid QMD search across all session message history
    tool_registry.register(Arc::new(SessionSearchTool::new(db.pool().clone())));
    // Mission control report — shareable analytics/activity/inbox/schedule the agent can send to a chat
    tool_registry.register(Arc::new(
        crate::brain::tools::mission_control_report::MissionControlReportTool::new(
            db.pool().clone(),
        ),
    ));
    // Channel search — search passively captured channel messages (Telegram groups, etc.)
    tool_registry.register(Arc::new(
        crate::brain::tools::channel_search::ChannelSearchTool::new(
            crate::db::ChannelMessageRepository::new(db.pool().clone()),
        ),
    ));
    // Cron job management — agent can create/list/delete/enable/disable scheduled jobs
    tool_registry.register(Arc::new(
        crate::brain::tools::cron_manage::CronManageTool::new(crate::db::CronJobRepository::new(
            db.pool().clone(),
        )),
    ));
    // Goal management — agent can set/manage its own autonomous session goal,
    // so it can drive multi-turn test/review/fix cycles instead of waiting for
    // the human to type /goal. The post-turn judge already activates on any
    // active goal row, tool-set or slash-set alike.
    tool_registry.register(Arc::new(crate::brain::tools::goal_manage::GoalManageTool));
    // A2A send — agent can communicate with remote A2A agents
    tool_registry.register(Arc::new(crate::brain::tools::a2a_send::A2aSendTool::new()));
    tool_registry.register(Arc::new(
        crate::brain::tools::profile_list::ProfileListTool::new(),
    ));
    // Tasks list — one roster of sub-agents + detached commands (#1160)
    tool_registry.register(Arc::new(
        crate::brain::tools::tasks_list::TasksListTool::new(),
    ));
    // Config management (read/write config.toml, commands.toml)
    tool_registry.register(Arc::new(ConfigTool));
    // L1 decision-reuse ring (#1648) — shadow by default; the release-day
    // keep-or-cut evaluation runs on its persisted counters.
    tool_registry.register(Arc::new(
        crate::brain::tools::decide_cached::DecideCachedTool::new(db.pool().clone()),
    ));
    // Slash command invocation (agent can call any slash command)
    tool_registry.register(Arc::new(SlashCommandTool));
    // Session rename — agent can update the current session's title
    tool_registry.register(Arc::new(RenameSessionTool));
    // #129: interactive-surface-only tools. On headless paths (CLI one-shot
    // run, cron daemon execute, sub-agent spawn) neither tool can reach a
    // user: session_notify parks until a turn-end relay that never fires on
    // a one-shot process, and suggest_options renders nowhere. Skipped at
    // registration; the ToolExecutionContext.headless guard backstops.
    if !headless {
        // Follow-up question — agent asks the user a multi-choice question
        // mid-task and blocks until they click an option button.
        tool_registry.register(Arc::new(SuggestOptionsTool));
    }
    // Tool discovery — lets the agent activate extended tools on demand
    // (lazy-tools mode). Holds the registry Arc so it can search all tools;
    // harmless when lazy_tools is off (just one more always-available tool).
    tool_registry.register(Arc::new(
        crate::brain::tools::tool_search::ToolSearchTool::new(tool_registry),
    ));
    // Tools whose availability depends on config / keys (EXA, Brave, image
    // generation, vision/video). Shared with the config watcher so a key added
    // to keys.toml is picked up at runtime with no restart, in the daemon too.
    super::ui::register_config_dependent_tools(tool_registry, config);

    // Phase 5: Multi-agent orchestration
    let subagent_manager = Arc::new(crate::brain::tools::subagent::SubAgentManager::new());
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::SpawnAgentTool::new(
            subagent_manager.clone(),
            tool_registry.clone(),
        ),
    ));
    tool_registry.register(Arc::new(crate::brain::tools::subagent::WaitAgentTool::new(
        subagent_manager.clone(),
    )));
    tool_registry.register(Arc::new(crate::brain::tools::subagent::SendInputTool::new(
        subagent_manager.clone(),
    )));
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::CloseAgentTool::new(subagent_manager.clone()),
    ));
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::ResumeAgentTool::new(
            subagent_manager.clone(),
            tool_registry.clone(),
        ),
    ));
    // Cross-session push (issue #1203): needs no manager — deliver_to_session
    // is a free function on the in-process session-route registry.
    // #129: interactive-only — parked notifications die with the process on
    // headless paths (owner ruling C: sub-agents report via the harness's
    // final-message relay, proven live-fire 2026-09-07). Gated with
    // suggest_options above; the context guard backstops both.
    // #1840 (owner order): also gated on the kill switch. A disabled tool
    // must not exist in the model's surface at all: no schema in context,
    // nothing to call, nothing to be confused by. The #1803 execute-time
    // refusal stays as the backstop for the non-registry paths (A2A, CLI).
    if !headless && config.agent.session_notify_enabled {
        tool_registry.register(Arc::new(crate::brain::tools::subagent::SessionNotifyTool));
    }

    // Phase 6: Team orchestration
    let team_manager = Arc::new(crate::brain::tools::subagent::TeamManager::new());
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::TeamCreateTool::new(
            subagent_manager.clone(),
            team_manager.clone(),
            tool_registry.clone(),
        ),
    ));
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::TeamDeleteTool::new(
            subagent_manager.clone(),
            team_manager.clone(),
        ),
    ));
    tool_registry.register(Arc::new(
        crate::brain::tools::subagent::TeamBroadcastTool::new(
            subagent_manager.clone(),
            team_manager.clone(),
        ),
    ));
    tracing::info!("Registered 9 sub-agent + team orchestration tools");

    // Recursive Self-Improvement tools
    tool_registry.register(Arc::new(
        crate::brain::tools::feedback_record::FeedbackRecordTool,
    ));
    tool_registry.register(Arc::new(
        crate::brain::tools::feedback_analyze::FeedbackAnalyzeTool,
    ));
    tool_registry.register(Arc::new(crate::brain::tools::self_improve::SelfImproveTool));
    tracing::info!("Registered 3 recursive self-improvement tools");

    subagent_manager
}

/// ACP tool-surface assembly: the registry for `opencrabs acp`.
///
/// ACP clients (the Mi 8 phone app, Zed, monocle) are LIVE user surfaces, so
/// the base assembly runs interactive (`headless = false`): `suggest_options`
/// is registered and the service keeps the FOLLOW-UP SUGGESTIONS prompt
/// paragraph that tells the model to call it. The client renders the options
/// as tappable chips from the `tool_call` frame's `rawInput`; a tap starts a
/// new `session/prompt`, so the tool's `halts_turn` settle (turn ends after a
/// successful call, buttons stay live) maps cleanly onto the protocol.
///
/// `session_notify` is dropped afterwards: an ACP server has no turn-end
/// relay to flush a parked notification (the event has no ACP vocabulary,
/// `acp/turn.rs`), so registering it would advertise a tool that can never
/// deliver. On genuinely interactive surfaces it stays reachable behind its
/// own config gate (`[agent] session_notify_enabled`, default off).
///
/// Callers must still build the `AgentService` WITHOUT
/// `with_headless(true)` — the flag would strip the prompt paragraph and
/// hard-error the tool at the loop backstop, undoing this assembly.
pub(crate) fn register_acp_agent_tools(
    tool_registry: &Arc<ToolRegistry>,
    db: &Database,
    config: &Config,
) -> Arc<crate::brain::tools::subagent::SubAgentManager> {
    let subagent_manager = register_core_agent_tools(tool_registry, db, config, false);
    tool_registry.unregister("session_notify");
    subagent_manager
}

/// Register the headless-safe RUNTIME tools that depend only on config + the
/// registry itself (never on live TUI or channel state): user-defined dynamic
/// tools from `tools.toml`, the dynamic-tool manager, and browser automation.
///
/// Every entry point that runs an agent calls this on top of
/// [`register_core_agent_tools`] so the tool set is identical across the TUI,
/// the multi-profile cron daemon, and the one-shot/headless CLI paths. Without
/// it, a cron job under a secondary profile (or a `run`/agent CLI invocation)
/// could not use the user's own `tools.toml` tools or the browser.
///
/// Deliberately EXCLUDED here (and registered only by the interactive path):
/// - **Channel send/connect tools** (`telegram_send`, `*_connect`, ...): they
///   hold live in-process channel state (`TelegramState` etc.) that only exists
///   once a bot is connected, which the daemon never does. Registering them
///   would yield dead tools that always answer "not connected". Cron jobs reach
///   channels through the job's config-based `deliver_to` path instead, which
///   builds its own client from the API key and needs no live state.
/// - **evolve**: binary self-update is owned by the primary daemon;
///   letting concurrent secondary profiles self-update would race on one binary.
pub(crate) fn register_runtime_tools(tool_registry: &Arc<ToolRegistry>, config: &Config) {
    // User-defined dynamic tools from the profile-aware ~/.opencrabs/tools.toml.
    let tools_toml_path = crate::brain::tools::dynamic::DynamicToolLoader::default_path()
        .unwrap_or_else(|| std::path::PathBuf::from("tools.toml"));
    let dynamic_count =
        crate::brain::tools::dynamic::DynamicToolLoader::load(&tools_toml_path, tool_registry);
    if dynamic_count > 0 {
        tracing::info!("Loaded {dynamic_count} dynamic tool(s) from tools.toml");
    }
    // tool_manage — agent can add/remove/reload dynamic tools at runtime.
    tool_registry.register(Arc::new(
        crate::brain::tools::tool_manage::ToolManageTool::new(
            tool_registry.clone(),
            tools_toml_path,
        ),
    ));

    // Browser automation (headless Chrome via CDP). Config-only construction and
    // lazy launch, so it is safe to register everywhere — chrome is spawned on
    // first use, not here.
    #[cfg(feature = "browser")]
    {
        let browser_manager = Arc::new(crate::brain::tools::browser::BrowserManager::new(
            config.browser.clone(),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserNavigateTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserScreenshotTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserClickTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserTypeTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserEvalTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserContentTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserWaitTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserFindTool::new(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(crate::brain::tools::browser::BrowserActTool::new(
            browser_manager.clone(),
        )));
        // web_scrape escalates JS-only pages to a headless render, so it takes a
        // clone of the browser manager here (kept out of CORE_TOOLS, surfaced via
        // tool_search). Registered before BrowserCloseTool consumes the manager.
        tool_registry.register(Arc::new(
            crate::brain::tools::web_scrape::WebScrapeTool::default()
                .with_browser(browser_manager.clone()),
        ));
        tool_registry.register(Arc::new(
            crate::brain::tools::browser::BrowserCloseTool::new(browser_manager),
        ));
        tracing::info!("Browser automation tools registered (10 tools)");
    }
    #[cfg(not(feature = "browser"))]
    {
        let _ = config;
        // No browser to escalate JS shells; web_scrape still serves static pages.
        tool_registry.register(Arc::new(
            crate::brain::tools::web_scrape::WebScrapeTool::default(),
        ));
    }
}

/// Headless-only preamble (#129, owner-ruled design): appended to the system
/// brain on headless paths ONLY (CLI one-shot `run`, cron daemon execute,
/// sub-agent spawn). The user cannot see a headless session's intermediate
/// messages — only the final one — so the agent must make that final message
/// self-contained instead of narrating progress nobody receives.
///
/// Deliberately NOT in AGENTS.md: that file is always-loaded on every surface
/// (TUI, channels), where the directive is wrong — there, intermediate
/// messages ARE visible and narrating progress is correct behaviour. A
/// brain-only rule cannot be headless-scoped; this preamble can.
pub const HEADLESS_PREAMBLE: &str = "\n\n## Headless session\n\nYou are running HEADLESS: the user cannot see your intermediate messages, tool calls, or progress updates — they receive ONLY your final message. That final message must be SELF-CONTAINED: state what was done with the concrete specifics (commit shas, file paths, test results, numbers), anything that failed and why, and what is needed next. Do not ask questions mid-task (no answer channel exists); make the reasonable assumption, state it, finish. Do not use `session_notify` or `suggest_options` — neither can reach a user from here. If you cannot complete the task, your final message is the only report: say what blocked you with the evidence.\n";
