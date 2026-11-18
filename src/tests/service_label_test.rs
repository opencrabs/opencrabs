//! `opencrabs service` label ownership (#2834).
//!
//! Two launchd labels exist per user, and before this only the managed one was
//! known to any service command:
//!
//!   * `com.opencrabs.daemon` / `com.opencrabs.daemon.<profile>` written by
//!     `service install`,
//!   * `com.opencrabs.agent` written by the TUI onboarding wizard and the GUI
//!     Start daemon chip.
//!
//! Both templates set `KeepAlive`, so the mismatch showed up as `status`
//! printing "not running" over a live daemon and `stop` printing "Stopped"
//! while the process stayed up.
//!
//! Everything here is pure: no test spawns `launchctl` and no path resolves
//! against the live `~/Library/LaunchAgents`.

use crate::cli::commands::{
    LabelMigration, any_owned_label_loaded, build_launchagent_plist, launchagent_plist_path_in,
    managed_service_label, owned_launchctl_rows, parse_launchctl_rows, plan_label_migration,
    remove_legacy_plist_in, service_labels_for,
};

/// A realistic `launchctl list`: a header, a system job, our two labels (one
/// running with a pid, one loaded but idle), and a stranger.
const LIST_OUTPUT: &str = "PID\tStatus\tLabel\n\
                           4242\t0\tcom.opencrabs.agent\n\
                           -\t0\tcom.opencrabs.daemon\n\
                           99\t0\tcom.apple.somehelper\n";

#[test]
fn default_profile_owns_managed_and_onboarding_labels() {
    let labels = service_labels_for(None);
    assert_eq!(
        labels,
        vec![
            "com.opencrabs.daemon".to_string(),
            "com.opencrabs.agent".to_string()
        ]
    );
    assert_eq!(service_labels_for(Some("default")), labels);
}

/// The onboarding plist carries no `-p`, so it runs the DEFAULT profile. A
/// named profile that unloaded it would stop someone else's daemon.
#[test]
fn named_profile_owns_only_its_own_managed_label() {
    assert_eq!(
        service_labels_for(Some("hermes")),
        vec!["com.opencrabs.daemon.hermes".to_string()]
    );
}

#[test]
fn rows_keep_our_labels_and_drop_foreign_ones() {
    let rows = parse_launchctl_rows(LIST_OUTPUT);
    assert_eq!(
        rows,
        vec![
            (Some(4242), "com.opencrabs.agent".to_string()),
            (None, "com.opencrabs.daemon".to_string()),
        ]
    );
}

#[test]
fn a_loaded_label_we_do_not_own_is_not_ours_to_stop() {
    // Only the agent label is loaded; a named profile must not touch it.
    let rows = owned_launchctl_rows(LIST_OUTPUT, Some("hermes"));
    assert!(rows.is_empty());
    assert!(!any_owned_label_loaded(&rows));
}

/// The exact #2834 symptom: only `com.opencrabs.agent` is loaded, so `status`
/// must report running, and a loaded-but-idle job counts too because KeepAlive
/// brings it back.
#[test]
fn any_owned_label_loaded_counts_the_onboarding_label() {
    let rows = owned_launchctl_rows(LIST_OUTPUT, Some("default"));
    assert_eq!(rows.len(), 2);
    assert!(
        any_owned_label_loaded(&rows),
        "an owned label was loaded but the profile read as not running"
    );

    let idle_only = "PID\tStatus\tLabel\n-\t0\tcom.opencrabs.agent\n";
    let rows = owned_launchctl_rows(idle_only, None);
    assert!(any_owned_label_loaded(&rows));
}

#[test]
fn nothing_loaded_reports_not_running() {
    let rows = owned_launchctl_rows("PID\tStatus\tLabel\n99\t0\tcom.apple.somehelper\n", None);
    assert!(rows.is_empty());
    assert!(!any_owned_label_loaded(&rows));
}

