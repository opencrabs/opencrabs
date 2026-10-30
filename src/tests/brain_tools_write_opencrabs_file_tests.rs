// Tests for `write_opencrabs_file` tool.

use crate::brain::tools::write_opencrabs_file::*;
use crate::brain::tools::{Tool, ToolExecutionContext};
use tempfile::TempDir;
use uuid::Uuid;

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4())
}

fn tool() -> WriteOpenCrabsFileTool {
    WriteOpenCrabsFileTool
}

// ── metadata ─────────────────────────────────────────────────────────────────

#[test]
fn test_tool_name_and_requires_approval() {
    let t = tool();
    assert_eq!(t.name(), "write_opencrabs_file");
    assert!(t.requires_approval(), "writes must require approval");
}

#[test]
fn test_input_schema_required_fields() {
    let schema = tool().input_schema();
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v.as_str() == Some("path")));
    assert!(required.iter().any(|v| v.as_str() == Some("operation")));
}

// ── path validation ───────────────────────────────────────────────────────────

/// A named-profile home shape: `<tmp>/.opencrabs/profiles/<name>`.
fn named_profile_home(dir: &TempDir, name: &str) -> std::path::PathBuf {
    dir.path().join(".opencrabs").join("profiles").join(name)
}

/// The default-profile home shape: `<tmp>/.opencrabs`.
fn default_home(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(".opencrabs")
}

#[test]
fn test_home_is_named_profile_predicate() {
    let dir = TempDir::new().unwrap();

    assert!(
        home_is_named_profile(&named_profile_home(&dir, "ops")),
        "base/profiles/ops is a named-profile home"
    );
    assert!(
        home_is_named_profile(&named_profile_home(&dir, "family")),
        "base/profiles/family is a named-profile home"
    );
    assert!(
        !home_is_named_profile(&default_home(&dir)),
        "base is NOT a named-profile home"
    );
    assert!(
        !home_is_named_profile(&dir.path().join("profiles")),
        "a directory merely named profiles (no name segment under it) is not a profile home"
    );
}

#[test]
fn test_leading_profiles_segment_predicate() {
    assert!(leading_profiles_segment("profiles/ops/TOOLS.md"));
    assert!(
        leading_profiles_segment("./profiles/ops/TOOLS.md"),
        "a leading CurDir must be skipped, not treated as the first segment"
    );
    assert!(leading_profiles_segment("profiles"));
    assert!(!leading_profiles_segment("MEMORY.md"));
    assert!(!leading_profiles_segment("memory/profiles/note.md"));
    assert!(!leading_profiles_segment(""));
}

#[test]
fn test_empty_path_rejected() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "").is_err());
}

#[test]
fn test_absolute_path_rejected() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "/etc/passwd").is_err());
    assert!(validate_opencrabs_path(&home, "~/MEMORY.md").is_err());
}

#[test]
fn test_path_traversal_rejected() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "../etc/passwd").is_err());
    assert!(validate_opencrabs_path(&home, "../../secrets").is_err());
    assert!(validate_opencrabs_path(&home, "subdir/../../etc/passwd").is_err());
}

#[test]
fn test_valid_brain_files_accepted() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "MEMORY.md").is_ok());
    assert!(validate_opencrabs_path(&home, "USER.md").is_ok());
    assert!(validate_opencrabs_path(&home, "SOUL.md").is_ok());
}

#[test]
fn test_valid_config_files_accepted() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "commands.toml").is_ok());
    assert!(validate_opencrabs_path(&home, "config.toml").is_ok());
}

#[test]
fn test_valid_subdirectory_paths_accepted() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    assert!(validate_opencrabs_path(&home, "memory/2026-03-02.md").is_ok());
    assert!(validate_opencrabs_path(&home, "agents/session/context.json").is_ok());
}

