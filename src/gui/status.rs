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

/// The checkout's branch name in `dir` as the crab last answered it, or
/// `None` while the answer is on its way, when the directory is not a repo,
/// or when the crab refused. Never waits and never runs `git` here: the
/// frame reads it, and [`ask_git_branch`] is what asks.
pub(crate) fn git_branch(dir: &std::path::Path) -> Option<String> {
    let answered = BRANCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match answered.as_ref() {
        Some(place) if place.dir == dir => place.branch.clone(),
        _ => None,
    }
}

/// Ask the crab `backend` names which branch is checked out in `dir`, off
/// the window's thread. The answer lands where [`git_branch`] reads it, and
/// `sink` gets [`TurnEvent::Refresh`](crate::agent_bridge::TurnEvent) so the
/// next frame paints it. `git` is a subprocess on the crab, so the footer
/// asks when the directory changes, never per frame.
///
/// Only the latest question is kept: an answer to an older one (the reader
/// moved on, or the window swapped crabs) is dropped when it lands.
pub(crate) fn ask_git_branch(
    backend: &crate::backend_glue::Backend,
    dir: std::path::PathBuf,
    sink: crate::agent_bridge::EventSink,
) {
    let asked = {
        let mut slot = BRANCH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let asked = slot.as_ref().map_or(0, |place| place.asked.wrapping_add(1));
        *slot = Some(BranchAnswer {
            asked,
            dir: dir.clone(),
            branch: None,
        });
        asked
    };
    let spawned = backend.spawn(
        opencrabs::backend::status::GitBranch { dir },
        move |reply| {
            {
                let mut slot = BRANCH
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match slot.as_mut() {
                    Some(place) if place.asked == asked => {
                        place.branch = reply.ok().flatten();
                    }
                    _ => return,
                }
            }
            let _ = sink.send(crate::agent_bridge::TurnEvent::Refresh);
        },
    );
    if let Err(e) = spawned {
        crate::logging::error(&format!("footer branch: {e}"));
    }
}

/// The footer's last branch question and, once it lands, its answer.
struct BranchAnswer {
    /// Moves on every question, so a late answer to an older one is dropped.
    asked: u64,
    dir: std::path::PathBuf,
    branch: Option<String>,
}

/// One window per process, one footer: the latest question it asked.
static BRANCH: std::sync::Mutex<Option<BranchAnswer>> = std::sync::Mutex::new(None);

/// Run `op` on the window's crab and wait for the answer, for a door whose
/// callers are still synchronous. The op runs on its `spawn_worker` lane
/// through [`Backend::call`](crate::backend_glue::Backend::call); this thread
/// only parks on the oneshot, which needs no reactor.
///
/// Before the crab has booted there is no primary `Ctx`, and the footer, the
/// finder and the /cd picker can already ask. They get a ledgerless local
/// crab: the ops that go through here read the disk or run `git` and never
/// open the ledger, so they answer exactly what they answered before.
pub(crate) fn ask_crab<O: opencrabs::backend::Op>(
    op: O,
) -> Result<O::Reply, opencrabs::backend::BackendError> {
    let ctx = opencrabs::backend::primary().unwrap_or_else(|| {
        std::sync::Arc::new(opencrabs::backend::Ctx::new(std::path::PathBuf::new()))
    });
    futures::executor::block_on(crate::backend_glue::Backend::Local(ctx).call(op))
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
