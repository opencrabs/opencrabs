//! Edit File Tool
//!
//! Intelligently modify portions of files (find/replace, line-based edits).

use super::error::{Result, ToolError, validate_file_path};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;

/// Edit file tool
pub struct EditTool;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation")]
enum EditOperation {
    /// Replace old_text with new_text
    #[serde(rename = "replace")]
    Replace { old_text: String, new_text: String },

    /// Replace text at specific line range
    #[serde(rename = "replace_lines")]
    ReplaceLines {
        start_line: usize,
        end_line: usize,
        new_text: String,
    },

    /// Insert text at specific line
    #[serde(rename = "insert_line")]
    InsertLine { line: usize, text: String },

    /// Delete lines
    #[serde(rename = "delete_lines")]
    DeleteLines { start_line: usize, end_line: usize },

    /// Regex replace
    #[serde(rename = "regex_replace")]
    RegexReplace {
        pattern: String,
        replacement: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct EditInput {
    /// Path to the file to edit
    path: String,

    /// Edit operation to perform
    #[serde(flatten)]
    operation: EditOperation,
}

impl EditTool {
    /// Normalize the incoming JSON so the model can call us with the
    /// Claude-style `{path, old_text, new_text}` shape without the
    /// explicit `operation` tag. Also accepts legacy aliases
    /// (`old_string`/`new_string`).
    fn normalize_input(mut value: Value) -> Value {
        let Some(obj) = value.as_object_mut() else {
            return value;
        };

        // Accept `old_string`/`new_string` aliases (Anthropic Edit tool).
        if !obj.contains_key("old_text")
            && let Some(v) = obj.remove("old_string")
        {
            obj.insert("old_text".to_string(), v);
        }
        if !obj.contains_key("new_text")
            && let Some(v) = obj.remove("new_string")
        {
            obj.insert("new_text".to_string(), v);
        }

        // Infer `operation` when the model omitted it.
        if !obj.contains_key("operation") {
            let inferred = if obj.contains_key("old_text") && obj.contains_key("new_text") {
                Some("replace")
            } else if obj.contains_key("start_line")
                && obj.contains_key("end_line")
                && obj.contains_key("new_text")
            {
                Some("replace_lines")
            } else if obj.contains_key("line") && obj.contains_key("text") {
                Some("insert_line")
            } else if obj.contains_key("start_line") && obj.contains_key("end_line") {
                Some("delete_lines")
            } else if obj.contains_key("pattern") && obj.contains_key("replacement") {
                Some("regex_replace")
            } else {
                None
            };
            if let Some(op) = inferred {
                obj.insert("operation".to_string(), Value::String(op.to_string()));
            }
        }

        value
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    /// The one path this invocation writes, resolved against the working
    /// directory (#593). The batch scheduler asks the tool instead of
    /// consulting a central name table.
    fn write_target(
        &self,
        input: &Value,
        working_directory: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        crate::brain::tools::r#trait::write_target_from_path_arg(input, working_directory)
    }

    fn description(&self) -> &str {
        "Edit a file intelligently using various operations: replace text, replace lines, insert lines, delete lines, or regex replace."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "operation": {
                    "type": "string",
                    "description": "Type of edit operation",
                    "enum": ["replace", "replace_lines", "insert_line", "delete_lines", "regex_replace"]
                },
                "old_text": {
                    "type": "string",
                    "description": "Text to find and replace (for 'replace' operation)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text (for 'replace' and 'replace_lines' operations)"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Starting line number (0-indexed, for line operations)",
                    "minimum": 0
                },
                "end_line": {
                    "type": "integer",
                    "description": "Ending line number (0-indexed, inclusive, for line operations)",
                    "minimum": 0
                },
                "line": {
                    "type": "integer",
                    "description": "Line number to insert at (0-indexed, for 'insert_line')",
                    "minimum": 0
                },
                "text": {
                    "type": "string",
                    "description": "Text to insert (for 'insert_line')"
                },
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to match (for 'regex_replace')"
                },
                "replacement": {
                    "type": "string",
                    "description": "Replacement text (for 'regex_replace')"
                },
            },
            "required": ["path"],
            "description": "If 'operation' is omitted but 'old_text' and 'new_text' are provided, 'replace' is inferred (Claude-style Edit shape)."
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ReadFiles,
            ToolCapability::WriteFiles,
            ToolCapability::SystemModification,
        ]
    }

