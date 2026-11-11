//! The window's live typeface (#2786).
//!
//! The families the window paints, and the list of families the machine has.
//! The pure resolution lives in `opencrabs::config::gui_font`, so the
//! regression tests reach it without a window; this module only holds the live
//! values and the one-time enumeration.
//!
//! `TextSystem::all_font_names()` walks CoreText on macOS and keeps no cache of
//! its own, so the list is read once and held: calling it per frame would
//! re-enumerate every installed family on every paint.

use std::sync::{Arc, RwLock};

use opencrabs::config::{Config, gui_font};

use crate::logging;

/// The family the window draws text in. `None` means the mirror has not been
/// asked yet.
static UI: RwLock<Option<String>> = RwLock::new(None);

/// The family code blocks draw in. `None` means either the mirror has not been
/// asked yet, or the installed list arrived after the first answer, so the walk
/// has to run again.
static MONO: RwLock<Option<String>> = RwLock::new(None);

/// Every family the machine has, read once at the first paint.
static INSTALLED: RwLock<Option<Arc<Vec<String>>>> = RwLock::new(None);

/// The family the window draws text in right now.
pub fn current_ui() -> String {
    if let Some(family) = UI.read().ok().and_then(|live| live.clone()) {
        return family;
    }
    let saved = Config::current().gui.font.clone();
    let family = gui_font::resolve_ui(saved.as_deref());
    set_ui(family.clone());
    family
}

/// The family code blocks draw in right now.
pub fn current_mono() -> String {
    if let Some(family) = MONO.read().ok().and_then(|live| live.clone()) {
        return family;
    }
    let saved = Config::current().gui.mono_font.clone();
    let family = gui_font::resolve_mono(saved.as_deref(), &installed());
    set_mono(Some(family.clone()));
    family
}

/// Every family the machine has. Empty until the first paint fills it, which
/// is why the mono cache is dropped when the list arrives.
pub fn installed() -> Arc<Vec<String>> {
    if let Some(names) = INSTALLED.read().ok().and_then(|names| names.clone()) {
        return names;
    }
    Arc::new(Vec::new())
}

/// Whether the enumeration has run. The window asks once and never again.
pub fn loaded() -> bool {
    INSTALLED
        .read()
        .map(|names| names.is_some())
        .unwrap_or(false)
}

/// Keep the families the platform reported. The mono walk re-runs against the
/// real list, so a `mono_font` left on `auto` lands on a real family instead of
/// the generic it answered before the enumeration.
pub fn remember_installed(names: Vec<String>) {
    if loaded() {
        return;
    }
    logging::debug(&format!("fonts: {} families installed", names.len()));
    if let Ok(mut live) = INSTALLED.write() {
        *live = Some(Arc::new(names));
    }
    set_mono(None);
}

/// Save a family as `[gui] font` and paint with it. `system` is stored as the
/// word, so the key says what it means instead of naming a platform alias.
pub fn keep_ui(family: &str) -> Result<(), String> {
    let value = if gui_font::is_system(family) {
        gui_font::SYSTEM_KEY
    } else {
        family.trim()
    };
    Config::write_key_string("gui", "font", value).map_err(|e| e.to_string())?;
    set_ui(gui_font::resolve_ui(Some(family)));
    Ok(())
}

/// The rows the modal's font list draws.
pub fn picker_rows() -> Vec<String> {
    gui_font::picker_rows(&installed())
}

fn set_ui(family: String) {
    if let Ok(mut live) = UI.write() {
        *live = Some(family);
    }
}

fn set_mono(family: Option<String>) {
    if let Ok(mut live) = MONO.write() {
        *live = family;
    }
}
