//! Telegram userbot: local MTProto authentication and passive capture.
//!
//! The receive loop persists allowlisted text for `channel_search`. It never
//! invokes the agent and exposes no outbound-as-user path.

pub(crate) mod capture;
pub(crate) mod download;
pub(crate) mod login;
pub(crate) mod runner;
pub(crate) mod session;
pub(crate) mod watch;

use std::path::PathBuf;

use crate::config::opencrabs_home;
use crate::config::types::TelegramUserbotConfig;

/// Where the local userbot session lives.
pub(crate) fn session_file(cfg: &TelegramUserbotConfig) -> PathBuf {
    let Some(raw) = cfg.session_path.as_deref().filter(|path| !path.is_empty()) else {
        return opencrabs_home().join("telegram_userbot.session.json");
    };
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(opencrabs_home);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_else(opencrabs_home).join(rest);
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        opencrabs_home().join(path)
    }
}

pub(crate) struct UserbotCreds {
    pub api_id: i32,
    pub api_hash: String,
    pub phone: String,
}

pub(crate) fn resolve_creds(cfg: &TelegramUserbotConfig) -> anyhow::Result<UserbotCreds> {
    let api_id = cfg.api_id.ok_or_else(|| {
        anyhow::anyhow!("channels.telegram.userbot.api_id missing; get it from my.telegram.org")
    })?;
    let api_id = i32::try_from(api_id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            anyhow::anyhow!("channels.telegram.userbot.api_id must be a positive i32")
        })?;
    let api_hash = cfg
        .api_hash
        .clone()
        .filter(|hash| hash.len() == 32 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "channels.telegram.userbot.api_hash must be 32 hexadecimal characters in keys.toml"
            )
        })?;
    let phone = cfg
        .phone
        .clone()
        .filter(|phone| {
            phone.starts_with('+')
                && phone.len() >= 8
                && phone[1..].bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| {
            anyhow::anyhow!("channels.telegram.userbot.phone must use international +digits format")
        })?;
    Ok(UserbotCreds {
        api_id,
        api_hash,
        phone,
    })
}
