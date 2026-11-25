//! Retirement of dead plan files (#3107).
//!
//! The classify-only sweep in [`crate::utils::plan_files::PlanIndex`] has a
//! hard guard: it never archives or deletes what it reads, because drawing a
//! list must never touch somebody's history. That guard is what leaves 713
//! plan files older than 30 days (measured 2026-11-20) reporting in-flight
//! state on every surface.
//!
//! This module is the other half: an EXPLICIT maintenance pass that a
//! background caller runs on its own schedule, never the paint path. It
//! retires (archives, never deletes) plan files that are stale on both
//! axes the issue defines: the file has not moved in a long time AND the
//! session has not written a message in that time. Either axis fresh means
//! keep: a long build can hold a file still, and a live conversation can
//! keep rewriting an old plan.
//!
//! Like the loader's own retirement, files MOVE into the session's
//! `archive/` with a timestamp, so nothing is lost, and every stale copy of
//! the plan id across all roots goes with it, so a surviving duplicate cannot
//! resurrect the corpse on the next sweep (#3107 receipt: the MC plan was
//! archived into `projects/opencrabs/session/archive/` while a second copy
//! survived in the legacy `session/` root).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

use super::{PLAN_PREFIX, PLAN_SUFFIX};

/// Archived plan files drop the leading dot and gain a timestamp BEFORE the
/// suffix: `archive/opencrabs_plan_<session>-<YYYYmmdd-HHMMSS>.json`, the
/// exact naming the loader's own retirement (`archive_plan_files`) writes, so
/// its index recognizes these as read-only history, not live state.
/// A plan file must be stale on BOTH axes for at least this long before it
/// retires. Thirty days, matching the issue's own measurement bucket.
/// Retirement removes the row from every surface, so it stays conservative.
pub const RETIRE_AFTER: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// One retired plan id, for the caller's log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredPlan {
    pub session: Uuid,
    /// Every path that moved (json and md copies, across all roots).
    pub moved: Vec<PathBuf>,
}

/// Scan every root for plan files stale on both axes and move every stale
/// copy of each dead plan id into that root's `archive/` with a timestamp.
///
/// `last_activity` maps a session id to the time of its most recent message;
/// a session missing from the map counts as never active, so its plan
/// retires purely on file age. `now` is injectable for tests.
pub fn retire_dead_plans(
    roots: &[PathBuf],
    last_activity: &HashMap<Uuid, SystemTime>,
    now: SystemTime,
) -> Vec<RetiredPlan> {
    // First pass: find the dead plan ids and every copy of each, across all
    // roots. A stale copy whose plan id also has a FRESH copy elsewhere is
    // superseded history: archive the stale revision, leave the fresh one
    // alone, so a duplicated id cannot survive retirement but a live plan is
    // never touched.
    let mut dead: HashMap<Uuid, Vec<PathBuf>> = HashMap::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = plan_id_of(&path) else {
                continue;
            };
            if is_stale(&path, last_activity.get(&id).copied(), now) {
                dead.entry(id).or_default().push(path);
            }
        }
    }
    let mut retired = Vec::new();
    for (id, mut paths) in dead {
        paths.sort();
        let mut moved = Vec::new();
        for path in paths {
            if archive_copy(&path, now).is_ok() {
                moved.push(path);
            }
        }
        if !moved.is_empty() {
            retired.push(RetiredPlan { session: id, moved });
        }
    }
    retired.sort_by_key(|r| r.session);
    retired
}

/// The session id a plan path carries, or `None` for anything else. Both the
/// live json and its mirrored design `.md` carry the id, so both are copies.
fn plan_id_of(path: &Path) -> Option<Uuid> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix(PLAN_PREFIX)?;
    let stem = rest
        .strip_suffix(PLAN_SUFFIX)
        .or_else(|| rest.strip_suffix(".md"))?;
    Uuid::parse_str(stem).ok()
}

/// Stale on BOTH axes: the file's own mtime and the session's last message
/// are both older than [`RETIRE_AFTER`]. A session with no recorded activity
/// counts as never active, so only file age applies.
fn is_stale(path: &Path, last_activity: Option<SystemTime>, now: SystemTime) -> bool {
    let file_old = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .is_some_and(|m| now.duration_since(m).is_ok_and(|age| age > RETIRE_AFTER));
    if !file_old {
        return false;
    }
    match last_activity {
        Some(at) => now.duration_since(at).is_ok_and(|age| age > RETIRE_AFTER),
        None => true,
    }
}

