//! A portable snapshot of the live process table (#3219).
//!
//! A Bash call can leave work running: `nohup cargo build &`, a server started
//! with `&`, a CLI sub-agent. The shell exits, the tool returns, the turn ends,
//! and OpenCrabs never learns the process existed. Nothing draws a row and
//! nothing reports the result, so the user ends up asking "is it done?", which
//! is the one thing background tasks exist to prevent.
//!
//! This module is the read-only half of the fix: what is running, and how to
//! tell one process from another across time. The manager polls it
//! ([`crate::brain::agent::service::background_tasks`]); nothing here mutates
//! anything.
//!
//! # Identity is pid AND start time
//!
//! A pid is not a name. The kernel reuses pids, so a build that exited a minute
//! ago and an unrelated process that inherited its number are the same pid and
//! different processes. Recording only the number produces a row that never
//! closes, or worse, a completion that names the wrong work. `ps` prints the
//! kernel's start time for a pid, so [`ProcId`] carries both and
//! [`same_process`] compares both.
//!
//! # Why a process group
//!
//! The bash tool spawns every command through `setsid()`
//! (`detach_session_pre_exec`), so the shell is its own session and process
//! group leader: its pid IS the group id, and everything it starts inherits it.
//! That link survives the shell's exit, which is exactly what a snapshot taken
//! AFTER the call returns needs. A command that deliberately escapes with its
//! own `setsid` opens a new group and is out of reach of this scan; so is a
//! `systemd-run --user` unit, whose process belongs to systemd rather than to
//! the session. Both are named in #3219 as follow-ups, and neither is silently
//! claimed here.

/// Identity of one process: the pid plus the moment the kernel started it.
///
/// `start_time` is `ps`'s `lstart` text ("Fri Nov 27 23:05:39 2026"). It is a
/// human string rather than an epoch because that is what every supported `ps`
/// prints; the only thing done with it is equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcId {
    pub pid: u32,
    /// The kernel's start time for this pid, as `ps` prints it.
    pub start_time: String,
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub id: ProcId,
    /// Process group. A shell spawned with `setsid` is its own group leader,
    /// so its descendants carry its pid here even after the shell is gone.
    pub pgid: u32,
    /// Full command line, for a row's label.
    pub command: String,
}

/// Whether `seen` is the process `expected` refers to.
///
/// The pid alone is not enough: see the module docs. A row adopted for pid 900
/// must not be closed by the death of a different process that happens to be
/// called 900 now.
pub fn same_process(expected: &ProcId, seen: &ProcEntry) -> bool {
    seen.id.pid == expected.pid && seen.id.start_time == expected.start_time
}

/// Whether `id` is still the process it claims to be, per a fresh snapshot.
pub fn is_alive(entries: &[ProcEntry], id: &ProcId) -> bool {
    entries.iter().any(|e| same_process(id, e))
}

/// The live members of process group `pgid`, excluding the group leader.
///
/// The leader is the shell the bash tool spawned, and by the time a call
/// returns it has been waited on; a member equal to it would be a pid-reuse
/// artefact, not work worth tracking. Our own process is excluded too: it is
/// never in a child's group, but a malformed `ps` line must not be able to put
/// a row around the agent itself.
pub fn group_members(entries: &[ProcEntry], pgid: u32) -> Vec<ProcEntry> {
    let own = std::process::id();
    entries
        .iter()
        .filter(|e| e.pgid == pgid && e.id.pid != pgid && e.id.pid != own)
        .cloned()
        .collect()
}

/// Every process the platform will report, or an empty list when it cannot be
/// asked.
///
/// Empty is an ordinary answer, never an error: a machine without `ps` simply
/// gets no survivor tracking, which is the behaviour that shipped before
/// #3219. Nothing here is allowed to fail a Bash call.
pub fn list_processes() -> Vec<ProcEntry> {
    #[cfg(unix)]
    {
        // One call, three columns plus the command: `ps` is cheap but a call
        // per tracked pid would not be. `lstart` is five whitespace-separated
        // tokens (weekday month day time year) on both BSD and procps, which
        // is what makes the rest of the line unambiguously the command.
        let out = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,pgid=,lstart=,command="])
            .output();
        match out {
            Ok(out) if out.status.success() => {
                parse_ps(&String::from_utf8_lossy(&out.stdout))
            }
            Ok(out) => {
                tracing::debug!(
                    target: "background_task",
                    "ps exited {} while listing processes; no survivor tracking this call",
                    out.status
                );
                Vec::new()
            }
            Err(e) => {
                tracing::debug!(
                    target: "background_task",
                    "could not run ps to list processes: {e}"
                );
                Vec::new()
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Windows has no `ps`, and `tasklist` reports neither a start time nor
        // a process group, so the two things this module is built on are both
        // missing. Returning nothing means no rows and no completions, which
        // is honest; inventing an identity from the pid alone would not be.
        Vec::new()
    }
}

/// Parse `ps -A -o pid=,pgid=,lstart=,command=` output.
///
/// Split out so the shape of a real `ps` line is pinned by a test instead of
/// only by a running machine: a line that fails to parse is dropped, and a
/// parser that quietly drops everything looks identical to a machine with
/// nothing running.
pub(crate) fn parse_ps(text: &str) -> Vec<ProcEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|f| f.parse::<u32>().ok()) else {
            continue;
        };
        let Some(pgid) = fields.next().and_then(|f| f.parse::<u32>().ok()) else {
            continue;
        };
        // lstart is exactly five tokens; a line with fewer is truncated and
        // carries no usable identity, so it is dropped rather than guessed at.
        let start: Vec<&str> = fields.by_ref().take(5).collect();
        if start.len() < 5 {
            continue;
        }
        let command = fields.collect::<Vec<_>>().join(" ");
        entries.push(ProcEntry {
            id: ProcId {
                pid,
                start_time: start.join(" "),
            },
            pgid,
            command,
        });
    }
    entries
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_real_snapshot_contains_this_process() {
        // The parser is only useful if it agrees with the machine it runs on.
        // Our own pid is the one row every supported unix is guaranteed to
        // report, so it doubles as a witness that `ps` was actually readable.
        let entries = list_processes();
        assert!(
            entries.iter().any(|e| e.id.pid == std::process::id()),
            "ps listed {} processes, none of them this one: {:?}",
            entries.len(),
            entries.first()
        );
    }
}
