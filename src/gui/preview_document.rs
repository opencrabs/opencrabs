//! Async state and rendering for the document body of the preview modal.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{AnyElement, ScrollHandle, div, px, rgb};
use opencrabs::ui_core::local_document::{self, DocumentKind};

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
    let blocks = markdown::blocks_flow(&loaded.text, kind == DocumentKind::Markdown);
    Ok(ParsedDocument {
        blocks,
        truncated: loaded.truncated,
    })
}

pub fn body(state: &DocumentState) -> AnyElement {
    let content = match &state.status {
        DocumentStatus::Loading => div()
            .text_sm()
            .text_color(rgb(0x94a3b8))
            .child("Loading document...")
            .into_any_element(),
        DocumentStatus::Error(message) => div()
            .text_sm()
            .text_color(rgb(0xfca5a5))
            .child(format!("Could not preview this document: {message}"))
            .into_any_element(),
        DocumentStatus::Ready(document) => {
            let mut rendered = div().flex().flex_col().gap_2().w_full();
            if document.truncated {
                rendered = rendered.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xfbbf24))
                        .child("Preview truncated. Use Open file to read the rest."),
                );
            }
            if document.blocks.is_empty() {
                rendered = rendered.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x94a3b8))
                        .child("This document is empty."),
                );
            } else {
                rendered = rendered.child(transcript::document_blocks(&document.blocks));
            }
            rendered.into_any_element()
        }
    };

    div()
        .id("preview-document-scroll")
        .flex_1()
        .min_h_0()
        .w_full()
        .max_h(px(460.0))
        .overflow_y_scroll()
        .track_scroll(&state.scroll)
        .rounded_md()
        .border_1()
        .border_color(rgb(0x263449))
        .bg(rgb(0x0b1220))
        .p_3()
        .child(content)
        .into_any_element()
}
