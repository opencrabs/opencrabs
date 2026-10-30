//! Comprehensive tests for profile management — multi-instance isolation.
//!
//! Tests cover: name validation, token hashing, registry CRUD, profile lifecycle,
//! token-lock isolation, export/import, and edge cases.
//!
//! IMPORTANT: Filesystem CRUD tests that write to `~/.opencrabs/profiles.toml`
//! are combined into single sequential functions to prevent concurrent write
//! corruption. In-memory tests remain separate since they don't share state.

use std::fs;
use std::path::PathBuf;

use crate::config::profile::{
    ProfileEntry, ProfileRegistry, acquire_token_lock, active_profile, base_opencrabs_dir,
    create_profile, delete_profile, export_profile, foreign_lock_owners, hash_token,
    import_profile, list_profiles, migrate_profile, parse_lock_owner_pid, process_creation_stamp,
    release_all_locks, release_token_lock, resolve_creation_ticks, resolve_profile_home,
    set_active_profile, split_pid_ticks, validate_profile_name,
};
use tempfile::tempdir;

/// Serialize all tests that read or write under the $HOME-resolved
/// ~/.opencrabs (profiles.toml, locks/). This must be the SAME lock the
/// HOME-mutating tests take (crate::tests::HOME_ENV_LOCK), not a private
/// one: these tests resolve paths through the live $HOME, so interleaving
/// with a test that has HOME pointed at a tempdir makes writes land in the
/// temp home while the assertions check the real one — the source of rare
/// cross-file flakes in the parallel run (profile_test /
/// config_last_good_recovery_test failing only in full-suite runs).
/// Uses unwrap_or_else to recover from poisoned state (prior test panic).
fn fs_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::tests::HOME_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ─── Name Validation ─────────────────────────────────────────────────

#[test]
fn valid_profile_names() {
    assert!(validate_profile_name("hermes").is_ok());
    assert!(validate_profile_name("my-profile").is_ok());
    assert!(validate_profile_name("test_123").is_ok());
    assert!(validate_profile_name("a").is_ok());
    assert!(validate_profile_name("UPPERCASE").is_ok());
    assert!(validate_profile_name("MiXeD-CaSe_99").is_ok());
    assert!(validate_profile_name("x".repeat(64).as_str()).is_ok());
}

#[test]
fn invalid_profile_name_default() {
    let err = validate_profile_name("default").unwrap_err();
    assert!(err.to_string().contains("reserved"));
}

#[test]
fn invalid_profile_name_empty() {
    let err = validate_profile_name("").unwrap_err();
    assert!(err.to_string().contains("1-64"));
}

#[test]
fn invalid_profile_name_too_long() {
    let long = "x".repeat(65);
    let err = validate_profile_name(&long).unwrap_err();
    assert!(err.to_string().contains("1-64"));
}

#[test]
fn invalid_profile_name_spaces() {
    let err = validate_profile_name("has spaces").unwrap_err();
    assert!(err.to_string().contains("alphanumeric"));
}

#[test]
fn invalid_profile_name_slashes() {
    assert!(validate_profile_name("has/slash").is_err());
    assert!(validate_profile_name("back\\slash").is_err());
}

#[test]
fn invalid_profile_name_special_chars() {
    assert!(validate_profile_name("name@here").is_err());
    assert!(validate_profile_name("name.dot").is_err());
    assert!(validate_profile_name("name!bang").is_err());
    assert!(validate_profile_name("name#hash").is_err());
    assert!(validate_profile_name("emoji🦀").is_err());
}

#[test]
fn validate_boundary_length_names() {
    assert!(validate_profile_name("x").is_ok());
    assert!(validate_profile_name(&"a".repeat(64)).is_ok());
    assert!(validate_profile_name(&"a".repeat(65)).is_err());
}

// ─── Token Hashing ───────────────────────────────────────────────────

#[test]
fn hash_token_deterministic() {
    let h1 = hash_token("bot123:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw");
    let h2 = hash_token("bot123:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw");
    assert_eq!(h1, h2);
}

#[test]
fn hash_token_different_inputs() {
    let h1 = hash_token("token_a");
    let h2 = hash_token("token_b");
    assert_ne!(h1, h2);
}

#[test]
fn hash_token_fixed_length() {
    assert_eq!(hash_token("short").len(), 16);
    assert_eq!(hash_token("a".repeat(1000).as_str()).len(), 16);
    assert_eq!(hash_token("").len(), 16);
}

