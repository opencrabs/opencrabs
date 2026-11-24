//! Files an operation points at instead of carrying.
//!
//! An icon, an attachment, a plan file or a spoken reply lives on the crab's
//! disk. An operation answers with a [`BlobRef`]; a local surface opens the
//! path, a remote one fetches the bytes once and caches them.
//!
//! A remote caller names a path, so this file is the guard between that
//! name and the crab's disk. A path is served only when it resolves, after
//! every `..` and every symlink, inside one of [`ROOTS`] under the crab's
//! home, is a regular file, and fits [`MAX_BLOB_BYTES`]. Anything else is
//! [`BlobError::Denied`], which the HTTP door answers as 403 whether or not
//! the file exists, so a refused caller learns nothing about the disk
//! outside the roots.
//!
//! Uploads (`BlobPut`) land in [`UPLOAD_DIR`], named by content, so the same
//! bytes sent twice are one file and the answer repeats.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BackendError, Ctx, Op, Scope};

/// A file on the crab, named so either side can find it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobRef {
    /// Where the file lives on the crab.
    pub path: String,
    /// Its size in bytes, when the crab knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl BlobRef {
    /// A reference to `path` with no size known.
    pub fn at(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: None,
        }
    }
}

/// The largest file served or accepted: the Telegram document ceiling
/// (`utils::image::TELEGRAM_DOCUMENT_MAX_BYTES`), so any attachment a channel
/// could have delivered to the crab can also be fetched from it.
pub const MAX_BLOB_BYTES: u64 = 50 * 1024 * 1024;

/// The directories under the crab's home a blob may come from.
///
/// | Root | What lives there |
/// |---|---|
/// | `session` | session files and the unbound plan files (`plan_files::session_dir`) |
/// | `projects` | each project's files and its sessions' plan files |
/// | `agents/session` | plan files written before plans became project aware |
/// | `plans` | the plan tool's reference files |
/// | `tmp` | channel attachments (Telegram, WhatsApp, clipboard) and uploads |
/// | `gui-icons` | custom topic icons (`topic_icons::image_store_dir`) |
pub const ROOTS: &[&str] = &[
    "session",
    "projects",
    "agents/session",
    "plans",
    "tmp",
    "gui-icons",
];

/// Where uploaded attachments land, under the crab's home. Inside `tmp`, the
/// convention for incoming files, so an upload is served back like any other
/// channel attachment and is cleaned up with them.
pub const UPLOAD_DIR: &str = "tmp/gui-uploads";

/// Why a blob was not served or stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    /// The path is relative, climbs out of every root, resolves outside them
    /// through a symlink, or is not a regular file.
    #[error("blob: path not allowed")]
    Denied,
    /// Inside a root, but nothing is there.
    #[error("blob: not found")]
    NotFound,
    /// Bigger than [`MAX_BLOB_BYTES`].
    #[error("blob: {size} bytes is over the 50 MiB limit")]
    TooLarge { size: u64 },
    /// The upload was not valid base64.
    #[error("blob: bad base64: {0}")]
    BadEncoding(String),
    /// The disk failed under an allowed path.
    #[error("blob: {0}")]
    Io(String),
}

impl BlobError {
    /// The HTTP status the `gui/blob` door answers with.
    pub fn status(&self) -> u16 {
        match self {
            BlobError::Denied => 403,
            BlobError::NotFound => 404,
            BlobError::TooLarge { .. } => 413,
            BlobError::BadEncoding(_) => 400,
            BlobError::Io(_) => 500,
        }
    }
}

impl From<BlobError> for BackendError {
    fn from(e: BlobError) -> Self {
        match e {
            BlobError::BadEncoding(_) => BackendError::BadParams(e.to_string()),
            other => BackendError::Failed(other.to_string()),
        }
    }
}

/// The home a crab's blobs live under: the one it was pinned to, otherwise
/// the profile home.
pub fn home_of(ctx: &Ctx) -> PathBuf {
    ctx.home()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::opencrabs_home)
}

/// The allowed roots under `home`, as written (not canonicalized: a root that
/// does not exist yet is still a root).
pub fn roots(home: &Path) -> Vec<PathBuf> {
    ROOTS.iter().map(|root| home.join(root)).collect()
}

