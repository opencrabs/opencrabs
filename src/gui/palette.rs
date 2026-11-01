//! The window's live palette: the colours every surface reads. It is resolved
//! from `[gui]` in config.toml at startup and again whenever the system
//! appearance changes (#2341).
//!
//! `DARK` is the palette the window has always drawn, so an install that sets
//! nothing looks the same. Any other theme is a TUI theme converted role by
//! role. A role the TUI defines as a terminal colour rather than RGB keeps the
//! `DARK` value, since a terminal colour has no fixed RGB to read.

use std::sync::RwLock;

use gpui::WindowAppearance;
use opencrabs::config::Config;
use opencrabs::tui::render::{Theme, find_theme};
use ratatui::style::Color;

use crate::logging;

/// The name the built-in dark palette answers to in `[gui]`.
pub const DARK_NAME: &str = "gui-dark";
/// The light theme when `[gui] light_theme` is unset.
pub const DEFAULT_LIGHT: &str = "alucard";

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
    /// The TUI's primary accent (`palette::ORANGE`), so a thinking crab reads
    /// the same colour in both clients.
    pub accent: u32,
    /// The TUI's fold-header accent (ANSI cyan), so a collapsed turn reads the
    /// same in both clients.
    pub teal: u32,
    pub red: u32,
    pub warn: u32,
    /// The focus ring: a soft orange off the accent.
    pub focus: u32,
    /// The selected row's background, at panel strength.
    pub selection: u32,
}

/// The palette the window has always drawn. Dark, low-contrast panels.
pub const DARK: Palette = Palette {
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

/// The live palette. Read on every frame, written only when the appearance or
/// the config changes.
static CURRENT: RwLock<Palette> = RwLock::new(DARK);

/// The palette to draw with right now.
pub fn current() -> Palette {
    CURRENT.read().map(|p| *p).unwrap_or(DARK)
}

fn set(palette: Palette) {
    if let Ok(mut live) = CURRENT.write() {
        *live = palette;
    }
}

/// 0xRRGGBB for an RGB colour, or `fallback` for a terminal colour.
fn rgb_or(color: Color, fallback: u32) -> u32 {
    match color {
        Color::Rgb(r, g, b) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        _ => fallback,
    }
}

/// Convert a TUI theme to the window's roles. The mapping is deliberate: each
/// window role names the TUI role it stands for.
pub fn from_tui(theme: &Theme) -> Palette {
    let c = &theme.colors;
    let d = DARK;
    Palette {
        bg: c.background.map_or(d.bg, |bg| rgb_or(bg, d.bg)),
        panel: rgb_or(c.surface_panel, d.panel),
        panel_hi: rgb_or(c.surface_code, d.panel_hi),
        track: rgb_or(c.gray_dark, d.track),
        text: rgb_or(c.text_primary, d.text),
        text_strong: rgb_or(c.text_primary, d.text_strong),
        text_normal: rgb_or(c.text_secondary, d.text_normal),
        text_muted: rgb_or(c.text_muted, d.text_muted),
        text_title: rgb_or(c.text_secondary, d.text_title),
        text_faint: rgb_or(c.text_dim, d.text_faint),
        text_hint: rgb_or(c.gray_muted, d.text_hint),
        green: rgb_or(c.success, d.green),
        blue: rgb_or(c.blue_link, d.blue),
        amber: rgb_or(c.amber_muted, d.amber),
        accent: rgb_or(c.accent, d.accent),
        teal: rgb_or(c.accent_teal, d.teal),
        red: rgb_or(c.error, d.red),
        warn: rgb_or(c.warning, d.warn),
        focus: rgb_or(c.accent, d.focus),
        selection: rgb_or(c.selection_bg, d.selection),
    }
}

/// The palette for a theme name from `[gui]`. Unknown names log and fall back
/// to the dark palette, so a typo never blanks the window.
pub fn resolve(name: &str) -> Palette {
    if name.eq_ignore_ascii_case(DARK_NAME) {
        return DARK;
    }
    match find_theme(name) {
        Some(theme) => from_tui(theme),
        None => {
            logging::error(&format!(
                "[gui] theme '{name}' is not a built-in or user theme"
            ));
            DARK
        }
    }
}

/// The palette the config picks for the current system appearance.
pub fn for_appearance(dark: bool) -> Palette {
    let gui = Config::load().map(|c| c.gui).unwrap_or_default();
    let name = if dark {
        gui.dark_theme.unwrap_or_else(|| DARK_NAME.to_string())
    } else {
        gui.light_theme.unwrap_or_else(|| DEFAULT_LIGHT.to_string())
    };
    resolve(&name)
}

/// Make the palette match the system appearance. Called at startup and on
/// every change, so the window follows the system without a restart.
pub fn follow_appearance(appearance: WindowAppearance) {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    let palette = for_appearance(dark);
    logging::info(&format!(
        "palette: system {} -> bg 0x{:06x}",
        if dark { "dark" } else { "light" },
        palette.bg
    ));
    set(palette);
}
