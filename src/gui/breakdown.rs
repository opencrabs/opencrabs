//! The cache price breakdown panel and the key footer.
//!
//! Labels are render-side strings; which of them show comes from
//! [`input::advertises`], so a key the handler ignores is never advertised
//! and the footer cannot promise a dead key.

use gpui::{Div, div, prelude::*, px, rgb};

use opencrabs::ui_core::keys::UiKey;
use opencrabs::ui_core::usage::input::{self, DashboardState};
use opencrabs::ui_core::usage::view_model::CacheView;

fn text(s: impl Into<String>, size_px: f32, color: u32) -> Div {
    div()
        .text_size(px(size_px))
        .text_color(rgb(color))
        .child(div().child(s.into()))
}

/// The breakdown the Enter key opens on the Cache card: TOTAL row, per-model
/// rows, footnotes. Every cell is the view model's own formatted string.
pub fn panel(cache: Option<&CacheView>) -> Div {
    let mut body =
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(text("Cache price breakdown", 16.0, 0xf0f0f0));

    let Some(c) = cache else {
        body = body.child(text(
            "no cache data recorded for this period",
            12.0,
            0x808080,
        ));
        return body;
    };

    body = body.child(
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .child(text(
                format!("cached  {}", c.total.cached_str),
                12.0,
                0x909090,
            ))
            .child(text(format!("read  {}", c.total.read_str), 12.0, 0x909090))
            .child(text(
                format!("full rate  {}", c.total.full_rate_str),
                12.0,
                0x909090,
            ))
            .child(text(
                format!("saved  {}", c.total.saved_str),
                13.0,
                0x4f8f6f,
            )),
    );

    for m in &c.models {
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .bg(rgb(0x1a1a1a))
                .rounded_md()
                .p_3()
                .child(text(m.model.clone(), 13.0, 0xe8e8e8))
                .child(text(
                    format!(
                        "{} cached  ·  {} read  ·  {} full rate  ·  {} saved",
                        m.cached_str, m.read_str, m.full_rate_str, m.saved_str
                    ),
                    12.0,
                    0x909090,
                )),
        );
    }

    body = body
        .child(text(c.footnote1.clone(), 11.0, 0x606060))
        .child(text(c.footnote2.to_string(), 11.0, 0x606060));
    body
}

/// The key footer. A hint line is rendered only while [`input::advertises`]
/// says the key is live in this state, so the labels and the handler share
/// one rule set.
pub fn footer(dash: &DashboardState) -> Div {
    let hints: [(&str, UiKey); 8] = [
        ("esc close", UiKey::Esc),
        ("tab next card", UiKey::Tab),
        ("shift-tab prev card", UiKey::BackTab),
        ("enter breakdown", UiKey::Enter),
        ("t today", UiKey::Char('t')),
        ("w week", UiKey::Char('w')),
        ("m month", UiKey::Char('m')),
        ("a all time", UiKey::Char('a')),
    ];
    let mut bar = div().flex().flex_row().flex_wrap().gap_3();
    for (label, key) in hints {
        if input::advertises(dash, key) {
            bar = bar.child(text(label, 11.0, 0x707070));
        }
    }
    bar
}
