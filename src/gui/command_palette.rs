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
    AnyElement, App, Bounds, ClickEvent, CursorStyle, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, LayoutId, MouseButton, Pixels, Point, ScrollWheelEvent, SharedString,
    Style, TextAlign, TextRun, WeakEntity, Window, WrappedLine, div, fill, linear_color_stop,
    linear_gradient, point, prelude::*, px, relative, rems, rgb, rgba, size,
};
use std::ops::Range;
use std::rc::Rc;

use crate::{Dashboard, palette};
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
            title: row.item.title,
            subtitle: row.item.subtitle,
            kind: kind_label(row.item.kind),
            armed: ix + start == armed,
        })
        .collect()
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
            } else if rows.len() == 1 && rows[0].item.kind == PaletteKind::Ask {
                "no match. enter asks a fresh session".to_string()
            } else {
                let matches = rows
                    .iter()
                    .filter(|row| row.item.kind != PaletteKind::Ask)
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
const INK_MUTED: u32 = 0x9ba0aa;
const INK_WARM: u32 = 0xe8b96a;

/// The inactive kind chip's translucent white fill. Filled capsules read as
/// one Spotlight control; the old transparent, bordered chips read as labels.
const CHIP_FILL: u32 = 0xffffff14;

/// The invitation the field prints while it is empty.
///
/// Short on purpose (#3044): the field is a thick pill, and a sentence long
/// enough to fill it reads as a value typed into the box instead of an
/// invitation to type one. Three words and a trailing ellipsis, the way
/// Spotlight invites a query, so the line never competes with what a person
/// types and never has to be measured against the pill's width.
const PLACEHOLDER: &str = "Search anything...";

/// The field's vertical padding (#3044): the pill is a target a hand aims at,
/// so its height comes from the pillow around the line and not from a bigger
/// font, which would make the query look like a heading.
const FIELD_PAD_Y: f32 = 22.0;

/// The field's horizontal padding, the same on both caps so neither the
/// invitation nor the microphone reads as glued to the curve (#3044).
const FIELD_PAD_X: f32 = 22.0;

/// The space the microphone puts between itself and the line it follows
/// (#3044). It is the control's own inset rather than a gap on the pill: a
/// flex gap beside the shaped line would move the caret, which has to sit
/// exactly where the text does (#2531).
const FIELD_MIC_GAP: f32 = 14.0;

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
        let display = if empty { self.placeholder } else { &self.query };
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color: rgb(if empty { INK_MUTED } else { INK }).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_text(
                SharedString::from(display.to_string()),
                font_size,
                &[run],
                None,
                None,
            )
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
        // selection is live (#2531, #2534).
        let caret = if self.caret_on && self.selection.is_none() {
            line.position_for_index(self.caret, line_height)
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

/// The Seeker's take, as the card paints it (#3044).
///
/// Derived from the window's handles on every frame rather than stored: the
/// card must never show a counter for a microphone that already closed.
pub(crate) struct SeekerVoice {
    /// The device is open and samples are arriving.
    pub recording: bool,
    /// The recording was asked to stop and its WAV is being built.
    pub finishing: bool,
    /// Seconds the take has run, for the counter.
    pub secs: u64,
    /// The meter, newest last, for the wave.
    pub levels: Vec<f32>,
}

/// The dictation control on the Seeker field (#3044).
///
/// Same glyph, colour and click as the composer's microphone, so the two are
/// one control on two surfaces: a person who learned it in the field finds it
/// in the card, and the hover says the same thing on both.
///
/// It records here, in the card. The counter and the meter ride inside the
/// pill, to the left of this control, and the words land in the field itself: a
/// search query has to end up in the box the person is looking at, which is
/// why handing the job to the composer behind the card was the wrong shape.
///
/// The tile is one step larger than the composer's (2.1rem against 1.8rem,
/// with a 1.5rem glyph against 1.25rem) because the field around it is: the
/// line here is `text_2xl`, and a control sized for the composer's line would
/// read as a leftover from the surface it came from (#3044).
fn field_mic(view: WeakEntity<Dashboard>, voice: Option<&SeekerVoice>) -> impl IntoElement {
    let theme = palette::current();
    let recording = voice.is_some_and(|v| v.recording);
    let working = voice.is_some_and(|v| v.finishing && !v.recording);
    // The four states wear the four colours the composer's microphone wears:
    // quiet grey, recording red, working amber. A control that changed the
    // meaning of a click without changing its face would be a trap.
    let colour = if recording {
        theme.red
    } else if working {
        theme.amber
    } else {
        theme.text_muted
    };
    let mark: AnyElement = if recording {
        crate::icon::stop_icon(colour, 1.15).into_any_element()
    } else {
        crate::icon::mic_icon(colour, 1.5).into_any_element()
    };
    // While the take runs the tile wears a red pad whose strength follows the
    // newest meter reading, so the control itself breathes with the voice
    // instead of only changing colour (#3044). It costs no clock: the meter
    // already repaints the card once per reading.
    let heard = voice
        .and_then(|v| v.levels.last().copied())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let pad = if recording {
        rgba((theme.red << 8) | (0x1c + (heard * 0x54 as f32) as u32))
    } else {
        rgba(0x00000000)
    };
    div()
        .id("seeker-mic")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .ml(px(FIELD_MIC_GAP))
        .size(rems(2.1))
        .rounded_sm()
        .bg(pad)
        .cursor_pointer()
        .opacity(if recording { 1.0 } else { 0.8 })
        .hover(|d| d.opacity(1.0))
        // The hover names the key, exactly as the composer's microphone does:
        // two surfaces, one binding, one sentence about it.
        .tooltip(|_window, cx| crate::hint::mic_tooltip(crate::hint::mic_line(), cx))
        .child(mark)
        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
            // The click belongs to the control, not to the scrim under it.
            cx.stop_propagation();
            let _ = view.update(cx, |this, cx| this.seeker_dictate(cx));
        })
}

