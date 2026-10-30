//! Outcome of the post-swap restart-scheduling step, and the user-facing
//! success line derived from it. Never says "Restarting" when no restart
//! was actually scheduled (the original #136 symptom).

use super::systemd::SYSTEMD_UNIT_PATTERN;

/// Outcome of the post-swap restart-scheduling step. Used to tailor
/// the user-facing success string so we never say "Restarting…" when
/// no restart was actually scheduled (the original #136 symptom, and we
/// must not reintroduce it for a new reason).
#[derive(Debug)]
pub(crate) enum RestartStatus {
    /// Not running on a systemd host (no `/run/systemd/system`). The
    /// caller's `RestartReady` progress event is the only restart
    /// signal, e.g. cargo-install / TUI launch paths handle that.
    NotSystemd,
    /// `systemctl list-units` matched zero units. systemd is present
    /// but nothing in the unit registry corresponds to opencrabs;
    /// scheduling a restart would be a no-op so we don't.
    NoUnitsMatched,
    /// systemctl could not be queried, so no unit name could be resolved and
    /// no restart was scheduled. Deliberately distinct from
    /// [`Self::NoUnitsMatched`]: there we know there is nothing to restart,
    /// here we could not ask. A glob is not a safe stand-in for the missing
    /// name, because it also matches the transient unit the restart would run
    /// inside (#2081).
    Unresolved,
    /// systemd-run was spawned successfully, and the restart fires in 3s.
    Scheduled,
    /// systemd-run failed to spawn (binary missing on this host,
    /// permission denied, etc.). Carries the error string so the
    /// user-visible message can quote it for forensics.
    SpawnFailed(String),
}

impl RestartStatus {
    /// The restart half of the success line, with no version claim in it.
    ///
    /// Split out of [`Self::user_message`] so a caller that cannot state the new
    /// version with certainty still reuses the restart guidance rather than
    /// writing a second copy of it. Homebrew is that caller: brew owns the
    /// version, and the only honest statement is whatever brew reports after the
    /// upgrade (#1779). Empty for [`RestartStatus::Scheduled`], where the timer
    /// handles the restart and the version sentence is the whole message.
    ///
    /// The manual-restart advice names the unit by placeholder rather than
    /// handing the user the discovery glob to pass to `systemctl restart`: the
    /// glob also matches the transient evolve unit, so restarting with it is
    /// the very defect this module's messages must not recommend (#2081). The
    /// glob is still shown where it is safe, as the argument to `list-units`.
    pub(super) fn restart_clause(&self) -> String {
        match self {
            RestartStatus::Scheduled => String::new(),
            RestartStatus::NotSystemd => "Binary updated on disk; restart \
                 the process / relaunch to load the new version."
                .to_string(),
            RestartStatus::NoUnitsMatched => format!(
                "Binary updated on disk, but no \
                 systemd units matched `{SYSTEMD_UNIT_PATTERN}` at system or user level \
                 so your daemon (if any) was not restarted. Restart it manually by name \
                 with `systemctl --user restart <your-unit>` (if installed as a user \
                 service) or `systemctl restart <your-unit>` (if a system service), or \
                 relaunch if running standalone. List the candidates with \
                 `systemctl --user list-units '{SYSTEMD_UNIT_PATTERN}'`."
            ),
            RestartStatus::Unresolved => format!(
                "Binary updated on disk, but the systemd unit list could not be read \
                 (`systemctl list-units '{SYSTEMD_UNIT_PATTERN}'` failed), so no restart \
                 was scheduled. Restart your daemon manually by name with \
                 `systemctl --user restart <your-unit>` (if a user service) or \
                 `systemctl restart <your-unit>` (if a system service)."
            ),
            RestartStatus::SpawnFailed(err) => format!(
                "Binary updated on disk, but \
                 scheduling the systemd restart failed ({err}). Restart your daemon \
                 manually by name with `systemctl --user restart <your-unit>` \
                 (if a user service) or `systemctl restart <your-unit>` \
                 (if a system service)."
            ),
        }
    }

    pub(super) fn user_message(&self, current: &str, latest: &str) -> String {
        let clause = self.restart_clause();
        if clause.is_empty() {
            format!("Evolved from v{current} to v{latest}.")
        } else {
            format!("Evolved from v{current} to v{latest}. {clause}")
        }
    }
}