/// Move one file (json or md) into its root's `archive/` with a timestamp,
/// the same naming the loader's own retirement writes.
fn archive_copy(path: &Path, now: SystemTime) -> std::io::Result<()> {
    let dir = path
        .parent()
        .map(|p| p.join("archive"))
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    std::fs::create_dir_all(&dir)?;
    let ts = timestamp(now);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("non-utf8 name"))?;
    // Archive names insert `-<ts>` BEFORE the suffix so the extension
    // survives, and strip the leading dot with the prefix itself:
    // `opencrabs_plan_<session>-<ts>.json`.
    // The loader writes `format!("{stem}-{ts}.json")` straight from
    // `file_stem().trim_start_matches('.')`, with NO prefix prepended: the
    // prefix is already inside the stem. Rsplit the extension off and replay
    // that byte-for-byte.
    let (base, ext) = name
        .rsplit_once('.')
        .ok_or_else(|| std::io::Error::other("plan file has no extension"))?;
    let base = base.trim_start_matches('.');
    let target = dir.join(format!("{base}-{ts}.{ext}"));
    std::fs::rename(path, target)
}

/// The `%Y%m%d-%H%M%S` stamp the archive naming uses, from a SystemTime so
/// tests can pin it.
fn timestamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y,
        mo,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Days since the epoch to (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The startup retirement pass: resolve this profile's plan roots, read each
/// session's last activity from the DB, and retire what is dead on both
/// axes. Best effort by design: a missing DB or a busy store skips the pass
/// with a log line and the files wait for the next startup.
pub async fn retire_dead_plans_at_startup() -> anyhow::Result<()> {
    use crate::config::opencrabs_home;

    let home = opencrabs_home();
    let mut roots = vec![home.join("session"), home.join("agents").join("session")];
    if let Ok(entries) = std::fs::read_dir(home.join("projects")) {
        let mut per_project: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("session"))
            .filter(|dir| dir.is_dir())
            .collect();
        per_project.sort();
        roots.extend(per_project);
    }
    if !roots.iter().any(|r| r.is_dir()) {
        return Ok(());
    }
    let activity = last_message_times(&home).await?;
    if activity.is_empty() {
        return Ok(());
    }
    let now = SystemTime::now();
    let retired = retire_dead_plans(&roots, &activity, now);
    for plan in &retired {
        tracing::info!(
            "🧹 Retired dead plan for session {}: {} file(s) moved to archive (#3107)",
            plan.session,
            plan.moved.len()
        );
    }
    Ok(())
}

