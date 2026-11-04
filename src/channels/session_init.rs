//! Channel session initialization
//!
//! When a messaging channel (Discord, Telegram, Slack, WhatsApp, Trello) needs
//! a new session — a fresh DM, a new group thread, a per-channel session — it
//! used to call `session_svc.create_session(title)` directly, which stamped
//! `provider_name = NULL` / `model = NULL`. Downstream, `sync_provider_for_session`
//! saw `None` and fell through to `config.active_provider_and_model()` — the
//! fixed priority list in config.toml — which does NOT reflect whatever the TUI
//! user had actively selected (TUI persists provider changes per-session, not
//! into config.toml).
//!
//! Net effect: a user who picked OpenRouter in the TUI would open Discord and
//! find the bot pinned to whatever provider happened to be first-enabled in
//! config.toml, not the one they were actually using.
//!
//! `create_channel_session` closes that gap: it creates the session and stamps
//! it with the most recent existing session's provider/model (falling back to
//! `None` if no such session exists, which then falls through to the config
//! priority list — the previous behavior). Call this instead of
//! `session_svc.create_session(...)` in every channel handler.
//!
//! Working directory is scoped differently on purpose (#263). Provider/model
//! come from the global most-recent session so a channel adopts whatever
//! provider the user last actively selected. The working directory, however,
//! comes ONLY from `inherit_wd_from`: the session that received the `/new`
//! command (or the idle-expired session being replaced) in this same chat.
//! Sourcing the cwd from the global most-recent session let a TUI session in
//! one directory bleed its cwd into an unrelated channel chat, so `/new` in a
//! Telegram group could silently adopt the directory the operator happened to
//! be using in the TUI. Same-chat scoping keeps within-chat continuity without
//! the cross-channel leak.

use anyhow::Result;

use crate::config::Config;
use crate::db::models::Session;
use crate::services::SessionService;

/// Create a new channel session, inheriting provider + model from the most
/// recent existing session (TUI or any channel) when available, and the
/// working directory from `inherit_wd_from` — the session that received the
/// `/new` in this same chat (pass `None` for a brand-new chat with no prior
/// session).
///
/// Returns the created `Session`. Inheritance is best-effort: if the
/// most-recent-session lookup fails for any reason, the session is still
/// created with `provider_name = None` / `model = None`, matching the old
/// behavior so callers never fail because of this helper. When
/// `inherit_wd_from` is `None` or its session has no working directory, the
/// new session gets `working_directory = None`.
pub async fn create_channel_session(
    session_svc: &SessionService,
    title: Option<String>,
    inherit_wd_from: Option<&Session>,
) -> Result<Session> {
    create_channel_session_reported(session_svc, title, inherit_wd_from)
        .await
        .map(|(session, _)| session)
}

/// Same as [`create_channel_session`], also reporting whether a row was
/// created (#2183).
///
/// The chat-key insert is an insert-OR-RESOLVE: it returns the winning row
/// when one already exists for that chat key. A caller that logs "created"
/// therefore has to be told which happened, or it claims creation on every
/// message of a long-lived chat.
pub async fn create_channel_session_reported(
    session_svc: &SessionService,
    title: Option<String>,
    inherit_wd_from: Option<&Session>,
) -> Result<(Session, bool)> {
    // First, try to inherit from the most recent session (TUI or any channel).
    let (mut inherited_provider, mut inherited_model) =
        match session_svc.get_most_recent_session().await {
            Ok(Some(prev)) => (prev.provider_name, prev.model),
            _ => (None, None),
        };

    // A provider the config does not enable is never carried forward (#2158):
    // a disabled provider would otherwise spread into every new session.
    let config = Config::load().ok();
    if let (Some(name), Some(cfg)) = (inherited_provider.as_deref(), config.as_ref())
        && !provider_enabled(cfg, name)
    {
        tracing::info!(
            "Channel session: not inheriting disabled provider {name:?}; using [agent] defaults"
        );
        inherited_provider = None;
        inherited_model = None;
    }

    // If no recent session exists, check if [agent] has default_provider/default_model set.
    // This lets config drive the fallback instead of relying on the config priority list.
    if inherited_provider.is_none()
        && let Some(config) = config.as_ref()
        && config.agent.default_provider.is_some()
    {
        inherited_provider = config.agent.default_provider.clone();
        inherited_model = config.agent.default_model.clone();
        tracing::info!(
            "Channel session using [agent] defaults: provider {:?} / model {:?}",
            inherited_provider,
            inherited_model,
        );
    }

    // Same-chat scoping: the working directory follows the session that
    // received /new, never the global most-recent session (#263).
    let inherited_wd = inherit_wd_from.and_then(|s| s.working_directory.clone());

    if inherited_provider.is_some() || inherited_wd.is_some() {
        tracing::info!(
            "Channel session inherited provider {:?} / model {:?} / wd {:?}",
            inherited_provider,
            inherited_model,
            inherited_wd,
        );
    }

    create_with_chat_identity_reported(
        session_svc,
        title,
        inherited_provider,
        inherited_model,
        inherited_wd,
    )
    .await
}