/// The running take, drawn inside the field's own row (#3044): the counter and
/// the live meter, to the left of the microphone.
///
/// It rides in the pill rather than under it. Below the field it read as a
/// second card hanging off the first, and the person watching the counter is
/// looking at the line they are about to search with. The word that the same
/// key stops it is not lost: it lives on the microphone's hover, next to the
/// chord, and the tile itself turns into the stop while the take runs.
fn seeker_take(voice: &SeekerVoice) -> impl IntoElement {
    let theme = palette::current();
    let colour = if voice.recording {
        theme.red
    } else {
        theme.amber
    };
    div()
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(colour))
                .child(format!("{}:{:02}", voice.secs / 60, voice.secs % 60)),
        )
        .child(seeker_wave(&voice.levels, colour))
}

/// How many meter readings the card's wave draws. The composer's own count, so
/// the two strips read as the same instrument.
const SEEKER_WAVE_BARS: usize = 34;

/// The live meter in the card (#3044): one bar per reading, newest on the
/// right, so the strip scrolls the way a waveform should and its right edge is
/// what the microphone heard a moment ago.
fn seeker_wave(levels: &[f32], colour: u32) -> impl IntoElement {
    let shown = &levels[levels.len().saturating_sub(SEEKER_WAVE_BARS)..];
    let mut row = div()
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(px(1.5))
        .h(px(14.0));
    for level in shown {
        row = row.child(
            div()
                .flex_none()
                .w(px(2.0))
                .h(px(2.0 + level.clamp(0.0, 1.0) * 12.0))
                .rounded(px(1.0))
                .bg(rgb(colour)),
        );
    }
    row
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
    } = frame;
    let theme = palette::current();
    let printed = rows(state);
    let idle = state.query().trim().is_empty();
    let pick_armed = state.pick().is_some();
    // The reveal gate: picker rows and typed results print at once, the
    // idle menu holds for IDLE_REVEAL_DELAY so the first paint is the field
    // and the invitation alone (#2491).
    let reveal = pick_armed || !idle || idle_revealed;

    // The glass is always dark, Spotlight-style, light theme included: the
    // theme keeps its say through the accent on the armed row and the
    // active chip (#2491).
    let glass = if idle && idle_revealed && !pick_armed {
        // The idle menu sits under the solid field: more transparency at
        // the bottom than the top, so the list reads as a hint, not a wall.
        linear_gradient(
            180.0,
            linear_color_stop(rgba(0x14141cf2), 0.0),
            linear_color_stop(rgba(0x14141cbf), 1.0),
        )
    } else {
        linear_gradient(
            180.0,
            linear_color_stop(rgba(0x14141cfa), 0.0),
            linear_color_stop(rgba(0x14141cf2), 1.0),
        )
    };
    let armed_bg = rgba((theme.accent << 8) | 0x2e);
    let chip = Rc::new(chip);

    // Inert: a click on the shell stops here instead of reaching the
    // scrim, so reading the list never closes it.
    let mut shell = div()
        .id("command-palette")
        .flex()
        .flex_col()
        .gap_2()
        .w(rems(34.0))
        .on_click(|_ev, _window, cx| cx.stop_propagation());

    // The field: Spotlight's floating pill, fatter than his (#3044). The
    // invitation is a short phrase now, so the height is a pillow around the
    // line instead of a box sized for a sentence, and the microphone closes the
    // row the way it closes the composer's: one control, two surfaces. The pill
    // carries no flex gap, because the caret rides the shaped line and a gap
    // beside it would move the one thing that has to sit exactly where the text
    // does (#2531); the microphone brings its own inset instead.
    let dictate = view.clone();
    let mut pill = div()
        .flex()
        .flex_row()
        .items_center()
        .pl(px(FIELD_PAD_X))
        .pr(px(FIELD_PAD_X))
        .py(px(FIELD_PAD_Y))
        .rounded_full()
        .bg(rgba(0x1b1b25fa))
        .border_1()
        .border_color(rgba(0xffffff2e))
        .shadow_md()
        .child(query_line(state, view, paint, caret_on));
    // The take rides in the field's own row (#3044): counter and meter between
    // the query and the microphone, so a running recording can never hang a
    // second card off the bottom of the field.
    if let Some(take) = voice.as_ref() {
        pill = pill.child(seeker_take(take));
    }
    pill = pill.child(field_mic(dictate, voice.as_ref()));
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
        .border_1()
        .border_color(rgba(0xffffff2e))
        .bg(glass)
        .shadow_md()
        .overflow_hidden();

    if reveal {
        // The kind chips. Hidden while a session pick is armed: a narrowed
        // conversation list has no kinds to filter (#2491).
        if !pick_armed {
            let wanted = state.kind_filter();
            let mut row = div().flex().flex_row().gap_1().px_3().pt_4().pb_2();
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
                            d.bg(rgba((theme.accent << 8) | 0x2e))
                                .text_color(rgb(theme.accent))
                        })
                        .when(!active, |d| {
                            d.bg(rgba(CHIP_FILL)).text_color(rgb(INK_MUTED))
                        })
                        .hover(|d| {
                            if active {
                                d.bg(rgba((theme.accent << 8) | 0x42))
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
            .px_3()
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
                .px_2()
                .py_1p5()
                .rounded(px(seeker_concentric_radius(12.0)))
                .cursor_pointer()
                .when(armed, |d| d.bg(armed_bg));
            if armed && !rowv.missing {
                cell = cell.child(
                    div()
                        .w(px(10.0))
                        .text_xs()
                        .text_color(rgb(theme.accent))
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
                            .text_color(rgb(if rowv.missing { INK_MUTED } else { INK }))
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
            cell = cell.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(rgb(INK_MUTED))
                    .mr_2()
                    .child(SharedString::from(rowv.kind)),
            );
            if rowv.missing {
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
                    .px_3()
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
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(INK_WARM))
                        .child(SharedString::from(text)),
                )
                .child(
                    div()
                        .px_3()
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
                    .px_3()
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
                    .px_3()
                    .pb_3()
                    .text_xs()
                    .text_color(rgb(INK_MUTED))
                    .child("enter run · up/down move · esc close"),
            );
        }

        shell = shell.child(card);
    }

    div()
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
        // Spotlight floats the card high, not centered: the eye reads it as
        // a command surface, not a dialog (#2491).
        .justify_start()
        .pt(rems(6.5))
        .bg(rgba(0x08080acc))
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
            src.contains("line.position_for_index(self.caret, line_height)"),
            "the caret is painted at the shaped insertion point"
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
        let pill = src
            .find("let mut pill = div()")
            .expect("the pill is drawn in overlay()");
        let pill_src = src.get(pill..).expect("the pill starts on a glyph");
        let pill_end = pill_src
            .find(".child(query_line(")
            .expect("the pill draws the line");
        let pill_src = pill_src
            .get(..pill_end)
            .expect("the query line starts on a glyph");
        assert!(
            !pill_src.contains("gap_3"),
            "the pill row itself carries no gap either"
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
        let pill = src
            .find("let mut pill = div()")
            .expect("the pill is drawn in overlay()");
        let tail = src.get(pill..).expect("the pill starts on a glyph");
        let mounted = tail
            .find("shell = shell.child(pill);")
            .expect("the pill is mounted on the shell");
        let pill_src = tail
            .get(..mounted)
            .expect("the pill body ends at its mount");
        assert!(
            pill_src.contains(".pl(px(FIELD_PAD_X))") && pill_src.contains(".pr(px(FIELD_PAD_X))"),
            "both caps keep the same inset, so neither the invitation nor the microphone is glued to the curve"
        );
        assert!(
            pill_src.contains(".py(px(FIELD_PAD_Y))"),
            "the height comes from the pillow around the line, not from a bigger font"
        );
        assert!(
            pill_src.contains(".child(field_mic(dictate, voice.as_ref()))"),
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
            body.contains("mic_icon(colour, 1.5)") && body.contains(".size(rems(2.1))"),
            "the microphone is a step larger than the composer's, so it matches the bigger line instead of reading as a leftover"
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

    /// A running take is drawn inside the field, never as a strip hanging off
    /// it (#3044).
    ///
    /// The counter and the meter are what the person watches while the
    /// microphone is open, and a second row under the pill reads as a card
    /// below the card. They ride in the pill's own row, between the query and
    /// the microphone, so the field stays one control in every state.
    #[test]
    fn the_running_take_rides_inside_the_field() {
        let src = include_str!("command_palette.rs");
        let pill = src
            .find("let mut pill = div()")
            .expect("the pill is drawn in overlay()");
        let tail = src.get(pill..).expect("the pill starts on a glyph");
        let mounted = tail
            .find("shell = shell.child(pill);")
            .expect("the pill is mounted on the shell");
        let pill_src = tail
            .get(..mounted)
            .expect("the pill body ends at its mount");
        assert!(
            pill_src.contains("pill = pill.child(seeker_take(take));"),
            "the counter and the meter are pill children, not shell children"
        );
        let take = pill_src
            .find("seeker_take(take)")
            .expect("the take is mounted on the pill");
        let mic = pill_src
            .find("field_mic(dictate")
            .expect("the microphone closes the pill");
        assert!(
            take < mic,
            "the take sits between the query and the microphone, so the stop stays last"
        );
        let overlay = src
            .find("pub(super) fn overlay(")
            .expect("the overlay is one function");
        let tail = src.get(overlay..).expect("overlay starts on a glyph");
        let end = tail.find("\n}\n").expect("overlay closes");
        let body = tail.get(..end).expect("the body ends on a glyph");
        assert!(
            !body.contains("seeker_voice_strip"),
            "nothing paints a second row under the pill"
        );
    }

    /// The invitation is a short phrase, not the sentence that filled the pill
    /// and read as a value already typed (#3044).
    #[test]
    fn the_invitation_is_a_phrase_and_never_a_sentence() {
        assert!(
            PLACEHOLDER.ends_with("..."),
            "the invitation trails off the way a search field's does"
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
    /// on that curve instead of carrying a hardcoded radius (#2532).
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
    }

    /// The blink has an idle cutoff. A caret that never stops reads as a
    /// stuck cell, not a field (#2531).
    #[test]
    fn seeker_caret_stops_blinking_once_typing_stops() {
        // Two of the three pins moved to the finder in the split (#2822): the
        // blink check and the reset that restarts it. The idle constant stayed
        // in the window. Read the whole dashboard so a pin cannot pass while
        // the code it names lives one module over.
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
}
