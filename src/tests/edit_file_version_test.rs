//! Regression (#2210): an edit records what the session now knows on disk, so
//! the next whole-file write is not refused as another agent's change.
//!
//! `edit_file` and `hashline_edit` both read the whole file and rewrite it, but
//! neither recorded the result in `file_versions`. The next `write_file`
//! compared the bytes on disk against the hash left by the last read, saw the
//! difference its own edit had made, and refused with "another agent is
//! working in this directory" while the session was the only writer.
//!
//! The partial-view guard (#1168) failed the same way for the same reason: an
//! edit is a whole-file read, so it is a basis for a later overwrite.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::edit::EditTool;
use crate::brain::tools::file_versions::forget_session;
use crate::brain::tools::hashline::edit::HashlineEditTool;
use crate::brain::tools::hashline::hash::hash_line;
use crate::brain::tools::write::WriteTool;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

const ORIGINAL: &str = "apples=3\nbananas=5\ncherries=7\ndates=9\n";
const REPLACEMENT: &str = "kiwis=1\nlemons=2\n";

fn ctx(dir: &std::path::Path, session: Uuid) -> ToolExecutionContext {
    ToolExecutionContext::new(session)
        .with_working_directory(dir.to_path_buf())
        .with_auto_approve(true)
}

/// Seed the file through `write_file` itself, so the session has a recorded
/// hash to go stale, exactly as the reported reproduction did.
async fn seed(dir: &std::path::Path, session: Uuid, path: &std::path::Path) {
    let result = WriteTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "content": ORIGINAL,
            }),
            &ctx(dir, session),
        )
        .await
        .expect("write_file create");
    assert!(result.success, "seed write failed: {:?}", result.output);
}

async fn overwrite(dir: &std::path::Path, session: Uuid, path: &std::path::Path) -> String {
    let result = WriteTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "content": REPLACEMENT,
                "overwrite_read_confirm": true,
            }),
            &ctx(dir, session),
        )
        .await
        .expect("write_file overwrite");
    assert!(
        result.success,
        "a whole-file write after this session's own edit must not be refused: {}",
        result.output
    );
    result.output
}

#[tokio::test]
async fn a_write_after_edit_file_is_not_refused() {
    let dir = TempDir::new().unwrap();
    let session = Uuid::new_v4();
    let path = dir.path().join("inventory.txt");
    seed(dir.path(), session, &path).await;

    let edited = EditTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "operation": "replace",
                "old_text": "bananas=5",
                "new_text": "bananas=6",
            }),
            &ctx(dir.path(), session),
        )
        .await
        .expect("edit_file");
    assert!(edited.success, "edit failed: {:?}", edited.output);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("bananas=6")
    );

    overwrite(dir.path(), session, &path).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REPLACEMENT);

    forget_session(session);
}

#[tokio::test]
async fn a_write_after_hashline_edit_is_not_refused() {
    let dir = TempDir::new().unwrap();
    let session = Uuid::new_v4();
    let path = dir.path().join("inventory.txt");
    seed(dir.path(), session, &path).await;

    let edited = HashlineEditTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "edits": [{
                    "op": "replace",
                    "pos": format!("2#{}", hash_line("bananas=5")),
                    "lines": "bananas=6"
                }],
            }),
            &ctx(dir.path(), session),
        )
        .await
        .expect("hashline_edit");
    assert!(edited.success, "hashline edit failed: {:?}", edited.output);

    overwrite(dir.path(), session, &path).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REPLACEMENT);

    forget_session(session);
}

/// The guard still has to fire for a real concurrent change: another writer
/// moves the file between this session's edit and its write.
#[tokio::test]
async fn a_write_after_another_writer_moved_the_file_is_still_refused() {
    let dir = TempDir::new().unwrap();
    let session = Uuid::new_v4();
    let path = dir.path().join("inventory.txt");
    seed(dir.path(), session, &path).await;

    let edited = EditTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "operation": "replace",
                "old_text": "bananas=5",
                "new_text": "bananas=6",
            }),
            &ctx(dir.path(), session),
        )
        .await
        .expect("edit_file");
    assert!(edited.success);

    // Someone else writes after our edit. The recorded hash is now stale in
    // the way the guard exists to catch.
    std::fs::write(&path, "someone else was here\n").unwrap();

    let refused = WriteTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "content": REPLACEMENT,
                "overwrite_read_confirm": true,
            }),
            &ctx(dir.path(), session),
        )
        .await
        .expect("write_file");
    assert!(
        !refused.success,
        "a concurrent change must still be refused, got: {:?}",
        refused.output
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "someone else was here\n",
        "the refused write must not touch the file"
    );

    forget_session(session);
}
