//! `opencrabs init` owns creating the config, so startup must not create it
//! first.
//!
//! The startup path seeds `config.toml` from the shipped example before
//! subcommands are dispatched (#2079), so `cmd_init` found a file that already
//! existed and refused to run on a fresh install (#2093). Seeding is skipped
//! for `init` now, and the command writes that same example itself.
//!
//! These tests scope the config home with `with_home_override_async` rather
//! than pointing `$HOME` at a tempdir, which is process-global and would let a
//! parallel test resolve into this one's directory (#912).

use crate::cli::commands::cmd_init;
use crate::cli::{Commands, seeds_config_on_startup};
use crate::config::Config;
use crate::config::profile::{with_home_override, with_home_override_async};
use crate::config::seed::CONFIG_EXAMPLE;

#[tokio::test]
async fn init_writes_the_shipped_example_on_a_fresh_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join("config.toml");

    with_home_override_async(home.path().to_path_buf(), async {
        let config = Config::default();
        cmd_init(&config, false)
            .await
            .expect("a fresh home must let init create the config");
    })
    .await;

    let written = std::fs::read_to_string(&path).expect("init must leave a config.toml");
    assert_eq!(
        written, CONFIG_EXAMPLE,
        "init must leave the shipped example, byte for byte: a config written \
         from the in-memory default has no provider section to hand-edit"
    );
}

#[tokio::test]
async fn init_still_refuses_a_second_run_without_force() {
    let home = tempfile::tempdir().expect("tempdir");

    with_home_override_async(home.path().to_path_buf(), async {
        let config = Config::default();
        cmd_init(&config, false).await.expect("first run");

        let err = cmd_init(&config, false)
            .await
            .expect_err("a second run without --force must fail");
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "unexpected message: {msg}");
        assert!(
            msg.contains("--force"),
            "the refusal must name the flag that overrides it: {msg}"
        );
    })
    .await;
}

#[tokio::test]
async fn init_force_rewrites_an_edited_config_back_to_the_example() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join("config.toml");

    with_home_override_async(home.path().to_path_buf(), async {
        std::fs::write(&path, "[agent]\napproval_policy = \"ask\"\n")
            .expect("stand in for a stripped config");
        let config = Config::default();
        cmd_init(&config, true)
            .await
            .expect("--force must overwrite");
    })
    .await;

    let written = std::fs::read_to_string(&path).expect("read");
    assert_eq!(
        written, CONFIG_EXAMPLE,
        "--force must leave the documented file, not the one it replaced"
    );
}

#[test]
fn startup_seeding_is_skipped_for_init_and_kept_for_every_other_command() {
    assert!(
        !seeds_config_on_startup(Some(&Commands::Init { force: false })),
        "seeding at startup is what made init find its own output (#2093)"
    );
    assert!(
        !seeds_config_on_startup(Some(&Commands::Init { force: true })),
        "--force does not make the startup path the right creator either"
    );
    assert!(
        seeds_config_on_startup(Some(&Commands::Status)),
        "every other command still relies on the startup seed (#2079)"
    );
    assert!(
        seeds_config_on_startup(None),
        "the default TUI run must still seed a blank home (#2079)"
    );
}

#[test]
fn a_non_init_startup_still_seeds_both_files() {
    let home = tempfile::tempdir().expect("tempdir");

    with_home_override(home.path().to_path_buf(), || {
        assert!(
            crate::config::seed::ensure_config_seeded(),
            "a blank home must still get config.toml from the startup path"
        );
        assert!(
            crate::config::seed::ensure_keys_seeded(),
            "a blank home must still get keys.toml from the startup path"
        );
        assert!(
            home.path().join("config.toml").exists(),
            "exempting init must not weaken the seed for everyone else"
        );
        assert!(home.path().join("keys.toml").exists());
    });
}
