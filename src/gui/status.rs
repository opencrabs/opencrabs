//! The footer's identity line and the key reference's rows.
//!
//! Both are plain data: a string built from state the window already holds,
//! and the list of chords the window actually answers. Neither needs a
//! window, which is why a test can read them without one.
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions and their comments are unchanged, only the module boundary is
//! new.

/// The footer's identity line: who is answering, from where, and on what
/// branch. Every field is read live; a field with no value prints a dash
/// instead of a guess.
///
/// No streaming rate here (#3143). A number that moves several times a second
/// while an answer is written is a per-turn measurement, not an identity: it
/// says nothing about who is answering or from where, so it spent the widest
/// field on the row on a number that is stale the moment the turn ends.
pub(crate) fn status_identity(provider: &str, model: &str, cwd: &str, branch: &str) -> String {
    format!("{provider} · {model} · {cwd} · {branch}")
}

/// A path with the home directory collapsed to `~`, the way a shell prompt
/// prints it. The home prefix is compared as a whole component, so
/// `/Users/al` never matches `/Users/adolfousierstudio`.
pub(crate) fn tilde_path(p: &std::path::Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return p.display().to_string();
    };
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// The checkout's branch name, or `None` when the directory is not a repo.
/// This runs a subprocess, so callers keep the answer beside the directory it
/// was read from and ask again only when that directory changes: it is a
/// footer label, never a per-frame read.
pub(crate) fn git_branch(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// The key reference, painted over everything else (#2320).
///
/// Rows come from `ui_core::help_keys`, the table the terminal's `/help`
/// prints, and only the rows this window actually answers are listed: `gui`
/// is `None` for a chord whose port has not landed yet, and a help screen
/// that advertises a dead chord is worse than one that stays quiet.
/// The rows the overlay paints, in display order: every row whose `gui`
/// spelling exists, section by section (#2320).
///
/// A function rather than a loop inside the renderer, so a test can compare
/// the painted list with the shared table without building a window. The
/// window answers a chord or it does not, and a row the window ignores is a
/// row it must not advertise.
pub(crate) fn help_rows() -> Vec<(
    opencrabs::ui_core::help_keys::HelpSection,
    &'static opencrabs::ui_core::help_keys::HelpRow,
)> {
    use opencrabs::ui_core::help_keys::{self, HelpSection};

    let mut out = Vec::new();
    for section in HelpSection::ALL {
        out.extend(
            help_keys::section_rows(section)
                .filter(|row| row.gui.is_some())
                .map(|row| (section, row)),
        );
    }
    out
}