/// Plist paths resolve under the directory the caller passes, so the uninstall
/// and stop loops are testable without touching the live home directory.
#[test]
fn plist_path_is_built_under_the_given_directory() {
    let dir = std::env::temp_dir().join("opencrabs-service-label-test");
    let path = launchagent_plist_path_in(&dir, "com.opencrabs.agent");
    assert_eq!(path, dir.join("com.opencrabs.agent.plist"));
    let path = launchagent_plist_path_in(&dir, "com.opencrabs.daemon.hermes");
    assert_eq!(path, dir.join("com.opencrabs.daemon.hermes.plist"));
}

/// The installer writes the label this function returns, so a named profile
/// owns `com.opencrabs.daemon.<profile>` and never the unscoped one (#2837).
#[test]
fn managed_label_is_scoped_per_profile() {
    assert_eq!(
        managed_service_label(None),
        "com.opencrabs.daemon".to_string()
    );
    assert_eq!(
        managed_service_label(Some("default")),
        "com.opencrabs.daemon".to_string()
    );
    assert_eq!(
        managed_service_label(Some("hermes")),
        "com.opencrabs.daemon.hermes".to_string()
    );
}

/// One template for every install surface. The label and the daemon arguments
/// come from the caller, so the plist can never disagree with the label the
/// rest of the CLI looks for.
#[test]
fn launchagent_plist_carries_the_label_and_args_it_is_given() {
    let args = vec!["-p".to_string(), "hermes".to_string(), "daemon".to_string()];
    let plist = build_launchagent_plist(
        "com.opencrabs.daemon.hermes",
        "/opt/bin/opencrabs",
        &args,
        "/tmp/out.log",
        "/tmp/err.log",
    );
    assert!(plist.contains("<string>com.opencrabs.daemon.hermes</string>"));
    assert!(!plist.contains("<string>com.opencrabs.agent</string>"));
    assert!(plist.contains("<string>/opt/bin/opencrabs</string>"));
    assert!(plist.contains("<string>-p</string>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<string>/tmp/err.log</string>"));
}

/// A throwaway `LaunchAgents` directory: the migration tests exercise the real
/// file work against this, never `~/Library/LaunchAgents`.
fn temp_agents_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "opencrabs-launchagents-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp LaunchAgents dir");
    dir
}

fn write_plist(agents_dir: &std::path::Path, label: &str) {
    std::fs::write(launchagent_plist_path_in(agents_dir, label), "<plist/>")
        .expect("write plist");
}

/// The case a long-lived install hits: both labels on disk. The managed plist
/// must survive and the legacy one must go, leaving the profile with one job.
#[test]
fn migration_leaves_only_the_managed_plist_on_a_two_label_home() {
    let dir = temp_agents_dir("both");
    write_plist(&dir, "com.opencrabs.agent");
    write_plist(&dir, "com.opencrabs.daemon");

    assert_eq!(
        plan_label_migration(&dir, Some("default")),
        LabelMigration::Retire
    );
    assert!(remove_legacy_plist_in(&dir).expect("remove the legacy plist"));
    assert!(!dir.join("com.opencrabs.agent.plist").exists());
    assert!(dir.join("com.opencrabs.daemon.plist").exists());

    // A second startup finds nothing left to do, so the hook stays quiet.
    assert_eq!(
        plan_label_migration(&dir, Some("default")),
        LabelMigration::Nothing
    );
    assert!(!remove_legacy_plist_in(&dir).expect("second remove is a no-op"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing is deleted before a replacement exists: with only the legacy plist
/// on disk the migration installs the managed one first.
#[test]
fn legacy_only_home_installs_the_managed_label_before_retiring() {
    let dir = temp_agents_dir("legacy-only");
    write_plist(&dir, "com.opencrabs.agent");

    assert_eq!(
        plan_label_migration(&dir, None),
        LabelMigration::InstallThenRetire
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The agent plist belongs to the default profile, so a named profile plans no
/// work even when both its own label and the legacy one are present.
#[test]
fn named_profile_never_retires_the_unscoped_label() {
    let dir = temp_agents_dir("named");
    write_plist(&dir, "com.opencrabs.agent");
    write_plist(&dir, "com.opencrabs.daemon.hermes");

    assert_eq!(
        plan_label_migration(&dir, Some("hermes")),
        LabelMigration::Nothing
    );
    let _ = std::fs::remove_dir_all(&dir);
}
