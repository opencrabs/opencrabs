//! The window controls the app draws itself when the compositor draws none
//! (#2813).
//!
//! This half is the drawing: it needs a `Window` and the view's own `cx`, so it
//! lives in the GUI binary. The decision of which controls a window has to draw
//! lives in `opencrabs::ui_core::window_controls`, where the test suite can
//! execute it.
//!
//! The controls are an overlay and not a title row on purpose: the owner
//! removed the chat screen's title row in #2596 and does not want it back.

use gpui::{App, ClickEvent, Decorations, Div, Stateful, Window, div, prelude::*, rgb};
/// The window's own `match` names the controls, so the type is re-exported
/// rather than reached through the library path at every use.
pub use opencrabs::ui_core::window_controls::Control;
use opencrabs::ui_core::window_controls::{controls, glyph};

use crate::palette;

/// One control button, styled but without its click handler: only the view's
/// own `cx` can mint one, so the caller attaches it.
pub fn button(control: Control) -> Stateful<Div> {
    let shown = palette::current();
    let (label, area) = glyph(control);
    // Close is the one control that reads as destructive, so it takes the red
    // wash on hover instead of the panel's neutral one.
    let hover = match control {
        Control::Close => shown.red,
        Control::Minimize | Control::Maximize => shown.panel_hi,
    };
    div()
        .id(match control {
            Control::Minimize => "window-control-min",
            Control::Maximize => "window-control-max",
            Control::Close => "window-control-close",
        })
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(gpui::px(22.0))
        .h(gpui::px(22.0))
        .rounded_sm()
        .text_sm()
        .text_color(rgb(shown.text_muted))
        .cursor_pointer()
        .window_control_area(area)
        .hover(move |d| d.bg(rgb(hover)).text_color(rgb(shown.text)))
        .child(label)
}

/// The cluster for a window, or `None` when the platform draws its own.
///
/// Absolute, in the top-right corner, above the header's own padding: the
/// header is a row of the window and the controls belong to the frame, so they
/// do not take a slot in any screen's layout.
pub fn cluster(
    decorations: Decorations,
    on_control: impl Fn(Control, &ClickEvent, &mut Window, &mut App) + 'static,
) -> Option<Div> {
    let list = controls(decorations);
    if list.is_empty() {
        return None;
    }
    let on_control = std::rc::Rc::new(on_control);
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .absolute()
        .top_0()
        .right_0()
        .p(gpui::px(6.0));
    for control in list.iter().copied() {
        let on_control = on_control.clone();
        row = row.child(
            button(control).on_click(move |ev, window, cx| on_control(control, ev, window, cx)),
        );
    }
    Some(row)
}
