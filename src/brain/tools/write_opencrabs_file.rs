//! Write OpenCrabs File Tool
//!
//! Writes or edits any file within `~/.opencrabs/` (brain files like
//! MEMORY.md, USER.md, config files like commands.toml, memory logs, and
//! any other app-owned files). The standard `edit_file`/`write_file`
//! tools refuse to touch protected brain files (issue #91 guardrail)
//! and route the caller here instead. This tool enforces append-only
//! writes, dedup-aware shrinking, and saves a `.bak` snapshot before
//! every change.

use super::brain_file_safety;
use super::brain_verify;
use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

pub struct WriteOpenCrabsFileTool;

/// True when `home` is itself a named-profile directory, i.e. `<base>/profiles/<name>`.
///
/// In that shape a leading `profiles/` segment in the caller-supplied path
/// duplicates the home path; in the default-profile shape (`<base>`) the same
/// spelling is a *sibling* profile's real home and must be accepted.
pub(crate) fn home_is_named_profile(home: &std::path::Path) -> bool {
    home.parent()
        .and_then(std::path::Path::file_name)
        .is_some_and(|name| name == std::ffi::OsStr::new("profiles"))
}

/// True when the first non-`CurDir` component of `path` is the literal
/// `profiles` segment.
///
/// Component-based rather than a string prefix, so `./profiles/...` gets the
/// same verdict as `profiles/...` — the string test this replaces was
/// bypassable with a leading `./`.
pub(crate) fn leading_profiles_segment(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .find(|component| !matches!(component, std::path::Component::CurDir))
        .is_some_and(|component| {
            component == std::path::Component::Normal(std::ffi::OsStr::new("profiles"))
        })
}

/// Validate that `path` is a safe relative path within the active OpenCrabs home.
/// Prevents path traversal outside the app home directory.
///
/// `home` is the resolved profile home (`crate::config::opencrabs_home()`), which
/// is either `<base>` (default profile) or `<base>/profiles/<name>` (named
/// profile). The leading-`profiles/` check needs it: the two shapes share the
/// same spelling but demand opposite verdicts.
pub(crate) fn validate_opencrabs_path(
    home: &std::path::Path,
    path: &str,
) -> std::result::Result<(), String> {
    if path.is_empty() {
        return Err("path is required".into());
    }
    // Reject absolute paths — must be relative to the OpenCrabs home
    if path.starts_with('/') || path.starts_with('~') {
        return Err(format!(
            "Use a relative path (e.g. \"MEMORY.md\" or \"memory/2026-03-02.md\"), \
             not an absolute path '{}'",
            path
        ));
    }
    // Reject traversal attempts
    if path.contains("..") {
        return Err(format!(
            "'{}' contains '..' — path traversal is not allowed",
            path
        ));
    }
    // Reject null bytes
    if path.contains('\0') {
        return Err("path contains null bytes".into());
    }
    // Reject a leading `profiles/` segment only when the active home is itself a
    // named profile (`<base>/profiles/<name>`). There the prefix duplicates the
    // home path and the write lands one level too deep. For the default profile
    // (`<base>`) `profiles/family/USER.md` is that sibling's real home and is
    // accepted. Component-based, so `./profiles/...` cannot bypass the gate.
    if home_is_named_profile(home) && leading_profiles_segment(path) {
        return Err(format!(
            "Path '{}' starts with a 'profiles/' segment, but this profile's home already lies \
             under profiles/ ({}), so the prefix duplicates it. This tool expects a relative \
             path from your home directory. For example: pass \"TOOLS.md\" not \
             \"profiles/ops/TOOLS.md\", or \"memory/note.md\" not \
             \"profiles/ops/memory/note.md\".",
            path,
            home.display()
        ));
    }
    Ok(())
}

