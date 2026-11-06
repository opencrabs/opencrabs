//! The desktop window's colour model (#2619).
//!
//! Four themes only: `orgn`, the window's own dark palette, `light`, a soft
//! native light palette, `custom`, which paints the saved accent on whichever
//! base the system asks for, and `auto`, which follows the system with no
//! accent override.
//!
//! The resolution is pure and lives here, in the library, so the regression
//! tests reach it without a window. `src/gui/palette.rs` owns the live value
//! and re-exports these names; the TUI's theme catalog plays no part.

use super::types::GuiTheme;

/// Every colour the window draws with, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub panel_hi: u32,
    pub track: u32,
    pub text: u32,
    pub text_strong: u32,
    pub text_normal: u32,
    pub text_muted: u32,
    pub text_title: u32,
    pub text_faint: u32,
    pub text_hint: u32,
    pub green: u32,
    pub blue: u32,
    pub amber: u32,
    pub accent: u32,
    pub teal: u32,
    pub red: u32,
    pub warn: u32,
    /// The focus ring: a soft wash of the accent over the background.
    pub focus: u32,
    /// The selected row's background, a faint wash of the accent.
    pub selection: u32,
}

/// `orgn`, the dark palette the window has always drawn.
pub const ORGN: Palette = Palette {
    bg: 0x101010,
    panel: 0x161616,
    panel_hi: 0x1a1a1a,
    track: 0x242424,
    text: 0xf0f0f0,
    text_strong: 0xe8e8e8,
    text_normal: 0xd0d0d0,
    text_muted: 0x909090,
    text_title: 0x8a8a8a,
    text_faint: 0x606060,
    text_hint: 0x707070,
    green: 0x4f8f6f,
    blue: 0x6f8fbf,
    amber: 0xbf8f5f,
    accent: 0xd76414,
    teal: 0x5fbfbf,
    red: 0xe07050,
    warn: 0xb0885a,
    focus: 0x8a4f24,
    selection: 0x2f1e12,
};

/// `light`, a soft paper palette: warm canvas, near-white panels, same accent.
pub const LIGHT: Palette = Palette {
    bg: 0xf7f5f2,
    panel: 0xfffefc,
    panel_hi: 0xfbf9f6,
    track: 0xe9e4dd,
    text: 0x1b1917,
    text_strong: 0x000000,
    text_normal: 0x3b3833,
    text_muted: 0x615c54,
    text_title: 0x4d4842,
    text_faint: 0x87817a,
    text_hint: 0x736d65,
    green: 0x2c7250,
    blue: 0x2b5a95,
    amber: 0x8a5f2c,
    accent: 0xc2560d,
    teal: 0x1b7a7a,
    red: 0xb33824,
    warn: 0x7a5418,
    focus: 0xe6a878,
    selection: 0xf3e3d3,
};

/// The base a mode draws, before any accent is painted.
pub const fn base_for(mode: GuiTheme, system_dark: bool) -> Palette {
    match (mode, system_dark) {
        (GuiTheme::Orgn, _) => ORGN,
        (GuiTheme::Light, _) => LIGHT,
        (GuiTheme::Auto, dark) | (GuiTheme::Custom, dark) => {
            if dark {
                ORGN
            } else {
                LIGHT
            }
        }
    }
}

/// Whether a mode re-picks its base when the system appearance flips.
pub const fn follows_system(mode: GuiTheme) -> bool {
    matches!(mode, GuiTheme::Auto | GuiTheme::Custom)
}

/// Parse a `#RRGGBB` or `RRGGBB` accent. Anything else is not an accent.
pub fn parse_accent(text: &str) -> Option<u32> {
    let body = text.trim();
    let body = body.strip_prefix('#').unwrap_or(body);
    if body.len() != 6 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(body, 16).ok()
}

/// Render an accent the way `[gui] custom_accent` stores it.
pub fn format_accent(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0x00ff_ffff)
}

