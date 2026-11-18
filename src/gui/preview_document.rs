//! Async state and rendering for the document body of the preview modal.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, MouseButton, MouseDownEvent, ScrollHandle, Window,
    canvas, div, fill, point, px, rgb, rgba, size,
};
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::local_document::{self, DocumentKind};
use opencrabs::ui_core::scroll::vertical_scroll_thumb;

use crate::palette;

use super::markdown::{self, MdBlock};
use super::transcript;

pub struct ParsedDocument {
    pub blocks: Vec<MdBlock>,
    pub truncated: bool,
}

pub enum DocumentStatus {
    Loading,
    Ready(ParsedDocument),
    Error(String),
}

pub struct DocumentState {
    pub path: PathBuf,
    pub generation: u64,
    pub status: DocumentStatus,
    pub scroll: ScrollHandle,
}

impl DocumentState {
    pub fn loading(path: PathBuf, generation: u64) -> Self {
        Self {
            path,
            generation,
            status: DocumentStatus::Loading,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn matches(&self, path: &Path, generation: u64) -> bool {
        self.path == path && self.generation == generation
    }

    pub fn complete(&mut self, result: Result<ParsedDocument, String>) {
        self.status = match result {
            Ok(document) => DocumentStatus::Ready(document),
            Err(message) => DocumentStatus::Error(message),
        };
    }
}

/// Called by the background executor. Both UTF-8 reading and Markdown parsing
/// stay off the UI thread; only the completed render model crosses back.
pub fn parse(path: &Path, kind: DocumentKind) -> Result<ParsedDocument, String> {
    let loaded = local_document::load(path).map_err(|error| error.to_string())?;
    // Markdown documents reflow like a reply; plain text keeps the breaks the
    // author typed, because those breaks are the only structure it has.
    let flow = match kind {
        DocumentKind::Markdown => markdown::MdFlow::Prose,
        DocumentKind::PlainText => markdown::MdFlow::Verbatim,
    };
    let blocks = markdown::blocks_flow(&loaded.text, flow);
    Ok(ParsedDocument {
        blocks,
        truncated: loaded.truncated,
    })
}

pub fn body(
    state: &DocumentState,
    height: f32,
    middle_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let shown = palette::current();
    let content = match &state.status {
        DocumentStatus::Loading => div()
            .text_sm()
            .text_color(rgb(shown.text_muted))
            .child("Loading document...")
            .into_any_element(),
        DocumentStatus::Error(message) => div()
            .text_sm()
            .text_color(rgb(shown.red))
            .child(format!("Could not preview this document: {message}"))
            .into_any_element(),
        DocumentStatus::Ready(document) => {
            let mut rendered = div().flex().flex_col().gap_3().w_full().min_w_0();
            if document.truncated {
                rendered = rendered.child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.warn))
                        .child("Preview truncated. Use Open file to read the rest."),
                );
            }
            if document.blocks.is_empty() {
                rendered = rendered.child(
                    div()
                        .text_sm()
                        .text_color(rgb(shown.text_muted))
                        .child("This document is empty."),
                );
            } else {
                rendered = rendered.child(transcript::document_blocks(&document.blocks));
            }
            rendered.into_any_element()
        }
    };

    let reader = div()
        .id("preview-document-scroll")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(px(8.0))
        .track_scroll(&state.scroll)
        .rounded(px(concentric_radius(8.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .p_5()
        .on_mouse_down(MouseButton::Middle, move |ev, window, cx| {
            middle_click(ev, window, cx);
            cx.stop_propagation();
        })
        .child(content);

    div()
        .relative()
        .w_full()
        .h(px(height))
        .min_h_0()
        .child(reader)
        .child(scrollbar(state.scroll.clone()))
        .into_any_element()
}

/// Paint after the reader so the thumb uses this frame's measured content.
/// The track is deliberately quiet; the thumb appears only when there is
/// somewhere to scroll and follows the active theme like the rest of the card.
fn scrollbar(scroll: ScrollHandle) -> impl IntoElement {
    let shown = palette::current();
    div()
        .id("preview-document-scrollbar")
        .absolute()
        .top_3()
        .bottom_3()
        .right_2()
        .w(px(4.0))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.paint_quad(fill(bounds, rgba((shown.track << 8) | 0x66)));
                    let Some(thumb) = vertical_scroll_thumb(
                        f32::from(bounds.size.height),
                        f32::from(scroll.max_offset().height),
                        f32::from(scroll.offset().y),
                    ) else {
                        return;
                    };
                    let thumb_bounds = Bounds::new(
                        point(bounds.origin.x, bounds.origin.y + px(thumb.top)),
                        size(bounds.size.width, px(thumb.height)),
                    );
                    window.paint_quad(gpui::quad(
                        thumb_bounds,
                        px(2.0),
                        rgb(shown.text_muted),
                        px(0.0),
                        rgba(0x00000000),
                        BorderStyle::default(),
                    ));
                },
            )
            .size_full(),
        )
}
