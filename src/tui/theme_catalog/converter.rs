//! The fixed 43-role mapping and TOML emission shared by both source
//! formats. Pipeline: [`SourcePalette`] -> role map -> contrast repair ->
//! emit -> certify through the real validator
//! ([`crate::tui::render::user_themes::build_theme`]). A returned `Ok`
//! string is therefore guaranteed to load unchanged — the hard invariant
//! from #1461.
//!
//! The mapping is many-to-one and deliberately boring: every role has
//! exactly one derivation rule below, identical for every source theme, so
//! the catalog stays consistent and regenerating a pack is reproducible.

use ratatui::style::Color;

use super::{SourcePalette, mix};
use crate::tui::render::theme::Role;
use crate::tui::render::user_themes::{
    CONTRAST_FLOOR, build_theme, contrast, contrast_pairs_for, is_light_canvas,
};

/// Convert an alacritty-theme TOML document into OpenCrabs user-theme TOML.
/// `provenance` is embedded as a `# source:` comment header (free-form,
/// e.g. "alacritty/alacritty-theme themes/tokyo_night.toml, fetched
/// 2026-09-11"). The colors are certified through `build_theme` under a
/// shadow stem; collision of `name` with a built-in is NOT checked here
/// (regeneration legitimately reuses shipped pack names) — that gate lives
/// in the loader, which rejects colliding stems at install time.
pub fn convert_alacritty_toml(name: &str, src: &str, provenance: &str) -> Result<String, String> {
    let palette = super::alacritty::parse(src)?;
    convert(name, &palette, provenance)
}

/// Convert an opencode theme JSON document (resolving `dark`/`light`
/// variant tokens per `variant`) into OpenCrabs user-theme TOML. Same
/// certification guarantee as [`convert_alacritty_toml`].
pub fn convert_opencode_json(
    name: &str,
    src: &str,
    variant: super::Variant,
    provenance: &str,
) -> Result<String, String> {
    let palette = super::opencode::parse(src, variant)?;
    convert(name, &palette, provenance)
}

/// Shared tail of both conversions.
fn convert(name: &str, pal: &SourcePalette, provenance: &str) -> Result<String, String> {
    let mut roles = map_roles(pal);
    repair_contrast(&mut roles, pal.bg)?;
    let text = emit(provenance, &roles, pal.bg);
    // Certify under a shadow stem: once the pack ships as built-ins,
    // `build_theme(name, ...)` would reject the pack's own names as
    // collisions and regeneration would break. The stem feeds only the
    // collision check and the leaked display name; the 43 colors and the
    // contrast floor (what the invariant is about) validate identically.
    // Install-time collision stays the loader's job.
    build_theme(&format!("certify-{name}"), &text)
        .map_err(|e| format!("validator rejected generated theme '{name}': {e}"))?;
    Ok(text)
}

