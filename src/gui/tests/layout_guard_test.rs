//! Guard: the window file stays wiring only, and its tests stay in this
//! directory (#3062).
//!
//! `gui/main.rs` was 10,704 lines. The type definitions, the 3,456 line
//! render method, the run sequence and 141 tests all lived in it, so a module
//! it declared was reachable only after scrolling past three thousand lines
//! of layout for the next declaration. The split moved that code out by
//! concern (#3059, #3060, #3061) and left the file with its one remaining
//! job: declare the module tree and start the run sequence.
//!
//! Nothing stops it growing back. A struct is one paste away, and an inline
//! `#[cfg(test)] mod tests` beside it is the second paste. Three invariants
//! hold the shape:
//!
//! 1. `gui/main.rs` stays inside its line budget and declares nothing but
//!    modules: no type, no impl, no trait, and no function except `main`.
//! 2. No new inline test module appears under `gui/`. The ones present when
//!    the split landed are frozen by name, so the list can shrink but never
//!    grow.
//! 3. Every module file under `gui/` is declared in `gui/main.rs`, so a file
//!    nothing declares cannot sit in the tree unnoticed.

use std::fs;
use std::path::{Path, PathBuf};

/// A window file that needs more than this is carrying a concern of its own,
/// and the concern gets a module named after it.
const WINDOW_FILE_BUDGET: usize = 400;

/// The inline test modules that were in the window when the split landed, as
/// `file:mod`. Removing one means moving its tests here, so the entry goes
/// with it; adding one is the regression this list exists to catch.
const FROZEN_INLINE_TESTS: &[&str] = &[
    "agent_bridge.rs:tests",
    "attachment_view.rs:tests",
    "command_palette.rs:tests",
    "commands.rs:tests",
    "config_reload.rs:tests",
    "dashboard_rail.rs:tests",
    "dashboard_topics.rs:group_filter_tests",
    "editor.rs:tests",
    "help_view.rs:tests",
    "icon.rs:tests",
    "layout_store.rs:tests",
    "markdown.rs:tests",
    "mission_control.rs:tests",
    "model_fetch.rs:tests",
    "models.rs:tests",
    "onboard.rs:tests",
    "pane_chrome.rs:window_footer_tests",
    "plan_strip.rs:tests",
    "platform_icons.rs:tests",
    "preview.rs:tests",
    "rail_activity.rs:tests",
    "rail_view.rs:tests",
    "screen_keys.rs:tests",
    "seeker_orb.rs:tests",
    "sessions.rs:tests",
    "skills_view.rs:tests",
    "task_detail.rs:tests",
    "task_list.rs:tests",
    "text_commands.rs:tests",
    "thumbs.rs:tests",
    "topic_icons.rs:tests",
    "topic_tabs.rs:tests",
    "transcript.rs:tests",
    "usage_view.rs:tests",
    "voice_input.rs:tests",
];

fn gui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui")
}

fn window_files() -> Vec<PathBuf> {
    let dir = gui_dir();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {:?}: {e}", dir))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    assert!(
        files.len() > 50,
        "source walk found {} files under {:?}; the window is much larger",
        files.len(),
        dir
    );
    files
}

/// The `mod name {` that follows a `#[cfg(test)]` attribute, which is an
/// inline test module. The declaration form `#[cfg(test)] mod name;` points
/// at a file and is not one.
fn inline_test_modules(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        let next = match source.lines().nth(idx + 1) {
            Some(next) => next.trim(),
            None => continue,
        };
        let Some(rest) = next.strip_prefix("mod ") else {
            continue;
        };
        let Some(name) = rest.strip_suffix(" {") else {
            continue;
        };
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            found.push(name.to_string());
        }
    }
    found
}

