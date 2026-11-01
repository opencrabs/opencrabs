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

/// One selectable row in the sidebar list. `selected` marks the theme the
/// window is previewing, `current` the row the keyboard is standing on: they
/// are different facts, so the border and the fill carry them apart.
pub fn row(name: &str, selected: bool, current: bool) -> Stateful<Div> {
    let shown = palette::resolve(name);
    let border = if selected {
        shown.accent
    } else if current {
        shown.text_muted
    } else {
        shown.panel_hi
    };
    let fill = if selected || current {
        shown.panel_hi
    } else {
        shown.panel
    };
    div()
        .id(SharedString::from(format!("theme-row-{name}")))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(fill))
        .hover(|d| d.bg(rgb(shown.panel_hi)))
        .cursor_pointer()
        .child(text(name.to_string(), Size::Xs, shown.text))
        .child(swatches(shown))
}

/// A row in the preview history: the themes this visit has already shown, so
/// a good one can be found again without hunting the list. Deliberately
/// quieter than [`row`]: no border, no swatches, just the name.
pub fn history_row(name: &str) -> Stateful<Div> {
    let shown = palette::resolve(name);
    div()
        .id(SharedString::from(format!("theme-history-{name}")))
        .flex()
        .flex_row()
        .items_center()
        .px_2()
        .py_1()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(shown.panel_hi)))
        .child(text(name.to_string(), Size::Xs, shown.text_muted))
}
