//! What the sessions rail says about work in flight (#2636).
//!
//! The rail answers one question per row without opening the conversation:
//! is something happening there, and does the owner have to act? Two signals,
//! two meanings:
//!
//! | Signal | Meaning | Clock |
//! |---|---|---|
//! | braille spinner by the name | a turn is running right now | 80 ms |
//! | slow ring around the row | a plan is written and waits for approval | 2.4 s |
//!
//! The frames are the chat's own ([`crate::transcript::SPINNER_FRAMES`], the
//! TUI's order), so the rail blinks in phase with the turn card instead of
//! inventing a second look for the same event.
//!
//! The live half is memory only: the window's own `busy`, each split leaf's
//! `busy`, and the progress stream of a session with no leaf, which is what a
//! sub-agent looks like (#2438). Nothing here reads a file or the DB, so it is
//! safe to ask for every row on every paint.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

use gpui::{IntoElement, ParentElement, Styled, div, rgba};
use opencrabs::tui::plan::{PlanDocument, PlanStatus, TaskStatus};

use crate::palette;

/// The chat spinner's cadence, so the rail and the turn card blink together.
pub const FRAME_MS: u128 = 80;

/// The spinner's decision for one row. A turn running now outranks a plan,
/// because the plan badge would describe the same work one step removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// A turn is running in this process for that session.
    Live,
    /// An approved plan on that session holds a task in flight.
    Plan,
}

/// Which badge a row draws, or `None` for nothing.
///
/// One function, two booleans, because precedence is the whole decision: the
/// row must not draw two spinners, and it must not draw the plan badge for a
/// plan whose own work is already being reported by the live one.
pub fn badge(live: bool, plan_running: bool) -> Option<Badge> {
    if live {
        Some(Badge::Live)
    } else if plan_running {
        Some(Badge::Plan)
    } else {
        None
    }
}

/// The frame for `phase`, the window's single monotonic origin.
///
/// Every row reads the same phase, so the rail blinks as one clock instead of
/// one clock per row, and a list of ten busy sessions cannot turn into visual
/// noise.
pub fn frame(phase: Duration) -> &'static str {
    let frames = crate::transcript::SPINNER_FRAMES;
    frames[(phase.as_millis() / FRAME_MS) as usize % frames.len()]
}

/// The in-memory half of the badge: which sessions are streaming with no pane
/// of their own.
///
/// The open turn and the split leaves already keep their own `busy` flag, so
/// mirroring those here would be a second copy that can drift from the first.
/// This set holds only what no other field records: a session whose progress
/// arrives on the event channel and has nowhere to be folded, because it has
/// no leaf. A sub-agent runs exactly that way.
#[derive(Default)]
pub struct RailActivity {
    streaming: HashSet<Uuid>,
}

impl RailActivity {
    /// Record that `session` is streaming with no pane of its own.
    ///
    /// Returns whether this is the first sighting, so the caller can skip a
    /// repaint on the repeats: progress events arrive many times a second and
    /// only the transition changes what the row draws.
    pub fn observe_stream(&mut self, session: Uuid) -> bool {
        self.streaming.insert(session)
    }

    /// A turn ended. Returns whether a badge actually went away, so a `Done`
    /// for a session that was never recorded costs no repaint.
    pub fn clear(&mut self, session: Uuid) -> bool {
        self.streaming.remove(&session)
    }

    /// Whether this session has work in flight, from any of the four places it
    /// can be: the open turn, a split leaf's turn, a detached command, or
    /// progress arriving for a session with no leaf at all.
    ///
    /// The detached one is not redundant: a background command takes the turn
    /// idle, so a long build would otherwise leave the rail silent for exactly
    /// the sessions that are working longest (#762).
    pub fn is_live(&self, session: Uuid, busy: bool, pane_busy: bool, detached: bool) -> bool {
        busy || pane_busy || detached || self.streaming.contains(&session)
    }

    /// How many sessions are streaming with no pane.
    ///
    /// The rail reads it to decide whether a live preview refresh is worth a
    /// database query: with nothing streaming, the last message cannot have
    /// changed since the last refresh. The tests use the same count to prove
    /// the set does not grow past the sessions that are actually running.
    pub fn streaming_count(&self) -> usize {
        self.streaming.len()
    }
}

