//! Split panes, stage one (#2438): the chat shows two sessions side by side
//! (`ctrl+shift+|`) or stacked (`ctrl+shift+_`). This module holds the chord
//! and the layout rule; the window owns the panes and their transcripts.

/// How the two panes sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Side by side, a vertical divider: `ctrl+shift+|`.
    Side,
    /// One above the other, a horizontal divider: `ctrl+shift+_`.
    Stack,
}

impl Layout {
    /// The label the menu and the dialog print for this layout.
    pub fn label(self) -> &'static str {
        match self {
            Layout::Side => "|",
            Layout::Stack => "_",
        }
    }

    /// Whether the second pane is laid out as a column rather than a row.
    pub fn is_stacked(self) -> bool {
        matches!(self, Layout::Stack)
    }
}

/// The narrowest a pane may be dragged to, in px (#2476). Below this a pane
/// stops being a transcript and becomes a strip, so the drag stops here.
pub const MIN_PANE_PX: f32 = 240.0;

/// The share the panes start from: an even split (#2473, #2476).
pub const EVEN_SHARE: f32 = 0.5;

/// The first pane's share of the pane area after a drag step (#2476).
///
/// `travelled` is how far the pointer moved along the drag axis in px and
/// `span` is the pane area along that axis. The result is clamped so neither
/// pane can be dragged under [`MIN_PANE_PX`], and an area too small to hold
/// two panes at that minimum keeps the split where it was: collapsing a side
/// would be the one outcome a resize must never produce.
pub fn share_after_drag(from_share: f32, travelled: f32, span: f32) -> f32 {
    if !span.is_finite() || span <= MIN_PANE_PX * 2.0 {
        return from_share;
    }
    let floor = MIN_PANE_PX / span;
    (from_share + travelled / span).clamp(floor, 1.0 - floor)
}

/// The split chord for a keystroke holding control, or None (#2438, #2471).
///
/// The shifted glyph arrives as `key_char` on most layouts (`|` and `_`), and
/// as the base key with shift held on others (`\` and `-`), so both are
/// accepted. The glyph alone is enough: macOS folds shift into it and reports
/// `shift = false` for a keystroke that plainly typed `|` (`gpui`
/// `platform/mac/events.rs`), so demanding `control && shift` here meant the
/// chord could never fire on the window it was written for (#2471).
pub fn chord_layout(
    key: &str,
    key_char: Option<&str>,
    control: bool,
    shift: bool,
) -> Option<Layout> {
    if !control {
        return None;
    }
    match key_char.unwrap_or(key) {
        "|" => return Some(Layout::Side),
        "_" => return Some(Layout::Stack),
        _ => {}
    }
    // The base key still needs shift: ctrl+- is zoom out and a bare ctrl+\ is
    // nothing, so neither may open a pane.
    if !shift {
        return None;
    }
    match key {
        "\\" => Some(Layout::Side),
        "-" => Some(Layout::Stack),
        _ => None,
    }
}

/// Rows every pane spends on its own chrome: the top and bottom border.
pub const PANE_CHROME_ROWS: u16 = 2;

/// The fewest rows a pane's transcript may be squeezed to. Below this the
/// answer is not readable, so a pane gives up its strip instead of its body.
pub const PANE_BODY_FLOOR: u16 = 3;

/// Rows a pane's input takes. This is the TUI input box's real size: one
/// content row plus two borders, reused verbatim for the focused pane so
/// scroll and cursor math stay identical to the window input. The input does
/// not shrink on a short pane; the strip and the queue give way first.
pub const PANE_INPUT_ROWS: u16 = 3;

/// Rows a pane spends on its own compact footer: the model, the approval
/// policy and the pane's place in the split. It is the window status bar
/// moved inside the session panel, so it stays attached to the pane it
/// describes instead of spanning the whole window (#2575).
pub const PANE_FOOTER_ROWS: u16 = 1;

/// The fewest rows a compact plan strip can carry: the title row, the running
/// task and enough left over to read them apart.
pub const PANE_STRIP_MIN: u16 = 3;

/// The most rows a compact strip ever takes. Anything longer belongs in the
/// full checklist, which keeps the pane from turning into a plan viewer.
pub const PANE_STRIP_MAX: u16 = 4;

