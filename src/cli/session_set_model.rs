//! Pure helpers for `opencrabs session set-model` (#465): pair parsing and
//! target resolution, kept side-effect free so the selection rules are unit
//! tested without a database.

use crate::db::models::Session;
use uuid::Uuid;

/// Shared pair parser (#467 uses it on channels and the TUI too).
pub(crate) use crate::utils::provider_pair::parse_pair;

/// The argument of a direct switch typed as `/models <provider/model>` (#467,
/// #2064). `None` for a bare `/models`, which opens the picker. The argument
/// keeps its `all` scope suffix; the caller strips it.
pub(crate) fn direct_switch_arg(input: &str) -> Option<&str> {
    let mut parts = input.trim().splitn(2, char::is_whitespace);
    if parts.next()? != "/models" {
        return None;
    }
    let arg = parts.next()?.trim();
    (!arg.is_empty()).then_some(arg)
}

/// Resolve which sessions a set-model invocation targets.
///
/// Exactly one selector must be used: an id prefix, a `--name` title
/// substring, or `--all`. Ambiguous prefixes and titles are an error that
/// lists the candidates, never a guess. `--all` targets every non-archived
/// session.
pub(crate) fn resolve_targets(
    sessions: &[Session],
    id_prefix: Option<&str>,
    name_sub: Option<&str>,
    all: bool,
) -> Result<Vec<Uuid>, String> {
    let selectors =
        usize::from(id_prefix.is_some()) + usize::from(name_sub.is_some()) + usize::from(all);
    if selectors != 1 {
        return Err(
            "pick exactly ONE target: a session id, --name <title match>, or --all".to_string(),
        );
    }

    if all {
        let ids: Vec<Uuid> = sessions
            .iter()
            .filter(|s| s.archived_at.is_none())
            .map(|s| s.id)
            .collect();
        if ids.is_empty() {
            return Err("no non-archived sessions found".to_string());
        }
        return Ok(ids);
    }

    if let Some(prefix) = id_prefix {
        return super::session_resolve::resolve_one_by_prefix(sessions, prefix).map(|id| vec![id]);
    }

    let sub = name_sub.expect("selector count checked").to_lowercase();
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.title
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(&sub))
        })
        .collect();
    match matches.len() {
        0 => Err(format!("no session title contains '{sub}'")),
        1 => Ok(vec![matches[0].id]),
        _ => Err(format!(
            "'{sub}' matches several sessions — candidates:\n{}",
            super::session_resolve::candidates(&matches)
        )),
    }
}
