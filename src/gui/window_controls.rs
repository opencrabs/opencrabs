//! The window controls the app draws itself when the compositor draws none
//! (#2813).
//!
//! gpui asks for server-side decorations by default (`Window::new` requests
//! `WindowDecorations::Server` when `WindowOptions::window_decorations` is
//! `None`). On Wayland that request travels through
//! `zxdg_decoration_manager_v1`, and a compositor that does not implement it
//! never answers. mutter 16 is one: the protocol name is absent from
//! `libmutter-16.so`, so gpui's decoration global stays `None`, no decoration
//! object is created, and no mode is ever set. The window then opens with no
//! chrome at all: the compositor draws no title bar, and the app drew none
//! either, so minimise, maximise and close did not exist anywhere.
//!
//! The window therefore asks for client-side decorations on Linux and paints
//! the three controls itself, in the top-right corner. They are an overlay and
//! not a title row on purpose: the owner removed the chat screen's title row in
//! #2596 and does not want it back.

use gpui::{
    App, ClickEvent, Decorations, Div, Stateful, Window, WindowControlArea, div, prelude::*, rgb,
};

use crate::palette;

/// One control the cluster can draw, in the order it paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Minimize,
    Maximize,
    Close,
}

/// Which controls a window has to draw itself.
///
/// A server-decorated window already carries the compositor's own buttons, so
/// the cluster draws nothing and the corner stays free. A client-decorated one
/// draws all three, minus the maximize button while an edge is tiled: a tiled
/// window has no restore size, so a maximize there would be a dead control.
pub fn controls(decorations: Decorations) -> &'static [Control] {
    match decorations {
        Decorations::Server => &[],
        Decorations::Client { tiling } => {
            if tiling.top || tiling.bottom || tiling.left || tiling.right {
                &[Control::Minimize, Control::Close]
            } else {
                &[Control::Minimize, Control::Maximize, Control::Close]
            }
        }
    }
}

/// The glyph a control paints and the hitbox the platform reads for it.
///
/// The area is what tells the window manager which corner of the surface is a
/// control rather than content: on X11 and Windows it answers the hit test, and
/// on Wayland it is what `window_controls` reports back to the app.
fn glyph(control: Control) -> (&'static str, WindowControlArea) {
    match control {
        Control::Minimize => ("\u{2013}", WindowControlArea::Min),
        Control::Maximize => ("\u{25a1}", WindowControlArea::Max),
        Control::Close => ("\u{2715}", WindowControlArea::Close),
    }
}

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

#[cfg(test)]
mod tests {
    use super::{Control, controls};
    use gpui::{Decorations, Tiling};

    fn client() -> Decorations {
        Decorations::Client {
            tiling: Tiling::default(),
        }
    }

    /// The bug this module exists for: a client-decorated window has to draw
    /// all three, or the window cannot be closed from inside it (#2813).
    #[test]
    fn a_client_decorated_window_draws_all_three() {
        assert_eq!(
            controls(client()),
            &[Control::Minimize, Control::Maximize, Control::Close]
        );
    }

    /// A server-decorated window already has the compositor's buttons, so a
    /// second set would be a duplicate.
    #[test]
    fn a_server_decorated_window_draws_none() {
        assert_eq!(controls(Decorations::Server), &[]);
    }

    /// A tiled window has no restore size, so maximize would do nothing there.
    #[test]
    fn a_tiled_window_drops_maximize() {
        for tiling in [
            Tiling {
                top: true,
                ..Tiling::default()
            },
            Tiling {
                bottom: true,
                ..Tiling::default()
            },
            Tiling {
                left: true,
                ..Tiling::default()
            },
            Tiling {
                right: true,
                ..Tiling::default()
            },
        ] {
            assert_eq!(
                controls(Decorations::Client { tiling }),
                &[Control::Minimize, Control::Close],
                "a tiled edge drops maximize: {tiling:?}"
            );
        }
    }

    /// Close is last in every configuration that draws it: the destructive
    /// control never sits where the pointer arrives first.
    #[test]
    fn close_is_always_last() {
        assert_eq!(controls(client()).last(), Some(&Control::Close));
        let tiled = Decorations::Client {
            tiling: Tiling {
                left: true,
                ..Tiling::default()
            },
        };
        assert_eq!(controls(tiled).last(), Some(&Control::Close));
    }

    /// Every control carries its own window-control area: the area is what the
    /// platform reads to tell a control from content, so two controls sharing
    /// one would make one of the buttons act as the other.
    #[test]
    fn every_control_carries_its_own_area() {
        use super::glyph;
        use gpui::WindowControlArea;
        let areas = [
            glyph(Control::Minimize).1,
            glyph(Control::Maximize).1,
            glyph(Control::Close).1,
        ];
        assert_eq!(areas[0], WindowControlArea::Min);
        assert_eq!(areas[1], WindowControlArea::Max);
        assert_eq!(areas[2], WindowControlArea::Close);
        for (index, area) in areas.iter().enumerate() {
            for (other_index, other) in areas.iter().enumerate() {
                if index != other_index {
                    assert_ne!(area, other, "two controls share one area");
                }
            }
        }
    }
}
