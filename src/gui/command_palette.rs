//! The Seeker overlay: Spotlight's shape, fatter. The field is its own fat
//! pill floating on top, the results a second rounded card below it with a
//! gap (#2491); kind chips under the pill, and a curated menu that waits
//! three seconds before it shows.
//!
//! The ranking, the keyword table and the state live in `ui_core::palette`, so
//! the terminal and the window print the same rows in the same order (#2458).
//! This file is the window's half: the scrim, the field, the rows and the
//! footer. The content is built by [`rows`], which is pure, so what the card
//! says can be tested without a window.

use gpui::{
    App, Bounds, ClickEvent, CursorStyle, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, LayoutId, MouseButton, Pixels, Point, RenderImage, ScrollWheelEvent,
    SharedString, Style, TextAlign, TextRun, WeakEntity, Window, WrappedLine, div, fill, img,
    linear_color_stop, linear_gradient, point, prelude::*, px, relative, rems, rgb, rgba, size,
};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use crate::{Dashboard, palette};
use opencrabs::backend::hello::HelloReply;
use opencrabs::ui_core::card::{SEEKER_RADIUS, seeker_concentric_radius};
use opencrabs::ui_core::commands::{Support, support_gui};
use opencrabs::ui_core::palette::{PaletteKind, PaletteState, SEEKER_PAGE, roast};

/// One printed row, decided without gpui. The renderer paints what this says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RowView {
    /// The focus marker: the row the arrows stand on, or the best match while
    /// nothing is armed.
    pub marker: &'static str,
    pub title: String,
    pub subtitle: String,
    pub kind: &'static str,
    pub armed: bool,
    /// A command the terminal runs and this window does not. It is printed
    /// anyway, dimmed, because hiding it would leave a user who saw it in the
    /// terminal wondering why it vanished here (#2458).
    pub missing: bool,
    /// The calculator's own answer. It is a result, not a match, so the card
    /// floats it instead of painting it like a row (#3066).
    pub calc: bool,
    /// A command the crab will not run for this window, and why: a missing
    /// scope, an older crab, or an action that only works on this machine.
    /// Printed dimmed with the reason in place of the enter hint, so a
    /// greyed out row says what is missing instead of failing when picked.
    pub unavailable: Option<String>,
}

/// The rows the card prints, in the order the engine ranked them.
///
/// With nothing typed the cursor is unarmed and the first row is still the one
/// Enter acts on, so the top row carries the marker before the user touches an
/// arrow. That is the whole "no dead keyboard" rule: the field keeps accepting
/// characters while a row is armed.
/// The slice the card prints: (start, len) inside the full filtered list
/// (#2520). The engine keeps the armed row inside this window, so arrows,
/// the wheel and the render all walk the same page.
pub fn window_range(state: &PaletteState) -> (usize, usize) {
    let len = state.visible().len();
    let start = state.scroll().min(len.saturating_sub(1));
    (start, SEEKER_PAGE.min(len.saturating_sub(start)))
}

pub fn rows(state: &PaletteState) -> Vec<RowView> {
    let visible = state.visible();
    let armed = state.cursor().unwrap_or(0);
    let (start, page) = window_range(state);
    visible
        .into_iter()
        .skip(start)
        .take(page)
        .enumerate()
        .map(|(ix, row)| RowView {
            marker: if ix + start == armed { MARKER } else { " " },
            // Read before the title is moved into the row: same value, and the
            // borrow checker has one order that works.
            missing: row.item.kind == PaletteKind::Command
                && support_gui(&row.item.title) == Support::Missing,
            calc: row.item.kind == PaletteKind::Calc,
            title: row.item.title,
            subtitle: row.item.subtitle,
            kind: kind_label(row.item.kind),
            armed: ix + start == armed,
            unavailable: None,
        })
        .collect()
}

/// [`rows`], with every command row the crab that answered `hello` will not
/// run marked with its reason. With no handshake yet this is [`rows`].
pub fn rows_gated(state: &PaletteState, hello: Option<&HelloReply>) -> Vec<RowView> {
    let mut printed = rows(state);
    for row in &mut printed {
        if row.kind == kind_label(PaletteKind::Command) {
            row.unavailable = command_unavailable(&row.title, hello);
        }
    }
    printed
}

/// Why the command row titled `title` is greyed out, or `None` when it runs.
fn command_unavailable(title: &str, hello: Option<&HelloReply>) -> Option<String> {
    use super::commands::{Outcome, parse_command, unavailable};
    match parse_command(title)? {
        Outcome::Run(action) | Outcome::RunWith(action, _) => unavailable(action, hello),
        Outcome::Missing(_) | Outcome::Unknown { .. } => None,
    }
}

/// The line under the field when nothing matched, or a count of what did.
pub fn status(state: &PaletteState) -> String {
    match state.query() {
        "" => String::new(),
        query => {
            let rows = state.visible();
            if rows.is_empty() {
                // No rows at all: blank words. The roast still says it.
                roast(query).to_string()
            } else if rows.len() == 1 && rows[0].item.kind == PaletteKind::Calc {
                // The field is a sum, so there is no search to count: Enter
                // copies the answer, the way Spotlight does.
                "enter copies the result".to_string()
            } else if rows.len() == 1 && rows[0].item.kind == PaletteKind::Ask {
                "no match. enter asks a fresh session".to_string()
            } else {
                let matches = rows
                    .iter()
                    // Neither the miss row nor the answer row is a search
                    // result, so neither counts toward the total.
                    .filter(|row| !matches!(row.item.kind, PaletteKind::Ask | PaletteKind::Calc))
                    .count();
                if matches == 1 {
                    "1 match".to_string()
                } else {
                    format!("{matches} matches")
                }
            }
        }
    }
}

/// One click handler per printed row, built by the caller because only it holds
/// the context the action needs. Named so the card's signature, and every call
/// site that fills it, says what the box holds.
pub type PickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One kind chip's press: `None` is the All chip. The caller builds it,
/// because only it can mutate the state behind the card (#2491).
pub type ChipHandler = Box<dyn Fn(Option<PaletteKind>, &mut App) + 'static>;

/// The chips the card prints, in paint order. Four kinds cover what the card
/// actually lists; anything rarer stays reachable by typing its word.
fn chip_defs() -> Vec<(&'static str, Option<PaletteKind>)> {
    vec![
        ("all", None),
        ("screens", Some(PaletteKind::Surface)),
        ("commands", Some(PaletteKind::Command)),
        ("docs", Some(PaletteKind::Docs)),
        ("sessions", Some(PaletteKind::Session)),
    ]
}

/// Spotlight's shape: the field is its own fat pill floating on top, and
/// the results are a second rounded card below it with a gap between the
/// two. `pick` holds one handler per printed row, in the same order
/// [`rows`] prints them, so a click acts on the row it landed on and nothing
/// else. `chip` is the press handler for the kind chips. `setup` is the line
/// for a channel the query names but the config does not carry yet (#2462).
/// `idle_revealed` is the caller's [`PaletteState::idle_ready`] verdict: a
/// fresh card paints the pill alone until it says otherwise (#2491).
/// Both elements are always dark glass, so their ink is fixed light-on-dark.
/// A theme's `text` colour belongs to the surface behind the window, and
/// using it here printed near-black words on a dark card under every light
/// theme.
const INK: u32 = 0xf2f3f5;
/// The softer muted silver the secondary line wears. The old grey read as a
/// disabled control next to the white query, which is the wrong signal: this
/// text is an invitation, not a dead one.
const INK_MUTED: u32 = 0xa8adb8;
const INK_WARM: u32 = 0xe8b96a;

/// The inactive kind chip's translucent white fill. Filled capsules read as
/// one Spotlight control; the old transparent, bordered chips read as labels.
const CHIP_FILL: u32 = 0xffffff14;

/// The active kind chip's fill: the same neutral lift as an armed row, one
/// step stronger so a switched-on capsule still reads as switched on. The old
/// fill was the theme accent, which tinted the capsule warm over a smoked
/// card (#3066).
const CHIP_ACTIVE_FILL: u32 = 0xffffff26;

