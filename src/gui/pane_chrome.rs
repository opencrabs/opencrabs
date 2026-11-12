//! A pane's chrome and the split's geometry: the header and the × that closes
//! it, the dimmed composer of a pane that does not hold the keyboard, the
//! footer row, and the rules, bands and widths the split draws with.
//!
//! Every function here takes plain data. The pane walk in the crate root owns
//! the state and the listeners; this module only says what a pane looks like,
//! which is why the measure and the divider math can be tested without a
//! window.
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions, their constants, their comments and the footer test module are
//! unchanged, only the module boundary is new. `SendOrigin`, which sat between
//! the footer and the dim composer, stays where it was: it gates sends, it does
//! not draw a pane.

use gpui::{
    AnyElement, App, ClickEvent, CursorStyle, IntoElement, MouseButton, MouseDownEvent,
    SharedString, Window, div, prelude::*, px, relative, rgb, rgba,
};
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::pane_tree::DropHalf;
use opencrabs::ui_core::split;
use uuid::Uuid;

use crate::{Dashboard, PaneDrag, PaneGhost, palette};

/// Whether the window-level footer row stands down (#2641). A split hands
/// that row to each pane, exactly as the terminal's status bar yields its
/// height in split view (#2575), so two footers never stack and the
/// window-wide pair can never contradict the pair the pane above is running.
pub(crate) fn window_footer_yields(pane_count: usize) -> bool {
    pane_count > 1
}

/// A pane's status row at its bottom edge (#2593): the fields the window
/// footer carries window-wide, scoped to the pane's own session. Unfocused
/// panes draw it muted and the focused one in the accent, so feedback stays
/// visible on every panel while the pane holding the keyboard stays the only
/// one at full strength.
pub(crate) fn pane_footer(
    id: SharedString,
    text: Option<SharedString>,
    focused: bool,
) -> AnyElement {
    let Some(text) = text else {
        return div().into_any_element();
    };
    let color = if focused {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        // One line, and only one line: a long model name, roster or plan
        // label clipped the text at the window's edge and painted the rest
        // over the neighbouring pane. The row now clips and the text
        // ellipsizes inside it (#2620).
        .min_w_0()
        .max_w_full()
        .overflow_hidden()
        .gap_2()
        .px_2()
        .py_1()
        .text_xs()
        .whitespace_nowrap()
        .text_color(rgb(color))
        .border_t_1()
        .border_color(rgb(split_rule_color()))
        .child(div().min_w_0().overflow_hidden().truncate().child(text))
        .into_any_element()
}

/// A pane's composer while another pane holds the keyboard (#2530): the draft
/// it keeps, dimmed and inert. Clicking it hands the keyboard to that pane, so
/// the next frame draws the real field in the same spot and the typed text
/// never lands in a pane the user is not looking at.
pub(crate) fn pane_composer_dim(
    draft: String,
    session: Uuid,
    is_chat: bool,
    entity: gpui::WeakEntity<Dashboard>,
) -> AnyElement {
    let empty = draft.trim().is_empty();
    let shown: SharedString = if empty {
        SharedString::from("Talk to the crab\u{2026}")
    } else {
        SharedString::from(draft.replace('\n', " "))
    };
    let color = if empty {
        palette::current().text_muted
    } else {
        palette::current().text
    };
    div()
        .id(SharedString::from(format!("pane-composer-{session}")))
        .flex()
        .flex_none()
        // A pane that does not own the keyboard still shows the draft of the
        // session it holds, and a long draft used to widen the whole pane
        // instead of staying inside it: `flex_none` with no width floor lets
        // the row refuse to shrink below its text, and the column grew with
        // it (#2688). The dim row is one line by definition, so it takes the
        // pane's width, clamps to it and ellipsises the rest.
        .w_full()
        .min_w_0()
        .truncate()
        .min_h(px(44.))
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .opacity(0.55)
        .cursor(CursorStyle::IBeam)
        .text_size(px(14.))
        .text_color(rgb(color))
        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
            let _ = entity.update(cx, |this, cx| {
                this.pane_focus = if is_chat { None } else { Some(session) };
                // The next launch puts the caret back on this pane (#2675).
                this.persist_layout();
                this.refocus_screen();
                cx.notify();
            });
        })
        .child(shown)
        .into_any_element()
}