/// A leading `profiles/` segment duplicates a NAMED profile's home path, so it
/// is refused in BOTH spellings — the plain one and the `./`-prefixed one that
/// used to slip past the string test (issue #350).
#[test]
fn test_profiles_prefix_duplication_rejected() {
    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");

    let result = validate_opencrabs_path(&home, "profiles/ops/TOOLS.md");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("duplicates it"),
        "Error message should name the real rule: {err}"
    );
    assert!(
        err.contains("profiles/ops/TOOLS.md"),
        "Error should echo the bad path: {err}"
    );

    // A second duplicating path, to show the refusal is not specific to one file.
    // The bare `profiles` segment is covered in the predicate test above.
    assert!(validate_opencrabs_path(&home, "profiles/ops/MEMORY.md").is_err());

    // The bypass spelling must get the SAME verdict, not be accepted.
    assert!(
        validate_opencrabs_path(&home, "./profiles/ops/TOOLS.md").is_err(),
        "the leading './' spelling must not bypass the gate"
    );
}

/// Issue #350: for the DEFAULT profile (`<base>`), `profiles/<name>/...` is a
/// sibling profile's real home, not a duplicated prefix — it must be accepted,
/// in both spellings.
#[test]
fn test_profiles_sibling_path_accepted_for_default_home() {
    let dir = TempDir::new().unwrap();
    let home = default_home(&dir);

    assert!(
        validate_opencrabs_path(&home, "profiles/family/USER.md").is_ok(),
        "profiles/family/USER.md is the sibling profile's real home"
    );
    assert!(
        validate_opencrabs_path(&home, "profiles/ops/TOOLS.md").is_ok(),
        "profiles/ops/TOOLS.md is the sibling profile's real home"
    );
    assert!(
        validate_opencrabs_path(&home, "./profiles/family/USER.md").is_ok(),
        "identical target, identical verdict — the ./ spelling is accepted too"
    );
}

/// The default home keeps every traversal refusal it had before #350.
#[test]
fn test_default_home_still_rejects_traversal() {
    let dir = TempDir::new().unwrap();
    let home = default_home(&dir);

    assert!(validate_opencrabs_path(&home, "../etc/passwd").is_err());
    assert!(validate_opencrabs_path(&home, "/etc/passwd").is_err());
    assert!(validate_opencrabs_path(&home, "~/MEMORY.md").is_err());
    assert!(validate_opencrabs_path(&home, "").is_err());
}

// ── operation validation ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_unknown_operation_returns_error() {
    let result = tool()
        .execute(
            serde_json::json!({"path": "MEMORY.md", "operation": "delete"}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("Unknown operation"));
}

#[tokio::test]
async fn test_overwrite_missing_content_returns_error() {
    let result = tool()
        .execute(
            serde_json::json!({"path": "MEMORY.md", "operation": "overwrite"}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("content is required"));
}

#[tokio::test]
async fn test_append_missing_content_returns_error() {
    let result = tool()
        .execute(
            serde_json::json!({"path": "MEMORY.md", "operation": "append"}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("content is required"));
}

#[tokio::test]
async fn test_replace_missing_old_text_returns_error() {
    let result = tool()
        .execute(
            serde_json::json!({"path": "MEMORY.md", "operation": "replace", "new_text": "x"}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("old_text is required"));
}

#[tokio::test]
async fn test_replace_missing_new_text_returns_error() {
    let result = tool()
        .execute(
            serde_json::json!({"path": "MEMORY.md", "operation": "replace", "old_text": "x"}),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("new_text is required"));
}

// ── roundtrip write + read ────────────────────────────────────────────────────

#[test]
fn test_overwrite_roundtrip_via_fs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("MEMORY.md");
    std::fs::write(&path, "initial content").unwrap();
    std::fs::write(&path, "updated content").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated content");
}

#[test]
fn test_append_roundtrip_via_fs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("MEMORY.md");
    std::fs::write(&path, "## Section\n").unwrap();
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"\nnew note").unwrap();
    let result = std::fs::read_to_string(&path).unwrap();
    assert!(result.contains("## Section"));
    assert!(result.contains("new note"));
}