/// `path` with `.` and `..` folded away without touching the disk, or `None`
/// when it is relative or climbs above `/`.
fn normalize(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_none() {
                    return None;
                }
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

/// Whether `path` is strictly inside one of `roots`, component by component
/// (`/home/tmpfoo` is not inside `/home/tmp`, and a root is not inside
/// itself).
fn inside(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path != root.as_path() && path.starts_with(root))
}

/// The real file `requested` names, when it is allowed.
///
/// Two checks, both required. Lexical first: the path, with `..` folded,
/// must sit inside a root, so a path aimed outside is refused before the disk
/// is asked whether it exists. Then canonical: every symlink resolved, the
/// file must still sit inside a canonical root, so a link planted inside a
/// root cannot lead out of it.
pub fn resolve(roots: &[PathBuf], requested: &str) -> Result<PathBuf, BlobError> {
    let requested = Path::new(requested);
    let lexical = normalize(requested).ok_or(BlobError::Denied)?;
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    if !inside(&lexical, roots) && !inside(&lexical, &canonical_roots) {
        return Err(BlobError::Denied);
    }
    let real = match lexical.canonicalize() {
        Ok(real) => real,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(BlobError::NotFound),
        Err(_) => return Err(BlobError::Denied),
    };
    if !inside(&real, &canonical_roots) {
        return Err(BlobError::Denied);
    }
    Ok(real)
}

/// The bytes of the file `requested` names, through [`resolve`].
///
/// The type and size are read from the open handle, not the path, so a file
/// swapped between the check and the read is still the file that was checked.
pub fn read(roots: &[PathBuf], requested: &str) -> Result<Vec<u8>, BlobError> {
    let real = resolve(roots, requested)?;
    let file = File::open(&real).map_err(|e| match e.kind() {
        ErrorKind::NotFound => BlobError::NotFound,
        _ => BlobError::Io(e.to_string()),
    })?;
    let meta = file.metadata().map_err(|e| BlobError::Io(e.to_string()))?;
    if !meta.is_file() {
        return Err(BlobError::Denied);
    }
    if meta.len() > MAX_BLOB_BYTES {
        return Err(BlobError::TooLarge { size: meta.len() });
    }
    let mut bytes = Vec::new();
    file.take(MAX_BLOB_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| BlobError::Io(e.to_string()))?;
    let size = bytes.len() as u64;
    if size > MAX_BLOB_BYTES {
        return Err(BlobError::TooLarge { size });
    }
    Ok(bytes)
}

/// A file name a caller offered, cut to something safe to put on disk: the
/// last path component only, `[A-Za-z0-9._-]` kept and anything else turned
/// into `_`, no leading dot, at most 96 characters, `blob` when nothing is
/// left.
fn safe_name(offered: &str) -> String {
    let last = offered.rsplit(['/', '\\']).next().unwrap_or_default();
    let kept: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed: String = kept.trim_start_matches('.').chars().take(96).collect();
    if trimmed.is_empty() {
        "blob".to_string()
    } else {
        trimmed
    }
}

/// Store an uploaded attachment under `home`'s [`UPLOAD_DIR`] and name it.
///
/// The file is `<first 16 hex of sha256>-<safe name>`, so the same bytes
/// under the same name are the same file: a second upload finds it there and
/// answers the same reference.
pub fn store_upload(home: &Path, name: &str, bytes: &[u8]) -> Result<BlobRef, BlobError> {
    let size = bytes.len() as u64;
    if size > MAX_BLOB_BYTES {
        return Err(BlobError::TooLarge { size });
    }
    let dir = home.join(UPLOAD_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| BlobError::Io(e.to_string()))?;
    let digest: String = format!("{:x}", Sha256::digest(bytes))
        .chars()
        .take(16)
        .collect();
    let target = dir.join(format!("{digest}-{}", safe_name(name)));
    match OpenOptions::new().write(true).create_new(true).open(&target) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                let _ = std::fs::remove_file(&target);
                return Err(BlobError::Io(e.to_string()));
            }
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(BlobError::Io(e.to_string())),
    }
    let real = target
        .canonicalize()
        .map_err(|e| BlobError::Io(e.to_string()))?;
    Ok(BlobRef {
        path: real.to_string_lossy().into_owned(),
        size: Some(size),
    })
}

