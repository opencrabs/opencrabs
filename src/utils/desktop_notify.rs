//! OS-level desktop notifications for completion events (#2729).
//!
//! The GUI already shows a chime and a visual badge when work finishes. This
//! module carries the same signal outside the window: a real OS notification
//! you can see when you walked away from the machine.
//!
//! ## Why no `UNUserNotificationCenter` on macOS
//!
//! A probe run on this project (receipt:
//! `~/.opencrabs/research/desktop-notify-probe.md`) measured the alternatives
//! instead of guessing:
//!
//! - A bare Mach-O binary calling `UNUserNotificationCenter.current()` throws
//!   `NSInternalInconsistencyException: bundleProxyForCurrentProcess is nil`
//!   and the process dies with SIGABRT (exit 134). That is not a catchable
//!   error, it is a kill, so the TUI cannot use that API at all.
//! - An ad-hoc signed bundle placed in a hidden directory is refused with
//!   `UNErrorDomain Code=1 "Notifications are not allowed for this
//!   application"` and never prompts. The same bundle copied into
//!   `~/Applications` gets authorized and delivers a notification. Location
//!   is the gate, not the signature (this machine has zero signing identities).
//! - `osascript -e 'display notification'` works from a bare binary.
//!
//! So the macOS arm hands the banner to `OpenCrabs Notifier.app`, a tiny
//! helper bundle living under `~/Applications`. That location is what macOS
//! authorizes, and because the helper carries our `icon.icns` the banner shows
//! the crab. When the helper is missing the arm falls back to
//! `osascript -e 'display notification'`, which runs from a bare binary but
//! attributes the banner to the parent application.
//!
//! ## Failure policy
//!
//! Every transport is best effort. A missing helper binary, a laptop with no
//! logged-in desktop session, or a sandboxed container all end in one debug
//! log line and a `false` return. Nothing here ever blocks the caller, and
//! nothing here panics.

use std::collections::{HashSet, VecDeque};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use uuid::Uuid;

use crate::tui::plan::{PlanDocument, TaskStatus};

/// Set to a non-empty value other than `0` to disable desktop notifications.
///
/// Accepts `1`, `true`, `yes` and `on`. Anything else leaves notifications on,
/// so an empty variable in a shell profile does not silently mute the feature.
pub const ENV_OPT_OUT: &str = "OPENCRABS_NO_DESKTOP_NOTIFY";

/// Display name used as the notification source / app name.
#[cfg(target_os = "linux")]
const APP_NAME: &str = "OpenCrabs";

/// Bodies are trimmed to this many characters. A notification shows roughly
/// two lines; the full text is already in the transcript.
const MAX_BODY_CHARS: usize = 200;

/// The completion facts worth carrying to the OS.
///
/// One variant per seam that raises: a plan finishing, a single plan task
/// settling, a turn ending normally, and a turn dying. Every variant is plain
/// data with no handle to the app, so the transport stays callable from the
/// agent loop, the TUI and the GUI without any of them owning a window.
///
/// Every variant also names the session it is about (#2751). A banner you can
/// click is only worth raising if the click can land somewhere: the id travels
/// with the event, the transport hands it back when the banner is activated,
/// and the window opens that conversation. `None` means the caller has no
/// conversation to point at and the banner stays informational.
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// A whole plan reached completion. `tasks` is how many had settled.
    PlanCompleted {
        title: String,
        tasks: usize,
        session: Option<Uuid>,
    },
    /// One plan task settled. `success` is false when the task was skipped or
    /// failed, so the banner never claims work that was abandoned.
    TaskCompleted {
        label: String,
        success: bool,
        elapsed_secs: u64,
        session: Option<Uuid>,
    },
    /// The turn ended with an answer.
    TurnCompleted {
        summary: String,
        session: Option<Uuid>,
    },
    /// The turn ended in an error the user might have walked away from.
    TurnFailed {
        reason: String,
        session: Option<Uuid>,
    },
}

impl NotificationEvent {
    /// One line of banner title: what happened, and to what.
    ///
    /// Short by construction. The OS renders a title on a single line and a
    /// long plan title would push the event off screen, so the subject is
    /// trimmed here and the untrimmed text stays in the transcript.
    pub fn title(&self) -> String {
        match self {
            Self::PlanCompleted { title, .. } => {
                format!("Plan complete: {}", truncate(title, 60))
            }
            Self::TaskCompleted { label, success, .. } => format!(
                "{}: {}",
                if *success {
                    "Task complete"
                } else {
                    "Task stopped"
                },
                truncate(label, 60)
            ),
            Self::TurnCompleted { .. } => "Turn complete".to_string(),
            Self::TurnFailed { .. } => "Turn failed".to_string(),
        }
    }

