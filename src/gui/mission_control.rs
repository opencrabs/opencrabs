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

/// Which panel the keyboard is on, in the terminal's own order
/// (`tui::app::mission_control::McPanel`): Tab and `l` walk it forward, `h`
/// and Shift-Tab walk it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Inbox,
    Analytics,
    Activity,
    Schedule,
}

impl Panel {
    pub fn title(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Analytics => "Analytics",
            Self::Activity => "Activity",
            Self::Schedule => "Schedule",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Inbox => Self::Analytics,
            Self::Analytics => Self::Activity,
            Self::Activity => Self::Schedule,
            Self::Schedule => Self::Inbox,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Inbox => Self::Schedule,
            Self::Analytics => Self::Inbox,
            Self::Activity => Self::Analytics,
            Self::Schedule => Self::Activity,
        }
    }

    /// Rows the panel can select. Analytics is one page, exactly as the
    /// terminal counts it, so Enter still has something to open there.
    pub fn rows(self, snapshot: Option<&Snapshot>) -> usize {
        match self {
            Self::Inbox => snapshot.map_or(0, |s| s.inbox.len()),
            Self::Analytics => usize::from(snapshot.is_some()),
            Self::Activity => snapshot.map_or(0, |s| s.activity.len()),
            Self::Schedule => snapshot.map_or(0, |s| s.schedule.len()),
        }
    }
}

/// The screen's keyboard state: the focused panel, the selected row and
/// whether the detail popup is open. Pure data with pure transitions, so the
/// walking rules are tested without a window.
#[derive(Debug, Clone, Default)]
pub struct Nav {
    pub panel: Panel,
    pub selected: usize,
    pub detail: bool,
}

impl Nav {
    /// Tab or `l`: the next panel, keeping the row index if it still exists,
    /// the way the terminal keeps one selection across panels.
    pub fn focus_next(&mut self, snapshot: Option<&Snapshot>) {
        self.panel = self.panel.next();
        self.clamp(snapshot);
    }

    /// Shift-Tab or `h`: the previous panel.
    pub fn focus_prev(&mut self, snapshot: Option<&Snapshot>) {
        self.panel = self.panel.prev();
        self.clamp(snapshot);
    }

    /// `j` / down, `k` / up: move one row inside the focused panel.
    pub fn move_selection(&mut self, snapshot: Option<&Snapshot>, delta: i64) {
        let rows = self.panel.rows(snapshot);
        if rows == 0 {
            self.selected = 0;
            self.detail = false;
            return;
        }
        let last = rows - 1;
        let moved = self.selected.min(last) as i64 + delta;
        self.selected = moved.clamp(0, last as i64) as usize;
    }

    /// `g` / Home: the first row.
    pub fn home(&mut self) {
        self.selected = 0;
    }

    /// `G` / End: the last row the panel has.
    pub fn end(&mut self, snapshot: Option<&Snapshot>) {
        self.selected = self.panel.rows(snapshot).saturating_sub(1);
    }

    /// Enter: open the detail when the focused panel has something to show.
    /// The inbox is the panel whose rows carry a record to read, which is why
    /// the terminal opens it there and nowhere else so far.
    pub fn open_detail(&mut self, snapshot: Option<&Snapshot>) {
        if self.panel == Panel::Inbox && self.panel.rows(snapshot) > 0 {
            self.detail = true;
        }
    }

    /// A shorter snapshot or a fresh read: keep the selection inside it.
    pub fn clamp(&mut self, snapshot: Option<&Snapshot>) {
        let rows = self.panel.rows(snapshot);
        if rows == 0 {
            self.selected = 0;
            self.detail = false;
            return;
        }
        self.selected = self.selected.min(rows - 1);
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

    /// An inbox holding `items` proposals, for the keyboard tests.
    fn inbox_snapshot(items: usize) -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.inbox = (0..items)
            .map(|i| McInboxItem {
                id: format!("prop_tool_{i}"),
                label: format!("tool_{i}"),
                summary: "why it was proposed".into(),
                kind: opencrabs::brain::mission_control::McInboxKind::ProposedTool,
                source: "rsi-autonomous".into(),
                created_at: chrono::Utc::now(),
                detail: None,
            })
            .collect();
        snapshot
    }

    #[test]
    fn mission_nav_walks_the_panels_in_the_terminals_order() {
        let snapshot = inbox_snapshot(2);
        let mut nav = Nav::default();
        assert_eq!(nav.panel, Panel::Inbox);
        nav.focus_next(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Analytics);
        nav.focus_next(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Activity);
        nav.focus_next(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Schedule);
        // Tab past the last panel wraps to the first, never to a dead end.
        nav.focus_next(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Inbox);
        nav.focus_prev(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Schedule);
    }

    #[test]
    fn mission_nav_keeps_the_selection_inside_the_snapshot() {
        let snapshot = inbox_snapshot(3);
        let mut nav = Nav::default();
        nav.end(Some(&snapshot));
        assert_eq!(nav.selected, 2);
        // One more row down is still the last row, and one up leaves it.
        nav.move_selection(Some(&snapshot), 1);
        assert_eq!(nav.selected, 2);
        nav.move_selection(Some(&snapshot), -1);
        assert_eq!(nav.selected, 1);
        nav.home();
        assert_eq!(nav.selected, 0);
        nav.move_selection(Some(&snapshot), -1);
        assert_eq!(nav.selected, 0);

        // A shorter read (a proposal got applied elsewhere) clamps the row.
        let shorter = inbox_snapshot(1);
        nav.end(Some(&shorter));
        assert_eq!(nav.selected, 0);

        // An empty read takes the row with it and closes the detail.
        nav.selected = 4;
        nav.detail = true;
        nav.clamp(Some(&Snapshot::default()));
        assert_eq!(nav.selected, 0);
        assert!(!nav.detail);
    }

    #[test]
    fn mission_nav_opens_a_detail_only_where_there_is_a_record() {
        let empty = Snapshot::default();
        let mut nav = Nav::default();
        nav.open_detail(Some(&empty));
        assert!(!nav.detail, "an empty inbox has nothing to open");
        nav.open_detail(None);
        assert!(!nav.detail, "no read yet means nothing to open");

        let snapshot = inbox_snapshot(2);
        nav.open_detail(Some(&snapshot));
        assert!(nav.detail);

        // The other panels have no record view yet, so Enter there is a no-op
        // rather than a popup with nothing in it.
        let mut nav = Nav {
            panel: Panel::Schedule,
            ..Nav::default()
        };
        nav.open_detail(Some(&snapshot));
        assert!(!nav.detail);
    }
}
