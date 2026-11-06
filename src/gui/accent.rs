//! What the theme modal drags, and the maths behind it (#2619).
//!
//! The maths is pure and lives in `config::gui_theme`, beside the palette it
//! derives from, so the tests reach it without a window. This module is the
//! window's handle on it, plus the one piece of state only the window has:
//! which control a drag under the pointer is editing.

pub use opencrabs::config::gui_theme::{inside, slider_value, wheel_drag, wheel_hit};

/// How many pixels of grace a press gets around a control's painted box, so a
/// drag that slips a little off an edge does not drop the control.
pub const SLACK: f32 = 4.0;

/// Which control the pointer is dragging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    /// The colour wheel: angle picks the hue, distance the saturation.
    Wheel,
    /// The saturation track.
    Saturation,
    /// The brightness track.
    Value,
}
