//! Shared channel-session resolution: look up by stable suffix, fall back to
//! legacy exact-title rows with forward-migration, archive on idle, otherwise
//! create.
//!
//! Issue #121: title-based session lookup orphans rows once the agent's
//! auto-rename rewrites the title. Discord, Slack, and WhatsApp all used
//! `find_session_by_title(&exact_template)`, so after a rename the next
//! message's lookup missed and a duplicate session was created on every turn.
//!
//! Fix mirrors the Telegram approach (PR #123): embed a stable `[chat:<id>]`
//! suffix at session creation, look up by suffix on subsequent messages, and
//! one-shot migrate any legacy pre-suffix row when found.
//!
//! Telegram has its own resolver in `channels::telegram::session_resolve`
//! because it also juggles an in-memory chat→session binding map and group
//! label-refresh policy. Discord/Slack/WhatsApp don't need either of those.

use anyhow::Result;
use uuid::Uuid;

use crate::services::SessionService;

/// Build the stable `[chat:<id>]` suffix that every channel session title ends
/// with. The id is the platform-stable identifier (Discord channel id, Slack
/// channel id, WhatsApp phone, …). Same shape as Telegram so a single
/// `find_session_by_title_suffix` call resolves any channel.
pub fn chat_id_suffix(id: &str) -> String {
    format!("[chat:{id}]")
}

/// True when a session exceeded the configured idle window.
pub fn session_idle_expired(
    updated_at: chrono::DateTime<chrono::Utc>,
    idle_hours: Option<f64>,
) -> bool {
    idle_hours.is_some_and(|h| {
        let elapsed = (chrono::Utc::now() - updated_at).num_seconds();
        elapsed > (h * 3600.0) as i64
    })
}

/// Resolve an existing channel session or create a new one, preferring
/// suffix-stable lookup so auto-renamed rows stay findable.
///
/// Resolution order:
/// 1. `find_session_by_channel_chat_key(suffix)` — the unique identity, and
///    the only lookup that survives auto-renaming (#2183).
/// 2. `find_session_by_title_suffix(suffix)` — rows that predate
///    `channel_chat_key`.
/// 3. `find_session_by_title(legacy_title)` — pre-suffix rows; on hit, the
///    row is forward-migrated to include the suffix so future lookups take
///    path 1.
/// 4. Idle archive + new session, or fresh new session.
///
/// `log_prefix` is the channel name used in `tracing` output ("Discord",
/// "Slack", "WhatsApp"). `current_title` is the freshly built template with
/// suffix — used for new sessions and for forward-migration writes.
pub async fn resolve_or_create_channel_session(
    session_svc: &SessionService,
    suffix: &str,
    legacy_title: &str,
    current_title: &str,
    idle_hours: Option<f64>,
    log_prefix: &str,
) -> Result<Uuid> {
    // Single-flight per (channel, chat-id) key (#1201, #1228): resolution is
    // look-up-then-create and the two steps are not atomic, so two messages
    // into one brand-new chat both miss the lookup and both create a session
    // (two parallel turns, orphaned context). The guard spans the ENTIRE
    // resolve-or-create below — released when we return — so a concurrent
    // second caller waits and then finds the session this one created.
    let _resolve_gate =
        crate::channels::single_flight::hold(format!("chan:{log_prefix}:{suffix}")).await;

    // A lookup ERROR is never no-session-found (#442): treating it as None
    // silently forks the chat onto a fresh session and orphans its history
    // (an i32-unreadable row did exactly that). Propagate; the caller tells
    // the user and skips the message.
    // `suffix` IS the chat key (`[chat:<id>]`), so the keyed lookup answers
    // the question directly. Auto-renaming rewrites the title and takes the
    // suffix with it, which is why the title lookups below miss on
    // long-lived chats (#2183).
    let mut existing = session_svc
        .find_session_by_channel_chat_key(suffix)
        .await
        .map_err(|e| e.context(format!("{log_prefix}: session lookup failed for {suffix}")))?;

    if existing.is_none() {
        existing = session_svc
            .find_session_by_title_suffix(suffix)
            .await
            .map_err(|e| e.context(format!("{log_prefix}: session lookup failed for {suffix}")))?;
    }

    let legacy_hit = if existing.is_none() {
        session_svc
            .find_session_by_title(legacy_title)
            .await
            .map_err(|e| {
                e.context(format!(
                    "{log_prefix}: legacy session lookup failed for '{legacy_title}'"
                ))
            })?
    } else {
        None
    };
    if existing.is_none()
        && let Some(legacy) = legacy_hit
    {
        tracing::info!(
            "{}: forward-migrating legacy session {} '{}' → '{}'",
            log_prefix,
            legacy.id,
            legacy.title.as_deref().unwrap_or(""),
            current_title,
        );
        let mut migrated = legacy.clone();
        migrated.title = Some(current_title.to_string());
        if let Err(e) = session_svc.update_session(&migrated).await {
            tracing::warn!(
                "{}: failed to forward-migrate session {} title: {}",
                log_prefix,
                legacy.id,
                e
            );
            existing = Some(legacy);
        } else {
            existing = Some(migrated);
        }
    }

    if let Some(session) = existing {
        if session_idle_expired(session.updated_at, idle_hours) {
            if let Err(e) = session_svc.archive_session(session.id).await {
                tracing::error!(
                    "{}: failed to archive idle session {}: {}",
                    log_prefix,
                    session.id,
                    e,
                );
            }
            let (new_session, created) =
                crate::channels::session_init::create_channel_session_reported(
                    session_svc,
                    Some(current_title.to_string()),
                    Some(&session),
                )
                .await?;
            if created {
                tracing::info!(
                    "{}: idle-timeout reset — new session {} for \"{}\"",
                    log_prefix,
                    new_session.id,
                    current_title,
                );
            } else {
                tracing::info!(
                    "{}: idle-timeout reset — resolved existing session {} for \"{}\"",
                    log_prefix,
                    new_session.id,
                    current_title,
                );
            }
            Ok(new_session.id)
        } else {
            Ok(session.id)
        }
    } else {
        let (new_session, created) =
            crate::channels::session_init::create_channel_session_reported(
                session_svc,
                Some(current_title.to_string()),
                None,
            )
            .await?;
        if created {
            tracing::info!(
                "{}: created new channel session {} for \"{}\"",
                log_prefix,
                new_session.id,
                current_title,
            );
        } else {
            tracing::info!(
                "{}: resolved existing channel session {} for \"{}\"",
                log_prefix,
                new_session.id,
                current_title,
            );
        }
        Ok(new_session.id)
    }
}
