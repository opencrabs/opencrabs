use crate::brain::provider::opencrabs_store::{StoredInstall, load, save};
use crate::config::profile::with_home_override;

fn in_temp_home(f: impl FnOnce()) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    with_home_override(opencrabs, f);
}

fn keys_text() -> String {
    std::fs::read_to_string(crate::config::keys_path()).expect("keys.toml written")
}

fn install() -> StoredInstall {
    StoredInstall {
        install_id: "abcd1234abcd1234abcd1234abcd1234".to_string(),
        token: "tok-live-123".to_string(),
    }
}

#[test]
fn debug_output_of_stored_install_hides_the_token() {
    let token = "fedcba9876543210".repeat(4);
    let stored = StoredInstall {
        install_id: "0123456789abcdef0123456789abcdef".to_string(),
        token: token.clone(),
    };
    let shown = format!("{stored:?}");
    assert!(!shown.contains(&token), "token leaked into Debug: {shown}");
    assert!(shown.contains("<redacted>"), "no redaction marker: {shown}");
    assert!(
        shown.contains("0123456789abcdef0123456789abcdef"),
        "install ID dropped: {shown}"
    );
}

#[test]
fn fresh_home_has_no_stored_install() {
    in_temp_home(|| {
        assert_eq!(load(), None);
    });
}

#[test]
fn save_then_load_round_trips_id_and_token() {
    in_temp_home(|| {
        save(&install()).expect("save");
        assert_eq!(load(), Some(install()));
    });
}

#[test]
fn id_and_token_land_in_the_opencrabs_section() {
    in_temp_home(|| {
        save(&install()).expect("save");
        let text = keys_text();
        assert!(text.contains("[providers.opencrabs]"), "{text}");
        assert!(text.contains("install_id = \"abcd1234abcd1234abcd1234abcd1234\""));
        assert!(text.contains("api_key = \"tok-live-123\""));
    });
}

#[test]
fn save_keeps_other_provider_sections() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.xiaomi]\napi_key = \"xiaomi-key\"\n",
        )
        .expect("seed keys.toml");
        save(&install()).expect("save");
        let text = keys_text();
        assert!(text.contains("xiaomi-key"), "{text}");
        assert_eq!(load(), Some(install()));
    });
}

#[test]
fn saving_a_new_install_replaces_the_old_one() {
    in_temp_home(|| {
        save(&install()).expect("first save");
        let next = StoredInstall {
            install_id: "ffff0000ffff0000ffff0000ffff0000".to_string(),
            token: "tok-new-456".to_string(),
        };
        save(&next).expect("second save");
        assert_eq!(load(), Some(next));
    });
}

#[test]
fn id_without_token_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.opencrabs]\ninstall_id = \"abcd1234abcd1234abcd1234abcd1234\"\n",
        )
        .expect("seed keys.toml");
        assert_eq!(load(), None);
    });
}

#[test]
fn malformed_stored_id_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.opencrabs]\ninstall_id = \"bad id\"\napi_key = \"tok\"\n",
        )
        .expect("seed keys.toml");
        assert_eq!(load(), None);
    });
}

#[test]
fn unparseable_keys_file_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(crate::config::keys_path(), "[providers.opencrabs\n").expect("seed");
        assert_eq!(load(), None);
    });
}

// --- #2198: the identity is not a secret and must not share a parser with one ---

fn seed_keys(text: &str) {
    std::fs::write(crate::config::keys_path(), text).expect("seed keys.toml");
}

#[test]
fn a_corrupt_keys_file_keeps_the_install_id() {
    in_temp_home(|| {
        save(&install()).expect("save");
        // The exact shape from the issue: a hand-edit broke keys.toml. The
        // token is unreadable, the identity is not.
        seed_keys("[providers.opencrabs\ninstall_id = \"abcd");

        assert_eq!(load(), None, "the token really is unreachable");
        assert_eq!(
            crate::brain::provider::opencrabs_store::load_install_id(),
            Some(install().install_id),
            "a broken keys.toml must not cost the identity"
        );
    });
}

#[test]
fn a_legacy_keys_id_is_migrated_into_its_own_file() {
    in_temp_home(|| {
        seed_keys(&format!(
            "[providers.opencrabs]\ninstall_id = \"{}\"\napi_key = \"tok-legacy\"\n",
            install().install_id
        ));

        // The legacy install loads with the token it was seeded with. `Debug`
        // redacts the token, so compare it directly or a mismatch shows up as
        // two identical-looking structs.
        let loaded = load().expect("the legacy install still loads");
        assert_eq!(loaded.install_id, install().install_id);
        assert_eq!(loaded.token, "tok-legacy");
        let migrated =
            std::fs::read_to_string(crate::brain::provider::opencrabs_store::install_id_path())
                .expect("migrated id file");
        assert_eq!(migrated.trim(), install().install_id);

        // The identity no longer depends on keys.toml being readable.
        seed_keys("[providers.opencrabs\n");
        assert_eq!(
            crate::brain::provider::opencrabs_store::load_install_id(),
            Some(install().install_id)
        );
    });
}

#[test]
fn the_id_file_wins_over_a_stale_copy_in_keys() {
    in_temp_home(|| {
        let file_id = "11112222333344445555666677778888";
        crate::brain::provider::opencrabs_store::write_install_id(file_id).expect("id file");
        seed_keys(&format!(
            "[providers.opencrabs]\ninstall_id = \"{}\"\napi_key = \"tok-live-123\"\n",
            install().install_id
        ));

        assert_eq!(
            crate::brain::provider::opencrabs_store::load_install_id(),
            Some(file_id.to_string()),
            "the dedicated file is the identity, keys.toml only keeps a copy"
        );
        let loaded = load().expect("token and identity");
        assert_eq!(loaded.install_id, file_id);
    });
}

#[test]
fn an_invalid_id_file_falls_back_to_the_keys_copy_and_is_rewritten() {
    in_temp_home(|| {
        crate::brain::provider::opencrabs_store::write_install_id("not an id").expect("id file");
        seed_keys(&format!(
            "[providers.opencrabs]\ninstall_id = \"{}\"\napi_key = \"tok-live-123\"\n",
            install().install_id
        ));

        assert_eq!(
            crate::brain::provider::opencrabs_store::load_install_id(),
            Some(install().install_id)
        );
        let rewritten =
            std::fs::read_to_string(crate::brain::provider::opencrabs_store::install_id_path())
                .expect("id file rewritten");
        assert_eq!(rewritten.trim(), install().install_id);
    });
}

#[test]
fn the_identity_survives_a_token_write_that_fails() {
    in_temp_home(|| {
        // A directory where keys.toml belongs makes the token write fail for a
        // reason the test cannot paper over.
        std::fs::create_dir_all(crate::config::keys_path()).expect("block keys.toml");

        let err = save(&install()).expect_err("the token cannot be written");
        assert!(!err.to_string().is_empty());

        assert_eq!(
            crate::brain::provider::opencrabs_store::read_id_file(),
            Some(install().install_id),
            "the ID is written before the token, so losing the token costs a reissue"
        );
    });
}

#[cfg(unix)]
#[test]
fn the_id_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    in_temp_home(|| {
        save(&install()).expect("save");
        let mode = std::fs::metadata(crate::brain::provider::opencrabs_store::install_id_path())
            .expect("id file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "install_id landed at {mode:o}");
    });
}