#[test]
fn test_replace_roundtrip_via_fs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("MEMORY.md");
    std::fs::write(&path, "old value here").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let updated = content.replacen("old value", "new value", 1);
    std::fs::write(&path, &updated).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new value here");
}

#[test]
fn test_subdirectory_created_on_write() {
    let dir = TempDir::new().unwrap();
    let subdir = dir.path().join("memory");
    let path = subdir.join("note.md");
    // Simulate create_dir_all + write
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(&path, "content").unwrap();
    assert!(path.exists());
}

// ── NFC normalization for replace ────────────────────────────────────────────

/// Verify that NFC normalization makes replace work when the file has NFD
/// bytes but the search string uses NFC (the common macOS copy-paste case).
/// Regression test for issue #256.
#[test]
fn test_replace_nfc_normalizes_both_sides() {
    use unicode_normalization::UnicodeNormalization;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("MEMORY.md");

    // Em-dash: U+2014. In NFC it's 3 bytes (E2 80 94). In NFD it's the same
    // (em-dash has no decomposition). Multiplication sign: U+00D7.
    // In NFC: C3 97. In NFD: same (no decomposition).
    // The real-world issue is with accented characters, e.g. é:
    // NFC: C3 A9 (single codepoint U+00E9)
    // NFD: 65 CC 81 (e + combining acute accent U+0301)

    // Write file with NFD-normalized content (as macOS might produce)
    let nfd_content: String = "café résumé".nfd().collect();
    std::fs::write(&path, &nfd_content).unwrap();

    // Search with NFC-normalized text (as most users would type)
    let search_nfc: String = "café".nfc().collect();
    let replace_nfc: String = "coffee".nfc().collect();

    // Verify they're different byte sequences
    let search_nfd: String = search_nfc.nfd().collect();
    assert_ne!(
        search_nfc.as_bytes(),
        search_nfd.as_bytes(),
        "NFC and NFD should differ for accented chars"
    );

    // Raw contains() would FAIL because NFC search won't match NFD file
    let raw_file = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw_file.contains(search_nfc.as_str()),
        "raw contains should fail with mixed normalization"
    );

    // NFC-normalize both sides (what write_opencrabs_file now does)
    let file_nfc: String = raw_file.nfc().collect();
    let search_nfc_norm: String = search_nfc.nfc().collect();
    assert!(
        file_nfc.contains(search_nfc_norm.as_str()),
        "NFC-normalized contains should match"
    );

    // Do the replacement
    let updated = file_nfc.replacen(search_nfc_norm.as_str(), replace_nfc.as_str(), 1);
    std::fs::write(&path, &updated).unwrap();

    let result = std::fs::read_to_string(&path).unwrap();
    assert!(
        result.contains("coffee"),
        "replacement should have succeeded: got {:?}",
        result
    );
    assert!(
        result.contains("résumé"),
        "unrelated text should be preserved: got {:?}",
        result
    );
}

/// Verify that replace with Unicode symbols (em-dash, multiplication sign,
/// less-than-or-equal) works through NFC normalization.
#[test]
fn test_replace_unicode_symbols_via_nfc() {
    use unicode_normalization::UnicodeNormalization;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.md");

    // Content with various Unicode symbols
    let content = "price: 10×2 ≤ 25 — final";
    std::fs::write(&path, content).unwrap();

    let search = "10×2";
    let replacement = "10x2";

    // NFC normalize both sides
    let raw = std::fs::read_to_string(&path).unwrap();
    let file_nfc: String = raw.nfc().collect();
    let search_nfc: String = search.nfc().collect();

    assert!(
        file_nfc.contains(search_nfc.as_str()),
        "multiplication sign should match after NFC normalization"
    );

    let updated = file_nfc.replacen(search_nfc.as_str(), replacement, 1);
    std::fs::write(&path, &updated).unwrap();

    let result = std::fs::read_to_string(&path).unwrap();
    assert!(
        result.contains("10x2"),
        "replacement succeeded: {:?}",
        result
    );
    assert!(
        result.contains("≤"),
        "other Unicode preserved: {:?}",
        result
    );
    assert!(result.contains("—"), "em-dash preserved: {:?}", result);
}

