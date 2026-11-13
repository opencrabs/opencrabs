//! The window's live palette: the colours every surface reads.
//!
//! The window has exactly four themes (#2619), and their colours and
//! resolution live in `opencrabs::config::gui_theme`, so the tests reach them
//! without a window. This module only holds the live value and answers the
//! system appearance.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::WindowAppearance;
use opencrabs::config::{Config, GuiTheme, gui_theme};

use crate::logging;

pub use opencrabs::config::gui_theme::{ORGN, Palette, resolve};

/// The live palette. Read on every frame, written only when the appearance or
/// the config changes.
static CURRENT: RwLock<Palette> = RwLock::new(ORGN);

/// The system appearance last seen. Starts dark, the window's own default.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);

/// The `[gui]` mode, accent and base as the mirror holds them.
type SavedTheme = Option<(GuiTheme, Option<String>, Option<u32>)>;

/// The `[gui]` mode, accent and base last read or saved. The outer `None`
/// means the mirror has not been asked yet.
static SAVED: RwLock<SavedTheme> = RwLock::new(None);

/// The palette to draw with right now.
pub fn current() -> Palette {
    CURRENT.read().map(|p| *p).unwrap_or(ORGN)
}

fn set(palette: Palette) {
    if let Ok(mut live) = CURRENT.write() {
        *live = palette;
    }
}

/// The mode and accent `[gui]` saved, with `auto` as the default.
///
/// Read on every frame while the modal is open, and on every mouse move of a
/// drag, so it never touches the disk: the in-memory config mirror answers the
/// first read and the modal updates the cache the moment it saves a pick.
pub fn saved_mode() -> (GuiTheme, Option<String>) {
    let (mode, accent, _) = saved_triple();
    (mode, accent)
}

/// The base `[gui]` saved (#2746), as the `#RRGGBB` value the derivation needs.
/// Unset means the theme's own background, so nothing changes for a user who
/// never opened the base picker.
pub fn saved_base() -> Option<u32> {
    saved_triple().2
}

fn saved_triple() -> (GuiTheme, Option<String>, Option<u32>) {
    if let Some(saved) = SAVED.read().ok().and_then(|saved| saved.clone()) {
        return saved;
    }
    let gui = Config::current().gui.clone();
    let triple = (
        gui.theme,
        gui.custom_accent,
        gui.base.as_deref().and_then(gui_theme::parse_accent),
    );
    if let Ok(mut saved) = SAVED.write() {
        *saved = Some(triple.clone());
    }
    triple
}

/// Record what the modal just saved, so the next frame draws the choice without
/// waiting for the config watcher to refresh the mirror (#2619).
pub fn remember_mode(mode: GuiTheme, accent: Option<String>) {
    let (_, _, base) = saved_triple();
    if let Ok(mut saved) = SAVED.write() {
        *saved = Some((mode, accent, base));
    }
}

/// Record the base the picker just saved (#2746), the way `remember_mode`
/// records the mode and the accent: the window repaints from the mirror, not
/// from a config re-read.
pub fn remember_base(base: Option<u32>) {
    let (mode, accent, _) = saved_triple();
    if let Ok(mut saved) = SAVED.write() {
        *saved = Some((mode, accent, base));
    }
}

/// The palette the saved config picks for the last-seen system appearance.
pub fn saved() -> Palette {
    let (mode, accent) = saved_mode();
    resolve(mode, system_is_dark(), accent.as_deref(), saved_base())
}

/// Show a candidate palette without saving it. The theme modal paints the
/// colour wheel this way while the user drags, then saves on release.
pub fn preview(mode: GuiTheme, accent: Option<&str>) {
    set(resolve(mode, system_is_dark(), accent, saved_base()));
}

/// Adopt a palette the modal already resolved and previewed.
pub fn adopt(palette: Palette) {
    set(palette);
}

/// Make the palette match the system appearance. Called at startup and on
/// every change, so `auto` and `custom` follow the system without a restart.
/// `orgn` and `light` ignore the appearance; recording it still matters
/// because picking another mode later must use the current one.
pub fn follow_appearance(appearance: WindowAppearance) {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
    let palette = saved();
    // The window calls this once while building and the appearance observer
    // fires once on subscribe with the same value, so a launch used to log the
    // same line twice. Writing an identical palette is a no-op anyway (#2601).
    if palette == current() {
        return;
    }
    logging::info(&format!(
        "palette: system {} -> bg 0x{:06x}",
        if dark { "dark" } else { "light" },
        palette.bg
    ));
    set(palette);
}

/// Whether the system is currently dark, as last reported by the window.
pub fn system_is_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}
