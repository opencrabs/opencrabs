//! Bounded loading for documents shown inside a UI preview.
//!
//! This module only reads and validates text. Markdown parsing and painting
//! belong to the frontend, so a slow or oversized file never does work on the
//! render thread.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DOCUMENT_CHARS: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
    Markdown,
    PlainText,
}

impl DocumentKind {
    pub fn of_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown") {
            Some(Self::Markdown)
        } else if extension.eq_ignore_ascii_case("txt") || extension.eq_ignore_ascii_case("text") {
            Some(Self::PlainText)
        } else {
            None
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct LoadedDocument {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DocumentLoadError {
    Io(String),
    NotAFile,
    InvalidUtf8,
}

impl fmt::Display for DocumentLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "{message}"),
            Self::NotAFile => f.write_str("the path is not a regular file"),
            Self::InvalidUtf8 => f.write_str("the file is not valid UTF-8 text"),
        }
    }
}

pub fn load(path: &Path) -> Result<LoadedDocument, DocumentLoadError> {
    load_with_limits(path, MAX_DOCUMENT_BYTES, MAX_DOCUMENT_CHARS)
}

pub fn load_with_limits(
    path: &Path,
    max_bytes: usize,
    max_chars: usize,
) -> Result<LoadedDocument, DocumentLoadError> {
    let metadata = path
        .metadata()
        .map_err(|error| DocumentLoadError::Io(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DocumentLoadError::NotAFile);
    }

    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| DocumentLoadError::Io(error.to_string()))?
        .take(max_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| DocumentLoadError::Io(error.to_string()))?;
    let mut truncated = metadata.len() > max_bytes as u64;

    // A byte limit can end inside a codepoint. Only that final, incomplete
    // sequence is removable; malformed UTF-8 elsewhere is an honest error.
    let mut text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_owned(),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .expect("the valid prefix of truncated UTF-8 remains valid")
                .to_owned()
        }
        Err(_) => return Err(DocumentLoadError::InvalidUtf8),
    };

    if let Some((end, _)) = text.char_indices().nth(max_chars) {
        text.truncate(end);
        truncated = true;
    }

    Ok(LoadedDocument { text, truncated })
}
