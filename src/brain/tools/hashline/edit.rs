//! Hashline Edit Tool
//!
//! Hash-anchored file editing: models reference lines by content hashes
//! instead of reproducing text. Eliminates stale-line errors and reduces token
//! usage, especially for weaker models.

use super::hash::{HASH_ALPHABET, HASH_LEN, hash_all_lines};
use super::types::{HashRef, HashlineEditInput, HashlineEditOp, ResolvedEdit, ResolvedOp};
use crate::brain::tools::brain_file_safety;
use crate::brain::tools::edit::build_edit_diff;
use crate::brain::tools::error::{Result, ToolError, validate_file_path};
use crate::brain::tools::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use tokio::fs;

/// Hashline edit tool
pub struct HashlineEditTool;

#[async_trait]
impl Tool for HashlineEditTool {
    fn name(&self) -> &str {
        "hashline_edit"
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
        "Edit a file using hash-anchored line references. Each line is identified by the \
         content hash read_file prints with hashline=true. Reference lines by hash alone (e.g. \
         'VKQMZP' or '#VKQMZP'); legacy 'LINE#HASH' format is accepted but the line number is ignored. \
         Stale hashes are rejected before any changes are applied. Supports batch edits (multiple \
         operations in one call)."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "edits": {
                    "type": "array",
                    "description": "Array of edit operations to apply atomically",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["replace", "append", "prepend"],
                                "description": "Edit operation type"
                            },
                            "pos": {
                                "type": "string",
                                "description": "Anchor: the content hash from hashline read (e.g. 'VKQMZP' or '#VKQMZP'), taken from before the `|` on the line. Legacy 'LINE#HASH' accepted; line number ignored. Required for replace, optional for append/prepend."
                            },
                            "end": {
                                "type": "string",
                                "description": "End hash for range replace (inclusive, same format as pos). Omit to replace a single line."
                            },
                            "lines": {
                                "type": "string",
                                "description": "Replacement or insertion text. Use \\n for multi-line content."
                            }
                        },
                        "required": ["op", "lines"]
                    },
                    "minItems": 1
                }
            },
            "required": ["path", "edits"]
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
        true
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let _: HashlineEditInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: HashlineEditInput = serde_json::from_value(input)?;

        // Validate path
        let path = match validate_file_path(&input.path, &context.working_dir()) {
            Ok(p) => p,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };

        // Brain-file guardrail
        if brain_file_safety::is_protected_path(&path) {
            return Ok(ToolResult::error(format!(
                "Refusing to edit protected brain file '{}' with hashline_edit. \
                 Use the `write_opencrabs_file` tool instead.",
                path.display()
            )));
        }

        // One writer at a time on this path (#593). `hashline_edit` is a
        // read-modify-write like `edit_file`, so without the lock two
        // concurrent calls both read the original and the later write
        // replaces the file, dropping the earlier edit. The guard is
        // advisory: a contended write proceeds rather than failing.
        let write_lock = crate::brain::tools::path_lock::acquire(&path);
        let contended = write_lock.as_ref().is_some_and(|l| !l.is_held());

        // Read file
        let content = fs::read_to_string(&path).await.map_err(ToolError::Io)?;
        let original_lines: Vec<&str> = content.lines().collect();
        let total_lines = original_lines.len();

        // Compute hashes for all lines
        let line_hashes = hash_all_lines(&content);

        // Build a reverse lookup: hash → list of line numbers (for collision detection and lookup)
        let mut hash_to_lines: std::collections::HashMap<&str, Vec<usize>> =
            std::collections::HashMap::new();
        for (num, hash) in &line_hashes {
            hash_to_lines.entry(hash.as_str()).or_default().push(*num);
        }

        // Phase 1: Validate all hash references and resolve to line numbers
        let mut resolved = Vec::with_capacity(input.edits.len());
        for (i, edit) in input.edits.iter().enumerate() {
            match resolve_edit(edit, i, &hash_to_lines, total_lines)? {
                Ok(resolved_edit) => resolved.push(resolved_edit),
                Err(error_msg) => {
                    // A parse failure is the caller pasting a source line where a
                    // tag belongs. Show the real thing: two tags from the file
                    // they named, in the exact form read_file prints them (#2201).
                    let error_msg = if error_msg.contains("Invalid hash ref") {
                        format!(
                            "{error_msg}\n\n{}",
                            ref_examples(&line_hashes, &original_lines)
                        )
                    } else {
                        error_msg
                    };
                    return Ok(ToolResult::error(error_msg));
                }
            }
        }

        // Phase 2: Sort bottom-up (highest line number first) to preserve line numbers
        resolved.sort_by(|a, b| {
            let line_a = edit_sort_line(a);
            let line_b = edit_sort_line(b);
            line_b.cmp(&line_a) // descending
        });

        // Phase 3: Detect overlapping ranges
        if let Some(overlap_err) = detect_overlaps(&resolved) {
            return Ok(ToolResult::error(overlap_err));
        }

        // Phase 4: Apply edits (bottom-up, so line numbers stay stable)
        let mut result_lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();

        for edit in &resolved {
            apply_edit(&mut result_lines, edit);
        }

        // Join with the file's own ending (#2227). A bare `\n` here rewrote
        // every line of a CRLF file: editing one line converted the whole
        // document to LF, and the result diff only listed the changed line, so
        // nothing said so. This tool applies edits bottom-up over a line
        // vector, so a mixed-ending file cannot keep per-line endings; the
        // dominant ending wins, which `hashline_crlf_2227_test` pins.
        let eol = crate::brain::tools::line_ops::dominant_eol(&content);
        let new_content = result_lines.join(eol);

        // Preserve the final newline if the original had one
        let new_content = if content.ends_with('\n') && !new_content.ends_with('\n') {
            format!("{}{}", new_content, eol)
        } else {
            new_content
        };

        // Write
        fs::write(&path, &new_content)
            .await
            .map_err(ToolError::Io)?;
        // Release the path lock taken before the read (#593).
        drop(write_lock);

        // The session now knows the file as what it just wrote, so its next
        // write_file is not mistaken for another agent's change (#2210). The
        // read above was whole-file, so this edit is also a basis for a later
        // overwrite (#1168), the same as a whole-file read or a write.
        crate::brain::tools::file_versions::record(context.session_id, &path, &new_content);
        crate::brain::tools::read_state::mark_fully_read(context.session_id, &path);

        // Track file in session (fire and forget, path-only)
        if let Some(ref sc) = context.service_context {
            let fs = crate::services::FileService::new(sc.clone());
            let _ = fs
                .get_or_create_file(context.session_id, path.clone(), None)
                .await;
        }

        let lines_before = original_lines.len();
        let lines_after = new_content.lines().count();
        let diff = build_edit_diff(&content, &new_content);

        let mut output = format!(
            "Successfully edited {} (hashline). Lines: {} → {}\n",
            path.display(),
            lines_before,
            lines_after
        );
        output.push_str(&diff);
        // An overlapping write is reported, not swallowed: the file may hold
        // neither writer's intent, and only the caller can decide (#593).
        if contended {
            output.push_str(&crate::brain::tools::path_lock::contention_notice(&path));
        }

        Ok(ToolResult::success(output))
    }
}

