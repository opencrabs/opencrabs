//! GPUI shells for the settings modal (#2878).
//!
//! Setting descriptors and value transitions live in `settings_view`; this
//! module only paints them. Click and key listeners stay with the dashboard.

use gpui::{Div, ScrollHandle, SharedString, Stateful, div, prelude::*, px, rgb};

use crate::palette;
use crate::theme::{Size, text};

/// The row shell: cursor highlight, label and blurb on the left, the control
/// (built by the caller, listeners included) on the right.
pub fn row_shell(
    id: SharedString,
    selected: bool,
    label: &str,
    blurb: &'static str,
    restart: bool,
    control: Div,
) -> Stateful<Div> {
    let shown = palette::current();
    let mut caption = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(text(label.to_string(), Size::Sm, shown.text));
    if restart {
        caption = caption.child(text("restart", Size::Xs, shown.amber));
    }
    let mut row = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .w_full()
        .px_2()
        .py_1()
        .rounded(px(6.0))
        .child({
            let mut left = div().flex().flex_col().flex_1().min_w_0().child(caption);
            // An empty blurb is how a row says "the label above is the whole
            // story", so it paints nothing instead of a blank line.
            if !blurb.is_empty() {
                left = left.child(text(blurb, Size::Xs, shown.text_faint));
            }
            left
        })
        .child(control);
    if selected {
        row = row.bg(rgb(shown.panel_hi));
    }
    row
}

/// One category in the settings rail (#2890). The dashboard attaches the
/// click listener; this only paints the label, the hint and the active state.
pub fn rail_item(
    id: SharedString,
    label: &'static str,
    hint: &'static str,
    active: bool,
) -> Stateful<Div> {
    let shown = palette::current();
    let mut item = div()
        .id(id)
        .flex()
        .flex_col()
        .gap_0p5()
        .w_full()
        .px_2()
        .py_1p5()
        .rounded(px(8.0))
        .cursor_pointer()
        .child(text(
            label,
            Size::Sm,
            if active { shown.text_strong } else { shown.text_muted },
        ))
        .child(text(hint, Size::Xs, shown.text_faint));
    if active {
        item = item.bg(rgb(shown.panel_hi));
    }
    item
}

/// The modal shell: the same scrim-and-card idiom as the theme modal (#2619),
/// with a scrollable body so the whole table fits a short window.
pub fn modal(
    rail: Vec<gpui::AnyElement>,
    body: Vec<gpui::AnyElement>,
    footer: &str,
    scroll: &ScrollHandle,
    dismiss: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    use std::rc::Rc;

    let shown = palette::current();
    // One dismiss handler, two doors (header close and scrim), shared behind
    // an Rc exactly as the theme modal does it.
    let dismiss = Rc::new(dismiss);
    let close_door = {
        let dismiss = dismiss.clone();
        move |ev: &gpui::ClickEvent, window: &mut gpui::Window, app: &mut gpui::App| {
            dismiss(ev, window, app)
        }
    };
    let scrim_door =
        move |ev: &gpui::ClickEvent, window: &mut gpui::Window, app: &mut gpui::App| {
            dismiss(ev, window, app)
        };
    let card = div()
        .id("settings-modal-card")
        // A click on the card stops here: only the scrim dismisses.
        .on_click(|_ev, _window, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .w(px(760.0))
        .rounded(px(18.0))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(rgb(shown.panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .child(text("Settings".to_string(), Size::Sm, shown.text))
                .child(
                    div()
                        .id("settings-modal-close")
                        .flex()
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
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .w_full()
                .h(px(420.0))
                .child(
                    div()
                        .id("settings-modal-rail")
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(156.0))
                        .flex_shrink_0()
                        .children(rail),
                )
                .child(
                    div()
                        .id("settings-modal-body")
                        .flex()
                        .flex_col()
                        .gap_1()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        .track_scroll(scroll)
                        .children(body),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_muted))
                .child(SharedString::from(footer.to_string())),
        );
    div()
        .id("settings-modal-scrim")
        .occlude()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right_0()
        .flex()
        .items_center()
        .justify_center()
        .on_click(scrim_door)
        .child(card)
}
