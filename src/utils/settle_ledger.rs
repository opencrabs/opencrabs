//! Durable record of the plan tasks each session has already announced.
//!
//! The in-memory seen set (#2574, #2691) answers "did this task already ring
//! in THIS process". That is the wrong scope for the question a surface asks
//! on its first frame: a task that rang before the last restart looks brand
//! new, so every launch replays the rail flash for a plan that has not moved
//! (#3004). The record here outlives the process, and it is keyed by session
//! so one conversation's settles never answer for another's.
//!
//! The write is immediate and atomic, in the same call that records the task:
//! a panic between the flash and the write would replay that one task, and a
//! process that dies with an unwritten record is exactly the case this exists
//! to survive. A record that cannot be read is treated as empty, because
//! replaying one settle is a smaller sin than never announcing again.
//!
//! No new plan state is needed to know WHICH task settled: `PlanTask::id` is
//! stable in the plan file, so an id recorded here keeps matching across
//! restarts. A task that settles while the app is closed is not in the record
//! yet, so it still announces once when the window opens: that is the
//! documented intent, and this module keeps it while dropping the replay.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::tui::plan::PlanDocument;

/// File name under the profile home. One record per profile, sessions keyed
/// inside it, so a session's entry never answers for a different one.
const STORE_FILE: &str = "notify_seen.json";

/// Bumped only if the on-disk shape changes in a way older binaries cannot
/// read. An unknown version still parses: every field is optional.
const STORE_VERSION: u32 = 1;

/// Entries older than this are dropped on the next write. A session that has
/// not settled anything in three months is not going to be reopened with a
/// live plan; keeping it would grow the file forever.
const MAX_AGE_DAYS: i64 = 90;

/// Upper bound per session. A plan holds a handful of tasks, so this only
/// catches a corrupt or hostile file; when it trips the session's record is
/// cleared rather than truncated, because truncating a `BTreeSet` would drop
/// arbitrary ids and replay them.
const MAX_TASKS_PER_SESSION: usize = 4096;

fn store_version() -> u32 {
    STORE_VERSION
}

#[derive(Debug, Serialize, Deserialize)]
struct Store {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    sessions: BTreeMap<Uuid, SessionRecord>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            sessions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SessionRecord {
    #[serde(default)]
    tasks: BTreeSet<Uuid>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
}

impl Store {
    /// Drop records no plan can still reference, and bound a pathological one.
    fn prune(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::days(MAX_AGE_DAYS);
        // A record with no timestamp is kept: it was written by a binary that
        // predates the stamp, and dropping it would replay every task in it.
        self.sessions
            .retain(|_, record| record.updated_at.is_none_or(|stamp| stamp >= cutoff));
        for record in self.sessions.values_mut() {
            if record.tasks.len() > MAX_TASKS_PER_SESSION {
                record.tasks.clear();
            }
        }
    }
}

/// Read the record at `path`. A missing file is an empty record; so is a file
/// that does not parse, with one line saying why. Never panics: this runs on
/// the first frame of a window and on every settle.
fn load(path: &Path) -> Store {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Store::default();
    };
    match serde_json::from_str(&text) {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!(
                "announce record at {} did not parse, starting empty: {e}",
                path.display()
            );
            Store::default()
        }
    }
}

/// Merge `tasks` into `session`'s record and write the file.
///
/// The file is re-read first rather than written from a snapshot: the
/// terminal and the window can run at the same time, and each only knows what
/// IT announced. Merging on the current file keeps the other surface's
/// entries instead of replacing them with the state read at startup.
fn persist(path: &Path, session: Uuid, tasks: &[Uuid]) {
    let mut store = load(path);
    let record = store.sessions.entry(session).or_default();
    record.tasks.extend(tasks.iter().copied());
    record.updated_at = Some(Utc::now());
    store.prune();
    let text = match serde_json::to_string_pretty(&store) {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!("announce record for {session} not serialized: {e}");
            return;
        }
    };
    if let Err(e) = crate::config::types::io::atomic_write(path, &text) {
        tracing::warn!("announce record at {} not written: {e}", path.display());
    }
}

/// What each session has already announced, plus the process-local seen set.
///
/// One ledger per surface. The in-memory set is the union across sessions,
/// which is safe because a task id belongs to exactly one plan; the durable
/// record is what carries that union across a restart.
pub struct SettleLedger {
    /// Tasks this process has already accounted for, across every session.
    seen: HashSet<Uuid>,
    /// Sessions whose durable record has been folded into `seen` already, so
    /// the file is read once per session per process rather than per tick.
    seeded: HashSet<Uuid>,
    /// Where the durable record lives.
    path: PathBuf,
}

impl SettleLedger {
    /// The profile's record, under the OpenCrabs home.
    pub fn open() -> Self {
        Self::at(crate::config::opencrabs_home().join(STORE_FILE))
    }

    /// A ledger over an explicit file. Tests use this so nothing touches the
    /// live home; a caller with its own profile can point it elsewhere.
    pub fn at(path: PathBuf) -> Self {
        Self {
            seen: HashSet::new(),
            seeded: HashSet::new(),
            path,
        }
    }

    /// Record the settled tasks of one plan, against the session that owns it.
    ///
    /// Returns the ids that are new in this process AND absent from the
    /// durable record, which is exactly what a surface flashes for. The first
    /// call for a session folds its persisted ids in silently, so a relaunch
    /// after a clean exit, a panic, or a machine restart resumes instead of
    /// replaying.
    ///
    /// `Uuid::nil()` means the plan has no session yet (a pre-init sidecar
    /// whose hook has not stamped it). It still dedupes in memory, but there
    /// is nothing durable to key it on, so nothing is written.
    pub fn record(&mut self, session: Uuid, plan: Option<&PlanDocument>) -> Vec<Uuid> {
        if session != Uuid::nil() && self.seeded.insert(session) {
            // Read at first sight, not at construction: another surface may
            // have written since this ledger was opened.
            let record = load(&self.path).sessions.remove(&session);
            if let Some(record) = record {
                self.seen.extend(record.tasks);
            }
        }
        let fresh = crate::utils::desktop_notify::fresh_settled_tasks(&mut self.seen, &[plan]);
        if !fresh.is_empty() && session != Uuid::nil() {
            persist(&self.path, session, &fresh);
        }
        fresh
    }

    /// The tasks `session` has announced, read from the durable record.
    ///
    /// Sorted so a caller or a test can compare it without owning the file
    /// format. Empty when the session has never announced anything.
    pub fn announced(&self, session: Uuid) -> Vec<Uuid> {
        load(&self.path)
            .sessions
            .remove(&session)
            .map(|record| record.tasks.into_iter().collect())
            .unwrap_or_default()
    }
}