/// The armed row's fill.
///
/// A theme accent works as a mark and fails as a wash: at 18 percent across a
/// whole row it stops reading as a colour and starts reading as a tint, and
/// over near-black glass that tint came out brown. Spotlight lifts the same
/// row with plain white at low alpha instead, so the highlight is a shade of
/// the glass and never a hue laid over it (#3066).
const ARMED_FILL: u32 = 0xffffff14;

/// The frosted fill the calculator's own result card wears: a whisper of white
/// over the smoked glass, the way macOS floats a result above a surface
/// instead of painting a coloured bar across it.
const FROST_FILL: u32 = 0xffffff1c;

/// The invitation the field prints while it is empty.
///
/// Short on purpose (#3044): the field is a thick pill, and a sentence long
/// enough to fill it reads as a value typed into the box instead of an
/// invitation to type one. Spotlight's own two-word form, so the line never
/// competes with what a person types and never has to be measured against the
/// pill's width.
const PLACEHOLDER: &str = "Search or Ask";

/// The grey tail the field prints after a typed query, Siri's move: the words
/// a person wrote stay white, and the invitation to hand them to the crab
/// follows in the same line, dimmer, so the field says what Enter can do
/// without a legend underneath (#3066).
const ASK_SUFFIX: &str = " - Ask the crab";

/// The field's vertical padding (#3044): the pill is a target a hand aims at,
/// so its height comes from the pillow around the line and not from a bigger
/// font, which would make the query look like a heading.
const FIELD_PAD_Y: f32 = 22.0;

/// The field's left inset: where the query starts, measured from the curve.
///
/// The microphone's glyph does not sit at the right inset itself: its tile is
/// wider than the glyph it centres, so the visible gap on that side is 30 plus
/// the tile's own bearing, about 35.6. The query starts level with it instead
/// of 14 short, which is what put the caret left of the card's own rows and
/// made the field look shoved into the curve (#3066).
const FIELD_PAD_X: f32 = 34.0;

/// The field's right inset, measured to the microphone's tile and not to its
/// glyph. A tile wider than its glyph is what makes the visible right gap
/// larger than this number; the left inset is set to match the visible gap.
const FIELD_PAD_RIGHT: f32 = 30.0;

/// The space the microphone puts between itself and the line it follows
/// (#3044). It is the control's own inset rather than a gap on the pill: a
/// flex gap beside the shaped line would move the caret, which has to sit
/// exactly where the text does (#2531).
const FIELD_MIC_GAP: f32 = 14.0;

/// The microphone's tile and glyph. A step larger than the composer's again
/// (#3044): at the field's `text_2xl` line the old tile read as a small
/// leftover rather than the control a hand aims at.
const FIELD_MIC_TILE: f32 = 2.35;
const FIELD_MIC_GLYPH: f32 = 1.65;

/// The smoked glass the card floats on: near-black, faintly transparent, so
/// white ink lands hard against it and the code behind reads as a shadow
/// rather than as texture (#3066).
const GLASS_TOP: u32 = 0x0b0b0ee8;
const GLASS_BOTTOM: u32 = 0x0a0a0cd8;

/// The field's fill: darker than the card, because the pill sits on top of the
/// scrim and the card sits under it, and the stack has to read top-down.
const PILL_FILL: u32 = 0x0c0c10f2;

/// The invitation's glass rim. Spotlight's edge is not a drawn border: it is a
/// lit edge, brightest along the top where the light falls, thinning as it
/// turns down the sides, and catching once more along the bottom curve. A
/// uniform 1px stroke read as a debug outline at every size (#3066).
const RIM_TOP: u32 = 0xffffff5c;
const RIM_BOTTOM: u32 = 0xffffff24;
/// The faint edge the frosted result card catches along its top, and the
/// translucent disc its copy mark sits in.
const RIM_EDGE: u32 = 0xffffff33;
const DISC_FILL: u32 = 0xffffff1f;
const RIM_WIDTH: f32 = 1.0;

/// The frosted result card's own curve, tighter than the card it sits in: an
/// inner surface nests, so it never repeats the outer radius.
const CALC_CARD_RADIUS: f32 = 16.0;

/// The copy disc's diameter: a control a cursor lands on, not a tag.
const CALC_COPY_DISC: f32 = 26.0;

/// The field's curve. `rounded_full` is 9999 px, which the quad clamps to half
/// the pill's height: stating it as a number lets the rim behind it round to the
/// same curve instead of guessing at a smaller radius and leaving a lip.
const PILL_RADIUS: f32 = 9999.0;

/// The gap between the field and the card. Spotlight holds them apart enough
/// that the two read as one island with a seam, not as one tall panel.
const SEEKER_GAP: f32 = 10.0;

/// The card's inner gutter and the row's own inset. Their sum is the 24 px a
/// row's text sits from the glass edge, which is the measure Spotlight keeps
/// and the old 20 px missed.
const SEEKER_GUTTER: f32 = 12.0;
const SEEKER_ROW_PAD_X: f32 = 12.0;

/// The rail the card's chrome stands on: the chips, the status line and the
/// legend all start where a row's text starts, so the card reads as one column
/// with a single left edge. Three surfaces had drifted onto their own insets
/// (chips and footers at the raw 12 px gutter, rows at 24), which put the
/// chrome left of every row beneath it and made the card look unaligned. It is
/// the gutter plus the row's own inset by construction, never a second guess
/// at the same measure (#3066).
const SEEKER_CARD_PAD_X: f32 = SEEKER_GUTTER + SEEKER_ROW_PAD_X;

/// The floating take's diameter (#3044). Siri-sized rather than chip-sized:
/// while the microphone is open the bubble is the only thing on the screen, so
/// it has to carry the whole state at a glance from across a desk.
const TAKE_BUBBLE: f32 = 9.0;

/// The line the field prints while it is empty. An armed pick names its job
/// instead of inviting a query: the list under it is a set of conversations
/// to choose from, and nothing in the rows says which job the choice serves
/// (#2711).
pub(super) fn field_prompt(state: &PaletteState) -> &'static str {
    match state.pick() {
        Some(purpose) => opencrabs::ui_core::palette::pick_prompt(purpose),
        None => PLACEHOLDER,
    }
}

/// The shaped line and bounds from the latest paint. Mouse hit testing reads
/// this snapshot; the shared palette state never depends on GPUI types.
pub(super) struct FinderPaint {
    pub line: Option<WrappedLine>,
    pub bounds: Option<Bounds<Pixels>>,
    pub line_height: Pixels,
}

impl Default for FinderPaint {
    fn default() -> Self {
        Self {
            line: None,
            bounds: None,
            line_height: Pixels::ZERO,
        }
    }
}

/// Selection geometry in field-local pixels, derived from the real glyph
/// positions rather than an estimated character width (#2534).
fn selection_highlight(
    line: &WrappedLine,
    range: &Range<usize>,
    line_height: Pixels,
) -> Option<(Point<Pixels>, Point<Pixels>)> {
    if range.is_empty() {
        return None;
    }
    Some((
        line.position_for_index(range.start, line_height)?,
        line.position_for_index(range.end, line_height)?,
    ))
}

struct SeekerTextElement {
    paint: Entity<FinderPaint>,
    query: String,
    /// The invitation the field prints while the query is empty: the armed
    /// pick's own sentence, or the generic one (#2711).
    placeholder: &'static str,
    /// The grey tail the field prints straight after a typed query (#3066).
    /// Empty while an armed pick lists conversations, because there the field
    /// is naming a menu rather than offering to ask about what was typed.
    suffix: &'static str,
    caret: usize,
    selection: Option<Range<usize>>,
    caret_on: bool,
}

impl IntoElement for SeekerTextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

struct SeekerPrepaint {
    line: WrappedLine,
    selection: Option<(Point<Pixels>, Point<Pixels>)>,
    caret: Option<Point<Pixels>>,
}

