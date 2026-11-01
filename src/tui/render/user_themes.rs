//! User theme presets, loaded at runtime from `~/.opencrabs/themes/*.toml`.
//!
//! A preset file is a flat map of the 43 theme roles to `#RRGGBB` hexes —
//! the same schema the built-in presets use, spelled in snake_case:
//!
//! ```toml
//! # ~/.opencrabs/themes/my-theme.toml — the file stem IS the theme name.
//! accent = "#FFB86C"
//! accent_teal = "#8BE9FD"
//! # ... all 43 roles required; unknown keys are rejected (typo guard)
//! ink = "#282A36"
//! ```
//!
//! Validation is loud, never silent: a file that fails to parse, misses a
//! role, carries an unknown key, or scores below the contrast floor is
//! rejected with a human-readable reason surfaced through `/theme list`.
//! Valid presets additionally get their ANSI-256 tier derived for free via
//! [`rgb_to_ansi256`], so degraded terminals render them too.
//!
//! Loaded themes are `Box::leak`ed into a process-lifetime registry. Each
//! explicit `reload()` re-leaks — bounded by (file count × invocations),
//! a few hundred bytes each, acceptable for an interactive TUI.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::RwLock;

use ratatui::style::Color;
use serde::Deserialize;

use super::theme::{AnsiColors, Role, Theme, ThemeColors, rgb_to_ansi256};

/// Contrast floor for readable role pairs. 2.5:1 keeps `crab-dark` (the
/// reference preset, whose dimmest validated pair — GrayMid on SurfacePanel —
/// sits at ≈2.77:1) passing while catching unreadable combos like
/// same-on-same or near-black-on-black.
pub(crate) const CONTRAST_FLOOR: f64 = 2.5;

/// (foreground, background) role pairs that must stay readable. Deliberately
/// dim decoration tiers (TextMuted, TextDim, GrayDim…) are excluded — dim is
/// their job. Reading-critical roles only.
/// `pub(crate)` for the theme tests in `src/tests` — every test in this
/// repository lives there, so the pair table has to be reachable from it.
pub(crate) const CONTRAST_PAIRS: &[(Role, Role)] = &[
    (Role::TextPrimary, Role::Ink),
    (Role::TextSecondary, Role::Ink),
    (Role::Accent, Role::Ink),
    (Role::Success, Role::Ink),
    (Role::Error, Role::Ink),
    (Role::Warning, Role::Ink),
    (Role::BlueCode, Role::SurfaceCode),
    (Role::GrayMid, Role::SurfacePanel),
];

/// Raw TOML shape. `deny_unknown_fields` turns typos into parse errors;
/// non-`Option` fields make `toml` name any missing role in its error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserThemeFile {
    accent: String,
    accent_teal: String,
    accent_soft: String,
    text_primary: String,
    text_secondary: String,
    text_muted: String,
    text_dim: String,
    gray: String,
    gray_mid: String,
    gray_detail: String,
    gray_dim: String,
    gray_dark: String,
    gray_base: String,
    gray_light: String,
    gray_soft: String,
    gray_muted: String,
    success: String,
    analytics_green: String,
    green_check: String,
    error: String,
    error_soft: String,
    error_faded: String,
    warning: String,
    warning_muted: String,
    amber_muted: String,
    teal_vivid: String,
    teal_bright: String,
    teal_muted: String,
    teal_calm: String,
    blue_slate: String,
    blue_steel: String,
    blue_link: String,
    blue_sky: String,
    blue_soft: String,
    blue_vivid: String,
    blue_code: String,
    selection_bg: String,
    surface_panel: String,
    surface_qr: String,
    surface_code: String,
    surface_code_alt: String,
    ink: String,
    purple_soft: String,
    /// Modal dialog borders (#1775). Optional: absent falls back to
    /// accent_teal, matching the built-in default.
    #[serde(default)]
    border_modal: Option<String>,
    /// Canvas background (#1634). Optional so every theme file written
    /// before the canvas existed stays valid: absent means the theme
    /// declares none and the terminal's own background shows through.
    #[serde(default)]
    background: Option<String>,
}

/// A preset file that failed validation, with the reason.
#[derive(Debug)]
pub struct RejectedPreset {
    pub file: String,
    pub reason: String,
}

/// Outcome of scanning a themes directory.
pub struct LoadReport {
    pub themes: Vec<&'static Theme>,
    pub rejected: Vec<RejectedPreset>,
}

static REGISTRY: RwLock<Vec<&'static Theme>> = RwLock::new(Vec::new());
static BOOT_LOADED: OnceLock<()> = OnceLock::new();