/// One pane's header: the session's name and the × that closes that pane
/// (#2474). Every pane carries one, so a split reads as two sessions rather
/// than a session beside a nameless strip, and the pane holding the caret
/// wears the accent so Tab's move is visible.
pub(crate) fn pane_header(
    id: SharedString,
    title: SharedString,
    focused: bool,
    draggable: bool,
    session: Uuid,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let title_color = if focused {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    // The focused pane wears the accent as a full-width band under its header,
    // so which pane Tab moved to is legible at a glance and not only from the
    // title colour (#2475). A one-pixel rule in the panel colour read as
    // nothing on both themes, which is why the band is two pixels and the
    // unfocused header sits on the same contrast as the divider.
    let rule_color = focus_ring_color(focused);
    let header_bg = if focused {
        palette::current().panel_hi
    } else {
        palette::current().bg
    };
    // The header is the pane's grip (#2683): pressing it and moving picks the
    // whole pane up, and the drop target under the pointer decides where it
    // lands. The ghost carries the pane's title, so what follows the pointer is
    // the pane the user grabbed and not a blank chip.
    let drag_title = title.clone();
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .bg(rgb(header_bg))
        .border_b_2()
        .border_color(rgb(rule_color))
        // The grip needs a stateful element: `on_drag` is only offered by
        // `StatefulInteractiveElement`, and a bare `Div` has no id to key the
        // drag on, so the pane's own key rides here (#2683).
        .id(SharedString::from(format!("pane-grip-{id}")))
        .when(draggable, |d| {
            d.cursor(CursorStyle::OpenHand).on_drag(
                PaneDrag { session },
                move |_drag, _offset, _window, cx| {
                    let title = drag_title.clone();
                    cx.new(|_| PaneGhost { title })
                },
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(title_color))
                .child(title),
        )
        .child(
            div()
                .id(id)
                .px_2()
                .cursor_pointer()
                .text_color(rgb(palette::current().text_muted))
                .hover(|d| d.text_color(rgb(palette::current().accent)))
                .on_click(close)
                .child(SharedString::from("×")),
        )
}

/// The gap either side of the rule between two panes, in px: `gap_2()`.
const SPLIT_GAP_PX: f32 = 8.0;

/// The rule that separates two panes, in px (#2473). Two pixels rather than
/// one, and drawn in the faint text colour rather than a panel tone: at one
/// pixel of `panel_hi` the boundary disappeared into both themes, so a split
/// still read as one transcript with a gap in it.
const SPLIT_DIVIDER_PX: f32 = 2.0;

/// How wide the strip that takes the drag is, in px (#2476). The rule inside
/// it stays [`SPLIT_DIVIDER_PX`]; the strip either side of it is transparent
/// and takes the pointer, because a two pixel target is a pixel hunt.
const SPLIT_HANDLE_PX: f32 = 12.0;

/// How far a pane is drawn back while another one owns the keyboard (#2612).
///
/// It has to read as "disabled" at a glance without hiding the transcript: at
/// 0.62 the body text still parses, and the accent ring on the live pane is
/// unmistakable beside it.
pub(crate) const UNFOCUSED_PANE_OPACITY: f32 = 0.62;

/// The chord that opens a split (#2438, #2471): `ctrl-shift-pipe` puts the
/// panes side by side and `ctrl-shift-underscore` stacks them.
///
/// The window reads it through here rather than inline so the two shapes the
/// platforms send are handled in one place: the base key with shift held, and
/// the shifted glyph on its own, which is all macOS reports once it has folded
/// shift into the character. The rule for which shape means what lives in
/// [`split::chord_layout`].
pub(crate) fn split_chord(ks: &gpui::Keystroke) -> Option<split::Layout> {
    split::chord_layout(
        &ks.key,
        ks.key_char.as_deref(),
        ks.modifiers.control,
        ks.modifiers.shift,
    )
}

/// The chord that rotates the panes (#2580, #2579): `alt+tab` clockwise and
/// `alt+shift+tab` anticlockwise.
///
/// The window reads it through here rather than inline so the rule stays in
/// one place, [`split::rotation_chord`], where `ctrl+alt+tab` and
/// `cmd+alt+tab` are refused and a shifted tab spelled `backtab` is still
/// accepted.
pub(crate) fn rotation_chord(ks: &gpui::Keystroke) -> Option<split::PaneRotation> {
    split::rotation_chord(
        &ks.key,
        ks.modifiers.alt,
        ks.modifiers.control,
        ks.modifiers.platform,
        ks.modifiers.shift,
    )
}

/// The colour of the rule between two panes (#2473). `text_faint` is the
/// quietest colour that still contrasts with `bg` on every theme, where
/// `panel_hi` sat within a few points of the background on the light ones.
pub(crate) fn split_rule_color() -> u32 {
    palette::current().text_faint
}

/// The ring around a pane (#2612): the accent for the leaf that owns the
/// keyboard, the divider colour for the rest.
///
/// Both states carry the same width, so moving the caret never shifts a row.
/// The unfocused ring is not decoration: it is what keeps a dimmed pane
/// readable as a pane instead of as a hole in the window.
pub(crate) fn focus_ring_color(focused: bool) -> u32 {
    if focused {
        palette::current().accent
    } else {
        split_rule_color()
    }
}

/// The band that shows where a dragged pane will land (#2683).
///
/// It covers the half of the pane the pointer is nearest and sits inside the
/// pane's own box, so the drop is aimed before the release instead of guessed
/// after it. The tint is the accent at low alpha with a solid edge, so it reads
/// over a transcript on every theme without hiding the text under it.
pub(crate) fn drop_indicator(half: DropHalf) -> AnyElement {
    let accent = palette::current().accent;
    let band = div()
        .absolute()
        .bg(rgba((accent << 8) | 0x2e))
        .border_2()
        .border_color(rgb(accent));
    let band = if half.layout.is_stacked() {
        let band = band.left(px(0.)).right(px(0.)).h(relative(0.5));
        if half.after {
            band.bottom(px(0.))
        } else {
            band.top(px(0.))
        }
    } else {
        let band = band.top(px(0.)).bottom(px(0.)).w(relative(0.5));
        if half.after {
            band.right(px(0.))
        } else {
            band.left(px(0.))
        }
    };
    band.into_any_element()
}

/// What one pane may plan its rows against (#2473, #2476). Side by side the
/// halves share the content width less the gap either side of the handle and
/// the handle itself, and `share` is the chat's part of that. Stacked, each
/// pane spans the full width and only the height is divided, so a row there is
/// planned against all of it.
pub(crate) fn split_pane_width(content: f32, stacked: bool, share: f32) -> f32 {
    if stacked {
        content
    } else {
        (content - SPLIT_GAP_PX * 2.0 - SPLIT_HANDLE_PX) * share
    }
}

/// The widest the conversation itself draws, in px (#2586). A line of body
/// text past this is a wall: the eye travels too far back to the left margin
/// to find the next one, so the transcript is centred inside a wide pane
/// instead of stretching edge to edge. Any pane under this width draws at
/// its own width, unchanged.
pub(crate) const CHAT_MEASURE_PX: f32 = 760.0;

/// What the conversation draws at, given the pane it sits in and whether the
/// window is split (#2586). A split window is exempt: every pane there takes
/// its full span, however narrow, because a half-width pane is already the
/// measure. The cap is for the single pane only, so it applies when `split`
/// is false and never in a half of a split.
pub(crate) fn chat_measure(pane_width: f32, split: bool) -> f32 {
    if split {
        pane_width
    } else {
        pane_width.min(CHAT_MEASURE_PX)
    }
}

/// The column a single pane draws its conversation in (#2586): the transcript
/// sits at the measure and the slack goes to the two margins, so the text is
/// centred in the window instead of stretching to a wall. Only a pane that is
/// wider than the measure is wrapped, so a narrow window keeps the exact tree
/// it had, and a split pane is never wrapped because it never takes the cap.
pub(crate) fn chat_measure_column(view: AnyElement, width: f32) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(width))
                .min_w_0()
                .min_h_0()
                .child(view),
        )
        .into_any_element()
}

