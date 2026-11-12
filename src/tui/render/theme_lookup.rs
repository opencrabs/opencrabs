//! The theme names the desktop window offers (#1461 follow-up).
//!
//! Built-in presets first, then the user presets read from
//! `~/.opencrabs/themes`. Both answers are read on each open of the themes
//! screen, so a new file shows up without a restart.
//!
//! Kept out of `render/mod.rs` so that file stays a list of declarations,
//! which is what `CONTRIBUTING.md` asks of a `mod.rs`.

use super::Theme;
use super::{presets, user_themes};

/// Look up a theme by name for the desktop window: built-in presets first,
/// then user themes in `~/.opencrabs/themes`. Case-insensitive, like `/theme`.
pub fn find_theme(name: &str) -> Option<&'static Theme> {
    presets::by_name(name).or_else(|| user_themes::find(name))
}

/// Every theme name the desktop window can pick: the built-in presets, then
/// the user presets. Read on each open of the themes screen, so a new file in
/// `~/.opencrabs/themes` shows up without a restart.
pub fn theme_names() -> Vec<&'static str> {
    presets::built_ins()
        .iter()
        .map(|t| t.name)
        .chain(user_themes::all().into_iter().map(|t| t.name))
        .collect()
}
