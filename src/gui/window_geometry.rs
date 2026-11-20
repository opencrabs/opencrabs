//! The area a window may occupy on the screen it opens on (#3082).
//!
//! The decision of which frame to open with is arithmetic and lives in
//! `ui_core::window_geometry`; this module supplies the one input that needs a
//! platform call, the work area: the screen minus whatever the platform keeps
//! for itself.
//!
//! Windows answers `rcWork`, which already excludes the taskbar and any docked
//! app bar. gpui reads only `rcMonitor` for its display bounds
//! (`gpui-0.2.2/src/platform/windows/display.rs:38`), so the taskbar is missing
//! from anything built on `display.bounds()` alone, which is why a 760 px frame
//! could open 132 px taller than the usable screen. Elsewhere `bounds()` is the
//! whole screen; the panels a desktop keeps there (menu bar, dock) are smaller
//! than the margin this app leaves, so the full screen is a usable answer.

use gpui::{App, Bounds, Pixels, PlatformDisplay};
#[cfg(target_os = "windows")]
use gpui::{point, px, size};

use opencrabs::ui_core::window_geometry::{MIN_SIZE, PREFERRED_SIZE, fit};

/// The frame the app opens with: the preferred size, fitted to the work area
/// of the screen it opens on.
///
/// Falls back to the platform's own centring when no display answers, which is
/// the headless case where there is no work area to respect.
pub fn boot_bounds(cx: &App) -> Bounds<Pixels> {
    match work_area(cx) {
        Some(area) => fit(area, PREFERRED_SIZE, MIN_SIZE),
        None => Bounds::centered(None, PREFERRED_SIZE, cx),
    }
}

/// The work area of the primary display, in logical pixels. Outside Windows
/// `bounds()` is the whole screen: the menu bar and dock a desktop keeps there
/// are smaller than the margin this app leaves, so the screen is a usable
/// answer.
#[cfg(not(target_os = "windows"))]
fn work_area(cx: &App) -> Option<Bounds<Pixels>> {
    cx.primary_display().map(|display| display.bounds())
}

/// The work area of the primary monitor, in logical pixels.
///
/// The platform answers in physical pixels, and the scale factor comes from
/// the display gpui already read: the ratio between the logical size it reports
/// and the physical size of the same monitor. Reading the ratio instead of
/// asking for a DPI keeps the two sources on one monitor by construction, which
/// a system DPI call cannot promise on a mixed-DPI desktop.
#[cfg(target_os = "windows")]
fn work_area(cx: &App) -> Option<Bounds<Pixels>> {
    let display = cx.primary_display()?;
    let monitor = display.bounds();
    let (screen, work) = win32::primary_work_area()?;
    let physical_width = (screen.right - screen.left) as f32;
    let physical_height = (screen.bottom - screen.top) as f32;
    if physical_width <= 0.0 || physical_height <= 0.0 {
        return None;
    }
    let scale = monitor.size.width / px(physical_width);
    if !(scale > 0.0) {
        return None;
    }
    let offset_x = (work.left - screen.left) as f32 / scale;
    let offset_y = (work.top - screen.top) as f32 / scale;
    let width = (work.right - work.left) as f32 / scale;
    let height = (work.bottom - work.top) as f32 / scale;
    Some(Bounds::new(
        point(
            monitor.origin.x + px(offset_x),
            monitor.origin.y + px(offset_y),
        ),
        size(px(width), px(height)),
    ))
}

/// The two rectangles `GetMonitorInfoW` reports, in physical pixels.
///
/// Hand-declared the way `src/config/winlock.rs` declares its Win32 calls: this
/// crate has no `windows` dependency, and two structs plus two functions do not
/// justify one.
#[cfg(target_os = "windows")]
mod win32 {
    /// A Win32 `RECT`: edges in physical pixels, right and bottom exclusive.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(super) struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    /// A Win32 `MONITORINFO`. `cb_size` must be set before the call or the
    /// function refuses the buffer.
    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        flags: u32,
        device: [u16; 32],
    }

    #[repr(C)]
    struct NativePoint {
        x: i32,
        y: i32,
    }

    type Monitor = isize;

    /// Ask for the primary monitor regardless of which one holds the point.
    const MONITOR_DEFAULTTOPRIMARY: u32 = 1;

    unsafe extern "system" {
        fn MonitorFromPoint(at: NativePoint, flags: u32) -> Monitor;
        fn GetMonitorInfoW(monitor: Monitor, info: *mut MonitorInfo) -> i32;
    }

    /// The primary monitor's full rect and its work rect.
    pub(super) fn primary_work_area() -> Option<(Rect, Rect)> {
        let mut info = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            flags: 0,
            device: [0; 32],
        };
        // SAFETY: both calls take a valid out-parameter owned by this frame and
        // answer a status code; nothing is retained past the call.
        unsafe {
            let monitor = MonitorFromPoint(NativePoint { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            if monitor == 0 {
                return None;
            }
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
        }
        Some((info.rc_monitor, info.rc_work))
    }
}
