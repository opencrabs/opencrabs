//! The controls a window draws itself when the platform draws none (#2813,
//! #2966, #3083).
//!
//! Two platforms arrive here from opposite directions. On Wayland with a
//! compositor that does not implement `zxdg_decoration_manager_v1` the window
//! opened with no title bar and no minimise, maximise or close button (#2813).
//! On Windows the question turned out to be about the title bar, not the
//! platform: gpui leaves the frame to the OS unless the window asks for a
//! transparent title bar, so a window created with `appears_transparent:
//! false` carries the native caption and drawing the cluster on top of it put
//! two sets of buttons in the same corner (#2966, #3083). The decision of
//! which controls to draw is executed here; the drawing that needs a window
//! lives in `src/gui/window_controls.rs`.

#![cfg(feature = "gui")]

use crate::ui_core::window_controls::{
    Control, HandlePress, TITLEBAR_APPEARS_TRANSPARENT, controls, effective_decorations, glyph,
    handle_press, platform_draws_caption,
};
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

/// A platform that draws no caption of its own answers `Server` for a title bar
/// that does not exist, as a Wayland compositor without a decoration manager
/// does. Read as the client decoration it really is, the window draws all three
/// controls and can be closed from inside it (#2813, #2966).
#[test]
fn a_captionless_platform_draws_all_three_from_a_server_report() {
    let decorations = effective_decorations(Decorations::Server, false);
    assert_eq!(decorations, client());
    assert_eq!(
        controls(decorations),
        &[Control::Minimize, Control::Maximize, Control::Close]
    );
}

/// Where the platform does draw a caption, a `Server` report is the truth. A
/// second set of buttons would duplicate the ones already in that corner.
#[test]
fn a_captioned_platform_keeps_a_server_report_and_draws_none() {
    let decorations = effective_decorations(Decorations::Server, true);
    assert_eq!(decorations, Decorations::Server);
    assert_eq!(controls(decorations), &[]);
}

/// A client report is already the answer, tiling included: the caption
/// question must not flatten a tiled window into an untiled one.
#[test]
fn a_client_report_passes_through_with_its_tiling() {
    let tiled = Decorations::Client {
        tiling: Tiling {
            left: true,
            ..Tiling::default()
        },
    };
    for platform_draws_caption in [true, false] {
        assert_eq!(
            effective_decorations(tiled, platform_draws_caption),
            tiled,
            "a client report is passed through: {platform_draws_caption}"
        );
    }
    assert_eq!(
        controls(effective_decorations(tiled, false)),
        &[Control::Minimize, Control::Close]
    );
}

/// #3083: the window the GUI creates asks for a title bar it does not paint,
/// so the platform draws the caption. On Windows that is the case gpui keeps
/// the frame for, and a cluster drawn on top of it is a second set of buttons
/// in the same corner, with no click path of its own.
#[test]
fn a_window_that_keeps_its_frame_has_a_caption_on_every_platform() {
    // A compile-time pin, not a runtime one: the constant is what every
    // Windows user sees, so flipping it has to fail the build and not just a
    // test run. It also keeps the assertion honest for clippy, which refuses
    // a runtime `assert!` on a value it can fold.
    const {
        assert!(
            !TITLEBAR_APPEARS_TRANSPARENT,
            "the GUI creates an opaque title bar; flipping this needs a Windows click path (#3083)"
        )
    };
    assert!(platform_draws_caption(TITLEBAR_APPEARS_TRANSPARENT));
    let decorations = effective_decorations(
        Decorations::Server,
        platform_draws_caption(TITLEBAR_APPEARS_TRANSPARENT),
    );
    assert_eq!(decorations, Decorations::Server);
    assert_eq!(controls(decorations), &[], "no second set of buttons");
}

/// #3083: a transparent title bar is the only case where Windows strips the
/// frame, and then the app has to draw the three controls itself.
#[test]
fn a_transparent_title_bar_leaves_windows_without_a_caption() {
    assert_eq!(
        platform_draws_caption(true),
        !cfg!(target_os = "windows"),
        "only Windows loses its caption when the title bar is transparent"
    );
    if cfg!(target_os = "windows") {
        let decorations = effective_decorations(Decorations::Server, platform_draws_caption(true));
        assert_eq!(
            controls(decorations),
            &[Control::Minimize, Control::Maximize, Control::Close]
        );
    }
}

/// #3083: the window's creation and the frame's decision have to read the same
/// answer, or the window gets two sets of buttons (as it did) or none.
#[test]
fn the_window_and_the_frame_read_the_same_titlebar_answer() {
    let app = include_str!("../gui/app.rs");
    assert!(
        app.contains("appears_transparent: TITLEBAR_APPEARS_TRANSPARENT"),
        "the window is created from the shared constant"
    );
    let frame = include_str!("../gui/dashboard_render.rs");
    assert!(
        frame.contains("platform_draws_caption(TITLEBAR_APPEARS_TRANSPARENT)"),
        "the frame decides from the shared constant"
    );
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

/// #3014: where the app runs the handle, one press moves the window. Before
/// the fix the chat screen had no handle at all, so nothing ever asked.
#[test]
fn a_press_on_the_drag_handle_moves_the_window() {
    assert_eq!(handle_press(1, true), HandlePress::Move);
}

/// #3014: a double press maximises, the way a title bar does, instead of
/// starting a second move.
#[test]
fn a_double_press_on_the_drag_handle_maximises() {
    assert_eq!(handle_press(2, true), HandlePress::Zoom);
    assert_eq!(handle_press(3, true), HandlePress::Zoom);
}

/// #3014: where the platform runs the handle itself the app stays out of it,
/// whatever the click count, so a native move is never doubled.
#[test]
fn a_platform_run_handle_gets_nothing_from_the_app() {
    for clicks in 0..4 {
        assert_eq!(handle_press(clicks, false), HandlePress::Platform);
    }
}

/// #3014: the handle exists on the chat screen, which has no title row. The
/// rail's brand row and the closed rail's column are both wrapped, and the
/// header's buttons stop their own press so they never start a move.
#[test]
fn the_rail_is_the_windows_drag_handle_open_or_closed() {
    let sessions = include_str!("../gui/sessions.rs");
    let (_, header) = sessions
        .split_once("fn brand_header(")
        .expect("brand header exists");
    let (header, _) = header.split_once("#[cfg(test)]").expect("header ends");
    assert!(header.contains("window_controls::drag_handle(row)"));
    let (_, closed) = sessions
        .split_once("pub fn collapsed_rail(")
        .expect("closed rail exists");
    let (closed, _) = closed
        .split_once("/// A compact brand header")
        .expect("closed rail ends");
    assert!(closed.contains("window_controls::drag_handle(column)"));
    let rail_view = include_str!("../gui/rail_view.rs");
    assert!(rail_view.contains("crate::window_controls::inside_drag_handle(div().id(id))"));
    let drawing = include_str!("../gui/window_controls.rs");
    assert!(drawing.contains(".window_control_area(WindowControlArea::Drag)"));
    assert!(drawing.contains("HandlePress::Move => window.start_window_move()"));
    assert!(drawing.contains("HandlePress::Zoom => window.zoom_window()"));
}
