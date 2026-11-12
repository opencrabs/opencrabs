//! The controls a Linux window draws itself when the compositor draws none
//! (#2813).
//!
//! The bug: on Wayland with a compositor that does not implement
//! `zxdg_decoration_manager_v1`, the window opened with no title bar and no
//! minimise, maximise or close button. The decision of which controls to draw
//! is executed here; the drawing that needs a window lives in
//! `src/gui/window_controls.rs`.

#![cfg(feature = "gui")]

use crate::ui_core::window_controls::{Control, controls, glyph};
use gpui::{Decorations, Tiling, WindowControlArea};

fn client() -> Decorations {
    Decorations::Client {
        tiling: Tiling::default(),
    }
}

/// The bug this exists for: a client-decorated window has to draw all three,
/// or the window cannot be closed from inside it (#2813).
#[test]
fn a_client_decorated_window_draws_all_three() {
    assert_eq!(
        controls(client()),
        &[Control::Minimize, Control::Maximize, Control::Close]
    );
}

/// A server-decorated window already has the compositor's buttons, so a second
/// set would be a duplicate in the same corner.
#[test]
fn a_server_decorated_window_draws_none() {
    assert_eq!(controls(Decorations::Server), &[]);
}

/// A tiled window has no restore size, so a maximize there would do nothing.
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

/// Close is last in every configuration that draws it: the destructive control
/// never sits where the pointer arrives first.
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
/// platform reads to tell a control from content, so two controls sharing one
/// would make one button act as the other.
#[test]
fn every_control_carries_its_own_area() {
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