/// Resolve a single edit operation, validating all hash references.
///
/// Returns Ok(Ok(resolved)) on success, Ok(Err(message)) on validation failure
/// (so we can return a user-friendly error without propagating ToolError).
fn resolve_edit(
    edit: &HashlineEditOp,
    index: usize,
    hash_to_lines: &std::collections::HashMap<&str, Vec<usize>>,
    total_lines: usize,
) -> Result<std::result::Result<ResolvedEdit, String>> {
    match edit {
        HashlineEditOp::Replace { pos, end, lines } => {
            let pos_ref = match HashRef::parse(pos) {
                Ok(r) => r,
                Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
            };

            // Validate pos hash and get line number
            let start_line = match validate_hash(&pos_ref, hash_to_lines, total_lines) {
                Ok(line) => line,
                Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
            };

            let end_line = if let Some(end_str) = end {
                let end_ref = match HashRef::parse(end_str) {
                    Ok(r) => r,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                };
                let end_line = match validate_hash(&end_ref, hash_to_lines, total_lines) {
                    Ok(line) => line,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                };
                if end_line < start_line {
                    return Ok(Err(format!(
                        "Edit #{}: end line ({}) must be >= start line ({})",
                        index + 1,
                        end_line,
                        start_line
                    )));
                }
                end_line
            } else {
                start_line
            };

            let new_lines = strip_hashline_prefixes(lines);

            Ok(Ok(ResolvedEdit {
                op: ResolvedOp::Replace {
                    start_line,
                    end_line,
                    new_lines,
                },
                index,
            }))
        }

        HashlineEditOp::Append { pos, lines } => {
            let after_line = if let Some(pos_str) = pos {
                let pos_ref = match HashRef::parse(pos_str) {
                    Ok(r) => r,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                };
                match validate_hash(&pos_ref, hash_to_lines, total_lines) {
                    Ok(line) => line,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                }
            } else {
                total_lines // append at EOF
            };

            let new_lines = strip_hashline_prefixes(lines);

            Ok(Ok(ResolvedEdit {
                op: ResolvedOp::Append {
                    after_line,
                    new_lines,
                },
                index,
            }))
        }

        HashlineEditOp::Prepend { pos, lines } => {
            let before_line = if let Some(pos_str) = pos {
                let pos_ref = match HashRef::parse(pos_str) {
                    Ok(r) => r,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                };
                match validate_hash(&pos_ref, hash_to_lines, total_lines) {
                    Ok(line) => line,
                    Err(e) => return Ok(Err(format!("Edit #{}: {}", index + 1, e))),
                }
            } else {
                1 // prepend at BOF
            };

            let new_lines = strip_hashline_prefixes(lines);

            Ok(Ok(ResolvedEdit {
                op: ResolvedOp::Prepend {
                    before_line,
                    new_lines,
                },
                index,
            }))
        }
    }
}