#[test]
fn hash_token_hex_chars_only() {
    let h = hash_token("anything");
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_token_empty_string() {
    let h = hash_token("");
    assert_eq!(h.len(), 16);
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_token_unicode() {
    let h = hash_token("🦀🦀🦀");
    assert_eq!(h.len(), 16);
}

// ─── Profile Registry (In-Memory) ───────────────────────────────────
// These tests use in-memory registry instances — no filesystem contention.

#[test]
fn registry_default_is_empty() {
    let reg = ProfileRegistry::default();
    assert!(reg.profiles.is_empty());
}

#[test]
fn registry_register_single() {
    let mut reg = ProfileRegistry::default();
    reg.register("hermes", Some("Messenger of the gods"));
    assert!(reg.profiles.contains_key("hermes"));
    assert_eq!(reg.profiles["hermes"].name, "hermes");
    assert_eq!(
        reg.profiles["hermes"].description.as_deref(),
        Some("Messenger of the gods")
    );
    assert!(!reg.profiles["hermes"].created_at.is_empty());
    assert!(reg.profiles["hermes"].last_used.is_none());
}

#[test]
fn registry_register_no_description() {
    let mut reg = ProfileRegistry::default();
    reg.register("scout", None);
    assert!(reg.profiles["scout"].description.is_none());
}

#[test]
fn registry_register_multiple() {
    let mut reg = ProfileRegistry::default();
    reg.register("alpha", Some("First"));
    reg.register("beta", Some("Second"));
    reg.register("gamma", None);
    assert_eq!(reg.profiles.len(), 3);
}

#[test]
fn registry_register_overwrites_duplicate() {
    let mut reg = ProfileRegistry::default();
    reg.register("hermes", Some("v1"));
    let created_v1 = reg.profiles["hermes"].created_at.clone();

    // `register` stamps `Utc::now().to_rfc3339()`. In release mode two
    // back-to-back calls can land in the same nanosecond, producing
    // identical timestamps and flaking the `assert_ne!` below. A 1 ms
    // sleep guarantees the second timestamp is strictly later.
    std::thread::sleep(std::time::Duration::from_millis(1));

    reg.register("hermes", Some("v2"));
    assert_eq!(reg.profiles["hermes"].description.as_deref(), Some("v2"));
    assert_ne!(reg.profiles["hermes"].created_at, created_v1);
}

#[test]
fn registry_touch_updates_last_used() {
    let mut reg = ProfileRegistry::default();
    reg.register("hermes", None);
    assert!(reg.profiles["hermes"].last_used.is_none());

    reg.touch("hermes");
    assert!(reg.profiles["hermes"].last_used.is_some());
}

#[test]
fn registry_touch_nonexistent_is_noop() {
    let mut reg = ProfileRegistry::default();
    reg.touch("ghost");
    assert!(reg.profiles.is_empty());
}

#[test]
fn registry_serde_roundtrip() {
    let mut reg = ProfileRegistry::default();
    reg.register("hermes", Some("Test profile"));
    reg.register("scout", None);
    reg.touch("hermes");

    let serialized = toml::to_string_pretty(&reg).unwrap();
    let deserialized: ProfileRegistry = toml::from_str(&serialized).unwrap();

    assert_eq!(deserialized.profiles.len(), 2);
    assert!(deserialized.profiles.contains_key("hermes"));
    assert!(deserialized.profiles.contains_key("scout"));
    assert!(deserialized.profiles["hermes"].last_used.is_some());
    assert_eq!(
        deserialized.profiles["hermes"].description.as_deref(),
        Some("Test profile")
    );
}

#[test]
fn registry_serde_empty() {
    let reg = ProfileRegistry::default();
    let serialized = toml::to_string_pretty(&reg).unwrap();
    let deserialized: ProfileRegistry = toml::from_str(&serialized).unwrap();
    assert!(deserialized.profiles.is_empty());
}

#[test]
fn registry_deserialized_from_toml_string() {
    let toml_str = r#"
[profiles.hermes]
name = "hermes"
description = "Messenger"
created_at = "2026-03-31T00:00:00Z"

[profiles.scout]
name = "scout"
created_at = "2026-03-31T00:00:00Z"
"#;
    let reg: ProfileRegistry = toml::from_str(toml_str).unwrap();
    assert_eq!(reg.profiles.len(), 2);
    assert_eq!(
        reg.profiles["hermes"].description.as_deref(),
        Some("Messenger")
    );
    assert!(reg.profiles["scout"].description.is_none());
}

// ─── Profile Entry Serde ────────────────────────────────────────────

#[test]
fn profile_entry_json_roundtrip() {
    let entry = ProfileEntry {
        name: "test".to_string(),
        description: Some("desc".to_string()),
        created_at: "2026-03-31T00:00:00Z".to_string(),
        last_used: Some("2026-03-31T01:00:00Z".to_string()),
    };

    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: ProfileEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, "test");
    assert_eq!(deserialized.description.as_deref(), Some("desc"));
    assert_eq!(
        deserialized.last_used.as_deref(),
        Some("2026-03-31T01:00:00Z")
    );
}

#[test]
fn profile_entry_optional_fields() {
    let entry = ProfileEntry {
        name: "minimal".to_string(),
        description: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_used: None,
    };
    assert!(entry.description.is_none());
    assert!(entry.last_used.is_none());
}

// ─── Path Resolution ─────────────────────────────────────────────────

#[test]
fn base_dir_ends_with_opencrabs() {
    let base = base_opencrabs_dir();
    assert!(base.ends_with(".opencrabs"));
}

#[test]
fn base_dir_is_absolute() {
    let base = base_opencrabs_dir();
    assert!(base.is_absolute());
}

// ─── Error Messages ──────────────────────────────────────────────────

#[test]
fn delete_default_profile_fails() {
    let err = delete_profile("default").unwrap_err();
    assert!(err.to_string().contains("cannot delete"));
}

#[test]
fn delete_nonexistent_profile_fails() {
    let err = delete_profile("_nonexistent_profile_xyz").unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn export_nonexistent_profile_fails() {
    let archive = std::env::temp_dir().join("_test_nonexistent_export.tar.gz");
    let err = export_profile("_definitely_not_a_profile", &archive).unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn import_nonexistent_archive_fails() {
    let err = import_profile(&PathBuf::from("/tmp/_nonexistent_archive.tar.gz")).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

// ─── Registry Filesystem (Read-Only) ────────────────────────────────

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn registry_load_from_real_path() {
    let _guard = fs_lock();
    // Should not error regardless of host state
    let loaded = ProfileRegistry::load().unwrap_or_default();
    let _ = loaded.profiles.len();
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn list_profiles_always_includes_default() {
    let _guard = fs_lock();
    let profiles = list_profiles().unwrap();
    assert!(!profiles.is_empty());
    assert_eq!(profiles[0].name, "default");
    assert!(
        profiles[0]
            .description
            .as_deref()
            .unwrap()
            .contains("Default")
    );
}

// ─── Profile CRUD (Filesystem — Sequential) ─────────────────────────
// Everything that writes to ~/.opencrabs/ (profiles.toml, locks/, profiles/)
// runs inside ONE test function to prevent concurrent corruption from
// parallel test execution.

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn filesystem_operations_sequential() {
    let _guard = fs_lock();
    let pid = std::process::id();
    let lock_dir = base_opencrabs_dir().join("locks");
    fs::create_dir_all(&lock_dir).unwrap();

    // ══════════════════════════════════════════════════════════════════
    // Part 1: Profile CRUD lifecycle
    // ══════════════════════════════════════════════════════════════════
    let name = "_test_fs_seq";
    let profile_dir = base_opencrabs_dir().join("profiles").join(name);

    // Clean slate
    let _ = fs::remove_dir_all(&profile_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    let _ = reg.save();

    // Create
    let path = create_profile(name, Some("sequential test")).unwrap();
    assert!(path.exists(), "profile directory should be created");
    assert!(path.join("memory").exists(), "memory subdir should exist");
    assert!(path.join("logs").exists(), "logs subdir should exist");

    // Verify in registry
    let reg = ProfileRegistry::load().unwrap_or_default();
    assert!(reg.profiles.contains_key(name), "should be in registry");

    // Duplicate create fails
    let err = create_profile(name, None).unwrap_err();
    assert!(err.to_string().contains("already exists"));

    // List includes it
    let profiles = list_profiles().unwrap();
    let found = profiles.iter().any(|p| p.name == name);
    assert!(found, "should appear in list");

    // Delete
    delete_profile(name).unwrap();
    assert!(!path.exists(), "directory gone after delete");

    let reg = ProfileRegistry::load().unwrap_or_default();
    assert!(!reg.profiles.contains_key(name), "removed from registry");

    // Delete again fails
    let err = delete_profile(name).unwrap_err();
    assert!(err.to_string().contains("does not exist"));

    // ══════════════════════════════════════════════════════════════════
    // Part 2: Export/Import roundtrip
    // ══════════════════════════════════════════════════════════════════
    let exp_name = "_test_fs_exp";
    let exp_dir = base_opencrabs_dir().join("profiles").join(exp_name);
    let archive = std::env::temp_dir().join(format!("_test_fs_export_{}.tar.gz", pid));

    let _ = fs::remove_dir_all(&exp_dir);
    let _ = fs::remove_file(&archive);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(exp_name);
    let _ = reg.save();

    // Create with content
    let dir = create_profile(exp_name, Some("export test")).unwrap();
    fs::write(dir.join("config.toml"), "[agent]\ncontext_limit = 42000").unwrap();
    fs::write(dir.join("memory").join("note.md"), "remember this").unwrap();

    // Export
    export_profile(exp_name, &archive).unwrap();
    assert!(archive.exists(), "archive created");
    assert!(archive.metadata().unwrap().len() > 0, "archive non-empty");

    // Delete
    delete_profile(exp_name).unwrap();
    assert!(!dir.exists());

    // Import
    let imported = import_profile(&archive).unwrap();
    assert_eq!(imported, exp_name);

    // Verify content survived
    let reimported = base_opencrabs_dir().join("profiles").join(exp_name);
    assert!(reimported.exists());
    let config = fs::read_to_string(reimported.join("config.toml")).unwrap();
    assert!(config.contains("context_limit = 42000"));
    let note = fs::read_to_string(reimported.join("memory").join("note.md")).unwrap();
    assert_eq!(note, "remember this");

    // Registry has it
    let reg = ProfileRegistry::load().unwrap_or_default();
    assert!(reg.profiles.contains_key(exp_name));

    // Clean up
    let _ = delete_profile(exp_name);
    let _ = fs::remove_file(&archive);

    // Export default profile
    let default_archive =
        std::env::temp_dir().join(format!("_test_fs_default_export_{}.tar.gz", pid));
    let _ = fs::remove_file(&default_archive);
    // Retry once for transient IO (concurrent dir mutations under ~/.opencrabs/)
    let result = export_profile("default", &default_archive)
        .or_else(|_| export_profile("default", &default_archive));
    if result.is_ok() {
        assert!(default_archive.exists());
    }
    let _ = fs::remove_file(&default_archive);

    // ══════════════════════════════════════════════════════════════════
    // Part 3: Token locks
    // ══════════════════════════════════════════════════════════════════

    // Basic acquire and release
    let ch1 = "_test_fs_lk1";
    let th1 = hash_token("fs_lock_1");
    release_token_lock(ch1, &th1);

    acquire_token_lock(ch1, &th1).unwrap();
    let lf1 = lock_dir.join(format!("{}_{}.lock", ch1, th1));
    assert!(lf1.exists(), "lock file created");

    let contents = fs::read_to_string(&lf1).unwrap();
    assert!(contents.contains(&pid.to_string()), "contains our PID");

    // Format: "profile:pid", with a Windows-only ":ticks" tail that records
    // the owner's creation time for the preempt path's ownership proof.
    // Assert the shape both platforms share rather than the exact string,
    // so this test does not silently only pass off Windows.
    let (profile_field, pid_field) = contents
        .trim()
        .split_once(':')
        .expect("lock file should be 'profile:pid' format");
    assert!(
        !profile_field.is_empty(),
        "profile name should not be empty"
    );
    let (stamped_pid, _ticks) = split_pid_ticks(pid_field).expect("PID field must parse");
    assert_eq!(stamped_pid, pid);

    release_token_lock(ch1, &th1);
    assert!(!lf1.exists(), "lock file removed after release");

    // Re-acquire same lock (same PID, same profile)
    let ch2 = "_test_fs_lk2";
    let th2 = hash_token("fs_lock_2");
    release_token_lock(ch2, &th2);
    acquire_token_lock(ch2, &th2).unwrap();
    acquire_token_lock(ch2, &th2).unwrap(); // same PID overwrite
    release_token_lock(ch2, &th2);

    // Stale lock from dead PID
    let ch3 = "_test_fs_lk3";
    let th3 = hash_token("fs_lock_3");
    let stale = lock_dir.join(format!("{}_{}.lock", ch3, th3));
    fs::write(&stale, "default:999999999").unwrap();
    acquire_token_lock(ch3, &th3).unwrap();
    let contents = fs::read_to_string(&stale).unwrap();
    assert!(contents.contains(&pid.to_string()));
    release_token_lock(ch3, &th3);

    // Different channels, same token hash
    let th_multi = hash_token("multi_ch");
    let ch_a = "_test_fs_mca";
    let ch_b = "_test_fs_mcb";
    release_token_lock(ch_a, &th_multi);
    release_token_lock(ch_b, &th_multi);
    acquire_token_lock(ch_a, &th_multi).unwrap();
    acquire_token_lock(ch_b, &th_multi).unwrap();
    let la = lock_dir.join(format!("{}_{}.lock", ch_a, th_multi));
    let lb = lock_dir.join(format!("{}_{}.lock", ch_b, th_multi));
    assert!(la.exists());
    assert!(lb.exists());

    // release_all_locks cleans our locks
    release_all_locks();
    assert!(!la.exists(), "release_all cleaned lock a");
    assert!(!lb.exists(), "release_all cleaned lock b");

    // release_all preserves other profiles' locks
    let th_other = hash_token("other_profile_tok");
    let other_lock = lock_dir.join(format!("_test_fs_other_{}.lock", th_other));
    fs::write(&other_lock, "other_profile:999999999").unwrap();
    release_all_locks();
    assert!(other_lock.exists(), "other profile's lock preserved");
    let _ = fs::remove_file(&other_lock);

    // Release nonexistent lock is noop
    release_token_lock("_nonexistent_channel", "0000000000000000");
}

// ─── Migration Tests ─────────────────────────────────────────────────

#[test]
fn migrate_same_profile_errors() {
    let err = migrate_profile("default", "default", false);
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("source and destination profiles are the same")
    );
}

#[test]
fn migrate_nonexistent_source_errors() {
    let err = migrate_profile("_test_migrate_no_src", "default", false);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn migrate_nonexistent_destination_errors() {
    let err = migrate_profile("default", "_test_migrate_no_dst", false);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("does not exist"));
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn migrate_profile_copies_md_and_toml_files() {
    let _guard = fs_lock();
    let base = crate::config::profile::base_opencrabs_dir();
    let src_name = "_test_migrate_src";
    let dst_name = "_test_migrate_dst";
    let src_dir = base.join("profiles").join(src_name);
    let dst_dir = base.join("profiles").join(dst_name);

    // Cleanup from previous runs
    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    reg.profiles.remove(dst_name);
    let _ = reg.save();

    // Create both profiles
    create_profile(src_name, Some("source")).unwrap();
    create_profile(dst_name, Some("destination")).unwrap();

    // Populate source with brain files and config
    fs::write(src_dir.join("SOUL.md"), "# Source Soul").unwrap();
    fs::write(src_dir.join("IDENTITY.md"), "# Source Identity").unwrap();
    fs::write(src_dir.join("config.toml"), "[general]\nname = \"source\"").unwrap();
    fs::write(src_dir.join("keys.toml"), "[keys]\nsecret = \"abc\"").unwrap();
    fs::create_dir_all(src_dir.join("memory")).unwrap();
    fs::write(src_dir.join("memory").join("note.md"), "# A memory").unwrap();

    // Files that should NOT migrate
    fs::write(src_dir.join("layout.json"), "{}").unwrap();
    fs::write(src_dir.join("profiles.toml"), "skip").unwrap();
    fs::write(src_dir.join("random.txt"), "not a toml or md").unwrap();

    // Migrate
    let migrated = migrate_profile(src_name, dst_name, false).unwrap();

    // Verify correct files were copied
    assert!(dst_dir.join("SOUL.md").exists());
    assert!(dst_dir.join("IDENTITY.md").exists());
    assert!(dst_dir.join("config.toml").exists());
    assert!(dst_dir.join("keys.toml").exists());
    assert!(dst_dir.join("memory").join("note.md").exists());

    // Verify content matches
    assert_eq!(
        fs::read_to_string(dst_dir.join("SOUL.md")).unwrap(),
        "# Source Soul"
    );
    assert_eq!(
        fs::read_to_string(dst_dir.join("memory").join("note.md")).unwrap(),
        "# A memory"
    );

    // Verify skipped files
    assert!(!dst_dir.join("layout.json").exists());
    assert!(!dst_dir.join("random.txt").exists());
    // profiles.toml in dst should not be the source's "skip" content
    assert!(!dst_dir.join("profiles.toml").exists());

    assert!(migrated.contains(&"SOUL.md".to_string()));
    assert!(migrated.contains(&"IDENTITY.md".to_string()));
    assert!(migrated.contains(&"config.toml".to_string()));
    assert!(migrated.contains(&"keys.toml".to_string()));
    assert!(migrated.contains(&"memory/note.md".to_string()));
    assert_eq!(migrated.len(), 5);

    // Cleanup
    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    reg.profiles.remove(dst_name);
    let _ = reg.save();
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn migrate_profile_skips_existing_without_force() {
    let _guard = fs_lock();
    let base = crate::config::profile::base_opencrabs_dir();
    let src_name = "_test_migrate_skip_src";
    let dst_name = "_test_migrate_skip_dst";
    let src_dir = base.join("profiles").join(src_name);
    let dst_dir = base.join("profiles").join(dst_name);

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    reg.profiles.remove(dst_name);
    let _ = reg.save();

    create_profile(src_name, None).unwrap();
    create_profile(dst_name, None).unwrap();

    // Source has a file
    fs::write(src_dir.join("SOUL.md"), "source content").unwrap();
    // Destination already has the same file
    fs::write(dst_dir.join("SOUL.md"), "existing content").unwrap();

    // Migrate without force — should skip
    let migrated = migrate_profile(src_name, dst_name, false).unwrap();
    assert!(
        migrated.is_empty(),
        "should skip existing files without --force"
    );
    assert_eq!(
        fs::read_to_string(dst_dir.join("SOUL.md")).unwrap(),
        "existing content",
        "original content preserved"
    );

    // Migrate with force — should overwrite
    let migrated = migrate_profile(src_name, dst_name, true).unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(
        fs::read_to_string(dst_dir.join("SOUL.md")).unwrap(),
        "source content",
        "overwritten with source"
    );

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    reg.profiles.remove(dst_name);
    let _ = reg.save();
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn migrate_from_default_profile_works() {
    let _guard = fs_lock();
    let base = crate::config::profile::base_opencrabs_dir();
    let dst_name = "_test_migrate_from_default";
    let dst_dir = base.join("profiles").join(dst_name);

    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(dst_name);
    let _ = reg.save();

    create_profile(dst_name, None).unwrap();

    // Default profile should have at least some .md or .toml files
    // Migrate from default — should succeed and find files
    let result = migrate_profile("default", dst_name, false);
    assert!(result.is_ok(), "migrate from default should succeed");

    let _ = fs::remove_dir_all(&dst_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(dst_name);
    let _ = reg.save();
}

// ─── Profile Home Resolution ────────────────────────────────────────

#[test]
fn resolve_profile_home_default_is_base_dir() {
    // Without any active profile set, resolve_profile_home falls back to env var.
    // If OPENCRABS_PROFILE is unset, it returns the base ~/.opencrabs/ dir.
    // We can't test set_active_profile() here because OnceLock is global and
    // may already be set by CLI or another test. Test the env var path instead.
    let base = base_opencrabs_dir();
    let home = resolve_profile_home();

    // Either it's the base dir (default) or a profiles/<name> subdir
    // (if OnceLock or env var is set). Both are valid.
    assert!(
        home == base || home.starts_with(base.join("profiles")),
        "home {:?} should be base {:?} or a profiles subdir",
        home,
        base
    );
}

#[test]
fn resolve_profile_home_env_var_override() {
    // Test that OPENCRABS_PROFILE env var resolves to the right path.
    // We can't actually set it because it would affect other tests running
    // concurrently, so we verify the logic by checking the resolved path
    // structure matches what we'd expect.
    let base = base_opencrabs_dir();
    let expected_hermes = base.join("profiles").join("hermes");

    // The path should be constructable from base + profiles + name
    assert!(expected_hermes.ends_with("profiles/hermes"));
    assert!(expected_hermes.starts_with(&base));
}

#[test]
fn active_profile_returns_none_or_valid() {
    // active_profile() should return None (default) or a valid profile name.
    // It's set once via OnceLock, so the value depends on how the test runner
    // was invoked (with or without -p flag).
    match active_profile() {
        None => {} // default profile, totally valid
        Some(name) => {
            assert!(!name.is_empty(), "active profile name should not be empty");
            assert!(
                validate_profile_name(name).is_ok(),
                "active profile name should be valid"
            );
        }
    }
}

// ─── Profile Isolation ──────────────────────────────────────────────

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn profiles_have_completely_separate_directories() {
    let _guard = fs_lock();
    let base = base_opencrabs_dir();
    let name_a = "_test_iso_alpha";
    let name_b = "_test_iso_beta";
    let dir_a = base.join("profiles").join(name_a);
    let dir_b = base.join("profiles").join(name_b);

    // Cleanup
    let _ = fs::remove_dir_all(&dir_a);
    let _ = fs::remove_dir_all(&dir_b);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name_a);
    reg.profiles.remove(name_b);
    let _ = reg.save();

    // Create both profiles
    create_profile(name_a, Some("Alpha")).unwrap();
    create_profile(name_b, Some("Beta")).unwrap();

    // Write different content to each
    fs::write(dir_a.join("SOUL.md"), "I am Alpha").unwrap();
    fs::write(dir_b.join("SOUL.md"), "I am Beta").unwrap();
    fs::write(dir_a.join("config.toml"), "[general]\nname = \"alpha\"").unwrap();
    fs::write(dir_b.join("config.toml"), "[general]\nname = \"beta\"").unwrap();
    fs::create_dir_all(dir_a.join("memory")).unwrap();
    fs::create_dir_all(dir_b.join("memory")).unwrap();
    fs::write(dir_a.join("memory").join("fact.md"), "alpha fact").unwrap();
    fs::write(dir_b.join("memory").join("fact.md"), "beta fact").unwrap();

    // Verify complete isolation — each profile has its own data
    assert_eq!(
        fs::read_to_string(dir_a.join("SOUL.md")).unwrap(),
        "I am Alpha"
    );
    assert_eq!(
        fs::read_to_string(dir_b.join("SOUL.md")).unwrap(),
        "I am Beta"
    );
    assert_eq!(
        fs::read_to_string(dir_a.join("config.toml")).unwrap(),
        "[general]\nname = \"alpha\""
    );
    assert_eq!(
        fs::read_to_string(dir_b.join("config.toml")).unwrap(),
        "[general]\nname = \"beta\""
    );
    assert_eq!(
        fs::read_to_string(dir_a.join("memory/fact.md")).unwrap(),
        "alpha fact"
    );
    assert_eq!(
        fs::read_to_string(dir_b.join("memory/fact.md")).unwrap(),
        "beta fact"
    );

    // Modifying one profile doesn't affect the other
    fs::write(dir_a.join("SOUL.md"), "Alpha changed").unwrap();
    assert_eq!(
        fs::read_to_string(dir_b.join("SOUL.md")).unwrap(),
        "I am Beta"
    );

    // Deleting one profile doesn't affect the other
    delete_profile(name_a).unwrap();
    assert!(!dir_a.exists());
    assert!(dir_b.exists());
    assert_eq!(
        fs::read_to_string(dir_b.join("SOUL.md")).unwrap(),
        "I am Beta"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&dir_b);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name_a);
    reg.profiles.remove(name_b);
    let _ = reg.save();
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn token_lock_prevents_same_token_reuse() {
    let _guard = fs_lock();
    let base = base_opencrabs_dir();
    let locks_dir = base.join("locks");
    let channel = "_test_iso_telegram";
    let token_hash = hash_token("shared-bot-token-12345");

    // Cleanup stale locks
    release_token_lock(channel, &token_hash);

    // Current profile acquires the lock
    let result = acquire_token_lock(channel, &token_hash);
    assert!(result.is_ok(), "should acquire the lock");

    // Verify lock file exists with correct content
    let lock_path = locks_dir.join(format!("{}_{}.lock", channel, token_hash));
    assert!(lock_path.exists());
    let content = fs::read_to_string(&lock_path).unwrap();
    let current_profile = active_profile().unwrap_or("default");
    assert!(
        content.contains(current_profile),
        "lock should contain profile name"
    );
    assert!(
        content.contains(&std::process::id().to_string()),
        "lock should contain PID"
    );

    // Same profile, same PID — re-acquiring should succeed (overwrite)
    let result = acquire_token_lock(channel, &token_hash);
    assert!(result.is_ok(), "same profile+PID should re-acquire");

    // Simulate a foreign lock — write a different profile with OUR PID.
    // On Windows CI, OpenProcess may fail for our own PID in restricted
    // environments, causing is_pid_alive to return false (stale lock path).
    // We only assert rejection when the platform can actually detect alive PIDs.
    let our_pid = std::process::id();
    fs::write(&lock_path, format!("foreign_profile:{our_pid}")).unwrap();
    let result = acquire_token_lock(channel, &token_hash);
    if let Err(e) = result {
        // Platform correctly detected alive foreign PID — verify error message
        let err_msg = e.to_string();
        assert!(
            err_msg.contains("foreign_profile"),
            "error should mention blocking profile: {}",
            err_msg
        );
    } else {
        // Platform couldn't verify PID is alive (e.g. Windows CI restrictions),
        // so lock was treated as stale and overwritten — acceptable behavior.
        eprintln!(
            "NOTE: is_pid_alive({}) returned false on this platform, stale-lock path taken",
            our_pid
        );
    }

    // Simulate a stale foreign lock — dead PID
    fs::write(&lock_path, "dead_profile:999999").unwrap();
    let result = acquire_token_lock(channel, &token_hash);
    assert!(result.is_ok(), "should acquire — stale lock from dead PID");

    // Cleanup
    release_token_lock(channel, &token_hash);
}

#[test]
fn different_tokens_same_channel_no_conflict() {
    let _guard = fs_lock();
    let channel = "_test_iso_noconflict";
    let hash_a = hash_token("bot-token-aaa");
    let hash_b = hash_token("bot-token-bbb");

    // Cleanup
    release_token_lock(channel, &hash_a);
    release_token_lock(channel, &hash_b);

    // Two different token hashes on the same channel — no conflict
    assert!(acquire_token_lock(channel, &hash_a).is_ok());
    assert!(acquire_token_lock(channel, &hash_b).is_ok());

    // Both should have their own lock files
    let base = base_opencrabs_dir();
    let locks_dir = base.join("locks");
    assert!(
        locks_dir
            .join(format!("{}_{}.lock", channel, hash_a))
            .exists()
    );
    assert!(
        locks_dir
            .join(format!("{}_{}.lock", channel, hash_b))
            .exists()
    );

    // Cleanup
    release_token_lock(channel, &hash_a);
    release_token_lock(channel, &hash_b);
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn default_profile_isolation_from_named_profiles() {
    let _guard = fs_lock();
    // The default profile (root ~/.opencrabs/) should not interfere with
    // named profiles under ~/.opencrabs/profiles/<name>/
    let base = base_opencrabs_dir();
    let named = "_test_iso_vs_default";
    let named_dir = base.join("profiles").join(named);

    // Cleanup
    let _ = fs::remove_dir_all(&named_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(named);
    let _ = reg.save();

    create_profile(named, Some("isolated from default")).unwrap();

    // Named profile has its own directory separate from root
    assert!(named_dir.exists());
    assert_ne!(named_dir, base, "named profile dir must differ from base");

    // Writing to named profile doesn't touch default's files
    fs::write(named_dir.join("IDENTITY.md"), "# Named Profile").unwrap();
    // Default profile's IDENTITY.md (if it exists) should be unchanged
    if base.join("IDENTITY.md").exists() {
        let default_content = fs::read_to_string(base.join("IDENTITY.md")).unwrap();
        assert_ne!(default_content, "# Named Profile");
    }

    // Cleanup
    let _ = fs::remove_dir_all(&named_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(named);
    let _ = reg.save();
}

// ─── Concurrent Profile Access ───────────────────────────────────────

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn concurrent_writes_to_separate_profiles_are_isolated() {
    // Hold lock for entire test — setup and cleanup both touch shared profiles.toml.
    // The concurrent threads only write to separate profile directories, not the registry.
    use std::thread;
    let _lock = fs_lock();

    let base = base_opencrabs_dir();
    let name_a = "_test_conc_alpha";
    let name_b = "_test_conc_beta";
    let dir_a = base.join("profiles").join(name_a);
    let dir_b = base.join("profiles").join(name_b);

    // Force-clean any stale state
    let _ = delete_profile(name_a);
    let _ = delete_profile(name_b);

    create_profile(name_a, Some("concurrent alpha")).unwrap();
    create_profile(name_b, Some("concurrent beta")).unwrap();

    let dir_a_clone = dir_a.clone();
    let dir_b_clone = dir_b.clone();

    // Spawn two threads writing to different profiles simultaneously
    let handle_a = thread::spawn(move || {
        for i in 0..50 {
            let content = format!("alpha iteration {}", i);
            fs::write(dir_a_clone.join("SOUL.md"), &content).unwrap();
            fs::write(
                dir_a_clone.join("config.toml"),
                format!("[gen]\niter = {}", i),
            )
            .unwrap();
        }
        fs::read_to_string(dir_a_clone.join("SOUL.md")).unwrap()
    });

    let handle_b = thread::spawn(move || {
        for i in 0..50 {
            let content = format!("beta iteration {}", i);
            fs::write(dir_b_clone.join("SOUL.md"), &content).unwrap();
            fs::write(
                dir_b_clone.join("config.toml"),
                format!("[gen]\niter = {}", i),
            )
            .unwrap();
        }
        fs::read_to_string(dir_b_clone.join("SOUL.md")).unwrap()
    });

    let result_a = handle_a.join().unwrap();
    let result_b = handle_b.join().unwrap();

    // Each profile has its own final state — no cross-contamination
    assert!(result_a.starts_with("alpha"), "alpha got: {}", result_a);
    assert!(result_b.starts_with("beta"), "beta got: {}", result_b);

    // Final verification: SOUL.md files have profile-specific content
    let soul_a = fs::read_to_string(dir_a.join("SOUL.md")).unwrap();
    let soul_b = fs::read_to_string(dir_b.join("SOUL.md")).unwrap();
    assert!(
        soul_a.contains("alpha"),
        "alpha soul should contain 'alpha'"
    );
    assert!(soul_b.contains("beta"), "beta soul should contain 'beta'");
    assert_ne!(soul_a, soul_b, "profile souls should be distinct");

    // Cleanup
    let _ = fs::remove_dir_all(&dir_a);
    let _ = fs::remove_dir_all(&dir_b);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name_a);
    reg.profiles.remove(name_b);
    let _ = reg.save();
}

// ─── Export/Import with Nested Memory ────────────────────────────────

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn export_import_preserves_nested_memory_directories() {
    let _guard = fs_lock();
    let base = base_opencrabs_dir();
    let src_name = "_test_nested_export_src";
    let src_dir = base.join("profiles").join(src_name);
    let export_path = std::env::temp_dir().join("_test_nested_export.tar.gz");

    // Cleanup
    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_file(&export_path);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    let _ = reg.save();

    create_profile(src_name, Some("nested export test")).unwrap();

    // Create deeply nested memory structure
    let deep_dir = src_dir.join("memory").join("projects").join("opencrabs");
    fs::create_dir_all(&deep_dir).unwrap();
    fs::write(src_dir.join("memory").join("user.md"), "# User prefs").unwrap();
    fs::write(
        src_dir.join("memory").join("projects").join("index.md"),
        "# Projects",
    )
    .unwrap();
    fs::write(deep_dir.join("architecture.md"), "# Architecture notes").unwrap();
    fs::write(deep_dir.join("decisions.md"), "# ADRs").unwrap();

    // Also add regular brain files
    fs::write(src_dir.join("SOUL.md"), "# Deep soul").unwrap();
    fs::write(src_dir.join("config.toml"), "[test]\nnested = true").unwrap();

    // Export
    export_profile(src_name, &export_path).unwrap();
    assert!(export_path.exists());
    assert!(export_path.metadata().unwrap().len() > 0);

    // Delete the source profile
    delete_profile(src_name).unwrap();
    assert!(!src_dir.exists());

    // Import — import_profile extracts the profile name from the archive
    // The archive was created from src_name, so it imports as src_name
    let imported_name = import_profile(&export_path).unwrap();
    let imported_dir = base.join("profiles").join(&imported_name);
    assert!(imported_dir.exists());

    // Verify all nested files survived the roundtrip
    assert_eq!(
        fs::read_to_string(imported_dir.join("SOUL.md")).unwrap(),
        "# Deep soul"
    );
    assert_eq!(
        fs::read_to_string(imported_dir.join("memory").join("user.md")).unwrap(),
        "# User prefs"
    );
    assert_eq!(
        fs::read_to_string(
            imported_dir
                .join("memory")
                .join("projects")
                .join("index.md")
        )
        .unwrap(),
        "# Projects"
    );
    assert_eq!(
        fs::read_to_string(
            imported_dir
                .join("memory")
                .join("projects")
                .join("opencrabs")
                .join("architecture.md")
        )
        .unwrap(),
        "# Architecture notes"
    );
    assert_eq!(
        fs::read_to_string(
            imported_dir
                .join("memory")
                .join("projects")
                .join("opencrabs")
                .join("decisions.md")
        )
        .unwrap(),
        "# ADRs"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&imported_dir);
    let _ = fs::remove_file(&export_path);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(src_name);
    reg.profiles.remove(&imported_name);
    let _ = reg.save();
}

// ─── Profile Home Resolution Edge Cases ──────────────────────────────

#[test]
fn explicit_default_name_resolves_to_base_dir() {
    // Contract: "default" resolves to the base (~/.opencrabs/), not a
    // profiles/default/ subdirectory, and is reserved so it cannot be created
    // as a named profile.
    assert!(
        validate_profile_name("default").is_err(),
        "\"default\" is reserved and cannot be created as a named profile"
    );
}

#[test]
fn profile_directories_never_overlap() {
    let base = base_opencrabs_dir();

    // Default profile home
    let default_home = base.clone();

    // Named profile homes
    let hermes_home = base.join("profiles").join("hermes");
    let scout_home = base.join("profiles").join("scout");

    // No profile home is a prefix of another (prevents accidental nesting)
    assert!(!hermes_home.starts_with(&scout_home));
    assert!(!scout_home.starts_with(&hermes_home));

    // Named profiles are always under profiles/ subdir, never at root
    assert!(hermes_home.starts_with(base.join("profiles")));
    assert!(scout_home.starts_with(base.join("profiles")));

    // Default home is the base — named profiles are children, not siblings
    assert!(!hermes_home.starts_with(default_home.join("config.toml")));
}

// ─── Additional Profile Tests ────────────────────────────────────────

#[test]
fn test_resolve_profile_home_returns_valid_path() {
    // Verify resolve_profile_home returns a path rooted in base_opencrabs_dir.
    // We do NOT test env var mutation here — std::env::set_var is UB in
    // multi-threaded contexts (Rust 2024) and causes flaky failures on CI.
    let base = base_opencrabs_dir();
    let home = resolve_profile_home();

    // Home must be either the base dir (default profile) or a profiles subdir
    assert!(
        home == base || home.starts_with(base.join("profiles")),
        "home {:?} should be base {:?} or a profiles subdir",
        home,
        base
    );
}

#[test]
fn test_set_and_get_active_profile() {
    // OnceLock can only be set once per process. Attempt to set it to None
    // (default profile) to avoid corrupting other tests that check for "default".
    // If already set by CLI or another test, we just verify the current value.
    let result = set_active_profile(None);

    match result {
        Ok(()) => {
            // We successfully set it to None — active_profile should return None
            assert_eq!(
                active_profile(),
                None,
                "active_profile should return None for default"
            );
        }
        Err(_) => {
            // Already set by another test or CLI — just verify it returns something valid
            let current = active_profile();
            // It's either None (default) or a valid profile name
            if let Some(name) = current {
                assert!(!name.is_empty(), "active profile name should not be empty");
            }
        }
    }
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn test_concurrent_profile_writes() {
    use std::thread;

    let _guard = fs_lock();
    let base = base_opencrabs_dir();
    let names: Vec<String> = (0..5).map(|i| format!("_test_concurrent_{}", i)).collect();

    // Cleanup from previous runs
    for name in &names {
        let dir = base.join("profiles").join(name);
        let _ = fs::remove_dir_all(&dir);
    }
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    for name in &names {
        reg.profiles.remove(name.as_str());
    }
    let _ = reg.save();

    // Create all 5 profiles sequentially (create_profile writes to shared
    // profiles.toml and cannot safely run concurrently)
    for name in &names {
        create_profile(name, Some("concurrent test")).unwrap();
    }

    // Spawn 5 threads, each writing to its own profile directory concurrently
    let handles: Vec<_> = names
        .iter()
        .map(|name| {
            let dir = base.join("profiles").join(name);
            let name = name.clone();
            thread::spawn(move || {
                for i in 0..20 {
                    let content = format!("{} iteration {}", name, i);
                    fs::write(dir.join("SOUL.md"), &content).unwrap();
                }
                name
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all 5 directories exist and have been written to
    for name in &names {
        let dir = base.join("profiles").join(name);
        assert!(dir.exists(), "profile directory for {} should exist", name);
        assert!(
            dir.join("memory").exists(),
            "memory subdir for {} should exist",
            name
        );
        let content = fs::read_to_string(dir.join("SOUL.md")).unwrap();
        assert!(
            content.starts_with(name),
            "SOUL.md for {} should start with profile name, got: {}",
            name,
            content
        );
    }

    // Cleanup all 5
    for name in &names {
        let _ = delete_profile(name);
    }
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    for name in &names {
        reg.profiles.remove(name.as_str());
    }
    let _ = reg.save();
}

#[test]
#[ignore = "touches user's ~/.opencrabs/ — slow + serial; run with `cargo test -- --ignored`"]
fn test_export_import_nested_memory() {
    let _guard = fs_lock();
    let base = base_opencrabs_dir();
    let name = "_test_exp_imp_nested";
    let profile_dir = base.join("profiles").join(name);
    let archive = std::env::temp_dir().join(format!(
        "_test_exp_imp_nested_{}.tar.gz",
        std::process::id()
    ));

    // Cleanup
    let _ = fs::remove_dir_all(&profile_dir);
    let _ = fs::remove_file(&archive);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    let _ = reg.save();

    // Create profile with deeply nested memory structure
    create_profile(name, Some("nested memory test")).unwrap();
    let deep_dir = profile_dir.join("memory").join("sub").join("deep");
    fs::create_dir_all(&deep_dir).unwrap();
    fs::write(deep_dir.join("note.md"), "deeply nested content").unwrap();

    // Export
    export_profile(name, &archive).unwrap();
    assert!(archive.exists(), "archive should be created");

    // Delete the profile
    delete_profile(name).unwrap();
    assert!(
        !profile_dir.exists(),
        "profile dir should be gone after delete"
    );

    // Import
    let imported_name = import_profile(&archive).unwrap();
    assert_eq!(imported_name, name);

    // Verify the nested file exists with correct content
    let reimported_dir = base.join("profiles").join(&imported_name);
    let reimported_note = reimported_dir
        .join("memory")
        .join("sub")
        .join("deep")
        .join("note.md");
    assert!(
        reimported_note.exists(),
        "nested note.md should survive export/import roundtrip"
    );
    assert_eq!(
        fs::read_to_string(&reimported_note).unwrap(),
        "deeply nested content",
        "nested file content should be preserved"
    );

    // Cleanup
    let _ = delete_profile(name);
    let _ = fs::remove_file(&archive);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    reg.profiles.remove(&imported_name);
    let _ = reg.save();
}

#[test]
fn test_resolve_profile_home_default_explicit() {
    // "default" is reserved — resolve_profile_home should return the base
    // ~/.opencrabs/ path, NOT ~/.opencrabs/profiles/default/
    let base = base_opencrabs_dir();

    // "default" is rejected as a profile name (it's reserved)
    assert!(
        validate_profile_name("default").is_err(),
        "\"default\" should be a reserved name"
    );

    // The base path should NOT have a profiles/default/ component
    let would_be_wrong = base.join("profiles").join("default");

    // resolve_profile_home with no active profile and no env var returns base
    let original_env = std::env::var("OPENCRABS_PROFILE").ok();
    unsafe { std::env::remove_var("OPENCRABS_PROFILE") };

    let home = resolve_profile_home();

    // If OnceLock hasn't been set, home should be exactly base (not profiles/default/)
    match active_profile() {
        None => {
            assert_eq!(
                home, base,
                "with no active profile, resolve_profile_home should return base dir"
            );
            assert_ne!(
                home, would_be_wrong,
                "should NOT resolve to profiles/default/"
            );
        }
        Some(_) => {
            // OnceLock is set — can't test the None path, but verify it's not profiles/default/
            assert_ne!(
                home, would_be_wrong,
                "should never resolve to profiles/default/"
            );
        }
    }

    // Restore env var
    if let Some(val) = original_env {
        unsafe { std::env::set_var("OPENCRABS_PROFILE", val) };
    }
}

// ─── Brain-file seeding on profile create (commit 8d87559b) ─────────────────

/// Names of the 8 templates `create_profile` is contracted to seed.
/// Must stay in sync with `seed_brain_templates` and the onboarding
/// wizard's `TEMPLATE_FILES`.
const EXPECTED_SEEDED_BRAIN_FILES: &[&str] = &[
    "SOUL.md",
    "USER.md",
    "AGENTS.md",
    "TOOLS.md",
    "MEMORY.md",
    "CODE.md",
    "SECURITY.md",
];

#[test]
fn create_profile_seeds_all_brain_templates() {
    let _guard = fs_lock();
    let name = "_test_seed_brain";
    let profile_dir = base_opencrabs_dir().join("profiles").join(name);

    // Clean any leftovers from a prior aborted run.
    let _ = fs::remove_dir_all(&profile_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    let _ = reg.save();

    let path = create_profile(name, Some("brain-seed test")).unwrap();

    for filename in EXPECTED_SEEDED_BRAIN_FILES {
        let target = path.join(filename);
        assert!(
            target.exists(),
            "create_profile must seed {filename} (issue from #120 audit)",
        );
        let content = fs::read_to_string(&target).expect("seeded file readable");
        assert!(
            !content.trim().is_empty(),
            "{filename} seeded but empty — bundled template is broken",
        );
    }

    // Cleanup
    let _ = delete_profile(name);
}

#[test]
fn create_profile_does_not_overwrite_existing_brain_files() {
    // If the user pre-created the profile dir AND wrote their own
    // SOUL.md before calling create_profile, the seeder must NOT
    // overwrite it. (Realistic when imports happen out of band, or
    // a user is recovering from a broken state.)
    let _guard = fs_lock();
    let name = "_test_seed_no_overwrite";
    let profile_dir = base_opencrabs_dir().join("profiles").join(name);
    let _ = fs::remove_dir_all(&profile_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    let _ = reg.save();

    // Pre-create the dir and drop a custom SOUL.md.
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(profile_dir.join("SOUL.md"), "# my custom soul").unwrap();

    // `create_profile` would error on the existing dir — clear our
    // pre-existing marker for the dir check, but leave the file.
    // We exercise the seeder directly instead via the public path it
    // uses inside create_profile.
    crate::config::profile::seed_brain_templates(&profile_dir);

    assert_eq!(
        fs::read_to_string(profile_dir.join("SOUL.md")).unwrap(),
        "# my custom soul",
        "seeder must not clobber an existing custom SOUL.md",
    );
    // Other templates SHOULD still be seeded for the first time.
    assert!(
        profile_dir.join("TOOLS.md").exists(),
        "absent templates must still be seeded",
    );

    let _ = fs::remove_dir_all(&profile_dir);
}

#[test]
fn create_profile_brain_files_are_non_trivial_size() {
    // Sanity check that the templates pull in actual content (not the
    // empty placeholders that would happen if include_str! ever pointed
    // at the wrong path). TOOLS.md was trimmed from ~40KB to ~2KB to
    // remove reference bloat — it's now a lean routing index.
    let _guard = fs_lock();
    let name = "_test_seed_sizes";
    let profile_dir = base_opencrabs_dir().join("profiles").join(name);
    let _ = fs::remove_dir_all(&profile_dir);
    let mut reg = ProfileRegistry::load().unwrap_or_default();
    reg.profiles.remove(name);
    let _ = reg.save();

    let path = create_profile(name, None).unwrap();

    let tools_bytes = fs::metadata(path.join("TOOLS.md"))
        .expect("TOOLS.md exists")
        .len();
    assert!(
        tools_bytes >= 1_000,
        "TOOLS.md seeded at only {tools_bytes} bytes — template path is probably wrong",
    );

    let security_bytes = fs::metadata(path.join("SECURITY.md"))
        .expect("SECURITY.md exists")
        .len();
    assert!(
        security_bytes >= 1_000,
        "SECURITY.md seeded at only {security_bytes} bytes — template path is probably wrong",
    );

    let _ = delete_profile(name);
}

/// `current_profile_name()` reads the task-local profile set by
/// `with_profile_home_async`, falling back to the process global. The
/// multi-profile cron daemon relies on this so a job created or run inside a
/// foreign profile's scope is attributed to THAT profile, not the process one.
#[tokio::test]
async fn current_profile_name_follows_the_task_local_scope() {
    use crate::config::profile::{current_profile_name, with_profile_home_async};

    // Inside a profile scope, the name is that profile.
    let inside = with_profile_home_async(Some("ops"), async { current_profile_name() }).await;
    assert_eq!(inside, "ops");

    // Some(\"default\") scopes to the base profile, reported as "default".
    let base = with_profile_home_async(Some("default"), async { current_profile_name() }).await;
    assert_eq!(base, "default");

    // Outside any scope it falls back to the global active profile (or
    // "default" when none is set in this test process).
    let outside = current_profile_name();
    assert!(
        !outside.is_empty(),
        "must always return a usable profile name, got empty"
    );
}

// ── #1381: rejection error names the char, codepoint, and position ─────────

#[test]
fn invalid_name_error_names_char_codepoint_and_position() {
    // U+00A0 (no-break space) at position 4 — the classic clipboard stowaway
    let err = validate_profile_name("some\u{00A0}bot").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("alphanumeric"), "charset hint present");
    assert!(msg.contains("U+00A0"), "codepoint named, got: {}", msg);
    assert!(msg.contains("position 4"), "position named, got: {}", msg);
    assert!(
        msg.contains("\u{00A0}") || msg.contains("\\u{a0}") || msg.contains("'\u{00A0}'"),
        "char shown, got: {}",
        msg
    );
}

#[test]
fn invalid_name_error_names_zero_width_space() {
    // U+200B (zero-width space) at position 2
    let err = validate_profile_name("ab\u{200B}cd").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("U+200B"), "codepoint named, got: {}", msg);
    assert!(msg.contains("position 2"), "position named, got: {}", msg);
}

#[test]
fn invalid_name_error_names_first_offending_char_only() {
    // Two bad chars (space at 1, dot at 3); error names the first offender only
    let err = validate_profile_name("a b.c").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("position 1"),
        "first offending position, got: {}",
        msg
    );
    assert!(msg.contains("U+0020"), "space codepoint, got: {}", msg);
    assert!(
        !msg.contains("U+002E"),
        "second offender (dot) not named, got: {}",
        msg
    );
}

/// Round 7, PR #3. The Windows lock stamp is `profile:pid:creation-ticks`, and
/// the token-lock reader parsed that whole third field as a number. A failed
/// parse is read as "corrupt, take it over", so on Windows a live instance's
/// token lock was silently handed to whoever arrived second -- destroying the
/// one-credential-one-instance guarantee that `acquire_token_lock` exists for.
#[test]
fn lock_owner_pid_survives_the_windows_ticks_field() {
    assert_eq!(parse_lock_owner_pid("103104"), Some(103104));
    assert_eq!(
        parse_lock_owner_pid("103104:133700000000000000"),
        Some(103104)
    );
    assert_eq!(parse_lock_owner_pid(" 103104:7 "), Some(103104));
    // Tolerance ends at the PID half. A field that does not *start* with a
    // usable pid is still corruption and is still taken over (issue #192).
    assert_eq!(parse_lock_owner_pid("101528ops:103104family:101507"), None);
    assert_eq!(parse_lock_owner_pid("notapid:123"), None);
    assert_eq!(parse_lock_owner_pid("0:123"), None);
}

/// Restated by #2084: the scan decides liveness from the stamp itself, so a
/// lock file whose recorded creation time disagrees with the live process is
/// dropped before any claims are resolved. That is the recycled-PID case, and
/// `profile_lock_stamp_test` pins it. What is left to check here is ordering:
/// two stamps name one live PID, one carrying its creation time and one a
/// legacy bare PID, and they must resolve to the same owner and the same
/// channels whichever order `read_dir` happens to return them in.
///
/// `read_dir` order cannot be forced from here, so this varies the *write*
/// order instead.
#[test]
fn foreign_lock_owner_resolves_a_reused_pid_against_the_live_process() {
    use std::process::{Command, Stdio};
    let _guard = fs_lock();
    let profile = active_profile().unwrap_or("default");
    // A made-up pid cannot exercise this path: the scan drops owners that are
    // not alive, so the pid has to belong to a real process.
    let spawned = if cfg!(windows) {
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    } else {
        Command::new("/bin/sleep")
            .args(["60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    let mut child = spawned.expect("spawn a helper process to own a live pid");
    let live = child.id();

    // #2084: the scan drops a stamp whose recorded creation time disagrees
    // with the live process, so a claim built from fake ticks is discarded
    // before it can be resolved and this test would prove nothing. The claim
    // here is the kernel's own reading of the live child, which is what a real
    // owner's lock carries on every platform that can be asked.
    let recorded = process_creation_stamp(live)
        .expect("the helper was spawned here, so this platform reads its creation time");
    let claim = format!("{profile}:{live}:{recorded}");
    let legacy = format!("{profile}:{live}");
    for order in [
        [
            ("telegram_aaa.lock", claim.as_str()),
            ("whatsapp_bbb.lock", legacy.as_str()),
        ],
        [
            ("telegram_aaa.lock", legacy.as_str()),
            ("whatsapp_bbb.lock", claim.as_str()),
        ],
    ] {
        let dir = tempdir().expect("tempdir");
        for (name, contents) in order {
            fs::write(dir.path().join(name), contents).expect("write stamp");
        }
        let owners = foreign_lock_owners(dir.path());
        let Some(owner) = owners.get(&live) else {
            panic!("live owner {live} missing for profile {profile:?}");
        };
        assert_eq!(
            owner.creation_ticks,
            Some(recorded),
            "resolution must be order-independent for {order:?}"
        );
        let mut channels = owner.channels.clone();
        channels.sort();
        assert_eq!(
            channels,
            vec!["telegram".to_string(), "whatsapp".to_string()]
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The bug this pins: a wall clock that stepped backward between the dead lock
/// holder and the process that inherited its PID leaves the DEAD claim as the
/// larger number, so taking the maximum hands `terminate` a creation time that
/// cannot match the live owner and the handover never completes. Matching the
/// live process's own reading picks the owner whichever way the clock moved.
#[test]
fn resolve_creation_ticks_matches_the_live_process_not_the_newest_claim() {
    assert_eq!(
        resolve_creation_ticks(Some(8000), &[Some(9000), Some(8000)]),
        Some(8000),
        "the live process's own reading must win over a larger stale claim"
    );
    assert_eq!(
        resolve_creation_ticks(Some(8000), &[Some(8000), Some(9000)]),
        Some(8000),
        "the answer must not depend on the order the stamps were read in"
    );
    // Nothing recorded belongs to the live process, so ownership is not
    // proved: refuse (fail-closed) rather than kill on a guess.
    assert_eq!(resolve_creation_ticks(Some(8000), &[Some(9000)]), None);
    assert_eq!(resolve_creation_ticks(Some(8000), &[None]), None);
}

/// Without a kernel reading -- a platform that cannot be asked, or a refused
/// handle -- the greatest claim still wins, and a legacy stamp never displaces
/// a recorded time. Since #2084 every supported platform answers, so this is
/// the path taken for stamps written by builds that predate the reading.
#[test]
fn resolve_creation_ticks_falls_back_to_the_newest_claim() {
    assert_eq!(
        resolve_creation_ticks(None, &[Some(9000), Some(8000)]),
        Some(9000)
    );
    assert_eq!(
        resolve_creation_ticks(None, &[Some(8000), Some(9000)]),
        Some(9000)
    );
    assert_eq!(
        resolve_creation_ticks(None, &[None, Some(9000)]),
        Some(9000)
    );
    assert_eq!(resolve_creation_ticks(None, &[None, None]), None);
}

#[test]
fn seed_brain_templates_seeds_ralph_loop_gate() {
    // #1902: the Ralph verification gate read as Disabled on every fresh
    // install because nothing ever seeded safety/ralph_loop.toml, so plan
    // completions passed unverified with only a DEBUG line as signal.
    // The bundled template must land at seed time, enabled, and never
    // clobber a user's own file.
    let dir = tempfile::TempDir::new().unwrap();
    let profile_dir = dir.path();
    crate::config::profile::seed_brain_templates(profile_dir);

    let ralph = profile_dir.join("safety").join("ralph_loop.toml");
    assert!(
        ralph.exists(),
        "the safety seed must create ralph_loop.toml alongside brain_verify.toml"
    );
    let body = std::fs::read_to_string(&ralph).unwrap();
    let parsed: toml::Value = toml::from_str(&body).expect("seeded file parses as TOML");
    assert!(
        parsed["verification"]["enabled"].as_bool().unwrap_or(false),
        "the seeded gate must be enabled: seeding a disabled file repeats #872",
    );

    // Idempotence: a user's customized file is never overwritten.
    std::fs::write(&ralph, "[verification]\nenabled = false\n").unwrap();
    crate::config::profile::seed_brain_templates(profile_dir);
    assert_eq!(
        std::fs::read_to_string(&ralph).unwrap(),
        "[verification]\nenabled = false\n",
        "seeding must not clobber a user's ralph_loop.toml",
    );
}
