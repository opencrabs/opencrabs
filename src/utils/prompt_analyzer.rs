//! Prompt Analysis and Transformation (shared soft-nudge)
//!
//! Analyzes natural-language user prompts to detect keywords and appends
//! explicit tool call hints for the LLM to ensure proper tool usage.
//!
//! Shared by every surface (TUI and Telegram). Hints are LLM-only: they are
//! appended to the agent input string and must never reach user-visible
//! display paths (Telegram `display_text`, TUI chat bubbles). Slash commands
//! and skill/user-command expansions are never analyzed: soft-nudge means
//! the USER said keyword-shaped language, not that a skill author wrote it.
//!
//! ## Multilanguage support
//!
//! Keywords are loaded from per-language TOML files at compile time via
//! `include_str!`. Which languages exist, and the character-set heuristic
//! that picks one, are owned by `phantom_lang`
//! ([`crate::brain::agent::service::phantom_lang::Lang`]); this module
//! resolves that language to its analyzer pack through an exhaustive match,
//! so a language added there does not compile until its TOML is registered
//! here too (#2226).
//!
//! Multi-word phrases are scanned across all languages to catch code-switched
//! input (e.g. "Voy a usar write_file" with no ñ/¿). Single-word keywords
//! are gated to the detected language to avoid false positives.

use crate::brain::agent::service::phantom_lang::{self, Lang};
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

/// Language-specific prompt analyzer configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyzerLang {
    #[serde(default)]
    pub plan: Vec<String>,
    #[serde(default)]
    pub read_file: Vec<String>,
    #[serde(default)]
    pub search: Vec<String>,
    #[serde(default)]
    pub write_file: Vec<String>,
    #[serde(default)]
    pub edit_file: Vec<String>,
    #[serde(default)]
    pub bash: Vec<String>,
    #[serde(default)]
    pub web_search: Vec<String>,
    /// Phrases that mean "stop what you are doing right now" (#965).
    ///
    /// Read by [`crate::utils::stop_intent`], which scans every language via
    /// [`all_packs`] rather than guessing one. Single-word entries only match
    /// a whole message; multi-word entries may also match a leading clause.
    /// See that module for why the two behave differently.
    #[serde(default)]
    pub stop_intent: Vec<String>,
    /// Address terms stripped from the END of a message before matching
    /// [`Self::stop_intent`] (#965), so "stop crab" and "hold on crabs" reduce
    /// to a bare interrupt. Only ever stripped as a trailing run, never from
    /// the middle, so "stop the bot container" keeps its object.
    #[serde(default)]
    pub stop_address: Vec<String>,
}

/// Embedded TOML content (compile-time validated).
const EN_TOML: &str = include_str!("prompt_analyzer_lang/en.toml");
const RU_TOML: &str = include_str!("prompt_analyzer_lang/ru.toml");
const AR_TOML: &str = include_str!("prompt_analyzer_lang/ar.toml");
const ZH_TOML: &str = include_str!("prompt_analyzer_lang/zh.toml");
const ES_TOML: &str = include_str!("prompt_analyzer_lang/es.toml");
const PT_TOML: &str = include_str!("prompt_analyzer_lang/pt.toml");
const FR_TOML: &str = include_str!("prompt_analyzer_lang/fr.toml");
const ID_TOML: &str = include_str!("prompt_analyzer_lang/id.toml");

pub(crate) static LANG_EN: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(EN_TOML).expect("BUG: en.toml failed to parse at runtime"));
pub(crate) static LANG_RU: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(RU_TOML).expect("BUG: ru.toml failed to parse at runtime"));
pub(crate) static LANG_AR: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(AR_TOML).expect("BUG: ar.toml failed to parse at runtime"));
pub(crate) static LANG_ZH: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(ZH_TOML).expect("BUG: zh.toml failed to parse at runtime"));
pub(crate) static LANG_ES: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(ES_TOML).expect("BUG: es.toml failed to parse at runtime"));
pub(crate) static LANG_PT: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(PT_TOML).expect("BUG: pt.toml failed to parse at runtime"));
pub(crate) static LANG_FR: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(FR_TOML).expect("BUG: fr.toml failed to parse at runtime"));
pub(crate) static LANG_ID: LazyLock<AnalyzerLang> =
    LazyLock::new(|| toml::from_str(ID_TOML).expect("BUG: id.toml failed to parse at runtime"));

/// The prompt-analyzer pack for one language.
///
/// The language set is owned by
/// [`phantom_lang`](crate::brain::agent::service::phantom_lang): this match is
/// exhaustive on its [`Lang`], so a language registered there does not
/// compile until its analyzer TOML is wired here too (#2226).
pub(crate) fn analyzer_pack(lang: Lang) -> &'static AnalyzerLang {
    match lang {
        Lang::En => &LANG_EN,
        Lang::Ru => &LANG_RU,
        Lang::Ar => &LANG_AR,
        Lang::Zh => &LANG_ZH,
        Lang::Es => &LANG_ES,
        Lang::Pt => &LANG_PT,
        Lang::Fr => &LANG_FR,
        Lang::Id => &LANG_ID,
    }
}