// ── cross-boundary: the tool's real execute() path ───────────────────────────
//
// The predicate tests above call the validator directly. These two drive the
// ACTUAL tool boundary — `WriteOpenCrabsFileTool::execute` under a temp home
// override — so the call-site wiring (home hoisted above validation, the
// two-argument invocation) is exercised, not merely the function it delegates
// to. Nothing is asserted against a helper's own delegate: the file either
// exists on disk afterwards or it does not.

#[tokio::test]
async fn test_execute_writes_real_file_under_temp_home() {
    use crate::config::profile::with_home_override_async;

    let dir = TempDir::new().unwrap();
    let home = default_home(&dir);
    std::fs::create_dir_all(&home).unwrap();

    let ctx = ctx();
    let t = tool();
    let result = with_home_override_async(home.clone(), async {
        t.execute(
            serde_json::json!({
                "path": "memory/2026-03-02.md",
                "operation": "overwrite",
                "content": "hello from the boundary test",
            }),
            &ctx,
        )
        .await
    })
    .await
    .unwrap();

    assert!(result.success, "write should succeed: {:?}", result.error);

    // Read back through the filesystem — the real cross-boundary assertion.
    let landed = std::fs::read_to_string(home.join("memory/2026-03-02.md")).unwrap();
    assert_eq!(landed, "hello from the boundary test");
}

#[tokio::test]
async fn test_execute_rejects_profiles_prefix_and_writes_nothing() {
    use crate::config::profile::with_home_override_async;

    let dir = TempDir::new().unwrap();
    let home = named_profile_home(&dir, "ops");
    std::fs::create_dir_all(&home).unwrap();

    let ctx = ctx();
    let t = tool();
    for spelling in ["profiles/ops/TOOLS.md", "./profiles/ops/TOOLS.md"] {
        let result = with_home_override_async(home.clone(), async {
            t.execute(
                serde_json::json!({
                    "path": spelling,
                    "operation": "overwrite",
                    "content": "must never land",
                }),
                &ctx,
            )
            .await
        })
        .await
        .unwrap();

        assert!(!result.success, "{spelling} must be rejected");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("duplicates it"),
            "{spelling}: error should explain the duplication, got {:?}",
            result.error
        );
        // The real boundary check: nothing was created one level too deep.
        assert!(
            !home.join(spelling).exists(),
            "{spelling} must not exist on disk"
        );
    }
}

// ── config parse guard (#2099) ───────────────────────────────────────────────
//
// The capability stays exactly as it is: a config the user asks for still
// writes. Only content that would not LOAD is refused, and the previous bytes
// stay on disk. These drive the real `execute()` boundary under a temp home, so
// the guard is proven wired into each write path rather than merely available.

/// Run one `execute()` under a temp home that already holds `file_name` with
/// `initial` content. Returns the tool result and the bytes on disk afterwards.
async fn write_under_home(
    home: std::path::PathBuf,
    file_name: &str,
    initial: &str,
    input: serde_json::Value,
) -> (crate::brain::tools::ToolResult, String) {
    use crate::config::profile::with_home_override_async;

    std::fs::create_dir_all(&home).unwrap();
    let target = home.join(file_name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&target, initial).unwrap();

    let ctx = ctx();
    let t = tool();
    let result = with_home_override_async(home.clone(), async { t.execute(input, &ctx).await })
        .await
        .unwrap();

    let on_disk = std::fs::read_to_string(&target).unwrap_or_default();
    (result, on_disk)
}

#[tokio::test]
async fn test_overwrite_rejects_unparseable_config_and_keeps_old_bytes() {
    let dir = TempDir::new().unwrap();
    let original = "[agent]\napproval_policy = \"auto-always\"\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "config.toml",
        original,
        serde_json::json!({
            "path": "config.toml",
            "operation": "overwrite",
            "content": "allowed = [\n",
        }),
    )
    .await;

    assert!(!result.success, "unparseable config must be refused");
    let err = result.error.unwrap_or_default();
    assert!(
        err.contains("Refused to write config.toml"),
        "error must name the file it refused: {err}"
    );
    assert_eq!(
        on_disk, original,
        "the previous bytes must survive a refused write"
    );
}