/// Two or three real `HASH|content` lines from the file being edited, printed
/// next to a parse failure. The dominant `hashline_edit` failure is a source
/// line pasted where a tag belongs, and the old error only stated a length, so
/// the correct form lived one tool call away. Showing it inside the failure
/// ends the round trip (#2201).
fn ref_examples(line_hashes: &[(usize, String)], lines: &[&str]) -> String {
    let mut out =
        String::from("Hash refs look like this, taken from `read_file` with hashline=true:\n");
    let mut shown = 0;
    for (num, hash) in line_hashes {
        if shown == 3 {
            break;
        }
        let Some(content) = lines.get(num.saturating_sub(1)) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        let mut sample: String = content.chars().take(60).collect();
        if content.chars().count() > 60 {
            sample.push_str("...");
        }
        out.push_str(&format!(
            "  {}\n",
            super::hash::format_hashline(*num, hash, &sample)
        ));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("  (this file has no non-blank lines to sample)\n");
    }
    out.push_str("Use the tag before the `|`, not the source line.");
    out
}

/// Validate that a HashRef's hash exists in the file and is unique.
///
/// Returns the line number where the hash was found.
///
/// Checks for:
/// 1. Hash not found in file
/// 2. Hash collision (same hash on multiple lines) → escalate to edit_file
fn validate_hash(
    href: &HashRef,
    hash_to_lines: &std::collections::HashMap<&str, Vec<usize>>,
    _total_lines: usize,
) -> std::result::Result<usize, String> {
    // Look up which line(s) have this hash
    match hash_to_lines.get(href.hash.as_str()) {
        None => Err(format!(
            "Hash #{} not found in file. The file may have changed since your last read. \
             Re-read with hashline=true to get updated references.",
            href.hash
        )),
        Some(lines_with_hash) if lines_with_hash.len() > 1 => {
            let line_list: Vec<String> = lines_with_hash.iter().map(|l| l.to_string()).collect();
            Err(format!(
                "Hash collision: #{} appears on {} lines ({}). \
                 This hash is ambiguous and cannot safely identify a single line. \
                 Use the `edit_file` tool with search/replace instead of hashline_edit.",
                href.hash,
                lines_with_hash.len(),
                line_list.join(", ")
            ))
        }
        Some(lines_with_hash) => {
            // Unique hash, return the line number
            Ok(lines_with_hash[0])
        }
    }
}

/// Get the sort key for an edit (used for bottom-up ordering).
fn edit_sort_line(edit: &ResolvedEdit) -> usize {
    match &edit.op {
        ResolvedOp::Replace { start_line, .. } => *start_line,
        ResolvedOp::Append { after_line, .. } => *after_line,
        ResolvedOp::Prepend { before_line, .. } => *before_line,
    }
}

