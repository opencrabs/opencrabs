//! Removing optional config keys through the GUI settings write path (#2878).
//!
//! Every test uses a temporary home. No test may touch the live config.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::config::profile::with_home_override;

fn test_home(config: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::write(home.join("config.toml"), config).expect("write config");
    std::fs::write(home.join("keys.toml"), "").expect("write empty keys");
    (dir, home)
}

fn config_content(home: &Path) -> String {
    std::fs::read_to_string(home.join("config.toml")).expect("read config")
}

#[test]
fn removing_an_override_restores_the_absent_state_and_preserves_neighbours() {
    let original = concat!(
        "# retained comment\n",
        "[agent]\n",
        "approval_policy = \"ask\"\n",
        "redact_group = true\n",
        "redact_dm = false\n",
    );
    let (_temp, home) = test_home(original);
    with_home_override(home.clone(), || {
        Config::remove_key("agent", "redact_group").expect("remove the override");
        let content = config_content(&home);
        assert!(content.contains("# retained comment"));
        let doc: toml_edit::DocumentMut = content.parse().expect("valid TOML");
        assert!(doc["agent"].get("redact_group").is_none());
        assert_eq!(doc["agent"]["redact_dm"].as_bool(), Some(false));
        assert_eq!(doc["agent"]["approval_policy"].as_str(), Some("ask"));
        let loaded = Config::load().expect("the edited config still loads");
        assert_eq!(loaded.agent.redact_group, None);
        assert_eq!(loaded.agent.redact_dm, Some(false));
    });
}

#[test]
fn removing_an_absent_key_or_section_is_a_byte_preserving_no_op() {
    let original = "[agent]\napproval_policy = \"ask\"\n";
    let (_temp, home) = test_home(original);
    with_home_override(home.clone(), || {
        Config::remove_key("agent", "redact_group").expect("missing key is already absent");
        Config::remove_key("memory", "not_present").expect("missing section is already absent");
        assert_eq!(config_content(&home), original);
    });
}

#[test]
fn a_scalar_where_the_section_should_be_fails_without_touching_the_file() {
    let original = "agent = true\n";
    let (_temp, home) = test_home(original);
    with_home_override(home.clone(), || {
        let error = Config::remove_key("agent", "redact_group")
            .expect_err("a scalar is not a section table");
        assert!(error.to_string().contains("not a table"), "{error}");
        assert_eq!(config_content(&home), original);
    });
}
