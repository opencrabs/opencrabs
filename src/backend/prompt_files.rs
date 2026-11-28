//! Prompt files: where they live and persisting them.
//!
//! A prompt past the fold bound is drawn as its opening lines plus a card that
//! names a `.md` (#3064). The whole message is written to the conversation's
//! project files, in the same tree that already holds the plans, audits and
//! scrapes a turn produces, so the text has a home the reader and the agent
//! can both open later. The write lives on the crab that owns that tree, which
//! is why it is an operation and not a window helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Pool;
use crate::services::{FileService, ProjectService, ServiceContext};

use super::{BackendError, Ctx, Op, Scope};

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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptDocument {
    /// The conversation the row belongs to, which is what decides the project.
    pub session: Uuid,
    /// The row's id, the part of the file name that cannot collide.
    pub id: Uuid,
    /// The name the card prints, already ending in `.md`.
    pub name: String,
    pub content: String,
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
    let files = service.project_files_dir(session).await.unwrap_or_else(|| {
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
///
/// The name crosses the wire, so it is checked before it touches a path: a
/// name that is not one plain file name (a separator, `..`, a leading dot) is
/// refused and nothing is written. Every name `message_reader::document_name`
/// produces passes, so the window's own documents are never refused.
pub fn write(dir: &Path, doc: &PromptDocument) -> Option<PathBuf> {
    if !plain_name(&doc.name) {
        return None;
    }
    let path = dir.join(file_name(doc));
    let same = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == doc.content.len() as u64);
    if same {
        return Some(path);
    }
    std::fs::write(&path, doc.content.as_bytes()).ok()?;
    Some(path)
}

/// Whether `name` is one plain file name that stays inside the directory it
/// is joined to.
fn plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && parts.next().is_none()
        && !name.contains(['/', '\\'])
        && !name.starts_with('.')
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

/// The directory a conversation's prompt documents go to, created on demand
/// ([`directory`]). `None` when it could not be created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFilesDir {
    /// The conversation whose project decides the directory.
    pub session: Uuid,
}

impl Op for PromptFilesDir {
    const NAME: &'static str = "prompt_files.dir";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<PathBuf>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        Ok(directory(db.pool(), self.session).await)
    }
}

/// Write the folded prompts a window just loaded ([`persist`]), answering
/// where each one landed, keyed by row id. A document whose directory or file
/// could not be written is left out of the answer.
///
/// A failed connect keeps the `prompt docs db: {e}` text the window logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFilePersist {
    /// The documents to write.
    pub docs: Vec<PromptDocument>,
}

impl Op for PromptFilePersist {
    const NAME: &'static str = "prompt_files.persist";
    const SCOPE: Scope = Scope::Manage;
    type Reply = HashMap<Uuid, PathBuf>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("prompt docs db: {e}")))?;
        Ok(persist(db.pool(), &self.docs).await)
    }
}

ops! { PromptFilesDir, PromptFilePersist }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::Session;
    use crate::db::repository::SessionRepository;

    async fn unfiled_session(gateway: &Gateway) -> Uuid {
        let db = gateway.ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let session = Session::new(Some("chat".into()), None, None);
        repo.create(&session).await.expect("create");
        session.id
    }

    #[test]
    fn a_name_that_leaves_the_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inner = dir.path().join("prompts");
        std::fs::create_dir_all(&inner).expect("prompts dir");
        for name in ["../escape.md", "a/b.md", "a\\b.md", "/abs.md", "..", ".hidden.md", ""] {
            let doc = PromptDocument {
                session: Uuid::new_v4(),
                id: Uuid::new_v4(),
                name: name.into(),
                content: "body".into(),
            };
            assert_eq!(write(&inner, &doc), None, "{name:?} was written");
        }
        let entries = std::fs::read_dir(&inner).expect("list").count()
            + std::fs::read_dir(dir.path()).expect("list").count();
        assert_eq!(entries, 1, "only the prompts dir itself exists");
        let ok = PromptDocument {
            session: Uuid::new_v4(),
            id: Uuid::new_v4(),
            name: crate::ui_core::message_reader::document_name("Deploy log: v2"),
            content: "body".into(),
        };
        assert!(write(&inner, &ok).is_some(), "a window name is written");
    }

    #[tokio::test]
    async fn prompt_files_dir_has_parity() {
        let gateway = Gateway::start().await;
        let session = unfiled_session(&gateway).await;

        let answer = parity!(gateway, PromptFilesDir { session });
        let dir = PathBuf::from(answer["Ok"].as_str().expect("a directory"));
        assert!(dir.starts_with(gateway.home()), "{}", dir.display());
        assert!(dir.ends_with(Path::new(UNFILED_SLUG).join("files").join(PROMPTS_DIR)));
        assert!(dir.is_dir());

        forbidden!(gateway, PromptFilesDir { session });
    }

    #[tokio::test]
    async fn prompt_file_persist_has_parity() {
        let gateway = Gateway::start().await;
        let session = unfiled_session(&gateway).await;
        let doc = PromptDocument {
            session,
            id: Uuid::new_v4(),
            name: "deploy-log.md".into(),
            content: "the whole body".into(),
        };

        // Run twice by `parity!`: the second write finds the same size and
        // answers the same path, which is what lets the doors agree.
        let answer = parity!(
            gateway,
            PromptFilePersist {
                docs: vec![doc.clone()],
            }
        );
        let path = answer["Ok"][doc.id.to_string()]
            .as_str()
            .map(PathBuf::from)
            .expect("the document landed");
        assert!(path.starts_with(gateway.home()), "{}", path.display());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            doc.content
        );

        let refused = PromptDocument {
            id: Uuid::new_v4(),
            ..doc
        };
        forbidden!(
            gateway,
            PromptFilePersist {
                docs: vec![refused.clone()],
            }
        );
        assert!(
            !path.with_file_name(file_name(&refused)).exists(),
            "a refused persist wrote nothing"
        );
    }
}