/// Detect overlapping ranges in a set of resolved edits.
/// Returns Some(error_message) if overlaps found, None if clean.
fn detect_overlaps(edits: &[ResolvedEdit]) -> Option<String> {
    // Build a list of (start, end) ranges for each edit
    let mut ranges: Vec<(usize, usize, usize)> = edits
        .iter()
        .map(|e| {
            let (start, end) = match &e.op {
                ResolvedOp::Replace {
                    start_line,
                    end_line,
                    ..
                } => (*start_line, *end_line),
                ResolvedOp::Append { after_line, .. } => (*after_line + 1, *after_line + 1),
                ResolvedOp::Prepend { before_line, .. } => (*before_line, *before_line),
            };
            (start, end, e.index + 1)
        })
        .collect();

    // Sort by start line ascending
    ranges.sort_by_key(|r| r.0);

    // Check adjacent ranges for overlap
    for i in 0..ranges.len().saturating_sub(1) {
        let (_, end_a, idx_a) = ranges[i];
        let (start_b, _, idx_b) = ranges[i + 1];
        if end_a >= start_b {
            return Some(format!(
                "Overlapping edits: edit #{} (ending at line {}) overlaps with edit #{} (starting at line {}). \
                 Adjust the ranges so they don't overlap.",
                idx_a, end_a, idx_b, start_b
            ));
        }
    }

    None
}

/// Apply a single resolved edit to the line buffer.
/// Edits are applied bottom-up (highest line first) so earlier line numbers stay stable.
fn apply_edit(lines: &mut Vec<String>, edit: &ResolvedEdit) {
    match &edit.op {
        ResolvedOp::Replace {
            start_line,
            end_line,
            new_lines,
        } => {
            let start_idx = start_line - 1; // convert to 0-indexed
            let end_idx = *end_line; // exclusive end for drain

            // Clamp to valid range
            let start_idx = start_idx.min(lines.len());
            let end_idx = end_idx.min(lines.len());

            // Remove old lines
            lines.drain(start_idx..end_idx);

            // Insert new lines
            for (i, new_line) in new_lines.iter().enumerate() {
                lines.insert(start_idx + i, new_line.clone());
            }
        }

        ResolvedOp::Append {
            after_line,
            new_lines,
        } => {
            let insert_idx = (*after_line).min(lines.len());
            for (i, new_line) in new_lines.iter().enumerate() {
                lines.insert(insert_idx + i, new_line.clone());
            }
        }

        ResolvedOp::Prepend {
            before_line,
            new_lines,
        } => {
            let insert_idx = (before_line - 1).min(lines.len());
            for (i, new_line) in new_lines.iter().enumerate() {
                lines.insert(insert_idx + i, new_line.clone());
            }
        }
    }
}

/// Strip a leading hashline tag the model echoed from the read output.
///
/// `read_file hashline=true` prints each line as `<HASH>|content`, and marks
/// ambiguous lines `COLLISION|content`. Models routinely copy those exact forms
/// into the replacement `lines`, so without stripping, `VKQM|let x = 1;` would
/// be written verbatim into the file — the "edit mangled my file" reports
/// (#573). Strip a leading tag when it is unambiguously one of ours:
///   - `COLLISION|` (the ambiguity marker), or
///   - `<HASH>|` where `<HASH>` is exactly `HASH_LEN` chars all from the hash
///     alphabet, or
///   - the legacy `<DIGITS>#<HASH>|` form.
///
/// The hash-alphabet check keeps real content that merely contains an early
/// `|` (markdown tables, `a | b`) untouched: those tags are not valid hashes.
fn strip_hashline_prefixes(text: &str) -> Vec<String> {
    text.lines().map(strip_one_hashline_prefix).collect()
}

/// True when every byte of `tag` is a hash-alphabet character and the tag is
/// exactly `HASH_LEN` long.
fn is_hash_tag(tag: &str) -> bool {
    tag.len() == HASH_LEN && tag.bytes().all(|b| HASH_ALPHABET.contains(&b))
}

fn strip_one_hashline_prefix(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("COLLISION|") {
        return rest.to_string();
    }
    let Some(pipe) = line.find('|') else {
        return line.to_string();
    };
    let (tag, after) = (&line[..pipe], &line[pipe + 1..]);
    // Current format: `<HASH>|content`.
    if is_hash_tag(tag) {
        return after.to_string();
    }
    // Legacy format: `<DIGITS>#<HASH>|content`.
    if let Some(hash_pos) = tag.find('#') {
        let (num, hash) = (&tag[..hash_pos], &tag[hash_pos + 1..]);
        if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) && is_hash_tag(hash) {
            return after.to_string();
        }
    }
    line.to_string()
}