    /// The detail line under the title.
    pub fn body(&self) -> String {
        match self {
            Self::PlanCompleted { tasks, .. } => format!("{tasks} tasks finished"),
            Self::TaskCompleted { success: false, .. } => "Stopped before finishing".to_string(),
            Self::TaskCompleted { elapsed_secs, .. } if *elapsed_secs > 0 => {
                format!("Finished in {elapsed_secs}s")
            }
            Self::TaskCompleted { .. } => "Finished".to_string(),
            Self::TurnCompleted { summary } => {
                let line = truncate(summary, MAX_BODY_CHARS);
                if line.trim().is_empty() {
                    "Done.".to_string()
                } else {
                    line
                }
            }
            Self::TurnFailed { reason } => truncate(reason, MAX_BODY_CHARS),
        }
    }

    /// The conversation this banner is about (#2751), when it names one.
    ///
    /// The transport reads this to decide whether a click can go anywhere, so
    /// it is the single place a variant says which session it belongs to.
    pub fn session(&self) -> Option<Uuid> {
        match self {
            Self::PlanCompleted { session, .. }
            | Self::TaskCompleted { session, .. }
            | Self::TurnCompleted { session, .. }
            | Self::TurnFailed { session, .. } => *session,
        }
    }

    /// Identity of the fact, for suppressing a repeat of the same one.
    ///
    /// Only settled plans and settled tasks carry a key. A turn is not a
    /// durable object: the same turn asked twice legitimately deserves two
    /// banners, so turns never dedupe.
    pub(crate) fn dedupe_key(&self) -> Option<String> {
        match self {
            Self::PlanCompleted { title, .. } => Some(format!("plan:{title}")),
            Self::TaskCompleted { label, .. } => Some(format!("task:{label}")),
            Self::TurnCompleted { .. } | Self::TurnFailed { .. } => None,
        }
    }
}

/// Last focus state reported by a surface. `0` means nobody reported yet.
///
/// A surface that never reports (a headless run, a channel session, a terminal
/// that does not send focus events) leaves this at unknown, and unknown RAISES
/// the banner rather than swallowing it. Silence is the one failure this
/// feature exists to prevent.
const FOCUS_UNKNOWN: u8 = 0;
const FOCUS_ACTIVE: u8 = 1;
const FOCUS_IDLE: u8 = 2;

static SURFACE_FOCUS: AtomicU8 = AtomicU8::new(FOCUS_UNKNOWN);

/// Keys already announced in this process, so a repaint that re-checks the
/// same plan cannot repeat a banner the user already saw.
static ANNOUNCED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Upper bound on remembered keys. Reaching it clears the set instead of
/// growing without limit; a repeated banner is a smaller sin than a leak.
const MAX_ANNOUNCED_KEYS: usize = 512;

/// Report whether the app surface has the user's attention right now.
///
/// The TUI calls this from its focus events, the GUI from window activation.
/// Callers that never report leave the state unknown, which raises.
pub fn set_surface_focused(focused: bool) {
    SURFACE_FOCUS.store(
        if focused { FOCUS_ACTIVE } else { FOCUS_IDLE },
        Ordering::Relaxed,
    );
}

/// Current focus state, or `None` when no surface has reported one.
fn surface_focused() -> Option<bool> {
    match SURFACE_FOCUS.load(Ordering::Relaxed) {
        FOCUS_ACTIVE => Some(true),
        FOCUS_IDLE => Some(false),
        _ => None,
    }
}

fn announced() -> &'static Mutex<HashSet<String>> {
    ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record `key` and report whether this is the first time it was seen.
///
/// A poisoned lock is recovered rather than propagated: a panic elsewhere must
/// not turn every later notification into a no-op.
pub(crate) fn first_time(key: &str) -> bool {
    let mut keys = match announced().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if keys.len() >= MAX_ANNOUNCED_KEYS {
        keys.clear();
    }
    keys.insert(key.to_string())
}

