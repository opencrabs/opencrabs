//! Shared look for the phase-0 window: palette, text sizes, the accent.
//!
//! Every length here is rem-based on purpose. The window's zoom (cmd +/= ,
//! cmd -/_ , cmd 0) works by changing the window's rem size, so a `px` length
//! would sit still while everything around it scaled. gpui's own text helpers
//! (`text_xs` … `text_xl`) are rem-based too, which is why the size enum maps
//! onto them instead of naming pixel sizes.

use gpui::{Div, SharedString, div, prelude::*, rgb};

/// The rem size zoom 1.0 means. gpui's default is 16px.
pub const BASE_REM: f32 = 16.0;
pub const ZOOM_MIN: f32 = 0.7;
pub const ZOOM_MAX: f32 = 2.0;
pub const ZOOM_STEP: f32 = 1.1;

/// Text sizes on gpui's rem scale: Xs 12px, Sm 14px, Xl 20px at
/// zoom 1.0.
#[derive(Clone, Copy)]
pub enum Size {
    Xs,
    Sm,
    Xl,
}

/// One line of text at a palette colour.
pub fn text(s: impl Into<SharedString>, size: Size, color: u32) -> Div {
    let d = div().text_color(rgb(color)).child(s.into());
    match size {
        Size::Xs => d.text_xs(),
        Size::Sm => d.text_sm(),
        Size::Xl => d.text_xl(),
    }
}
