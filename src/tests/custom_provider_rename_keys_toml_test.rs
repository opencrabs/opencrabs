//! Pin the keys.toml cleanup on custom-provider rename.
//!
//! Regression: 2026-06-05. User renamed `modelscope-qwen` → `modelscope`
//! via /models. config.toml was updated correctly (old section removed,
//! new section created). BUT keys.toml still had the old
//! `[providers.custom.modelscope-qwen]` section. On the next
//! `Config::load()` call, `merge_provider_keys` saw the orphan keys.toml
//! entry, didn't find a matching config.toml entry, and CREATED a
//! phantom entry from the keys.toml side (the "creating minimal entry"
//! fallback at types.rs ~line 1878). The user then saw BOTH names in
//! /models — modelscope (real) and modelscope-qwen (phantom).
//!
//! The fix is two-pronged:
//!
//! 1. **Source**: `Config::remove_secret_section()` was added, and the
//!    rename path in `dialogs.rs` calls it right after porting the
//!    api_key to the new section. No more orphan left behind.
//!
//! 2. **Defensive**: `cleanup_keys_custom_providers()` was structurally
//!    broken — it asked `Self::load()` for "what's in config", which
//!    runs `merge_provider_keys`, which re-creates entries from
//!    keys.toml itself. So the orphan check always passed and nothing
//!    got cleaned. Now reads config.toml raw via
//!    `raw_config_custom_provider_names()` so the orphan check is true.
//!
//! Regression 2: 2026-09-07 (#1458). config.toml failed to parse
//! (duplicate `bot_owner` in `[channels.telegram]`). The raw name
//! helper returned an empty set on parse failure, so EVERY keys.toml
//! custom provider failed the counterpart check and the cleanup wiped
//! all 21 sections at startup. The helper now returns `Option` — `None`
//! (unreadable/unparseable) means "presence unknown, remove nothing".
//! The tests below pin that fail-open contract.

// Every test here overrides the profile home, which `dirs::home_dir()`
// ignores on Windows, so the harness is unix-only.
#[cfg(unix)]
use crate::config::Config;
#[cfg(unix)]
use crate::config::profile::{home_for_profile, with_profile_home};

#[cfg(unix)]
fn write_profile_home(home: &std::path::Path, config_toml: &str, keys_toml: &str) {
    std::fs::create_dir_all(home).expect("create profile home");
    std::fs::write(home.join("config.toml"), config_toml).expect("write config");
    std::fs::write(home.join("keys.toml"), keys_toml).expect("write keys");
}

#[cfg(unix)]
fn read_keys_toml(home: &std::path::Path) -> String {
    std::fs::read_to_string(home.join("keys.toml")).expect("read keys.toml")
}

// ── remove_secret_section unit (the source fix's primitive) ──────────

#[cfg(unix)]
#[test]
fn remove_secret_section_drops_named_provider_only() {
    let profile = format!("test_rename_drop_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "",
        "[providers.custom.modelscope-qwen]\napi_key = \"old\"\n\n\
         [providers.custom.modelscope]\napi_key = \"new\"\n",
    );

    with_profile_home(Some(&profile), || {
        Config::remove_secret_section("providers.custom.modelscope-qwen")
            .expect("remove_secret_section succeeds");

        let after = read_keys_toml(&home);
        assert!(
            !after.contains("modelscope-qwen"),
            "old-name section must be gone from keys.toml after remove_secret_section; got:\n{}",
            after
        );
        assert!(
            after.contains("[providers.custom.modelscope]"),
            "new-name section must survive untouched; got:\n{}",
            after
        );
        assert!(
            after.contains("api_key = \"new\""),
            "new section's api_key must survive untouched; got:\n{}",
            after
        );
    });
}