/// The same centred column for the chat's chrome (#2588): the plan card, the
/// composer and the footer take the conversation's measure instead of the
/// pane's whole width, so the lower half of the window reads on one axis with
/// the text above it. It carries no vertical flex, because these rows keep the
/// height they ask for rather than growing into the column's slack.
pub(crate) fn chat_measure_row(view: AnyElement, width: f32) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .w_full()
        .min_w_0()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(width))
                .min_w_0()
                .child(view),
        )
        .into_any_element()
}

/// The chat's chrome takes the conversation's measure (#2588, #2590), so the
/// plan card, the strips, the popups, the composer and the footer all sit on
/// the axis the text above them draws on. It is a no-op wherever the transcript
/// itself is not capped, which covers every split pane and every window
/// narrower than the measure.
pub(crate) fn central(view: AnyElement, centred: bool, measure: f32) -> AnyElement {
    if centred {
        chat_measure_row(view, measure)
    } else {
        view
    }
}

/// The rule between two panes: vertical when they sit side by side, horizontal
/// when they stack, and draggable to resize both (#2473, #2476). The strip is
/// the target and the rule inside it is what is drawn, so the layout is
/// unchanged but the pointer has a real target to grab.
pub(crate) fn split_divider(
    stacked: bool,
    grab: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("split-divider")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(stacked, |d| d.h(px(SPLIT_HANDLE_PX)).w_full())
        .when(!stacked, |d| d.w(px(SPLIT_HANDLE_PX)).h_full())
        .cursor(if stacked {
            CursorStyle::ResizeUpDown
        } else {
            CursorStyle::ResizeLeftRight
        })
        .on_mouse_down(MouseButton::Left, grab)
        .child(
            div()
                .when(stacked, |d| d.h(px(SPLIT_DIVIDER_PX)).w_full())
                .when(!stacked, |d| d.w(px(SPLIT_DIVIDER_PX)).h_full())
                .bg(rgb(split_rule_color())),
        )
}

#[cfg(test)]
mod window_footer_tests {
    use super::window_footer_yields;

    /// The window keeps its footer while it is one pane: the row is the only
    /// place the pair, the path and the branch are named.
    #[test]
    fn one_pane_keeps_the_window_footer() {
        assert!(!window_footer_yields(1));
    }

    /// Two panes or more hand the row to the panes: each one already names its
    /// own pair, so the window row would only print a second, staler model.
    #[test]
    fn a_split_yields_the_window_footer() {
        assert!(window_footer_yields(2));
        assert!(window_footer_yields(4));
    }
}