impl Element for SeekerTextElement {
    type RequestLayoutState = ();
    type PrepaintState = SeekerPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let empty = self.query.is_empty();
        // Siri's line, not one flat colour (#3066): what the person typed stays
        // white, and the invitation that follows it is grey. The field then
        // says what Enter can do in the same breath as the query, which is why
        // the legend under the card is chrome rather than instruction.
        let suffix = if empty { "" } else { self.suffix };
        let display = if empty {
            self.placeholder.to_string()
        } else {
            format!("{}{}", self.query, suffix)
        };
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let font = style.font();
        let mut runs = vec![TextRun {
            len: if empty {
                display.len()
            } else {
                self.query.len()
            },
            font: font.clone(),
            color: rgb(if empty { INK_MUTED } else { INK }).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        if !suffix.is_empty() {
            runs.push(TextRun {
                len: suffix.len(),
                font,
                color: rgb(INK_MUTED).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        let line = window
            .text_system()
            .shape_text(SharedString::from(display), font_size, &runs, None, None)
            .ok()
            .and_then(|lines| lines.into_iter().next())
            .unwrap_or_default();
        let line_height = window.line_height();
        let selection = self
            .selection
            .as_ref()
            .and_then(|range| selection_highlight(&line, range, line_height));
        // The caret is a glyph coordinate now. It hugs the actual insertion
        // point, including the middle of a query, and disappears while a
        // selection is live (#2531, #2534). The index is clamped to the query's
        // own bytes, so a stray cursor can never paint the caret inside the
        // grey tail: the tail is not text anyone can type into (#3066).
        let caret = if self.caret_on && self.selection.is_none() {
            line.position_for_index(self.caret.min(self.query.len()), line_height)
        } else {
            None
        };
        self.paint.update(cx, |paint, _cx| {
            paint.line = Some(line.clone());
            paint.bounds = Some(bounds);
            paint.line_height = line_height;
        });
        SeekerPrepaint {
            line,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_height = window.line_height();
        if let Some((from, to)) = prepaint.selection {
            window.paint_quad(fill(
                Bounds::from_corners(
                    point(bounds.left() + from.x.min(to.x), bounds.top() + from.y),
                    point(
                        bounds.left() + from.x.max(to.x),
                        bounds.top() + from.y.max(to.y) + line_height,
                    ),
                ),
                rgba(0x0a84ff66),
            ));
        }
        prepaint
            .line
            .paint(
                bounds.origin,
                line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            )
            .unwrap();
        if let Some(caret) = prepaint.caret {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left() + caret.x, bounds.top() + caret.y),
                    size(px(2.5), line_height),
                ),
                rgb(INK),
            ));
        }
    }
}

/// The query field paints shaped text, selection and caret as one element.
/// Mouse handlers update the same `PaletteState` the keyboard edits, so a
/// click, a drag and a chord can never disagree about the insertion point.
pub(super) fn query_line(
    state: &PaletteState,
    view: WeakEntity<Dashboard>,
    paint: Entity<FinderPaint>,
    caret_on: bool,
) -> impl IntoElement {
    let (before, selected, _) = state.render_spans();
    let selection = (!selected.is_empty()).then(|| before.len()..before.len() + selected.len());
    let element = SeekerTextElement {
        paint,
        query: state.query().to_string(),
        placeholder: field_prompt(state),
        // An armed pick lists conversations, so there is nothing to ask: the
        // grey tail belongs to a query, not to a menu (#3066).
        suffix: if state.pick().is_none() {
            ASK_SUFFIX
        } else {
            ""
        },
        caret: before.len(),
        selection,
        caret_on,
    };
    let down = view.clone();
    let up = view.clone();
    let up_out = view.clone();
    let drag = view;
    div()
        .id("seeker-query-field")
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .text_2xl()
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            let _ = down.update(cx, |this, cx| this.finder_mouse_down(event, cx));
        })
        .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
            let _ = up.update(cx, |this, cx| this.finder_mouse_up(cx));
        })
        .on_mouse_up_out(MouseButton::Left, move |_event, _window, cx| {
            let _ = up_out.update(cx, |this, cx| this.finder_mouse_up(cx));
        })
        .on_mouse_move(move |event, _window, cx| {
            let _ = drag.update(cx, |this, cx| this.finder_mouse_move(event, cx));
        })
        .child(element)
}

/// The Seeker's take, as the window paints it (#3044).
///
/// Derived from the window's handles on every frame rather than stored: the
/// bubble must never paint a microphone that already closed.
pub(crate) struct SeekerVoice {
    /// The device is open and samples are arriving.
    pub recording: bool,
    /// The newest high-resolution glass frame from the orb worker.
    pub frame: Arc<RenderImage>,
}

/// The dictation control on the Seeker field (#3044).
///
/// Same glyph, colour and click as the composer's microphone, so the two are
/// one control on two surfaces: a person who learned it in the field finds it
/// in the card, and the hover says the same thing on both.
///
/// The take starts here, in the card, and the words land in the field itself: a
/// search query has to end up in the box the person is looking at, which is why
/// handing the job to the composer behind the card was the wrong shape. The
/// open microphone is not drawn on this control though: while a take runs the
/// card leaves the window and the take floats as one bubble in its middle
/// (#3044), so the tile is only ever painted idle.
///
/// The tile is one step larger than the composer's (2.35rem against 1.8rem,
/// with a 1.65rem glyph against 1.25rem) because the field around it is: the
/// line here is `text_2xl`, and a control sized for the composer's line would
/// read as a leftover from the surface it came from (#3044).
fn field_mic(view: WeakEntity<Dashboard>) -> impl IntoElement {
    let theme = palette::current();
    div()
        .id("seeker-mic")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .ml(px(FIELD_MIC_GAP))
        .size(rems(FIELD_MIC_TILE))
        .rounded_sm()
        .cursor_pointer()
        .opacity(0.8)
        .hover(|d| d.opacity(1.0))
        // The hover names the key, exactly as the composer's microphone does:
        // two surfaces, one binding, one sentence about it.
        .tooltip(|_window, cx| crate::hint::mic_tooltip(crate::hint::mic_line(), cx))
        .child(crate::icon::mic_icon(theme.text_muted, FIELD_MIC_GLYPH))
        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
            // The click belongs to the control, not to the scrim under it.
            cx.stop_propagation();
            let _ = view.update(cx, |this, cx| this.seeker_dictate(cx));
        })
}

/// The take, as one animated glass bubble in the middle of the window (#3044).
///
/// The worker supplies the transparent material, sized to this bubble. GPUI
/// paints it as an image rather than stacking static circles, so the liquid
/// ribbons and the microphone response move together. Nothing sits over the
/// glass: the whole bubble remains the click target that ends the take for
/// editing.
fn seeker_bubble(voice: &SeekerVoice, view: WeakEntity<Dashboard>) -> impl IntoElement {
    div()
        .id("seeker-take-bubble")
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(rems(TAKE_BUBBLE))
        .when(voice.recording, |d| d.cursor(CursorStyle::PointingHand))
        .on_click(move |_ev, _window, cx| {
            cx.stop_propagation();
            let _ = view.update(cx, |this, cx| this.seeker_stop_for_edit(cx));
        })
        .child(img(voice.frame.clone()).absolute().inset_0().size_full())
}

/// The lit edge both surfaces wear, painted as a one-pixel larger quad behind
/// the fill rather than as a border (#3066).
///
/// A border is one colour on all four sides, which is exactly why the old card
/// read as a drawn outline: the top edge caught no light and the bottom edge was
/// as loud as the top. This quad sits `RIM_WIDTH` outside the fill and carries a
/// vertical gradient, so the rim is brightest where light would land on the top
/// curve, thins as it turns down the sides, and shows once more along the
/// bottom. The fill painted over it owns the inner curve, so the two round
/// together and no seam can open at a corner.
fn rim_under(radius: f32) -> impl IntoElement {
    div()
        .absolute()
        .inset(px(-RIM_WIDTH))
        .rounded(px(radius + RIM_WIDTH))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgba(RIM_TOP), 0.0),
            linear_color_stop(rgba(RIM_BOTTOM), 1.0),
        ))
}

/// Presentation state for one Seeker frame. Grouping the borrowed engine,
/// reveal flags and paint handles keeps the renderer's callback list readable.
pub(super) struct FinderOverlayState<'a> {
    pub state: &'a PaletteState,
    pub idle_revealed: bool,
    pub caret_on: bool,
    pub view: WeakEntity<Dashboard>,
    pub paint: Entity<FinderPaint>,
    /// The card's own take, or `None` while the microphone is idle (#3044).
    pub voice: Option<SeekerVoice>,
    /// The handshake of the crab the window holds, so a command it will not
    /// run is drawn greyed out with its reason. `None` greys nothing out.
    pub hello: Option<&'a HelloReply>,
}