/// Every analyzer pack, in the registry's detection-priority order.
///
/// Multi-word phrases are scanned across all languages to catch code-switched
/// input. Single-word keywords stay gated to the detected language to avoid
/// false positives.
pub(crate) fn all_packs() -> [&'static AnalyzerLang; 8] {
    Lang::ALL.map(analyzer_pack)
}

/// Detect language from text content using the shared character-set
/// heuristics, and return its analyzer pack.
pub fn detect_language(text: &str) -> &'static AnalyzerLang {
    analyzer_pack(phantom_lang::detect_lang(text))
}

static SHARED: LazyLock<PromptAnalyzer> = LazyLock::new(PromptAnalyzer::new);

/// True when an utterance is natural-language chat the analyzer may inspect.
/// Slash commands and system triggers are never soft-nudged.
pub fn is_natural_chat(text: &str) -> bool {
    let t = text.trim_start();
    !t.starts_with('/') && !t.to_ascii_lowercase().starts_with("[system")
}

/// Prompt analyzer that detects keywords and suggests tool usage
pub struct PromptAnalyzer {
    /// Multi-word phrase regexes (scanned across all languages)
    plan_phrase: Regex,
    read_file_phrase: Regex,
    search_phrase: Regex,
    write_file_phrase: Regex,
    edit_file_phrase: Regex,
    bash_phrase: Regex,
    web_search_phrase: Regex,
}

impl PromptAnalyzer {
    /// Create a new prompt analyzer
    pub fn new() -> Self {
        // Collect all multi-word phrases across all languages
        let plan_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.plan
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let read_file_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.read_file
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let search_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.search
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let write_file_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.write_file
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let edit_file_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.edit_file
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let bash_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.bash
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();
        let web_search_phrases: Vec<&str> = all_packs()
            .iter()
            .flat_map(|lang| {
                lang.web_search
                    .iter()
                    .filter(|k| k.contains(' '))
                    .map(|s| s.as_str())
            })
            .collect();

        Self {
            plan_phrase: Self::build_keyword_regex(&plan_phrases),
            read_file_phrase: Self::build_keyword_regex(&read_file_phrases),
            search_phrase: Self::build_keyword_regex(&search_phrases),
            write_file_phrase: Self::build_keyword_regex(&write_file_phrases),
            edit_file_phrase: Self::build_keyword_regex(&edit_file_phrases),
            bash_phrase: Self::build_keyword_regex(&bash_phrases),
            web_search_phrase: Self::build_keyword_regex(&web_search_phrases),
        }
    }

