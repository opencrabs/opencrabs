//! Which window controls a window has to draw itself, and what each one maps
//! to (#2813).
//!
//! gpui asks for server-side decorations when `WindowOptions::window_decorations`
//! is `None` (`gpui-0.2.2/src/window.rs:997`). On Wayland that request travels
//! through `zxdg_decoration_manager_v1`, and a compositor that does not
//! implement it never answers: mutter 16 is one, the protocol name is absent
//! from `libmutter-16.so`, gpui's decoration global stays `None`, and the window
//! opens with no chrome at all. No title bar, no minimise, maximise or close
//! button, and no way out but a keyboard shortcut or killing the process.
//!
//! The window therefore asks for client-side decorations on Linux and paints
//! the three controls itself. The drawing needs a window, so it lives in the
//! GUI binary (`src/gui/window_controls.rs`); the decision lives here, where
//! the suite can execute it.
//!
//! Windows arrives at the same place from the other side, and the answer
//! changed once a live window was measured (#2966, #3083). gpui creates every
//! window with `WS_SYSMENU`, adds `WS_THICKFRAME | WS_MAXIMIZEBOX` when the
//! window is resizable and `WS_MINIMIZEBOX` when it is minimizable
//! (`gpui-0.2.2/src/platform/windows/window.rs:392-405`), and leaves the
//! non-client area to the OS whenever the title bar is not transparent:
//! `WM_NCCALCSIZE` returns `None` (`events.rs:705`) and so does the hit test
//! (`events.rs:884`), so `DefWindowProc` keeps the frame. A window created
//! that way carries the native caption with its own min, max and close, which
//! is what a live Windows 11 window reports: `WS_CAPTION` set, a 45 px caption
//! band, `HTCLOSE` answering over the system buttons. Drawing the cluster on
//! top of that put two sets of buttons in the same corner, and the app's set
//! was the dead one, because the platform only runs a control area whose hit
//! test it answers itself.
//!
//! A transparent title bar is the other case: `hide_title_bar` turns on, gpui
//! strips the frame through `WM_NCCALCSIZE`, and then nothing draws a caption
//! unless the app does. That is the case [`TITLEBAR_APPEARS_TRANSPARENT`]
//! describes, and the only one where Windows needs this module's cluster.

use gpui::{Decorations, Tiling, WindowControlArea};

/// One control a cluster can draw, in the order it paints.
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

/// Whether the GUI asks for a title bar it paints itself.
///
/// `src/gui/app.rs` passes this to `TitlebarOptions::appears_transparent`, and
/// `src/gui/dashboard_render.rs` reads it back to decide who draws the
/// controls. One constant, because a window created with one answer and drawn
/// with the other ends up with either two sets of buttons or none (#3083).
///
/// Flipping it to `true` is not free on Windows: gpui then strips the frame,
/// so the cluster has to carry its own click handler, and `zoom()` there only
/// ever maximises (`gpui-0.2.2/src/platform/windows/window.rs:782`), so
/// restoring a maximised window needs a check of its own.
pub const TITLEBAR_APPEARS_TRANSPARENT: bool = false;

/// Whether the platform draws a caption of its own for a window created with
/// this title bar.
///
/// macOS and the X11 and Wayland servers always do. Windows does whenever the
/// window keeps its frame, which is exactly the window that did not ask for a
/// transparent title bar: see the module doc (#3083).
pub fn platform_draws_caption(titlebar_appears_transparent: bool) -> bool {
    !cfg!(target_os = "windows") || !titlebar_appears_transparent
}

/// The decorations a window actually has, once the platform's own caption is
/// accounted for.
///
/// `reported` is what the platform answers. Where the platform draws no caption
/// at all, a `Server` report describes a title bar that does not exist, so it
/// becomes a client decoration and the app draws the controls itself. Every
/// other report is passed through untouched: a client report is already the
/// answer, tiling included.
pub fn effective_decorations(reported: Decorations, platform_draws_caption: bool) -> Decorations {
    match reported {
        Decorations::Server if !platform_draws_caption => Decorations::Client {
            tiling: Tiling::default(),
        },
        other => other,
    }
}

/// What a press on the window's drag handle does (#3014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlePress {
    /// Start an interactive move of the window.
    Move,
    /// Toggle maximise, the way a double press on a title bar does.
    Zoom,
    /// Nothing from the app: the platform runs the handle itself.
    Platform,
}

/// Whether the app has to run the drag handle's presses itself.
///
/// Linux is the one platform where it does: the window is client-decorated
/// there (#2813), and gpui's hit-test callback for control areas is a no-op on
/// Wayland and X11, so nothing moves the window unless the app asks. Windows
/// answers the `Drag` area from its non-client hit test and runs both the move
/// and the double-press maximise natively. macOS keeps its own title bar.
pub const APP_RUNS_DRAG_HANDLE: bool = cfg!(any(target_os = "linux", target_os = "freebsd"));

/// What one press on the drag handle does, from the press's click count.
///
/// A second press inside the double-click window maximises instead of starting
/// another move: that is what every title bar does, and a window whose only
/// handle is this one has no other way to offer it.
pub fn handle_press(click_count: usize, app_runs_handle: bool) -> HandlePress {
    if !app_runs_handle {
        HandlePress::Platform
    } else if click_count >= 2 {
        HandlePress::Zoom
    } else {
        HandlePress::Move
    }
}

/// The glyph a control paints and the hitbox the platform reads for it.
///
/// The area is what tells the window manager which corner of the surface is a
/// control rather than content: on X11 and Windows it answers the hit test, and
/// on Wayland it is what the window reports back to the app.
pub fn glyph(control: Control) -> (&'static str, WindowControlArea) {
    match control {
        Control::Minimize => ("\u{2013}", WindowControlArea::Min),
        Control::Maximize => ("\u{25a1}", WindowControlArea::Max),
        Control::Close => ("\u{2715}", WindowControlArea::Close),
    }
}
