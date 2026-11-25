//! The window's handle on a crab.
//!
//! The window never opens the ledger or holds the agent service by itself:
//! it keeps a [`Backend`] and asks it. Either this process's own crab, or
//! another one over its gateway (`backend_remote.rs`). The callers do not
//! change between the two, because both answer the same [`Op`] values.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::future::LocalBoxFuture;
use opencrabs::backend::blob::{self, BlobPut};
use opencrabs::backend::cap::{self, Cap, Unavailable};
use opencrabs::backend::sessions::IconFromUpload;

use opencrabs::backend::tasks::{SessionTasks, TasksSnapshot};
use opencrabs::backend::{BackendError, Ctx, Facts, Op};
use opencrabs::remote::instance_store::KnownInstance;
use uuid::Uuid;

use crate::agent_bridge::EventSink;

pub use crate::backend_remote::{RemoteCrab, Watch};

/// Which crab the window talks to.
#[derive(Clone)]
pub enum Backend {
    /// This process's own crab.
    Local(Arc<Ctx>),
    /// Another crab, reached through its gateway.
    Remote(Arc<RemoteCrab>),
}

impl Backend {
    /// A local backend on the ledger at `db_path`, with no service yet and
    /// not recorded as the primary: the helper tests reach a ledger through.
    #[cfg(test)]
    pub fn local(db_path: std::path::PathBuf) -> Self {
        Backend::Local(Arc::new(Ctx::new(db_path)))
    }

    /// Another crab, from the record its pairing left in the instance store.
    ///
    /// `home` is the backend the window started with: the new one keeps that
    /// crab's ledger as the window's own, so what belongs to the window
    /// (dismissed hints) does not change with the crab it looks at.
    pub fn remote(known: &KnownInstance, home: &Backend) -> Self {
        let window_ledger = match home {
            Backend::Local(ctx) => ctx.db_path().to_path_buf(),
            Backend::Remote(crab) => crab.window_ledger().to_path_buf(),
        };
        Backend::Remote(Arc::new(
            RemoteCrab::new(
                known.label.clone(),
                known.url.clone(),
                Some(known.token.clone()),
                window_ledger,
            )
            .with_id(known.host_id.clone()),
        ))
    }

    /// Run `op` and wait for its reply.
    ///
    /// Locally the op runs on its own lane of `spawn_worker`, named after the
    /// op: a 4 MB stack and a `current_thread` runtime built before the job
    /// is sent, so every ledger connect the op makes has a reactor (#2973,
    /// #3033). Remotely it is one `gui/call` on the transport's runtime. The
    /// window only awaits a oneshot, which needs no reactor.
    pub async fn call<O: Op>(&self, op: O) -> Result<O::Reply, BackendError> {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.spawn(op, move |reply| {
            let _ = tx.send(reply);
        })
        .map_err(BackendError::Failed)?;
        rx.await
            .map_err(|_| BackendError::Failed(format!("{} worker dropped its result", O::NAME)))?
    }

    /// Run `op` off the window's thread and hand its reply to `done`, on the
    /// same lane `call` uses. For the `spawn_*` doors that report through the
    /// event sink instead of awaiting. `Err` means the job never reached a
    /// worker, so `done` will not run: the caller reports that itself, as it
    /// did for a failed thread spawn.
    pub fn spawn<O: Op>(
        &self,
        op: O,
        done: impl FnOnce(Result<O::Reply, BackendError>) + Send + 'static,
    ) -> Result<(), String> {
        match self {
            Backend::Local(ctx) => {
                let ctx = ctx.clone();
                crate::agent_bridge::spawn_worker(O::NAME, move |runtime| {
                    done(runtime.block_on(op.run(&ctx)));
                })
            }
            Backend::Remote(crab) => crate::backend_remote::spawn(crab.clone(), op, done),
        }
    }

    /// Why this backend cannot do `cap`, or `None` when it can.
    ///
    /// Decided by which crab the window holds, never by the handshake: a
    /// remote crab lacks a local-only capability before its `hello` lands,
    /// so an action that would act on this machine does not fail open.
    pub fn lacks(&self, cap: Cap) -> Option<String> {
        match self {
            Backend::Local(_) => None,
            Backend::Remote(_) if cap.available(false) => None,
            Backend::Remote(_) => Some(Unavailable::LocalOnly(cap).reason()),
        }
    }