/// Run blocking disk work off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, BlobError> + Send + 'static,
) -> Result<T, BackendError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| BackendError::Failed(format!("blob: worker: {e}")))?
        .map_err(BackendError::from)
}

/// The bytes of a file on the crab, base64 in the reply.
///
/// The JSON door for small files. A surface that fetches many or large blobs
/// uses the `gui/blob` HTTP door, which answers the same bytes raw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobGet {
    /// The path a [`BlobRef`] carried.
    pub path: String,
}

/// A blob's bytes on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobBytes {
    /// The byte count, before encoding.
    pub size: u64,
    /// The bytes, standard base64.
    pub base64: String,
}

impl Op for BlobGet {
    const NAME: &'static str = "blob.get";
    const SCOPE: Scope = Scope::Read;
    type Reply = BlobBytes;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let roots = roots(&home_of(ctx));
        let bytes = blocking(move || read(&roots, &self.path)).await?;
        Ok(BlobBytes {
            size: bytes.len() as u64,
            base64: BASE64.encode(&bytes),
        })
    }
}

/// Send an attachment to the crab, for a turn to carry. `Send` scope: an
/// upload exists to be attached to a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobPut {
    /// The file name as the sender knew it. Only its last component, made
    /// safe, reaches the disk.
    pub name: String,
    /// The bytes, standard base64.
    pub base64: String,
}

impl Op for BlobPut {
    const NAME: &'static str = "blob.put";
    const SCOPE: Scope = Scope::Send;
    type Reply = BlobRef;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        // Refuse an oversized upload before decoding it: base64 is four
        // characters per three bytes.
        let encoded = self.base64.len() as u64;
        if encoded / 4 * 3 > MAX_BLOB_BYTES + 3 {
            return Err(BlobError::TooLarge {
                size: encoded / 4 * 3,
            }
            .into());
        }
        let bytes = BASE64
            .decode(self.base64.as_bytes())
            .map_err(|e| BlobError::BadEncoding(e.to_string()))?;
        let home = home_of(ctx);
        blocking(move || store_upload(&home, &self.name, &bytes)).await
    }
}

