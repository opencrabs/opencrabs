//! Work a CLI turn leaves running after it ends (#3219).
//!
//! Under a CLI provider the agent runs its own shell, so a command it detaches
//! (`nohup`, `setsid`, `systemd-run --user`) never passes through the
//! background task manager: no row while it runs, no message when it ends.
//! This module holds the provider-neutral half of the fix, the half the
//! provider and the agent service both reach:
//!
//! - [`SESSION_ENV`]: the CLI process is spawned with this variable set to
//!   the session id. Every descendant inherits it, including one that detaches
//!   with `nohup` or `setsid`, which is what lets a survivor be traced back to
//!   the session that started it.
//! - A pending list of `systemd-run` units. A unit does not inherit the
//!   environment, so it is found instead by the line `systemd-run` prints
//!   ("Running as unit: X"), read from the tool result the CLI streams.
//! - The `/proc` readers, split into pure parsers (tested without a process)
//!   and a Linux scan.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use uuid::Uuid;

/// Environment variable carrying the session id into the CLI's process tree.
pub const SESSION_ENV: &str = "OPENCRABS_SESSION";

/// The two prefixes `systemd-run` prints when it hands a command to systemd.
const UNIT_PREFIXES: [&str; 2] = ["Running as unit: ", "Running scope as unit: "];

/// Unit names a tool result announced, grouped by session, waiting to be
/// adopted when the turn's CLI process ends.
static PENDING_UNITS: Mutex<Vec<(Uuid, String)>> = Mutex::new(Vec::new());

/// Identities already being watched (`pid:<pid>:<start>` or `unit:<name>`), so
/// a survivor found again by a later turn's scan gets no second row.
static WATCHED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Every unit name `systemd-run` announced in `text`.
///
/// `text` may be a raw NDJSON line, where the newline after the name arrives
/// escaped (`\n`), so a name ends at whitespace, a quote or a backslash. Only
/// names with a unit suffix count: the prefix inside ordinary prose ("Running
/// as unit: tests") must not be watched as a unit.
pub fn units_announced(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in UNIT_PREFIXES {
        let mut rest = text;
        while let Some((_, after)) = rest.split_once(prefix) {
            rest = after;
            let name: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\\')
                .collect();
            if valid_unit_name(&name) && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Whether `name` is a unit name safe to hand to `systemctl` as an argument:
/// a unit suffix, only the characters systemd allows in a name, and no
/// leading `-` that would read as an option.
pub fn valid_unit_name(name: &str) -> bool {
    let has_suffix = name.ends_with(".service") || name.ends_with(".scope");
    let allowed = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '@' | '-'));
    has_suffix && allowed && !name.starts_with('-') && name.len() <= 256
}

/// Note the units a CLI line announced for `session_id`.
pub fn note_units(session_id: Uuid, line: &str) {
    let units = units_announced(line);
    if units.is_empty() {
        return;
    }
    if let Ok(mut pending) = PENDING_UNITS.lock() {
        for unit in units {
            if !pending.iter().any(|(s, u)| *s == session_id && *u == unit) {
                pending.push((session_id, unit));
            }
        }
    }
}

/// Take every unit noted for `session_id`, leaving other sessions' units.
pub fn take_units(session_id: Uuid) -> Vec<String> {
    let Ok(mut pending) = PENDING_UNITS.lock() else {
        return Vec::new();
    };
    let mut taken = Vec::new();
    pending.retain(|(s, u)| {
        if *s == session_id {
            taken.push(u.clone());
            false
        } else {
            true
        }
    });
    taken
}

/// Claim `identity` for a watcher. `false` when one already holds it.
pub fn claim(identity: &str) -> bool {
    let Ok(mut watched) = WATCHED.lock() else {
        return false;
    };
    watched
        .get_or_insert_with(HashSet::new)
        .insert(identity.to_string())
}

/// Release `identity` once its watcher has delivered the completion.
pub fn release(identity: &str) {
    if let Ok(mut watched) = WATCHED.lock()
        && let Some(set) = watched.as_mut()
    {
        set.remove(identity);
    }
}

/// A process, pinned by its start time so a reused pid is never mistaken for
/// the one that was adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcId {
    pub pid: i32,
    pub start: u64,
}

impl ProcId {
    /// The key a watcher claims for this process.
    pub fn identity(&self) -> String {
        format!("pid:{}:{}", self.pid, self.start)
    }
}

/// `(ppid, start time)` from the text of `/proc/<pid>/stat`.
///
/// The command name sits in parentheses and may itself hold spaces or
/// parentheses, so fields are counted from the LAST `)`. After it come the
/// state (field 3), the ppid (field 4) and, at field 22, the start time in
/// clock ticks since boot.
pub fn parse_stat(stat: &str) -> Option<(i32, u64)> {
    let (_, after) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let ppid = fields.get(1)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    Some((ppid, start))
}

/// Whether a NUL-separated environment block holds `key=value` exactly.
pub fn env_holds(environ: &[u8], key: &str, value: &str) -> bool {
    let wanted = format!("{key}={value}");
    environ
        .split(|b| *b == 0)
        .any(|entry| entry == wanted.as_bytes())
}

