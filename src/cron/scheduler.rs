//! Cron Scheduler
//!
//! Background task that checks the `cron_jobs` table every 60 seconds,
//! executes due jobs in a shared "Cron" session, and delivers results
//! to the configured channel. Each run inserts its compaction marker at run
//! START, before the turn reads history, so the next fire starts from an empty
//! context AND a run the daemon killed mid-flight still leaves a boundary
//! behind (#149 contamination vector, #1703 stale-context reload). Cron jobs
//! are fully isolated from the TUI —
//! they never share or mutate the user's active session.

use crate::channels::ChannelFactory;
use crate::config::Config;
use crate::db::CronJobRepository;
use crate::db::CronJobRunRepository;
use crate::db::models::{CronJob, CronJobRun};
use crate::services::{ServiceContext, SessionService};
use crate::utils::string::truncate_str;
use chrono::Utc;
use std::sync::Arc;
use tracing::Instrument;
use uuid::Uuid;

/// Whether `job_profile` is the active process profile (so the cheap, already
/// wired factory agent can run it) rather than a foreign profile that needs its
/// own config + brain materialized. `None` = legacy pre-stamping row, treated as
/// the active profile. The base profile is stored as the literal "default".
fn is_active_profile(job_profile: Option<&str>, active: Option<&str>) -> bool {
    match job_profile {
        None => true,
        Some(stamped) => stamped == active.unwrap_or("default"),
    }
}

