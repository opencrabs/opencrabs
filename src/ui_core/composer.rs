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

/// The first line of the history file, saying which format the rest is in.
///
/// The file used to hold one entry per line, and a line cannot hold a
/// newline: a pasted log was torn into one entry per line and came back as a
/// list of fragments, some of them a line of their own and some of them
/// refusing to move at all (#3187). One entry is one record now, with the
/// newlines inside it escaped, and this marker is how a reader tells the two
/// formats apart.
pub const HISTORY_FORMAT_MARKER: &str = "# opencrabs-history v2";

/// One entry as one record: the newlines inside it are escaped.
///
/// The backslash goes first, so the escape stays unambiguous and an entry
/// holding the two characters `\n` comes back as the two characters it was.
fn encode_history_record(entry: &str) -> String {
    let mut record = String::with_capacity(entry.len());
    for ch in entry.chars() {
        match ch {
            '\\' => record.push_str("\\\\"),
            '\n' => record.push_str("\\n"),
            '\r' => record.push_str("\\r"),
            _ => record.push(ch),
        }
    }
    record
}

/// The entry a record holds, newlines and all.
///
/// A backslash that starts nothing the format knows is data, not an escape:
/// it came from a hand edit or from a file written before the rules, and it
/// belongs in the entry as it is.
fn decode_history_record(record: &str) -> String {
    let mut entry = String::with_capacity(record.len());
    let mut chars = record.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            entry.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => entry.push('\\'),
            Some('n') => entry.push('\n'),
            Some('r') => entry.push('\r'),
            Some(other) => {
                entry.push('\\');
                entry.push(other);
            }
            None => entry.push('\\'),
        }
    }
    entry
}

/// Read the submitted history, oldest first, one entry per record.
///
/// A file without the marker is the old format: one entry per line. Its
/// entries are imported as they stand, because a file that once held a pasted
/// log has already lost where that log began and nothing can put it back
/// (#3187). The next save rewrites the file in the current format.
pub fn load_history(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    let entries: Vec<String> = if first.trim_end() == HISTORY_FORMAT_MARKER {
        lines
            .filter(|record| !record.is_empty())
            .map(decode_history_record)
            .collect()
    } else {
        std::iter::once(first)
            .chain(lines)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    };
    // The cap counts entries, so a file that grew past it drops its oldest
    // ones here as well as on the way out.
    let start = entries.len().saturating_sub(HISTORY_CAP);
    entries[start..].to_vec()
}

/// Rewrite the file from `entries`, keeping only the newest [`HISTORY_CAP`].
///
/// The file is written in the current format: the marker, then one record per
/// entry, with the newlines inside an entry escaped (#3187).
pub fn save_history(path: &Path, entries: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let start = entries.len().saturating_sub(HISTORY_CAP);
    let mut body = String::from(HISTORY_FORMAT_MARKER);
    body.push('\n');
    for entry in &entries[start..] {
        body.push_str(&encode_history_record(entry));
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
    fn composer_history_file_keeps_a_pasted_block_as_one_entry() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("history.txt");
        let pasted = "use std::io;\n\nfn main() {\n    println!(\"hi\");\n}";

        append_history(&path, pasted).expect("append");

        assert_eq!(
            load_history(&path),
            vec![pasted.to_string()],
            "a pasted block is one entry, newlines and blank lines included"
        );
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(
            raw.starts_with(HISTORY_FORMAT_MARKER),
            "the file says which format it is in"
        );
        assert_eq!(
            raw.lines().count(),
            2,
            "the marker and one record: the newlines live inside the record"
        );
    }

    #[test]
    fn composer_history_file_keeps_a_literal_backslash_n_as_two_characters() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("history.txt");
        let entry = "printf 'a\\nb'";

        append_history(&path, entry).expect("append");

        assert_eq!(
            load_history(&path),
            vec![entry.to_string()],
            "an escaped newline in the text stays the text it was"
        );
    }

    #[test]
    fn composer_history_file_imports_the_legacy_format_and_rewrites_it() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("history.txt");
        // The old file held one entry per line, so a pasted log in it is
        // already torn apart and nothing can put it back together.
        std::fs::write(&path, "first\nsecond\n").expect("write the old format");

        assert_eq!(
            load_history(&path),
            vec!["first".to_string(), "second".to_string()],
            "the old file is imported one entry per line"
        );

        append_history(&path, "third").expect("append");

        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(
            raw.starts_with(HISTORY_FORMAT_MARKER),
            "the next save rewrites the file in the current format"
        );
        assert_eq!(
            load_history(&path),
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ],
            "the imported entries survive the rewrite"
        );
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
