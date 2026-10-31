//! # OpenCrabs
//!
//! The autonomous, self-improving AI agent. Single Rust binary. Every channel.
//!
//! OpenCrabs is a high-performance AI orchestration agent with a modern TUI,
//! multi-channel messaging (Telegram, Discord, Slack, WhatsApp), and an
//! extensible tool system — all in one statically-linked binary.
//!
//! ## Providers
//!
//! - **Anthropic** — Claude models (Sonnet, Opus, Haiku)
//! - **OpenAI** — GPT-4, GPT-5, o-series models
//! - **Google Gemini** — Gemini 2.x family
//! - **OpenRouter** — 300+ models via single API key
//! - **MiniMax** — MiniMax-M3, MiniMax-M2.7, MiniMax-M2.5, Kimi, and Text models
//! - **Custom** — any OpenAI-compatible endpoint (Ollama, LM Studio, vLLM, etc.)
//!
//! ## Features
//!
//! - **Modern TUI** — Ratatui-based terminal interface with streaming, markdown, and syntax highlighting
//! - **Multi-Channel** — Telegram, Discord, Slack, and WhatsApp gateways with shared sessions
//! - **Tool System** — 30+ built-in tools (shell, file I/O, web search, code execution, etc.)
//! - **A2A Protocol** — Agent-to-Agent communication for multi-agent workflows
//! - **Local-First** — SQLite storage, config hot-reload, no cloud dependency
//! - **Session Management** — persistent chat sessions with token/cost tracking
//! - **Cron Scheduler** — scheduled agent tasks with natural-language definitions
//!
//! ## Quick Start
//!
//! ```bash
//! # Interactive TUI mode
//! opencrabs
//!
//! # Non-interactive (pipe-friendly)
//! opencrabs run "explain this code"
//!
//! # With auto-approve
//! opencrabs run --auto-approve "refactor this file"
//! ```
//!
//! ## Stability
//!
//! **The command-line interface is the stable surface. The library is not.**
//!
//! This crate is published so the binary can be installed with `cargo install
//! opencrabs`. The modules below are its internals: they are `pub` because the
//! binary and the integration tests need them across module boundaries, not
//! because they are an API anyone should build against.
//!
//! Under semantic versioning that distinction has to be stated rather than
//! assumed, so: the CLI commands, their flags, the config file format and the
//! on-disk layout of `~/.opencrabs/` are covered by the version number. Every
//! Rust item in this crate may be renamed, moved, or removed in any release,
//! including a patch.
//!
//! If you need a stable Rust API against OpenCrabs, open an issue describing
//! what you are building. Carving a supported surface out of these internals is
//! a decision worth making deliberately, and it has not been made.
//!
//! ## Architecture
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`brain`] | LLM providers, tool registry, agent service |
//! | [`channels`] | Telegram, Discord, Slack, WhatsApp handlers |
//! | [`tui`] | Terminal UI (Ratatui + crossterm) |
//! | [`db`] | SQLite persistence (deadpool-sqlite) |
//! | [`config`] | TOML config with hot-reload and key separation |
//! | [`a2a`] | Agent-to-Agent protocol server |
//! | [`cron`] | Scheduled task execution |
//! | [`services`] | Session, message, and file services |

pub mod app;
pub mod brain;
pub mod cli;
pub mod config;
pub mod db;
pub mod decisions;
pub mod error;
pub mod logging;
pub mod memory;
pub mod runtime;
pub mod services;
pub mod tui;
pub mod utils;

pub mod a2a;
pub mod acp;
pub mod channels;
pub mod cron;
pub mod remote;
pub mod rtk;
pub mod ui_core;
pub mod usage;

/// Offline evaluation harness for context engineering and memory quality.
/// Compiled only under test or the `eval` feature so it never ships in a
/// normal release binary.
#[cfg(any(test, feature = "eval"))]
pub mod eval;

// Re-export commonly used types
pub use error::{ErrorCode, OpenCrabsError};

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Package authors
pub const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");
/// Package description
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[cfg(test)]
mod tests;