    /// The per-frame facts for `session`, or `None` while the crab is still
    /// booting. Synchronous: the window reads it while it renders.
    #[allow(dead_code)] // no window caller yet: the footer still reads the service
    pub fn facts(&self, session: Option<Uuid>) -> Option<Facts> {
        match self {
            Backend::Local(ctx) => opencrabs::backend::facts_for(ctx, session).ok(),
            // The window reads facts every frame and a remote read is a round
            // trip, so a remote crab answers from its cache. Facts never read
            // yet read as "still booting".
            Backend::Remote(crab) => crab.facts(session),
        }
    }

    /// What is running in each of `sessions`, for the frame being drawn.
    /// Synchronous, like [`facts`](Self::facts): a local crab answers from
    /// its task manager, a remote one from its cache, never over the wire.
    /// A crab still booting, or one with no task manager, runs nothing.
    pub fn tasks_snapshot(&self, sessions: Vec<Uuid>) -> Vec<SessionTasks> {
        match self {
            Backend::Local(ctx) => TasksSnapshot { sessions }.answer(ctx).unwrap_or_default(),
            Backend::Remote(crab) => crab.tasks_snapshot(&sessions),
        }
    }

    /// The copy on this machine of the crab's file at `path`, or `None` when
    /// there is none yet. Synchronous and never fetches: the window asks
    /// while it renders. The local crab's files are on this disk, so it
    /// answers `path` itself and never touches the blob cache.
    pub fn cached_blob(&self, path: &Path) -> Option<PathBuf> {
        match self {
            Backend::Local(_) => Some(path.to_path_buf()),
            Backend::Remote(crab) => crab.cached_blob(path),
        }
    }

    /// Ask for a copy of the crab's file at `path` without waiting for it,
    /// and answer where it will be. A remote crab fetches it off the frame
    /// and repaints the window when it lands; the local crab fetches
    /// nothing and answers `path`.
    pub fn request_blob(&self, path: &Path) -> PathBuf {
        match self {
            Backend::Local(_) => path.to_path_buf(),
            Backend::Remote(crab) => crate::backend_remote::request_blob(crab, path),
        }
    }

    /// Keep a picked icon image (`filename`, `bytes`) on the crab, and
    /// answer the value a topic stores for it (`img:<file>`).
    ///
    /// The local crab's icon store is on this disk: `write_here` writes the
    /// file there and the value is ready at once, as the picker always did.
    /// A crab on another machine is sent the bytes by `blob.put` and keeps
    /// its upload as an icon (`sessions.icon_from_upload`); that is two
    /// round trips, so the value comes [`Kept::Later`].
    pub fn keep_icon_image(
        &self,
        filename: &str,
        bytes: &[u8],
        write_here: impl FnOnce(&str, &[u8]) -> Result<String, String>,
    ) -> Kept {
        match self {
            Backend::Local(_) => Kept::Now(write_here(filename, bytes)),
            Backend::Remote(_) => {
                let backend = self.clone();
                let put = BlobPut {
                    name: filename.to_string(),
                    base64: BASE64.encode(bytes),
                };
                Kept::Later(Box::pin(async move {
                    let upload = backend
                        .call(put)
                        .await
                        .map_err(BackendError::into_message)?;
                    backend
                        .call(IconFromUpload {
                            upload: upload.path,
                        })
                        .await
                        .map_err(BackendError::into_message)
                }))
            }
        }
    }

