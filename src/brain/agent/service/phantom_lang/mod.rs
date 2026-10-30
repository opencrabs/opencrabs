//! Per-language phantom-detection data loaded from TOML at compile time.
//!
//! Each `.toml` file defines the phrases, verbs, and regex patterns
//! for one language. The loader embeds them into the binary via
//! `include_str!` so any TOML syntax error fails the build.
//!
//! [`config`] holds the struct, the embedded tables, and the [`Lang`]
//! registry that every pack list resolves from; [`detect`] holds the
//! character-set heuristic that picks one. This file is declarations only:
//! no function definitions live here (CONTRIBUTING.md).

pub(crate) mod config;
mod detect;

pub use config::{Lang, LangConfig, all_langs};
pub use detect::{detect_lang, detect_language};
