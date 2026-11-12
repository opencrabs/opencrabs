//! The normalized palette every source format is parsed into (#1461).
//!
//! Both converters read a third-party theme into one of these, then run the
//! fixed 43-role mapping over it. The type lives here rather than in the
//! module root so `mod.rs` stays a list of declarations, which is what
//! `CONTRIBUTING.md` asks of every `mod.rs` in the tree.

use ratatui::style::Color;

/// Which variant to resolve when a source format carries light/dark pairs
/// (opencode JSON `{dark, light}` token values). alacritty-theme files are
/// single-variant per file, so this only applies to opencode sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Dark,
    Light,
}

/// The normalized color set both source formats are parsed into before the
/// fixed 43-role mapping runs. The 18 required fields are the classic
/// terminal palette (bg/fg + 8 normal + 8 bright ANSI); the optional fields
/// carry semantic tokens only some sources provide (opencode). The mapping
/// prefers a semantic token when present and falls back to ANSI colors or
/// deterministic mixes of bg/fg otherwise.
#[derive(Debug, Clone)]
pub struct SourcePalette {
    pub bg: Color,
    pub fg: Color,
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
    /// opencode `accent`.
    pub accent: Option<Color>,
    /// opencode `secondary` (purple-ish by convention).
    pub purple: Option<Color>,
    /// opencode `info`.
    pub info: Option<Color>,
    /// opencode `markdownLink`.
    pub link: Option<Color>,
    /// opencode `textMuted`.
    pub muted: Option<Color>,
    /// opencode `backgroundPanel`.
    pub panel: Option<Color>,
    /// opencode `backgroundElement`.
    pub element: Option<Color>,
    /// alacritty `[colors.selection].background` / opencode
    /// `selectionBackground`.
    pub selection: Option<Color>,
    /// opencode `border`.
    pub border: Option<Color>,
    /// opencode `borderSubtle`.
    pub border_subtle: Option<Color>,
}

/// Linear per-channel blend: `t = 0.0` yields `a`, `t = 1.0` yields `b`.
/// `t` is clamped to `0.0..=1.0`. Non-RGB inputs fall back to `a`, which
/// cannot happen for palettes built by this module (both parsers only ever
/// produce `Color::Rgb`).
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    let (ar, ag, ab) = match a {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return a,
    };
    let (br, bg_, bb) = match b {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return a,
    };
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 { (f64::from(x) * (1.0 - t) + f64::from(y) * t).round() as u8 };
    Color::Rgb(lerp(ar, br), lerp(ag, bg_), lerp(ab, bb))
}

/// The standard xterm 16-color table, used to resolve opencode JSON tokens
/// whose value is an ANSI color integer. Indices above 15 are rejected by
/// the parser (no built-in opencode theme uses them, and there is no
/// canonical RGB for the 6x6x6 cube across terminals).
pub(crate) const XTERM16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];
