//! Migration 6 (#3118): the five completion keys collapse into two.
//!
//! `completion_notification_enabled` was a master switch that behaved as a kill
//! switch: it was read before the event key, so `notify_on_turn_failed = true`
//! beside a false master asked for failure banners and got silence. The pack is
//! gone, and the migration has to carry the operator's real behaviour across:
//! the master's value becomes `notify_on_plan_completed` (the surviving
//! completion event, banner and chime), `notify_on_turn_failed` is ANDed with
//! the master so a switch that never fired cannot turn one on, and the per-task
//! and per-turn keys are deleted with no successor.
//!
//! Both passes are exercised: the value pass decides that a rewrite is needed,
//! the `toml_edit` pass is the one that reaches the file. A migration written
//! only on the value pass leaves the dead keys on disk.

use crate::config::Config;
use tempfile::TempDir;

fn migrate(contents: &str) -> (TempDir, toml::Value) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, contents).unwrap();
    Config::migrate_if_needed(&path);
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    (dir, doc)
}

fn agent(doc: &toml::Value) -> &toml::value::Table {
    doc.get("agent")
        .and_then(|a| a.as_table())
        .expect("the [agent] table survives")
}

#[test]
fn the_master_becomes_the_plan_switch_and_the_dead_keys_go() {
    let (_d, doc) = migrate(
        "[agent]\ncompletion_notification_enabled = true\n\
         notify_on_plan_completed = false\n\
         notify_on_task_completed = true\n\
         notify_on_turn_completed = true\n\
         notify_on_turn_failed = true\n",
    );
    let agent = agent(&doc);
    assert_eq!(
        agent.get("notify_on_plan_completed").and_then(|v| v.as_bool()),
        Some(true),
        "the master's value is what the plan switch inherits"
    );
    assert_eq!(
        agent.get("notify_on_turn_failed").and_then(|v| v.as_bool()),
        Some(true),
        "a failure switch under an enabled master keeps firing"
    );
    for dead in [
        "completion_notification_enabled",
        "notify_on_task_completed",
        "notify_on_turn_completed",
    ] {
        assert!(agent.get(dead).is_none(), "{dead} must be deleted");
    }
}

#[test]
fn a_dead_failure_switch_cannot_survive_a_disabled_master() {
    // The master being off means nothing ever fired. Carrying the true across
    // would start raising banners the operator never had.
    let (_d, doc) = migrate(
        "[agent]\ncompletion_notification_enabled = false\n\
         notify_on_turn_failed = true\n",
    );
    let agent = agent(&doc);
    assert_eq!(
        agent.get("notify_on_turn_failed").and_then(|v| v.as_bool()),
        Some(false),
        "the failure switch is ANDed with the master"
    );
    assert!(
        agent.get("notify_on_plan_completed").is_none(),
        "an absent plan switch under a false master stays absent, not written false"
    );
    assert!(
        agent.get("completion_notification_enabled").is_none(),
        "the master is gone"
    );
}

#[test]
fn the_dead_keys_are_removed_even_without_a_master() {
    let (_d, doc) = migrate(
        "[agent]\nnotify_on_task_completed = true\nnotify_on_turn_completed = true\n",
    );
    let agent = agent(&doc);
    assert!(agent.get("notify_on_task_completed").is_none());
    assert!(agent.get("notify_on_turn_completed").is_none());
    assert!(
        agent.get("notify_on_plan_completed").is_none(),
        "no master means no value to carry"
    );
}

#[test]
fn a_config_without_the_pack_is_left_alone() {
    let before = "[agent]\nnotify_on_plan_completed = true\nnotify_on_turn_failed = true\n";
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, before).unwrap();
    Config::migrate_if_needed(&path);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a migrated file must not be rewritten on every launch"
    );
}

#[test]
fn a_second_launch_leaves_the_migrated_file_byte_identical() {
    // The other migrations inject commented defaults when nothing structural
    // changed, so the fixture carries those keys: what this test pins is that
    // migration 6 itself stops asking for a rewrite once the pack is gone.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[agent]\ncompletion_notification_enabled = true\n\
         notify_on_plan_completed = false\n\
         notify_on_task_completed = true\n\
         notify_on_turn_completed = true\n\
         notify_on_turn_failed = true\n\
         subagent_provider = \"inherit\"\nsubagent_model = \"inherit\"\n\
         plan_provider = \"inherit\"\nexecute_provider = \"inherit\"\n",
    )
    .unwrap();

    Config::migrate_if_needed(&path);
    let after_first = std::fs::read_to_string(&path).unwrap();
    assert!(
        !after_first.contains("completion_notification_enabled"),
        "the first pass left the dead master on disk: {after_first}"
    );

    Config::migrate_if_needed(&path);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        after_first,
        "the migration must be idempotent, or every launch rewrites the config"
    );
}

#[test]
fn the_load_path_folds_the_pack_before_serde_sees_it() {
    // A repo-local `opencrabs.toml` is never migrated on disk, so the fold in
    // `merge_from_file` is what keeps a legacy file honest at read time.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencrabs.toml");
    std::fs::write(
        &path,
        "[agent]\ncompletion_notification_enabled = false\n\
         notify_on_turn_failed = true\n\
         notify_on_task_completed = true\n\
         notify_on_turn_completed = true\n",
    )
    .unwrap();

    let config = Config::load_from_path(&path).expect("the legacy file still loads");
    assert!(
        !config.agent.notify_on_turn_failed,
        "a switch that never fired under the master stays off after the fold"
    );
    assert!(!config.agent.notify_on_plan_completed);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("notify_on_task_completed"),
        "the read path folds in memory and leaves the file as written"
    );
}
