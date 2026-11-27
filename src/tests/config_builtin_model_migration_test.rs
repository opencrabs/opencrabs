//! Config migration moves the built-in provider off the legacy chat model (#3203).
//!
//! The proxy publishes `combo` and keeps `deepseek-v4.1-flash` only as an alias
//! for clients built before the rename, so an install that upgrades has to end
//! up sending the new id. The example handles a fresh install; this migration
//! handles the one that already has a `config.toml`.
//!
//! Only the shipped default moves. A config that pinned any other model is the
//! user's choice and stays as written, which is why the second case below
//! exists: a migration that rewrote every `default_model` would silently undo a
//! pick.

use crate::config::Config;
use tempfile::TempDir;

fn write_cfg(contents: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

fn migrated(path: &std::path::Path) -> toml::Value {
    Config::migrate_if_needed(path);
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn moves_the_shipped_default_to_the_combo_id() {
    let (_d, path) = write_cfg(
        "[providers.opencrabs]\nenabled = true\ndefault_model = \"deepseek-v4.1-flash\"\n\
         models = [\"deepseek-v4.1-flash\"]\n",
    );
    let doc = migrated(&path);
    let section = &doc["providers"]["opencrabs"];
    assert_eq!(
        section["default_model"].as_str(),
        Some("combo"),
        "the shipped default moves to the id the proxy publishes"
    );
    assert_eq!(
        section["models"]
            .as_array()
            .map(|m| m.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
        Some(vec!["combo"]),
        "a single-entry list holding only the legacy id moves with it"
    );
}

#[test]
fn leaves_a_pinned_model_alone() {
    let (_d, path) = write_cfg(
        "[providers.opencrabs]\nenabled = true\ndefault_model = \"some-other-model\"\n\
         models = [\"some-other-model\"]\n",
    );
    let doc = migrated(&path);
    let section = &doc["providers"]["opencrabs"];
    assert_eq!(
        section["default_model"].as_str(),
        Some("some-other-model"),
        "a model the user picked is not the shipped default and stays"
    );
    assert_eq!(
        section["models"]
            .as_array()
            .map(|m| m.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
        Some(vec!["some-other-model"])
    );
}