/// Decide whether an event should reach the OS.
///
/// Order is deliberate: the env opt-out is cheapest, the config gate is the
/// user's standing choice, focus suppression is transient, and dedupe is last
/// so a suppressed event does not burn its key.
///
/// Unknown focus raises. A surface that never reports is not evidence the user
/// is watching, and staying silent on a guess defeats the point.
pub(crate) fn should_raise(
    event: &NotificationEvent,
    opt_out: Option<&str>,
    config_allows: bool,
) -> bool {
    if !enabled(opt_out) || !config_allows {
        return false;
    }
    if surface_focused() == Some(true) {
        return false;
    }
    match event.dedupe_key() {
        Some(key) => first_time(&key),
        None => true,
    }
}

/// Whether the operator's config wants a banner for this event.
///
/// The four switches live beside `completion_notification_enabled`, which
/// arms them, so an unset master switch means no banners whatever the rest
/// say. A config that fails to load also means no banner: an unreadable config
/// is not consent.
fn config_allows(event: &NotificationEvent) -> bool {
    let Ok(config) = crate::config::Config::load() else {
        return false;
    };
    let agent = &config.agent;
    if !agent.completion_notification_enabled {
        return false;
    }
    match event {
        NotificationEvent::PlanCompleted { .. } => agent.notify_on_plan_completed,
        NotificationEvent::TaskCompleted { .. } => agent.notify_on_task_completed,
        NotificationEvent::TurnCompleted { .. } => agent.notify_on_turn_completed,
        NotificationEvent::TurnFailed { .. } => agent.notify_on_turn_failed,
    }
}

/// Whether a banner will be raised when a whole plan finishes.
///
/// Read on its own because the settled-task path needs the answer for an event
/// it is not raising: when the plan banner covers the finish, the task banner
/// for that same finish stands down (#2752). Mirrors the `PlanCompleted` arm of
/// [`config_allows`], including the master switch.
fn plan_completed_allowed() -> bool {
    let Ok(config) = crate::config::Config::load() else {
        return false;
    };
    let agent = &config.agent;
    agent.completion_notification_enabled && agent.notify_on_plan_completed
}

/// Settled tasks in `plans` that `seen` had not recorded yet, adding them as
/// it goes.
///
/// Split out from [`announce_settled_tasks`] so the bookkeeping is testable
/// without touching the config file or an OS transport.
pub(crate) fn fresh_settled_tasks<'a>(
    seen: &mut HashSet<Uuid>,
    plans: &[Option<&'a PlanDocument>],
) -> Vec<(Uuid, &'a str, bool)> {
    let mut fresh: Vec<(Uuid, &'a str, bool)> = Vec::new();
    for entry in plans {
        // `Option<&T>` is Copy, so this moves the reference out of the slice
        // instead of borrowing the slot.
        let Some(plan) = *entry else { continue };
        for id in crate::utils::completion_sound::resolved_task_ids(Some(plan)) {
            if !seen.insert(id) {
                continue;
            }
            let Some(task) = plan.tasks.iter().find(|task| task.id == id) else {
                continue;
            };
            let success = matches!(task.status, TaskStatus::Completed);
            fresh.push((id, task.title.as_str(), success));
        }
    }
    fresh
}

/// The settled tasks from `fresh` that deserve a banner of their own.
///
/// A plan that has nothing left to run settles its final task in the same pass
/// that the plan itself finishes, and the plan banner carries that news from
/// the turn-settle seam. Raising the task banner for that pass put two cards on
/// screen for one finish (#2752), so it stands down. A task that settles while
/// the plan is still running keeps its banner: that is the case the task
/// switch exists for. With plan banners turned off the task banner is the only
/// one left, so it still fires.
pub(crate) fn bannerable_settles<'a>(
    plan: &PlanDocument,
    fresh: &'a [(Uuid, &'a str, bool)],
    plan_banners_enabled: bool,
) -> &'a [(Uuid, &'a str, bool)] {
    if plan_banners_enabled && plan.finishes_on_this_settle() {
        return &[];
    }
    fresh
}

/// A banner click waiting to be taken (#2751).
///
/// The transport runs on a thread the window does not own: on Linux the click
/// arrives as a line on `notify-send`'s stdout, long after the event that
/// raised the banner. So the window is never called from there. The transport
/// posts the session here instead and the surface that owns the window drains
/// the queue on its next tick, on the thread that can actually move a window.
static PENDING_ACTIVATIONS: OnceLock<Mutex<VecDeque<Uuid>>> = OnceLock::new();

