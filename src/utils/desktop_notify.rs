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

use std::collections::HashSet;
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
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// A whole plan reached completion. `tasks` is how many had settled.
    PlanCompleted { title: String, tasks: usize },
    /// One plan task settled. `success` is false when the task was skipped or
    /// failed, so the banner never claims work that was abandoned.
    TaskCompleted {
        label: String,
        success: bool,
        elapsed_secs: u64,
    },
    /// The turn ended with an answer.
    TurnCompleted { summary: String },
    /// The turn ended in an error the user might have walked away from.
    TurnFailed { reason: String },
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

/// Record settled plan tasks in `seen` and announce each new one.
///
/// Returns the number of tasks this call recorded, which is exactly the count
/// the completion chime uses. The caller hands over the very set it keeps for
/// the chime, so a task settles for both or for neither, and a plan file that
/// is re-read twice cannot announce twice.
///
/// A skipped task counts as settled for the chime but is announced as a stop,
/// never a success: the banner must not claim work that was abandoned.
pub fn announce_settled_tasks(seen: &mut HashSet<Uuid>, plans: &[Option<&PlanDocument>]) -> usize {
    let fresh = fresh_settled_tasks(seen, plans);
    for (_, label, success) in &fresh {
        let _ = raise(NotificationEvent::TaskCompleted {
            label: (*label).to_string(),
            success: *success,
            elapsed_secs: 0,
        });
    }
    fresh.len()
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

    match dispatch(&title, &body) {
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

#[cfg(target_os = "linux")]
fn dispatch(title: &str, body: &str) -> Result<(), String> {
    spawn_detached(Command::new("notify-send").args([
        "--app-name",
        APP_NAME,
        "--urgency",
        "normal",
        title,
        body,
    ]))
}

#[cfg(target_os = "windows")]
fn dispatch(title: &str, body: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // Windows attributes a toast to an AppUserModelID. Without one set for
    // this process the toast can be dropped with no error at all, so the id is
    // installed with `SetCurrentProcessExplicitAppUserModelID` before the
    // toast is built and the same id is passed to the notifier. It matches the
    // macOS bundle id so one name covers both platforms.

    /// `CREATE_NO_WINDOW`: the toast must not flash a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // WinRT toast. Values arrive through the environment and are XML-escaped
    // above, so no quoting or markup can break the script.
    const SCRIPT: &str = "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null\n\
                          $sig = '[DllImport(\"shell32.dll\", SetLastError = true)] public static extern int SetCurrentProcessExplicitAppUserModelID([MarshalAs(UnmanagedType.LPWStr)] string AppID);'\n\
                          $aumid = Add-Type -MemberDefinition $sig -Name 'Aumid' -Namespace 'OpenCrabsNotify' -PassThru\n\
                          $aumid::SetCurrentProcessExplicitAppUserModelID('com.opencrabs.app') > $null\n\
                          $xml = New-Object Windows.Data.Xml.Dom.XmlDocument\n\
                          $xml.LoadXml('<toast><visual><binding template=\"ToastText02\"><text id=\"1\">' + $env:OC_NOTIFY_TITLE + '</text><text id=\"2\">' + $env:OC_NOTIFY_BODY + '</text></binding></visual></toast>')\n\
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
        .env("OC_NOTIFY_TITLE", xml_escape(title))
        .env("OC_NOTIFY_BODY", xml_escape(body))
        .creation_flags(CREATE_NO_WINDOW);

    spawn_detached(&mut command)
}

/// Every other target is a deliberate no-op. The module still compiles so a
/// caller never needs its own `cfg` gate.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn dispatch(title: &str, body: &str) -> Result<(), String> {
    let _ = (title, body);
    Err("desktop notifications unsupported on this platform".to_string())
}