/// The ring's own clock: one pulse per 2.4 s, far slower than the spinner's
/// 80 ms so a plan waiting for approval reads as breathing rather than working.
pub const RING_PERIOD_MS: u128 = 2400;

/// The ring's alpha for `phase`, the same window clock the spinner reads.
///
/// A triangle wave rather than a sine: it needs no float trig on the paint
/// path, it never snaps back to dim at the wrap, and the eye cannot tell the
/// difference at this amplitude.
pub fn ring_alpha(phase: Duration) -> u8 {
    let t = (phase.as_millis() % RING_PERIOD_MS) as f32 / RING_PERIOD_MS as f32;
    let tri = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
    (RING_MIN_ALPHA as f32 + tri * (RING_MAX_ALPHA - RING_MIN_ALPHA) as f32) as u8
}

/// The ring's dimmest and brightest alpha: visible on a dark rail at 60, still
/// an edge rather than a highlight at 230.
const RING_MIN_ALPHA: u8 = 60;
const RING_MAX_ALPHA: u8 = 230;

/// The plans the owner has already opened, so a ring goes away once it has
/// been read and comes back only for a NEW plan in the same session.
///
/// Keyed by the file's stamp, not the session: a plan rewritten in a session
/// the owner opened a week ago is news again. Persisted, or every restart
/// would re-ring everything already read.
#[derive(Default)]
pub struct SeenPlans {
    path: Option<PathBuf>,
    seen: HashMap<Uuid, u64>,
}

impl SeenPlans {
    /// Load from `path`, or hold nothing when there is no file yet. An
    /// unreadable or stale file is treated as empty: a lost "seen" mark costs
    /// one extra ring, never a broken rail.
    pub fn load(path: Option<PathBuf>) -> Self {
        let seen = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<HashMap<Uuid, u64>>(&text).ok())
            .unwrap_or_default();
        Self { path, seen }
    }

    /// Whether this exact plan was read. Equality, not "at or before": the key
    /// is the file's own stamp, so a plan rewritten after it was read is news
    /// again, and marking a newer one must never silence an older one.
    pub fn is_seen(&self, session: Uuid, stamp: SystemTime) -> bool {
        let Ok(since) = stamp.duration_since(std::time::UNIX_EPOCH) else {
            return false;
        };
        let stamp = since.as_millis() as u64;
        self.seen.get(&session).is_some_and(|seen| *seen == stamp)
    }

    /// Record that the owner opened this plan. Best effort on disk: a rail is
    /// not worth a panic over a write the user never asked to see.
    pub fn mark(&mut self, session: Uuid, stamp: SystemTime) {
        let Ok(since) = stamp.duration_since(std::time::UNIX_EPOCH) else {
            return;
        };
        let stamp = since.as_millis() as u64;
        if self.seen.get(&session) == Some(&stamp) {
            return;
        }
        self.seen.insert(session, stamp);
        // A window left open for weeks would otherwise grow one entry per
        // conversation it ever showed.
        if self.seen.len() > 512 {
            let mut stamps: Vec<(Uuid, u64)> = self.seen.iter().map(|(k, v)| (*k, *v)).collect();
            stamps.sort_by_key(|(_, stamp)| *stamp);
            for (session, _) in stamps.into_iter().take(self.seen.len() - 512) {
                self.seen.remove(&session);
            }
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if let Ok(text) = serde_json::to_string(&self.seen) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// The plan-file prefix: the loader writes `.opencrabs_plan_<session>.json`
/// beside the design `.md` it mirrors.
const PLAN_PREFIX: &str = ".opencrabs_plan_";
const PLAN_SUFFIX: &str = ".json";

/// How long a plan file counts as live evidence after its own last write.
pub const FRESH_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How often a session id the index has never seen may force it to re-sweep
/// the roots.
///
/// The sweep is `read_dir` calls only. The per-file `stat` runs for the rows
/// the rail actually draws, never for the whole store: this box holds 3600
/// plan files, and a rail that statted all of them twenty times a second would
/// be a syscall storm for rows nobody is looking at.
pub const SWEEP_EVERY: Duration = Duration::from_secs(5);

/// What a session's plan file says that the rail draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanEvidence {
    /// Written and waiting for the owner: the row rings.
    pub ready: bool,
    /// Approved, with a task in flight: the row spins.
    pub running: bool,
    /// The file's own mtime, which is what "this plan" means for the ring's
    /// seen key: a new plan in the same session writes a new stamp, so the
    /// ring comes back for it even though the session id did not change.
    pub stamp: SystemTime,
}

/// One row's whole answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowState {
    /// The spinner to draw, if any.
    pub badge: Option<Badge>,
    /// The row rings: a plan waits for approval.
    pub ring: bool,
}