/// Bound on undrained clicks. A click nobody is awake to take is not worth
/// remembering forever, and eight is far more than a person produces before
/// looking at the window.
pub(crate) const MAX_PENDING_ACTIVATIONS: usize = 8;

fn pending_activations() -> &'static Mutex<VecDeque<Uuid>> {
    PENDING_ACTIVATIONS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Record that the user clicked a banner about `session`.
///
/// A poisoned lock is recovered rather than propagated, the same rule the
/// announce set follows: a panic elsewhere must not silence a later click.
pub fn post_activation(session: Uuid) {
    let mut pending = match pending_activations().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if pending.len() >= MAX_PENDING_ACTIVATIONS {
        pending.pop_front();
    }
    pending.push_back(session);
}

/// Take the oldest banner click, if one is waiting.
///
/// A plain pop with no callback and no window handle, so the caller decides
/// when and where it runs. The oldest first: a person clicks the banner they
/// are looking at, and a later click must not jump the queue in front of it.
pub fn take_activation() -> Option<Uuid> {
    let mut pending = match pending_activations().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    pending.pop_front()
}

/// The URI a banner click uses to reach the app (#2751).
///
/// Every platform that can hand a click back to a fresh process does it the
/// same way: it asks the shell to open this URI, and the shell starts OpenCrabs
/// with it as an argument. Keeping the shape here means the transport never has
/// to know how a given desktop delivers activations, and the Windows toast can
/// name it in its XML without the PowerShell script building a string.
pub const ACTIVATION_SCHEME: &str = "opencrabs://session/";

/// The session named by an activation argument, when it names one.
///
/// Pure, so it is exercised on every platform: a protocol launch hands the URI
/// over as a plain argv element and nothing else about the click survives, so
/// this is the whole of what a fresh process learns from it.
pub fn activation_from_arg(arg: &str) -> Option<Uuid> {
    let rest = arg.trim().strip_prefix(ACTIVATION_SCHEME)?;
    Uuid::parse_str(rest.trim()).ok()
}

/// Queue the session named by any activation argument in `args`.
///
/// A protocol launch starts a fresh process with the URI as argv, so the click
/// is queued before a window exists; the poll tick opens it once there is one to
/// open it in. Returns whether anything was queued.
pub fn post_activation_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut posted = false;
    for arg in args {
        if let Some(session) = activation_from_arg(arg.as_ref()) {
            post_activation(session);
            posted = true;
        }
    }
    posted
}

/// Record settled plan tasks in `seen` and announce each new one.
///
/// Returns the number of tasks this call recorded, which is exactly the count
/// the completion chime uses. The caller hands over the very set it keeps for
/// the chime, so a task settles for both or for neither, and a plan file that
/// is re-read twice cannot announce twice.
///
/// A skipped task counts as settled for the chime but is announced as a stop,
/// never a success: the banner must not claim work that was abandoned.
///
/// The count is the raw settle count even when a banner stands down, because
/// the chime and the rail flash still mark the moment.
pub fn announce_settled_tasks(seen: &mut HashSet<Uuid>, plans: &[Option<&PlanDocument>]) -> usize {
    let plan_banners = plan_completed_allowed();
    let mut recorded = 0;
    for entry in plans {
        let Some(plan) = *entry else { continue };
        let fresh = fresh_settled_tasks(seen, &[Some(plan)]);
        recorded += fresh.len();
        // The session comes off the plan itself (#2751): the plan file is
        // written by the turn that owns the conversation, so the banner points
        // at the row the user would have clicked. A plan written before the
        // hook stamped its session carries the nil id, which is not a session
        // and must never become a click target.
        let session = (plan.session_id != Uuid::nil()).then_some(plan.session_id);
        for (_, label, success) in bannerable_settles(plan, &fresh, plan_banners) {
            let _ = raise(NotificationEvent::TaskCompleted {
                label: (*label).to_string(),
                success: *success,
                elapsed_secs: 0,
                session,
            });
        }
    }
    recorded
}

