//! Terminal-neutral UI core shared by every frontend (TUI today, GPUI GUI later).
//!
//! Owns pure UI logic: data loading, view models, state machines, and
//! key-neutral input decisions. Nothing in this module may depend on
//! ratatui, crossterm, or any other terminal-specific crate, so a GUI
//! binary can link it as-is.

pub mod actions;
pub mod bang;
// `card` is the shared card shape and reads `gpui` types, so it only
// compiles with the feature that provides them (#2510). Every consumer is
// in `src/gui/`.
#[cfg(feature = "gui")]
pub mod card;
pub mod channel_status;
pub mod commands;
pub mod composer;
pub mod copy_select;
pub mod docs;
pub mod help_keys;
pub mod input_history;
pub mod keys;
pub mod message_select;
pub mod model_layout;
pub mod model_selector;
pub mod palette;
pub mod pane_tree;
pub mod plan_history;
pub mod query;
pub mod rail;
pub mod rate;
pub mod scroll;
pub mod split;
pub mod topic_bar;
pub mod usage;

#[cfg(test)]
mod purity_tests {
    //! The whole point of this module is that a non-terminal frontend can
    //! link it. An import from either terminal crate slipping in would
    //! break that silently at compile time for the GUI, never for us.

    /// Every source file in `ui_core`, with its contents.
    fn sources() -> [(&'static str, &'static str); 36] {
        [
            ("ui_core/actions.rs", include_str!("actions.rs")),
            ("ui_core/bang.rs", include_str!("bang.rs")),
            ("ui_core/card.rs", include_str!("card.rs")),
            (
                "ui_core/channel_status.rs",
                include_str!("channel_status.rs"),
            ),
            ("ui_core/copy_select.rs", include_str!("copy_select.rs")),
            ("ui_core/composer.rs", include_str!("composer.rs")),
            ("ui_core/docs.rs", include_str!("docs.rs")),
            ("ui_core/mod.rs", include_str!("mod.rs")),
            ("ui_core/palette.rs", include_str!("palette.rs")),
            ("ui_core/plan_history.rs", include_str!("plan_history.rs")),
            ("ui_core/pane_tree.rs", include_str!("pane_tree.rs")),
            ("ui_core/query.rs", include_str!("query.rs")),
            ("ui_core/split.rs", include_str!("split.rs")),
            ("ui_core/rail.rs", include_str!("rail.rs")),
            ("ui_core/rate.rs", include_str!("rate.rs")),
            ("ui_core/scroll.rs", include_str!("scroll.rs")),
            ("ui_core/topic_bar.rs", include_str!("topic_bar.rs")),
            ("ui_core/commands.rs", include_str!("commands.rs")),
            ("ui_core/help_keys.rs", include_str!("help_keys.rs")),
            ("ui_core/input_history.rs", include_str!("input_history.rs")),
            ("ui_core/keys.rs", include_str!("keys.rs")),
            (
                "ui_core/message_select.rs",
                include_str!("message_select.rs"),
            ),
            ("ui_core/model_layout.rs", include_str!("model_layout.rs")),
            (
                "ui_core/model_selector/mod.rs",
                include_str!("model_selector/mod.rs"),
            ),
            (
                "ui_core/model_selector/state.rs",
                include_str!("model_selector/state.rs"),
            ),
            (
                "ui_core/model_selector/discovery.rs",
                include_str!("model_selector/discovery.rs"),
            ),
            (
                "ui_core/model_selector/fetch.rs",
                include_str!("model_selector/fetch.rs"),
            ),
            (
                "ui_core/model_selector/catalogue.rs",
                include_str!("model_selector/catalogue.rs"),
            ),
            (
                "ui_core/model_selector/order.rs",
                include_str!("model_selector/order.rs"),
            ),
            ("ui_core/usage/mod.rs", include_str!("usage/mod.rs")),
            ("ui_core/usage/data.rs", include_str!("usage/data.rs")),
            ("ui_core/usage/pricing.rs", include_str!("usage/pricing.rs")),
            (
                "ui_core/usage/categorizer.rs",
                include_str!("usage/categorizer.rs"),
            ),
            (
                "ui_core/usage/view_model.rs",
                include_str!("usage/view_model.rs"),
            ),
            ("ui_core/usage/input.rs", include_str!("usage/input.rs")),
            ("ui_core/usage/loader.rs", include_str!("usage/loader.rs")),
        ]
    }

    /// Built from single words rather than written as a crate path with
    /// the path separator, so this file does not match the needles it
    /// scans for. A scanner that spells the forbidden string in its own
    /// source flags itself.
    fn terminal_crates() -> [String; 2] {
        ["crossterm", "ratatui"].map(String::from)
    }

    #[test]
    fn no_file_imports_a_terminal_crate() {
        let crates = terminal_crates();
        for (name, src) in sources() {
            let import = src.lines().find(|line| {
                let t = line.trim_start();
                !t.starts_with("//")
                    && crates.iter().any(|c| {
                        t.strip_prefix("use ")
                            .is_some_and(|rest| rest.starts_with(c.as_str()))
                    })
            });
            assert!(
                import.is_none(),
                "{name} imports a terminal crate: {import:?}"
            );
        }
    }

    #[test]
    fn no_file_names_a_terminal_type() {
        let needles: Vec<String> = terminal_crates().iter().map(|c| format!("{c}::")).collect();
        for (name, src) in sources() {
            let hit = src.lines().find(|line| {
                let t = line.trim_start();
                !t.starts_with("//") && needles.iter().any(|n| t.contains(n.as_str()))
            });
            assert!(hit.is_none(), "{name} references a terminal type: {hit:?}");
        }
    }

    /// Every `.rs` file under `ui_core` is in the scan list above. The list is
    /// a fixed-size array, so a new file adds no coverage until someone
    /// remembers to bump it: this fails instead, naming the file that was
    /// added silently (#2458).
    #[test]
    fn the_scan_list_equals_the_directory() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let root = src.join("ui_core");
        let mut on_disk: Vec<String> = Vec::new();
        fn walk(dir: &std::path::Path, src: &std::path::Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, src, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let rel = path
                        .strip_prefix(src)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push(rel);
                }
            }
        }
        walk(&root, &src, &mut on_disk);
        on_disk.sort();
        let mut listed: Vec<String> = sources().iter().map(|(name, _)| name.to_string()).collect();
        listed.sort();
        assert_eq!(
            listed, on_disk,
            "the purity scan list and src/ui_core disagree"
        );
    }
}
