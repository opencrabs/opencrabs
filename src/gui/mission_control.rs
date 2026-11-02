//! The window's Mission Control snapshot.
//!
//! The terminal reads four services on open and re-reads them when the
//! analytics window changes (`tui::app::mission_control`). The window reads
//! the same four and renders the same structs, so the two front ends can never
//! tell different stories about the same instance (#2426).
//!
//! Two rules from the window's other DB reads carry over: the connection is
//! made on a plain std thread with its own current-thread Tokio reactor, and
//! `Config` is never loaded from a gpui worker. gpui's threads are dispatch
//! queue threads with small stacks and no reactor, and sqlx panics there.

/// The analytics window is part of the snapshot's own interface (the panels
/// label their numbers with it, and the keys move it), so the window
/// re-exports it rather than making every caller reach into the crate.
pub use opencrabs::brain::mission_control::TimeWindow;
use opencrabs::brain::mission_control::{
    McActivity, McAnalytics, McInboxItem, McScheduleItem, activity_service, analytics_service,
    inbox_service, schedule_service,
};
use opencrabs::db::{Database, Pool};
use std::path::PathBuf;

/// How many activity rows the panel asks for. The terminal shows the newest
/// handful rather than the whole journal, and this is that same handful.
pub const ACTIVITY_ROWS: usize = 12;

/// Everything one Mission Control screen draws, read once.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// RSI proposals waiting on a decision, newest first.
    pub inbox: Vec<McInboxItem>,
    /// Recent RSI activity, newest first.
    pub activity: Vec<McActivity>,
    /// Every cron job, enabled and paused.
    pub schedule: Vec<McScheduleItem>,
    /// Usage and reliability numbers for the analytics window.
    pub analytics: McAnalytics,
    /// The window those analytics were read for.
    pub window: TimeWindow,
}

impl Snapshot {
    /// Tool calls and failures in one line, the pair the analytics panel leads
    /// with.
    pub fn tool_line(&self) -> String {
        format!(
            "{} calls · {} failed",
            self.analytics.tool_total_calls, self.analytics.tool_total_fails
        )
    }
}

/// Read the snapshot for `window` on a thread that owns its reactor.
///
/// The thread is 4 MB and named, exactly like [`super::agent_bridge::load_files`],
/// because the caller waits on it from a gpui task.
pub async fn load(db_path: PathBuf, window: TimeWindow) -> Result<Snapshot, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-mission-control".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("tokio runtime failed: {e}")));
                    return;
                }
            };
            let result = runtime.block_on(async move {
                let db = Database::connect(&db_path)
                    .await
                    .map_err(|e| format!("db connect: {e:#}"))?;
                Ok(read_from_pool(db.pool().clone(), window).await)
            });
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("mission control thread dropped its result".into()),
        },
        Err(e) => Err(format!("mission control thread failed to spawn: {e}")),
    }
}

/// The four reads, given a pool. Separate from [`load`] so a test can point it
/// at an in-memory database instead of the live one.
pub async fn read_from_pool(pool: Pool, window: TimeWindow) -> Snapshot {
    Snapshot {
        inbox: inbox_service::list(),
        activity: activity_service::recent(ACTIVITY_ROWS),
        schedule: schedule_service::list(pool.clone()).await,
        analytics: analytics_service::summary(pool, window).await,
        window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::config::profile::with_home_override;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    #[test]
    fn mission_snapshot_reads_every_service_from_an_empty_home() {
        // The home override is the whole point of the test: an empty inbox, an
        // empty activity log and an empty schedule are what this instance would
        // show a new user, and a reader that fell back to the live home would
        // paint this machine's real proposals into that picture.
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = with_home_override(dir.path().to_path_buf(), || {
            runtime().block_on(async {
                let db = Database::connect_in_memory().await.expect("in-memory db");
                read_from_pool(db.pool().clone(), TimeWindow::All).await
            })
        });
        assert!(snapshot.inbox.is_empty(), "a fresh home has no proposals");
        assert!(snapshot.activity.is_empty(), "a fresh home has no journal");
        assert!(
            snapshot.schedule.is_empty(),
            "a fresh home has no cron jobs"
        );
        assert_eq!(snapshot.analytics.tool_total_calls, 0);
        assert_eq!(snapshot.window, TimeWindow::All);
    }

    #[test]
    fn mission_panels_answer_from_the_structs_they_were_given() {
        // The renderer trusts these three readings, so they are pinned here
        // against real structs rather than against a mock: a paused job counts
        // as paused, an enabled one does not, and the tool line is the pair the
        // analytics panel prints.
        let mut snapshot = Snapshot::default();
        snapshot.analytics.tool_total_calls = 41;
        snapshot.analytics.tool_total_fails = 3;
        snapshot.inbox = vec![McInboxItem {
            id: "prop_tool_1".into(),
            label: "deploy_staging".into(),
            summary: "a tool".into(),
            kind: opencrabs::brain::mission_control::McInboxKind::ProposedTool,
            source: "rsi-autonomous".into(),
            created_at: chrono::Utc::now(),
            detail: None,
        }];
        let job = |enabled: bool, id: &str| McScheduleItem {
            id: id.into(),
            label: id.into(),
            schedule: "0 9 * * *".into(),
            kind: opencrabs::brain::mission_control::McScheduleKind::Cron,
            awaiting_user: false,
            prompt: String::new(),
            deliver_to: None,
            last_run_at: None,
            next_run_at: None,
            created_at: chrono::Utc::now(),
            enabled,
            profile_name: None,
            last_run_status: None,
            last_run_cost: None,
            last_run_duration_secs: None,
        };
        snapshot.schedule = vec![job(true, "morning-recap"), job(false, "cargo-clean")];
        assert_eq!(snapshot.inbox.len(), 1);
        // The schedule panel marks a paused job; the fixture holds exactly one.
        assert_eq!(snapshot.schedule.iter().filter(|j| !j.enabled).count(), 1);
        assert_eq!(snapshot.tool_line(), "41 calls · 3 failed");
    }
}