/// Raise one desktop notification. Returns `true` when a transport was
/// dispatched, `false` when the feature is opted out or no transport worked.
///
/// Fire and forget: the helper process is reaped on a detached thread, so the
/// caller never waits on a notification.
pub fn raise(event: NotificationEvent) -> bool {
    let opt_out = std::env::var(ENV_OPT_OUT).ok();
    let wanted = config_allows(&event);
    if !should_raise(&event, opt_out.as_deref(), wanted) {
        tracing::debug!(
            "desktop notify: suppressed (config={wanted}, focus={:?}, key={:?})",
            surface_focused(),
            event.dedupe_key()
        );
        return false;
    }

    let title = event.title();
    let body = event.body();

    // Linux and Windows both have a click path, so both are handed the session
    // the banner names (#2751). macOS takes the banner alone: the helper that
    // draws it accepts a title and a body and nothing else, it has no
    // notification delegate to hear a click, and its source is not in this
    // repository, so there is nothing here to wire. The intake below is shared,
    // so the day the helper can report a click the macOS arm needs no more than
    // passing the URI through.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let dispatched = dispatch(&title, &body, event.session());
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let dispatched = dispatch(&title, &body);

    match dispatched {
        Ok(()) => true,
        Err(err) => {
            tracing::debug!("desktop notify: {err}");
            false
        }
    }
}

/// Whether notifications are enabled for this process.
///
/// Separated from the transports so the opt-out is testable on every platform.
pub(crate) fn enabled(opt_out: Option<&str>) -> bool {
    match opt_out {
        None => true,
        Some(raw) => {
            let value = raw.trim();
            if value.is_empty() {
                return true;
            }
            !(value == "1"
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("yes")
                || value.eq_ignore_ascii_case("on"))
        }
    }
}

/// Cut a string to `limit` characters on a character boundary.
///
/// Counted in `char`s rather than bytes: plan titles and error reasons carry
/// user text, and slicing a byte range through a multi-byte character would
/// panic.
pub(crate) fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Compress a whole response into the one summary line a banner can show.
///
/// A turn's final text is markdown, often several hundred lines. A toast has
/// room for a single line, so take the first non-empty one, drop the heading
/// or list marker that would otherwise lead it, and let [`truncate`] cut the
/// rest. Empty text (a turn that only called tools) yields an empty string and
/// the event's title carries the message on its own.
pub(crate) fn summary_line(text: &str) -> String {
    let Some(first) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    let stripped = first.trim_start_matches(['#', '>', '*', '-', ' ']);
    truncate(stripped, MAX_BODY_CHARS)
}

/// Escape the five XML metacharacters. Only the Windows toast builds XML, but
/// the function is pure and the test file exercises it on every platform.
#[cfg(any(test, target_os = "windows"))]
pub(crate) fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// The toast document handed to the Windows shell (#2751, #2867).
///
/// Built in Rust and passed to PowerShell through the environment rather than
/// concatenated inside the script. Two reasons: the XML becomes a pure value the
/// test file can check on any platform, and a title containing a quote can no
/// longer break out of a PowerShell string literal. A banner about a session
/// carries one action, and both the toast and the action name the activation
/// URI, so a click on the body or on the button reaches the app.
#[cfg(any(test, target_os = "windows"))]
pub(crate) fn toast_xml(title: &str, body: &str, session: Option<Uuid>) -> String {
    // `protocol` hands the URI to the shell, which starts the registered
    // handler. The alternative, a `foreground` activation, is delivered to the
    // process that showed the toast, and that process has already exited: the
    // click has to work with the window closed (#2867).
    let launch = session.map(|session| format!("{ACTIVATION_SCHEME}{session}"));
    let mut xml = String::from("<toast");
    if let Some(uri) = launch.as_deref() {
        xml.push_str(&format!(
            " activationType=\"protocol\" launch=\"{uri}\""
        ));
    }
    xml.push_str("><visual><binding template=\"ToastText02\"><text id=\"1\">");
    xml.push_str(&xml_escape(title));
    xml.push_str("</text><text id=\"2\">");
    xml.push_str(&xml_escape(body));
    xml.push_str("</text></binding></visual>");
    if let Some(uri) = launch.as_deref() {
        xml.push_str(&format!(
            "<actions><action content=\"Open session\" arguments=\"{uri}\" activationType=\"protocol\"/></actions>"
        ));
    }
    xml.push_str("</toast>");
    xml
}

/// Spawn the helper and reap it off the calling thread.
///
/// Arguments are handed over as argv (or as environment for the PowerShell
/// arm), never concatenated into a shell string, so a task label containing a
/// quote cannot turn into an injected command.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn spawn_detached(command: &mut Command) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = command.spawn().map_err(|err| err.to_string())?;

    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

