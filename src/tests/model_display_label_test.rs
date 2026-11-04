//! #754: the claude-cli model picker must format every row consistently.
//!
//! Before this, `prettify_claude_cli_model` only understood two-part versions
//! (`opus-4-8`), so a major-only release (`opus-5`) fell through to its raw id
//! and sat un-formatted directly below "Opus 4.8". Separately, the bare alias
//! `fable` and the version `fable-5` both rendered "Fable 5", putting the same
//! label on two different rows.

use crate::tui::provider_selector::model_display_label;

#[test]
fn major_only_versions_are_formatted() {
    assert_eq!(model_display_label("opus-5"), "Opus 5");
    assert_eq!(model_display_label("sonnet-5"), "Sonnet 5");
    assert_eq!(model_display_label("fable-5"), "Fable 5");
}

#[test]
fn major_minor_versions_still_formatted() {
    assert_eq!(model_display_label("opus-4-8"), "Opus 4.8");
    assert_eq!(model_display_label("opus-4-7"), "Opus 4.7");
    assert_eq!(model_display_label("sonnet-4-6"), "Sonnet 4.6");
    assert_eq!(model_display_label("haiku-4-5"), "Haiku 4.5");
}

#[test]
fn bare_aliases_are_labelled_as_the_moving_latest_pointer() {
    // They point at whatever is newest, so they must not carry a fixed version.
    assert_eq!(model_display_label("opus"), "Opus (latest)");
    assert_eq!(model_display_label("fable"), "Fable (latest)");
    assert_eq!(model_display_label("sonnet"), "Sonnet (latest)");
    assert_eq!(model_display_label("haiku"), "Haiku (latest)");
}

#[test]
fn no_two_claude_models_share_a_label() {
    // The picker lists these together; duplicate labels are unreadable.
    let ids = crate::brain::provider::claude_cli::available_models();
    let mut labels: Vec<&str> = ids.iter().map(|m| model_display_label(m)).collect();
    labels.sort_unstable();
    let before = labels.len();
    labels.dedup();
    assert_eq!(
        before,
        labels.len(),
        "duplicate labels in the picker: {ids:?}"
    );
}

/// The families [`model_display_label`] knows how to format. Anything else the
/// `claude` CLI has recorded on this machine (it can be pointed at a gateway
/// that serves another provider's model) has no formatting scheme of its own.
fn is_claude_family(id: &str) -> bool {
    ["fable", "sonnet", "opus", "haiku"]
        .iter()
        .any(|f| id == *f || id.starts_with(&format!("{f}-")))
}

#[test]
fn every_claude_model_renders_formatted_not_raw() {
    // No Claude row may show a raw lower-case id while its neighbours are
    // formatted. A model the CLI ran from another provider is not a Claude row:
    // it keeps its own name, which is asserted rather than skipped, so neither
    // branch can pass by finding nothing to check.
    let mut claude_rows = 0;
    for id in crate::brain::provider::claude_cli::available_models() {
        let label = model_display_label(&id);
        if is_claude_family(&id) {
            claude_rows += 1;
            assert!(
                label.starts_with(|c: char| c.is_ascii_uppercase()),
                "model {id:?} renders un-formatted as {label:?}"
            );
        } else {
            assert_eq!(
                label, id,
                "a non-Claude model must render as its own id, not a Claude label"
            );
        }
    }
    assert!(
        claude_rows > 0,
        "the picker must always offer at least one Claude model"
    );
}

#[test]
fn nonsense_versions_are_left_alone() {
    assert_eq!(model_display_label("opus-x"), "opus-x");
    assert_eq!(model_display_label("opus-"), "opus-");
}

#[test]
fn picker_orders_aliases_first_then_newest_version_per_family() {
    // #755: the list came out in merge order, so Opus 5 rendered BELOW Opus 4.8
    // and above Opus 4.7 — the newest release looking older than its neighbour.
    let ids = crate::brain::provider::claude_cli::available_models();
    let pos = |needle: &str| ids.iter().position(|m| m == needle);

    // Bare aliases lead the list.
    let first_versioned = ids
        .iter()
        .position(|m| m.contains('-'))
        .expect("expected at least one versioned model");
    for (i, m) in ids.iter().enumerate() {
        if i < first_versioned {
            assert!(!m.contains('-'), "alias block contains a version: {m:?}");
        }
    }

    // Newest first within a family: 5 outranks 4.8 outranks 4.7 outranks 4.6.
    if let (Some(o5), Some(o48)) = (pos("opus-5"), pos("opus-4-8")) {
        assert!(o5 < o48, "opus-5 must sort above opus-4-8: {ids:?}");
    }
    if let (Some(o48), Some(o47)) = (pos("opus-4-8"), pos("opus-4-7")) {
        assert!(o48 < o47, "opus-4-8 must sort above opus-4-7: {ids:?}");
    }
    if let (Some(o47), Some(o46)) = (pos("opus-4-7"), pos("opus-4-6")) {
        assert!(o47 < o46, "opus-4-7 must sort above opus-4-6: {ids:?}");
    }
}