/// Each session's most recent message time, straight from the store. Any
/// turn touches the session row, so `sessions.updated_at` IS the last
/// activity signal; a session with no row counts as never active.
async fn last_message_times(home: &Path) -> anyhow::Result<HashMap<Uuid, SystemTime>> {
    use crate::db::{Database, interact_err};

    let db_path = home.join("opencrabs.db");
    if !db_path.exists() {
        return Ok(HashMap::new());
    }
    let db = Database::connect(&db_path).await?;
    db.run_migrations().await?;
    let rows: Vec<(String, i64)> = db
        .pool()
        .get()
        .await
        .map_err(|e| anyhow::anyhow!("pool: {e}"))?
        .interact(|conn| {
            conn.prepare_cached("SELECT id, updated_at FROM sessions")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, rusqlite::Error>>()
        })
        .await
        .map_err(interact_err)?
        .map_err(|e| anyhow::anyhow!("query: {e}"))?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, ts)| {
            let id = Uuid::parse_str(&id).ok()?;
            Some((
                id,
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts.max(0) as u64),
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Write a live plan file and age it by `age`.
    fn seed_plan(dir: &Path, session: Uuid, age: Duration, now: SystemTime) -> PathBuf {
        let path = dir.join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, format!(r#"{{"status":"Active"}}"#)).expect("write plan");
        age_file(&path, age, now);
        path
    }

    /// Push a file's mtime to `now - age`.
    fn age_file(path: &Path, age: Duration, now: SystemTime) {
        let when = now.checked_sub(age).expect("clock");
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("set mtime");
    }

    /// A timestamp far outside the retirement window.
    fn old(now: SystemTime) -> SystemTime {
        now.checked_sub(RETIRE_AFTER * 2).expect("clock")
    }

    #[test]
    fn retires_dead_plans_to_session_archive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let dead = sid(1);
        let live = sid(2);
        seed_plan(&root, dead, RETIRE_AFTER * 3, now);
        seed_plan(&root, live, Duration::from_secs(10), now);

        // The dead session last spoke half a year ago; the live one now.
        let mut activity = HashMap::new();
        activity.insert(dead, old(now));
        activity.insert(live, now);

        let retired = retire_dead_plans(&[root.clone()], &activity, now);
        assert_eq!(retired.len(), 1, "only the dead plan retires");
        assert_eq!(retired[0].session, dead);
        let archive = root.join("archive");
        let landed: Vec<_> = std::fs::read_dir(&archive)
            .expect("archive exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(landed.len(), 1, "the plan json moved to archive");
        // Loader-identical shape: `opencrabs_plan_<uuid>-<ts>.json`, the
        // prefix built by file_stem, not by a prepended constant.
        assert!(landed[0].starts_with(&format!(
            "{}{}-",
            PLAN_PREFIX.trim_start_matches('.'),
            dead
        )));
        assert!(
            !root
                .join(format!("{PLAN_PREFIX}{dead}{PLAN_SUFFIX}"))
                .exists(),
            "the live copy is gone"
        );
        assert!(
            root.join(format!("{PLAN_PREFIX}{live}{PLAN_SUFFIX}"))
                .exists(),
            "the live session's plan stays put"
        );
    }

    #[test]
    fn a_recent_message_keeps_an_old_plan_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_001);
        let session = sid(3);
        seed_plan(&root, session, RETIRE_AFTER * 3, now);
        let mut activity = HashMap::new();
        // The file is old but the session spoke yesterday.
        activity.insert(session, now - Duration::from_secs(86_400));
        assert!(retire_dead_plans(&[root], &activity, now).is_empty());
    }

    #[test]
    fn a_stale_duplicate_retires_while_its_fresh_twin_stays_put() {
        // The bug receipt from #3107: a duplicate in another root. The stale
        // copy retires; the fresh one stays, and the id stays resolvable.
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).expect("dir");
        std::fs::create_dir_all(&b).expect("dir");
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_002);
        let session = sid(4);
        let stale = seed_plan(&a, session, RETIRE_AFTER * 3, now);
        let fresh = seed_plan(&b, session, Duration::from_secs(5), now);
        let retired = retire_dead_plans(&[a, b], &HashMap::new(), now);
        assert_eq!(retired.len(), 1);
        assert!(!stale.exists());
        assert!(fresh.exists(), "the fresh copy is never touched");
    }

    #[test]
    fn a_session_with_no_recorded_activity_retires_on_file_age_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_003);
        let session = sid(5);
        seed_plan(&root, session, RETIRE_AFTER * 3, now);
        let retired = retire_dead_plans(&[root], &HashMap::new(), now);
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].session, session);
    }

    #[test]
    fn the_design_md_moves_with_its_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_004);
        let session = sid(6);
        let json = seed_plan(&root, session, RETIRE_AFTER * 3, now);
        let md = json.with_extension("md");
        std::fs::write(&md, "# plan").expect("write md");
        age_file(&md, RETIRE_AFTER * 3, now);
        let retired = retire_dead_plans(&[root.clone()], &HashMap::new(), now);
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].moved.len(), 2, "json and md both moved");
        assert!(!md.exists());
        let archive = root.join("archive");
        let names: Vec<_> = std::fs::read_dir(&archive)
            .expect("archive")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|n| n.ends_with(".md")), "md archived too");
    }

    #[test]
    fn a_recently_written_file_is_never_touched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_005);
        let session = sid(7);
        seed_plan(&root, session, Duration::from_secs(60), now);
        assert!(retire_dead_plans(&[root], &HashMap::new(), now).is_empty());
    }
}