/// The id to plan-file index, with the two caches that keep it off the paint
/// path: the paths themselves (the sweep is throttled) and the parsed
/// documents (keyed by mtime, exactly as `PlanStripState` caches its own).
#[derive(Default)]
pub struct PlanIndex {
    /// The directories to sweep. `None` resolves the defaults once, because
    /// the home is only known at run time.
    roots: Option<Vec<PathBuf>>,
    paths: HashMap<Uuid, PathBuf>,
    swept_at: Option<Instant>,
    docs: HashMap<PathBuf, (SystemTime, Option<PlanDocument>)>,
}

impl PlanIndex {
    /// An index over explicit roots. The tests use it; production resolves the
    /// profile's own directories through [`PlanIndex::default`].
    #[cfg(test)]
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: Some(roots),
            ..Default::default()
        }
    }

    /// The directories a plan file can live in, in the order they are swept:
    /// the profile's `session/`, the legacy `agents/session/`, then one
    /// `session/` per project.
    fn roots(&mut self) -> Vec<PathBuf> {
        if self.roots.is_none() {
            self.roots = Some(default_roots());
        }
        self.roots.clone().unwrap_or_default()
    }

    /// Rebuild the id to path map. A session that moved projects leaves its old
    /// plan behind, so the same id can appear twice; the newer write is the
    /// live one, and only that duplicate pays for the extra stat.
    fn sweep(&mut self, now: Instant) {
        let mut paths: HashMap<Uuid, PathBuf> = HashMap::new();
        for root in self.roots() {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|name| {
                    name.strip_prefix(PLAN_PREFIX)
                        .and_then(|rest| rest.strip_suffix(PLAN_SUFFIX))
                        .and_then(|stem| Uuid::parse_str(stem).ok())
                }) else {
                    continue;
                };
                match paths.get(&id) {
                    Some(existing) => {
                        if mtime(&path) > mtime(existing) {
                            paths.insert(id, path);
                        }
                    }
                    None => {
                        paths.insert(id, path);
                    }
                }
            }
        }
        self.paths = paths;
        self.swept_at = Some(now);
        // A long-lived window sees sessions come and go; keeping every document
        // it ever parsed would leak one small struct per conversation.
        if self.docs.len() > 512 {
            let live: HashSet<&PathBuf> = self.paths.values().collect();
            self.docs.retain(|path, _| live.contains(path));
        }
    }

    /// Parse the plan at `path`, or hand back the cached one. The mtime is the
    /// key, so a rewritten plan redraws on the next ask and an untouched one
    /// costs a `stat`, not a parse.
    fn doc_at(&mut self, path: &Path, mtime: SystemTime) -> Option<&PlanDocument> {
        let cached = matches!(self.docs.get(path), Some((at, _)) if *at == mtime);
        if !cached {
            let doc = read_plan(path);
            self.docs.insert(path.to_path_buf(), (mtime, doc));
        }
        self.docs.get(path).and_then(|(_, doc)| doc.as_ref())
    }

    /// The plan file behind `session` and its current stamp, or `None`.
    ///
    /// The sweep is throttled for ids the index has never seen; a known path
    /// costs one `stat`. The badge question and the progress question both
    /// resolve through here, so they can never disagree about which file is
    /// this session's plan.
    fn resolve(&mut self, session: Uuid, now: Instant) -> Option<(PathBuf, SystemTime)> {
        if !self.paths.contains_key(&session) {
            let due = self
                .swept_at
                .is_none_or(|at| now.duration_since(at) >= SWEEP_EVERY);
            if due {
                self.sweep(now);
            }
        }
        let path = self.paths.get(&session)?.clone();
        let stamp = mtime(&path)?;
        Some((path, stamp))
    }

    /// What the rail should draw for `session`, or `None` when its plan says
    /// nothing worth animating.
    ///
    /// `live` is whether a turn for that session is running in this process.
    /// It outranks the freshness window: a build can hold a plan file still for
    /// longer than the window, and the work would then read as abandoned.
    pub fn evidence(&mut self, session: Uuid, live: bool, now: Instant) -> Option<PlanEvidence> {
        let (path, stamp) = self.resolve(session, now)?;
        let doc = self.doc_at(&path, stamp)?;
        let state = plan_state(doc, &path)?;
        match state {
            // A design draft or a seeded checklist is written and waits for the
            // owner: the row rings until it is opened. Nothing executes yet, so
            // no spinner.
            PlanStatus::Editing => Some(PlanEvidence {
                ready: true,
                running: false,
                stamp,
            }),
            PlanStatus::Active => {
                let running_task = doc
                    .tasks
                    .iter()
                    .any(|t| matches!(t.status, TaskStatus::InProgress));
                if !running_task {
                    return None;
                }
                // Measured on this box: 26 of 27 plans with a task in flight
                // were abandoned, the oldest 100 days old. A badge lit from the
                // file alone would animate for months-old corpses, so the file
                // has to be recent or the turn has to be live here.
                let fresh = SystemTime::now()
                    .duration_since(stamp)
                    .is_ok_and(|age| age < FRESH_WINDOW);
                (live || fresh).then_some(PlanEvidence {
                    ready: false,
                    running: true,
                    stamp,
                })
            }
        }
    }

    /// The plan's checklist progress for the rail card: `(done, total)`.
    ///
    /// Only an approved plan with tasks has a bar to draw: a design draft has
    /// no checklist yet, and an empty one would read as zero of nothing. The
    /// counts come from the strip's own rule, so the card and the composer
    /// strip agree about the same plan. No freshness gate here: the bar is
    /// state, not animation, and 4 of 8 done stays true while the badge sleeps.
    pub fn progress(&mut self, session: Uuid, now: Instant) -> Option<(usize, usize)> {
        let (path, stamp) = self.resolve(session, now)?;
        let doc = self.doc_at(&path, stamp)?;
        if !matches!(plan_state(doc, &path)?, PlanStatus::Active) || doc.tasks.is_empty() {
            return None;
        }
        Some(crate::plan_strip::progress_counts(doc))
    }
}