/// The roots among tagged processes: those whose parent is not tagged too.
///
/// A detached `bash build.sh` and the `cargo` and `rustc` under it are all
/// tagged; one row for the script says what is running, a row per child would
/// bury it.
pub fn roots(tagged: &[(i32, i32)]) -> Vec<i32> {
    let pids: HashSet<i32> = tagged.iter().map(|(pid, _)| *pid).collect();
    tagged
        .iter()
        .filter(|(_, ppid)| !pids.contains(ppid))
        .map(|(pid, _)| *pid)
        .collect()
}

/// The file a process writes its stdout to, when that is a regular file.
/// A terminal, a pipe or `/dev/null` has no log a completion could quote.
pub fn stdout_file(pid: i32) -> Option<PathBuf> {
    let target = std::fs::read_link(format!("/proc/{pid}/fd/1")).ok()?;
    let meta = std::fs::metadata(&target).ok()?;
    meta.is_file().then_some(target)
}

/// Read a live process's identity. `None` once it has exited.
pub fn proc_id(pid: i32) -> Option<ProcId> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, start) = parse_stat(&stat)?;
    Some(ProcId { pid, start })
}

/// Whether the process `id` names is still the one running under its pid.
pub fn alive(id: ProcId) -> bool {
    proc_id(id.pid).is_some_and(|now| now.start == id.start)
}

/// A short name for the process `pid` runs, safe to show and to log.
pub fn command_label(pid: i32) -> String {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    label_from_argv(&argv)
}

/// The program's file name plus, when it is not an option, the file name of
/// its first argument (`bash build.sh`, `cargo build`, `sleep 4`).
///
/// Never the whole command line: an argument can carry a token, a password or
/// a URL with credentials, and the label is drawn on a card and written to
/// the log.
pub fn label_from_argv(argv: &[String]) -> String {
    let base = |a: &str| {
        let name = a.rsplit('/').next().unwrap_or(a);
        name.chars().take(48).collect::<String>()
    };
    let Some(program) = argv.first() else {
        return String::new();
    };
    match argv.get(1) {
        Some(arg) if !arg.starts_with('-') && !arg.contains('=') && !arg.contains("://") => {
            format!("{} {}", base(program), base(arg))
        }
        _ => base(program),
    }
}

/// Whether `pid` descends from `ancestor` (or is it).
fn descends_from(pid: i32, ancestor: i32) -> bool {
    let mut current = pid;
    // A bounded walk: a pid chain deeper than this is not a real tree.
    for _ in 0..128 {
        if current == ancestor {
            return true;
        }
        if current <= 1 {
            return false;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{current}/stat")) else {
            return false;
        };
        let Some((ppid, _)) = parse_stat(&stat) else {
            return false;
        };
        current = ppid;
    }
    false
}

/// Processes tagged with `session_id` that no longer hang off this process.
///
/// A tagged process still under this process is a CLI that is running, or a
/// tool inside it, and is that turn's own business. One that was reparented
/// away outlived the turn that started it: that is the survivor nobody is
/// watching. Only processes of this user are readable, which is also the only
/// set worth showing. Returns roots only (see [`roots`]).
pub fn survivors(session_id: Uuid) -> Vec<ProcId> {
    let me = std::process::id() as i32;
    let value = session_id.to_string();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut tagged = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if !env_holds(&environ, SESSION_ENV, &value) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((ppid, _)) = parse_stat(&stat) else {
            continue;
        };
        tagged.push((pid, ppid));
    }
    roots(&tagged)
        .into_iter()
        .filter(|pid| !descends_from(*pid, me))
        .filter_map(proc_id)
        .collect()
}

/// What `systemctl --user show` says about a unit, as far as a watcher cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitState {
    pub load: String,
    pub active: String,
    pub result: String,
    pub status: Option<i32>,
}

impl UnitState {
    /// Parse `Key=Value` lines from `systemctl show`.
    pub fn parse(text: &str) -> Self {
        let mut state = UnitState::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "LoadState" => state.load = value.to_string(),
                "ActiveState" => state.active = value.to_string(),
                "Result" => state.result = value.to_string(),
                "ExecMainStatus" => state.status = value.parse().ok(),
                _ => {}
            }
        }
        state
    }

    /// Whether the unit has stopped running.
    ///
    /// A transient unit that ended cleanly is garbage-collected, so it reads
    /// back as `not-found`; a failed one is kept loaded in `failed`.
    pub fn ended(&self) -> bool {
        self.load == "not-found" || matches!(self.active.as_str(), "inactive" | "failed")
    }

    /// One line for the completion message.
    pub fn outcome(&self) -> String {
        if self.load == "not-found" {
            // systemd keeps a failed unit loaded, so a collected one did not fail.
            return "finished; systemd already collected the unit, which it \
                    only does for a unit that did not fail"
                .to_string();
        }
        match (self.result.as_str(), self.status) {
            ("success", Some(code)) => format!("finished, exit {code}"),
            ("success", None) => "finished".to_string(),
            (result, Some(code)) => format!("failed ({result}), exit {code}"),
            (result, None) => format!("failed ({result})"),
        }
    }
}