    /// The message `text` as the crab will read it, with `files` sent along.
    ///
    /// `files` are the attachments the composer holds for this message (the
    /// chips and a pasted image), never paths read out of `text`: a quoted
    /// reply comes from the crab, and a marker planted in it must not make
    /// this machine read and upload the file it names.
    ///
    /// The local crab reads this disk itself, so `text` comes back as it is,
    /// on the first poll. A crab elsewhere gets each file by `blob.put`, and
    /// its attachment marker (`<<IMG:..>>`, `<<VID:..>>`, `<<AUD:..>>` with
    /// the local path) is swapped for the same marker on the crab's copy;
    /// nothing else in `text` changes. Every file is read before the first
    /// upload, off the window's thread, so one that cannot be read or is
    /// over [`MAX_BLOB_BYTES`](blob::MAX_BLOB_BYTES) refuses the whole
    /// message and nothing is sent.
    pub fn carry(
        &self,
        text: String,
        files: Vec<PathBuf>,
    ) -> LocalBoxFuture<'static, Result<String, String>> {
        match self {
            Backend::Local(_) => Box::pin(async move { Ok::<_, String>(text) }),
            Backend::Remote(_) => {
                let backend = self.clone();
                Box::pin(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::Builder::new()
                        .name("gui-carry".to_string())
                        .spawn(move || {
                            let _ = tx.send(read_attachments(&files));
                        })
                        .map_err(|e| format!("attachments: {e}"))?;
                    let read = rx
                        .await
                        .map_err(|_| "attachments: the reader stopped".to_string())??;
                    let mut text = text;
                    for (local, put) in read {
                        let kept = backend
                            .call(put)
                            .await
                            .map_err(|e| cap::notice(<BlobPut as Op>::NAME, &e))?;
                        text = swap_marker(&text, &local, &kept.path);
                    }
                    Ok::<_, String>(text)
                })
            }
        }
    }

    /// Feed `session`'s events into `sink` until the returned [`Watch`]
    /// drops. A local crab has nothing to open: its agent service already
    /// forwards every event into the window's sink, so it answers `None`.
    /// `Err` means the reader's thread never started.
    #[allow(dead_code)] // the instance rail (C3) opens one with a remote session
    pub fn watch(&self, session: Uuid, sink: EventSink) -> Result<Option<Watch>, String> {
        match self {
            Backend::Local(_) => Ok(None),
            Backend::Remote(crab) => {
                crate::backend_remote::watch(crab.clone(), session, sink).map(Some)
            }
        }
    }
}

/// Every file of one message as the upload that carries it, or why the
/// message cannot go. Read through the crab's own blob reader, rooted at the
/// file's real folder: the same regular-file check and the same
/// [`MAX_BLOB_BYTES`](blob::MAX_BLOB_BYTES) ceiling the crab applies to what
/// it serves.
fn read_attachments(files: &[PathBuf]) -> Result<Vec<(PathBuf, BlobPut)>, String> {
    let mut read = Vec::with_capacity(files.len());
    for local in files {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| local.display().to_string());
        let refuse = |why: String| format!("{name}: {why}; nothing was sent");
        let real = local.canonicalize().map_err(|e| refuse(e.to_string()))?;
        let (Some(folder), Some(requested)) = (real.parent(), real.to_str()) else {
            return Err(refuse("not a file this window can send".to_string()));
        };
        let bytes =
            blob::read(&[folder.to_path_buf()], requested).map_err(|e| refuse(e.to_string()))?;
        read.push((
            local.clone(),
            BlobPut {
                name: name.clone(),
                base64: BASE64.encode(bytes),
            },
        ));
    }
    Ok(read)
}

/// `text` with the attachment marker for `local` pointing at `crab` instead.
///
/// The composer appends a message's markers after its words, so the last
/// marker naming `local` is the one this send added; an earlier one, in a
/// quoted reply, is the crab's own text and stays as it was.
pub(crate) fn swap_marker(text: &str, local: &Path, crab: &str) -> String {
    let local = local.display().to_string();
    let found = ["IMG", "VID", "AUD"]
        .into_iter()
        .filter_map(|kind| {
            let marker = format!("<<{kind}:{local}>>");
            text.rfind(&marker).map(|at| (at, kind, marker.len()))
        })
        .max_by_key(|(at, _, _)| *at);
    let Some((at, kind, len)) = found else {
        return text.to_string();
    };
    let (Some(before), Some(after)) = (text.get(..at), text.get(at + len..)) else {
        return text.to_string();
    };
    format!("{before}<<{kind}:{crab}>>{after}")
}

/// Where [`Backend::keep_icon_image`] left a picked icon.
pub enum Kept {
    /// Written on this disk: the stored value, or why it failed.
    Now(Result<String, String>),
    /// Sent to the crab: the stored value once its replies land.
    Later(LocalBoxFuture<'static, Result<String, String>>),
}