/// The plan directories this profile writes to.
fn default_roots() -> Vec<PathBuf> {
    let home = opencrabs::config::opencrabs_home();
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
    roots
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Read a plan file without the loader's side effects.
///
/// `load_plan_from_path` archives a Completed plan and DELETES a Cancelled one,
/// which is right when the user opens that session and wrong for a sweep that
/// touches every plan the rail can see: drawing a list must never delete
/// somebody's history.
fn read_plan(path: &Path) -> Option<PlanDocument> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw: serde_json::Value = serde_json::from_str(&text).ok()?;
    // The loader's own terminal rule: those two end a plan's life and are
    // never drawn.
    match raw.get("status").and_then(|s| s.as_str()) {
        Some("Completed") | Some("Cancelled") => return None,
        _ => {}
    }
    serde_json::from_value(raw).ok()
}

/// The plan's state, with the loader's legacy rule applied in memory: an
/// Editing checklist that has tasks, no design doc and no approval marker
/// predates the design/checklist split and was executable. Reading it as
/// "waiting for approval" would ring forever for a plan that already ran.
///
/// `None` means there is no plan here worth drawing: `/plan` was entered but
/// `plan init` never landed, so the file holds a flag rather than a plan.
fn plan_state(doc: &PlanDocument, json_path: &Path) -> Option<PlanStatus> {
    if doc.status != PlanStatus::Editing {
        return Some(doc.status);
    }
    let md_exists = json_path.with_extension("md").exists();
    if doc.pre_init_editing && !md_exists {
        return None;
    }
    if !doc.pending_approval && !md_exists && !doc.tasks.is_empty() {
        return Some(PlanStatus::Active);
    }
    Some(PlanStatus::Editing)
}