/// `base` with `over` mixed in `num/den` of the way. Integer maths, so the
/// result is the same on every platform and easy to pin in a test.
pub fn mix(base: u32, over: u32, num: u32, den: u32) -> u32 {
    let channel = |b: u32, o: u32| ((b * (den - num) + o * num) / den) & 0xff;
    (channel((base >> 16) & 0xff, (over >> 16) & 0xff) << 16)
        | (channel((base >> 8) & 0xff, (over >> 8) & 0xff) << 8)
        | channel(base & 0xff, over & 0xff)
}

/// Paint a palette with a custom accent, deriving the focus ring and the
/// selection wash from it so they never disagree.
///
/// The pick is stored exactly as it was made; `legible_accent` is what the
/// window paints, so a dark accent on a dark base cannot vanish. Only the
/// accent, the focus ring and the selection wash move: semantic and
/// message-role colours keep what the base gave them.
pub fn with_accent(mut palette: Palette, accent: u32) -> Palette {
    let accent = legible_accent(accent, palette);
    palette.accent = accent;
    palette.focus = mix(palette.bg, accent, 55, 100);
    palette.selection = mix(palette.bg, accent, 18, 100);
    palette
}

/// The palette a mode draws, given the system appearance and the saved accent.
///
/// `orgn` and `light` are fixed: they ignore the system. `auto` follows the
/// system. `custom` follows the system too, then paints the accent on top.
/// An accent that does not parse is ignored rather than failing the draw.
pub fn resolve(mode: GuiTheme, system_dark: bool, accent: Option<&str>) -> Palette {
    let base = base_for(mode, system_dark);
    match (mode, accent.and_then(parse_accent)) {
        (GuiTheme::Custom, Some(rgb)) => with_accent(base, rgb),
        _ => base,
    }
}

