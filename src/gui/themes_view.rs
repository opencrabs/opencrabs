//! The themes screen's rows (#2342). Each theme is drawn with its own
//! swatches, so a row shows what it will look like before it is clicked.
//!
//! The rows are built here from data. The click handlers live with the
//! dashboard in `main.rs`, which owns the preview and keep state.

use gpui::{Div, SharedString, Stateful, div, prelude::*, px, rgb};
use opencrabs::tui::render::theme_names;

use crate::palette::{self, Palette};
use crate::theme::{Size, text};

/// The theme names offered on the screen: built-in presets, then user presets.
pub fn names() -> Vec<&'static str> {
    theme_names()
}

/// Four small squares: background, panel, text, accent. Enough to tell a
/// light theme from a dark one at a glance.
fn swatches(shown: Palette) -> Div {
    let square = |color: u32| {
        div()
            .size(px(14.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(shown.panel_hi))
            .bg(rgb(color))
    };
    div()
        .flex()
        .flex_row()
        .gap_1()
        .child(square(shown.bg))
        .child(square(shown.panel))
        .child(square(shown.text))
        .child(square(shown.accent))
}

/// One selectable row. `selected` marks the theme the window is showing, so
/// the row reads as the current choice while the others are previews.
pub fn row(name: &str, selected: bool) -> Stateful<Div> {
    let shown = palette::resolve(name);
    let border = if selected {
        shown.accent
    } else {
        shown.panel_hi
    };
    div()
        .id(SharedString::from(format!("theme-row-{name}")))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(shown.panel))
        .hover(|d| d.bg(rgb(shown.panel_hi)))
        .cursor_pointer()
        .child(text(name.to_string(), Size::Sm, shown.text))
        .child(swatches(shown))
}
