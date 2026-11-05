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

use opencrabs::brain::agent::AgentService;
/// The analytics window is part of the snapshot's own interface (the panels
/// label their numbers with it, and the keys move it), so the window
/// re-exports it rather than making every caller reach into the crate.
pub use opencrabs::brain::mission_control::TimeWindow;
use opencrabs::brain::mission_control::{
    ACTIVITY_LIMIT, McActivity, McAnalytics, McInboxItem, McInboxKind, McScheduleItem,
    activity_service, analytics_service, inbox_service, schedule_service,
};
use opencrabs::brain::prompt_builder::BrainLoader;
use opencrabs::brain::tools::ToolRegistry;
use opencrabs::brain::tools::dynamic::DynamicToolLoader;
use opencrabs::brain::tools::rsi_proposals::RsiProposalsTool;
use opencrabs::db::{Database, Pool};
use opencrabs::tui::app::mission_control::log_viewer::{self, LogViewerState};
use std::path::PathBuf;
use std::sync::Arc;

/// How many activity rows a card shows before its window follows the
/// selection. The snapshot itself keeps the shared 100-entry fetch limit.
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
    /// Tab or `l`: the next panel. Row indices are panel-local, so the
    /// terminal and GUI both start the newly focused panel at its first row.
    pub fn focus_next(&mut self, snapshot: Option<&Snapshot>) {
        self.panel = self.panel.next();
        self.selected = 0;
        self.detail = false;
        self.clamp(snapshot);
    }

    /// Shift-Tab or `h`: the previous panel.
    pub fn focus_prev(&mut self, snapshot: Option<&Snapshot>) {
        self.panel = self.panel.prev();
        self.selected = 0;
        self.detail = false;
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

    /// Enter: open the focused panel's detail when it has something to show.
    /// Analytics is one snapshot-wide item, matching the terminal's count.
    pub fn open_detail(&mut self, snapshot: Option<&Snapshot>) {
        if self.panel.rows(snapshot) > 0 {
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

/// The slice of rows to draw for a panel that can show fewer than it holds:
/// `start` and `len` describe a window that keeps the selected row on screen,
/// so `j` never walks past the bottom of the panel.
pub fn visible_window(total: usize, selected: Option<usize>, cap: usize) -> (usize, usize) {
    if total == 0 || cap == 0 {
        return (0, 0);
    }
    let Some(selected) = selected else {
        return (0, total.min(cap));
    };
    let selected = selected.min(total - 1);
    let start = selected.saturating_sub(cap - 1);
    (start, cap.min(total - start))
}

/// The narrowest column a card is drawn in, in rem, before the grid drops a
/// column: enough for a title, a label and its value.
pub const CARD_BASIS_REM: f32 = 22.0;

/// The gap between two cards, in rem. `rems`, so the window's zoom scales the
/// grid with everything else.
pub const CARD_GAP_REM: f32 = 0.5;

/// Most cards a single row will ever hold. Beyond six the cards stop being
/// readable long before the window stops growing.
pub const MAX_COLUMNS: usize = 6;

/// How many cards fit one row at this pane width (#2459).
///
/// The pane width arrives in pixels and the card floor in rem, so the answer
/// follows the window's zoom: a zoomed-in window keeps the same card size and
/// shows fewer of them, which is what a card size in rem means. Never zero, so
/// a collapsed or not-yet-measured window still draws one column instead of a
/// grid with no cells.
pub fn grid_columns(pane_width: f32, rem: f32) -> usize {
    let rem = if rem.is_finite() && rem > 0.0 {
        rem
    } else {
        16.0
    };
    if !pane_width.is_finite() || pane_width <= 0.0 {
        return 1;
    }
    let basis = CARD_BASIS_REM * rem;
    let gap = CARD_GAP_REM * rem;
    let fits = ((pane_width + gap) / (basis + gap)).floor();
    if !fits.is_finite() || fits < 1.0 {
        1
    } else {
        (fits as usize).min(MAX_COLUMNS)
    }
}

/// Place every card in a row-major grid of `cols` columns (#2459).
///
/// Each row lists one slot per column: `Some(index)` for a card and `None` for
/// the empty slots that keep the last row's cards the same width as the rows
/// above. Every card index appears exactly once, which is the property the
/// screen depends on: a card that is dropped here is a panel nobody can see,
/// and that is how the analytics and schedule panels went missing.
pub fn grid_rows(cards: usize, cols: usize) -> Vec<Vec<Option<usize>>> {
    let cols = cols.clamp(1, MAX_COLUMNS);
    let mut rows = Vec::with_capacity(cards.div_ceil(cols));
    let mut next = 0usize;
    while next < cards {
        let mut row = Vec::with_capacity(cols);
        for _ in 0..cols {
            if next < cards {
                row.push(Some(next));
                next += 1;
            } else {
                row.push(None);
            }
        }
        rows.push(row);
    }
    rows
}

/// What a keystroke means for the analytics window (#2426).
///
/// The split matters: a capital that names the window already on screen must
/// still be swallowed, or it falls through to the inbox's bare letters and
/// `Shift+A` would apply a proposal instead of re-filtering. Only a capital
/// naming a different window costs a read.
pub enum WindowKey {
    /// Not one of the filter's capitals: the caller keeps going.
    Pass,
    /// A capital naming the window already shown: consume it, do not read.
    Same,
    /// A capital naming another window: switch and re-read.
    Switch(TimeWindow),
}

/// Decide what one capital does to the analytics window, given the window on
/// screen. `D`/`W`/`M`/`A` are the terminal's keys.
pub fn window_key(current: TimeWindow, key: &str) -> WindowKey {
    let asked = match key {
        "d" => TimeWindow::Day,
        "w" => TimeWindow::Week,
        "m" => TimeWindow::Month,
        "a" => TimeWindow::All,
        _ => return WindowKey::Pass,
    };
    if asked == current {
        WindowKey::Same
    } else {
        WindowKey::Switch(asked)
    }
}

/// Apply one keystroke to the log viewer, in the terminal's own mapping
/// (#1528). The state, its scrolling, its filters and its file paging are the
/// terminal's type, so there is no second parser and nothing to disagree.
///
/// Returns true when the viewer should close. Esc clears a filter first and
/// closes second, so a search that matched nothing costs one key rather than
/// the whole screen. `shift` is passed in because a window hands us `G` as
/// `g` with the modifier set.
pub fn log_key(viewer: &mut LogViewerState, key: &str, shift: bool) -> bool {
    // While the search box is open printable keys are search text rather than
    // commands, or typing "debug" would trip the `d` level filter on the way.
    if viewer.search_active {
        match key {
            "escape" => viewer.clear_search(),
            "enter" => viewer.search_active = false,
            "backspace" => viewer.pop_search(),
            _ => {
                if let Some(c) = single_char(key) {
                    viewer.push_search(c);
                }
            }
        }
        return false;
    }
    match key {
        "escape" => {
            if viewer.search.is_empty() {
                return true;
            }
            viewer.clear_search();
        }
        "/" => viewer.search_active = true,
        "up" | "k" => viewer.scroll_up(1),
        "down" | "j" => viewer.scroll_down(1),
        "pageup" => viewer.page_up(),
        "pagedown" => viewer.page_down(),
        "home" => viewer.scroll = 0,
        "end" => viewer.scroll_to_end(),
        "[" => viewer.step_file(-1),
        "]" => viewer.step_file(1),
        _ => {
            let Some(c) = single_char(key) else {
                return false;
            };
            if c == 'g' {
                if shift {
                    viewer.scroll_to_end();
                } else {
                    viewer.scroll = 0;
                }
            } else if let Some(level) = log_viewer::level_for_key(c) {
                // Digits and initials both, the terminal's own table.
                viewer.set_level(level);
            }
        }
    }
    false
}

/// The printable character a keystroke names, if it names exactly one.
fn single_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

/// What a key on the inbox asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Install the proposal: the loaders write it live.
    Apply,
    /// Archive it without installing.
    Reject,
}

/// What the screen says after a decision: the tool's own message, and
/// whether it worked, so the strip can be green or red.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub ok: bool,
}

/// Apply or reject one proposal with the paths already resolved.
///
/// Sync and path-explicit so a test can point every write at a scratch home
/// without the override having to reach a spawned thread (the profile
/// override is a task-local and does not cross `std::thread::spawn`).
pub fn decide_with(
    registry: Arc<ToolRegistry>,
    id: &str,
    kind: &McInboxKind,
    decision: Decision,
    tools_path: PathBuf,
    brain_path: PathBuf,
) -> Result<String, String> {
    let tool = RsiProposalsTool::new(registry, tools_path, brain_path);
    match decision {
        Decision::Apply => tool.apply(id, kind),
        Decision::Reject => tool.reject(id, None),
    }
}

/// Decide one proposal on a thread of its own.
///
/// Resolving the two paths happens here because they are cheap home lookups;
/// the writes themselves (tools.toml, commands.toml, SKILL.md, the archive)
/// run off the gpui worker, which has a small stack and no reactor.
pub async fn decide(
    service: Arc<AgentService>,
    id: String,
    kind: McInboxKind,
    decision: Decision,
) -> Result<String, String> {
    // The same two paths the terminal resolves, so an applied proposal lands
    // in this instance's own files and nowhere else.
    let Some(tools_path) = DynamicToolLoader::default_path() else {
        return Err("no tools.toml path for this instance".to_string());
    };
    let brain_path = BrainLoader::resolve_path();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-mission-decide".into())
        .spawn(move || {
            let result = decide_with(
                service.tool_registry().clone(),
                &id,
                &kind,
                decision,
                tools_path,
                brain_path,
            );
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("proposal thread dropped its result".to_string()),
        },
        Err(e) => Err(format!("proposal thread failed to spawn: {e}")),
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
        activity: activity_service::recent(ACTIVITY_LIMIT),
        schedule: schedule_service::list(pool.clone()).await,
        analytics: analytics_service::summary(pool, window).await,
        window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::brain::rsi_proposals::ProposalsStore;
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
        // The analytics panel prints these two counts, so they are pinned
        // here rather than against a mock.
        assert_eq!(snapshot.analytics.tool_total_calls, 41);
        assert_eq!(snapshot.analytics.tool_total_fails, 3);
    }

    /// An inbox holding `items` proposals, for the keyboard tests.
    fn inbox_snapshot(items: usize) -> Snapshot {
        Snapshot {
            inbox: (0..items)
                .map(|i| McInboxItem {
                    id: format!("prop_tool_{i}"),
                    label: format!("tool_{i}"),
                    summary: "why it was proposed".into(),
                    kind: opencrabs::brain::mission_control::McInboxKind::ProposedTool,
                    source: "rsi-autonomous".into(),
                    created_at: chrono::Utc::now(),
                    detail: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn full_snapshot() -> Snapshot {
        let mut snapshot = inbox_snapshot(3);
        let activity = McActivity {
            timestamp: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
            level: opencrabs::brain::mission_control::McActivityLevel::Success,
            source: "improvement".into(),
            detail: "A real activity entry".into(),
        };
        snapshot.activity = vec![activity.clone(), activity.clone(), activity];
        let job = McScheduleItem {
            id: "job-1".into(),
            label: "Morning recap".into(),
            schedule: "0 9 * * * (UTC)".into(),
            kind: opencrabs::brain::mission_control::McScheduleKind::Cron,
            awaiting_user: false,
            prompt: "Summarise the morning".into(),
            deliver_to: Some("here".into()),
            last_run_at: chrono::DateTime::from_timestamp(1_700_000_100, 0),
            next_run_at: chrono::DateTime::from_timestamp(1_700_000_200, 0),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            enabled: true,
            profile_name: Some("default".into()),
            last_run_status: Some("success".into()),
            last_run_cost: Some(0.25),
            last_run_duration_secs: Some(42),
        };
        snapshot.schedule = vec![job.clone(), job.clone(), job];
        snapshot
    }

    #[test]
    fn mission_log_keys_scroll_filter_and_close_like_the_terminal() {
        use opencrabs::logging::reader::{LogEntry, LogLevel};

        let entry = |level: LogLevel, message: &str| LogEntry {
            timestamp: "2026-11-02T10:00:00Z".into(),
            level,
            target: "opencrabs::test".into(),
            message: message.into(),
            continuation: Vec::new(),
        };
        let mut state = LogViewerState {
            entries: vec![entry(LogLevel::Info, "one"), entry(LogLevel::Error, "two")],
            // One row fits, so two entries leave exactly one row of scroll.
            viewport_rows: 1,
            ..Default::default()
        };

        // j walks down and stops at the last row rather than past it.
        assert!(!log_key(&mut state, "j", false));
        assert_eq!(state.scroll, 1);
        log_key(&mut state, "j", false);
        assert_eq!(state.scroll, 1, "two entries in a one-row view stop at one");
        log_key(&mut state, "k", false);
        assert_eq!(state.scroll, 0);
        log_key(&mut state, "g", true);
        assert_eq!(state.scroll, 1, "G lands on the newest line");
        log_key(&mut state, "g", false);
        assert_eq!(state.scroll, 0, "g returns to the oldest");

        // The level table is the terminal's: a digit and an initial both work.
        log_key(&mut state, "3", false);
        assert_eq!(state.min_level, LogLevel::Info);
        log_key(&mut state, "e", false);
        assert_eq!(state.min_level, LogLevel::Error);

        // Esc closes on an empty filter, but a filter costs one key first.
        assert!(log_key(&mut state, "escape", false));
        state.search = "payload".into();
        assert!(!log_key(&mut state, "escape", false));
        assert!(state.search.is_empty());
        assert!(log_key(&mut state, "escape", false));

        // Inside the search box every printable key is text, Esc empties it,
        // and Enter leaves it without losing what was typed.
        log_key(&mut state, "/", false);
        assert!(state.search_active);
        log_key(&mut state, "t", false);
        log_key(&mut state, "e", false);
        log_key(&mut state, "s", false);
        log_key(&mut state, "t", false);
        assert_eq!(state.search, "test");
        assert_eq!(
            state.min_level,
            LogLevel::Error,
            "typing must not trip the level filter"
        );
        log_key(&mut state, "enter", false);
        assert!(!state.search_active);
        assert_eq!(state.search, "test");
        log_key(&mut state, "/", false);
        log_key(&mut state, "escape", false);
        assert!(!state.search_active);
        assert!(state.search.is_empty());
    }

    #[test]
    fn mission_visible_window_keeps_the_selection_on_screen() {
        // A short list is drawn whole, selection or not.
        assert_eq!(visible_window(3, None, 6), (0, 3));
        assert_eq!(visible_window(3, Some(2), 6), (0, 3));
        // A long one slides so the selection is the last row of the window...
        assert_eq!(visible_window(20, Some(0), 6), (0, 6));
        assert_eq!(visible_window(20, Some(6), 6), (1, 6));
        // ...and the end of the list lands flush with the bottom.
        assert_eq!(visible_window(20, Some(19), 6), (14, 6));
        // A selection left past the end by a shorter read still lands inside.
        assert_eq!(visible_window(20, Some(99), 6), (14, 6));
        assert_eq!(visible_window(0, Some(3), 6), (0, 0));
    }

    #[test]
    fn mission_grid_columns_follow_the_pane_width() {
        // Never zero: a collapsed or not-yet-measured window still draws one
        // column instead of a grid with no cells.
        assert_eq!(grid_columns(0.0, 16.0), 1);
        assert_eq!(grid_columns(f32::NAN, 16.0), 1);
        assert_eq!(grid_columns(300.0, 16.0), 1);
        // 22rem cards with a 0.5rem gap: n cards need 360n - 8 pixels at the
        // default rem, so these are the exact widths where a column appears.
        assert_eq!(grid_columns(712.0, 16.0), 2);
        assert_eq!(grid_columns(1072.0, 16.0), 3);
        assert_eq!(grid_columns(1432.0, 16.0), 4);
        // A very wide window stops at the cap rather than drawing slivers.
        assert_eq!(grid_columns(5000.0, 16.0), MAX_COLUMNS);
        // Zoom is part of the answer: at double rem the same width holds half
        // the cards, because a card is measured in rem.
        assert_eq!(grid_columns(1432.0, 32.0), 2);
        // A nonsense rem falls back to the default instead of dividing by zero.
        assert_eq!(grid_columns(712.0, 0.0), 2);
    }

    #[test]
    fn mission_grid_places_every_card_exactly_once() {
        // Ten cards over three columns: four rows, the last one padded so its
        // card keeps the width of the rows above.
        let rows = grid_rows(10, 3);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], vec![Some(0), Some(1), Some(2)]);
        assert_eq!(rows[3], vec![Some(9), None, None]);

        // The property the screen leans on: a card dropped here is a panel
        // nobody can see, which is how the analytics and schedule cards went
        // missing. Every index lands exactly once at every width.
        for cards in 0..=20usize {
            for cols in 1..=MAX_COLUMNS {
                let placed: Vec<usize> = grid_rows(cards, cols)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .collect();
                assert_eq!(
                    placed,
                    (0..cards).collect::<Vec<_>>(),
                    "cards={cards} cols={cols}"
                );
            }
        }

        // No cards, no rows; a zero column count is read as one.
        assert!(grid_rows(0, 3).is_empty());
        assert_eq!(grid_rows(2, 0), vec![vec![Some(0)], vec![Some(1)]]);
    }

    #[test]
    fn mission_window_keys_reload_only_when_the_window_changes() {
        // The terminal's capitals, and nothing else, name an analytics window.
        assert!(matches!(
            window_key(TimeWindow::All, "d"),
            WindowKey::Switch(TimeWindow::Day)
        ));
        assert!(matches!(
            window_key(TimeWindow::All, "w"),
            WindowKey::Switch(TimeWindow::Week)
        ));
        assert!(matches!(
            window_key(TimeWindow::All, "m"),
            WindowKey::Switch(TimeWindow::Month)
        ));
        assert!(matches!(
            window_key(TimeWindow::Day, "a"),
            WindowKey::Switch(TimeWindow::All)
        ));
        // Asking again for the window already shown costs no read, but the key
        // is still the filter's: `Same` is swallowed, never passed on to the
        // inbox, where `a` and `Shift+A` are both in play (#2426).
        assert!(matches!(window_key(TimeWindow::Day, "d"), WindowKey::Same));
        assert!(matches!(window_key(TimeWindow::All, "a"), WindowKey::Same));
        // Anything else leaves the window and the key alone.
        assert!(matches!(window_key(TimeWindow::All, "x"), WindowKey::Pass));
        assert!(matches!(window_key(TimeWindow::All, ""), WindowKey::Pass));
        assert!(matches!(window_key(TimeWindow::All, "r"), WindowKey::Pass));
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
    fn mission_nav_focus_change_resets_the_selection() {
        let snapshot = full_snapshot();
        let mut nav = Nav {
            panel: Panel::Activity,
            ..Nav::default()
        };
        nav.end(Some(&snapshot));
        assert_eq!(nav.selected, 2);
        nav.detail = true;
        nav.focus_next(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Schedule);
        assert_eq!(nav.selected, 0);
        assert!(!nav.detail);

        nav.panel = Panel::Inbox;
        nav.end(Some(&snapshot));
        assert_eq!(nav.selected, 2);
        nav.detail = true;
        nav.focus_prev(Some(&snapshot));
        assert_eq!(nav.panel, Panel::Schedule);
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

        let snapshot = full_snapshot();
        for panel in [
            Panel::Inbox,
            Panel::Activity,
            Panel::Schedule,
            Panel::Analytics,
        ] {
            let mut nav = Nav {
                panel,
                ..Nav::default()
            };
            nav.open_detail(Some(&snapshot));
            assert!(nav.detail, "{panel:?} should open its detail");
        }
    }
    /// A proposal the window's inbox can decide on: a trivial shell tool, so
    /// the install path writes real bytes into `tools.toml` and registers a
    /// real entry in the registry the bridge hands to the agent.
    fn shell_tool_def(name: &str) -> opencrabs::brain::tools::dynamic::DynamicToolDef {
        opencrabs::brain::tools::dynamic::DynamicToolDef {
            name: name.to_string(),
            description: "say hello".to_string(),
            executor: opencrabs::brain::tools::dynamic::ExecutorType::Shell,
            enabled: true,
            requires_approval: false,
            method: None,
            url: None,
            headers: std::collections::HashMap::new(),
            timeout_secs: 5,
            command: Some("echo hello".to_string()),
            params: Vec::new(),
        }
    }

    #[test]
    fn mission_decision_applies_and_rejects_through_the_terminals_tool() {
        // The window must decide through `RsiProposalsTool`, not its own
        // writes: this test proves an applied proposal really lands in
        // tools.toml and leaves the inbox, and that a rejected one is
        // archived without touching the file. Every path is the scratch home,
        // so the live instance is never written.
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().to_path_buf();
        with_home_override(home.clone(), || {
            let store = ProposalsStore::with_dir(home.join("rsi"));
            let tools_path = home.join("tools.toml");
            let registry = Arc::new(ToolRegistry::new());

            let keep = store
                .add_tool_proposal("rsi-cycle", "repetitive work", shell_tool_def("hello_tool"))
                .expect("file a tool proposal");
            let drop_it = store
                .add_tool_proposal("rsi-cycle", "same work again", shell_tool_def("bye_tool"))
                .expect("file a second tool proposal");
            assert_eq!(store.list_tool_proposals().len(), 2);

            let applied = decide_with(
                registry.clone(),
                &keep,
                &McInboxKind::ProposedTool,
                Decision::Apply,
                tools_path.clone(),
                home.clone(),
            );
            assert!(applied.is_ok(), "apply failed: {applied:?}");
            assert!(
                store.list_tool_proposals().iter().all(|p| p.id != keep),
                "an applied proposal leaves the inbox"
            );
            let written = std::fs::read_to_string(&tools_path).expect("tools.toml");
            assert!(
                written.contains("hello_tool"),
                "apply writes the tool where the loader reads it"
            );

            let rejected = decide_with(
                registry,
                &drop_it,
                &McInboxKind::ProposedTool,
                Decision::Reject,
                tools_path.clone(),
                home.clone(),
            );
            assert!(rejected.is_ok(), "reject failed: {rejected:?}");
            assert!(
                store.list_tool_proposals().is_empty(),
                "a rejected proposal leaves the inbox too"
            );
            let after = std::fs::read_to_string(&tools_path).expect("tools.toml");
            assert!(
                !after.contains("bye_tool"),
                "reject archives without installing anything"
            );
        });
    }
}
