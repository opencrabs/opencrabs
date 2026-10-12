//! LLM Provider Abstraction Layer
//!
//! Provides a unified interface for interacting with different LLM providers.
//! This file is declarations only — no function definitions live here
//! (CONTRIBUTING.md).

pub mod cooldown;
pub mod error;
pub mod json_repair;
pub mod placeholder;
pub mod rate_limiter;
#[allow(clippy::module_inception)]
pub(crate) mod r#trait;
pub mod types;
mod which;

// Re-exports
pub use cooldown::CooldownRegistry;
pub use error::{ProviderError, Result};
pub use placeholder::PlaceholderProvider;
pub use r#trait::{Provider, ProviderCapabilities, ProviderStream};
pub use types::*;

// Provider implementations
pub mod anthropic;
pub(crate) mod bare_tool_call_extractor;
pub(crate) mod chain_order;
pub mod claude_cli;
pub mod codex_cli;
pub mod codex_oauth;
pub mod command_code_cli;
pub mod copilot;
pub mod custom_openai_compatible;
pub mod deepseek_reasoning;
pub mod factory;
pub mod fallback;
pub mod gemini;
pub mod glm_reasoning;
pub mod identity;
pub mod kimi_plan;
pub mod kimi_reasoning;
pub mod model_fetch;
pub(crate) mod nonstream_compat;
pub mod opencode_cli;
pub mod qwen;
pub mod qwen_reasoning;
pub mod zhipu_endpoint;

pub use anthropic::AnthropicProvider;
pub use claude_cli::ClaudeCliProvider;
pub use codex_cli::CodexCliProvider;
pub use codex_oauth::CodexOAuthProvider;
pub use command_code_cli::CommandCodeCliProvider;
pub use custom_openai_compatible::OpenAIProvider;
pub use factory::{create_provider, create_provider_by_name, create_provider_with_warning};
pub use fallback::{FallbackProvider, SwapEvent};
pub use gemini::GeminiProvider;
pub use opencode_cli::OpenCodeCliProvider;
pub(crate) use which::which_binary;
