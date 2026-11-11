//! The desktop window's typeface model (#2786).
//!
//! The window draws in the platform's own UI font unless the user picks
//! another family in the theme modal. Nothing is bundled: the list the modal
//! offers is the machine's own families, so unicode coverage and the first
//! paint stay the OS's job.
//!
//! The resolution is pure and lives here, in the library, so the regression
//! tests reach it without a window. `src/gui/fonts.rs` owns the live values and
//! the one-time enumeration.

/// gpui's alias for the platform UI font: `.AppleSystemUIFont` on macOS, the
/// DirectWrite system font on Windows, the desktop's own face on Linux.
pub const SYSTEM_FAMILY: &str = ".SystemUIFont";

/// The spelling `[gui] font = "system"` accepts for the same thing.
pub const SYSTEM_KEY: &str = "system";

/// The spelling `[gui] mono_font = "auto"` accepts for the walk below.
pub const AUTO_KEY: &str = "auto";

/// The mono families the walk tries, best first. Every one of these ships with
/// at least one supported platform, so the walk lands on a real mono on a
/// machine nobody configured.
pub const MONO_CANDIDATES: &[&str] = &[
    "JetBrains Mono",
    "SF Mono",
    "Menlo",
    "Consolas",
    "Cascadia Code",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Courier New",
];

/// What a platform with none of the candidates falls back to. A generic family
/// name is resolved by the platform, so it still answers a mono rather than a
/// proportional face.
pub const MONO_LAST_RESORT: &str = "monospace";

/// Whether `family` asks for the platform's own UI font. An empty string counts
/// as asking for it, because that is what an unset key leaves behind.
pub fn is_system(family: &str) -> bool {
    let family = family.trim();
    family.is_empty() || family.eq_ignore_ascii_case(SYSTEM_KEY) || family == SYSTEM_FAMILY
}

/// The family the window draws text in. `None`, an empty string and `system`
/// all mean the platform's own UI font.
pub fn resolve_ui(saved: Option<&str>) -> String {
    match saved {
        Some(name) if !is_system(name) => name.trim().to_string(),
        _ => SYSTEM_FAMILY.to_string(),
    }
}

/// The installed spelling of `family`, so the name the window paints is the one
/// the platform reports rather than the one the config happens to hold.
pub fn installed_spelling(installed: &[String], family: &str) -> Option<String> {
    installed
        .iter()
        .find(|name| name.eq_ignore_ascii_case(family))
        .cloned()
}

/// The first installed family from the mono walk.
pub fn first_mono(installed: &[String]) -> Option<String> {
    MONO_CANDIDATES
        .iter()
        .find_map(|family| installed_spelling(installed, family))
}

/// The family code blocks draw in.
///
/// `None`, an empty string and `auto` run the walk. A saved family that is
/// installed is kept, spelled the way the platform reports it. A saved family
/// that is not installed falls back to the walk instead of painting a name the
/// machine cannot resolve: a proportional face in a code block silently loses
/// column alignment, which is worse than a different mono.
pub fn resolve_mono(saved: Option<&str>, installed: &[String]) -> String {
    let named = saved
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(AUTO_KEY));
    if let Some(name) = named
        && let Some(spelling) = installed_spelling(installed, name)
    {
        return spelling;
    }
    first_mono(installed).unwrap_or_else(|| MONO_LAST_RESORT.to_string())
}

/// The rows the modal's font list draws: the platform UI font first, then the
/// installed families in case-insensitive order, with no duplicates.
///
/// The enumeration already carries `.SystemUIFont`, so the list is deduped
/// rather than trusting the platform to report it once.
pub fn picker_rows(installed: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = vec![SYSTEM_FAMILY.to_string()];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(SYSTEM_FAMILY.to_lowercase());
    let mut rest: Vec<&String> = installed.iter().collect();
    rest.sort_by_key(|name| name.to_lowercase());
    for name in rest {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            rows.push(trimmed.to_string());
        }
    }
    rows
}

/// The label a picker row shows. The platform alias is not a name a user
/// recognises, so it reads as the system font.
pub fn row_label(family: &str) -> String {
    if is_system(family) {
        "System (default)".to_string()
    } else {
        family.to_string()
    }
}
