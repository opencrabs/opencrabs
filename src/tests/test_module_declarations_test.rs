//! Guard: every file under `src/tests/` is declared in `mod.rs`, and every
//! declaration points at a file that exists.
//!
//! Four modules (`error_scenarios_test`, `integration_test`,
//! `plan_mode_integration_test`, `streaming_test`) sat commented out in
//! `mod.rs` for months while their sources still imported `opencrabs::llm`,
//! a module deleted in 2026-02. Nothing compiled them, so nothing complained:
//! dead code that looks like coverage is worse than no file at all. This test
//! makes an undeclared file and a dangling declaration both fail loudly.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests")
}

/// Active `mod foo;` / `pub mod foo;` declarations in `mod.rs`, skipping
/// commented-out lines.
fn declared_modules(mod_rs: &str) -> BTreeSet<String> {
    let mut declared = BTreeSet::new();
    for line in mod_rs.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        let rest = trimmed
            .strip_prefix("pub mod ")
            .or_else(|| trimmed.strip_prefix("pub(crate) mod "))
            .or_else(|| trimmed.strip_prefix("mod "));
        if let Some(rest) = rest
            && let Some(name) = rest.strip_suffix(';')
        {
            declared.insert(name.trim().to_string());
        }
    }
    declared
}

/// Source files under `src/tests/` that could be declared, keyed by module name.
fn source_modules(dir: &Path) -> BTreeSet<String> {
    let mut modules = BTreeSet::new();
    for entry in fs::read_dir(dir).expect("read src/tests") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "mod" {
            continue;
        }
        modules.insert(stem.to_string());
    }
    modules
}

#[test]
fn every_test_source_file_is_declared() {
    let dir = tests_dir();
    let mod_rs = fs::read_to_string(dir.join("mod.rs")).expect("read src/tests/mod.rs");
    let declared = declared_modules(&mod_rs);
    let on_disk = source_modules(&dir);

    let undeclared: Vec<&String> = on_disk.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "test files exist but are not declared in src/tests/mod.rs, so they never compile: {undeclared:?}\n\
         Declare them, or delete them."
    );
}

#[test]
fn every_declared_module_has_a_source_file() {
    let dir = tests_dir();
    let mod_rs = fs::read_to_string(dir.join("mod.rs")).expect("read src/tests/mod.rs");
    let declared = declared_modules(&mod_rs);
    let on_disk = source_modules(&dir);

    let missing: Vec<&String> = declared.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "src/tests/mod.rs declares modules with no source file: {missing:?}"
    );
}

#[test]
fn the_guard_ignores_commented_out_declarations() {
    let mod_rs = "pub mod live_test;\n//pub mod dead_test;\n// pub mod also_dead_test;\n";
    let declared = declared_modules(mod_rs);
    assert!(declared.contains("live_test"));
    assert!(!declared.contains("dead_test"));
    assert!(!declared.contains("also_dead_test"));
}

#[test]
fn the_guard_counts_plain_mod_declarations() {
    let mod_rs = "mod plain_test;\npub(crate) mod crate_test;\n";
    let declared = declared_modules(mod_rs);
    assert!(declared.contains("plain_test"));
    assert!(declared.contains("crate_test"));
}
