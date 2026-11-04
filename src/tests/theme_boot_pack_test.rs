//! A saved pack theme is found at boot, because the pack is built before the
//! saved name is looked up (#2164).

#[test]
fn boot_builds_the_theme_pack_before_looking_up_the_saved_theme() {
    let src = include_str!("../tui/runner.rs");
    let pack = src
        .find("theme_pack::themes();")
        .expect("boot builds the pack");
    let lookup = src
        .find("render::presets::by_name(n)")
        .expect("boot looks up the saved theme");
    assert!(
        pack < lookup,
        "the pack must be built before the saved theme is looked up"
    );
}