/// Scan `dir` for `*.toml` presets. Sorts accepted themes by name. A missing
/// directory is an empty report, not an error.
pub fn load_from(dir: &Path) -> LoadReport {
    let mut themes: Vec<&'static Theme> = Vec::new();
    let mut rejected: Vec<RejectedPreset> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return LoadReport { themes, rejected },
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_label = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| stem.clone());
        match std::fs::read_to_string(&path) {
            Ok(text) => match build_theme(&stem, &text) {
                Ok(theme) => themes.push(Box::leak(Box::new(theme))),
                Err(reason) => rejected.push(RejectedPreset {
                    file: file_label,
                    reason,
                }),
            },
            Err(e) => rejected.push(RejectedPreset {
                file: file_label,
                reason: format!("unreadable: {e}"),
            }),
        }
    }

    themes.sort_by_key(|t| t.name);
    LoadReport { themes, rejected }
}

/// The user themes directory: `~/.opencrabs/themes/`.
pub fn themes_dir() -> std::path::PathBuf {
    crate::config::opencrabs_home().join("themes")
}

/// Rescan the themes directory and replace the registry. Called on every
/// `/theme` invocation so file edits hot-load without a restart.
pub fn reload() -> LoadReport {
    let report = load_from(&themes_dir());
    if let Ok(mut reg) = REGISTRY.write() {
        *reg = report.themes.clone();
    }
    report
}

/// Populate the registry once (boot path). Explicit `reload()` supersedes.
fn ensure_loaded() {
    BOOT_LOADED.get_or_init(|| {
        reload();
    });
}

/// Case-insensitive lookup over loaded user presets.
pub fn find(name: &str) -> Option<&'static Theme> {
    ensure_loaded();
    let lowered = name.to_ascii_lowercase();
    REGISTRY
        .read()
        .ok()?
        .iter()
        .copied()
        .find(|t| t.name.to_ascii_lowercase() == lowered)
}