/// Post-write verification: run brain_verify checks on the file content.
/// If violations found, restores from backup and returns an error message.
/// Returns `Ok(())` if verification passes, `Err(message)` if rolled back.
fn verify_or_rollback(
    full_path: &std::path::Path,
    content: &str,
    backup_path: &Option<std::path::PathBuf>,
) -> std::result::Result<(), String> {
    use std::io::Write;

    let file_name = match full_path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return Ok(()), // Can't determine filename, skip verification
    };

    let violations = brain_verify::verify_brain_file(file_name, content);
    if violations.is_empty() {
        crate::db::repository::AnalyticsEventRepository::emit_brain_verify(file_name, "pass", None);
        return Ok(());
    }

    // Rollback from backup
    if let Some(bak) = backup_path
        && let Ok(original) = std::fs::read_to_string(bak)
    {
        match std::fs::File::create(full_path).and_then(|mut f| f.write_all(original.as_bytes())) {
            Ok(()) => tracing::warn!(
                "write_opencrabs_file: verification failed, rolled back {}: {}",
                file_name,
                violations.join("; ")
            ),
            // Never silent: the rollback is the only thing standing between a
            // rejected write and the file keeping it.
            Err(e) => tracing::error!(
                "write_opencrabs_file: verification failed for {} AND rollback failed ({e}) \
                 — the rejected content is still on disk: {}",
                file_name,
                violations.join("; ")
            ),
        }
    }

    let joined = violations.join("; ");
    crate::db::repository::AnalyticsEventRepository::emit_brain_verify(
        file_name,
        "rollback",
        Some(&joined),
    );
    Err(format!(
        "Brain file verification failed for {}: {}. Write rolled back.",
        file_name, joined
    ))
}