/// Warn-once guard for unparseable cron expressions (#1163): without it an
/// invalid row warns on every ~60s tick — 1,440 warns/day for a job that
/// never runs. One warning per process is enough to diagnose.
static INVALID_EXPR_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Escalation threshold (#1893): three consecutive tick failures is about three
/// minutes of a stopped scheduler, long enough to rule out a transient DB
/// blip, short enough to matter before a day of missed runs. Before this the
/// loop logged and slept forever, so 5,100 failures over 3.5 days stayed as
/// 5,100 log lines nobody read.
pub(crate) const TICK_ALERT_AFTER_FAILURES: u32 = 3;

/// Minimum spacing between escalation notices (#1893): a scheduler that cannot
/// read its table fails every 60s forever, so an unthrottled alert becomes the
/// new noise instead of the signal.
pub(crate) const TICK_ALERT_MIN_SPACING: std::time::Duration = std::time::Duration::from_secs(3600);

// ── scheduler liveness (#1925) ───────────────────────────────────────────────

/// Process-wide liveness stamps (#1925). The #1893 escalation only exists
/// inside the tick loop: when the loop never runs (scheduler lock held by a
/// live pid, a panic before the loop, a spawn that was never reached) there is
/// no counter, no notice and no log line at all. The loop stamps these; any
/// already-running surface in the process can read them, and the daemon
/// health endpoint reports them so an external `/health` poller sees the
/// absence without OpenCrabs owning a new timer.
///
/// 0 means "never": a fresh process and a scheduler-less one are identical
/// here, which is the point.
pub(crate) static CRON_SPAWNED_AT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static CRON_LAST_TICK_AT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Staleness window (#1925). Polling is 60s, but a tick may run job bodies
/// inline, so a healthy scheduler can legitimately go silent between tick
/// completions for a while; 10 minutes rules out the blips without hiding an
/// hour of dark schedule. The health endpoint always reports the raw age, so
/// this constant only picks the label, never hides the number.
pub(crate) const CRON_STALE_AFTER_SECS: u64 = 600;

/// The liveness verdict for a given instant (#1925).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CronLiveness {
    /// No scheduler loop in this process and no enabled jobs to notice.
    Idle,
    /// The #1925 silence: enabled jobs exist, and nothing is running them.
    NotRunningWithJobs { enabled: u64 },
    /// The loop ran but has not COMPLETED a tick inside the stale window.
    /// Covers both "never completed a first tick" (setup wedged) and "stopped
    /// completing them" (every tick failing: #1893 escalation only reaches a
    /// human when a `deliver_to` target was ever read; this is the signal
    /// that does not need one).
    Stalled { age_secs: u64, enabled: u64 },
    /// A tick completed inside the window (or startup is still inside grace).
    Healthy { age_secs: u64 },
}

/// Pure liveness decision (#1925), taking the clock as an argument so the
/// window is testable without waiting ten minutes. `spawned_at` is stamped
/// once when the loop begins; `last_tick_at` only on a COMPLETED (Ok) tick.
pub(crate) fn cron_liveness(
    now: u64,
    spawned_at: u64,
    last_tick_at: u64,
    enabled: u64,
) -> CronLiveness {
    if spawned_at == 0 {
        return if enabled > 0 {
            CronLiveness::NotRunningWithJobs { enabled }
        } else {
            CronLiveness::Idle
        };
    }
    let age = if last_tick_at == 0 {
        // Started but never completed a tick: measure from the loop's own
        // start, which is the last proof of life there is.
        now.saturating_sub(spawned_at)
    } else {
        now.saturating_sub(last_tick_at)
    };
    if age > CRON_STALE_AFTER_SECS {
        CronLiveness::Stalled {
            age_secs: age,
            enabled,
        }
    } else {
        CronLiveness::Healthy { age_secs: age }
    }
}

/// Wall-clock unix seconds, 0 only if the system clock is before 1970.
pub(crate) fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether this tick failure deserves a user-facing notice yet (#1893).
///
/// Pure, and taking the clock as an argument, so the hour window is testable
/// without sleeping an hour. `last_alert` is the process uptime at which the
/// previous notice went out.
pub(crate) fn alert_due(
    failures: u32,
    last_alert: Option<std::time::Duration>,
    uptime: std::time::Duration,
) -> bool {
    if failures < TICK_ALERT_AFTER_FAILURES {
        return false;
    }
    match last_alert {
        None => true,
        Some(at) => uptime.saturating_sub(at) >= TICK_ALERT_MIN_SPACING,
    }
}

/// Map a supervised tick result (#1963). A `JoinError::Panic` means the
/// tick task panicked: converted to an ordinary error so the scheduler's
/// failure accounting and alerting (#1893) treats it like any other tick
/// failure instead of the whole scheduler task dying silently.
pub(crate) fn supervise_tick_result(
    joined: Result<Result<(), anyhow::Error>, tokio::task::JoinError>,
) -> Result<(), anyhow::Error> {
    match joined {
        Ok(result) => result,
        Err(e) if e.is_panic() => Err(anyhow::anyhow!(
            "cron tick panicked (isolated by supervisor; scheduler keeps running): {e:#}"
        )),
        Err(e) => Err(anyhow::anyhow!(
            "cron tick task cancelled (isolated by supervisor; scheduler keeps running): {e:#}"
        )),
    }
}

/// Background cron scheduler that polls the database and executes due jobs.
pub struct CronScheduler {
    repo: CronJobRepository,
    run_repo: CronJobRunRepository,
    factory: Arc<ChannelFactory>,
    service_context: ServiceContext,
    /// Delivery targets named by the last job list this scheduler could read
    /// (#1893). A total read failure is precisely the moment `deliver_to` is
    /// unreadable, so the targets have to be remembered while they still are.
    known_targets: std::sync::Mutex<Vec<String>>,
}

impl CronScheduler {
    pub fn new(
        repo: CronJobRepository,
        run_repo: CronJobRunRepository,
        factory: Arc<ChannelFactory>,
        service_context: ServiceContext,
    ) -> Self {
        Self {
            repo,
            run_repo,
            factory,
            service_context,
            known_targets: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Spawn the scheduler as a background tokio task.
    /// Polls every 60 seconds for due jobs.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run())
    }

    /// Run the polling loop in the CURRENT task (no internal spawn). The
    /// multi-profile daemon drives this directly inside a
    /// `with_profile_home_async(profile, ...)` scope so the scheduler's own
    /// setup (cron session, config reads) resolves to that profile's home.
    /// `spawn()` is the thin wrapper for callers that just want it backgrounded.
    pub async fn run(self) {
        let me = Arc::new(self);
        // #1925: mark the instant the loop began. Readers age this stamp when
        // no tick has completed yet.
        CRON_SPAWNED_AT.store(epoch_secs(), std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            "Cron scheduler started — polling every 60s (shared Cron session, compaction-isolated)"
        );
        if let Err(e) = me.backfill_missing_next_run().await {
            tracing::error!("Failed to backfill missing next_run_at on startup: {e:#}");
        }

        // #1893: a tick failure used to be log-only, and a log line is the one
        // place nobody looks while the schedule is quietly dead. Track
        // consecutive failures, escalate at the threshold, reset on success.
        let started = std::time::Instant::now();
        let mut failures: u32 = 0;
        let mut last_alert: Option<std::time::Duration> = None;

        loop {
            // #1963: a panicking tick used to take the scheduler task down
            // with it: `spawn()` discards the JoinHandle, so the schedule
            // died silently. Each tick now runs as its own supervised task
            // and a panic lands in the #1893 failure path as a logged error.
            let s = Arc::clone(&me);
            let outcome = supervise_tick_result(tokio::spawn(async move { s.tick().await }).await);
            match outcome {
                Ok(()) => {
                    failures = 0;
                    last_alert = None;
                    // #1925: a COMPLETED tick is the liveness proof. Failing
                    // ticks deliberately do not stamp: a loop that only
                    // errors must go Stalled, not stay "alive".
                    CRON_LAST_TICK_AT.store(epoch_secs(), std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    tracing::error!("Cron scheduler tick error: {e:#}");
                    let uptime = started.elapsed();
                    if alert_due(failures, last_alert, uptime) {
                        last_alert = Some(uptime);
                        me.alert_tick_failure(failures, &e).await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    }

    /// Cache the delivery targets named by the jobs this tick could read
    /// (#1893). Rewritten on every successful read, so a job whose `deliver_to`
    /// was edited or deleted stops receiving notices.
    fn remember_targets(&self, jobs: &[CronJob]) {
        let mut targets: Vec<String> = Vec::new();
        for job in jobs {
            let Some(deliver_to) = job.deliver_to.as_deref() else {
                continue;
            };
            for target in deliver_to
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !targets.iter().any(|seen| seen == target) {
                    targets.push(target.to_string());
                }
            }
        }
        if let Ok(mut known) = self.known_targets.lock() {
            *known = targets;
        }
    }

    /// Tell a human the scheduler has stopped, not only the log (#1893).
    ///
    /// Carries the full anyhow chain: an alert someone can act on has to say why
    /// the schedule stopped, which is exactly what bare `{e}` was dropping
    /// (#1894). With no target ever read (fresh process, table already broken)
    /// there is nowhere to send, and that is logged loudly instead of skipped
    /// silently.
    async fn alert_tick_failure(&self, failures: u32, err: &anyhow::Error) {
        let targets = self
            .known_targets
            .lock()
            .map(|known| known.clone())
            .unwrap_or_default();
        if targets.is_empty() {
            tracing::error!(
                "Cron scheduler has failed {failures} consecutive ticks and has no delivery target to alert: no job list has ever been read by this process"
            );
            return;
        }
        let message = format!(
            "Cron scheduler is stopped: {failures} consecutive tick failures, no scheduled job will fire until this is fixed. Cause: {err:#}"
        );
        for target in targets {
            tracing::error!("Escalating cron scheduler failure to {target}");
            let _ = deliver_result(
                &target,
                "cron scheduler",
                &message,
                None,
                Some(self.service_context.pool()),
            )
            .await;
        }
    }

    /// One scheduler tick: check all enabled jobs and execute any that are due.
    async fn tick(&self) -> anyhow::Result<()> {
        // #1893: read row by row. One unreadable row used to fail the whole
        // read, which stopped every job instead of just the broken one.
        let (mut jobs, skipped) = self.repo.list_enabled_with_skips().await?;
        self.remember_targets(&jobs);
        // "Nothing is due" must not absorb "nothing can be read": an empty list
        // that skipped rows is a failure, and the tick loop escalates it.
        if jobs.is_empty() && skipped > 0 {
            return Err(anyhow::anyhow!(
                "all {skipped} enabled cron_jobs row(s) failed to decode, no job can run"
            ));
        }
        let now = Utc::now();

        // Ensure all enabled jobs have next_run_at populated even if created externally
        // or through legacy paths (#202).
        for job in &mut jobs {
            if job.next_run_at.is_some() {
                continue;
            }
            let Some(next) = super::next_run_utc(&job.cron_expr, job_tz(job), now) else {
                continue;
            };
            let patch = crate::db::repository::CronJobPatch {
                next_run_at: Some(Some(next)),
                ..Default::default()
            };
            if let Err(e) = self.repo.update_fields(&job.id.to_string(), patch).await {
                tracing::warn!(error = %e, job_id = %job.id, "Failed to persist next_run_at in tick");
            } else {
                job.next_run_at = Some(next);
            }
        }

        for job in &jobs {
            if self.is_due(job, now) {
                tracing::info!("Cron job '{}' ({}) is due — executing", job.name, job.id);

                // Calculate next run time before executing (so we don't re-trigger).
                // Use the scheduled boundary as anchor, not `now`: for a
                // first-run job (next_run_at = None), `now` may be 16s before
                // the boundary (e.g. 10:59:44 vs 11:00:00). Passing `now`
                // resolves to the SAME boundary, causing a double-fire next tick.
                // (#224)
                let next_run = match job.next_run_at {
                    Some(_) => self.next_run_after(job, now),
                    None => match super::next_run_utc(&job.cron_expr, job_tz(job), now) {
                        Some(boundary) => self.next_run_after(job, boundary),
                        None => None,
                    },
                };
                let next_run_str = next_run.map(|dt| dt.to_rfc3339());
                // #544: a one-shot retires in this same statement — the
                // schedule has already moved to next year by the time the job
                // body runs, so retiring here is the consistent read.
                //
                // #1917: one failed write must not abort the whole tick. The
                // `?` here used to return Err to the tick loop, skipping every
                // remaining due job THIS turn as collateral damage. Skipping
                // only the un-persistable job is safe in both directions:
                // because the advance failed, the job stays due and refires
                // next tick (no lost run), and a job whose storage is broken
                // is precisely the one that should not fire now anyway. The
                // old `?` escalated via the tick failure counter; that
                // escalation no longer sees this case, which is the
                // deliberate trade: collateral damage to every other due job
                // was the worse failure. The warn line is the signal.
                if let Err(e) = self
                    .repo
                    .update_last_run(&job.id.to_string(), next_run_str.as_deref(), job.run_once)
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        job_id = %job.id,
                        "Cron tick: failed to advance schedule for '{}' ({}) this tick; \
                         other due jobs proceed, this one stays due and refires next tick \
                         (#1917)",
                        job.name,
                        job.id
                    );
                    continue;
                }

                // Execute in background so we don't block other jobs
                let job = job.clone();
                let factory = self.factory.clone();
                let ctx = self.service_context.clone();
                let run_repo = self.run_repo.clone();
                let job_name = job.name.clone();
                let job_id = job.id;
                let report_name = job_name.clone();
                let job_task = tokio::spawn(
                    async move {
                        // For foreign-profile jobs, wrap the ENTIRE execution in a
                        // task-local profile home scope. This means every tool call
                        // the agent makes (memory writes, config reads, file ops,
                        // brain reads) resolves to the job's profile home, not the
                        // process profile. The scope lives until the task ends, so
                        // it persists across every .await inside the agent loop.
                        //
                        // This spawned task does NOT inherit the scheduler's own
                        // task-local home (tokio::spawn drops it), so it defaults to
                        // the process global. We therefore scope whenever the job's
                        // profile differs from the process global, which is exactly
                        // the multi-profile daemon case: a per-profile scheduler's
                        // jobs are stamped with a non-global profile and get scoped
                        // here.
                        let profile = job.profile_name.as_deref();
                        let active = crate::config::profile::active_profile().unwrap_or("default");
                        let needs_scope = profile.is_some() && profile != Some(active);

                        // #2065: the trigger gate is NOT part of the profile
                        // split. It used to live inside the `needs_scope` arm,
                        // so a job whose `profile_name` equalled the active
                        // profile took the other arm and never evaluated its
                        // trigger at all: `trigger_cmd` plus `trigger_on` were
                        // silently dead, and the job ran a full agent turn on
                        // every tick instead of the 0-token skip. Both arms now
                        // call the same body; only the home scope differs.
                        let result = if needs_scope {
                            crate::config::profile::with_profile_home_async(profile, async {
                                tracing::info!(
                                    "Cron job '{}' — task-local profile home set to {:?}",
                                    job.name,
                                    crate::config::opencrabs_home()
                                );
                                run_cron_job_body(&job, &factory, &ctx, &run_repo).await
                            })
                            .await
                        } else {
                            run_cron_job_body(&job, &factory, &ctx, &run_repo).await
                        };

                        if let Err(e) = result {
                            tracing::error!("Cron job '{}' failed: {e:#}", job.name);
                        }
                    }
                    .instrument(tracing::info_span!("job", name = %job_name, id = %job_id)),
                );
                // #1963: the job handle used to be dropped, so a panicking
                // job vanished with one line in nobody's log. Await it and
                // report loudly; the scheduler and other jobs are unaffected.
                tokio::spawn(async move {
                    if let Err(e) = job_task.await {
                        tracing::error!(
                            "Cron job {report_name} task failed (isolated; scheduler keeps running): {e:#}"
                        );
                    }
                });
            }
        }

        Ok(())
    }

    /// Backfill any enabled jobs that have `next_run_at: None` (#202).
    ///
    /// Leaves `last_run_at` completely untouched and schedules the upcoming run time.
    pub async fn backfill_missing_next_run(&self) -> anyhow::Result<usize> {
        let jobs = self.repo.list_enabled().await?;
        let now = Utc::now();
        let mut count = 0;

        for job in jobs {
            if job.next_run_at.is_none() {
                match super::next_run_utc(&job.cron_expr, job_tz(&job), now) {
                    Some(next) => {
                        let patch = crate::db::repository::CronJobPatch {
                            next_run_at: Some(Some(next)),
                            ..Default::default()
                        };
                        match self.repo.update_fields(&job.id.to_string(), patch).await {
                            Ok(true) => {
                                count += 1;
                                tracing::info!(
                                    "Backfilled next_run_at for cron job '{}' ({}) -> {}",
                                    job.name,
                                    job.id,
                                    next
                                );
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    job_id = %job.id,
                                    "Failed to persist backfilled next_run_at"
                                );
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "Could not compute next run time for job '{}' ({}) with cron '{}'",
                            job.name,
                            job.id,
                            job.cron_expr
                        );
                    }
                }
            }
        }
        Ok(count)
    }

    /// Check if a job is due to run.
    fn is_due(&self, job: &CronJob, now: chrono::DateTime<Utc>) -> bool {
        match &job.next_run_at {
            // If next_run_at is set and is in the past (or now), it's due
            Some(next) => *next <= now,
            // If next_run_at is None (first run), calculate from cron and check
            None => {
                // Interpret the schedule in the job's timezone (DST-aware),
                // then compare the resulting UTC instant. If any upcoming run
                // is within the next 60s (one tick), it's due.
                match super::next_run_utc(&job.cron_expr, job_tz(job), now) {
                    Some(next) => (next - now).num_seconds() <= 60,
                    None => {
                        // Warn once per process (#1163): this arm fires on
                        // every ~60s tick otherwise, so one bad row produced
                        // 1,440 identical warns/day.
                        if !INVALID_EXPR_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!(
                                "Invalid cron expression for job '{}': {} (suppressing further warnings until restart)",
                                job.name,
                                job.cron_expr
                            );
                        }
                        false
                    }
                }
            }
        }
    }

    /// Calculate the next run time after a given point, in the job's timezone.
    fn next_run_after(
        &self,
        job: &CronJob,
        after: chrono::DateTime<Utc>,
    ) -> Option<chrono::DateTime<Utc>> {
        super::next_run_utc(&job.cron_expr, job_tz(job), after)
    }
}

/// The stable per-job title suffix used as the session lookup key. Derived
/// from the job's UUID — unique per job row, rename-safe (LIKE-unsafe
/// characters are impossible in a UUID), and shared by the resolution path
/// and tests via this single definition.
pub(crate) fn cron_session_title_suffix(job: &CronJob) -> String {
    format!("[cron-job:{}]", job.id)
}

/// Resolve the session a cron job runs in — ONE SESSION PER JOB (#149).
///
/// Legacy behavior (pre-#149) resolved a single shared "Cron" session for
/// every job. That design cross-pollinates whenever jobs overlap in time,
/// which the 60s tick makes routine, not exceptional:
///
/// 1. **History**: context loads from the LAST `[CONTEXT COMPACTION]` marker
///    in the session (`messages_from_last_compaction`). With one shared
///    session, a fire's own boundary is the only thing that bounds its load,
///    so job B starting while job A is mid-flight can pick up A's rows that
///    landed between B's marker and B's load, and two concurrent turns
///    interleave writes into one history. Per-job sessions remove both.
/// 2. **Provider/model**: `execute_job` swaps the per-session provider keyed
///    to the session id — with one shared id, a concurrent job's swap
///    overwrites the running job's provider mid-turn (last writer wins).
///
/// Sessions are resolved by a stable TITLE suffix carrying the job id
/// (`Cron: <job_name> [cron-job:<uuid>]`), mirroring the channel-handler pattern
/// (`[chat:N]` suffix) so a user rename of the readable part still resolves
/// to the same session row while different jobs never share one.
///
/// Cross-RUN contamination within a single job is bounded by the boundary each
/// run writes at its own START (`open_run_boundary`): the job's next fire
/// reloads from that newest marker and nothing else, so neither a finished run
/// nor one the daemon killed mid-flight can hand its stale tool results forward
/// (deliberate - cron prompts are self-contained).
///
/// There is no per-run single-flight guard here: overlapping fires of the
/// same job each get a session that ONLY that job writes, so the two
/// pollution vectors above cannot cross jobs; self-overlap remains visible
/// in `cron_job_runs` and is addressed separately if it proves harmful.
pub(crate) async fn resolve_or_create_cron_session(
    ctx: &ServiceContext,
    job: &CronJob,
) -> anyhow::Result<Uuid> {
    // Stable lookup key: survives renames of the readable part, unique per
    // job id (a UUID — LIKE-escape characters cannot occur).
    let suffix = cron_session_title_suffix(job);
    let title = format!("Cron: {} {}", job.name, suffix);

    use crate::db::repository::SessionListOptions;
    let session_svc = SessionService::new(ctx.clone());
    let sessions = session_svc
        .list_sessions(SessionListOptions {
            include_archived: false,
            limit: None,
            offset: 0,
            query: None,
            include_subagents: false,
        })
        .await?;
    if let Some(existing) = sessions
        .iter()
        .find(|s| s.title.as_deref().is_some_and(|n| n.ends_with(&suffix)))
    {
        return Ok(existing.id);
    }
    let config = Config::load()?;
    let provider = config.cron.default_provider.clone();
    let model = config.cron.default_model.clone();
    let session = session_svc
        .create_session_with_provider(Some(title), provider, model, None)
        .await?;
    Ok(session.id)
}

/// Resolve a job's stored timezone string to a `Tz`, falling back to UTC for
/// an unknown zone (the tool/CLI reject unknown zones at creation, so this is
/// just a safety net for hand-edited rows).
fn job_tz(job: &CronJob) -> chrono_tz::Tz {
    super::parse_timezone(&job.timezone).unwrap_or(chrono_tz::UTC)
}

/// Resolve the `(Config, AgentService)` a job should run with.
///
/// Jobs created in the active profile (and legacy unstamped jobs) use the
/// already-wired factory agent. A job stamped with a DIFFERENT profile
/// (shared-DB case, #182) gets its own config + brain + provider built from
/// that profile's home. The shared DB pool is reused since that's exactly why
/// the foreign job is visible to this scheduler at all.
async fn resolve_job_agent(
    job: &CronJob,
    factory: &ChannelFactory,
    ctx: &ServiceContext,
) -> anyhow::Result<(Config, Arc<crate::brain::agent::AgentService>)> {
    // Use the task-local profile (set by the per-job home scope above) rather
    // than the process global, so a job running under its own profile scope
    // is recognized as "local" and reuses the in-scope factory/config instead
    // of needlessly re-materializing. Falls back to the global when unscoped.
    let current = crate::config::profile::current_profile_name();
    if is_active_profile(job.profile_name.as_deref(), Some(&current)) {
        return Ok((Config::load()?, factory.create_agent_service().await));
    }

    let profile = job.profile_name.as_deref();
    tracing::info!(
        "Cron job '{}' belongs to profile {:?} (current profile {:?}); \
         running under its own profile context",
        job.name,
        profile,
        current
    );

    // Materialize config + brain from the foreign profile's home.
    // with_profile_home sets a sync scope just for these two loads.
    let (config, brain, home) = crate::config::profile::with_profile_home(profile, || {
        let config = Config::load()?;
        let home = crate::config::opencrabs_home();
        let brain =
            crate::brain::prompt_builder::BrainLoader::new(home.clone()).build_core_brain(None);
        anyhow::Ok((config, brain, home))
    })?;

    // Provider is built from the foreign profile's keys.
    let provider = crate::brain::provider::create_provider(&config).await?;
    let mut builder = crate::brain::agent::AgentService::new(provider, ctx.clone(), &config)
        .await
        .with_system_brain(brain)
        .with_working_directory(home.clone())
        .with_brain_path(home);
    if let Some(registry) = factory.tool_registry() {
        builder = builder.with_tool_registry(registry);
    }
    // #129: cron execution is headless — backstop flag on the agent so
    // interactive-only tools hard-error even if re-registered.
    builder = builder.with_headless(true);
    // A scheduled job is a task already defined by its prompt: there is no
    // user in the loop to approve a plan, and refusing call 13 would strand
    // the run mid-job. The mechanical plan requirement does not apply.
    builder = builder.with_plan_gate_exempt(true);
    Ok((config, Arc::new(builder)))
}

/// The compaction boundary that opens a cron run (#1703 fix 3).
///
/// Written ONCE per run, at run START, before the turn reads history — never
/// after it. `messages_from_last_compaction` (`service/context.rs`) walks
/// backward to the LAST `[CONTEXT COMPACTION]` user row and loads everything
/// from there forward, so the marker's position in the sequence is the whole
/// guarantee: a run that wrote its boundary only on completion left NO marker
/// when the daemon died mid-flight, and the next fire reloaded from the
/// PREVIOUS run's marker, i.e. the dead run's prompt and half-finished tool
/// results came back as live context. Opening every run with its own marker
/// means the newest boundary is always younger than anything the run before
/// it produced, so a death between the marker and the finish line strands
/// those rows behind the next fire's own boundary instead.
///
/// Must keep the `[CONTEXT COMPACTION` prefix (that is the anchor the loader
/// searches) and must NOT contain `SEGMENT_SENTINEL` — a segment marker
/// extends the compaction window instead of restarting it.
pub(crate) const CRON_RUN_BOUNDARY: &str = "[CONTEXT COMPACTION — Cron job execution boundary]";

/// Stamp the boundary row that bounds this run's history for the NEXT fire.
///
/// A failed write is logged, not fatal: without the boundary the next fire of
/// this job starts from an empty context instead of the previous run's history
/// (a stale-context regression, not a crash), so a missing marker must not
/// silently eat the run. Same reasoning as the run-record insert above.
async fn open_run_boundary(ctx: &ServiceContext, session_id: Uuid) {
    let message_svc = crate::services::MessageService::new(ctx.clone());
    if let Err(e) = message_svc
        .create_message(
            session_id,
            "user".to_string(),
            CRON_RUN_BOUNDARY.to_string(),
        )
        .await
    {
        tracing::warn!("Failed to insert cron compaction marker: {e:#}");
    }
}

/// Run one cron job's body: evaluate the trigger gate, then either skip,
/// error out, deliver directly, or run the agent turn (#2065).
///
/// This is the part of a fire that does NOT depend on profile scoping. The
/// caller wraps it in `with_profile_home_async` when the job belongs to a
/// foreign profile, so the trigger command still executes against the job's
/// own home. Keeping the gate out here is the fix: it used to sit inside the
/// scoped arm, so every job stamped with the active profile (which is all of
/// them in a single-profile install) skipped evaluation entirely and burned a
/// full agent turn on every tick.
pub(crate) async fn run_cron_job_body(
    job: &CronJob,
    factory: &ChannelFactory,
    ctx: &ServiceContext,
    run_repo: &CronJobRunRepository,
) -> anyhow::Result<()> {
    // #526 — the fired trigger's payload, already substituted into the prompt
    // in the arm that owns the `TriggerResult`. None for every non-firing
    // outcome keeps the turn message byte-identical to pre-fix behaviour.
    let mut fired_prompt: Option<String> = None;

    // Pre-flight trigger evaluation
    match crate::cron::PipelineExecutor::evaluate_trigger(job).await {
        crate::cron::TriggerOutcome::Skipped(ref trig_res) => {
            tracing::info!(
                "Cron job '{}' trigger condition not met — skipping execution",
                job.name
            );
            let _ =
                crate::cron::PipelineExecutor::record_skipped_run(job, trig_res, run_repo).await;
            return Ok(());
        }
        crate::cron::TriggerOutcome::Error(err) => {
            tracing::error!("Cron job '{}' trigger execution error: {err}", job.name);
            let run = CronJobRun::new_running(
                job.id,
                job.name.clone(),
                job.provider.clone(),
                job.model.clone(),
            );
            let run_id = run.id.to_string();
            let _ = run_repo.insert(&run).await;
            let _ = run_repo
                .complete_error(&run_id, &format!("Trigger error: {err}"))
                .await;
            return Ok(());
        }
        crate::cron::TriggerOutcome::Fired(ref trig_res) => {
            tracing::info!(
                "Cron job '{}' trigger fired (exit={}, output_bytes={})",
                job.name,
                trig_res.exit_code,
                trig_res.combined_output().len()
            );
            // If prompt is empty or explicitly pass-through, execute direct 0-token delivery
            if job.prompt.trim().is_empty() {
                return execute_direct_trigger_job(job, ctx, run_repo, trig_res).await;
            }
            // #526 — only the non-empty-prompt half reaches here; the
            // empty-prompt half returned above.
            fired_prompt = Some(crate::cron::interpolate_template(&job.prompt, trig_res));
        }
        crate::cron::TriggerOutcome::NoTrigger => {}
    }

    match resolve_or_create_cron_session(ctx, job).await {
        Ok(cron_sid) => {
            execute_job(
                job,
                factory,
                ctx,
                cron_sid,
                run_repo,
                fired_prompt.as_deref(),
            )
            .await
        }
        Err(e) => Err(e),
    }
}

/// Execute a single cron job in its own isolated session.
/// Isolated from TUI — never touches the user's active session.
/// Results are always stored in the DB; channel delivery is optional.
async fn execute_job(
    job: &CronJob,
    factory: &ChannelFactory,
    ctx: &ServiceContext,
    cron_session_id: Uuid,
    run_repo: &CronJobRunRepository,
    fired_prompt: Option<&str>,
) -> anyhow::Result<()> {
    // Resolve the config + agent for this job's profile. A job created in a
    // non-active profile (shared-DB case, #182) runs under its own profile's
    // config + brain, not the process profile's.
    let (config, agent) = resolve_job_agent(job, factory, ctx).await?;
    let effective_provider = job
        .provider
        .clone()
        .or_else(|| config.cron.default_provider.clone());
    let effective_model = job
        .model
        .clone()
        .or_else(|| config.cron.default_model.clone());

    // Pre-validate the {provider, model} pair before spawning the agent.
    // A reversed cron config (e.g. model="zhipu", provider="glm-5.1") or a
    // typo would otherwise reach the tool loop and produce confusing RSI
    // entries like "dialagram/zhipu" where a provider name leaked into the
    // model slot. Catch it early, log loudly, skip the job.
    if let (Some(provider_name), Some(model)) = (&effective_provider, &effective_model) {
        match crate::brain::provider::create_provider_by_name(&config, provider_name).await {
            Ok(provider) => {
                let supported = provider.supported_models();
                if !supported.is_empty() && !supported.iter().any(|m| m == model) {
                    tracing::error!(
                        "Cron job '{}' — model '{}' is NOT supported by provider '{}' \
                         (supported: {}). SKIPPING job — fix cron config. \
                         Either set a valid model or remove the model override to use \
                         the provider's default ('{}').",
                        job.name,
                        model,
                        provider_name,
                        supported.join(", "),
                        provider.default_model(),
                    );
                    // Record the failure so RSI surfaces it
                    let run = CronJobRun::new_running(
                        job.id,
                        job.name.clone(),
                        effective_provider.clone(),
                        effective_model.clone(),
                    );
                    let run_id = run.id.to_string();
                    if let Err(e) = run_repo.insert(&run).await {
                        tracing::error!("Failed to insert cron run record: {e:#}");
                    }
                    let err_msg = format!(
                        "model '{}' not supported by provider '{}' — cron config invalid",
                        model, provider_name
                    );
                    if let Err(db_err) = run_repo.complete_error(&run_id, &err_msg).await {
                        tracing::error!("Failed to save cron run error to DB: {db_err}");
                    }
                    return Ok(());
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Cron job '{}' — cannot pre-validate model (provider '{}' creation \
                     failed: {e:#}) — proceeding with default validation",
                    job.name,
                    provider_name
                );
            }
        }
    }

    // Create a run record in the DB (status = "running")
    let run = CronJobRun::new_running(
        job.id,
        job.name.clone(),
        effective_provider.clone(),
        effective_model.clone(),
    );
    let run_id = run.id.to_string();
    if let Err(e) = run_repo.insert(&run).await {
        tracing::error!("Failed to insert cron run record: {e:#}");
    }
    // #1703 fix 2: the run's start time, formatted once for the delivery
    // stamps; the ledger row keeps the full timestamp itself.
    let run_started_at = run.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let session_id = cron_session_id;
    tracing::info!(
        "Cron job '{}' — using cron session {}",
        job.name,
        session_id
    );

    // #1703 fix 3: open the run with its compaction boundary, BEFORE the turn
    // reads history. The turn loads `[last marker .. new prompt]` inside
    // send_message_with_tools_and_callback, so this row is already the newest
    // marker by the time that load runs: this fire sees an empty context, and
    // if the daemon dies before the run finishes, whatever it managed to write
    // sits behind this boundary and is discarded by the NEXT fire rather than
    // reloaded as live tool results. One boundary per run, at start, never at
    // the end - a run that closed with a marker would double-stamp.
    open_run_boundary(ctx, session_id).await;

    // Swap to cron-specific provider if configured
    if let Some(ref provider_name) = effective_provider {
        match crate::brain::provider::create_provider_by_name(&config, provider_name).await {
            Ok(provider) => {
                tracing::info!(
                    "Cron job '{}' — using provider '{}'",
                    job.name,
                    provider_name
                );
                agent.swap_provider_for_session(
                    cron_session_id,
                    provider.clone(),
                    provider.default_model().to_string(),
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Cron job '{}' — failed to create provider '{}': {e:#}, using system default",
                    job.name,
                    provider_name
                );
            }
        }
    }

    // Permitted destinations for this turn, taken from the job's own
    // configuration (#148: authority-aware across telegram, discord, slack,
    // whatsapp). A job with no `deliver_to` may send to none: its output
    // lives in its session and the scheduler is the only thing that speaks.
    // Scoped across the whole turn so it holds inside every tool call, and
    // task-local so it never reaches a sibling job on the scheduler.
    // #526 — the caller already substituted a fired trigger's payload into the
    // prompt. None on every other outcome (and the rebuild path returned before
    // this line), so the turn message stays byte-identical to pre-fix behaviour
    // when nothing fired.
    let turn_prompt = fired_prompt.unwrap_or(job.prompt.as_str());

    let permitted_targets: Option<Vec<crate::cron::send_scope::PermittedTarget>> =
        job.deliver_to.as_deref().map(|targets| {
            targets
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .filter_map(|t| {
                    if let Some(rest) = t.strip_prefix("telegram:") {
                        parse_telegram_target(rest).map(|(chat_id, _)| {
                            crate::cron::send_scope::PermittedTarget {
                                channel: "telegram",
                                target_id: chat_id.to_string(),
                            }
                        })
                    } else if let Some(rest) = t.strip_prefix("discord:") {
                        // `:forum` is a delivery mode, not a second address (#1851): the
                        // job may post in that channel, so the bare channel id is what the
                        // scope permits, exactly as a Telegram thread target permits its
                        // chat. A malformed target permits nothing and is reported loudly
                        // at delivery time.
                        parse_discord_target(rest).map(|(channel_id, _)| {
                            crate::cron::send_scope::PermittedTarget {
                                channel: "discord",
                                target_id: channel_id,
                            }
                        })
                    } else if let Some(rest) = t.strip_prefix("slack:") {
                        Some(crate::cron::send_scope::PermittedTarget {
                            channel: "slack",
                            target_id: rest.to_string(),
                        })
                    } else {
                        t.strip_prefix("whatsapp:").map(|rest| {
                            crate::cron::send_scope::PermittedTarget {
                                channel: "whatsapp",
                                target_id: rest.to_string(),
                            }
                        })
                    }
                })
                .collect()
        });

    // #1703: execution evidence probe. Count every tool that actually
    // starts inside this run; the completion path refuses a `success`
    // verdict without at least one start.
    let (tool_starts, evidence_cb) = tool_start_counter();

    // Execute with auto-approved tools (no interactive user)
    let result = crate::cron::send_scope::with_permitted_targets(
        permitted_targets,
        agent.send_message_with_tools_and_callback(
            session_id,
            turn_prompt.to_string(),
            effective_model,
            None, // no cancel token
            Some(Arc::new(|_| {
                // Auto-approve all tools for cron jobs
                Box::pin(async { Ok((true, false)) })
            })),
            Some(evidence_cb),
            "cron",
            None,
            None,
        ),
    )
    .await;

    match result {
        Ok(response) => {
            let clean = crate::utils::sanitize::strip_llm_artifacts(&response.content);
            // #1703: a `success` verdict requires execution evidence. A turn
            // that started zero tools cannot be told apart from a stale
            // context replay, so the row is recorded `no_op` and every
            // destination receives a timestamped one-line notice instead of
            // the report.
            let starts = tool_starts.load(std::sync::atomic::Ordering::Relaxed);

            tracing::info!(
                "Cron job '{}' completed — {} tokens, ${:.6}, {} tool start(s)",
                job.name,
                response.usage.input_tokens + response.usage.output_tokens,
                response.cost,
                starts
            );

            let at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let deliver = match classify_turn_outcome(starts, &job.name, &run_id, &at) {
                TurnOutcome::Success => {
                    // Save result to DB
                    if let Err(e) = run_repo
                        .complete_success(
                            &run_id,
                            &clean,
                            response.usage.input_tokens as i64,
                            response.usage.output_tokens as i64,
                            response.cost,
                        )
                        .await
                    {
                        tracing::error!("Failed to save cron run result to DB: {e:#}");
                    }
                    // #1703 fix 2: the ledger row keeps the raw report; the
                    // delivered copy carries the run that produced it.
                    stamp_delivery(&clean, &run_id, &run_started_at)
                }
                TurnOutcome::NoOp(notice) => {
                    tracing::warn!(
                        "Cron job '{}' executed no tools this run, recorded as no_op, report suppressed (#1703)",
                        job.name
                    );
                    // The produced report stays on the row for forensics: a
                    // replay is only diagnosable when its text survives the
                    // gate.
                    if let Err(e) = run_repo
                        .complete_no_op(
                            &run_id,
                            &clean,
                            response.usage.input_tokens as i64,
                            response.usage.output_tokens as i64,
                            response.cost,
                        )
                        .await
                    {
                        tracing::error!("Failed to save cron no-op run to DB: {e:#}");
                    }
                    notice
                }
            };

            // Optionally deliver to configured channels too
            if let Some(ref deliver_to) = job.deliver_to {
                for target in deliver_to
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let _ = deliver_result(
                        target,
                        &job.name,
                        &deliver,
                        job.deliver_api_key.as_deref(),
                        Some(ctx.pool()),
                    )
                    .await;
                }
            }

            // Maybe dispatch goal to session
            let _ = crate::cron::PipelineExecutor::maybe_dispatch_goal(job, ctx, &deliver).await;
        }
        Err(e) => {
            tracing::error!("Cron job '{}' agent error: {e:#}", job.name);

            // Save error to DB
            let error_msg = format!("{e}");
            if let Err(db_err) = run_repo.complete_error(&run_id, &error_msg).await {
                tracing::error!("Failed to save cron run error to DB: {db_err}");
            }

            // Optionally deliver error to configured channels too
            if let Some(ref deliver_to) = job.deliver_to {
                // #1703 fix 2: scheduled error deliveries are messages the
                // run produced too, they carry their run stamp as well.
                let msg = stamp_delivery(
                    &format!("Cron job '{}' failed: {e}", job.name),
                    &run_id,
                    &run_started_at,
                );
                for target in deliver_to
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let _ = deliver_result(
                        target,
                        &job.name,
                        &msg,
                        job.deliver_api_key.as_deref(),
                        Some(ctx.pool()),
                    )
                    .await;
                }
            }
        }
    }

    // No boundary written here on purpose (#1703 fix 3): the run OPENED with
    // one, before its turn read history. A second marker per run would break
    // the 1:1 run-to-boundary ratio that makes stale context attributable,
    // and nothing after the turn needs sealing - the next fire writes its own.
    Ok(())
}

/// Parse the Telegram target out of a `deliver_to` entry (fork #104).
/// Grammar: `telegram:<chat_id>` → `(chat_id, None)` — the chat's default
/// topic, the behavior every existing job keeps; `telegram:<chat_id>:<thread_id>`
/// → `(chat_id, Some(thread_id))` — opt-in delivery into that forum topic.
/// Anything else (non-numeric components, an extra `:` segment) → `None`;
/// the caller owns the loud failure. Thread delivery is opt-in only: a job
/// configured without a thread component never gets retargeted (#1085
/// scope discipline).
pub(crate) fn parse_telegram_target(target: &str) -> Option<(i64, Option<i64>)> {
    let mut parts = target.split(':');
    let chat_id = parts.next()?.trim().parse::<i64>().ok()?;
    match parts.next() {
        None => Some((chat_id, None)),
        Some(thread) => {
            let thread_id = thread.trim().parse::<i64>().ok()?;
            // A third component means the target is malformed, not deeply nested.
            if parts.next().is_some() {
                return None;
            }
            Some((chat_id, Some(thread_id)))
        }
    }
}

/// How a `discord:` `deliver_to` entry says to post the report (#1851).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscordDelivery {
    /// One message into the channel: the behaviour every existing job keeps.
    Channel,
    /// One forum post per report in a `GUILD_FORUM`/`GUILD_MEDIA` channel.
    Forum,
}

/// Parse the Discord target out of a `deliver_to` entry (#1851).
/// Grammar: `discord:<channel_id>` -> `(id, Channel)`, what every existing job
/// stores, unchanged; `discord:<channel_id>:forum` -> `(id, Forum)`, opt-in
/// delivery as one forum post per report, the Discord shape of the Telegram
/// topic workflow. Anything else (non-numeric id, unknown mode, a further
/// component) -> `None`; the caller owns the loud failure, exactly as
/// [`parse_telegram_target`] does for its own grammar.
///
/// The `:forum` suffix is a delivery mode, not a second address: the send scope
/// keeps the bare channel id, because that is the form `discord_send` checks
/// (`brain/tools/discord_send.rs:60` passes a numeric `channel_id`).
pub(crate) fn parse_discord_target(target: &str) -> Option<(String, DiscordDelivery)> {
    let mut parts = target.split(':');
    let id = parts.next()?.trim();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match parts.next() {
        None => Some((id.to_string(), DiscordDelivery::Channel)),
        Some(mode) => {
            // A further component means the target is malformed, not nested.
            if parts.next().is_some() {
                return None;
            }
            if mode.trim().eq_ignore_ascii_case("forum") {
                Some((id.to_string(), DiscordDelivery::Forum))
            } else {
                None
            }
        }
    }
}

/// `Channel Flags`: `REQUIRE_TAG = 1 << 4`, "whether a tag is required to be
/// specified when creating a thread in a `GUILD_FORUM` or a `GUILD_MEDIA`
/// channel. Tags are specified in the `applied_tags` field"
/// (discord-api-docs `developers/resources/channel.mdx:98`).
const CHANNEL_FLAG_REQUIRE_TAG: u64 = 1 << 4;

/// The tag policy this ships with, chosen from the two #1851 offered: posts go
/// out with **no** `applied_tags`, so a forum carrying `REQUIRE_TAG` is refused
/// before the request instead of dying on a 400 nobody reads. No
/// `discord.forum_report_tag` knob is added here; that is the other option and
/// stays open as its own change.
///
/// `flags` is optional on the channel object, so absence means 0: no tag
/// required.
pub(crate) fn forum_requires_tag(channel: &serde_json::Value) -> bool {
    channel
        .get("flags")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|flags| flags & CHANNEL_FLAG_REQUIRE_TAG != 0)
}

/// Tag names the channel advertises, so the refusal log says what the operator
/// is dealing with (`available_tags` at channel.mdx:48, Forum Tag `name` at
/// :386-388).
pub(crate) fn forum_tag_names(channel: &serde_json::Value) -> Vec<String> {
    channel
        .get("available_tags")
        .and_then(serde_json::Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `GUILD_FORUM` = 15 and `GUILD_MEDIA` = 16 (channel.mdx:79-80): the two types
/// documented as channels that can only contain threads, and the only two
/// `Start Thread in Forum or Media Channel` accepts. A `:forum` target pointed
/// at a text channel therefore gets a precise log line instead of a bare 400.
pub(crate) fn is_thread_only_channel(kind: u64) -> bool {
    matches!(kind, 15 | 16)
}

/// Upper bound on a forum post name: `name` is a "1-100 character channel name"
/// (channel.mdx:681). Serenity's builder documents 2-100
/// (`builder/create_forum_post.rs:37`), so the crate's tighter floor is
/// respected too: a name shorter than 2 characters posts under the timestamp
/// alone rather than as a stub.
const FORUM_POST_NAME_MAX: usize = 100;

/// Post title: job name plus timestamp, which is what makes the archive
/// searchable per job. The timestamp is kept whole, because it is what orders
/// and greps posts, so the job name is the part that gives up room.
pub(crate) fn forum_post_title(job_name: &str, when: chrono::DateTime<Utc>) -> String {
    let stamp = when.format("%Y-%m-%d %H:%M UTC").to_string();
    let joiner = " | ";
    let budget = FORUM_POST_NAME_MAX.saturating_sub(stamp.len() + joiner.len());
    let head: String = job_name.trim().chars().take(budget).collect();
    let head = head.trim_end();
    if head.chars().count() < 2 {
        return stamp;
    }
    format!("{head}{joiner}{stamp}")
}

/// Body for `POST /channels/{channel.id}/threads` (channel.mdx:651-652): the
/// post `name` and the first message of the thread (:676-680, message params at
/// :684-690). `applied_tags` is deliberately absent; [`forum_requires_tag`] is
/// the policy that makes sending no tags safe.
pub(crate) fn forum_post_body(name: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "message": { "content": content },
    })
}

/// Parse a session target out of a `deliver_to` entry (fork #144).
/// Grammar: `session:<uuid-or-8+-char-prefix>` → the target session id.
/// Full UUIDs pass through untouched; anything else is matched as a
/// case-insensitive prefix against the session DB via the shared resolver
/// (`crate::cli::session_resolve`) so operators can paste the short id that
/// `session list` prints. Anything else → `None`; the caller owns the loud
/// failure.
pub(crate) async fn parse_session_target(target: &str) -> Option<Uuid> {
    // Full UUID fast path — no DB needed (resolver passthrough parity).
    if let Ok(uuid) = Uuid::parse_str(target) {
        return Some(uuid);
    }
    let config = crate::config::Config::load().ok()?;
    let db = crate::db::Database::connect(&config.database.path)
        .await
        .ok()?;
    let sessions = crate::db::repository::SessionRepository::new(db.pool().clone())
        .list(crate::db::repository::SessionListOptions::default())
        .await
        .ok()?;
    resolve_session_target(&sessions, target)
}

/// Pure resolution over a session set — the testable core. Full UUIDs are
/// already consumed by the fast path in [`parse_session_target`], so
/// everything reaching here is a prefix: the shared resolver's rules apply
/// verbatim (0 matches and ambiguity are both `None`; the caller logs loudly).
pub(crate) fn resolve_session_target(
    sessions: &[crate::db::models::Session],
    target: &str,
) -> Option<Uuid> {
    crate::cli::session_resolve::resolve_session_id(sessions, target).ok()
}

/// Deliver a cron job result to the specified channel.
/// Format: "telegram:chat_id", "telegram:chat_id:thread_id" (opt-in forum
/// topic), "discord:channel_id", "slack:channel_id", "session:<id|prefix>"
/// (fork #144: into a session's notify queue through the shared
/// `notify_policy` path — default mode `turn-end`, cron results are turn
/// outputs), or an HTTP(S) URL for generic webhook delivery.
/// Ledger outcome of a completed cron agent turn (#1703 hole 1). A
/// `success` verdict requires execution evidence: at least one tool must
/// have started inside the run. Zero starts is indistinguishable from a
/// stale context replay, so the turn is recorded `no_op` and its report is
/// replaced by a timestamped notice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TurnOutcome {
    /// At least one tool executed: the report is stored and delivered.
    Success,
    /// Zero tools executed: the produced report stays on the row for
    /// forensics, and the carried notice is what every destination gets.
    NoOp(String),
}

/// Pure decision behind the evidence gate: `starts` counts the tool
/// executions observed inside this run, `at` is the completion timestamp
/// written into the notice.
pub(crate) fn classify_turn_outcome(
    starts: usize,
    job_name: &str,
    run_id: &str,
    at: &str,
) -> TurnOutcome {
    if starts == 0 {
        TurnOutcome::NoOp(not_executed_notice(job_name, run_id, at))
    } else {
        TurnOutcome::Success
    }
}

/// The one-line notice that replaces the report of a zero-evidence run
/// (#1703). Timestamped and run-stamped so a reader can anchor it.
pub(crate) fn not_executed_notice(job_name: &str, run_id: &str, at: &str) -> String {
    format!(
        "Cron job '{job_name}' not executed: no tool ran in this run ({at}, run {run_id}). The report was suppressed."
    )
}

/// #1703 fix 2: the run stamp carried by every delivered cron message, so a
/// reader can anchor a message to the ledger row that produced it even when
/// the writer cannot tell fresh work from a stale replay. Short run id plus
/// the run's own start time: the start time is what separates two runs of
/// the same job.
pub(crate) fn run_stamp(run_id: &str, started_at: &str) -> String {
    let short = run_id.get(..8).unwrap_or(run_id);
    format!("[run {short}, started {started_at}]")
}

/// Append [`run_stamp`] to delivered content, never twice: text already
/// carrying this run's stamp passes through unchanged, so no call site can
/// stack stamps by accident (#1703).
pub(crate) fn stamp_delivery(content: &str, run_id: &str, started_at: &str) -> String {
    let stamp = run_stamp(run_id, started_at);
    if content.contains(&stamp) {
        content.to_string()
    } else {
        format!("{content}\n\n{stamp}")
    }
}

/// Execution-evidence probe for one cron run (#1703): a shared counter fed
/// by a progress callback that only counts `ToolStarted`. Streaming chunks,
/// intermediate text and completions must never inflate the evidence.
pub(crate) fn tool_start_counter() -> (
    Arc<std::sync::atomic::AtomicUsize>,
    crate::brain::agent::ProgressCallback,
) {
    use crate::brain::agent::{ProgressCallback, ProgressEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let starts = Arc::new(AtomicUsize::new(0));
    let cb: ProgressCallback = {
        let starts = starts.clone();
        Arc::new(move |_sid: Uuid, event: ProgressEvent| {
            if matches!(event, ProgressEvent::ToolStarted { .. }) {
                starts.fetch_add(1, Ordering::Relaxed);
            }
        })
    };
    (starts, cb)
}

/// Bound a run's output before delivery.
///
/// Every channel that has a message limit already owns a chunker for it: the
/// Telegram outbox splits at 4096, Discord at 2000, Slack at 3500, and the
/// session queue has no limit at all. This function therefore must not squeeze
/// a report into a single message. The old 4000-byte cut dropped the tail of
/// every longer report while the chunker below would have shipped it (#2252).
///
/// What is left is a runaway guard: a job that dumps hundreds of kilobytes must
/// not turn into hundreds of API posts and earn a flood ban, so anything past
/// `MAX_LEN` is cut there. The cut must land on a UTF-8 char boundary: the raw
/// `&content[..max_len]` panicked the scheduler when byte 4000 fell inside a
/// multibyte char (`✅` in a morning recap, #1919).
pub(crate) fn prepare_for_delivery(content: &str) -> String {
    const MAX_LEN: usize = 100_000;
    if content.len() > MAX_LEN {
        tracing::warn!(
            "Cron output is {} bytes, past the {}-byte delivery guard; sending the head only",
            content.len(),
            MAX_LEN
        );
        format!(
            "{}...\n\n(truncated at {} bytes, full output in session)",
            truncate_str(content, MAX_LEN),
            MAX_LEN
        )
    } else {
        content.to_string()
    }
}

pub(crate) async fn deliver_result(
    deliver_to: &str,
    job_name: &str,
    content: &str,
    api_key: Option<&str>,
    pool: Option<crate::db::Pool>,
) -> Option<tokio::task::JoinHandle<()>> {
    // Only the Telegram delivery arm uses the pool (to record the message for
    // reply recovery); other targets ignore it.
    #[cfg(not(feature = "telegram"))]
    let _ = &pool;
    // HTTP(S) URL — generic webhook delivery
    if deliver_to.starts_with("http://") || deliver_to.starts_with("https://") {
        deliver_http(deliver_to, job_name, content, api_key).await;
        return None;
    }

    // Leaked `oc://` target URL at fire time (#148 loud failure pin):
    // cron targets must be baked at create/update time. A leaked URL here
    // means the bake step was bypassed — refuse loudly.
    if crate::channels::target_resolver::is_target_url(deliver_to) {
        tracing::error!(
            "Unbaked target URL '{deliver_to}' reached delivery for job '{job_name}' — oc:// targets must be baked at create/update time (#148); refusing fire-time resolution"
        );
        return None;
    }

    let parts: Vec<&str> = deliver_to.splitn(2, ':').collect();
    if parts.len() != 2 {
        tracing::warn!(
            "Invalid deliver_to format '{}' for job '{}' — expected 'channel:id' or HTTP URL",
            deliver_to,
            job_name
        );
        return None;
    }

    let (channel, target_id) = (parts[0], parts[1]);

    // Per-channel chunkers own the message limits; this only guards runaway
    // output (#2252).
    let msg = prepare_for_delivery(content);

    let delivery_msg = format!("⏰ **Cron: {job_name}**\n\n{msg}");

    match channel {
        "session" => {
            // Fork #144: deliver into a session's notify queue. Cron results
            // are turn outputs, so the default mode is `turn-end` (never
            // derail a mid-turn session — the target drains at its next
            // boundary); `quiet` rides the same policy when configured.
            let Some(session_id) = parse_session_target(target_id).await else {
                tracing::error!(
                    "Invalid session deliver_to target '{target_id}' for job '{job_name}' \
                     — no session matches; not delivering"
                );
                return None;
            };
            tracing::info!("Delivering cron result to session {session_id} (mode turn-end)");
            let queued = crate::brain::agent::service::QueuedUserMessage {
                context_text: delivery_msg.clone(),
                display_text: delivery_msg,
                origin: crate::brain::agent::PushOrigin::SessionNotify,
                bg_meta: None,
            };
            let delivery = crate::brain::agent::service::session_routes::deliver_to_session(
                session_id, queued, false,
            );
            tracing::info!("Cron '{job_name}' session delivery verdict: {delivery:?}");
            return None;
        }
        "telegram" => {
            #[cfg(feature = "telegram")]
            {
                match parse_telegram_target(target_id) {
                    Some((cid, thread_id)) => {
                        tracing::info!(
                            "Delivering cron result to Telegram chat {cid}{}",
                            thread_id
                                .map(|t| format!(" thread {t}"))
                                .unwrap_or_default()
                        );
                        return deliver_telegram(
                            cid,
                            thread_id,
                            job_name,
                            &delivery_msg,
                            pool.clone(),
                        )
                        .await;
                    }
                    None => {
                        tracing::error!(
                            "Invalid Telegram deliver_to target '{target_id}' for job \
                             '{job_name}' — expected 'telegram:<chat_id>' or \
                             'telegram:<chat_id>:<thread_id>'; not delivering"
                        );
                    }
                }
            }
            #[cfg(not(feature = "telegram"))]
            {
                tracing::warn!("Telegram feature not enabled — cannot deliver cron result");
            }
        }
        "discord" => {
            #[cfg(feature = "discord")]
            {
                match parse_discord_target(target_id) {
                    Some((channel_id, DiscordDelivery::Channel)) => {
                        tracing::info!("Delivering cron result to Discord channel {channel_id}");
                        deliver_discord(&channel_id, &delivery_msg).await;
                    }
                    Some((channel_id, DiscordDelivery::Forum)) => {
                        tracing::info!(
                            "Delivering cron result as one forum post in Discord channel {channel_id}"
                        );
                        deliver_discord_forum(&channel_id, job_name, &delivery_msg).await;
                    }
                    None => {
                        tracing::error!(
                            "Invalid Discord deliver_to target '{target_id}' for job '{job_name}' \
                             (expected 'discord:<channel_id>' or 'discord:<channel_id>:forum'); \
                             not delivering"
                        );
                    }
                }
            }
            #[cfg(not(feature = "discord"))]
            {
                tracing::warn!("Discord feature not enabled — cannot deliver cron result");
            }
        }
        "slack" => {
            #[cfg(feature = "slack")]
            {
                tracing::info!("Delivering cron result to Slack channel {target_id}");
                deliver_slack(target_id, &delivery_msg).await;
            }
            #[cfg(not(feature = "slack"))]
            {
                tracing::warn!("Slack feature not enabled — cannot deliver cron result");
            }
        }
        other => {
            tracing::warn!("Unknown delivery channel '{other}' for job '{job_name}'");
        }
    }
    None
}

/// Execute direct 0-token trigger delivery (when prompt is empty and trigger fired).
async fn execute_direct_trigger_job(
    job: &CronJob,
    ctx: &ServiceContext,
    run_repo: &CronJobRunRepository,
    trig_res: &crate::cron::TriggerResult,
) -> anyhow::Result<()> {
    let run = CronJobRun::new_running(
        job.id,
        job.name.clone(),
        job.provider.clone(),
        job.model.clone(),
    );
    let run_id = run.id.to_string();
    if let Err(e) = run_repo.insert(&run).await {
        tracing::error!("Failed to insert cron run record: {e:#}");
    }

    let raw_output = trig_res.combined_output();
    let content = if let Some(ref tmpl) = job.goal_template {
        crate::cron::interpolate_template(tmpl, trig_res)
    } else {
        raw_output
    };

    let clean = crate::utils::sanitize::strip_llm_artifacts(&content);

    tracing::info!(
        "Cron job '{}' direct trigger completed — 0 tokens, $0.00",
        job.name
    );

    // Save result to DB (0 tokens)
    if let Err(e) = run_repo.complete_success(&run_id, &clean, 0, 0, 0.0).await {
        tracing::error!("Failed to save direct cron run result to DB: {e:#}");
    }

    // Deliver to configured channels
    if let Some(ref deliver_to) = job.deliver_to {
        for target in deliver_to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let _ = deliver_result(
                target,
                &job.name,
                &clean,
                job.deliver_api_key.as_deref(),
                Some(ctx.pool()),
            )
            .await;
        }
    }

    // Maybe dispatch goal to session
    let _ = crate::cron::PipelineExecutor::maybe_dispatch_goal(job, ctx, &clean).await;

    Ok(())
}
/// Deliver cron result via HTTP POST to a generic webhook URL.
async fn deliver_http(url: &str, job_name: &str, content: &str, api_key: Option<&str>) {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "job_name": job_name,
        "content": content,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    let mut request = client.post(url).json(&body);

    // Attach Bearer token if the job has one configured.
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("Cron result for '{job_name}' delivered to {url}");
        }
        Ok(resp) => {
            tracing::warn!(
                "HTTP delivery to {url} failed ({}): {:?}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Err(e) => {
            tracing::error!("HTTP delivery to {url} error: {e:#}");
        }
    }
}

/// Read `channels.<channel>.<field>` (e.g. a bot token) from the active
/// workspace's `keys.toml`. Cron delivery runs outside any channel's live
/// connection, so it reads the credential straight off disk.
#[cfg(any(feature = "telegram", feature = "discord", feature = "slack"))]
fn read_channel_secret(channel: &str, field: &str) -> Option<String> {
    let keys_path = crate::brain::BrainLoader::resolve_path().join("keys.toml");
    let content = std::fs::read_to_string(&keys_path).ok()?;
    content.parse::<toml::Table>().ok().and_then(|t| {
        t.get("channels")?
            .as_table()?
            .get(channel)?
            .as_table()?
            .get(field)?
            .as_str()
            .map(String::from)
    })
}

/// Split `text` into `<= max_len` byte chunks, breaking on a newline near the
/// limit when possible and never inside a multi-byte char. Used for Slack's
/// message limit. (Discord uses its own fence-aware `split_message`; Telegram
/// reuses its own chunker so HTML stays valid across splits.)
#[cfg(feature = "slack")]
fn split_for_delivery(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max_len).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let break_at = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .filter(|&pos| pos > end - start - 200)
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

/// Chunk a cron report for Discord, converting the markdown Discord cannot
/// render (#2069).
///
/// Discord has no table markup, so a pipe table arrives as its own source in a
/// proportional font where no column lines up. Every interactive path rewrites
/// the text first; cron delivery was the one outbound path that sent it raw.
///
/// The chunker is the Discord chat path's own `split_message`, not the generic
/// `split_for_delivery`. The grid render wraps a table in a fenced text block,
/// and only `split_message` closes a fence it has to break and reopens it on
/// the next chunk (#876). The generic splitter breaks at the nearest newline,
/// so a grid straddling the limit would arrive cut in half: one chunk holding
/// an unclosed fence, the next starting mid-table.
#[cfg(feature = "discord")]
pub(crate) fn discord_chunks_for_delivery(message: &str) -> Vec<String> {
    let normalized = crate::channels::discord::table_convert::tables_to_discord(message);
    crate::channels::discord::handler::split_message(&normalized, 2000)
}

/// Chunk a cron report for Slack, converting the markdown Slack cannot render
/// (#2069).
///
/// Slack renders neither tables nor headings: a pipe table arrives as its own
/// source and a `##` heading as two literal hashes. The chat path calls
/// `structure_to_slack` for both, so cron delivery calls the same function
/// rather than the table half alone.
///
/// The generic splitter is safe here. `structure_to_slack` emits bold labels,
/// `└` continuations and `---` rules, so it never opens a fence that a break
/// could cut.
#[cfg(feature = "slack")]
pub(crate) fn slack_chunks_for_delivery(message: &str) -> Vec<String> {
    let normalized = crate::channels::slack::table_convert::structure_to_slack(message);
    split_for_delivery(&normalized, 3500)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Deliver via the shared Telegram outbox ladder (#1085 P1b R2). The
/// hand-rolled raw `reqwest` POST + message_id JSON scrape is gone: retry
/// (#297), 4096 chunking, plain-text fallback (Q4) and correlation
/// telemetry come from `send_markdown_outbox`, and the delivery is
/// persisted through the same `record_outgoing` the tool path uses.
#[cfg(feature = "telegram")]
async fn deliver_telegram(
    chat_id: i64,
    thread_id: Option<i64>,
    job_name: &str,
    message: &str,
    pool: Option<crate::db::Pool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(token) = read_channel_secret("telegram", "token") else {
        tracing::warn!("No Telegram bot token found in keys.toml — cannot deliver cron result");
        return None;
    };
    let bot = teloxide::Bot::new(token);
    use teloxide::prelude::Requester;
    // Opt-in thread delivery (fork #104): `telegram:<chat_id>:<thread_id>`
    // routes into that forum topic; a bare `telegram:<chat_id>` stays on the
    // chat's default topic — existing jobs are never silently retargeted
    // (#1085 scope discipline).
    let thread = thread_id.map(|t| teloxide::types::ThreadId(teloxide::types::MessageId(t as i32)));
    // Fire-time validation for the opt-in path: the chat must be a forum
    // with topics enabled. A thread pointed at a non-forum chat is a
    // configuration error — fail LOUDLY here rather than drop the message
    // into General (that fallback is the exact silent-retargeting behavior
    // the deliver_to grammar must never perform). Topic *existence* has no
    // Bot API read; it is proven by the send itself, and a dead topic id
    // surfaces as a loud send error below.
    if let Some(tid) = thread_id {
        match bot.get_chat(teloxide::types::ChatId(chat_id)).await {
            Ok(chat) if is_forum_chat(&chat) => {}
            Ok(_) => {
                tracing::error!(
                    "Cron job '{job_name}': deliver_to thread {tid} rejected — chat {chat_id} \
                     is not a forum (topics disabled); refusing delivery instead of falling \
                     back to the default topic"
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Cron job '{job_name}': cannot validate chat {chat_id} for thread \
                     {tid} delivery: {e:#} — refusing delivery"
                );
                return None;
            }
        }
    }
    // Delivery runs detached (review F18): the shared outbox ladder can
    // legally wait out 429 windows (up to ~90s total per send). Awaiting
    // that inline would stall the whole scheduler tick — one flood-banned
    // chat must not delay every other job. The outbox telemetry carries
    // the outcome either way.
    //
    // Returns the spawn handle so rebuild jobs can await delivery before
    // exec() replaces the process (#1105). Normal cron jobs discard the
    // handle — their delivery survives the tick regardless.
    let message = message.to_string();
    let job_name = job_name.to_string();
    Some(tokio::spawn(async move {
        match crate::channels::telegram::send::send_markdown_outbox(
            &bot,
            teloxide::types::ChatId(chat_id),
            thread,
            &message,
            "cron",
            &job_name,
            None,
        )
        .await
        {
            Ok(outbox) => {
                tracing::info!(
                    "Cron result for '{job_name}' delivered to Telegram chat {chat_id}{} ({} part(s))",
                    outbox
                        .effective_thread_id
                        .map(|t| format!(" thread {}", t.0.0))
                        .unwrap_or_default(),
                    outbox.sent.len()
                );
                // Persist keyed by message id so a reply to the cron post
                // resolves to this exact content (#234, #169).
                outbox.record_outgoing(pool, chat_id).await;
            }
            Err(e) => {
                if let Some(t) = thread_id {
                    tracing::error!(
                        "Cron delivery for '{job_name}' to chat {chat_id} thread {t} failed: {e:#} — \
                         if the error is 'message thread not found', topic {t} does not exist \
                         in chat {chat_id}; fix the job's deliver_to (there is no fallback to the \
                         default topic)"
                    );
                } else {
                    tracing::error!(
                        "Cron delivery for '{job_name}' to chat {chat_id} failed: {e:#}"
                    );
                }
            }
        }
    }))
}

/// Whether a `get_chat` result describes a forum (topics-enabled) group.
/// Only supergroups carry the `is_forum` flag; channels/groups/private
/// chats are all non-forum, so opt-in thread delivery rejects them.
#[cfg(feature = "telegram")]
fn is_forum_chat(chat: &teloxide::types::ChatFullInfo) -> bool {
    matches!(
        &chat.kind,
        teloxide::types::ChatFullInfoKind::Public(public)
            if matches!(
                &public.kind,
                teloxide::types::ChatFullInfoPublicKind::Supergroup(supergroup)
                    if supergroup.is_forum
            )
    )
}

/// Deliver via Discord Bot API (direct HTTP POST to the channel-messages
/// endpoint). The report is rewritten for Discord first, so a table arrives as
/// a monospace grid or bold labels instead of its own source (#2069), and then
/// chunked to the 2000-char message limit.
#[cfg(feature = "discord")]
async fn deliver_discord(channel_id: &str, message: &str) {
    let Some(token) = read_channel_secret("discord", "token") else {
        tracing::warn!("No Discord bot token found in keys.toml — cannot deliver cron result");
        return;
    };

    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages");
    let client = reqwest::Client::new();
    let mut delivered = 0usize;
    for chunk in discord_chunks_for_delivery(message) {
        let body = serde_json::json!({ "content": chunk });
        match client
            .post(&url)
            .header("Authorization", format!("Bot {token}"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => delivered += 1,
            Ok(resp) => {
                tracing::warn!(
                    "Discord delivery to {channel_id} failed ({}): {:?}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
            Err(e) => {
                tracing::error!("Discord delivery to {channel_id} HTTP error: {e:#}");
            }
        }
    }
    if delivered > 0 {
        tracing::info!(
            "Cron result delivered to Discord channel {channel_id} ({delivered} part(s))"
        );
    }
}

/// Deliver one cron report as a single forum post (#1851).
///
/// `POST /channels/{forum_id}/threads` (channel.mdx:651-652) creates the post
/// with the report's title and its first 2000-character chunk; any remaining
/// chunks ride into the thread that call returns, so the parent forum keeps
/// exactly one post per report instead of a stack of loose messages.
///
/// The channel object is read once first (`GET /channels/{id}`, channel.mdx:395
/// -398) to apply the tag policy in [`forum_requires_tag`] and to confirm the
/// target is a forum or media channel. Both checks fail loudly in the log
/// rather than firing a request Discord is going to reject.
#[cfg(feature = "discord")]
async fn deliver_discord_forum(forum_id: &str, job_name: &str, message: &str) {
    let Some(token) = read_channel_secret("discord", "token") else {
        tracing::warn!("No Discord bot token found in keys.toml; cannot deliver cron result");
        return;
    };

    let client = reqwest::Client::new();
    let base = "https://discord.com/api/v10";
    let auth = format!("Bot {token}");

    let channel: serde_json::Value = match client
        .get(format!("{base}/channels/{forum_id}"))
        .header("Authorization", &auth)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(
                    "Discord forum delivery: unreadable channel object for {forum_id}: {e:#}"
                );
                return;
            }
        },
        Ok(resp) => {
            tracing::error!(
                "Discord forum delivery: cannot read channel {forum_id} ({}): {:?}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
            return;
        }
        Err(e) => {
            tracing::error!("Discord forum delivery: HTTP error reading channel {forum_id}: {e:#}");
            return;
        }
    };

    let Some(kind) = channel.get("type").and_then(serde_json::Value::as_u64) else {
        tracing::error!(
            "Discord forum delivery: channel {forum_id} has no type field; not posting"
        );
        return;
    };
    if !is_thread_only_channel(kind) {
        tracing::error!(
            "Discord forum delivery: channel {forum_id} is type {kind}, not GUILD_FORUM (15) or \
             GUILD_MEDIA (16): 'discord:{forum_id}:forum' has to name a forum channel; not posting"
        );
        return;
    }
    if forum_requires_tag(&channel) {
        tracing::error!(
            "Discord forum delivery: channel {forum_id} sets REQUIRE_TAG, so every post needs \
             applied_tags and this build sends none (the #1851 policy). Tags on offer: [{}]. \
             Point the job at a tagless forum, or land the configured-tag option; not posting",
            forum_tag_names(&channel).join(", ")
        );
        return;
    }

    let title = forum_post_title(job_name, Utc::now());
    let chunks = discord_chunks_for_delivery(message);
    let first = chunks.first().map(String::as_str).unwrap_or("");

    let mut posted = 0usize;
    let thread_id = match client
        .post(format!("{base}/channels/{forum_id}/threads"))
        .header("Authorization", &auth)
        .json(&forum_post_body(&title, first))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            posted += 1;
            resp.json().await.ok().and_then(|json: serde_json::Value| {
                json.get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        }
        Ok(resp) => {
            tracing::error!(
                "Discord forum delivery to {forum_id} failed ({}): {:?}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
            None
        }
        Err(e) => {
            tracing::error!("Discord forum delivery to {forum_id} HTTP error: {e:#}");
            None
        }
    };

    let Some(thread_id) = thread_id else {
        if posted == 0 {
            tracing::error!(
                "Discord forum delivery: no post created in {forum_id}; report not delivered"
            );
        }
        return;
    };

    for chunk in chunks.iter().skip(1) {
        let body = serde_json::json!({ "content": chunk });
        match client
            .post(format!("{base}/channels/{thread_id}/messages"))
            .header("Authorization", &auth)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => posted += 1,
            Ok(resp) => {
                tracing::warn!(
                    "Discord forum follow-up to thread {thread_id} failed ({}): {:?}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
            Err(e) => {
                tracing::error!("Discord forum follow-up to thread {thread_id} HTTP error: {e:#}");
            }
        }
    }

    tracing::info!(
        "Cron result delivered as forum post '{title}' in channel {forum_id} \
         ({posted} message(s), thread {thread_id})"
    );
}

/// Deliver via Slack Web API (`chat.postMessage`). The `text` field renders
/// Slack mrkdwn. Slack returns HTTP 200 even on a logical failure
/// (`{"ok":false,"error":...}`), so we inspect the body, not just the status.
#[cfg(feature = "slack")]
async fn deliver_slack(channel_id: &str, message: &str) {
    let Some(token) = read_channel_secret("slack", "token") else {
        tracing::warn!("No Slack bot token found in keys.toml — cannot deliver cron result");
        return;
    };

    let url = "https://slack.com/api/chat.postMessage";
    let client = reqwest::Client::new();
    let mut delivered = 0usize;
    for chunk in slack_chunks_for_delivery(message) {
        let body = serde_json::json!({ "channel": channel_id, "text": chunk });
        match client
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let parsed: serde_json::Value = resp.json().await.unwrap_or_default();
                if parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                    delivered += 1;
                } else {
                    tracing::warn!(
                        "Slack delivery to {channel_id} failed: {}",
                        parsed
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown error")
                    );
                }
            }
            Err(e) => {
                tracing::error!("Slack delivery to {channel_id} HTTP error: {e:#}");
            }
        }
    }
    if delivered > 0 {
        tracing::info!("Cron result delivered to Slack channel {channel_id} ({delivered} part(s))");
    }
}