    fn requires_approval(&self) -> bool {
        true // Editing files requires approval
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let normalized = Self::normalize_input(input.clone());
        let _: EditInput = serde_json::from_value(normalized)
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: EditInput = serde_json::from_value(Self::normalize_input(input))?;

        // Validate path: safety check, existence, and file type
        let path = match validate_file_path(&input.path, &context.working_dir()) {
            Ok(p) => p,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };

        // Brain-file guardrail (issue #91): protected brain files
        // (SOUL.md, MEMORY.md, USER.md, etc.) must go through
        // `write_opencrabs_file`, which enforces append-only,
        // dedup-aware shrink, and `.bak` snapshots. Generic edit_file
        // does none of those, so reject the call before mutating
        // anything and tell the agent to switch tools.
        if super::brain_file_safety::is_protected_path(&path) {
            return Ok(ToolResult::error(format!(
                "Refusing to edit protected brain file '{}' with generic edit_file. \
                 Use the `write_opencrabs_file` tool instead. It enforces append-only \
                 writes, dedup-aware shrinking, and saves a `.bak` snapshot before every \
                 change.",
                path.display()
            )));
        }

        // One writer at a time on this path (#1153), held across the whole
        // read-modify-write rather than the write alone (#593). The result is
        // computed from the bytes read below, so a lock taken after that read
        // leaves the two writers' reads racing: both see the same original and
        // the later write replaces the file, dropping the earlier edit while
        // both report success. The guard stays advisory — a contended write
        // proceeds rather than failing — but the hold is one read and one
        // write of a single file, far shorter than the wait, so a competing
        // edit of the same path serialises in practice.
        let write_lock = super::path_lock::acquire(&path);
        let contended = write_lock.as_ref().is_some_and(|l| !l.is_held());

        // Read file content
        let content = fs::read_to_string(&path).await.map_err(ToolError::Io)?;

        // Perform edit operation
        let new_content = match input.operation {
            EditOperation::Replace { old_text, new_text } => {
                // Try exact substring match first.
                if content.contains(&old_text) {
                    let count = content.matches(&old_text).count();
                    if count > 1 {
                        return Ok(ToolResult::error(format!(
                            "old_text appears {count} times as exact substring. \
                             Include more context to make a unique match."
                        )));
                    }
                    content.replacen(&old_text, &new_text, 1)
                } else {
                    // Fall back to fuzzy line-sequence matching (tolerates
                    // minor whitespace, indentation, and Unicode punctuation
                    // differences). Ported from OpenAI Codex seek_sequence.
                    match super::fuzzy::fuzzy_replace_once(&content, &old_text, &new_text) {
                        Ok(new_content) => new_content,
                        Err(msg) => return Ok(ToolResult::error(msg)),
                    }
                }
            }

            EditOperation::ReplaceLines {
                start_line,
                end_line,
                new_text,
            } => {
                let lines: Vec<&str> = content.lines().collect();
                if start_line >= lines.len() || end_line >= lines.len() {
                    return Ok(ToolResult::error(format!(
                        "Line range {}-{} out of bounds (file has {} lines)",
                        start_line,
                        end_line,
                        lines.len()
                    )));
                }
                if start_line > end_line {
                    return Ok(ToolResult::error(
                        "start_line must be <= end_line".to_string(),
                    ));
                }

                // Splice the edited byte range out of the original instead of
                // rebuilding the file from `lines()` (#2227). `lines()` drops
                // every terminator and a trailing `\r`, so the join could only
                // put back the ending it was told to: a CRLF file came out LF
                // and every file lost its final newline. Nothing said so,
                // because the result diff only lists the changed lines.
                super::line_ops::replace_lines(&content, start_line, end_line, &new_text)
            }

            EditOperation::InsertLine { line, text } => {
                let lines: Vec<&str> = content.lines().collect();
                if line > lines.len() {
                    return Ok(ToolResult::error(format!(
                        "Line {} out of bounds (file has {} lines)",
                        line,
                        lines.len()
                    )));
                }

                // Byte splice, same reason as `replace_lines` (#2227).
                super::line_ops::insert_line(&content, line, &text)
            }

            EditOperation::DeleteLines {
                start_line,
                end_line,
            } => {
                let lines: Vec<&str> = content.lines().collect();
                if start_line >= lines.len() || end_line >= lines.len() {
                    return Ok(ToolResult::error(format!(
                        "Line range {}-{} out of bounds (file has {} lines)",
                        start_line,
                        end_line,
                        lines.len()
                    )));
                }
                if start_line > end_line {
                    return Ok(ToolResult::error(
                        "start_line must be <= end_line".to_string(),
                    ));
                }

                // Byte splice, same reason as `replace_lines` (#2227).
                super::line_ops::delete_lines(&content, start_line, end_line)
            }

            EditOperation::RegexReplace {
                pattern,
                replacement,
            } => {
                let regex = regex::Regex::new(&pattern)
                    .map_err(|e| ToolError::InvalidInput(format!("Invalid regex: {}", e)))?;

                if !regex.is_match(&content) {
                    return Ok(ToolResult::error(format!(
                        "Pattern not found in file: '{}'",
                        pattern
                    )));
                }

                regex
                    .replace_all(&content, replacement.as_str())
                    .to_string()
            }
        };

        // Never let a raw edit corrupt the OpenCrabs config.toml/keys.toml (#713):
        // a broken write there takes down all API keys and the bot token. Deny it,
        // tell the agent why, and leave the file untouched.
        if let Err(msg) = crate::config::guard::deny_if_would_break(&path, &new_content) {
            return Err(ToolError::InvalidInput(msg));
        }

        // Write modified content. The path lock was taken above, before the
        // read, and is still held here (#593).
        super::fs_util::atomic_write_file(&path, new_content.as_bytes()).await?;
        drop(write_lock);

        // The session now knows the file as what it just wrote, so its next
        // write_file is not mistaken for another agent's change (#2210). The
        // read above was whole-file, so this edit is also a basis for a later
        // overwrite (#1168), the same as a whole-file read or a write.
        super::file_versions::record(context.session_id, &path, &new_content);
        super::read_state::mark_fully_read(context.session_id, &path);

        // Track file in session (fire and forget, path-only)
        if let Some(ref sc) = context.service_context {
            let fs = crate::services::FileService::new(sc.clone());
            let _ = fs
                .get_or_create_file(context.session_id, path.clone(), None)
                .await;
        }

        let lines_before = content.lines().count();
        let lines_after = new_content.lines().count();

        // Build a compact diff for context (shown in expanded tool details)
        let diff = build_edit_diff(&content, &new_content);
        let mut output = format!(
            "Successfully edited {}. Lines: {} → {}\n",
            path.display(),
            lines_before,
            lines_after
        );
        output.push_str(&diff);
        // An overlapping write is reported, not swallowed: the file may hold
        // neither writer's intent, and only the caller can decide what to do.
        if contended {
            output.push_str(&super::path_lock::contention_notice(&path));
        }

        Ok(ToolResult::success(output))
    }
}