ops! { BlobGet, BlobPut }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[test]
    fn a_blob_ref_survives_the_wire_without_a_size() {
        let blob = BlobRef::at("/home/crab/icon.png");
        let json = serde_json::to_string(&blob).expect("encode");
        assert_eq!(json, r#"{"path":"/home/crab/icon.png"}"#);
        let back: BlobRef = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, blob);
    }

    /// A temp home with every root created, a file in `tmp`, and a secret
    /// next to the home that no root covers.
    struct Disk {
        _dir: tempfile::TempDir,
        home: PathBuf,
        outside: PathBuf,
    }

    impl Disk {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let home = dir.path().join("home");
            for root in roots(&home) {
                std::fs::create_dir_all(&root).expect("root");
            }
            std::fs::write(home.join("tmp/photo.jpg"), b"jpeg").expect("inside file");
            std::fs::write(home.join("keys.toml"), b"secret").expect("home file");
            let outside = dir.path().join("outside.txt");
            std::fs::write(&outside, b"secret").expect("outside file");
            Self {
                _dir: dir,
                home,
                outside,
            }
        }

        fn roots(&self) -> Vec<PathBuf> {
            roots(&self.home)
        }

        fn path(&self, rel: &str) -> String {
            self.home.join(rel).to_string_lossy().into_owned()
        }
    }

    fn status(roots: &[PathBuf], requested: &str) -> u16 {
        match read(roots, requested) {
            Ok(_) => 200,
            Err(e) => e.status(),
        }
    }

    #[test]
    fn a_file_inside_a_root_is_served() {
        let disk = Disk::new();
        assert_eq!(
            read(&disk.roots(), &disk.path("tmp/photo.jpg")).expect("served"),
            b"jpeg"
        );
        assert_eq!(
            read(&disk.roots(), &disk.path("tmp/./sub/../photo.jpg")).expect("folded"),
            b"jpeg",
            "a `..` that stays inside the root is fine"
        );
    }

    #[test]
    fn dot_dot_out_of_a_root_is_403() {
        let disk = Disk::new();
        let roots = disk.roots();
        for requested in [
            disk.path("tmp/../keys.toml"),
            disk.path("tmp/../../outside.txt"),
            disk.path("tmp/../../../../../../../../etc/passwd"),
            disk.path("gui-icons/../tmp/../keys.toml"),
            disk.path("tmp/../missing.txt"),
        ] {
            assert_eq!(status(&roots, &requested), 403, "{requested}");
        }
    }

    #[test]
    fn an_absolute_path_outside_every_root_is_403() {
        let disk = Disk::new();
        let roots = disk.roots();
        for requested in [
            disk.outside.to_string_lossy().into_owned(),
            disk.path("keys.toml"),
            disk.path("opencrabs.db"),
            disk.path("tmpfoo/x"),
            disk.path("tmp"),
            "/etc/passwd".to_string(),
            "/".to_string(),
            "/nonexistent/at/all".to_string(),
        ] {
            assert_eq!(status(&roots, &requested), 403, "{requested}");
        }
    }

    #[test]
    fn a_relative_path_is_403() {
        let disk = Disk::new();
        let roots = disk.roots();
        for requested in ["tmp/photo.jpg", "../outside.txt", "", "."] {
            assert_eq!(status(&roots, requested), 403, "{requested:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_a_root_is_403() {
        let disk = Disk::new();
        let roots = disk.roots();
        std::os::unix::fs::symlink(&disk.outside, disk.home.join("tmp/link.txt"))
            .expect("file link");
        std::os::unix::fs::symlink(disk.home.clone(), disk.home.join("tmp/up"))
            .expect("dir link");
        std::os::unix::fs::symlink("/etc", disk.home.join("plans/etc")).expect("abs link");
        for requested in [
            disk.path("tmp/link.txt"),
            disk.path("tmp/up/keys.toml"),
            disk.path("plans/etc/passwd"),
        ] {
            assert_eq!(status(&roots, &requested), 403, "{requested}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_followed_but_not_escaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real-home");
        let home = dir.path().join("home");
        for root in roots(&real) {
            std::fs::create_dir_all(&root).expect("root");
        }
        std::fs::write(real.join("tmp/a.txt"), b"a").expect("file");
        std::fs::write(real.join("keys.toml"), b"secret").expect("secret");
        std::os::unix::fs::symlink(&real, &home).expect("home link");
        let roots = roots(&home);
        let via_link = home.join("tmp/a.txt").to_string_lossy().into_owned();
        let canonical = real
            .canonicalize()
            .expect("canonical")
            .join("tmp/a.txt")
            .to_string_lossy()
            .into_owned();
        assert_eq!(status(&roots, &via_link), 200);
        assert_eq!(status(&roots, &canonical), 200);
        let escape = home.join("tmp/../keys.toml").to_string_lossy().into_owned();
        assert_eq!(status(&roots, &escape), 403);
    }

    #[test]
    fn a_missing_file_inside_a_root_is_404() {
        let disk = Disk::new();
        assert_eq!(status(&disk.roots(), &disk.path("tmp/missing.jpg")), 404);
    }

    #[test]
    fn a_directory_is_403() {
        let disk = Disk::new();
        std::fs::create_dir_all(disk.home.join("projects/p/session")).expect("dir");
        assert_eq!(status(&disk.roots(), &disk.path("projects/p/session")), 403);
    }

    #[test]
    fn a_file_over_the_limit_is_413() {
        let disk = Disk::new();
        let big = disk.home.join("tmp/big.bin");
        File::create(&big)
            .expect("create")
            .set_len(MAX_BLOB_BYTES + 1)
            .expect("grow");
        assert_eq!(status(&disk.roots(), &disk.path("tmp/big.bin")), 413);
        let edge = disk.home.join("tmp/edge.bin");
        File::create(&edge)
            .expect("create")
            .set_len(MAX_BLOB_BYTES)
            .expect("grow");
        assert_eq!(status(&disk.roots(), &disk.path("tmp/edge.bin")), 200);
    }

    #[test]
    fn an_upload_lands_in_the_upload_dir_under_a_safe_name() {
        let disk = Disk::new();
        let blob = store_upload(&disk.home, "../../etc/.bashrc rc", b"hello").expect("stored");
        let path = PathBuf::from(&blob.path);
        let uploads = disk
            .home
            .join(UPLOAD_DIR)
            .canonicalize()
            .expect("upload dir");
        assert_eq!(path.parent(), Some(uploads.as_path()));
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name");
        assert!(file_name.ends_with("-bashrc_rc"), "{file_name}");
        assert_eq!(blob.size, Some(5));
        assert_eq!(read(&disk.roots(), &blob.path).expect("served back"), b"hello");

        let again = store_upload(&disk.home, "../../etc/.bashrc rc", b"hello").expect("again");
        assert_eq!(again, blob, "the same bytes under the same name are one file");
    }

    #[test]
    fn an_oversized_upload_is_refused() {
        let disk = Disk::new();
        let bytes = vec![0u8; (MAX_BLOB_BYTES + 1) as usize];
        assert_eq!(
            store_upload(&disk.home, "big.bin", &bytes),
            Err(BlobError::TooLarge {
                size: MAX_BLOB_BYTES + 1
            })
        );
    }

    #[test]
    fn safe_name_keeps_only_the_last_plain_component() {
        assert_eq!(safe_name("photo.jpg"), "photo.jpg");
        assert_eq!(safe_name("a/b/c.png"), "c.png");
        assert_eq!(safe_name("..\\..\\win.ini"), "win.ini");
        assert_eq!(safe_name(".."), "blob");
        assert_eq!(safe_name(""), "blob");
        assert_eq!(safe_name("ação 1.txt"), "a__o_1.txt");
        assert_eq!(safe_name(&"x".repeat(300)).chars().count(), 96);
    }

    #[tokio::test]
    async fn blob_get_has_parity() {
        let gateway = Gateway::start().await;
        let dir = gateway.home().join("gui-icons");
        std::fs::create_dir_all(&dir).expect("icons");
        std::fs::write(dir.join("i.png"), b"png").expect("icon");
        let op = BlobGet {
            path: dir.join("i.png").to_string_lossy().into_owned(),
        };
        let answer = parity!(gateway, op.clone());
        assert_eq!(answer["Ok"]["size"], 3, "{answer}");
        assert_eq!(answer["Ok"]["base64"], BASE64.encode(b"png"), "{answer}");
        forbidden!(gateway, op);
    }

    #[tokio::test]
    async fn blob_get_refuses_a_path_outside_the_roots() {
        let gateway = Gateway::start().await;
        let path = gateway.ctx.db_path().to_string_lossy().into_owned();
        let answer = parity!(gateway, BlobGet { path });
        assert_eq!(
            answer["Err"]["detail"],
            BlobError::Denied.to_string(),
            "{answer}"
        );
    }

    #[tokio::test]
    async fn blob_put_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            BlobPut {
                name: "note.txt".into(),
                base64: BASE64.encode(b"attached"),
            }
        );
        let path = answer["Ok"]["path"].as_str().expect("a path").to_string();
        assert!(
            Path::new(&path).starts_with(
                gateway
                    .home()
                    .join(UPLOAD_DIR)
                    .canonicalize()
                    .expect("upload dir")
            ),
            "{answer}"
        );
        let back = gateway.local(BlobGet { path }).await.expect("served back");
        assert_eq!(back.base64, BASE64.encode(b"attached"));
        forbidden!(
            gateway,
            BlobPut {
                name: "refused.txt".into(),
                base64: BASE64.encode(b"refused"),
            }
        );
        let names: Vec<String> = std::fs::read_dir(gateway.home().join(UPLOAD_DIR))
            .expect("upload dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "a refused upload wrote nothing: {names:?}");
    }

    #[tokio::test]
    async fn blob_put_refuses_bad_base64() {
        let gateway = Gateway::start().await;
        let answer = gateway
            .local(BlobPut {
                name: "x".into(),
                base64: "not base64!".into(),
            })
            .await;
        assert!(matches!(answer, Err(BackendError::BadParams(_))), "{answer:?}");
    }
}