/// Derive the epistemic belief `(key, value)` for a single MEMORY.md line.
/// Returns `None` for lines that shouldn't be tracked (blanks, markdown
/// headers, separators, short noise). Pure — no store access — so the
/// key-derivation logic is unit-testable without touching disk.
pub(crate) fn memory_belief_key_value(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty()
        || line.starts_with('#')
        || line.starts_with("---")
        || line.starts_with('*')
        || line.chars().count() < 12
    {
        return None;
    }
    // A belief's identity is its normalized topic (first few words) so a
    // rewrite of the same rule collides on the same key and the engine can
    // detect the value change as a contradiction.
    let topic: String = line
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    if topic.is_empty() {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    topic.hash(&mut hasher);
    Some((format!("memory:{:016x}", hasher.finish()), line.to_string()))
}

/// Epistemic engine integration (#862): when MEMORY.md is written, register
/// each meaningful line as a belief so the engine can decay stale memories
/// and flag contradictions when a rule about the same topic is rewritten.
/// Only MEMORY.md is tracked — SOUL/AGENTS/CODE/etc. are identity and config,
/// not beliefs. Contradictions are logged, never blocking.
fn track_memory_belief(path_str: &str, written: &str) {
    let file_name = std::path::Path::new(path_str)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if file_name != "MEMORY.md" {
        return;
    }
    for line in written.lines() {
        let Some((key, value)) = memory_belief_key_value(line) else {
            continue;
        };
        let result = super::epistemic::add_belief(
            &key,
            &value,
            super::epistemic::Confidence::Inferred,
            "write_opencrabs_file:MEMORY.md",
        );
        if let super::epistemic::ContradictionResult::Contradicted {
            old_value,
            new_value,
        } = result
        {
            tracing::warn!(
                "write_opencrabs_file: MEMORY.md belief contradicted — '{old_value}' → '{new_value}'"
            );
        }
    }
}

#[async_trait]
impl Tool for WriteOpenCrabsFileTool {
    fn name(&self) -> &str {
        "write_opencrabs_file"
    }

    /// The one path this invocation writes (#593). Unlike the generic
    /// `path`-keyed tools, this one addresses files under the profile home,
    /// so it resolves exactly as `execute` does — `home.join(path)` — rather
    /// than against the working directory. Two calls naming one brain file
    /// must compare equal here, or the batch would run them concurrently.
    fn write_target(
        &self,
        input: &Value,
        _working_directory: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        input
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| crate::config::opencrabs_home().join(p))
    }

    fn description(&self) -> &str {
        "Write or edit any file within the OpenCrabs home directory. \
         Use this for brain files (MEMORY.md, USER.md, AGENTS.md, SOUL.md, etc.), \
         config files (commands.toml), memory logs, and any other app files. \
         The standard edit_file/write_file tools cannot reach the home directory — use this instead. \
         \
         **Path rules:** \
         - Pass a relative path from your home directory (e.g. \"MEMORY.md\", \"memory/note.md\", \"rsi/improvements.md\"). \
         - No leading slash, no '..' in paths. \
         - Do NOT include any directory prefix that duplicates your home path. \
         \
         Supports three operations: \
         \"overwrite\" replaces entire file content, \
         \"append\" adds text to the end, \
         \"replace\" does a find-and-replace within the file. \
         \
         Protected brain files are append-only by default. To shrink/clean up a brain file \
         (remove outdated content), set cleanup_intent=true — this requires explicit user \
         approval and is NOT available in autonomous RSI operations. \
         \
         **Check before you append a rule or lesson.** Search first, then decide: \
         `memory_search` with `scope=\"brain\"` — it ranks across every brain file, so it \
         finds the rule wherever it lives, and costs a fraction of reading one. If it \
         hits, read the full section with `load_brain_file` + `query` before deciding \
         whether to sharpen it. Do NOT search the default \"memory\" scope for this: \
         daily notes outnumber brain files and will bury the rule. Do NOT reach for \
         `grep` either — it resolves against the working directory and cannot see your \
         home directory at all. Per-turn recall does NOT cover this: it surfaces a slice of \
         MEMORY.md chosen for the USER's message, not for the rule you are about to \
         write, so finding nothing similar in context is not evidence there is nothing. \
         Three outcomes: nothing similar exists, append it; a similar rule exists, \
         REPLACE that line in place so it is sharpened rather than repeated; it is \
         already covered, write nothing. Restating a rule in different words does not \
         reinforce it, it splits it — the next reader finds two half-rules and cannot \
         tell which is current."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path from your home directory (e.g. \"MEMORY.md\", \"memory/2026-03-02.md\", \"rsi/improvements.md\", \"commands.toml\"). No leading slash, no '..'. Do not include any prefix that duplicates your home path."
                },
                "operation": {
                    "type": "string",
                    "enum": ["overwrite", "append", "replace"],
                    "description": "\"overwrite\": replace entire file. \"append\": add to end. \"replace\": find old_text and replace with new_text."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write (required for overwrite and append)."
                },
                "old_text": {
                    "type": "string",
                    "description": "Text to find (required for replace)."
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text (required for replace)."
                },
                "cleanup_intent": {
                    "type": "boolean",
                    "description": "Set to true ONLY when you need to intentionally clean up a protected brain file (remove outdated content, consolidate sections, etc.). This bypasses the append-only restriction and allows shrinking. Requires explicit user approval (this tool has requires_approval: true). This parameter is NOT available in the autonomous RSI self_improve tool — only user-initiated operations can clean up brain files."
                }
            },
            "required": ["path", "operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WriteFiles]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, _ctx: &ToolExecutionContext) -> Result<ToolResult> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let home = crate::config::opencrabs_home();

        if let Err(e) = validate_opencrabs_path(&home, path_str) {
            return Ok(ToolResult::error(e));
        }

        let operation = input
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let full_path = home.join(path_str);

        match operation {
            "overwrite" => {
                let content = match input.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return Ok(ToolResult::error(
                            "content is required for overwrite".into(),
                        ));
                    }
                };
                // #2099: a config.toml or keys.toml that would not parse must
                // never land. The loader falls back to auto-repair or the
                // last-good snapshot, and if both miss the process does not
                // start. The capability is unchanged: any config the user asks
                // for still writes, only content that would not load is
                // refused, and the previous bytes stay on disk.
                if let Err(msg) = crate::config::guard::deny_if_would_break(&full_path, content) {
                    return Ok(ToolResult::error(msg));
                }
                // Append-only contract: overwriting a protected brain file
                // is only allowed when it doesn't lose bytes. The 2026-04-26
                // RSI rewrite of TOOLS.md happened by passing the whole file
                // as `old_content` to a `replace` — but `overwrite` is the
                // even more direct path to the same damage.
                use crate::brain::tools::brain_file_safety;
                if brain_file_safety::is_protected_path(&full_path) {
                    let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
                    let cleanup_intent = input
                        .get("cleanup_intent")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if let brain_file_safety::ShrinkCheck::Rejected { message } =
                        brain_file_safety::check_no_shrink(
                            &full_path,
                            &existing,
                            content,
                            cleanup_intent,
                            false, // consolidation: only self_improve's surgical update qualifies
                        )
                    {
                        return Ok(ToolResult::error(message));
                    }
                    // Record pruned sections when shrinking is allowed (cleanup)
                    if cleanup_intent {
                        let removed =
                            crate::brain::rsi_pruned::detect_removed_sections(&existing, content);
                        if !removed.is_empty() {
                            let mut pruned_state = crate::brain::rsi_pruned::PrunedState::load();
                            pruned_state.record_pruned(path_str, removed);
                            if let Err(e) = pruned_state.save() {
                                tracing::warn!(
                                    "write_opencrabs_file (create-update path): recorded {} pruned header(s) for {} \
                                     but pruned.toml save failed: {} — sync_templates() will re-add those sections \
                                     on the next sync until this is fixed",
                                    pruned_state
                                        .pruned
                                        .get(path_str)
                                        .map(|h| h.len())
                                        .unwrap_or(0),
                                    path_str,
                                    e
                                );
                            }
                        }
                    }
                }
                if let Some(parent) = full_path.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    return Ok(ToolResult::error(format!(
                        "Failed to create directory: {}",
                        e
                    )));
                }
                let backup_path = brain_file_safety::backup_before_write(&full_path)
                    .ok()
                    .flatten();
                match std::fs::write(&full_path, content) {
                    Ok(()) => {
                        if let Err(msg) = verify_or_rollback(&full_path, content, &backup_path) {
                            return Ok(ToolResult::error(msg));
                        }
                        // Epistemic engine (#862): track MEMORY.md facts as beliefs.
                        track_memory_belief(path_str, content);
                        Ok(ToolResult::success(format!(
                            "Wrote {} bytes to {}",
                            content.len(),
                            full_path.display()
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to write {}: {}",
                        path_str, e
                    ))),
                }
            }

            "append" => {
                let content = match input.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return Ok(ToolResult::error("content is required for append".into())),
                };
                use crate::brain::tools::brain_file_safety::{
                    self, AppendDedup, filter_duplicate_append,
                };
                // Dedup check for protected brain files: extract only genuinely
                // new paragraphs instead of blindly appending everything.
                let effective_content = if brain_file_safety::is_protected_path(&full_path) {
                    let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
                    match filter_duplicate_append(&existing, content) {
                        AppendDedup::AllNew => content.to_string(),
                        AppendDedup::Filtered {
                            filtered_content,
                            skipped_paragraphs,
                        } => {
                            tracing::info!(
                                "write_opencrabs_file: filtered {skipped_paragraphs} duplicate paragraph(s) from append to {path_str}"
                            );
                            filtered_content
                        }
                        AppendDedup::AllDuplicate => {
                            return Ok(ToolResult::error(format!(
                                "Content already exists in {}. Skipping duplicate append. \
                                 Use replace if you want to update existing content.",
                                path_str
                            )));
                        }
                    }
                } else {
                    content.to_string()
                };
                // #2099: appending can corrupt the profile config just as an
                // overwrite can, so the candidate is the whole document the
                // file would hold afterwards, not just the appended bytes.
                if crate::config::guard::protected_file(&full_path).is_some() {
                    let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
                    let candidate = format!("{existing}{effective_content}");
                    if let Err(msg) =
                        crate::config::guard::deny_if_would_break(&full_path, &candidate)
                    {
                        return Ok(ToolResult::error(msg));
                    }
                }
                if let Some(parent) = full_path.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    return Ok(ToolResult::error(format!(
                        "Failed to create directory: {}",
                        e
                    )));
                }
                let backup_path = brain_file_safety::backup_before_write(&full_path)
                    .ok()
                    .flatten();
                use std::io::Write;
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full_path)
                {
                    Ok(mut f) => match f.write_all(effective_content.as_bytes()) {
                        Ok(()) => {
                            // Post-write verification: re-read file for full content
                            if let Ok(full_content) = std::fs::read_to_string(&full_path)
                                && let Err(msg) =
                                    verify_or_rollback(&full_path, &full_content, &backup_path)
                            {
                                return Ok(ToolResult::error(msg));
                            }
                            // Epistemic engine (#862): track MEMORY.md beliefs on append.
                            track_memory_belief(path_str, &effective_content);
                            if brain_file_safety::is_protected_path(&full_path) {
                                // Index what we just wrote (#1018). The index was
                                // stayed unsearchable until the next restart — the
                                // window where a duplicate check silently passes.
                                // Writes are the mechanism; the search-side stat
                                // check is only the net for edits made outside this
                                // tool.
                                match crate::memory::get_store() {
                                    Ok(store) => {
                                        if let Err(e) =
                                            crate::memory::index_file(store, &full_path).await
                                        {
                                            tracing::warn!(
                                                "write_opencrabs_file: failed to index {path_str} after append: {e}"
                                            );
                                        }
                                    }
                                    Err(e) => tracing::warn!(
                                        "write_opencrabs_file: store unavailable, {path_str} not indexed: {e}"
                                    ),
                                }
                            }
                            Ok(ToolResult::success(format!(
                                "Appended {} bytes to {}",
                                effective_content.len(),
                                full_path.display()
                            )))
                        }
                        Err(e) => Ok(ToolResult::error(format!(
                            "Failed to append to {}: {}",
                            path_str, e
                        ))),
                    },
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to open {}: {}",
                        path_str, e
                    ))),
                }
            }

            "replace" => {
                let old_text = match input.get("old_text").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => {
                        return Ok(ToolResult::error("old_text is required for replace".into()));
                    }
                };
                let new_text = match input.get("new_text").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => {
                        return Ok(ToolResult::error("new_text is required for replace".into()));
                    }
                };
                let existing = match std::fs::read_to_string(&full_path) {
                    Ok(s) => s,
                    Err(_) => {
                        return Ok(ToolResult::error(format!(
                            "{} not found in your OpenCrabs home. Use overwrite to create it.",
                            path_str
                        )));
                    }
                };
                // NFC-normalize both sides so that NFD-encoded files still match
                // NFC search strings (and vice versa). Common with macOS which
                // stores filenames in NFD; copy-paste can leak NFD into brain files.
                use unicode_normalization::UnicodeNormalization;
                let existing_nfc: String = existing.nfc().collect();
                let old_text_nfc: String = old_text.nfc().collect();
                if !existing_nfc.contains(old_text_nfc.as_str()) {
                    // Trace hex bytes of both sides for encoding debugging.
                    // Useful when NFD/NFC or invisible chars cause silent mismatches.
                    let file_hex: String = existing_nfc
                        .bytes()
                        .take(120)
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let search_hex: String = old_text_nfc
                        .bytes()
                        .take(120)
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    tracing::trace!(
                        "write_opencrabs_file replace miss: file[0..120] hex={} | search[0..120] hex={}",
                        file_hex,
                        search_hex
                    );
                    return Ok(ToolResult::error(format!(
                        "old_text not found in {}. No changes made.",
                        path_str
                    )));
                }
                let new_text_nfc: String = new_text.nfc().collect();
                let updated =
                    existing_nfc.replacen(old_text_nfc.as_str(), new_text_nfc.as_str(), 1);
                // #2099: refuse a replace that would leave the profile config or
                // keys file unparseable. Nothing is written in that case.
                if let Err(msg) = crate::config::guard::deny_if_would_break(&full_path, &updated) {
                    return Ok(ToolResult::error(msg));
                }
                let cleanup_intent = input
                    .get("cleanup_intent")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let brain_file_safety::ShrinkCheck::Rejected { message } =
                    brain_file_safety::check_no_shrink(
                        &full_path,
                        &existing_nfc,
                        &updated,
                        cleanup_intent,
                        false, // consolidation: only self_improve's surgical update qualifies
                    )
                {
                    return Ok(ToolResult::error(message));
                }
                // Record pruned sections when shrinking is allowed (cleanup)
                if cleanup_intent {
                    let removed =
                        crate::brain::rsi_pruned::detect_removed_sections(&existing_nfc, &updated);
                    if !removed.is_empty() {
                        let mut pruned_state = crate::brain::rsi_pruned::PrunedState::load();
                        pruned_state.record_pruned(path_str, removed);
                        if let Err(e) = pruned_state.save() {
                            tracing::warn!(
                                "write_opencrabs_file (update path): recorded {} pruned header(s) for {} \
                                 but pruned.toml save failed: {} — sync_templates() will re-add those sections \
                                 on the next sync until this is fixed",
                                pruned_state
                                    .pruned
                                    .get(path_str)
                                    .map(|h| h.len())
                                    .unwrap_or(0),
                                path_str,
                                e
                            );
                        }
                    }
                }
                let backup_path = brain_file_safety::backup_before_write(&full_path)
                    .ok()
                    .flatten();
                match std::fs::write(&full_path, &updated) {
                    Ok(()) => {
                        if let Err(msg) = verify_or_rollback(&full_path, &updated, &backup_path) {
                            return Ok(ToolResult::error(msg));
                        }
                        // Epistemic engine (#862): track MEMORY.md beliefs on replace.
                        track_memory_belief(path_str, new_text);
                        Ok(ToolResult::success(format!(
                            "Replaced text in {}",
                            full_path.display()
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to write {}: {}",
                        path_str, e
                    ))),
                }
            }

            other => Ok(ToolResult::error(format!(
                "Unknown operation '{}'. Use: overwrite, append, replace.",
                other
            ))),
        }
    }
}
