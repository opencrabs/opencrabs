//! Where the desktop app's frame lands when it opens (#3082).
//!
//! The app asked for a fixed 980x760 and let the platform centre it. On a
//! 1920x1080 display at 150% that request is 1470x1140 physical, which is
//! taller than the screen: the bottom of the frame, input included, sat under
//! the taskbar with no way to drag it back. The fix is arithmetic, so the
//! arithmetic lives here and the window handle stays in `src/gui/`.
//!
//! Two rules decide the frame: never larger than the work area, and never
//! smaller than the floor the layout needs unless the work area itself is
//! smaller than that floor. A screen that cannot hold the floor wins over the
//! floor, because a frame that hangs off the edge is worse than a cramped one.

use gpui::{Bounds, Pixels, Size, point, px, size};

/// The frame the app asks for when nothing has been remembered.
pub const PREFERRED_SIZE: Size<Pixels> = size(px(980.0), px(760.0));

/// Below this the single-column grid stops fitting, so it is the floor.
pub const MIN_SIZE: Size<Pixels> = size(px(480.0), px(320.0));

/// Room kept between the frame and the edge of the work area.
pub const EDGE_MARGIN: Pixels = px(16.0);

/// The frame to open with, given the area the platform will let it occupy.
///
/// `work_area` is the screen minus the panels the platform keeps for itself
/// (the Windows taskbar, the macOS menu bar and dock). The result is centred in
/// that area, so it never straddles an edge, and it is clamped on both sides:
/// down to the room available, up to [`MIN_SIZE`].
pub fn fit(
    work_area: Bounds<Pixels>,
    preferred: Size<Pixels>,
    min: Size<Pixels>,
) -> Bounds<Pixels> {
    let width = extent(work_area.size.width, preferred.width, min.width);
    let height = extent(work_area.size.height, preferred.height, min.height);
    let origin = point(
        work_area.origin.x + (work_area.size.width - width) * 0.5,
        work_area.origin.y + (work_area.size.height - height) * 0.5,
    );
    Bounds::new(origin, size(width, height))
}

/// One axis of [`fit`]: the preferred extent, clamped to the room and to the
/// floor. The floor yields when the room cannot hold it.
fn extent(available: Pixels, preferred: Pixels, min: Pixels) -> Pixels {
    let room = (available - EDGE_MARGIN * 2.0).max(px(0.0));
    preferred.min(room).max(min.min(room))
}

/// A frame the user placed, put back where it was without leaving the work
/// area (#3082).
///
/// Unlike [`fit`], the origin is kept: the point of remembering a frame is that
/// the window opens where the user left it, not centred again. The size is
/// clamped the same way, and the origin is pushed back inside when the frame
/// would straddle an edge, which is what a screen that shrank since the last
/// run produces.
pub fn place(
    work_area: Bounds<Pixels>,
    requested: Bounds<Pixels>,
    min: Size<Pixels>,
) -> Bounds<Pixels> {
    let width = extent(work_area.size.width, requested.size.width, min.width);
    let height = extent(work_area.size.height, requested.size.height, min.height);
    let rightmost = work_area.origin.x + (work_area.size.width - width);
    let lowest = work_area.origin.y + (work_area.size.height - height);
    let origin = point(
        requested
            .origin
            .x
            .max(work_area.origin.x)
            .min(rightmost.max(work_area.origin.x)),
        requested
            .origin
            .y
            .max(work_area.origin.y)
            .min(lowest.max(work_area.origin.y)),
    );
    Bounds::new(origin, size(width, height))
}

/// Whether a saved frame belongs on this work area at all.
///
/// A frame remembered on a monitor that is no longer attached cannot be
/// checked against a work area it is not inside, and clamping it onto this one
/// would drag a second-monitor window to the primary every launch. Its top left
/// corner is the test: inside the area means this is the screen it was on.
pub fn is_on(work_area: Bounds<Pixels>, frame: Bounds<Pixels>) -> bool {
    frame.origin.x >= work_area.origin.x
        && frame.origin.x < work_area.origin.x + work_area.size.width
        && frame.origin.y >= work_area.origin.y
        && frame.origin.y < work_area.origin.y + work_area.size.height
}
