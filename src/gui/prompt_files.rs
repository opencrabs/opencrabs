//! The markdown file behind a folded prompt (#3064).
//!
//! A prompt past the fold bound is drawn as its opening lines plus a card that
//! names a `.md`. That name is not decoration: the whole message is written to
//! the conversation's project files, in the same tree that already holds the
//! plans, audits and scrapes a turn produces, so the text has a home the reader
//! and the agent can both open later. The transcript row keeps the body for the
//! ledger; the file keeps a copy a person can hand to anything else.
//!
//! Written when a window installs its rows, never while drawing: a frame must
//! not touch the disk, and the same rows are loaded again on every reopen. The
//! write is skipped when the file already holds the same number of bytes, so a
//! reopen costs one metadata call per folded prompt and nothing else.

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use opencrabs::backend::BackendError;
use opencrabs::backend::prompt_files::PromptFilePersist;
use opencrabs::db::models::Message;
use opencrabs::ui_core::message_reader;

/// One folded prompt, ready to be written. The backend owns the type because
/// the documents cross to the crab that writes them (`PromptFilePersist`).
pub use opencrabs::backend::prompt_files::PromptDocument;
/// The naming and the same-size write, now in the backend, named here for the
/// window's tests that pin them.
#[cfg(test)]
pub use opencrabs::backend::prompt_files::{file_name, write};

/// The documents among the rows a window just loaded (#3064).
///
/// Only a user row that folds has one: an answer's own overflow is bounded by
/// the working-out fold, which expands in place and carries no file.
pub fn documents(rows: &[Message]) -> Vec<PromptDocument> {
    rows.iter()
        .filter(|row| row.role == "user")
        .filter(|row| message_reader::opens_in_reader(&row.content))
        .map(|row| PromptDocument {
            session: row.session_id,
            id: row.id,
            name: message_reader::document_name(&row.content),
            content: row.content.clone(),
        })
        .collect()
}

/// Write every document, awaited from the window: the work runs on the
/// backend's worker lane (`PromptFilePersist`), whose thread has a real stack
/// and builds its runtime before the job is sent. A database pool built on a
/// gpui worker is what killed the process before (#3053's crash reports), so
/// the connect happens there and the window only waits. A failure is logged
/// and the window keeps the rows without their files.
pub async fn persist_off_thread(
    backend: crate::backend_glue::Backend,
    docs: Vec<PromptDocument>,
) -> HashMap<Uuid, PathBuf> {
    if docs.is_empty() {
        return HashMap::new();
    }
    match backend.call(PromptFilePersist { docs }).await {
        Ok(paths) => paths,
        Err(e) => {
            crate::logging::error(&BackendError::into_message(e));
            HashMap::new()
        }
    }
}