pub(super) fn overlay(
    frame: FinderOverlayState<'_>,
    pick: Vec<PickHandler>,
    chip: ChipHandler,
    wheel: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
    setup: Option<&'static str>,
    voice_error: Option<SharedString>,
) -> impl IntoElement {
    let FinderOverlayState {
        state,
        idle_revealed,
        caret_on,
        view,
        paint,
        voice,
        hello,
    } = frame;
    let theme = palette::current();
    let printed = rows_gated(state, hello);
    let idle = state.query().trim().is_empty();
    let pick_armed = state.pick().is_some();
    // The reveal gate: picker rows and typed results print at once, the
    // idle menu holds for IDLE_REVEAL_DELAY so the first paint is the field
    // and the invitation alone (#2491).
    let reveal = pick_armed || !idle || idle_revealed;

    // The glass is always dark, Spotlight-style, light theme included. The
    // arming marks are neutral lifts rather than the theme accent, because a
    // wash of accent across a whole row reads as a brown tint on near-black
    // glass (#2491, #3066).
    //
    // Smoked, not slate (#3066). The old fill was a blue-grey close enough to
    // the dimmed code behind it that the two read as one surface; a near-black
    // crystal with a little transparency puts the white ink in hard contrast and
    // lets the code show through as a shadow. The idle menu stays more
    // transparent at the bottom, so it still reads as a hint rather than a wall.
    let glass = if idle && idle_revealed && !pick_armed {
        linear_gradient(
            180.0,
            linear_color_stop(rgba(GLASS_TOP), 0.0),
            linear_color_stop(rgba((GLASS_BOTTOM & 0xffffff00) | 0xb8), 1.0),
        )
    } else {
        linear_gradient(
            180.0,
            linear_color_stop(rgba(GLASS_TOP), 0.0),
            linear_color_stop(rgba(GLASS_BOTTOM), 1.0),
        )
    };
    let armed_bg = rgba(ARMED_FILL);
    let chip = Rc::new(chip);

    // Inert: a click on the shell stops here instead of reaching the
    // scrim, so reading the list never closes it.
    let mut shell = div()
        .id("command-palette")
        .flex()
        .flex_col()
        // The seam is its own measure: Spotlight holds the field and the card
        // apart just enough that they read as one island with a break in it,
        // and the old flex gap was a stack's spacing, not this gap.
        .gap(px(SEEKER_GAP))
        .w(rems(34.0))
        .on_click(|_ev, _window, cx| cx.stop_propagation());

    // The field: Spotlight's floating pill, fatter than his (#3044). The
    // invitation is a short phrase now, so the height is a pillow around the
    // line instead of a box sized for a sentence, and the microphone closes the
    // row the way it closes the composer's: one control, two surfaces. The pill
    // carries no flex gap, because the caret rides the shaped line and a gap
    // beside it would move the one thing that has to sit exactly where the text
    // does (#2531); the microphone brings its own inset instead.
    // Two handles on the window: the microphone's click and the glass bubble's
    // click. The field's shaped line takes the original, so the copies have to
    // be made before it does (#3044).
    let dictate = view.clone();
    let bubble_view = view.clone();
    let mut field = div()
        .flex()
        .flex_row()
        .items_center()
        .pl(px(FIELD_PAD_X))
        .pr(px(FIELD_PAD_RIGHT))
        .py(px(FIELD_PAD_Y))
        .rounded(px(PILL_RADIUS))
        .bg(rgba(PILL_FILL))
        .child(query_line(state, view, paint, caret_on));
    // The microphone closes the field's row in the one state this card paints
    // it: idle. A running take leaves the card entirely for its own bubble
    // (#3044), so nothing can hang a second row off the field here.
    field = field.child(field_mic(dictate));
    // The rim rides the wrapper, not the fill, so the shadow and the lit edge
    // never paint over each other (#3066).
    let pill = div()
        .relative()
        .rounded(px(PILL_RADIUS))
        .shadow_2xl()
        .child(rim_under(PILL_RADIUS))
        .child(field);
    shell = shell.child(pill);

    // A failed take says so here, for the same reason: the composer's notice
    // sits behind the card and would be read by nobody (#3044).
    if let Some(reason) = voice_error {
        shell = shell.child(
            div()
                .px(px(FIELD_PAD_X))
                .text_xs()
                .text_color(rgb(theme.red))
                .child(reason),
        );
    }

    let mut card = div()
        .flex()
        .flex_col()
        .max_h(px(480.0))
        .rounded(px(SEEKER_RADIUS))
        .bg(glass)
        .overflow_hidden();

    if reveal {
        // The kind chips. Hidden while a session pick is armed: a narrowed
        // conversation list has no kinds to filter (#2491).
        if !pick_armed {
            let wanted = state.kind_filter();
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .px(px(SEEKER_CARD_PAD_X))
                .pt_4()
                .pb_3();
            for (ix, (label, kind)) in chip_defs().into_iter().enumerate() {
                let active = wanted == kind;
                let press = chip.clone();
                row = row.child(
                    div()
                        .id(("palette-chip", ix))
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .text_xs()
                        .cursor_pointer()
                        .when(active, |d| {
                            d.bg(rgba(CHIP_ACTIVE_FILL)).text_color(rgb(INK))
                        })
                        .when(!active, |d| {
                            d.bg(rgba(CHIP_FILL)).text_color(rgb(INK_MUTED))
                        })
                        .hover(|d| {
                            if active {
                                d.bg(rgba(0xffffff33))
                            } else {
                                d.bg(rgba(0xffffff28)).text_color(rgb(INK))
                            }
                        })
                        .on_click(move |_ev, _window, cx| {
                            cx.stop_propagation();
                            press(kind, cx);
                        })
                        .child(SharedString::from(label)),
                );
            }
            card = card.child(row);
        }

        // The rows: title only, subtitle and enter hint on the armed row,
        // and the one right-edge word that does work, terminal only (#2491).
        let mut list = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_0p5()
            .px(px(SEEKER_GUTTER))
            .pb_2()
            .on_scroll_wheel(move |ev, window, cx| wheel(ev, window, cx));
        let mut handlers = pick.into_iter();
        for (ix, rowv) in printed.iter().enumerate() {
            let armed = rowv.armed;
            let mut cell = div()
                .id(("palette-row", ix))
                .flex()
                .flex_row()
                .min_w_0()
                .items_center()
                .gap_2()
                .px(px(SEEKER_ROW_PAD_X))
                .py_1p5()
                .rounded(px(seeker_concentric_radius(12.0)))
                .cursor_pointer()
                // The calculator's answer is a result, not a selection, so it
                // floats on its own frosted card the way macOS floats a
                // conversion: a whisper of white over the glass, a lit top
                // edge, and the copy disc on its right (#3066). Every other
                // armed row takes the plain grey lift, because those rows are
                // choices and this one is an answer.
                .when(armed && rowv.calc, |d| {
                    d.bg(rgba(FROST_FILL))
                        .rounded(px(CALC_CARD_RADIUS))
                        .border_t_1()
                        .border_color(rgba(RIM_EDGE))
                })
                .when(armed && !rowv.calc, |d| d.bg(armed_bg))
                // A row under the pointer answers before it is clicked: the
                // lift is a tenth of white, enough to read on the smoked glass
                // and quiet enough that the armed row still owns the eye.
                .when(!armed, |d| d.hover(|d| d.bg(rgba(0xffffff0d))));
            let dimmed = rowv.missing || rowv.unavailable.is_some();
            if armed && !dimmed {
                cell = cell.child(
                    div()
                        .w(px(10.0))
                        .text_xs()
                        .text_color(rgb(INK))
                        .child(SharedString::from(MARKER)),
                );
            } else {
                cell = cell.child(div().w(px(10.0)));
            }
            cell = cell.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if dimmed { INK_MUTED } else { INK }))
                            .truncate()
                            .child(SharedString::from(rowv.title.clone())),
                    )
                    .when(armed && !rowv.subtitle.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(INK_MUTED))
                                .truncate()
                                .child(SharedString::from(rowv.subtitle.clone())),
                        )
                    }),
            );
            // The calc row trades the kind tag for its copy disc: the title
            // already reads `= answer`, and two words about what the row is
            // would crowd the one control it offers (#3066).
            if !(rowv.calc && armed) {
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(INK_MUTED))
                        .mr_2()
                        .child(SharedString::from(rowv.kind)),
                );
            }
            if rowv.calc && armed {
                // The answer's own affordance: a translucent disc carrying the
                // copy mark, the same act Enter performs and the same glyph the
                // terminal prints, so the row says what it does without reading
                // the legend (#3066).
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(CALC_COPY_DISC))
                        .rounded_full()
                        .bg(rgba(DISC_FILL))
                        .child(crate::icon::copy_icon(INK_MUTED, 0.8)),
                );
            } else if rowv.missing {
                // A command the terminal runs and this window does not keeps
                // its tag, dimmed; hiding it would leave a user who saw it
                // in the terminal wondering why it vanished (#2458).
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(INK_MUTED))
                        .child("terminal only"),
                );
            } else if let Some(reason) = &rowv.unavailable {
                // A command the crab will not run here says why in the slot
                // the enter hint would take, so picking it is never the way
                // to find out.
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(INK_MUTED))
                        .child(SharedString::from(reason.clone())),
                );
            } else if armed {
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(INK_MUTED))
                        .child("↵ enter"),
                );
            }
            if let Some(handler) = handlers.next() {
                cell = cell.on_click(handler);
            }
            list = list.child(cell);
        }
        card = card.child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .overflow_hidden()
                .child(list),
        );

        // Status only when it does work: the roast on a miss, the ask row's
        // naming. The count is chrome; the rows are the answer (#2491).
        let miss = printed.is_empty() && !state.is_empty();
        let vis = state.visible();
        let asks = vis.len() == 1 && vis[0].item.kind == PaletteKind::Ask;
        if miss || asks {
            card = card.child(
                div()
                    .px(px(SEEKER_CARD_PAD_X))
                    .pb_3()
                    .text_xs()
                    .text_color(rgb(INK_MUTED))
                    .child(SharedString::from(status(state))),
            );
        }

        if let Some(text) = setup {
            card = card
                .child(
                    div()
                        .px(px(SEEKER_CARD_PAD_X))
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(INK_WARM))
                        .child(SharedString::from(text)),
                )
                .child(
                    div()
                        .px(px(SEEKER_CARD_PAD_X))
                        .pb_2()
                        .text_xs()
                        .text_color(rgb(INK_MUTED))
                        .child("enter starts the setup · esc closes"),
                );
        } else if pick_armed {
            // An armed pick answers Enter with the pick, not with the row the
            // line would run, so the legend names that (#2711).
            card = card.child(
                div()
                    .px(px(SEEKER_CARD_PAD_X))
                    .pb_3()
                    .text_xs()
                    .text_color(rgb(INK_MUTED))
                    .child("enter pick · up/down move · esc cancel"),
            );
        } else if !idle && !miss && !asks {
            // The legend prints only under a typed list; an idle card ends
            // at its rows so the glass stays a hint (#2491).
            card = card.child(
                div()
                    .px(px(SEEKER_CARD_PAD_X))
                    .pb_3()
                    .text_xs()
                    .text_color(rgb(INK_MUTED))
                    .child("enter run · up/down move · esc close"),
            );
        }

        // The card wears the same lit edge as the field and floats on the same
        // deeper shadow, so the two read as one island rather than a bordered
        // box under a pill (#3066).
        shell = shell.child(
            div()
                .relative()
                .rounded(px(SEEKER_RADIUS))
                .shadow_2xl()
                .child(rim_under(SEEKER_RADIUS))
                .child(card),
        );
    }

    let scrim = div()
        .id("command-palette-scrim")
        // Modal: the scrim owns the pointer, so a click meant for the window
        // behind it cannot act on anything but the card.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .bg(rgba(0x08080acc));
    // A live take is one bubble in the middle of the window and nothing else
    // (#3044): the field and the rows would compete with the thing the person
    // is watching, and the words are coming back into that field in a moment
    // anyway. Green here means the take was handed over and the wait is the
    // transcriber's, not the microphone's.
    if let Some(take) = voice.as_ref() {
        return scrim
            .justify_center()
            .child(seeker_bubble(take, bubble_view));
    }
    // Spotlight floats the card high, not centered: the eye reads it as
    // a command surface, not a dialog (#2491).
    scrim
        .justify_start()
        .pt(rems(6.5))
        // scrim.child(shell): the pill and the results card float as one
        // island; the shell carries both, never the card alone (#2495).
        .child(shell)
}

