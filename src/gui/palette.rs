//! The window's live palette: the colours every surface reads.
//!
//! The window has exactly four themes (#2619), and their colours and
//! resolution live in `opencrabs::config::gui_theme`, so the tests reach them
//! without a window. This module only holds the live value and answers the
//! system appearance.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::WindowAppearance;
use opencrabs::config::{Config, GuiTheme};

use crate::logging;

pub use opencrabs::config::gui_theme::{ORGN, Palette, resolve};

/// The live palette. Read on every frame, written only when the appearance or
/// the config changes.
static CURRENT: RwLock<Palette> = RwLock::new(ORGN);

/// The system appearance last seen. Starts dark, the window's own default.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);

/// The `[gui]` mode and accent last read or saved. `None` means the mirror has
/// not been asked yet.
static SAVED: RwLock<Option<(GuiTheme, Option<String>)>> = RwLock::new(None);

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
    if let Some(saved) = SAVED.read().ok().and_then(|saved| saved.clone()) {
        return saved;
    }
    let gui = Config::current().gui.clone();
    let pair = (gui.theme, gui.custom_accent);
    remember_mode(pair.0, pair.1.clone());
    pair
}

/// Record what the modal just saved, so the next frame draws the choice without
/// waiting for the config watcher to refresh the mirror (#2619).
pub fn remember_mode(mode: GuiTheme, accent: Option<String>) {
    if let Ok(mut saved) = SAVED.write() {
        *saved = Some((mode, accent));
    }
}

/// The palette the saved config picks for the last-seen system appearance.
pub fn saved() -> Palette {
    let (mode, accent) = saved_mode();
    resolve(mode, system_is_dark(), accent.as_deref())
}

/// Show a candidate palette without saving it. The theme modal paints the
/// colour wheel this way while the user drags, then saves on release.
pub fn preview(mode: GuiTheme, accent: Option<&str>) {
    set(resolve(mode, system_is_dark(), accent));
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