/// Route through the atomic insert-or-resolve path when the title carries a
/// stable `[chat:<id>]` suffix (#1721): two processes sharing one database
/// cannot be serialized by the in-process single-flight gate, so the unique
/// index decides and the loser joins the winner. Titles without a suffix
/// (should not happen on channel paths, but is legal for direct callers)
/// fall back to the plain insert.
///
/// Also reports whether a row was created (#2183). The chat-key insert
/// resolves an existing row, so a caller that logs "created" has to know
/// which happened.
pub(crate) async fn create_with_chat_identity_reported(
    session_svc: &SessionService,
    title: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    wd: Option<String>,
) -> Result<(Session, bool)> {
    let chat_key = title.as_deref().and_then(|t| {
        let idx = t.rfind("[chat:")?;
        Some(t[idx..].to_string())
    });
    match chat_key {
        Some(key) => {
            session_svc
                .insert_or_resolve_channel_session_reported(title, provider, model, wd, &key)
                .await
        }
        None => {
            let session = session_svc
                .create_session_with_provider(title, provider, model, wd)
                .await?;
            Ok((session, true))
        }
    }
}

/// Whether `name` is a provider the config enables (#2158).
fn provider_enabled(config: &Config, name: &str) -> bool {
    crate::brain::provider::factory::provider_config_by_name(config, name)
        .is_some_and(|p| p.enabled)
}

/// Start the session a channel's `/new` command asked for (#2186).
///
/// `/new` has to hand the chat key over to the row it creates, or the next
/// message resolves straight back to the previous session and the new row is
/// an orphan nobody ever writes to. Three things follow from that:
///
/// 1. The prior session is looked up by chat key, not by exact title. Channel
///    titles carry a `[chat:<id>]` suffix, so an exact-title lookup misses and
///    the working-directory inheritance (#263) silently never happens.
/// 2. The new row is created from the suffixed title, which is what routes it
///    through the keyed insert and stamps `channel_chat_key`.
/// 3. The prior session must give the key up first, because the unique index
///    is partial on live rows. Guests archive it. The owner's rows stay live
///    so `/sessions` keeps showing their history, so those release the key
///    instead of archiving.
///
/// `chat_key` is the `[chat:<id>]` suffix, `legacy_title` the visible label
/// without it. Lookup and hand-over failures are logged and tolerated: `/new`
/// means a fresh session is the intent, so creation proceeds either way
/// (#442 keeps it from being silent).
pub async fn start_new_channel_session(
    session_svc: &SessionService,
    chat_key: &str,
    legacy_title: &str,
    keep_prior_live: bool,
    log_prefix: &str,
) -> Result<Session> {
    let prior_session = match session_svc.find_session_by_channel_chat_key(chat_key).await {
        Ok(found) => found,
        Err(e) => {
            tracing::error!("{log_prefix}: /new prior-session lookup failed: {e:#}");
            None
        }
    };
    let prior_session = match prior_session {
        Some(s) => Some(s),
        None => session_svc
            .find_session_by_title_suffix(chat_key)
            .await
            .unwrap_or_else(|e| {
                tracing::error!("{log_prefix}: /new title-suffix lookup failed: {e:#}");
                None
            }),
    };

    if let Some(old) = prior_session.as_ref() {
        let handed_over = if keep_prior_live {
            session_svc.release_channel_chat_key(old.id).await
        } else {
            session_svc.archive_session(old.id).await
        };
        if let Err(e) = handed_over {
            tracing::error!(
                "{log_prefix}: /new could not hand the chat key over from {}: {e:#}",
                old.id
            );
        }
    }

    let current_title = format!("{legacy_title} {chat_key}");
    create_channel_session(session_svc, Some(current_title), prior_session.as_ref()).await
}
