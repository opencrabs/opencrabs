//! Skills-change re-publication of the Telegram command menus (#1317).
//!
//! `set_my_commands` stores the menu server-side: clients render whatever
//! Telegram holds, so the picker only changes when something re-publishes.
//! The catalog's mutable inputs are `commands.toml` (covered — the
//! ConfigWatcher re-publishes on any config write) and the skills dirs
//! (`~/.opencrabs/skills/` plus `~/.opencrabs/projects/*/skills/`), which
//! had no trigger: a skills-only change on a quiet install left the picker
//! stale until the next config write or restart.
//!
//! The fix is a cheap signature over those dirs (sorted skill identities +
//! `SKILL.md` mtimes + sizes, following symlinks so symlinked skills count).
//! Every inbound message compares it against the last-published value held
//! in [`TelegramState`]; a mismatch re-publishes every audience — the
//! default floor, the owner DM and configured groups (via
//! `register_bot_commands`), plus the unconfigured solo-owner groups of
//! #1155, which `maybe_auto_register` publishes per-chat and never
//! revisits. An unchanged signature costs a few dozen `stat()` calls; API
//! round-trips happen only on real change.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::channels::telegram::state::TelegramState;
use teloxide::Bot;

/// Signature of every skill the command catalog can currently see. Builtins
/// are embedded in the binary and cannot change at runtime, so only the
/// on-disk overlays are hashed.
pub(crate) fn skills_signature() -> u64 {
    skills_signature_from(
        &crate::brain::skills::user_skills_dir(),
        &crate::services::ProjectService::projects_dir(),
    )
}

/// Pure core of [`skills_signature`] over explicit roots, so tests can point
/// it at temp dirs. Mirrors `load_all_skills`' scan shape:
/// `<user_skills>/<name>/SKILL.md` and `<projects>/<project>/skills/<name>/SKILL.md`.
/// File times are part of the hash, so an edit inside the same filesystem
/// timestamp granularity with an unchanged size can slip through — the next
/// edit, restart or config write still catches it.
pub(crate) fn skills_signature_from(user_skills: &Path, projects_dir: &Path) -> u64 {
    let mut skills: Vec<(String, SystemTime, u64)> = Vec::new();
    collect_skill_files(user_skills, "", &mut skills);
    if let Ok(projects) = std::fs::read_dir(projects_dir) {
        for project in projects.flatten() {
            let file_name = project.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            collect_skill_files(&project.path().join("skills"), name, &mut skills);
        }
    }
    skills.sort();
    let mut hasher = DefaultHasher::new();
    skills.len().hash(&mut hasher);
    for (name, mtime, len) in skills {
        name.hash(&mut hasher);
        mtime.hash(&mut hasher);
        len.hash(&mut hasher);
    }
    hasher.finish()
}

/// One overlay root: every subdirectory holding a `SKILL.md`. `is_dir()`
/// follows symlinks and `metadata()` resolves them, matching the loader — a
/// skill symlinked into the overlay (the #1317 report) is hashed by its
/// target's state.
fn collect_skill_files(dir: &Path, prefix: &str, out: &mut Vec<(String, SystemTime, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(path.join("SKILL.md")) else {
            continue;
        };
        let qualified = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        out.push((qualified, meta.modified().unwrap_or(UNIX_EPOCH), meta.len()));
    }
}

/// Compare the current skills signature against the last one published; on
/// change, re-publish every menu audience. Called from the inbound-message
/// path, so a skill added mid-flight shows up in the picker by the next
/// message — no restart, no config write (#1317).
pub(crate) async fn refresh_menus_if_skills_changed(bot: &Bot, telegram_state: &TelegramState) {
    let signature = skills_signature();
    if telegram_state.menu_skills_sig().await == Some(signature) {
        return;
    }
    telegram_state.set_menu_skills_sig(signature).await;

    // Default floor + owner DM + configured groups.
    super::agent::register_bot_commands(bot).await;

    // Unconfigured solo-owner groups (#1155) are published per-chat and
    // never revisited by `maybe_auto_register`, so refresh them here too.
    let solo_chats = telegram_state.solo_registered_chats().await;
    if !solo_chats.is_empty() {
        match crate::config::Config::load() {
            Ok(cfg) => match super::menu_scope::owner_user_id(
                &cfg.channels.telegram.allowed_users,
                &cfg.channels.telegram.bot_owner,
            )
            .map(|id| id as i64)
            {
                Some(owner_id) => {
                    let commands = super::agent::collect_command_catalog();
                    for chat_id in &solo_chats {
                        super::menu_auto::publish_owner_menu(bot, *chat_id, owner_id, &commands)
                            .await;
                    }
                }
                None => tracing::debug!(
                    "Telegram: no owner configured — solo-owner menus skipped in skills refresh"
                ),
            },
            Err(e) => tracing::warn!(
                "Telegram: config unreadable during skills refresh, {} solo menu(s) not \
                 re-published: {e}",
                solo_chats.len()
            ),
        }
    }
    tracing::info!(
        "Telegram: skills changed ({signature:#x}) — command menus re-published ({} \
         solo-owner group(s) refreshed)",
        solo_chats.len()
    );
}
