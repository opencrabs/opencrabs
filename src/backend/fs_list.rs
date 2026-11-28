//! Listing a folder on the crab.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{BackendError, Ctx, Op, Scope};

/// The /cd picker's rows for `dir` on the crab (#2527): the parent entry
/// first when one exists, then the subdirectories, dotfile directories
/// hidden unless `show_hidden`, all sorted by name. A folder that cannot be
/// read answers with the parent entry alone, as the picker always showed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDir {
    /// The folder to list, on the crab.
    pub dir: PathBuf,
    /// Include directories whose name starts with a dot.
    pub show_hidden: bool,
}

impl Op for ListDir {
    const NAME: &'static str = "fs_list.list_dir";
    const SCOPE: Scope = Scope::Admin;
    type Reply = Vec<PathBuf>;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let dir = self.dir;
        let mut files = Vec::new();
        if dir.parent().is_some() {
            files.push(dir.join(".."));
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let hidden = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('.'))
                    .unwrap_or(false);
                if hidden && !self.show_hidden {
                    continue;
                }
                files.push(path);
            }
        }
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(files)
    }
}

ops! { ListDir }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, parity};

    #[tokio::test]
    async fn list_dir_has_parity() {
        let gateway = Gateway::start().await;
        let root = gateway.home().join("pick");
        for sub in ["b", "a", ".hidden"] {
            std::fs::create_dir_all(root.join(sub)).expect("mkdir");
        }
        std::fs::write(root.join("file.txt"), b"x").expect("write");

        let answer = parity!(
            gateway,
            ListDir {
                dir: root.clone(),
                show_hidden: false,
            }
        );
        let want = [root.join(".."), root.join("a"), root.join("b")];
        assert_eq!(answer["Ok"], serde_json::json!(want), "{answer}");

        let answer = parity!(
            gateway,
            ListDir {
                dir: root.clone(),
                show_hidden: true,
            }
        );
        let rows = answer["Ok"].as_array().expect("a list of paths");
        assert_eq!(rows.len(), 4, "{answer}");
    }

    #[tokio::test]
    async fn list_dir_needs_admin() {
        // A read, but of the crab's disk: the scope is admin, as the
        // inventory routes it, and the remote door refuses anything less.
        let gateway = Gateway::start().await;
        crate::backend::parity::forbidden!(
            gateway,
            ListDir {
                dir: gateway.home().to_path_buf(),
                show_hidden: false,
            }
        );
    }
}