/// The row's whole answer: the spinner from whatever is live now, plus the
/// plan's own evidence. Assembled in one place so the rail cannot apply the
/// precedence twice and get two different answers.
///
/// `seen` is whether the owner already opened this exact plan; a ring that has
/// been read must not keep pulsing for attention.
pub fn row_state(live: bool, plan: Option<PlanEvidence>, seen: bool) -> RowState {
    let plan = plan.filter(|p| p.ready || p.running);
    RowState {
        badge: badge(live, plan.is_some_and(|p| p.running)),
        ring: plan.is_some_and(|p| p.ready) && !seen,
    }
}

/// Frame a rail row so a waiting plan reads without turning the row into a card
/// (#2540 keeps rows square and edge to edge; this is a frame, not chrome).
///
/// The border is always laid out and only its colour moves. Adding a border
/// when the ring starts would push every row below it down a pixel, and a rail
/// that jumps when a plan appears is worse than no ring at all.
pub fn ring(row: impl IntoElement, on: bool, phase: Duration) -> impl IntoElement {
    let colour = if on {
        // `rgba` packs 0xRRGGBBAA, so the teal keeps its channels and only the
        // alpha breathes.
        rgba((palette::current().teal << 8) | u32::from(ring_alpha(phase)))
    } else {
        // Same cell, nothing painted.
        rgba(0)
    };
    div().w_full().border_1().border_color(colour).child(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn rail_activity_a_streaming_session_reads_live_with_no_busy_flag() {
        let mut rail = RailActivity::default();
        let session = sid(1);
        assert!(!rail.is_live(session, false, false, false));

        // A sub-agent's first progress event is the only trace of its turn.
        assert!(rail.observe_stream(session));
        assert!(rail.is_live(session, false, false, false));

        // The repeats must not report a transition: the row already changed.
        assert!(!rail.observe_stream(session));
    }

    #[test]
    fn rail_activity_clearing_a_finished_turn_drops_the_badge_once() {
        let mut rail = RailActivity::default();
        let session = sid(2);
        rail.observe_stream(session);
        assert!(rail.is_live(session, false, false, false));

        assert!(rail.clear(session));
        assert!(!rail.is_live(session, false, false, false));

        // A second Done for the same session is not a change.
        assert!(!rail.clear(session));
    }

    #[test]
    fn rail_activity_the_open_turn_and_the_split_leaves_need_no_recording() {
        // Both already carry their own flag, so the set stays empty and the
        // badge still lights: deriving beats mirroring.
        let rail = RailActivity::default();
        let open = sid(3);
        let leaf = sid(4);
        assert_eq!(rail.streaming_count(), 0);
        assert!(rail.is_live(open, true, false, false));
        assert!(rail.is_live(leaf, false, true, false));
        // A detached command holds the turn idle and still counts as work.
        assert!(rail.is_live(open, false, false, true));
        // With nothing running anywhere, the row stays quiet.
        assert!(!rail.is_live(open, false, false, false));
    }

    #[test]
    fn rail_activity_a_live_turn_outranks_a_running_plan() {
        assert_eq!(badge(true, true), Some(Badge::Live));
        assert_eq!(badge(true, false), Some(Badge::Live));
        assert_eq!(badge(false, true), Some(Badge::Plan));
        assert_eq!(badge(false, false), None);
    }

    #[test]
    fn rail_badge_every_row_reads_one_clock() {
        // Two rows drawn from the same phase must pick the same glyph, which is
        // what keeps a list of busy sessions from flickering out of step.
        let phase = Duration::from_millis(FRAME_MS as u64 * 3 + 7);
        assert_eq!(frame(phase), frame(phase));
        assert_eq!(frame(Duration::ZERO), crate::transcript::SPINNER_FRAMES[0]);
        assert_eq!(
            frame(Duration::from_millis(FRAME_MS as u64)),
            crate::transcript::SPINNER_FRAMES[1]
        );
        // The tenth frame wraps to the first: the cycle is the frame list.
        let wrapped = FRAME_MS as u64 * crate::transcript::SPINNER_FRAMES.len() as u64;
        assert_eq!(
            frame(Duration::from_millis(wrapped)),
            crate::transcript::SPINNER_FRAMES[0]
        );
    }

    /// Write a plan file the way the plan tool does, and hand back its path.
    fn seed_plan(
        dir: &std::path::Path,
        session: Uuid,
        status: PlanStatus,
        task: Option<TaskStatus>,
        pending: bool,
    ) -> PathBuf {
        use opencrabs::tui::plan::{PlanTask, TaskType};
        let mut doc = PlanDocument::new(session, "rail test".to_string());
        doc.status = status;
        doc.pending_approval = pending;
        if let Some(task) = task {
            let mut t = PlanTask::new(1, "the task".to_string(), String::new(), TaskType::Edit);
            t.status = task;
            doc.tasks.push(t);
        }
        let path = dir.join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, serde_json::to_string(&doc).expect("plan json")).expect("write plan");
        path
    }

    /// Age a file past the freshness window without touching its content.
    fn backdate(path: &std::path::Path) {
        let old = SystemTime::now() - FRESH_WINDOW - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open plan")
            .set_modified(old)
            .expect("backdate plan");
    }

    #[test]
    fn rail_plan_the_sweep_finds_a_plan_by_its_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(10);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Active,
            Some(TaskStatus::InProgress),
            false,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("a fresh running plan is evidence");
        assert!(evidence.running);
        assert!(!evidence.ready);
        // A session with no file says nothing, and asking again is not a parse.
        assert!(index.evidence(sid(11), false, Instant::now()).is_none());
    }

    #[test]
    fn rail_plan_an_editing_plan_rings_until_it_is_opened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(12);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Editing,
            Some(TaskStatus::Pending),
            true,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("a plan waiting for approval is evidence");
        assert!(evidence.ready);
        assert!(!evidence.running);
    }

    #[test]
    fn rail_plan_a_pre_init_flag_is_not_a_plan() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(13);
        let path = tmp
            .path()
            .join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        // What `/plan` writes before `plan init` lands: an intent flag, no
        // tasks, no design doc.
        let mut doc = PlanDocument::new(session, String::new());
        doc.pre_init_editing = true;
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert!(
            index.evidence(session, false, Instant::now()).is_none(),
            "an intent flag must not ring the row"
        );
    }

    #[test]
    fn rail_plan_a_stale_file_stops_spinning_unless_the_turn_is_live() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(14);
        let path = seed_plan(
            tmp.path(),
            session,
            PlanStatus::Active,
            Some(TaskStatus::InProgress),
            false,
        );
        backdate(&path);
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);

        // 26 of 27 plans with a task in flight on this box were abandoned, so
        // an old file with no live turn is evidence of nothing.
        assert!(
            index.evidence(session, false, Instant::now()).is_none(),
            "a months-old plan must not spin"
        );
        // But a turn running here outranks the window: a build can hold the
        // file still longer than the window and the work is still real.
        let live = index
            .evidence(session, true, Instant::now())
            .expect("a live turn keeps its plan spinning");
        assert!(live.running);
    }

    #[test]
    fn rail_plan_an_approved_plan_with_nothing_running_draws_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(15);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Active,
            Some(TaskStatus::Completed),
            false,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert!(
            index.evidence(session, true, Instant::now()).is_none(),
            "an approved plan with no task in flight is not activity"
        );
    }

    #[test]
    fn rail_plan_progress_reports_done_over_total_for_an_active_plan() {
        use opencrabs::tui::plan::{PlanTask, TaskType};
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(40);
        // Two done (one completed, one skipped), one in flight, one pending:
        // the strip's rule counts Skipped, so the bar must read 2 of 4.
        let mut doc = PlanDocument::new(session, "progress test".to_string());
        doc.status = PlanStatus::Active;
        for (order, status) in [
            TaskStatus::Completed,
            TaskStatus::Skipped,
            TaskStatus::InProgress,
            TaskStatus::Pending,
        ]
        .into_iter()
        .enumerate()
        {
            let mut t = PlanTask::new(
                order + 1,
                format!("task {order}"),
                String::new(),
                TaskType::Edit,
            );
            t.status = status;
            doc.tasks.push(t);
        }
        let path = tmp
            .path()
            .join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert_eq!(
            index.progress(session, Instant::now()),
            Some((2, 4)),
            "completed and skipped both count as done"
        );
        // A session with no plan file has no bar.
        assert_eq!(index.progress(sid(41), Instant::now()), None);

        // A design draft has no checklist to draw, even with tasks seeded.
        doc.status = PlanStatus::Editing;
        doc.pending_approval = true;
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("rewrite");
        let stamp = SystemTime::now() + Duration::from_secs(2);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open plan")
            .set_modified(stamp)
            .expect("stamp plan");
        assert_eq!(
            index.progress(session, Instant::now()),
            None,
            "a draft awaiting approval has no progress bar"
        );

        // An approved plan with an empty checklist is zero of nothing: no bar.
        doc.status = PlanStatus::Active;
        doc.pending_approval = false;
        doc.tasks.clear();
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("rewrite");
        let stamp = SystemTime::now() + Duration::from_secs(4);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open plan")
            .set_modified(stamp)
            .expect("stamp plan");
        assert_eq!(
            index.progress(session, Instant::now()),
            None,
            "an empty checklist has no bar to draw"
        );
    }

    #[test]
    fn rail_plan_a_finished_plan_is_neither_drawn_nor_deleted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(16);
        let path = tmp
            .path()
            .join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, r#"{"status":"Completed"}"#).expect("write");
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert!(index.evidence(session, false, Instant::now()).is_none());
        // The shared loader archives a Completed plan and deletes a Cancelled
        // one. Reading the rail must never do that to somebody else's history.
        assert!(path.exists(), "the rail must not archive what it draws");
    }

    #[test]
    fn rail_badge_the_row_rings_and_spins_from_one_answer() {
        let stamp = SystemTime::now();
        let ready = PlanEvidence {
            ready: true,
            running: false,
            stamp,
        };
        let running = PlanEvidence {
            ready: false,
            running: true,
            stamp,
        };
        assert_eq!(
            row_state(false, Some(ready), false),
            RowState {
                badge: None,
                ring: true
            }
        );
        assert_eq!(
            row_state(false, Some(running), false),
            RowState {
                badge: Some(Badge::Plan),
                ring: false
            }
        );
        // A turn running now reports itself, not the plan that launched it.
        assert_eq!(
            row_state(true, Some(running), false),
            RowState {
                badge: Some(Badge::Live),
                ring: false
            }
        );
        assert_eq!(
            row_state(false, None, false),
            RowState {
                badge: None,
                ring: false
            }
        );
    }

    #[test]
    fn rail_badge_the_ring_breathes_and_never_snaps() {
        // Dim, bright, dim again, and the wrap is continuous: no jump from
        // bright straight back to the floor, which would read as a flash.
        let quarters: Vec<u8> = [0u128, 600, 1200, 1800]
            .into_iter()
            .map(|ms| ring_alpha(Duration::from_millis(ms as u64)))
            .collect();
        assert_eq!(quarters[0], RING_MIN_ALPHA);
        assert_eq!(quarters[2], RING_MAX_ALPHA);
        // Symmetric: the same height going up as coming down.
        assert_eq!(quarters[1], quarters[3]);
        // The period wraps to the same phase.
        let wrapped = ring_alpha(Duration::from_millis(RING_PERIOD_MS as u64));
        assert_eq!(wrapped, quarters[0]);
        // Never invisible, never a solid block.
        for ms in (0..(RING_PERIOD_MS as u64 * 2)).step_by(37) {
            let alpha = ring_alpha(Duration::from_millis(ms));
            assert!((RING_MIN_ALPHA..=RING_MAX_ALPHA).contains(&alpha));
        }
    }

    #[test]
    fn rail_plan_a_read_plan_stops_ringing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("seen.json");
        let session = sid(20);
        let stamp = SystemTime::now();

        let mut seen = SeenPlans::load(Some(file.clone()));
        assert!(!seen.is_seen(session, stamp));

        seen.mark(session, stamp);
        assert!(seen.is_seen(session, stamp));
        // The mark survives a restart, which is the whole reason it is a file.
        let reloaded = SeenPlans::load(Some(file));
        assert!(reloaded.is_seen(session, stamp));

        // A NEW plan in the same session is news again: the stamp moved.
        let newer = stamp + Duration::from_secs(30);
        assert!(!reloaded.is_seen(session, newer));
    }

    #[test]
    fn rail_plan_an_old_plan_never_covers_a_newer_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut seen = SeenPlans::load(Some(tmp.path().join("seen.json")));
        let session = sid(21);
        let new = SystemTime::now();
        let old = new - Duration::from_secs(600);
        // Opening the current plan must not mark an older one as read.
        seen.mark(session, new);
        assert!(!seen.is_seen(session, old));
    }

    #[test]
    fn rail_plan_the_sweep_reads_only_plan_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(30);
        let plan = seed_plan(
            tmp.path(),
            session,
            PlanStatus::Editing,
            Some(TaskStatus::Pending),
            true,
        );
        // Everything a plan directory also collects: the design doc beside the
        // plan, an unrelated json, and a plan-shaped name that is no session id.
        std::fs::write(plan.with_extension("md"), "# design").expect("design doc");
        std::fs::write(tmp.path().join("notes.json"), "{}").expect("stray json");
        std::fs::write(
            tmp.path()
                .join(format!("{PLAN_PREFIX}not-a-session-id{PLAN_SUFFIX}")),
            "{}",
        )
        .expect("bogus name");

        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("the one real plan is found");
        assert!(evidence.ready);

        // A name the file parser rejects never turns into a row for some other
        // session: nothing resolves without an id to resolve it to.
        assert!(index.evidence(sid(31), false, Instant::now()).is_none());
    }

    #[test]
    fn rail_plan_the_document_cache_follows_the_mtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(32);
        let path = seed_plan(
            tmp.path(),
            session,
            PlanStatus::Editing,
            Some(TaskStatus::Pending),
            true,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert!(
            index
                .evidence(session, false, Instant::now())
                .expect("waiting plan")
                .ready,
            "the draft rings first"
        );

        // The same path, approved and running. The cache is keyed by the file's
        // own stamp, so the rewrite is what the next ask reads; a cache that
        // outlived the write would keep the row ringing an approved plan.
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Active,
            Some(TaskStatus::InProgress),
            false,
        );
        let stamp = SystemTime::now();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open plan")
            .set_modified(stamp)
            .expect("stamp plan");

        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("running plan");
        assert!(evidence.running);
        assert!(!evidence.ready);
    }

    #[test]
    fn rail_plan_a_legacy_checklist_reads_as_approved() {
        // Before the design/checklist split, an Editing plan that had tasks, no
        // design doc and no approval marker was already executable. Ringing it
        // as "waiting for approval" would pulse forever for work that already
        // ran, so the index applies that legacy rule in memory.
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(33);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Editing,
            Some(TaskStatus::InProgress),
            false,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("a legacy checklist is running work");
        assert!(evidence.running);
        assert!(!evidence.ready);
    }

    #[test]
    fn rail_clock_the_spinner_and_the_ring_read_the_same_phase() {
        // One monotonic origin feeds both animators. A tick that advances one
        // advances the other, so a row that spins while some other row rings
        // can never fall out of phase with the turn card it reports.
        let phase = Duration::from_millis(173);
        assert_eq!(frame(phase), frame(phase));
        assert_eq!(ring_alpha(phase), ring_alpha(phase));

        // Both clocks are closed: the spinner wraps on its frame list and the
        // ring wraps on its period, so neither drifts from the shared origin.
        let spin = FRAME_MS * crate::transcript::SPINNER_FRAMES.len() as u128;
        assert_eq!(
            frame(phase + Duration::from_millis(spin as u64)),
            frame(phase)
        );
        assert_eq!(
            ring_alpha(phase + Duration::from_millis(RING_PERIOD_MS as u64)),
            ring_alpha(phase)
        );
    }
}