/// Build a compact unified-style diff between old and new content.
/// Shows only changed lines with `-`/`+` prefixes (capped at 40 diff lines).
pub(crate) fn build_edit_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut diff = String::new();
    let mut diff_lines = 0usize;
    let max_diff_lines = 40;

    // Simple LCS-based diff: walk both sequences
    let mut i = 0;
    let mut j = 0;
    while i < old_lines.len() || j < new_lines.len() {
        if diff_lines >= max_diff_lines {
            diff.push_str("... (diff truncated)\n");
            break;
        }
        if i < old_lines.len() && j < new_lines.len() && old_lines[i] == new_lines[j] {
            // Lines match — skip (context not needed for compact diff)
            i += 1;
            j += 1;
        } else {
            // Find how far ahead the old line appears in new (or vice versa)
            let new_ahead = new_lines[j..]
                .iter()
                .position(|l| i < old_lines.len() && *l == old_lines[i]);
            let old_ahead = old_lines[i..]
                .iter()
                .position(|l| j < new_lines.len() && *l == new_lines[j]);

            match (new_ahead, old_ahead) {
                (Some(na), Some(oa)) if na <= oa => {
                    // new has insertions before the match
                    for line in &new_lines[j..j + na] {
                        diff.push_str(&format!("+ {}\n", line));
                        diff_lines += 1;
                        if diff_lines >= max_diff_lines {
                            break;
                        }
                    }
                    j += na;
                }
                (Some(_), Some(oa)) => {
                    // old has deletions before the match
                    for line in &old_lines[i..i + oa] {
                        diff.push_str(&format!("- {}\n", line));
                        diff_lines += 1;
                        if diff_lines >= max_diff_lines {
                            break;
                        }
                    }
                    i += oa;
                }
                (Some(na), None) => {
                    for line in &new_lines[j..j + na] {
                        diff.push_str(&format!("+ {}\n", line));
                        diff_lines += 1;
                        if diff_lines >= max_diff_lines {
                            break;
                        }
                    }
                    j += na;
                }
                (None, Some(oa)) => {
                    for line in &old_lines[i..i + oa] {
                        diff.push_str(&format!("- {}\n", line));
                        diff_lines += 1;
                        if diff_lines >= max_diff_lines {
                            break;
                        }
                    }
                    i += oa;
                }
                (None, None) => {
                    // No match ahead — emit both as changed
                    if i < old_lines.len() {
                        diff.push_str(&format!("- {}\n", old_lines[i]));
                        diff_lines += 1;
                        i += 1;
                    }
                    if diff_lines < max_diff_lines && j < new_lines.len() {
                        diff.push_str(&format!("+ {}\n", new_lines[j]));
                        diff_lines += 1;
                        j += 1;
                    }
                }
            }
        }
    }

    diff
}