#[cfg(unix)]
#[test]
fn remove_secret_section_missing_file_is_ok() {
    let profile = format!("test_rename_nofile_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    std::fs::create_dir_all(&home).expect("create profile home");
    // No keys.toml file at all. Must succeed silently.
    with_profile_home(Some(&profile), || {
        Config::remove_secret_section("providers.custom.whatever")
            .expect("missing keys.toml must not error");
    });
}

#[cfg(unix)]
#[test]
fn remove_secret_section_missing_section_is_ok() {
    let profile = format!("test_rename_nosec_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(&home, "", "[providers.custom.other]\napi_key = \"key\"\n");

    with_profile_home(Some(&profile), || {
        Config::remove_secret_section("providers.custom.does-not-exist")
            .expect("missing section must not error");

        let after = read_keys_toml(&home);
        assert!(
            after.contains("[providers.custom.other]"),
            "unrelated sections must survive a noop remove; got:\n{}",
            after
        );
    });
}

// ── cleanup_keys_custom_providers (the defensive fix) ────────────────

#[cfg(unix)]
#[test]
fn cleanup_drops_orphan_keys_when_config_has_no_matching_entry() {
    let profile = format!("test_cleanup_orphan_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "[providers.custom.modelscope]\nenabled = true\nbase_url = \"https://api/v1\"\ndefault_model = \"m\"\n",
        "[providers.custom.modelscope-qwen]\napi_key = \"orphan\"\n\n\
         [providers.custom.modelscope]\napi_key = \"current\"\n",
    );

    with_profile_home(Some(&profile), || {
        Config::cleanup_keys_custom_providers();

        let after = read_keys_toml(&home);
        assert!(
            !after.contains("modelscope-qwen"),
            "orphan keys.toml entry (no config.toml counterpart) must be removed by cleanup. \
             If this assertion fires, the cleanup is back to consulting the merged config \
             loader instead of `raw_config_custom_provider_names`, and the circular bug is back. \
             Got keys.toml:\n{}",
            after
        );
        assert!(
            after.contains("[providers.custom.modelscope]"),
            "non-orphan entry must survive cleanup; got:\n{}",
            after
        );
    });
}

#[cfg(unix)]
#[test]
fn cleanup_preserves_keys_when_every_entry_has_config_counterpart() {
    let profile = format!("test_cleanup_preserve_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "[providers.custom.a]\nenabled = true\nbase_url = \"u\"\ndefault_model = \"m\"\n\n\
         [providers.custom.b]\nenabled = true\nbase_url = \"u\"\ndefault_model = \"m\"\n",
        "[providers.custom.a]\napi_key = \"key-a\"\n\n\
         [providers.custom.b]\napi_key = \"key-b\"\n",
    );

    with_profile_home(Some(&profile), || {
        Config::cleanup_keys_custom_providers();

        let after = read_keys_toml(&home);
        assert!(after.contains("[providers.custom.a]"));
        assert!(after.contains("[providers.custom.b]"));
        assert!(after.contains("api_key = \"key-a\""));
        assert!(after.contains("api_key = \"key-b\""));
    });
}

#[cfg(unix)]
#[test]
fn cleanup_no_op_when_keys_toml_does_not_exist() {
    let profile = format!("test_cleanup_nokeys_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    std::fs::create_dir_all(&home).expect("create profile home");
    // No keys.toml file at all.

    with_profile_home(Some(&profile), || {
        // Must not panic, must not create keys.toml as a side effect.
        Config::cleanup_keys_custom_providers();

        assert!(
            !home.join("keys.toml").exists(),
            "cleanup must not create keys.toml as a side effect when it didn't exist"
        );
    });
}

// ── Source-level invariant on the rename path ────────────────────────

#[cfg(unix)]
#[test]
fn rename_path_in_onboarding_save_calls_remove_secret_section() {
    // Anchor the source fix: the custom-provider rename branch in
    // `OnboardingState::apply_config` (the save path /models now reuses)
    // MUST call remove_secret_section on the old section name. Without it,
    // the fix regresses to the 2026-06-05 ghost-entry shape and only the
    // defensive cleanup catches it (and only on the next save that triggers
    // cleanup). The rename is detected via `editing_custom_key`, which the
    // provider selector sets when an existing custom entry is loaded.
    const SAVE_SRC: &str = include_str!("../tui/onboarding/config.rs");

    // Strip comments so the regression doc-comment doesn't false-match.
    let no_comments: String = SAVE_SRC
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        no_comments.contains("Config::remove_secret_section(&old_section)"),
        "apply_config must call Config::remove_secret_section(&old_section) in the \
         custom-provider rename branch (gated on editing_custom_key != custom_name). \
         Without it, keys.toml retains the old `[providers.custom.<old>]` section after a \
         rename and merge_provider_keys resurrects the old name as a phantom entry on the \
         next Config::load — exactly the 2026-06-05 modelscope-qwen → modelscope bug."
    );
}

#[cfg(unix)]
#[test]
fn rename_path_in_onboarding_save_removes_old_config_section() {
    // The config.toml counterpart of the keys.toml cleanup: on rename the old
    // `[providers.custom.<old>]` table must also be dropped from config.toml,
    // otherwise it lingers as a disabled phantom provider in /models.
    const SAVE_SRC: &str = include_str!("../tui/onboarding/config.rs");
    let no_comments: String = SAVE_SRC
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        no_comments.contains("Config::remove_section(&old_section)"),
        "apply_config must call Config::remove_section(&old_section) in the custom-provider \
         rename branch so the old config.toml table doesn't linger as a phantom entry."
    );
}