/// Path of the helper bundle that gives the banner the OpenCrabs icon.
///
/// The bundle is built once into `~/Applications`. macOS authorizes a sender
/// that lives in an install location and refuses one in a hidden directory
/// outright, so the path is part of the contract, not a preference.
#[cfg(target_os = "macos")]
fn notifier_app() -> Option<std::path::PathBuf> {
    let app = dirs::home_dir()?
        .join("Applications")
        .join("OpenCrabs Notifier.app");
    app.is_dir().then_some(app)
}

#[cfg(target_os = "macos")]
fn dispatch(title: &str, body: &str) -> Result<(), String> {
    // Preferred: the bundled helper. It carries our icon, macOS attributes the
    // banner to it, and it exits on its own after delivery.
    if let Some(app) = notifier_app() {
        let mut command = Command::new("open");
        // `-g` keeps the helper out of the foreground, `-n` gives it a fresh
        // instance per notification. Title and body travel as argv, never as a
        // shell string, so a task label cannot inject anything.
        command
            .arg("-g")
            .arg("-n")
            .arg("-a")
            .arg(&app)
            .arg("--args")
            .arg(title)
            .arg(body);
        return spawn_detached(&mut command);
    }

    // Fallback: `display notification` takes the body from its own argument,
    // and the title from `with title`. Both are passed as run-handler argv
    // items so no quoting is involved.
    const SCRIPT: &str = "on run argv\n\
                          display notification (item 2 of argv) with title (item 1 of argv)\n\
                          end run";

    spawn_detached(Command::new("osascript").args(["-e", SCRIPT, title, body]))
}

/// The name a click reports back (#2751).
///
/// `notify-send` prints the action's *name* on stdout, not its text, so this is
/// the string compared against the answer. It is never shown to the user.
#[cfg(target_os = "linux")]
const ACTION_OPEN: &str = "open";

/// How long the banner stays clickable.
///
/// Past this the daemon withdraws it and the reader thread ends on its own, so
/// a banner nobody touches does not hold a thread for the life of the process.
/// A minute is long enough to come back to the desk and notice it.
#[cfg(target_os = "linux")]
const CLICK_WINDOW_MS: u32 = 60_000;

