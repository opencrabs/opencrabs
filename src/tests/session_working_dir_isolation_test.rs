//! Regression for #703: the working directory must be isolated PER SESSION.
//!
//! Before this, `AgentService` held one process-global `Arc<RwLock<PathBuf>>`
//! shared by every session. Two sessions running concurrently in different
//! directories contaminated each other: a `cd` in one moved the other's cwd,
//! so the Runtime Info prompt line and tool execution reported the wrong
//! directory (observed: an ff7_remotion session's cwd leaking into the
//! opencrabs session's prompt while the footer stayed correct).
//!
//! These lock the isolation invariants at the `AgentService` API level.

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::MockProvider;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

async fn make_service() -> AgentService {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    AgentService::new_for_test(provider, context).await
}

/// Setting one session's cwd must not move another session's cwd.
#[tokio::test]
async fn sessions_have_independent_working_directories() {
    let svc = make_service().await;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    svc.set_working_directory_for_session(a, PathBuf::from("/tmp/session-a"));
    svc.set_working_directory_for_session(b, PathBuf::from("/tmp/session-b"));

    assert_eq!(
        svc.get_working_directory_for_session(a),
        PathBuf::from("/tmp/session-a")
    );
    assert_eq!(
        svc.get_working_directory_for_session(b),
        PathBuf::from("/tmp/session-b")
    );

    // Move A again; B stays put — the core contamination guard.
    svc.set_working_directory_for_session(a, PathBuf::from("/tmp/session-a-moved"));
    assert_eq!(
        svc.get_working_directory_for_session(a),
        PathBuf::from("/tmp/session-a-moved")
    );
    assert_eq!(
        svc.get_working_directory_for_session(b),
        PathBuf::from("/tmp/session-b")
    );
}

/// A background session mutating ITS OWN handle (as a tool `cd` does) must not
/// move the global cwd — the seed for future sessions.
#[tokio::test]
async fn per_session_cd_does_not_move_global() {
    let svc = make_service().await;
    let global_before = svc.get_working_directory();
    let bg = Uuid::new_v4();

    // First touch seeds from the global, then a `cd`-style mutation of the
    // session's own handle diverges it.
    let handle = svc.working_dir_handle_for_session(bg);
    assert_eq!(*handle.read().unwrap(), global_before);
    *handle.write().unwrap() = PathBuf::from("/tmp/background-cd");

    assert_eq!(
        svc.get_working_directory_for_session(bg),
        PathBuf::from("/tmp/background-cd")
    );
    // Global is untouched — a brand-new session still seeds from it.
    assert_eq!(svc.get_working_directory(), global_before);
    let fresh = Uuid::new_v4();
    assert_eq!(svc.get_working_directory_for_session(fresh), global_before);
}

/// An untouched session falls back to the global cwd (channels / first turn).
#[tokio::test]
async fn untouched_session_falls_back_to_global() {
    let svc = make_service().await;
    svc.set_working_directory(PathBuf::from("/tmp/global-seed"));
    let fresh = Uuid::new_v4();
    assert_eq!(
        svc.get_working_directory_for_session(fresh),
        PathBuf::from("/tmp/global-seed")
    );
}

/// #2007: restoring one session's own directory (a pane switch, a channel
/// resume, the boot fan-out) pins THAT session and must not move the global
/// every other session seeds from. Only an explicit operator `/cd` may move it.
#[tokio::test]
async fn restoring_a_sessions_own_directory_does_not_move_the_global() {
    let svc = make_service().await;
    let global_before = svc.get_working_directory();
    let resumed = Uuid::new_v4();
    let other = Uuid::new_v4();

    svc.set_session_only_working_directory(resumed, PathBuf::from("/tmp/resumed-repo"));

    assert_eq!(
        svc.get_working_directory_for_session(resumed),
        PathBuf::from("/tmp/resumed-repo")
    );
    assert_eq!(
        svc.get_working_directory(),
        global_before,
        "a resume dragged every session with no handle yet into the resumed repo (#2007)"
    );
    assert_eq!(
        svc.get_working_directory_for_session(other),
        global_before,
        "a session nobody switched to must keep the global seed"
    );

    // Boot restores several sessions in a row: the last one must not win for everyone else.
    svc.set_session_only_working_directory(other, PathBuf::from("/tmp/second-repo"));
    assert_eq!(
        svc.get_working_directory_for_session(resumed),
        PathBuf::from("/tmp/resumed-repo")
    );
    assert_eq!(svc.get_working_directory(), global_before);

    // The documented exception: an operator `/cd` moves the seed for brand-new
    // sessions, and sessions that already hold a handle keep it.
    svc.set_working_directory_for_session(resumed, PathBuf::from("/tmp/operator-cd"));
    assert_eq!(
        svc.get_working_directory(),
        PathBuf::from("/tmp/operator-cd")
    );
    assert_eq!(
        svc.get_working_directory_for_session(other),
        PathBuf::from("/tmp/second-repo"),
        "a session with its own handle keeps it across an operator /cd"
    );
}

/// The call-site guard. The bug was not the setter, it was who called it: a pane
/// switch, a resume and the boot fan-out all passed through the setter that
/// moves the global. If any of them goes back, the behaviour test above can only
/// catch the one it simulates. So the tree is scanned outside `src/tests/`, and
/// the ONLY raw call site allowed is the shared `/cd` applier
/// (`apply_operator_cd`), which both surfaces call: the terminal handler and the
/// window's text command (#2469). A second copy of the sequence is exactly how
/// the two drifted, so a second call site fails here.
#[test]
fn only_the_cd_path_calls_the_global_moving_setter() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut callers: Vec<(String, usize)> = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // A call site always reads `.<setter>(`; the definition in
            // builder.rs and its doc mentions do not.
            let hits = text.matches(".set_working_directory_for_session(").count();
            if hits > 0 {
                callers.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    hits,
                ));
            }
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        vec![("builder.rs".to_string(), 1)],
        "`set_working_directory_for_session` moves the process-global cwd. Its one call \
         site must stay inside `apply_operator_cd` in brain/agent/service/builder.rs, \
         which both `/cd` surfaces call (#2007, #2469)"
    );
}
