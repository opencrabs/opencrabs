//! Terminal-neutral UI core shared by every frontend (TUI today, GPUI GUI later).
//!
//! Owns pure UI logic: data loading, view models, state machines, and
//! key-neutral input decisions. Nothing in this module may depend on
//! ratatui, crossterm, or any other terminal-specific crate, so a GUI
//! binary can link it as-is.

pub mod keys;
pub mod usage;

#[cfg(test)]
mod purity_tests {
    //! The whole point of this module is that a non-terminal frontend can
    //! link it. A `use crossterm` or a `ratatui::` type slipping in would
    //! break that silently at compile time for the GUI, never for us.

    /// Every source file in `ui_core`, with its contents.
    fn sources() -> [(&'static str, &'static str); 8] {
        [
            ("ui_core/mod.rs", include_str!("mod.rs")),
            ("ui_core/keys.rs", include_str!("keys.rs")),
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
        ]
    }

    /// Built from single words rather than written as `crossterm::`, so this
    /// file does not match the needles it scans for. A scanner that spells
    /// the forbidden string in its own source flags itself.
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
}
