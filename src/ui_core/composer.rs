//! Composer state that must outlive the widget holding it.
//!
//! Two things a composer owns are per-conversation and must never be dropped
//! just because the widget was rebuilt or the user looked elsewhere: the text
//! being typed, and the submitted history. This module owns both, so the
//! terminal and the window keep the same answer without either of them keeping
//! a private copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use uuid::Uuid;

use super::input_history::{HISTORY_CAP, record_input_history};

/// Everything a composer is holding for one conversation.
///
/// `history_stash` is included on purpose: a recall that spans a focus change
/// must come back with the draft it covered, not leave it behind in the widget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerDraft {
    pub text: String,
    pub cursor: usize,
    pub history_index: Option<usize>,
    pub history_stash: String,
}

impl ComposerDraft {
    /// True when there is nothing worth restoring. Whitespace-only text and an
    /// empty stash both count as nothing, so switching conversations does not
    /// accumulate blank entries.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.history_stash.is_empty()
    }
}

/// Drafts keyed by the conversation they were typed in.
#[derive(Debug, Default, Clone)]
pub struct DraftStore {
    drafts: HashMap<Uuid, ComposerDraft>,
}

impl DraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep `draft` for `session`, or drop the entry when nothing is there to
    /// restore. Storing an empty draft would let a later switch resurrect a
    /// blank field over a real one.
    pub fn store(&mut self, session: Uuid, draft: ComposerDraft) {
        if draft.is_empty() {
            self.drafts.remove(&session);
        } else {
            self.drafts.insert(session, draft);
        }
    }

    /// Take the draft for `session`, leaving the store without it. The caller
    /// owns it from here: the composer is being shown again.
    pub fn take(&mut self, session: Uuid) -> Option<ComposerDraft> {
        self.drafts.remove(&session)
    }

    pub fn peek(&self, session: Uuid) -> Option<&ComposerDraft> {
        self.drafts.get(&session)
    }

    /// Forget a conversation's draft. A deleted session must not keep text
    /// waiting for a composer it can never reach.
    pub fn remove(&mut self, session: Uuid) {
        self.drafts.remove(&session);
    }

    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}

/// The submitted history every composer in one process shares.
///
/// The window and each of its split panes are separate widgets. They must walk
/// ONE list, in real time, or recalling shows a different past depending on
/// which field has the caret.
pub type SharedHistory = Arc<Mutex<Vec<String>>>;

pub fn new_shared_history(entries: Vec<String>) -> SharedHistory {
    Arc::new(Mutex::new(entries))
}

/// Lock the shared list, ignoring poisoning.
///
/// A panic inside one frontend must not take the history away from the other;
/// the list itself is still intact, so recovering the guard is the right
/// answer, not propagating the panic.
pub fn lock_history(history: &SharedHistory) -> MutexGuard<'_, Vec<String>> {
    history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `~/.opencrabs/history.txt`, the one file both surfaces read and write.
pub fn default_history_path() -> PathBuf {
    crate::config::opencrabs_home().join("history.txt")
}

