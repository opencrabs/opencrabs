//! Types for the hashline edit tool.

use serde::{Deserialize, Serialize};

/// A reference to a specific line by its content hash.
///
/// Format: `ID` or `#ID` where ID is the [`HASH_LEN`](super::hash::HASH_LEN)-char
/// content hash. Example: `VKQM` or `#VKQM`.
///
/// Note: Line numbers are NOT included in the ref to avoid the "avalanche" problem
/// where inserting/deleting lines invalidates all subsequent refs. The tool looks
/// up the line by hash alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashRef {
    /// Content hash (see [`HASH_LEN`](super::hash::HASH_LEN)).
    pub hash: String,
}

impl HashRef {
    /// Parse a hash reference string.
    ///
    /// Accepts formats:
    /// - `"VK"` → hash "VK"
    /// - `"#VK"` → hash "VK"
    /// - `"VK|some content"` → hash "VK" (strips content after pipe)
    /// - `"12#VK"` → hash "VK" (legacy format, line number ignored)
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();

        // Strip leading '#' if present
        let s = if let Some(stripped) = s.strip_prefix('#') {
            stripped
        } else {
            s
        };

        // Strip trailing '|' and anything after it (model might include content)
        let hash_str = if let Some(pipe_pos) = s.find('|') {
            &s[..pipe_pos]
        } else {
            s
        };

        // Handle legacy format: LINE#HASH (ignore the line number)
        let hash_str = if let Some(hash_pos) = hash_str.find('#') {
            // Legacy format like "12#VK" - extract just the hash part
            &hash_str[hash_pos + 1..]
        } else {
            hash_str
        };

        if hash_str.len() != super::hash::HASH_LEN {
            // A whole source line pasted where a tag belongs is the failure
            // class this text can actually prevent, so it names both the shape
            // and where a tag comes from (#2201). Across the 84 logged
            // hashline_edit failures the classes are stale tag 37, collision
            // 25, pasted source line 19, other 3: the pasted line is the
            // smallest of the three, which is why the collision and stale-tag
            // paths get their own wording rather than being folded in here.
            return Err(format!(
                "Invalid hash ref '{}': a hash is exactly {} characters taken from the tag \
                 `read_file` prints with hashline=true, which puts it before the `|` on each \
                 line. Use that tag, not the source line itself (got '{}')",
                s,
                super::hash::HASH_LEN,
                hash_str
            ));
        }

        Ok(HashRef {
            hash: hash_str.to_uppercase(),
        })
    }
}

/// A single edit operation in a hashline edit batch.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op")]
pub enum HashlineEditOp {
    /// Replace a single line or a range of lines.
    ///
    /// - `pos`: the anchor line (required)
    /// - `end`: optional end of range (inclusive). If omitted, replaces only `pos`.
    /// - `lines`: the replacement text (newline-separated for multi-line)
    #[serde(rename = "replace")]
    Replace {
        pos: String,
        end: Option<String>,
        lines: String,
    },

    /// Insert lines after the anchor line (or at end of file if pos is omitted).
    ///
    /// - `pos`: anchor line to insert after (optional, defaults to EOF)
    /// - `lines`: the text to insert
    #[serde(rename = "append")]
    Append { pos: Option<String>, lines: String },

    /// Insert lines before the anchor line (or at beginning of file if pos is omitted).
    ///
    /// - `pos`: anchor line to insert before (optional, defaults to BOF)
    /// - `lines`: the text to insert
    #[serde(rename = "prepend")]
    Prepend { pos: Option<String>, lines: String },
}

/// Input for the hashline_edit tool.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HashlineEditInput {
    /// Path to the file to edit
    pub path: String,

    /// Array of edit operations to apply
    pub edits: Vec<HashlineEditOp>,
}

/// Resolved edit with validated HashRefs and computed line ranges.
/// Used internally after validation, before applying edits.
#[derive(Debug, Clone)]
pub struct ResolvedEdit {
    /// The operation type
    pub op: ResolvedOp,
    /// Original index in the edits array (for error reporting)
    pub index: usize,
}

#[derive(Debug, Clone)]
pub enum ResolvedOp {
    Replace {
        start_line: usize,
        end_line: usize,
        new_lines: Vec<String>,
    },
    Append {
        after_line: usize,
        new_lines: Vec<String>,
    },
    Prepend {
        before_line: usize,
        new_lines: Vec<String>,
    },
}