    /// Process-wide shared instance, so the keyword regexes compile once.
    pub fn shared() -> &'static PromptAnalyzer {
        &SHARED
    }

    /// Build a regex from keywords (case-insensitive, word boundaries)
    fn build_keyword_regex(keywords: &[&str]) -> Regex {
        if keywords.is_empty() {
            // Return a regex that never matches
            return Regex::new(r"(?i)\b(NEVER_MATCH_THIS)\b").expect("Failed to compile regex");
        }
        let pattern = keywords
            .iter()
            .map(|k| regex::escape(k))
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&format!(r"(?i)\b({})\b", pattern)).expect("Failed to compile keyword regex")
    }

    /// Build a regex for a specific language's single-word keywords
    fn build_word_regex_for_lang(keywords: &[String]) -> Regex {
        let words: Vec<&str> = keywords
            .iter()
            .filter(|k| !k.contains(' '))
            .map(|s| s.as_str())
            .collect();
        Self::build_keyword_regex(&words)
    }

    /// Return the hint section for a prompt, or `None` when no keyword
    /// family matches. Callers append this to the LLM agent input only,
    /// never to user-visible display text.
    pub fn hints_for(&self, prompt: &str) -> Option<String> {
        let mut transformations = Vec::new();
        let lower_prompt = prompt.to_lowercase();

        // Detect language for single-word keyword gating
        let detected = phantom_lang::detect_lang(prompt);
        let lang = analyzer_pack(detected);
        let lang_name = detected.code();

        // Build language-specific word regexes on the fly (cheap, only when needed)
        let plan_word_lang = Self::build_word_regex_for_lang(&lang.plan);
        let read_file_word_lang = Self::build_word_regex_for_lang(&lang.read_file);
        let search_word_lang = Self::build_word_regex_for_lang(&lang.search);
        let write_file_word_lang = Self::build_word_regex_for_lang(&lang.write_file);
        let edit_file_word_lang = Self::build_word_regex_for_lang(&lang.edit_file);
        let bash_word_lang = Self::build_word_regex_for_lang(&lang.bash);
        let web_search_word_lang = Self::build_word_regex_for_lang(&lang.web_search);

        // Prompt snippet for logging (first 40 chars, respecting char boundaries)
        let snippet = if prompt.len() > 40 {
            let mut end = 40;
            while end > 0 && !prompt.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &prompt[..end])
        } else {
            prompt.to_string()
        };

        // Check for plan keywords: design track (the user said "plan", so
        // they get a reviewable SESSION PLAN and an Approve step, not a
        // checklist that starts executing on its own).
        // Multi-word phrases scanned across all languages, single-word gated to detected lang
        let plan_match = self.plan_phrase.captures(&lower_prompt).or_else(|| {
            if plan_word_lang.is_match(&lower_prompt) {
                plan_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = plan_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected PLAN intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations.push(
                "\n\n**CRITICAL**: The user wants a PLAN — enter the design track NOW:\n\
                1. Explore first if needed (reads, search, bash are available pre-init).\n\
                2. plan(operation='init', mode='design', title='...') creates the \
                SESSION PLAN .md and returns its absolute path.\n\
                3. Write the design INTO that .md (write_file/edit_file on that exact \
                path — the only writable file) using the template: ## Context with \
                **Problem:** / **Target state:** / **Intent:**, then numbered \
                ## Implementation steps.\n\
                4. STOP and wait for the user to APPROVE the plan (/execute). Do NOT \
                call start, do NOT edit project files, do NOT paste the plan in chat.\n\
                The checklist is seeded automatically after Approve. Valid operations \
                are EXACTLY: init, add_tasks, add_task, start, complete. There is NO \
                'create' and NO 'finalize' operation, never call those.",
            );
        }

        // Check for read_file keywords
        let read_file_match = self.read_file_phrase.captures(&lower_prompt).or_else(|| {
            if read_file_word_lang.is_match(&lower_prompt) {
                read_file_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = read_file_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected READ_FILE intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations
                .push("\n\n**TOOL HINT**: Use the `read_file` tool to read the contents of files.");
        }

        // Check for search keywords
        let search_match = self.search_phrase.captures(&lower_prompt).or_else(|| {
            if search_word_lang.is_match(&lower_prompt) {
                search_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = search_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected SEARCH/GREP intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations.push(
                "\n\n**TOOL HINT**: Use the `grep` tool to search for patterns in files, \
                or use `glob` to find files by pattern.",
            );
        }

        // Check for write_file keywords
        let write_file_match = self.write_file_phrase.captures(&lower_prompt).or_else(|| {
            if write_file_word_lang.is_match(&lower_prompt) {
                write_file_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = write_file_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected WRITE_FILE intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations.push(
                "\n\n**TOOL HINT**: Use the `write_file` tool to create new files. Keep each \
                 call under ~300 lines; split a larger file by concern (shell + js/ + css/) \
                 and add parts with `edit_file`.",
            );
        }

        // Check for edit_file keywords
        let edit_file_match = self.edit_file_phrase.captures(&lower_prompt).or_else(|| {
            if edit_file_word_lang.is_match(&lower_prompt) {
                edit_file_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = edit_file_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected EDIT_FILE intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations
                .push("\n\n**TOOL HINT**: Use the `edit_file` tool to modify existing files.");
        }

        // Check for bash keywords
        let bash_match = self.bash_phrase.captures(&lower_prompt).or_else(|| {
            if bash_word_lang.is_match(&lower_prompt) {
                bash_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = bash_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected BASH intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations
                .push("\n\n**TOOL HINT**: Use the `bash` tool to execute shell commands.");
        }

        // Check for web_search keywords
        let web_search_match = self.web_search_phrase.captures(&lower_prompt).or_else(|| {
            if web_search_word_lang.is_match(&lower_prompt) {
                web_search_word_lang.captures(&lower_prompt)
            } else {
                None
            }
        });
        if let Some(caps) = web_search_match {
            let matched = caps.get(1).map_or("unknown", |m| m.as_str());
            tracing::info!(
                "🔍 Detected WEB_SEARCH intent: keyword='{}', lang={}, prompt='{}'",
                matched,
                lang_name,
                snippet
            );
            transformations.push(
                "\n\n**TOOL HINT**: Use the `web_search` tool to search the internet for \
                real-time information.",
            );
        }

        if transformations.is_empty() {
            None
        } else {
            Some(transformations.join(""))
        }
    }

    /// Whether the prompt matches the plan keyword family. Callers use
    /// this to set the durable `pre_init_editing` flag when a plan-shaped
    /// message arrives on natural-language chat.
    pub fn plan_intent(&self, prompt: &str) -> bool {
        let lower_prompt = prompt.to_lowercase();
        let lang = detect_language(prompt);
        let plan_word_lang = Self::build_word_regex_for_lang(&lang.plan);
        self.plan_phrase.is_match(&lower_prompt) || plan_word_lang.is_match(&lower_prompt)
    }

    /// Analyze a prompt and transform it if needed
    pub fn analyze_and_transform(&self, prompt: &str) -> String {
        match self.hints_for(prompt) {
            Some(hint_section) => format!("{}{}", prompt, hint_section),
            None => prompt.to_string(),
        }
    }
}

impl Default for PromptAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}
