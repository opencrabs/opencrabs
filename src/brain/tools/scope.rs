//! Per-caller tool scope (#2134).
//!
//! `TelegramConfig::guest_allowed` decides WHO may send a guest query. It never
//! decided WHAT that caller may reach: an admitted guest turn ran the owner's
//! full registry with one restriction, `deny_all_approvals`, which only refuses
//! tools that declare `requires_approval()`. Every read-only tool stayed free:
//! `session_search` with no scope (the owner's DMs included), `channel_search`,
//! `load_brain_file`, `memory_search`, and `read_file` on any absolute path
//! outside the tree.
//!
//! The channel alone cannot answer the question: a guest turn and a normal
//! group turn both say `"telegram"`. So the fact rides the turn itself
//! (`ToolExecutionContext::guest_scope`) and this module owns the answer.
//!
//! Default-deny is deliberate. A tool added tomorrow is out of a guest's reach
//! until someone lists it here, so a new capability can never widen a
//! stranger's surface by accident. Widening it is one explicit line.

use std::path::{Path, PathBuf};

/// Tools a guest turn may reach.
///
/// A stranger's question needs the open web and nothing else from this machine.
/// `read_file` is here because a question can legitimately be about a file the
/// caller just had us look at in that chat, but it is confined to the session
/// working directory (see [`path_stays_inside`]), never the rest of the disk.
pub const GUEST_ALLOWED_TOOLS: &[&str] = &[
    "web_search",
    "exa_search",
    "brave_search",
    "read_file",
    // Discovery: filtered to the list above, so the model can still ask what
    // it has instead of guessing and burning a turn on a refusal.
    "tool_search",
];

/// True when a guest turn may call `name`.
pub fn guest_allows(name: &str) -> bool {
    GUEST_ALLOWED_TOOLS.contains(&name)
}

/// The refusal a guest turn gets, written for the model rather than the log:
/// it names the reachable set so a weaker model stops retrying the same tool
/// under a different spelling and answers with what it has.
pub fn guest_refusal(name: &str) -> String {
    format!(
        "{name} is not available to a guest turn. This query came from someone who is not the \
         owner of this agent, so the turn is scoped to: {}. Answer from the conversation, from \
         web search, or from a file inside the working directory.",
        GUEST_ALLOWED_TOOLS.join(", ")
    )
}

/// True when `path`, after resolving symlinks where it exists, stays under
/// `root`.
///
/// A guest read is confined to the session working directory. Both sides are
/// canonicalized first, so a symlink inside the tree that points outside it is
/// refused too: the check is about where the bytes live, not about how the
/// path was spelled.
pub fn path_stays_inside(root: &Path, path: &Path) -> bool {
    let root = canonical_or_normalized(root);
    let target = canonical_or_normalized(path);
    target.starts_with(&root)
}

/// Canonical form when the path exists, a lexically normalized form otherwise.
/// `read_file` only ever sees existing paths, but the helper stays honest for
/// callers that check before a write.
fn canonical_or_normalized(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}