/// WCAG relative luminance of a 0xRRGGBB colour, for the contrast tests.
pub fn relative_luminance(rgb: u32) -> f64 {
    let channel = |shift: u32| {
        let v = ((rgb >> shift) & 0xff) as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// The contrast ratio between two colours, 1:1 to 21:1.
pub fn contrast_ratio(fg: u32, bg: u32) -> f64 {
    let a = relative_luminance(fg);
    let b = relative_luminance(bg);
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// HSV to 0xRRGGBB. `hue` is degrees (wrapped into 0..360), `saturation` and
/// `value` are 0.0 to 1.0 and are clamped, so a drag off the wheel's edge
/// cannot produce a colour outside the cube.
pub fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> u32 {
    let h = hue.rem_euclid(360.0);
    let s = saturation.clamp(0.0, 1.0);
    let v = value.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let channel = |n: f32| (((n + m) * 255.0).round() as u32).min(255);
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// 0xRRGGBB to `(hue degrees, saturation, value)`.
pub fn rgb_to_hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue.rem_euclid(360.0), saturation, max)
}

/// Where a hue sits on the wheel, as a unit offset from the centre with
/// `(0, -1)` at the top. The wheel is read clockwise, the usual direction.
pub fn hue_to_unit(hue: f32) -> (f32, f32) {
    let radians = hue.rem_euclid(360.0).to_radians();
    (radians.sin(), -radians.cos())
}

/// The hue under a point on the wheel, given its offset from the centre.
/// A point exactly at the centre has no angle, so it answers red.
pub fn hue_from_unit(x: f32, y: f32) -> f32 {
    if x.abs() <= f32::EPSILON && y.abs() <= f32::EPSILON {
        return 0.0;
    }
    x.atan2(-y).to_degrees().rem_euclid(360.0)
}

/// The default accent the wheel opens on when nothing is saved yet.
pub const DEFAULT_ACCENT: u32 = ORGN.accent;

/// The wheel's grab tolerance: a press slightly outside the rim still drags,
/// so a fast flick outward keeps the colour it had instead of cancelling.
pub const WHEEL_GRAB: f32 = 1.15;

/// The contrast an accent needs against the base it sits on before it is
/// nudged towards the base's opposite. 3:1 is the WCAG floor for non-text UI,
/// which is what the accent, the focus ring and the selection wash are.
pub const MIN_ACCENT_CONTRAST: f64 = 3.0;

/// A point on the wheel, resolved into the two values a drag edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelHit {
    /// Degrees, 0 at the top, clockwise.
    pub hue: f32,
    /// 0.0 at the centre, 1.0 at the rim.
    pub saturation: f32,
}

/// Clamp a unit control, tolerating a pointer that ran off its track.
pub fn clamp_unit(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Whether a press is inside a painted box, with `slack` pixels of grace so a
/// drag that slips a little off an edge does not drop the control.
pub fn inside(bounds: [f32; 4], x: f32, y: f32, slack: f32) -> bool {
    let [x0, y0, x1, y1] = bounds;
    x >= x0 - slack && x <= x1 + slack && y >= y0 - slack && y <= y1 + slack
}

/// Turn a press in window coordinates into a wheel position.
///
/// `bounds` is the wheel's painted box as `[x0, y0, x1, y1]`. A point past the
/// grab tolerance, or a box with no area, is not a press on the wheel and
/// answers `None`; a point inside pins the saturation at the rim rather than
/// wrapping, so a flick outward keeps the hue it had.
pub fn wheel_hit(bounds: [f32; 4], x: f32, y: f32) -> Option<WheelHit> {
    let read = wheel_read(bounds, x, y)?;
    (read.2 <= WHEEL_GRAB).then_some(WheelHit {
        hue: read.0,
        saturation: read.1,
    })
}

/// The wheel position under a pointer that is already being dragged.
///
/// A drag does not have to stay on the wheel: once it holds the control, the
/// angle is read from wherever the pointer went and the saturation pins at the
/// rim, so a fast flick outward keeps the hue it had instead of stalling.
pub fn wheel_drag(bounds: [f32; 4], x: f32, y: f32) -> Option<WheelHit> {
    let read = wheel_read(bounds, x, y)?;
    Some(WheelHit {
        hue: read.0,
        saturation: read.1,
    })
}

/// `(hue, clamped saturation, raw distance)` for a pointer, or `None` when the
/// box has no area to read from.
fn wheel_read(bounds: [f32; 4], x: f32, y: f32) -> Option<(f32, f32, f32)> {
    let [x0, y0, x1, y1] = bounds;
    let radius = (x1 - x0).min(y1 - y0) / 2.0;
    if radius <= 0.0 {
        return None;
    }
    let dx = (x - (x0 + x1) / 2.0) / radius;
    let dy = (y - (y0 + y1) / 2.0) / radius;
    let distance = (dx * dx + dy * dy).sqrt();
    Some((hue_from_unit(dx, dy), clamp_unit(distance), distance))
}

/// Where a press on a horizontal slider sits, 0.0 at its left edge and 1.0 at
/// its right. A track with no width answers 0.0, so a layout that has not been
/// painted yet cannot produce a value.
pub fn slider_value(bounds: [f32; 4], x: f32) -> f32 {
    let [x0, _, x1, _] = bounds;
    let width = x1 - x0;
    if width <= 0.0 {
        return 0.0;
    }
    clamp_unit((x - x0) / width)
}

/// The accent the window paints, given the RGB the config stores.
///
/// The pick is saved exactly as it was made; this is the display value, moved
/// towards the base's opposite only as far as it has to be to stay visible. A
/// pick that already reads on the base is returned untouched, so the wheel's
/// swatch, the stored hex and the ring cannot disagree.
pub fn legible_accent(picked: u32, base: Palette) -> u32 {
    let picked = picked & 0x00ff_ffff;
    if contrast_ratio(picked, base.bg) >= MIN_ACCENT_CONTRAST {
        return picked;
    }
    let towards = if relative_luminance(base.bg) < 0.5 {
        0x00ff_ffff
    } else {
        0x000000
    };
    let mut step = 1;
    while step < 20 {
        let candidate = mix(picked, towards, step, 20);
        if contrast_ratio(candidate, base.bg) >= MIN_ACCENT_CONTRAST {
            return candidate;
        }
        step += 1;
    }
    towards
}
