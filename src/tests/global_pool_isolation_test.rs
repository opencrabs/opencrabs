//! The process-wide DB pool must stay unpublished in test builds (#2072).
//!
//! `Database::connect` publishes its pool into `GLOBAL_POOL`, a `OnceLock`
//! that never resets. In a test binary that meant the first test to open a
//! file DB handed every later test a pool pointing at a temp file it then
//! deleted, so those tests depended on run order. #2071 was the first
//! symptom. The publish is now gated with `#[cfg(not(test))]`; this pins it.

use crate::db::{Database, global_pool};

/// A file-backed `Database::connect` must not publish the process-wide pool
/// in a test build.
///
/// The precondition assertion is deliberate. If some other test has already
/// published a pool, this test fails at that line rather than passing on a
/// contaminated process, which is the whole point of the pin.
#[tokio::test]
async fn connect_does_not_publish_the_global_pool() {
    assert!(
        global_pool().is_none(),
        "a pool is already published, so another test in this binary leaked \
         its own connection into the process"
    );

    let path = std::env::temp_dir().join(format!(
        "test_global_pool_isolation_{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = Database::connect(&path).await.expect("file-backed connect");

    assert!(
        global_pool().is_none(),
        "Database::connect published GLOBAL_POOL in a test build; every other \
         test in this binary now resolves global_pool() to a pool for {}",
        path.display()
    );

    drop(db);
    let _ = std::fs::remove_file(&path);
}
