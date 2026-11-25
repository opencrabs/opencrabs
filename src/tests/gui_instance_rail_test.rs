//! The remote-instance rail is opt-in (#3178): a user who never asked for
//! remote-instance management keeps the 56 px. The window has no headless
//! harness, so the mount rule is checked where it is written, in source, and
//! the config key gets the same default-off and round-trip pins the other
//! `[gui]` keys have.

use opencrabs::config::types::GuiConfig;

const GUI_MOUNT: &str = concat!(
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);

#[test]
fn instance_rail_is_off_by_default() {
    let config = GuiConfig::default();
    assert!(!config.instance_rail, "the rail must be opt-in, never on for a fresh install");
}

#[test]
fn instance_rail_round_trips_through_toml() {
    let config: GuiConfig = toml::from_str("instance_rail = true").unwrap();
    assert!(config.instance_rail);
    let back = toml::to_string(&config).unwrap();
    let config: GuiConfig = toml::from_str(&back).unwrap();
    assert!(config.instance_rail);
}

/// The strip must be mounted behind the gate, and the startup store read and
/// probe loop must sit behind it too: a flag-off user pays nothing, not even
/// a five-second network probe.
#[test]
fn the_strip_mount_and_its_start_up_work_are_behind_the_gate() {
    let mount = GUI_MOUNT
        .split("Opt-in though (#3178)")
        .nth(1)
        .expect("the mount site names the opt-in rule");
    assert!(
        mount.contains("if self.instance_rail {"),
        "the strip mounts behind the gate"
    );
    assert!(
        mount.contains("root = root.child(self.instance_strip_element(cx));"),
        "the strip element is the gated child"
    );
    let startup = GUI_MOUNT
        .split("The rail is opt-in\n                // (#3178)")
        .nth(1)
        .expect("the startup block names the opt-in rule");
    assert!(
        startup.contains("if this.instance_rail {"),
        "the store read and the probe loop sit behind the gate"
    );
    assert!(
        startup.contains("this.load_instances(cx);")
            && startup.contains("Dashboard::spawn_instance_probe(cx);"),
        "both startup costs are inside the gate"
    );
}

/// The settings row is restart-gated: the mount and the probe loop are decided
/// once at window open, so a live toggle would pretend to apply.
#[test]
fn the_settings_row_carries_the_restart_chip() {
    let src = include_str!("../gui/settings_view.rs");
    let spec = src
        .split("key: \"instance_rail\",")
        .nth(1)
        .and_then(|tail| tail.split("SettingSpec {").next())
        .expect("the instance_rail spec exists");
    assert!(spec.contains("restart: true,"), "the row must say restart");
    assert!(spec.contains("SettingsSection::Advanced,"), "the row groups under Advanced");
    assert!(
        src.contains("(\"gui\", \"instance_rail\") => RowValue::Bool(g.instance_rail)"),
        "the spec has a reader"
    );
}