#[test]
fn the_window_file_stays_wiring_only() {
    let path = gui_dir().join("main.rs");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
    let lines = source.lines().count();
    assert!(
        lines <= WINDOW_FILE_BUDGET,
        "gui/main.rs is {lines} lines, over its {WINDOW_FILE_BUDGET} line budget. \
         It declares the module tree and starts the run sequence; move the code \
         that grew into a module named after its concern."
    );

    let definitions: Vec<&str> = source
        .lines()
        .filter(|line| {
            let head = line.split_whitespace().next().unwrap_or_default();
            let head = head.strip_prefix("pub(crate)").unwrap_or(head);
            let head = head.strip_prefix("pub").unwrap_or(head);
            matches!(
                head,
                "struct" | "enum" | "impl" | "trait" | "union" | "fn" | "const" | "static" | "type"
            ) && (line.starts_with(head) || head == "fn")
        })
        .collect();
    let expected: &[&str] = &["fn main() {"];
    assert_eq!(
        definitions.as_slice(),
        expected,
        "gui/main.rs declares something other than `fn main`: {definitions:?}. \
         The window's code belongs in the module named after it."
    );
}

#[test]
fn no_new_inline_test_module_appears_under_the_window() {
    let mut current = Vec::new();
    for file in window_files() {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let source =
            fs::read_to_string(&file).unwrap_or_else(|e| panic!("cannot read {file:?}: {e}"));
        for module in inline_test_modules(&source) {
            current.push(format!("{name}:{module}"));
        }
    }
    current.sort();

    let frozen: Vec<String> = FROZEN_INLINE_TESTS.iter().map(|s| s.to_string()).collect();
    let added: Vec<&String> = current.iter().filter(|c| !frozen.contains(c)).collect();
    let removed: Vec<&String> = frozen.iter().filter(|f| !current.contains(f)).collect();

    assert!(
        added.is_empty(),
        "new inline test module under gui/: {added:?}. The window's tests live in \
         src/gui/tests/, one file per module they exercise; move these there and \
         declare the file in src/gui/tests/mod.rs."
    );
    assert!(
        removed.is_empty(),
        "inline test module gone from gui/: {removed:?}. The list froze what was \
         there when the split landed, so if the tests moved out, drop the entry \
         from FROZEN_INLINE_TESTS in the same commit."
    );
}

#[test]
fn every_window_module_file_is_declared() {
    let path = gui_dir().join("main.rs");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
    let declared: Vec<String> = source
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("mod ")?;
            rest.strip_suffix(';').map(|name| name.to_string())
        })
        .collect();

    let mut undeclared = Vec::new();
    for file in window_files() {
        let name = file.file_stem().and_then(|n| n.to_str()).unwrap_or_default();
        if name == "main" {
            continue;
        }
        if !declared.iter().any(|d| d == name) {
            undeclared.push(name.to_string());
        }
    }

    assert!(
        undeclared.is_empty(),
        "modules with no `mod` line in gui/main.rs: {undeclared:?}. A file nothing \
         declares is dead weight; declare it or delete it."
    );
}

#[test]
fn every_test_file_is_declared_in_the_tests_module() {
    let dir = gui_dir().join("tests");
    let mod_rs = dir.join("mod.rs");
    let source =
        fs::read_to_string(&mod_rs).unwrap_or_else(|e| panic!("cannot read {mod_rs:?}: {e}"));
    let declared: Vec<String> = source
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("mod ")?;
            rest.strip_suffix(';').map(|name| name.to_string())
        })
        .collect();

    let mut undeclared = Vec::new();
    let entries = fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {dir:?}: {e}"));
    for file in entries.filter_map(|entry| entry.ok().map(|e| e.path())) {
        let name = file.file_stem().and_then(|n| n.to_str()).unwrap_or_default();
        if name == "mod" {
            continue;
        }
        if !declared.iter().any(|d| d == name) {
            undeclared.push(name.to_string());
        }
    }

    assert!(
        undeclared.is_empty(),
        "test files with no `mod` line in gui/tests/mod.rs: {undeclared:?}"
    );
}