/// Read the submitted history, one entry per line, oldest first.
pub fn load_history(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Rewrite the file from `entries`, keeping only the newest [`HISTORY_CAP`].
pub fn save_history(path: &Path, entries: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let start = entries.len().saturating_sub(HISTORY_CAP);
    let mut body = entries[start..].join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(path, body)
}

/// Record one submitted entry, then rewrite the file from the merged list.
///
/// The read is deliberate. The terminal holds its own copy of the list and the
/// window holds the shared one, so appending blind would let the older file
/// order win and lose entries written by the other surface. The cap is applied
/// here too, which is what stops a long-lived install from growing forever.
pub fn append_history(path: &Path, entry: &str) -> std::io::Result<()> {
    let mut entries = load_history(path);
    record_input_history(&mut entries, entry);
    save_history(path, &entries)
}

/// Esc twice clears the composer but keeps the typed draft in the stash, so
/// Arrow Up brings it back.
///
/// A recalled line is not the draft. Clearing it must not overwrite the stash
/// (that would replace the user's sentence with an entry they never typed),
/// and it must leave the recall walk, or the "restore the stash past the
/// newest entry" branch can never fire again.
pub fn escape_clears_input(
    buffer: &mut String,
    cursor: &mut usize,
    index: &mut Option<usize>,
    stash: &mut String,
) {
    if index.is_none() && !buffer.is_empty() {
        *stash = std::mem::take(buffer);
    } else {
        buffer.clear();
    }
    *cursor = 0;
    *index = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> ComposerDraft {
        ComposerDraft {
            text: text.to_string(),
            cursor: text.len(),
            history_index: None,
            history_stash: String::new(),
        }
    }

    #[test]
    fn composer_drafts_are_keyed_by_the_conversation_they_were_typed_in() {
        let mut store = DraftStore::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        store.store(a, draft("half a sentence"));
        store.store(b, draft("another one"));

        assert_eq!(store.len(), 2);
        assert_eq!(
            store.take(a).map(|d| d.text),
            Some("half a sentence".to_string()),
            "each conversation gets its own draft back"
        );
        assert!(store.peek(a).is_none(), "taking removes the entry");
        assert_eq!(
            store.peek(b).map(|d| d.text.as_str()),
            Some("another one"),
            "taking one draft never disturbs the other"
        );
    }

    #[test]
    fn composer_draft_store_drops_empty_drafts_instead_of_resurrecting_a_blank_field() {
        let mut store = DraftStore::new();
        let id = Uuid::new_v4();
        store.store(id, ComposerDraft::default());
        assert!(store.is_empty(), "nothing to restore is not a draft");

        store.store(id, draft("kept"));
        store.remove(id);
        assert!(store.peek(id).is_none(), "a deleted session leaves nothing");
    }

    #[test]
    fn composer_escape_keeps_the_typed_draft_when_a_recall_is_cleared() {
        let mut buffer = "recalled entry".to_string();
        let mut cursor = buffer.len();
        let mut index = Some(0);
        let mut stash = "what i actually typed".to_string();

        escape_clears_input(&mut buffer, &mut cursor, &mut index, &mut stash);

        assert!(buffer.is_empty());
        assert_eq!(cursor, 0);
        assert_eq!(index, None, "the recall walk ends with the clear");
        assert_eq!(
            stash, "what i actually typed",
            "the recalled line must not replace the draft it was covering"
        );
    }

    #[test]
    fn composer_escape_stashes_the_draft_when_no_recall_is_running() {
        let mut buffer = "typed but not sent".to_string();
        let mut cursor = buffer.len();
        let mut index = None;
        let mut stash = String::new();

        escape_clears_input(&mut buffer, &mut cursor, &mut index, &mut stash);

        assert!(buffer.is_empty());
        assert_eq!(stash, "typed but not sent");
        assert_eq!(index, None);
    }

    #[test]
    fn composer_history_file_round_trips_and_dedupes_consecutive_entries() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("history.txt");

        append_history(&path, "first").expect("append");
        append_history(&path, "second").expect("append");
        append_history(&path, "second").expect("duplicate is not recorded");

        assert_eq!(load_history(&path), vec!["first", "second"]);
    }

    #[test]
    fn composer_history_file_is_capped_at_the_newest_entries() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("history.txt");
        let entries: Vec<String> = (0..HISTORY_CAP + 12).map(|n| format!("m{n}")).collect();
        save_history(&path, &entries).expect("save");

        let loaded = load_history(&path);
        assert_eq!(loaded.len(), HISTORY_CAP);
        assert_eq!(loaded.first().map(String::as_str), Some("m12"));
        assert_eq!(loaded.last().map(String::as_str), Some("m511"));
    }

    #[test]
    fn composer_shared_history_is_one_list_for_every_frontend() {
        let history = new_shared_history(vec!["old".to_string()]);
        let window = history.clone();
        {
            let mut entries = lock_history(&window);
            record_input_history(&mut entries, "from the window");
        }
        let pane = history.clone();
        let entries = lock_history(&pane);
        assert_eq!(
            entries.as_slice(),
            ["old".to_string(), "from the window".to_string()],
            "a pane must recall what the window just sent"
        );
    }
}