#[tokio::test]
async fn test_overwrite_accepts_valid_config_including_approval_policy() {
    let dir = TempDir::new().unwrap();
    let asked_for = "[agent]\napproval_policy = \"auto-always\"\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "config.toml",
        "[agent]\n",
        serde_json::json!({
            "path": "config.toml",
            "operation": "overwrite",
            "content": asked_for,
        }),
    )
    .await;

    assert!(
        result.success,
        "a config the user asked for must still write: {:?}",
        result.error
    );
    assert_eq!(
        on_disk, asked_for,
        "the requested policy change must land, not be gated"
    );
}

#[tokio::test]
async fn test_overwrite_rejects_unparseable_keys_and_keeps_old_bytes() {
    let dir = TempDir::new().unwrap();
    let original = "[providers]\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "keys.toml",
        original,
        serde_json::json!({
            "path": "keys.toml",
            "operation": "overwrite",
            "content": "[providers\n",
        }),
    )
    .await;

    assert!(!result.success, "unparseable keys must be refused");
    let err = result.error.unwrap_or_default();
    assert!(
        err.contains("Refused to write keys.toml"),
        "error must name the file it refused: {err}"
    );
    assert_eq!(on_disk, original, "the previous keys must survive");
}

#[tokio::test]
async fn test_overwrite_accepts_valid_keys() {
    let dir = TempDir::new().unwrap();
    let asked_for = "[providers]\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "keys.toml",
        "",
        serde_json::json!({
            "path": "keys.toml",
            "operation": "overwrite",
            "content": asked_for,
        }),
    )
    .await;

    assert!(
        result.success,
        "valid keys must still write: {:?}",
        result.error
    );
    assert_eq!(on_disk, asked_for);
}

#[tokio::test]
async fn test_replace_that_would_break_config_is_refused() {
    let dir = TempDir::new().unwrap();
    let original = "[agent]\napproval_policy = \"auto-always\"\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "config.toml",
        original,
        serde_json::json!({
            "path": "config.toml",
            "operation": "replace",
            "old_text": "auto-always",
            "new_text": "auto-always\" [\nbroken",
        }),
    )
    .await;

    assert!(
        !result.success,
        "a replace that breaks parsing must be refused"
    );
    assert_eq!(on_disk, original, "the previous bytes must survive");
}

#[tokio::test]
async fn test_append_that_would_break_config_is_refused() {
    let dir = TempDir::new().unwrap();
    let original = "[agent]\napproval_policy = \"auto-always\"\n";

    let (result, on_disk) = write_under_home(
        default_home(&dir),
        "config.toml",
        original,
        serde_json::json!({
            "path": "config.toml",
            "operation": "append",
            "content": "allowed = [\n",
        }),
    )
    .await;

    assert!(
        !result.success,
        "an append that breaks parsing must be refused"
    );
    assert_eq!(on_disk, original, "the previous bytes must survive");
}

/// Only the profile's own `config.toml` is guarded. A `config.toml` in a
/// subdirectory of the home is a different file with a different purpose, so
/// the guard must not reach it. Pinned here because a blanket file-name check
/// would widen the gate past what #2099 asked for.
#[tokio::test]
async fn test_non_profile_config_name_is_not_guarded() {
    let dir = TempDir::new().unwrap();
    let home = default_home(&dir);

    let (result, _) = write_under_home(
        home,
        "notes/config.toml",
        "",
        serde_json::json!({
            "path": "notes/config.toml",
            "operation": "overwrite",
            "content": "not [ valid",
        }),
    )
    .await;

    assert!(
        result.success,
        "only the profile's real config.toml is protected: {:?}",
        result.error
    );
}