#[cfg(target_os = "linux")]
fn dispatch(title: &str, body: &str, session: Option<Uuid>) -> Result<(), String> {
    // No session, nowhere to go: raise the plain banner and let it be.
    let Some(session) = session else {
        return spawn_detached(Command::new("notify-send").args([
            "--app-name",
            APP_NAME,
            "--urgency",
            "normal",
            title,
            body,
        ]));
    };

    // A banner with somewhere to go carries one action. `--wait` is what makes
    // `notify-send` stay alive to hear the click: without it the process
    // returns as soon as the daemon accepts the banner, and the action line
    // would never arrive at all (#2751).
    // Built before the argv so every element is a `&str`: an array literal
    // mixing `&String` with `&str` does not unify, and the argv is handed over
    // as argv, never concatenated into a shell string.
    let action = format!("{ACTION_OPEN}=Open session");
    let window = CLICK_WINDOW_MS.to_string();
    let mut command = Command::new("notify-send");
    command
        .args([
            "--app-name",
            APP_NAME,
            "--urgency",
            "normal",
            "--action",
            action.as_str(),
            "--wait",
            "--expire-time",
            window.as_str(),
            title,
            body,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command.spawn().map_err(|err| err.to_string())?;
    let title = title.to_string();
    let body = body.to_string();

    // The wait happens on its own thread: the caller is the agent loop and must
    // never block on a notification. One line on stdout is the whole protocol,
    // the action name when the banner was clicked, nothing when it was
    // dismissed or expired.
    std::thread::spawn(move || {
        let mut line = String::new();
        if let Some(stdout) = child.stdout.take() {
            let mut reader = std::io::BufReader::new(stdout);
            let _ = std::io::BufRead::read_line(&mut reader, &mut line);
        }
        let status = child.wait();
        if line.trim() == ACTION_OPEN {
            post_activation(session);
            return;
        }
        // A daemon that does not advertise actions refuses the banner outright
        // and `notify-send` exits non-zero without showing anything. Losing the
        // notification to gain a click would be a bad trade, so it is raised
        // again without the action: no click, but the banner still arrives.
        let failed = status.map(|code| !code.success()).unwrap_or(true);
        if failed && line.trim().is_empty() {
            let _ = spawn_detached(Command::new("notify-send").args([
                "--app-name",
                APP_NAME,
                "--urgency",
                "normal",
                title.as_str(),
                body.as_str(),
            ]));
        }
    });

    Ok(())
}

#[cfg(target_os = "windows")]
fn dispatch(title: &str, body: &str, session: Option<Uuid>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // Windows attributes a toast to an AppUserModelID. Without one set for
    // this process the toast can be dropped with no error at all, so the id is
    // installed with `SetCurrentProcessExplicitAppUserModelID` before the
    // toast is built and the same id is passed to the notifier. It matches the
    // macOS bundle id so one name covers both platforms.

    /// `CREATE_NO_WINDOW`: the toast must not flash a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // WinRT toast. The document arrives through the environment already built
    // and escaped by `toast_xml`, so the script never concatenates user text
    // into its own string and no quoting can break it (#2751).
    const SCRIPT: &str = "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null\n\
                          $sig = '[DllImport(\"shell32.dll\", SetLastError = true)] public static extern int SetCurrentProcessExplicitAppUserModelID([MarshalAs(UnmanagedType.LPWStr)] string AppID);'\n\
                          $aumid = Add-Type -MemberDefinition $sig -Name 'Aumid' -Namespace 'OpenCrabsNotify' -PassThru\n\
                          $aumid::SetCurrentProcessExplicitAppUserModelID('com.opencrabs.app') > $null\n\
                          $xml = New-Object Windows.Data.Xml.Dom.XmlDocument\n\
                          $xml.LoadXml($env:OC_NOTIFY_XML)\n\
                          $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)\n\
                          [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('com.opencrabs.app').Show($toast)";

    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("OC_NOTIFY_XML", toast_xml(title, body, session))
        .creation_flags(CREATE_NO_WINDOW);

    spawn_detached(&mut command)
}

/// Register the activation URI for the current user (#2751, #2867).
///
/// A toast click on Windows is a protocol activation: the shell opens
/// `opencrabs://session/<id>` and the registered command starts the app with
/// that URI as an argument. The scheme therefore has to exist in the user's hive
/// before a click can reach anything. It is written once at window start, never
/// from the notification path: raising a banner must not mutate the registry.
///
/// `reg.exe` rather than a bindings crate because the shell already ships it and
/// every write here is idempotent, so a second run costs one hidden process and
/// changes nothing. Only `HKCU` is touched, so no elevation is involved and
/// removing the key undoes the whole thing.
#[cfg(target_os = "windows")]
pub fn ensure_activation_handler() {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Ok(exe) = std::env::current_exe() else {
        tracing::debug!("activation handler: no current executable");
        return;
    };
    // `%1` is the URI the shell hands over; it is quoted so a path containing a
    // space still resolves to one argument.
    let open = format!("\"{}\" \"%1\"", exe.display());

    // `URL Protocol` is the empty value that marks the key as a scheme rather
    // than a file type: without it the shell treats `opencrabs://` as a path.
    // `reg add` takes the default value through `/ve` with no name argument, so
    // the argv is built per write instead of forcing one shape on all three.
    let writes: [(&str, Option<&str>, &str); 3] = [
        (r"HKCU\Software\Classes\opencrabs", None, "URL:OpenCrabs"),
        (
            r"HKCU\Software\Classes\opencrabs",
            Some("URL Protocol"),
            "",
        ),
        (
            r"HKCU\Software\Classes\opencrabs\shell\open\command",
            None,
            open.as_str(),
        ),
    ];

    for (key, name, value) in writes {
        let mut argv: Vec<&str> = vec!["add", key];
        match name {
            Some(name) => argv.extend(["/v", name]),
            None => argv.push("/ve"),
        }
        argv.extend(["/t", "REG_SZ", "/d", value, "/f"]);

        let mut command = Command::new("reg");
        command
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        match command.spawn() {
            Ok(mut child) => {
                // Reaped on this thread: the window start must not leave a
                // zombie per write, and the write itself is instantaneous.
                let _ = child.wait();
            }
            Err(err) => {
                tracing::debug!("activation handler: {err}");
                return;
            }
        }
    }
}

/// Every other target is a deliberate no-op. The module still compiles so a
/// caller never needs its own `cfg` gate.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn dispatch(title: &str, body: &str) -> Result<(), String> {
    let _ = (title, body);
    Err("desktop notifications unsupported on this platform".to_string())
}
