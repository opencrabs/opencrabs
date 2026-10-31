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

// Palette. Dark, low-contrast panels; the accents are the same three the TUI
// dashboard uses for bars and the cache traffic light.
pub const BG: u32 = 0x101010;
pub const PANEL: u32 = 0x161616;
pub const PANEL_HI: u32 = 0x1a1a1a;
pub const TRACK: u32 = 0x242424;
pub const TEXT: u32 = 0xf0f0f0;
pub const TEXT_STRONG: u32 = 0xe8e8e8;
pub const TEXT_NORMAL: u32 = 0xd0d0d0;
pub const TEXT_MUTED: u32 = 0x909090;
pub const TEXT_TITLE: u32 = 0x8a8a8a;
pub const TEXT_FAINT: u32 = 0x606060;
pub const TEXT_HINT: u32 = 0x707070;
pub const GREEN: u32 = 0x4f8f6f;
pub const BLUE: u32 = 0x6f8fbf;
pub const AMBER: u32 = 0xbf8f5f;
/// The TUI's primary accent (`palette::ORANGE`), used for the live status
/// line's wording so a thinking crab reads the same colour in both clients.
pub const ACCENT: u32 = 0xd76414;
/// The TUI`s fold-header accent (ANSI cyan), so a collapsed turn reads the
/// same in both clients.
pub const TEAL: u32 = 0x5fbfbf;
pub const RED: u32 = 0xe07050;
pub const WARN: u32 = 0xb0885a;
/// The focus ring colour: a soft orange off the accent, in palette. The
/// green it used to be came from outside the TUI's own set.
pub const FOCUS: u32 = 0x8a4f24;
/// The selected row's background: the accent at panel strength, so the rail's
/// focus reads as a warm tint instead of a highlight.
pub const SELECTION: u32 = 0x2f1e12;

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
