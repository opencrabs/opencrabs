//! Brain Module
//!
//! The core intelligence layer — LLM providers, agent services, tools, tokenizer,
//! dynamic system prompt assembly, user-defined slash commands, and self-update.

pub mod agent;
pub mod bash_failure;
pub mod brain_sections;
pub mod commands;
pub mod detached;
pub mod directives;
pub mod feedback_policy;
pub mod filter;
pub mod goal;
pub mod hints;
pub mod memory_recall;
pub mod mission_control;
pub mod plans;
pub mod prompt_builder;
pub mod provider;
pub mod provider_spec;
pub mod rsi;
pub mod rsi_command_patterns;
pub mod rsi_disposition;
pub mod rsi_git_history;
pub mod rsi_proposals;
pub mod rsi_pruned;
pub mod rsi_skill_sequences;
pub mod rsi_stale_ledger;
pub mod rsi_stale_scan;
pub mod rsi_subsystem;
pub mod rsi_sync;
pub mod section_rank;
pub mod self_update;
pub mod skills;
pub mod timezone;
pub mod tokenizer;
pub mod toml_merge;
pub mod tools;

// Brain re-exports
pub use commands::{CommandLoader, UserCommand};
pub use prompt_builder::BrainLoader;
pub use self_update::SelfUpdater;

// LLM re-exports
pub use agent::{AgentContext, AgentError, AgentService};
pub use provider::{
    AnthropicProvider, ContentBlock, LLMRequest, LLMResponse, Message, Provider, ProviderError,
    ProviderStream, Role, StopReason, StreamEvent, TokenUsage, Tool,
};
pub use tools::{ToolError, ToolRegistry, ToolResult};
