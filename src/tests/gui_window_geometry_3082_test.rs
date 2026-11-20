//! The boot frame lands inside the work area (#3082).
//!
//! The app opened at a fixed 980x760 and let the platform centre it on the
//! whole screen. On a 1080p display at 150% that is 1140 physical pixels of
//! height against 1032 usable, so the bottom of the frame, input included,
//! opened under the taskbar. These tests pin the arithmetic that replaced it;
//! the platform call that supplies the work area is the GUI binary's job and
//! is pinned by source below.

#![cfg(feature = "gui")]

use crate::ui_core::window_geometry::{EDGE_MARGIN, MIN_SIZE, PREFERRED_SIZE, fit};
use gpui::{Bounds, Pixels, point, px, size};

fn area(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

/// A screen with room to spare gets the frame it asked for, centred.
#[test]
fn a_frame_that_fits_is_centred_in_the_work_area() {
    let bounds = fit(area(0.0, 0.0, 1920.0, 1040.0), PREFERRED_SIZE, MIN_SIZE);
    assert_eq!(bounds.size, PREFERRED_SIZE);
    assert_eq!(bounds.origin, point(px(470.0), px(140.0)));
}

/// The reported case: 980x760 logical against a 1080p work area at 150% scale,
/// which is 1280x688 logical. The height has to give way, and the frame stays
/// centred on what is left.
#[test]
fn a_frame_taller_than_the_work_area_shrinks_to_it() {
    let work = area(0.0, 0.0, 1280.0, 688.0);
    let bounds = fit(work, PREFERRED_SIZE, MIN_SIZE);
    assert_eq!(bounds.size.width, px(980.0));
    assert_eq!(bounds.size.height, px(688.0) - EDGE_MARGIN * 2.0);
    assert_eq!(bounds.origin, point(px(150.0), px(16.0)));
}

/// Whatever the work area, the frame stays inside it: no edge is crossed, so
/// nothing the frame holds can open where it cannot be reached.
#[test]
fn a_frame_never_leaves_the_work_area() {
    let areas = [
        area(0.0, 0.0, 1920.0, 1040.0),
        area(0.0, 0.0, 1280.0, 688.0),
        // A monitor to the left of the primary one has a negative origin, and
        // the frame has to follow it there rather than back to 0.
        area(-1920.0, 0.0, 1920.0, 1040.0),
        area(100.0, 50.0, 800.0, 600.0),
        area(0.0, 0.0, 400.0, 300.0),
    ];
    for work in areas {
        let bounds = fit(work, PREFERRED_SIZE, MIN_SIZE);
        assert!(
            bounds.origin.x >= work.origin.x && bounds.origin.y >= work.origin.y,
            "frame starts inside {work:?}: {bounds:?}"
        );
        assert!(
            bounds.origin.x + bounds.size.width <= work.origin.x + work.size.width,
            "frame ends inside horizontally: {bounds:?} in {work:?}"
        );
        assert!(
            bounds.origin.y + bounds.size.height <= work.origin.y + work.size.height,
            "frame ends inside vertically: {bounds:?} in {work:?}"
        );
    }
}

/// A screen too small to hold the layout floor wins over the floor: a cramped
/// frame is recoverable, one hanging off the edge is not.
#[test]
fn the_floor_yields_to_a_screen_too_small_to_hold_it() {
    let work = area(0.0, 0.0, 400.0, 300.0);
    let bounds = fit(work, PREFERRED_SIZE, MIN_SIZE);
    assert_eq!(bounds.size.width, px(400.0) - EDGE_MARGIN * 2.0);
    assert_eq!(bounds.size.height, px(300.0) - EDGE_MARGIN * 2.0);
    assert_eq!(bounds.origin, point(px(16.0), px(16.0)));
}

/// The window is opened from the fitted frame, not from a fixed size centred on
/// the whole screen: that literal was the bug (#3082).
#[test]
fn the_window_opens_from_the_fitted_frame() {
    let app = include_str!("../gui/app.rs");
    assert!(
        app.contains("window_geometry::boot_bounds(cx)"),
        "the window is opened from the fitted frame"
    );
    assert!(
        !app.contains("Bounds::centered(None, size(px(980.0)"),
        "the fixed frame that opened under the taskbar is gone"
    );
    assert!(
        app.contains("opencrabs::ui_core::window_geometry::MIN_SIZE"),
        "the window minimum comes from the same module"
    );
}

/// Windows answers `rcWork`, which already excludes the taskbar. gpui reads
/// only `rcMonitor`, so anything built on `display.bounds()` alone is a frame
/// that can open under the taskbar.
#[test]
fn the_work_area_on_windows_comes_from_the_work_rect() {
    let module = include_str!("../gui/window_geometry.rs");
    assert!(module.contains("GetMonitorInfoW"));
    assert!(module.contains("rc_work"), "the work rect, not the monitor rect");
    assert!(
        module.contains("MONITOR_DEFAULTTOPRIMARY"),
        "the primary monitor is asked for by flag, not by a window handle"
    );
}