/// Parse + validate one preset. Name collisions with built-ins are rejected
/// so a user file can never shadow a shipped preset.
/// `pub(crate)` for the theme tests in `src/tests`, same reason as
/// [`CONTRAST_PAIRS`].
pub(crate) fn build_theme(stem: &str, text: &str) -> Result<Theme, String> {
    if stem.is_empty() {
        return Err("theme name (file stem) is empty".to_string());
    }
    if super::presets::by_name(stem).is_some() {
        return Err(format!("name '{stem}' collides with a built-in preset"));
    }
    let f: UserThemeFile = toml::from_str(text).map_err(|e| format!("parse error: {e}"))?;

    macro_rules! hx {
        ($field:ident) => {
            parse_hex(&f.$field).ok_or_else(|| {
                format!(
                    "{}: invalid hex {:?} (expected \"#RRGGBB\")",
                    stringify!($field),
                    f.$field
                )
            })?
        };
    }
    let accent = hx!(accent);
    let accent_teal = hx!(accent_teal);
    let accent_soft = hx!(accent_soft);
    let text_primary = hx!(text_primary);
    let text_secondary = hx!(text_secondary);
    let text_muted = hx!(text_muted);
    let text_dim = hx!(text_dim);
    let gray = hx!(gray);
    let gray_mid = hx!(gray_mid);
    let gray_detail = hx!(gray_detail);
    let gray_dim = hx!(gray_dim);
    let gray_dark = hx!(gray_dark);
    let gray_base = hx!(gray_base);
    let gray_light = hx!(gray_light);
    let gray_soft = hx!(gray_soft);
    let gray_muted = hx!(gray_muted);
    let success = hx!(success);
    let analytics_green = hx!(analytics_green);
    let green_check = hx!(green_check);
    let error = hx!(error);
    let error_soft = hx!(error_soft);
    let error_faded = hx!(error_faded);
    let warning = hx!(warning);
    let warning_muted = hx!(warning_muted);
    let amber_muted = hx!(amber_muted);
    let teal_vivid = hx!(teal_vivid);
    let teal_bright = hx!(teal_bright);
    let teal_muted = hx!(teal_muted);
    let teal_calm = hx!(teal_calm);
    let blue_slate = hx!(blue_slate);
    let blue_steel = hx!(blue_steel);
    let blue_link = hx!(blue_link);
    let blue_sky = hx!(blue_sky);
    let blue_soft = hx!(blue_soft);
    let blue_vivid = hx!(blue_vivid);
    let blue_code = hx!(blue_code);
    let selection_bg = hx!(selection_bg);
    let surface_panel = hx!(surface_panel);
    let surface_qr = hx!(surface_qr);
    let surface_code = hx!(surface_code);
    let surface_code_alt = hx!(surface_code_alt);
    let ink = hx!(ink);
    let purple_soft = hx!(purple_soft);
    let border_modal = match f.border_modal.as_deref() {
        Some(raw) => parse_hex(raw)
            .ok_or_else(|| format!("border_modal: invalid hex {raw:?} (expected \"#RRGGBB\")"))?,
        None => accent_teal, // #1775: absent = accent teal, like the default theme
    };
    let background = match f.background.as_deref() {
        Some(raw) => Some(
            parse_hex(raw)
                .ok_or_else(|| format!("background: invalid hex {raw:?} (expected \"#RRGGBB\")"))?,
        ),
        None => None,
    };
    let ansi = AnsiColors {
        accent: quant(accent),
        accent_teal: quant(accent_teal),
        accent_soft: quant(accent_soft),
        text_primary: quant(text_primary),
        text_secondary: quant(text_secondary),
        text_muted: quant(text_muted),
        text_dim: quant(text_dim),
        gray: quant(gray),
        gray_mid: quant(gray_mid),
        gray_detail: quant(gray_detail),
        gray_dim: quant(gray_dim),
        gray_dark: quant(gray_dark),
        gray_base: quant(gray_base),
        gray_light: quant(gray_light),
        gray_soft: quant(gray_soft),
        gray_muted: quant(gray_muted),
        success: quant(success),
        analytics_green: quant(analytics_green),
        green_check: quant(green_check),
        error: quant(error),
        error_soft: quant(error_soft),
        error_faded: quant(error_faded),
        warning: quant(warning),
        warning_muted: quant(warning_muted),
        amber_muted: quant(amber_muted),
        teal_vivid: quant(teal_vivid),
        teal_bright: quant(teal_bright),
        teal_muted: quant(teal_muted),
        teal_calm: quant(teal_calm),
        blue_slate: quant(blue_slate),
        blue_steel: quant(blue_steel),
        blue_link: quant(blue_link),
        blue_sky: quant(blue_sky),
        blue_soft: quant(blue_soft),
        blue_vivid: quant(blue_vivid),
        blue_code: quant(blue_code),
        selection_bg: quant(selection_bg),
        surface_panel: quant(surface_panel),
        surface_qr: quant(surface_qr),
        surface_code: quant(surface_code),
        surface_code_alt: quant(surface_code_alt),
        ink: quant(ink),
        purple_soft: quant(purple_soft),
        border_modal: quant(border_modal),
        background: background.map(quant),
    };
    let rgb = ThemeColors {
        accent,
        accent_teal,
        accent_soft,
        text_primary,
        text_secondary,
        text_muted,
        text_dim,
        gray,
        gray_mid,
        gray_detail,
        gray_dim,
        gray_dark,
        gray_base,
        gray_light,
        gray_soft,
        gray_muted,
        success,
        analytics_green,
        green_check,
        error,
        error_soft,
        error_faded,
        warning,
        warning_muted,
        amber_muted,
        teal_vivid,
        teal_bright,
        teal_muted,
        teal_calm,
        blue_slate,
        blue_steel,
        blue_link,
        blue_sky,
        blue_soft,
        blue_vivid,
        blue_code,
        selection_bg,
        surface_panel,
        surface_qr,
        surface_code,
        surface_code_alt,
        ink,
        purple_soft,
        border_modal,
        background,
        ansi,
    };

    let theme = Theme {
        name: Box::leak(stem.to_string().into_boxed_str()),
        colors: rgb,
    };
    for &(fg, bg) in CONTRAST_PAIRS {
        let (fgc, bgc) = (theme.colors.get(fg), theme.colors.get(bg));
        let r = contrast(fgc, bgc);
        if r < CONTRAST_FLOOR {
            return Err(format!(
                "contrast {r:.2}:1 below {CONTRAST_FLOOR:.1}:1 for {} on {}",
                role_key(fg),
                role_key(bg)
            ));
        }
    }
    Ok(theme)
}

/// Debug variant name as its TOML key (`TextPrimary` → `text_primary`), so
/// rejection reasons name roles exactly as users write them in preset files.
fn role_key(r: Role) -> String {
    let mut out = String::new();
    for (i, ch) in format!("{r:?}").chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn quant(c: Color) -> u8 {
    match c {
        Color::Rgb(r, g, b) => rgb_to_ansi256(r, g, b),
        Color::Indexed(i) => i,
        _ => 250,
    }
}

/// `#RRGGBB` or `RRGGBB` (case-insensitive) → `Color::Rgb`.
pub(crate) fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// WCAG 2.x relative luminance.
fn relative_luminance(c: Color) -> f64 {
    let chan = |b: u8| -> f64 {
        let x = f64::from(b) / 255.0;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (chan(r), chan(g), chan(b)),
        _ => return 0.0,
    };
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio (1..=21), symmetric in its arguments.
/// `pub(crate)` for the theme tests in `src/tests`, same reason as
/// [`CONTRAST_PAIRS`].
pub(crate) fn contrast(a: Color, b: Color) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every loaded user preset, in the order the registry holds them.
pub fn all() -> Vec<&'static Theme> {
    ensure_loaded();
    REGISTRY.read().map(|reg| reg.clone()).unwrap_or_default()
}