/// The kind label for a row, exposed so a test can check the column without
/// reaching into the engine.
pub fn kind_label(kind: PaletteKind) -> &'static str {
    kind.label()
}

/// The marker the card prints for a row, kept here so the test and the paint
/// read the same string.
pub const MARKER: &str = ">";

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::palette::{PaletteAction, PaletteItem, Surface};

    fn state_with(items: Vec<PaletteItem>) -> PaletteState {
        PaletteState::new(items)
    }

    fn item(title: &str, kind: PaletteKind) -> PaletteItem {
        PaletteItem::new(
            title,
            "detail",
            kind,
            PaletteAction::OpenSurface(Surface::Help),
        )
    }

    /// The card prints only the page under the scroll offset (#2520), so a
    /// list longer than the clip stays steppable instead of clipped.
    #[test]
    fn rows_print_only_the_page_under_the_cursor() {
        let mut state = state_with(
            (0..12)
                .map(|ix| item(&format!("row{ix:02}"), PaletteKind::Command))
                .collect(),
        );
        for _ in 0..9 {
            state.move_by(1, SEEKER_PAGE);
        }
        let (start, page) = window_range(&state);
        assert!(page <= SEEKER_PAGE);
        let printed = rows(&state);
        assert_eq!(printed.len(), page);
        assert_eq!(printed[0].title, format!("row{start:02}"));
        assert!(
            printed.iter().any(|r| r.armed),
            "the armed row lands inside the printed window"
        );
    }

    /// Every row says what it is: a docs entry reads "docs" and a command
    /// reads "command", so nobody clicks a docs link expecting setup (#2521).
    #[test]
    fn every_row_carries_its_kind_label() {
        let state = state_with(vec![
            item("Channels overview", PaletteKind::Docs),
            item("Quit", PaletteKind::Command),
        ]);
        let printed = rows(&state);
        assert_eq!(printed.len(), 2);
        assert!(printed.iter().all(|r| !r.kind.is_empty()));
        let docs = printed
            .iter()
            .find(|r| r.title == "Channels overview")
            .expect("the docs row prints");
        assert_eq!(docs.kind, "docs");
    }

    /// The caret rides the actual insertion glyph, not a flex gap or an
    /// estimated character width (#2531, #2534). The shaped line also owns
    /// selection painting, so moving the caret cannot detach it from the text.
    #[test]
    fn seeker_caret_hugs_the_query_in_one_gap_less_row() {
        let src = include_str!("command_palette.rs");
        assert!(
            src.contains("line.position_for_index(self.caret.min(self.query.len()), line_height)"),
            "the caret is painted at the shaped insertion point, clamped to the query so it can never sit inside the grey tail"
        );
        assert!(
            src.contains("fn selection_highlight"),
            "selection geometry comes from that same shaped line"
        );
        let line = src
            .find("fn query_line")
            .expect("the query line is one function, so no caller can drift");
        let body = src.get(line..).expect("query_line starts on a glyph");
        let body_end = body.find("\n}\n").expect("query_line closes");
        let body = body
            .get(..body_end)
            .expect("the closing brace starts on a glyph");
        assert!(
            !body.contains("gap_"),
            "no flex gap can insert phantom space beside the caret"
        );
        let field = src
            .find("let mut field = div()")
            .expect("the field row is drawn in overlay()");
        let field_src = src.get(field..).expect("the field starts on a glyph");
        let field_end = field_src
            .find(".child(query_line(")
            .expect("the field draws the line");
        let field_src = field_src
            .get(..field_end)
            .expect("the query line starts on a glyph");
        assert!(
            !field_src.contains("gap_3"),
            "the field row itself carries no gap either"
        );
    }

    /// The typed line keeps Siri's shape (#3066): the query in white, the
    /// invitation in grey straight after its last character, and the caret on
    /// that boundary so it can never be painted inside the tail.
    #[test]
    fn the_field_prints_the_ask_invitation_after_the_query() {
        let src = include_str!("command_palette.rs");
        assert!(
            src.contains("const ASK_SUFFIX: &str"),
            "the tail is one named string, not a literal repeated in two places"
        );
        // The arm is read from its own head rather than as one line, so a
        // rewrap by the formatter cannot fail a test about behaviour.
        let suffix_arm = src
            .split("suffix: if state.pick().is_none() {")
            .nth(1)
            .expect("the tail is keyed to the query, not to a pick");
        assert!(
            suffix_arm.trim_start().starts_with("ASK_SUFFIX"),
            "a picker lists conversations, so only a query carries the invitation"
        );
        assert!(
            src.contains("self.caret.min(self.query.len())"),
            "the caret stops at the query, never inside the grey tail"
        );
        assert!(
            src.contains("runs.push(TextRun {"),
            "the tail is its own run, so it can wear the muted ink"
        );
    }

    /// The field is a fat pill with the microphone on its right (#3044).
    ///
    /// The card runs its own recording, so the pins here are the things a
    /// refactor can quietly undo: the glyph, the door the click walks, the tile
    /// that matches the bigger line, and the pillow that makes the field read
    /// as Spotlight instead of a box.
    #[test]
    fn the_field_is_a_fat_pill_with_the_microphone_on_its_right() {
        let src = include_str!("command_palette.rs");
        let field = src
            .find("let mut field = div()")
            .expect("the field row is drawn in overlay()");
        let tail = src.get(field..).expect("the field starts on a glyph");
        let mounted = tail
            .find("shell = shell.child(pill);")
            .expect("the field is mounted on the shell through its rim");
        let pill_src = tail
            .get(..mounted)
            .expect("the field body ends at its mount");
        assert!(
            pill_src.contains(".pl(px(FIELD_PAD_X))"),
            "the invitation keeps the left inset, so it never touches the curve"
        );
        assert!(
            pill_src.contains(".pr(px(FIELD_PAD_RIGHT))"),
            "the right cap is wider than the left one: the microphone needs room in from the edge, and a symmetric inset would either glue it to the curve or push the line off centre (#3066)"
        );
        assert!(
            pill_src.contains(".py(px(FIELD_PAD_Y))"),
            "the height comes from the pillow around the line, not from a bigger font"
        );
        assert!(
            pill_src.contains(".child(field_mic(dictate))"),
            "the microphone closes the field's row, where the composer's does"
        );
        let mic = src
            .find("fn field_mic(")
            .expect("the field's microphone is one function, so no layout can drift");
        let tail = src.get(mic..).expect("field_mic starts on a glyph");
        let end = tail.find("\n}\n").expect("field_mic closes");
        let body = tail.get(..end).expect("the body ends on a glyph");
        assert!(
            body.contains("crate::icon::mic_icon("),
            "the card draws the composer's microphone, never a second glyph"
        );
        assert!(
            body.contains("this.seeker_dictate(cx)"),
            "the click walks the window's door instead of reaching into the composer"
        );
        assert!(
            !body.contains("toggle_dictation"),
            "the card owns no recording state, so it cannot toggle one"
        );
        assert!(
            body.contains(".ml(px(FIELD_MIC_GAP))"),
            "the control brings its own inset, because a flex gap would move the caret"
        );
        assert!(
            body.contains("mic_icon(theme.text_muted, FIELD_MIC_GLYPH)")
                && body.contains(".size(rems(FIELD_MIC_TILE))"),
            "the microphone is a step larger than the composer's, so it matches the bigger line instead of reading as a leftover"
        );
        assert!(
            !body.contains("stop_icon"),
            "the tile is only ever painted idle: the stop belongs to the bubble that replaces the card"
        );
        let line = src
            .find("fn query_line(")
            .expect("the query line is one function, so no caller can drift");
        let tail = src.get(line..).expect("query_line starts on a glyph");
        let end = tail.find("\n}\n").expect("query_line closes");
        let line_body = tail.get(..end).expect("the body ends on a glyph");
        assert!(
            line_body.contains(".text_2xl()"),
            "the line is larger than the composer's, so the field reads as the window's own control"
        );
    }

    /// A live take is one bubble in the middle of the window, and the card is
    /// not drawn beside it (#3044).
    ///
    /// The bubble replaced the counter that rode in the pill and the strip that
    /// hung under it: while the microphone is open the person watches one
    /// thing, so the field and its rows leave the screen and come back with the
    /// words.
    #[test]
    fn the_take_is_one_bubble_and_the_card_steps_aside() {
        let src = include_str!("command_palette.rs");
        let overlay = src
            .find("pub(super) fn overlay(")
            .expect("the overlay is one function");
        let tail = src.get(overlay..).expect("overlay starts on a glyph");
        let end = tail.find("\n}\n").expect("overlay closes");
        let body = tail.get(..end).expect("the body ends on a glyph");
        assert!(
            body.contains(".child(seeker_bubble(take, bubble_view))"),
            "a live take is one bubble, centered, and overlay() returns before the shell is mounted"
        );
        let bubble = body
            .find(".justify_center()")
            .expect("the bubble is mounted on the scrim");
        let shell = body
            .find(".child(shell)")
            .expect("the card branch mounts the shell");
        assert!(
            bubble < shell,
            "the bubble returns first, so the field is never drawn beside it"
        );
        assert!(
            !body.contains("seeker_take") && !body.contains("seeker_voice_strip"),
            "neither the old in-field counter nor the strip under the pill survives"
        );
        let start = src
            .find("fn seeker_bubble(")
            .expect("the bubble is one function, so no caller can drift");
        let tail = src.get(start..).expect("seeker_bubble starts on a glyph");
        let end = tail.find("\n}\n").expect("seeker_bubble closes");
        let drawn = tail.get(..end).expect("the bubble body ends on a glyph");
        assert!(
            !drawn.contains("theme.amber") && !drawn.contains("theme.green"),
            "the phase colour rides the material the worker paints, so the bubble draws nothing over the glass"
        );
        assert!(
            drawn.contains(".when(voice.recording,"),
            "the pointer cursor follows the device, which is the one state the glass answers"
        );
        assert!(
            drawn.contains(".size(rems(TAKE_BUBBLE))")
                && drawn.contains("img(voice.frame.clone())"),
            "the take is the high-resolution glass frame, not a bar"
        );
        assert!(
            drawn.contains("this.seeker_stop_for_edit(cx)"),
            "clicking the glass still ends a take without requiring the chord"
        );
        assert!(
            !drawn.contains("this.seeker_dictate(cx)"),
            "the bubble hands the words back for editing; sending is the chord's job"
        );
        assert!(
            !drawn.contains("voice.secs") && !drawn.contains(".size(px(26.0))"),
            "no timer or separate stop button covers the glass"
        );
    }

    /// The invitation is a short phrase, not the sentence that filled the pill
    /// and read as a value already typed (#3044), and it names both jobs the
    /// field does instead of trailing off (#3066).
    #[test]
    fn the_invitation_is_a_phrase_and_never_a_sentence() {
        assert!(
            !PLACEHOLDER.ends_with("..."),
            "Spotlight's field names its jobs outright; an ellipsis trails off and reads as an unfinished value (#3066)"
        );
        assert!(
            PLACEHOLDER.contains("Search") && PLACEHOLDER.contains("Ask"),
            "the line offers both things enter can do: the search and the ask"
        );
        assert!(
            !PLACEHOLDER.contains("Seeker:"),
            "the card already says what it is, so the line does not repeat it"
        );
        assert!(
            PLACEHOLDER.len() <= 20,
            "a phrase fits the pill; a sentence has to be measured against its width"
        );
    }

    /// An armed pick names its job in the field, and the generic invitation
    /// returns the moment nothing is armed (#2711).
    #[test]
    fn an_armed_pick_names_its_job_in_the_field() {
        use opencrabs::ui_core::palette::{PickFor, pick_prompt};

        let idle = state_with(vec![item("Quit", PaletteKind::Command)]);
        assert_eq!(
            field_prompt(&idle),
            PLACEHOLDER,
            "an idle card keeps the invitation"
        );

        for purpose in [PickFor::NewTopic, PickFor::Favorite, PickFor::GroupTopics] {
            let armed =
                PaletteState::picking(purpose, vec![item("invoices", PaletteKind::Session)]);
            assert_eq!(
                field_prompt(&armed),
                pick_prompt(purpose),
                "{purpose:?} prints its own sentence"
            );
            assert_ne!(
                field_prompt(&armed),
                PLACEHOLDER,
                "{purpose:?} must not fall back to the generic invitation"
            );
        }
    }

    /// The field element takes its invitation from the state, so a second
    /// copy of the placeholder cannot come back inside prepaint (#2711).
    #[test]
    fn the_field_reads_its_prompt_from_the_state() {
        let src = include_str!("command_palette.rs");
        let prepaint = src
            .find("fn prepaint(")
            .expect("the element has a prepaint");
        let body = src.get(prepaint..).expect("prepaint starts on a glyph");
        let end = body.find("fn paint(").expect("prepaint ends before paint");
        let body = body.get(..end).expect("the cut lands on a glyph");
        assert!(
            !body.contains("PLACEHOLDER"),
            "prepaint prints the state's prompt, not the const"
        );
        assert!(
            body.contains("self.placeholder"),
            "prepaint paints the field the caller passed"
        );
        assert!(
            src.contains("placeholder: field_prompt(state)"),
            "query_line hands the state's prompt to the element"
        );
    }

    /// The results card uses its dedicated 32px radius, and its rows nest
    /// on that curve instead of carrying a hardcoded radius (#2532). The card
    /// also floats: it is mounted inside a wrapper that carries the shadow and
    /// the lit edge, because a fill cannot paint a gradient border (#3066).
    #[test]
    fn seeker_card_is_rounder_and_its_rows_nest() {
        let src = include_str!("command_palette.rs");
        assert!(
            src.contains(".rounded(px(SEEKER_RADIUS))"),
            "the results card is drawn on the dedicated Seeker radius"
        );
        assert!(
            src.contains(".rounded(px(seeker_concentric_radius("),
            "rows nest on the Seeker curve instead of a hardcoded number"
        );
        assert!(
            src.contains(".child(rim_under(SEEKER_RADIUS))"),
            "the card's rim rides a wrapper one pixel larger than the fill, so the two radii can never disagree"
        );
        assert!(
            src.contains(".gap(px(SEEKER_GAP))"),
            "the field and the card are spaced as two objects, not stacked flush"
        );
        assert!(
            src.contains(".px(px(SEEKER_GUTTER))") && src.contains(".px(px(SEEKER_ROW_PAD_X))"),
            "the gutters add up: the list's inset plus the row's own, so a row's text lines up with the field's"
        );
        assert!(
            src.contains(".when(!armed, |d| d.hover(|d| d.bg(rgba(0xffffff0d))))"),
            "a row under the pointer lifts before it is armed, so the list answers the mouse"
        );
    }

    /// Both surfaces float on a lit rim, not on an outline (#3066).
    ///
    /// `border_color` is one colour for all four sides in this gpui build, so a
    /// true top-lit edge has to be a second quad painted underneath the fill.
    /// The pin matters because the old 1px outline still compiles and still
    /// looks plausible: it just stops reading as glass.
    #[test]
    fn both_surfaces_float_on_a_lit_rim_instead_of_an_outline() {
        let src = include_str!("command_palette.rs");
        assert!(
            src.contains(".child(rim_under(PILL_RADIUS))"),
            "the field's rim rides its wrapper"
        );
        assert!(
            src.matches("rim_under(").count() >= 3,
            "one definition, two surfaces, so neither can drift to its own edge treatment"
        );
        assert!(
            src.matches(concat!(".shadow_2xl", "()")).count() == 2,
            "both surfaces float on the same shadow, because they are one object"
        );
        // Assembled from pieces so the literal this test hunts for is not the
        // literal this test contains: a pin that matches itself always fails,
        // or worse, always passes.
        let retired = concat!("0xffffff", "2e");
        assert!(
            !src.contains(retired),
            "the retired 1px outline colour is gone, not merely unreferenced"
        );
        let rim = src.find("fn rim_under(").expect("the rim is one function");
        let tail = src.get(rim..).expect("rim_under starts on a glyph");
        let end = tail.find("\n}\n").expect("rim_under closes");
        let body = tail.get(..end).expect("the body ends on a glyph");
        assert!(
            body.contains("RIM_TOP") && body.contains("RIM_BOTTOM"),
            "the edge is a gradient, brightest along the top, the way light lands on a raised surface"
        );
        assert!(
            body.contains("RIM_WIDTH"),
            "the rim is one hairline: any thicker and it reads as a frame"
        );
    }

    /// The calculator's answer is a result, so it floats on its own frosted
    /// card with a copy disc, while every other armed row takes the plain
    /// grey lift that marks a choice (#3066).
    #[test]
    fn the_calc_answer_floats_on_its_own_frosted_card() {
        let src = include_str!("command_palette.rs");
        assert!(
            src.contains(".when(armed && rowv.calc, |d| {"),
            "the frosted card is guarded by the row's own kind, so a choice never wears it"
        );
        assert!(
            src.contains(".when(armed && !rowv.calc, |d| d.bg(armed_bg))"),
            "the grey lift is the complement, so exactly one of the two paints"
        );
        assert!(
            src.contains(".bg(rgba(FROST_FILL))") && src.contains(".border_color(rgba(RIM_EDGE))"),
            "the card is a whisper of white with its own lit edge"
        );
        assert!(
            src.contains(".rounded(px(CALC_CARD_RADIUS))"),
            "an inner surface nests on a tighter curve than the card holding it"
        );
        assert!(
            src.contains(".size(px(CALC_COPY_DISC))") && src.contains(".rounded_full()"),
            "the copy mark sits in a disc, not a label: it is a control the cursor lands on"
        );
        assert!(
            src.contains("crate::icon::copy_icon("),
            "the row prints the same glyph the terminal does for the same act"
        );
        assert!(
            src.contains("calc: row.item.kind == PaletteKind::Calc"),
            "the view learns its kind once, in rows(), so no layout decides it twice"
        );
        let printed = rows(&state_with(vec![
            item("72", PaletteKind::Calc),
            item("Quit", PaletteKind::Command),
        ]));
        // An idle field ranks by the menu's own order, so the calculator is not
        // first: read the rows by their title instead of by position.
        let calc = printed
            .iter()
            .find(|row| row.title == "72")
            .expect("the calculator row survives the ordering");
        let command = printed
            .iter()
            .find(|row| row.title == "Quit")
            .expect("the command row survives the ordering");
        assert!(calc.calc, "a calculator row says it is one");
        assert!(!command.calc, "a command row does not");
    }

    /// The blink has an idle cutoff. A caret that never stops reads as a
    /// stuck cell, not a field (#2531).
    #[test]
    fn seeker_caret_stops_blinking_once_typing_stops() {
        // Two of the three pins moved to the finder in the split (#2822): the
        // blink check and the reset that restarts it. The idle constant left
        // the window for `window_input.rs` (#3060). Read the whole dashboard
        // so a pin cannot pass while the code it names lives one module over.
        let src = crate::dashboard_source::DASHBOARD;
        assert!(
            src.contains("const CARET_IDLE_AFTER"),
            "the idle window is a named constant"
        );
        assert!(
            src.contains("caret_blinks("),
            "the blink loop asks the shared rule whether the field is still live"
        );
        assert!(
            src.contains("fn finder_activity_reset"),
            "a key restarts the window"
        );
    }

    /// The top row is armed before any arrow press, because Enter acts on it:
    /// a card whose marker only appears after a key press reads as broken.
    #[test]
    fn the_top_row_is_armed_until_an_arrow_moves_it() {
        let mut state = state_with(vec![
            item("alpha", PaletteKind::Command),
            item("alpine", PaletteKind::Command),
        ]);
        let printed = rows(&state);
        assert_eq!(printed.len(), 2);
        assert_eq!(printed[0].marker, MARKER);
        assert!(printed[0].armed);
        assert_eq!(printed[1].marker, " ");
        assert!(!printed[1].armed);

        // The engine's rule: the first press arms the top row (so one arrow
        // never skips the best match), and the next press steps down from it.
        state.move_by(1, 10);
        assert_eq!(state.cursor(), Some(0));
        state.move_by(1, 10);
        let printed = rows(&state);
        assert!(!printed[0].armed);
        assert!(printed[1].armed);
        assert_eq!(printed[1].marker, MARKER);
    }

    /// The kind column is the engine's own word, so the two frontends cannot
    /// label the same row differently.
    #[test]
    fn the_kind_column_uses_the_engines_label() {
        let state = state_with(vec![item("a", PaletteKind::Session)]);
        let printed = rows(&state);
        assert_eq!(printed[0].kind, kind_label(PaletteKind::Session));
        assert_eq!(printed[0].kind, "session");
    }

    /// A long selected skill row yields only its flexible text. The source
    /// assertions pin GPUI's layout contract; the owner hand test checks paint.
    #[test]
    fn long_skill_rows_keep_fixed_labels_and_shrink_only_text() {
        let long_subtitle = "a useful skill description that keeps going ".repeat(16);
        let mut skill = item("skill with an intentionally long title", PaletteKind::Skill);
        skill.subtitle = long_subtitle.clone();
        let mut state = state_with(vec![skill]);
        state.insert_text("skill with");
        let printed = rows(&state);
        assert_eq!(printed.len(), 1);
        assert!(printed[0].armed);
        assert_eq!(printed[0].kind, "skill");
        assert_eq!(printed[0].subtitle, long_subtitle);
        assert!(printed[0].subtitle.len() > 600);

        let src = include_str!("command_palette.rs");
        let start = src
            .find("pub(super) fn overlay(")
            .expect("overlay renderer");
        let overlay = src.get(start..).expect("overlay start is a char boundary");
        let end = overlay.find("/// The kind label").expect("overlay ends");
        let compact: String = overlay
            .get(..end)
            .expect("overlay end is a char boundary")
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        for required in [
            ".flex_col().min_w_0().gap_0p5()",
            ".flex_row().min_w_0().items_center()",
            ".flex_1().min_w_0().flex().flex_col()",
            ".flex_shrink_0().text_xs().text_color(rgb(INK_MUTED)).mr_2().child(SharedString::from(rowv.kind))",
            ".flex_shrink_0().text_xs().text_color(rgb(INK_MUTED)).child(\"terminalonly\")",
            ".flex_shrink_0().text_xs().text_color(rgb(INK_MUTED)).child(\"↵enter\")",
            ".flex().flex_col().min_w_0().overflow_hidden().child(list)",
        ] {
            assert!(
                compact.contains(required),
                "selected-row layout lost {required}"
            );
        }
    }

    /// A miss offers the ask row and names it in the status line, never an
    /// empty panel with a silent footer (#2482).
    #[test]
    fn a_miss_offers_the_ask_row_and_the_status_names_it() {
        let mut state = state_with(vec![item("alpha", PaletteKind::Command)]);
        for c in "zzz".chars() {
            state.type_char(c);
        }
        let printed = rows(&state);
        assert_eq!(printed.len(), 1, "a miss must offer exactly the ask row");
        assert_eq!(printed[0].kind, kind_label(PaletteKind::Ask));
        assert_eq!(status(&state), "no match. enter asks a fresh session");
    }

    /// A single hit says "1 match", not "1 matches".
    #[test]
    fn the_count_reads_like_english() {
        let mut state = state_with(vec![
            item("alpha", PaletteKind::Command),
            item("beta", PaletteKind::Command),
            item("alphabet", PaletteKind::Command),
        ]);
        assert_eq!(status(&state), "", "an empty query prints no count");
        state.insert_text("beta");
        assert_eq!(status(&state), "1 match");
        state.clear();
        state.insert_text("alph");
        assert_eq!(status(&state), "2 matches");
    }

    /// Every command the terminal runs prints its row here, and since the
    /// window answers all of them none is marked "terminal only" any more
    /// (#2469, #2470). The mark itself is kept: the day a command lands in
    /// the shared table before its window wiring does, this same loop marks
    /// it rather than the row vanishing.
    #[test]
    fn every_command_row_is_runnable_on_the_window() {
        use opencrabs::ui_core::commands::{Support, gui_commands, support_gui};

        let items: Vec<PaletteItem> = gui_commands()
            .map(|cmd| item(cmd.name, PaletteKind::Command))
            .collect();

        // The idle menu is curated since #2491: it leads with surfaces and
        // drops /brain and /compact, whose screens already carry them. The
        // contract is reachability, not the dump: typing a command's own
        // name finds its row, and no found command is terminal only.
        for cmd in gui_commands() {
            let mut probe = state_with(items.clone());
            for ch in cmd.name.chars() {
                probe.type_char(ch);
            }
            let printed = rows(&probe);
            let row = printed
                .iter()
                .find(|row| row.title == cmd.name)
                .unwrap_or_else(|| panic!("{} not found by its own name", cmd.name));
            assert!(
                !row.missing,
                "{} is marked terminal only but the window runs it",
                row.title
            );
        }

        // The gap that would mark a row is gone, so the badge's own trigger is
        // asserted directly: a name the shared table does not know is not a
        // gap, it is a typo.
        let gap = gui_commands()
            .filter(|cmd| support_gui(cmd.name) == Support::Missing)
            .count();
        assert_eq!(gap, 0, "the window answers every command in the table");

        // Only command rows can carry the mark; a session or model row that
        // happens to be named like one never does.
        let sessions = rows(&state_with(vec![item("/usage", PaletteKind::Session)]));
        assert!(!sessions[0].missing, "a session row is never terminal only");
    }

    /// A sum is not a search result: the footer says what Enter does instead of
    /// counting a match, and the row it describes carries the answer.
    #[test]
    fn the_footer_offers_to_copy_a_calculator_answer() {
        let mut state = state_with(vec![item("Quit", PaletteKind::Command)]);
        state.insert_text("200/7.1");
        assert_eq!(status(&state), "enter copies the result");
        let printed = rows(&state);
        assert_eq!(printed[0].title, "= 28.1690140845");
        assert_eq!(printed[0].kind, "calc");
    }

    /// Before the handshake, and on a local crab, the gated rows are the
    /// plain rows: nothing is greyed out and nothing moves.
    #[test]
    fn no_command_row_is_greyed_out_before_the_handshake_or_locally() {
        use opencrabs::backend::Scope;
        use opencrabs::ui_core::commands::gui_commands;

        let items: Vec<PaletteItem> = gui_commands()
            .map(|cmd| item(cmd.name, PaletteKind::Command))
            .collect();
        let state = state_with(items);
        let local = HelloReply::for_caller(Scope::ALL, true);
        assert_eq!(rows_gated(&state, None), rows(&state));
        assert_eq!(rows_gated(&state, Some(&local)), rows(&state));
        assert!(rows(&state).iter().all(|row| row.unavailable.is_none()));
    }

    /// A read-only token greys out `/new` with the scope it lacks, and leaves
    /// a session row that happens to share the name alone.
    #[test]
    fn a_read_only_token_greys_out_new_with_its_reason() {
        use opencrabs::backend::Scope;

        let hello = HelloReply::for_caller(&[Scope::Read], false);
        let mut probe = state_with(vec![item("/new", PaletteKind::Command)]);
        for ch in "/new".chars() {
            probe.type_char(ch);
        }
        let printed = rows_gated(&probe, Some(&hello));
        let command = printed
            .iter()
            .find(|row| row.title == "/new")
            .expect("the command row");
        let why = command.unavailable.as_deref().expect("needs manage");
        assert!(why.contains("a2a:manage"), "{why}");

        // Only command rows are gated: a session row named like one never is.
        let sessions = rows_gated(
            &state_with(vec![item("/new", PaletteKind::Session)]),
            Some(&hello),
        );
        assert_eq!(sessions[0].unavailable, None);
    }
}
