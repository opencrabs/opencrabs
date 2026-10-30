//! Tool Execution Framework
//!
//! Provides an abstraction for tools that can be called by LLM agents,
//! including file operations, shell commands, and more.

pub mod brain_file_safety;
pub mod brain_verify;
pub mod catalog;
pub(crate) mod classify;
pub(crate) mod confidential;
pub mod epistemic;
pub mod error;
pub mod registry;
pub mod scope;
pub(crate) mod ssrf;
pub(crate) mod r#trait;

pub mod fuzzy;
pub mod tool_name_heal;
// Tool implementations - Phase 1: Essential File Operations
pub mod bash;
pub mod edit;
pub mod file_versions;
pub mod fs_util;
pub mod glob;
pub mod grep;
pub mod hashline;
pub mod line_ops;
pub mod ls;
pub mod read;
pub mod read_state;
pub mod seen_skills;
pub mod write;

// Tool implementations - Phase 2: Advanced Features
pub mod brave_search;
pub mod code_exec;
pub mod doc_gen;
pub mod doc_parser;
pub mod exa_search;
pub mod notebook;
pub mod pdf_to_images;
pub mod serper_search;
pub(crate) mod sources_footer;
pub mod vba_modules;
pub mod web_scrape;
pub mod web_search;

// Tool implementations - Phase 3: Workflow & Integration
pub mod a2a_send;
pub mod analyze_image;
pub mod analyze_video;
pub mod mission_control_report;
pub mod profile_list;
pub mod tasks_list;

// Tool implementations - Recursive Self-Improvement
pub mod channel_search;
pub mod config_tool;
pub mod context;
pub mod cron_manage;
pub mod decide_cached;
pub mod evolve;
pub mod feedback_analyze;
pub mod feedback_record;
pub mod generate_image;
pub mod goal_manage;
pub mod http;
pub mod load_brain_file;
pub mod memory_search;
pub(crate) mod path_lock;
pub mod plan_gate;
pub mod plan_tool;
pub(crate) mod project_runner;
pub mod provider_vision;
pub mod rename_session;
pub mod rsi_proposals;
pub mod rsi_propose;
pub mod self_improve;
pub mod self_improve_guards;
pub mod session_search;
pub mod skill_gate;
pub mod slash_command;
pub mod slash_onboard;
pub mod suggest_options;
pub mod task;
pub mod tool_search;
pub mod write_opencrabs_file;

// Tool implementations - Phase 5: Multi-Agent Orchestration
pub mod subagent;

// Dynamic tools — runtime-defined via tools.toml
pub mod dynamic;
pub mod toml_hot_reload;
pub mod tool_manage;

// Browser automation — headless Chrome via CDP
#[cfg(feature = "browser")]
pub mod browser;

// Tool implementations - Phase 4: Channel Integrations
#[cfg(feature = "telegram")]
pub mod cowork_connect;
#[cfg(feature = "discord")]
pub mod discord_connect;
#[cfg(feature = "discord")]
pub mod discord_send;
#[cfg(feature = "slack")]
pub mod slack_connect;
#[cfg(feature = "slack")]
pub mod slack_send;
#[cfg(feature = "telegram")]
pub mod telegram_connect;
#[cfg(feature = "telegram")]
pub mod telegram_send;
#[cfg(feature = "trello")]
pub mod trello_connect;
#[cfg(feature = "trello")]
pub mod trello_send;
#[cfg(feature = "whatsapp")]
pub mod whatsapp_connect;
#[cfg(feature = "whatsapp")]
pub mod whatsapp_history;
#[cfg(feature = "whatsapp")]
pub mod whatsapp_send;

// Re-exports
pub use error::{Result, ToolError};
pub use registry::ToolRegistry;
pub use r#trait::{OriginTarget, Tool, ToolCapability, ToolExecutionContext, ToolResult};
