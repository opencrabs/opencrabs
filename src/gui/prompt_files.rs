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
use std::path::{Path, PathBuf};

use uuid::Uuid;

use opencrabs::db::Pool;
use opencrabs::db::models::Message;
use opencrabs::services::{FileService, ProjectService, ServiceContext};
use opencrabs::ui_core::message_reader;

/// Directory inside a project's files that holds the prompt documents.
const PROMPTS_DIR: &str = "prompts";

/// Where a conversation's prompt documents live when it belongs to no project.
///
/// The projects directory is the one place the agent looks for what a turn
/// produced, so a prompt from an unfiled conversation goes there too instead of
/// nowhere. It is a slug, so it sorts beside the real projects and never lands
/// under one of their names.
const UNFILED_SLUG: &str = "unfiled";

/// One folded prompt, ready to be written.
#[derive(Clone, Debug)]
pub struct PromptDocument {
    /// The conversation the row belongs to, which is what decides the project.
    pub session: Uuid,
    /// The row's id, the part of the file name that cannot collide.
    pub id: Uuid,
    /// The name the card prints, already ending in `.md`.
    pub name: String,
    pub content: String,
}

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

/// Where a conversation's prompt documents live, created on demand.
///
/// The project's own `files/` directory, the one attachments and scrapes
/// already land in, with the documents grouped under `prompts/` so a listing of
/// a project's artifacts is not a wall of pasted logs. A conversation filed
/// under no project still gets a directory, because the card prints a path: the
/// reader must never open a name that was never written.
pub async fn directory(pool: &Pool, session: Uuid) -> Option<PathBuf> {
    let service = FileService::new(ServiceContext::new(pool.clone()));
    let files = service
        .project_files_dir(session)
        .await
        .unwrap_or_else(|| {
            ProjectService::projects_dir()
                .join(UNFILED_SLUG)
                .join("files")
        });
    let dir = files.join(PROMPTS_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The file one document takes: the row id first, so two prompts that open with
/// the same words still hold one file each, then the name the card prints so a
/// directory listing reads like the messages it holds.
pub fn file_name(doc: &PromptDocument) -> String {
    format!("{}-{}", doc.id, doc.name)
}

/// Write one document, unless the same number of bytes is already there.
///
/// The content of a row never changes, so a same-size file is the same file: a
/// byte compare would re-read every prompt on every reopen to learn nothing.
/// A truncated write leaves a different size behind and is rewritten next time,
/// which is the one failure this cheap check cannot hide.
pub fn write(dir: &Path, doc: &PromptDocument) -> Option<PathBuf> {
    let path = dir.join(file_name(doc));
    let same = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == doc.content.len() as u64);
    if same {
        return Some(path);
    }
    std::fs::write(&path, doc.content.as_bytes()).ok()?;
    Some(path)
}

/// Write every document, returning where each one landed, keyed by row id.
pub async fn persist(pool: &Pool, docs: &[PromptDocument]) -> HashMap<Uuid, PathBuf> {
    let mut dirs: HashMap<Uuid, Option<PathBuf>> = HashMap::new();
    let mut written = HashMap::new();
    for doc in docs {
        let dir = match dirs.get(&doc.session) {
            Some(dir) => dir.clone(),
            None => {
                let dir = directory(pool, doc.session).await;
                dirs.insert(doc.session, dir.clone());
                dir
            }
        };
        let Some(dir) = dir else { continue };
        if let Some(path) = write(&dir, doc) {
            written.insert(doc.id, path);
        }
    }
    written
}

/// The same write, awaited from the window: its own thread with a real stack
/// and its own runtime, the shape every load in the window uses. A database
/// pool built on a gpui worker is what killed the process before (#3053's
/// crash reports), so the connect happens here and the window only waits.
pub async fn persist_off_thread(
    db_path: PathBuf,
    docs: Vec<PromptDocument>,
) -> HashMap<Uuid, PathBuf> {
    if docs.is_empty() {
        return HashMap::new();
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("prompt-docs".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let paths = match runtime {
                Ok(rt) => rt.block_on(async move {
                    let db = match opencrabs::db::Database::connect(&db_path).await {
                        Ok(db) => db,
                        Err(e) => {
                            crate::logging::error(&format!("prompt docs db: {e}"));
                            return HashMap::new();
                        }
                    };
                    persist(db.pool(), &docs).await
                }),
                Err(e) => {
                    crate::logging::error(&format!("prompt docs runtime: {e}"));
                    HashMap::new()
                }
            };
            let _ = tx.send(paths);
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("prompt docs thread: {e}"));
        return HashMap::new();
    }
    rx.await.unwrap_or_default()
}