/// How a pane divides the rows inside its own border, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneRows {
    /// Transcript rows.
    pub body: u16,
    /// Compact plan strip rows, 0 when the pane cannot afford one.
    pub strip: u16,
    /// Queued-message preview rows, 0 when unaffordable or nothing queued.
    pub queue: u16,
    /// Input rows.
    pub input: u16,
    /// Footer rows at the very bottom of the pane, 0 when the pane cannot
    /// afford one.
    pub footer: u16,
}

impl PaneRows {
    /// Rows inside the pane's border, the sum the four fields must equal.
    pub fn inner(self) -> u16 {
        self.body + self.strip + self.queue + self.input + self.footer
    }
}

/// Rows a pane's compact plan strip may take (#2494).
///
/// The budget is pane-local on purpose: it reads the pane's own height and
/// nothing else, so two panes side by side can never take rows from each
/// other and moving focus cannot resize a strip. The yield order is fixed:
/// the transcript floor is reserved first, then the input, and the strip gets
/// whatever is left. A pane too short for [`PANE_STRIP_MIN`] rows after both
/// gets no strip at all, which is why an unfocused pane with no plan and a
/// focused pane with one can sit next to each other without either blanking.
pub fn pane_strip_height(pane_height: u16, desired: u16) -> u16 {
    // A pane with no plan asks for nothing and gets nothing: the clamp below
    // only raises a real request to a readable minimum, it never invents one.
    if desired == 0 {
        return 0;
    }
    let inner = pane_height.saturating_sub(PANE_CHROME_ROWS);
    let room = inner
        .saturating_sub(PANE_INPUT_ROWS)
        .saturating_sub(PANE_BODY_FLOOR);
    if room < PANE_STRIP_MIN {
        return 0;
    }
    desired.clamp(PANE_STRIP_MIN, room.min(PANE_STRIP_MAX))
}

/// Divide one pane's inner rows into transcript, strip and input (#2494).
///
/// [`pane_strip_height`] does the strip budget; this places it. The input is
/// fixed: a pane with no input has no reason to be on screen, and the
/// transcript floor is what makes the strip give way on a short pane.
pub fn pane_rows(pane_height: u16, desired_strip: u16) -> PaneRows {
    pane_rows_with(pane_height, desired_strip, 0)
}

/// [`pane_rows`] with a queued-message strip (#2494).
///
/// `desired_queue` is the request in rows (1 when the pane's session has a
/// queued message, 0 otherwise). The queue is the first thing to yield: it is
/// transient state, so when the pane cannot hold the transcript floor, the
/// input, and the plan strip with room left over, the queued preview is the
/// row that disappears. The plan strip is budgeted before the queue for the
/// same reason: a plan in flight outlives one queued message.
pub fn pane_rows_with(pane_height: u16, desired_strip: u16, desired_queue: u16) -> PaneRows {
    let inner = pane_height.saturating_sub(PANE_CHROME_ROWS);
    // The footer is identity, not content: it is reserved before anything
    // else, but only when the pane can spare the row. It yields to the
    // transcript floor, because a pane too short to read is worse than a
    // pane without a label.
    let footer = if inner >= PANE_INPUT_ROWS + PANE_BODY_FLOOR + PANE_FOOTER_ROWS {
        PANE_FOOTER_ROWS
    } else {
        0
    };
    let work = inner.saturating_sub(footer);
    let input = PANE_INPUT_ROWS.min(work);
    // Budget the strip against the rows the footer left behind, expressed in
    // the pane-height terms `pane_strip_height` expects.
    let strip = pane_strip_height(work.saturating_add(PANE_CHROME_ROWS), desired_strip);
    let fixed = input.saturating_add(strip).saturating_add(PANE_BODY_FLOOR);
    let queue = if desired_queue > 0 && work >= fixed.saturating_add(desired_queue) {
        desired_queue
    } else {
        0
    };
    PaneRows {
        body: work
            .saturating_sub(input)
            .saturating_sub(strip)
            .saturating_sub(queue),
        strip,
        queue,
        input,
        footer,
    }
}
