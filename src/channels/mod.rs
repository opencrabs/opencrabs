//! Channel Integrations
//!
//! Messaging channel integrations (Telegram, WhatsApp, Discord, Slack) and the
//! shared factory for creating channel-specific agent services.

pub(crate) mod background_work;
pub mod bg_resume;
pub mod commands;
mod factory;
pub(crate) mod group_history;
pub mod manager;
pub(crate) mod model_menu;
pub(crate) mod owner_alert;
pub mod question_common;
pub(crate) mod respond_to_scope;
pub mod session_init;
pub mod session_resolve;
pub mod single_flight;
pub mod target_resolver;
pub(crate) mod transport_ready;
pub(crate) mod typing_tick;

pub mod voice;

#[cfg(feature = "discord")]
pub mod discord;
#[cfg(feature = "slack")]
pub mod slack;
#[cfg(feature = "telegram")]
pub mod telegram;
#[cfg(feature = "trello")]
pub mod trello;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

mod greeting;

pub use factory::{ChannelFactory, configure_cron_factory};
pub use greeting::generate_connection_greeting;
pub use manager::ChannelManager;

/// Root of the durable channel-attachment store, profile-resolved via
/// `opencrabs_home()` so a named-profile instance stores under its OWN home
/// (`~/.opencrabs/profiles/<name>/channel_attachments/`) rather than the shared
/// default root, matching every other runtime path (config, logs, plans) and
/// what `push_known_paths` tells the agent (#681). Single source of truth: the
/// writers, the migration, and the prompt must all resolve the store the same
/// way, so they all route through here. (#1729: promoted here from
/// telegram::media so every channel persists to the same documented store.)
pub(crate) fn channel_attachments_dir() -> std::path::PathBuf {
    crate::config::opencrabs_home().join("channel_attachments")
}

/// Core of [`persist_channel_attachment`], parameterised on the base dir so it
/// is unit-testable against a tempdir. Writes `bytes` under
/// `<base>/<platform>/<unix-millis>-<filename>`: the millisecond prefix keeps
/// the store collision-free without a filesystem round-trip, and the filename
/// is sanitized so sender-supplied names cannot escape the platform subdir.
/// Returns the written path, or `None` when the dir cannot be created or the
/// write fails. Persistence is best-effort and must never fail the inbound
/// message over it (#1729).
pub(crate) fn persist_channel_attachment_in(
    base: &std::path::Path,
    platform: &str,
    filename: &str,
    bytes: &[u8],
) -> Option<std::path::PathBuf> {
    let dir = base.join(platform);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("channel_attachments: mkdir {platform} failed: {e}");
        return None;
    }
    let safe = sanitize_attachment_name(filename);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{millis}-{safe}"));
    match std::fs::write(&path, bytes) {
        Ok(()) => Some(path),
        Err(e) => {
            tracing::warn!("channel_attachments: write {platform}/{safe} failed: {e}");
            None
        }
    }
}

/// Persist an inbound channel attachment to the durable store. See
/// [`persist_channel_attachment_in`] for the contract; this wrapper resolves
/// the profile-scoped base dir.
pub(crate) fn persist_channel_attachment(
    platform: &str,
    filename: &str,
    bytes: &[u8],
) -> Option<std::path::PathBuf> {
    persist_channel_attachment_in(&channel_attachments_dir(), platform, filename, bytes)
}

/// Flatten a sender-supplied attachment name to a single safe path component:
/// only ASCII alphanumeric, dot, dash and underscore survive, everything else
/// becomes `_`, and leading dots are stripped, so `../../x`, empty and dotfile
/// names all collapse to something inert under the platform subdir (#1729).
pub(crate) fn sanitize_attachment_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_start_matches('.');
    if trimmed.is_empty() {
        "attachment.bin".to_string()
    } else {
        trimmed.to_string()
    }
}
