//! Shared window-level modal chrome. Content and actions belong to each surface.

use crate::palette;
use crate::theme::{Size, text};
use gpui::{AnyElement, ClickEvent, Div, Rgba, SharedString, Window, div, prelude::*, px, rgb, rgba};
use opencrabs::config::gui_theme;
use opencrabs::ui_core::card::concentric_radius;
use std::rc::Rc;

pub struct ModalView {
    pub id: &'static str,
    pub title: String,
    pub subtitle: String,
    pub actions: Option<AnyElement>,
    pub header_close: bool,
    pub body: AnyElement,
    pub footer: Option<AnyElement>,
    pub width: f32,
    pub max_height: f32,
}

/// The wash behind every modal (#2891).
///
/// The card fill sits six levels above the window background, so a light scrim
/// leaves the two surfaces indistinguishable. Dark themes therefore go nearly
/// black under the card; light themes keep a soft wash so the card still reads
/// as raised.
pub fn scrim() -> Rgba {
    let shown = palette::current();
    if gui_theme::relative_luminance(shown.bg) < 0.18 {
        rgba(0x050505d9)
    } else {
        rgba(0x1b19174d)
    }
}

/// The hairline around a modal card (#2891).
///
/// `panel_hi` sits four levels above the card fill, which is invisible in a
/// screenshot and reads as no edge at all. The ring comes from the ink instead:
/// a faint white line on dark themes, a faint black line on light ones.
pub fn ring() -> Rgba {
    let shown = palette::current();
    if gui_theme::relative_luminance(shown.bg) < 0.18 {
        rgba(0xffffff26)
    } else {
        rgba(0x0000001f)
    }
}

/// The card owns its clicks; only the surrounding scrim dismisses it.
pub fn render(
    view: ModalView,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let shown = palette::current();
    let dismiss = Rc::new(dismiss);
    let close_door = {
        let dismiss = dismiss.clone();
        move |ev: &ClickEvent, window: &mut Window, app: &mut gpui::App| dismiss(ev, window, app)
    };
    let scrim_door =
        move |ev: &ClickEvent, window: &mut Window, app: &mut gpui::App| dismiss(ev, window, app);
    let prefix = view.id;
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_w_0()
                .child(text(view.title, Size::Sm, shown.text))
                .child(
                    text(view.subtitle, Size::Xs, shown.text_muted)
                        .max_w_full()
                        .truncate(),
                ),
        );
    if view.header_close {
        header = header.child(
            div()
                .id(SharedString::from(format!("{prefix}-close")))
                .flex_none()
                .flex_row()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(rgb(shown.text_faint))
                .cursor_pointer()
                .hover(|d| d.text_color(rgb(shown.text)))
                .on_click(close_door)
                .child("Close")
                .child("esc"),
        );
    }
    if let Some(actions) = view.actions {
        header = header.child(actions);
    }
    let mut card = div()
        .id(SharedString::from(format!("{prefix}-card")))
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .w(px(view.width))
        .max_w(px(view.width))
        .max_h(px(view.max_height))
        .rounded(px(concentric_radius(18.0)))
        .border_1()
        .border_color(ring())
        .bg(rgb(shown.panel))
        .child(header)
        .child(view.body);
    if let Some(footer) = view.footer {
        card = card.child(footer);
    }
    div()
        .id(SharedString::from(format!("{prefix}-scrim")))
        .occlude()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .bg(scrim())
        .cursor_pointer()
        .on_click(scrim_door)
        .child(card)
}

pub fn footer(text_value: String) -> Div {
    let shown = palette::current();
    div()
        .text_xs()
        .text_color(rgb(shown.text_faint))
        .child(SharedString::from(text_value))
}