// ── #1458 regression: unparseable config must never delete keys ──────

/// Keys.toml content mirroring the 2026-09-07 incident state: several
/// custom providers with live api_keys, none of which may be touched.
#[cfg(unix)]
fn incident_keys_toml() -> String {
    "[providers.custom.bigmodel-glm52]\napi_key = \"k-bigmodel\"\n\n\
     [providers.custom.nvidia-stepfun]\napi_key = \"k-stepfun\"\n\n\
     [providers.custom.cerebras]\napi_key = \"k-cerebras\"\n\n\
     [providers.custom.modelscope]\napi_key = \"k-modelscope\"\n"
        .to_string()
}

/// config.toml mirroring the exact incident breakage: a duplicate key
/// inside `[channels.telegram]`, which rejects both the serde/toml load
/// AND the toml_edit parse the cleanup uses.
#[cfg(unix)]
fn incident_broken_config_toml() -> String {
    "bot_owner = 7711740248\n\n[channels.telegram]\nbot_owner = 7711740248\n\
     bot_owner = 7711740248\n"
        .to_string()
}

#[cfg(unix)]
#[test]
fn cleanup_keeps_all_keys_when_config_toml_unparseable() {
    let profile = format!("test_cleanup_unparseable_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    let broken = incident_broken_config_toml();
    let keys = incident_keys_toml();
    write_profile_home(&home, &broken, &keys);

    with_profile_home(Some(&profile), || {
        Config::cleanup_keys_custom_providers();

        let after = read_keys_toml(&home);
        assert_eq!(
            after, keys,
            "an unparseable config.toml means provider presence is UNKNOWN — cleanup must \
             leave keys.toml byte-identical. This fired for real on 2026-09-07 (#1458): 21 \
             custom providers were wiped at startup because a duplicate-key parse error made \
             them all look like ghosts."
        );
    });
}

#[cfg(unix)]
#[test]
fn cleanup_keeps_all_keys_when_config_toml_missing() {
    let profile = format!("test_cleanup_noconf_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    // config.toml deliberately NOT written; keys.toml populated.
    std::fs::create_dir_all(&home).expect("create profile home");
    std::fs::write(home.join("keys.toml"), incident_keys_toml()).expect("write keys");

    with_profile_home(Some(&profile), || {
        Config::cleanup_keys_custom_providers();

        let after = read_keys_toml(&home);
        assert!(
            after.contains("[providers.custom.bigmodel-glm52]")
                && after.contains("[providers.custom.nvidia-stepfun]")
                && after.contains("[providers.custom.cerebras]")
                && after.contains("[providers.custom.modelscope]"),
            "missing config.toml is 'unknown', not 'empty' — all key sections must survive"
        );
    });
}

#[cfg(unix)]
#[test]
fn raw_custom_provider_names_none_when_unreadable_some_when_readable() {
    use crate::config::types::io::raw_config_custom_provider_names;

    // Unparseable → None (never an empty set: that means "readable and empty").
    let profile = format!("test_raw_none_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(&home, &incident_broken_config_toml(), "");
    with_profile_home(Some(&profile), || {
        assert!(
            raw_config_custom_provider_names().is_none(),
            "duplicate-key config.toml must yield None, not an empty set — the empty set \
             was the direct cause of the #1458 wipe"
        );
    });

    // Missing file → None.
    let profile = format!("test_raw_missing_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    std::fs::create_dir_all(&home).expect("create profile home");
    with_profile_home(Some(&profile), || {
        assert!(raw_config_custom_provider_names().is_none());
    });

    // Readable with a custom table → Some(names).
    let profile = format!("test_raw_some_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(
        &home,
        "[providers.custom.alpha]\nenabled = true\nbase_url = \"u\"\ndefault_model = \"m\"\n",
        "",
    );
    with_profile_home(Some(&profile), || {
        let names = raw_config_custom_provider_names()
            .expect("readable config must yield Some, never None");
        assert!(names.contains("alpha"), "got: {names:?}");
    });

    // Readable with NO custom table → Some(empty) — the only case where
    // an empty set is legitimate (genuinely nothing configured).
    let profile = format!("test_raw_empty_{}", uuid::Uuid::new_v4());
    let home = home_for_profile(Some(&profile));
    write_profile_home(&home, "[channels.telegram]\nbot_owner = 1\n", "");
    with_profile_home(Some(&profile), || {
        assert_eq!(raw_config_custom_provider_names(), Some(Default::default()));
    });
}