/// The colour on the `bg -> fg` ramp that just clears `target`:1, found by
/// bisecting the blend. A fixed fraction of the foreground is what a dark
/// palette can afford; on a light canvas the same fraction is mud (15% of a
/// dark foreground is a mid grey, not a readable tier), so the reading tiers
/// of a light source are solved for their floor instead (#2492). Returns
/// `fg` when the palette holds nothing stronger, and the repair step below
/// reports that rather than hiding it.
fn tier_at_contrast(bg: Color, fg: Color, target: f64) -> Color {
    if contrast(fg, bg) <= target {
        return fg;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast(mix(bg, fg, mid), bg) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // `mix` rounds to 8-bit, so the bisection point can land a hundredth
    // under the floor. Walk forward until the emitted colour clears it.
    let mut t = hi;
    while t < 1.0 && contrast(mix(bg, fg, t), bg) < target {
        t = (t + 0.002_f64).min(1.0);
    }
    mix(bg, fg, t)
}

/// The nearest colour to `from`, walking toward `target`, that clears
/// `floor` against `bg`. `tier_at_contrast` answers a different question:
/// it starts at the canvas and returns the palest value that still reads,
/// which is right for deriving a dim tier and wrong for repairing a colour
/// the palette already chose. Repairing `accent` that way turned a saturated
/// orange into a pale grey (#2492). This keeps the hue and moves it only as
/// far as the floor demands, returning `target` when the palette cannot.
fn push_clear(bg: Color, from: Color, target: Color, floor: f64) -> Color {
    if contrast(from, bg) >= floor {
        return from;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast(mix(from, target, mid), bg) < floor {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut t = hi;
    while t < 1.0 && contrast(mix(from, target, t), bg) < floor {
        t = (t + 0.002_f64).min(1.0);
    }
    mix(from, target, t)
}

/// The fixed mapping, in canonical emission order (the field order of the
/// `UserThemeFile` schema). `mix(a, b, t)` blends toward `b`, so the gray
/// ladder is bg(0.0) -> fg(1.0) and dimmed texts are fg -> bg. Semantic
/// tokens from richer sources (opencode) win over ANSI-slot fallbacks.
///
/// A dark canvas keeps every fraction below byte for byte. A light canvas
/// solves the reading tiers for their floor instead: the fractions that read
/// as quiet on a dark background vanish on a light one, and the resulting
/// theme has to certify against the light floors anyway.
fn map_roles(p: &SourcePalette) -> [(Role, Color); 43] {
    use Role::*;
    let light = is_light_canvas(p.bg);
    let tier = |floor: f64| -> Option<Color> { light.then(|| tier_at_contrast(p.bg, p.fg, floor)) };
    // `accent_soft` is drawn as text (onboarding, selection foreground) but
    // maps from `bright_white`, which on a light source is a near-white
    // plate. Re-derive it from the accent when the accent can carry body
    // text contrast, otherwise from the primary ink.
    let accent_soft = if light && contrast(p.bright_white, p.bg) < CONTRAST_FLOOR {
        let tint = p
            .accent
            .filter(|a| contrast(*a, p.bg) >= 3.5)
            .unwrap_or(p.fg);
        tier_at_contrast(p.bg, tint, 4.5)
    } else {
        p.bright_white
    };
    [
        (Accent, p.accent.unwrap_or(p.bright_yellow)),
        (AccentTeal, p.cyan),
        (AccentSoft, accent_soft),
        (TextPrimary, p.fg),
        (
            TextSecondary,
            tier(4.5).unwrap_or_else(|| p.muted.unwrap_or_else(|| mix(p.fg, p.bg, 0.85))),
        ),
        (
            TextMuted,
            tier(4.0).unwrap_or_else(|| mix(p.fg, p.bg, 0.62)),
        ),
        (TextDim, tier(3.5).unwrap_or_else(|| mix(p.fg, p.bg, 0.42))),
        (Gray, p.border.unwrap_or_else(|| mix(p.bg, p.fg, 0.75))),
        (GrayMid, mix(p.bg, p.fg, 0.58)),
        (
            GrayDetail,
            tier(3.5).unwrap_or_else(|| mix(p.bg, p.fg, 0.52)),
        ),
        (
            GrayDim,
            tier(CONTRAST_FLOOR).unwrap_or_else(|| mix(p.bg, p.fg, 0.42)),
        ),
        (GrayDark, mix(p.bg, p.fg, 0.32)),
        (
            GrayBase,
            p.border_subtle.unwrap_or_else(|| mix(p.bg, p.fg, 0.22)),
        ),
        (GrayLight, mix(p.bg, p.fg, 0.85)),
        (GraySoft, tier(4.5).unwrap_or_else(|| mix(p.bg, p.fg, 0.68))),
        (
            GrayMuted,
            tier(4.0).unwrap_or_else(|| mix(p.bg, p.fg, 0.50)),
        ),
        (Success, p.green),
        (AnalyticsGreen, p.bright_green),
        (GreenCheck, p.bright_green),
        (Error, p.red),
        (ErrorSoft, p.bright_red),
        (ErrorFaded, mix(p.red, p.bg, 0.55)),
        (Warning, p.yellow),
        (WarningMuted, mix(p.yellow, p.bg, 0.30)),
        (AmberMuted, mix(p.yellow, p.bg, 0.45)),
        (TealVivid, p.bright_cyan),
        (TealBright, p.cyan),
        (TealMuted, mix(p.cyan, p.bg, 0.35)),
        (TealCalm, mix(p.cyan, p.bg, 0.55)),
        (BlueSlate, mix(p.blue, p.bg, 0.35)),
        (BlueSteel, p.blue),
        (BlueLink, p.link.unwrap_or(p.bright_blue)),
        (BlueSky, p.bright_blue),
        (BlueSoft, mix(p.blue, p.bg, 0.40)),
        (BlueVivid, p.info.unwrap_or(p.bright_blue)),
        (BlueCode, p.bright_blue),
        (
            SelectionBg,
            p.selection.unwrap_or_else(|| mix(p.bg, p.fg, 0.18)),
        ),
        (
            SurfacePanel,
            p.panel.unwrap_or_else(|| mix(p.bg, p.fg, 0.06)),
        ),
        // Preset convention: surface_qr sits next to ink (dracula uses bg
        // verbatim, mocha its mantle), never a bright plate.
        (SurfaceQr, mix(p.bg, p.fg, 0.03)),
        (
            SurfaceCode,
            p.element.unwrap_or_else(|| mix(p.bg, p.fg, 0.10)),
        ),
        (SurfaceCodeAlt, mix(p.bg, p.fg, 0.14)),
        (Ink, p.bg),
        (PurpleSoft, p.purple.unwrap_or(p.bright_magenta)),
    ]
}

/// Enforce the validator's floors up front, pair by pair. A dark canvas uses
/// the historical ladder: nudge the foreground toward whichever extreme
/// (white or black) contrasts better against its pair background, in 10%
/// steps, capped at 24 (asymptotic: reaches ~92% of the way to the extreme).
/// Because white-vs-black is 21:1, at least one extreme always clears the
/// 2.5:1 floor against any background, so the error path is a belt-and-braces
/// invariant guard rather than a reachable failure for real sources.
///
/// A light canvas repairs the same way but solves for the pair's own floor
/// instead of stepping: those floors run 2.5:1 to 4.5:1 and a 10% ladder
/// would need dozens of passes to crawl there. Each fg role appears in
/// exactly one pair, and no role is both an fg and a bg across pairs, so
/// sequential repair cannot undo an earlier pair.
fn repair_contrast(roles: &mut [(Role, Color); 43], canvas: Color) -> Result<(), String> {
    const WHITE: Color = Color::Rgb(255, 255, 255);
    const BLACK: Color = Color::Rgb(0, 0, 0);
    let light = is_light_canvas(canvas);
    for (fg_role, bg_role, floor) in contrast_pairs_for(canvas) {
        let bg = lookup(roles, bg_role);
        let target = if contrast(WHITE, bg) >= contrast(BLACK, bg) {
            WHITE
        } else {
            BLACK
        };
        if light {
            let cur = lookup(roles, fg_role);
            if contrast(cur, bg) < floor {
                set(roles, fg_role, push_clear(bg, cur, target, floor));
            }
        } else {
            let mut steps = 0;
            while contrast(lookup(roles, fg_role), bg) < floor && steps < 24 {
                let cur = lookup(roles, fg_role);
                set(roles, fg_role, mix(cur, target, 0.10));
                steps += 1;
            }
        }
        let ratio = contrast(lookup(roles, fg_role), bg);
        if ratio < floor {
            return Err(format!(
                "contrast repair failed for {} on {} ({ratio:.2}:1 < {floor:.1}:1)",
                key_of(fg_role),
                key_of(bg_role),
            ));
        }
    }
    Ok(())
}

fn lookup(roles: &[(Role, Color); 43], role: Role) -> Color {
    roles
        .iter()
        .find(|(r, _)| *r == role)
        .map(|(_, c)| *c)
        .unwrap_or(Color::Rgb(255, 0, 255))
}

fn set(roles: &mut [(Role, Color); 43], role: Role, color: Color) {
    if let Some(slot) = roles.iter_mut().find(|(r, _)| *r == role) {
        slot.1 = color;
    }
}

/// Emit the flat 43-key TOML, keys in canonical schema order, with the
/// provenance header. TOML comments survive parsing (the validator only
/// rejects unknown *keys*), so provenance rides inside the shipped file.
fn emit(provenance: &str, roles: &[(Role, Color); 43], background: Color) -> String {
    let mut out = String::with_capacity(1400);
    for line in provenance.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("# generated by tui::theme_catalog::converter, fixed 43-role mapping (#1461)\n");
    for (role, color) in roles {
        let Color::Rgb(r, g, b) = *color else {
            continue; // unreachable: every derivation produces Rgb
        };
        out.push_str(&format!("{} = \"#{r:02x}{g:02x}{b:02x}\"\n", key_of(*role)));
    }
    // The canvas (#1634). Not a `Role` and so not in the 43-role array:
    // it is the source's own declared background, emitted verbatim rather
    // than derived, and a theme may decline to declare one.
    if let Color::Rgb(r, g, b) = background {
        out.push_str(&format!("background = \"#{r:02x}{g:02x}{b:02x}\"\n"));
    }
    out
}

/// Role -> TOML key. An exhaustive match on purpose: adding a variant to
/// `Role` breaks compilation here until the mapping and emission are
/// extended, which is exactly the coupling #1461 wants.
fn key_of(role: Role) -> &'static str {
    match role {
        Role::Accent => "accent",
        Role::AccentTeal => "accent_teal",
        Role::AccentSoft => "accent_soft",
        Role::TextPrimary => "text_primary",
        Role::TextSecondary => "text_secondary",
        Role::TextMuted => "text_muted",
        Role::TextDim => "text_dim",
        Role::Gray => "gray",
        Role::GrayMid => "gray_mid",
        Role::GrayDetail => "gray_detail",
        Role::GrayDim => "gray_dim",
        Role::GrayDark => "gray_dark",
        Role::GrayBase => "gray_base",
        Role::GrayLight => "gray_light",
        Role::GraySoft => "gray_soft",
        Role::GrayMuted => "gray_muted",
        Role::Success => "success",
        Role::AnalyticsGreen => "analytics_green",
        Role::GreenCheck => "green_check",
        Role::Error => "error",
        Role::ErrorSoft => "error_soft",
        Role::ErrorFaded => "error_faded",
        Role::Warning => "warning",
        Role::WarningMuted => "warning_muted",
        Role::AmberMuted => "amber_muted",
        Role::TealVivid => "teal_vivid",
        Role::TealBright => "teal_bright",
        Role::TealMuted => "teal_muted",
        Role::TealCalm => "teal_calm",
        Role::BlueSlate => "blue_slate",
        Role::BlueSteel => "blue_steel",
        Role::BlueLink => "blue_link",
        Role::BlueSky => "blue_sky",
        Role::BlueSoft => "blue_soft",
        Role::BlueVivid => "blue_vivid",
        Role::BlueCode => "blue_code",
        Role::SelectionBg => "selection_bg",
        Role::SurfacePanel => "surface_panel",
        Role::SurfaceQr => "surface_qr",
        Role::SurfaceCode => "surface_code",
        Role::SurfaceCodeAlt => "surface_code_alt",
        Role::Ink => "ink",
        Role::PurpleSoft => "purple_soft",
        Role::BorderModal => "border_modal",
    }
}
