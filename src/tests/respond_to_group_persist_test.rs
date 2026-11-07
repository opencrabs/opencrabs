//! End-to-end regression for #264: `/respond_to <mode>` issued from a group
//! must persist a `[channels.telegram.groups.<chat_id>]` override to
//! config.toml, and a fresh `Config::load()` (what a restart does) must read
//! it back and resolve it for that group.
//!
//! The production symptom looked like the setting was wiped on restart. It
//! never was: the write path is format-preserving and restart-safe. The
//! setting was simply never written, because the group command had been
//! autocompleted to another bot's handle (so this bot ignored it) and the
//! fallback @handle strip used to discard the command's argument (#265).

// The tests here override the profile home, which `dirs::home_dir()` ignores
// on Windows, so every one of them is `#[cfg(unix)]` and so is this harness.
#[cfg(unix)]
use crate::channels::commands::handle_respond_to;
#[cfg(unix)]
use crate::config::profile::{home_for_profile, with_profile_home_async};
#[cfg(unix)]
use crate::config::{Config, RespondTo};

#[cfg(unix)]
fn write_profile_home(home: &std::path::Path, config_toml: &str) {
    std::fs::create_dir_all(home).expect("create profile home");
    std::fs::write(home.join("config.toml"), config_toml).expect("write config");
}

// Unix-only for the same reason as other HOME-override config tests: on
// Windows `dirs::home_dir()` ignores env vars, so the guard has no effect.
#[cfg(unix)]
#[tokio::test]
async fn respond_to_in_group_persists_and_survives_reload() {
    let profile = format!("test_respond_group_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "[channels.telegram]\nenabled = true\nrespond_to = \"mention\"\n",
    );

    with_profile_home_async(Some(&profile), async {
        let chat_id = "-1001234567890";
        let reply = handle_respond_to("all", Some(chat_id)).await;
        assert!(
            reply.starts_with("✅"),
            "expected a success reply, got: {reply}"
        );

        // The groups override is on disk, format preserved.
        let raw = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            raw.contains("groups") && raw.contains(chat_id),
            "config.toml is missing the groups override:\n{raw}"
        );

        // A fresh load (what a restart does) resolves the override.
        let cfg = Config::load().expect("config must reload after the group write");
        let group = cfg
            .channels
            .telegram
            .groups
            .get(chat_id)
            .expect("groups entry must persist across reload");
        assert!(matches!(group.respond_to, Some(RespondTo::All)));
        // Channel-level setting is untouched.
        assert!(matches!(
            cfg.channels.telegram.respond_to,
            RespondTo::Mention
        ));
    })
    .await;
}

// A DM (no chat id) keeps writing the channel-level key, not a group entry.
#[cfg(unix)]
#[tokio::test]
async fn respond_to_in_dm_writes_channel_level() {
    let profile = format!("test_respond_dm_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "[channels.telegram]\nenabled = true\nrespond_to = \"mention\"\n",
    );

    with_profile_home_async(Some(&profile), async {
        let reply = handle_respond_to("all", None).await;
        assert!(
            reply.starts_with("✅"),
            "expected a success reply, got: {reply}"
        );

        let cfg = Config::load().expect("config must reload");
        assert!(matches!(cfg.channels.telegram.respond_to, RespondTo::All));
        assert!(cfg.channels.telegram.groups.is_empty());
    })
    .await;
}

// Regression: when the requested mode MATCHES the global fallback (no
// per-group override yet), the handler must still CREATE the per-group
// section instead of short-circuiting with "Already in … mode".
#[cfg(unix)]
#[tokio::test]
async fn respond_to_in_group_same_as_global_still_creates_override() {
    let profile = format!("test_respond_same_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    // Global is "mention", no per-group overrides.
    write_profile_home(
        &home,
        "[channels.telegram]\nenabled = true\nrespond_to = \"mention\"\n",
    );

    with_profile_home_async(Some(&profile), async {
        let chat_id = "-5324478558";
        // Set the same mode as the global — must still write per-group.
        let reply = handle_respond_to("mention", Some(chat_id)).await;
        assert!(
            reply.starts_with("✅"),
            "expected success (not 'already in'), got: {reply}"
        );

        // Verify the per-group section was written to disk.
        let raw = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            raw.contains("groups") && raw.contains(chat_id),
            "config.toml is missing the groups override when value == global:\n{raw}"
        );

        // Fresh load must see the per-group override.
        let cfg = Config::load().expect("config must reload after group write");
        let group = cfg
            .channels
            .telegram
            .groups
            .get(chat_id)
            .expect("groups entry must exist even when value matches global");
        assert!(matches!(group.respond_to, Some(RespondTo::Mention)));
        // Channel-level is untouched.
        assert!(matches!(
            cfg.channels.telegram.respond_to,
            RespondTo::Mention
        ));
    })
    .await;
}
