//! The theme modal's cards (#2619).
//!
//! Four cards only: `orgn`, `light`, `custom`, `auto`. Each card is a
//! miniature of the window it would paint, so the choice is made by looking
//! rather than by reading theme names. The modal is centred over the
//! conversation, the way Telegram's theme picker is; the click handlers live
//! with the dashboard in `main.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, PathBuilder, SharedString, Stateful, Window, canvas, div,
    point, prelude::*, px, rgb, rgba,
};
use opencrabs::config::GuiTheme;
use opencrabs::config::gui_font;
use opencrabs::config::gui_theme::{self, LIGHT, ORGN, Palette};
use opencrabs::ui_core::card::concentric_radius;

use crate::theme::{Size, text};

/// The four cards, in the order the modal draws them.
pub const CARDS: [(GuiTheme, &str, &str); 4] = [
    (GuiTheme::Orgn, "Orgn", "the window's own dark palette"),
    (GuiTheme::Light, "Light", "soft paper, easy on the eyes"),
    (
        GuiTheme::Custom,
        "Custom",
        "your base and accent, everything derived",
    ),
    (GuiTheme::Auto, "Auto", "follows the system appearance"),
];

/// A miniature of the window: a canvas, two bubbles and an accent chip. It
/// carries the palette the card would paint, so a card is legible before it
/// is picked.
fn miniature(shown: Palette, accent: u32) -> Div {
    let bubble = |w: f32, color: u32| div().h(px(4.0)).w(px(w)).rounded(px(2.0)).bg(rgb(color));
    div()
        .flex()
        .flex_col()
        .justify_between()
        .p_2()
        .w_full()
        .h(px(64.0))
        .rounded(px(concentric_radius(10.0)))
        .bg(rgb(shown.bg))
        .border_1()
        .border_color(rgb(shown.track))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .w(px(52.0))
                .p_1()
                .rounded(px(4.0))
                .bg(rgb(shown.panel))
                .child(bubble(34.0, shown.text_muted))
                .child(bubble(24.0, shown.text_faint)),
        )
        .child(
            div().flex().flex_row().justify_end().w_full().child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(46.0))
                    .p_1()
                    .rounded(px(4.0))
                    .bg(rgb(shown.panel_hi))
                    .child(bubble(30.0, accent)),
            ),
        )
}

/// Auto's preview is both appearances at once, because that is what the mode
/// does: the left half is the dark palette, the right half the light one, so
/// the card cannot be mistaken for a fixed colour.
fn split_miniature(accent: u32) -> Div {
    let half = |shown: Palette| {
        div()
            .flex()
            .flex_col()
            .justify_between()
            .p_2()
            .w_full()
            .h_full()
            .bg(rgb(shown.bg))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(30.0))
                    .p_1()
                    .rounded(px(4.0))
                    .bg(rgb(shown.panel))
                    .child(
                        div()
                            .h(px(3.0))
                            .w(px(18.0))
                            .rounded(px(2.0))
                            .bg(rgb(shown.text_muted)),
                    )
                    .child(
                        div()
                            .h(px(3.0))
                            .w(px(12.0))
                            .rounded(px(2.0))
                            .bg(rgb(shown.text_faint)),
                    ),
            )
            .child(
                div().flex().flex_row().justify_end().w_full().child(
                    div()
                        .h(px(3.0))
                        .w(px(16.0))
                        .rounded(px(2.0))
                        .bg(rgb(accent)),
                ),
            )
    };
    div()
        .flex()
        .flex_row()
        .w_full()
        .h(px(64.0))
        .rounded(px(concentric_radius(10.0)))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(ORGN.track))
        .child(div().flex_1().h_full().overflow_hidden().child(half(ORGN)))
        .child(div().flex_1().h_full().overflow_hidden().child(half(LIGHT)))
}

/// One card: the miniature, its name, and the radio ring that marks the
/// theme in force. `selected` is the card the window is drawing: the saved
/// theme when the modal opens, and the card under the keyboard as the arrows
/// walk it, because the preview has already repainted the window.
///
/// `shown` is the candidate palette for the miniature, resolved by the caller
/// from the base the Theme screen holds (#2746). The surrounding card reads the
/// window's live palette instead, so every card keeps the same chrome (#2921).
pub fn card(
    mode: GuiTheme,
    label: &str,
    hint: &str,
    selected: bool,
    accent: u32,
    shown: Palette,
) -> Stateful<Div> {
    // The modal owns the card chrome. Candidate palettes belong in the
    // miniature only, so Light and Custom do not turn the picker itself into
    // a patchwork of unrelated surfaces (#2921).
    let chrome = crate::palette::current();
    let ring = if selected {
        div()
            .flex_none()
            .size(px(14.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.0))
            .bg(rgb(accent))
            .child(div().size(px(5.0)).rounded(px(3.0)).bg(rgb(chrome.panel)))
    } else {
        div()
            .flex_none()
            .size(px(14.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(chrome.text_faint))
    };
    div()
        .id(SharedString::from(format!("theme-card-{label}")))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .w(px(132.0))
        .rounded(px(concentric_radius(6.0)))
        .border_1()
        .border_color(if selected {
            rgb(accent)
        } else {
            rgb(chrome.track)
        })
        .bg(rgb(chrome.bg))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(chrome.panel_hi)))
        .child(if mode == GuiTheme::Auto {
            split_miniature(accent)
        } else {
            miniature(shown, accent)
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .child(text(label.to_string(), Size::Sm, chrome.text))
                .child(ring),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(chrome.text_muted))
                .child(SharedString::from(hint.to_string())),
        )
}

/// One base in the picker (#2746): the canvas colour, and the ring that marks
/// the one in force. It shows a background only, because that is all a base
/// is: every surface, border and text tone is derived from it.
pub fn swatch(bg: u32, name: &str, selected: bool, shown: Palette) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("theme-base-{name}")))
        .flex_none()
        .size(px(26.0))
        .rounded(px(concentric_radius(8.0)))
        .bg(rgb(bg))
        .border_2()
        .border_color(rgb(if selected { shown.accent } else { shown.track }))
        .cursor_pointer()
        .hover(|d| d.border_color(rgb(shown.text_muted)))
}

/// The transparency track and thumb, painted against the real laid-out width.
/// The canvas also records the hit box the dashboard converts into a value.
fn slider(fraction: f32, accent: u32, area: SliderArea) -> Div {
    const HEIGHT: f32 = 14.0;
    const THUMB_WIDTH: f32 = 8.0;
    let fraction = fraction.clamp(0.0, 1.0);
    let recorded = area;
    let painted = canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _, window, _cx| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let x0 = f32::from(bounds.origin.x);
            let y0 = f32::from(bounds.origin.y);
            recorded
                .borrow_mut()
                .replace([x0, y0, x0 + width, y0 + height]);

            let track_y = y0 + height / 2.0 - 1.0;
            let mut track = PathBuilder::fill();
            track.move_to(point(px(x0), px(track_y)));
            track.line_to(point(px(x0 + width), px(track_y)));
            track.line_to(point(px(x0 + width), px(track_y + 2.0)));
            track.line_to(point(px(x0), px(track_y + 2.0)));
            track.close();
            if let Ok(path) = track.build() {
                // A quiet tint of the active accent, never a new literal hue.
                window.paint_path(path, rgba((accent << 8) | 0x66));
            }

            let thumb_x = x0 + (width - THUMB_WIDTH).max(0.0) * fraction;
            let mut thumb = PathBuilder::fill();
            thumb.move_to(point(px(thumb_x), px(y0)));
            thumb.line_to(point(px(thumb_x + THUMB_WIDTH), px(y0)));
            thumb.line_to(point(px(thumb_x + THUMB_WIDTH), px(y0 + HEIGHT)));
            thumb.line_to(point(px(thumb_x), px(y0 + HEIGHT)));
            thumb.close();
            if let Ok(path) = thumb.build() {
                window.paint_path(path, rgb(accent));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full();
    div().relative().w_full().h(px(HEIGHT)).child(painted)
}

/// Whole-window transparency (#2922). This is independent of the palette, so
/// the control stays available for every theme card.
pub fn transparency_row(value: u8, shown: Palette, track: Rc<RefCell<Option<[f32; 4]>>>) -> Div {
    let value = gui_theme::clamp_window_transparency(value);
    let position = f32::from(value) / f32::from(gui_theme::MAX_WINDOW_TRANSPARENCY);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .p_3()
        .rounded(px(concentric_radius(6.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child("Window transparency"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.accent))
                        .child(SharedString::from(format!("{value}%"))),
                ),
        )
        .child(slider(position, shown.accent, track))
        .child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_faint))
                .child("Entire window, text and dialogs included. 0% is opaque; maximum 40%."),
        )
}

/// The base picker (#2746): the canvases the window can sit on, under the
/// cards. A pick saves the base and switches the window to `custom`, so the
/// whole interface follows the one colour that was chosen.
pub fn base_row(current: Option<u32>, swatches: Vec<Stateful<Div>>, shown: Palette) -> Div {
    let name = current
        .and_then(|bg| {
            gui_theme::BASES
                .iter()
                .find(|(known, _)| *known == bg)
                .map(|(_, name)| *name)
        })
        .unwrap_or("the system's");
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .p_3()
        .rounded(px(concentric_radius(6.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child("Base"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(name.to_string())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .w_full()
                .children(swatches),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_faint))
                .child("the canvas; surfaces, borders and text are derived from it"),
        )
}

/// The wheel's diameter, in pixels. The generated disc is drawn at this size,
/// so the knob can sit on the saturation the window holds.
pub const WHEEL_DIAMETER: f32 = 148.0;

/// The hue disc, drawn once per process.
///
/// Hue runs around, saturation outward, at full brightness, which is exactly
/// what `accent::wheel_hit` reads back: the angle under the pointer is the hue
/// and its distance from the centre is the saturation. It is a generated PNG
/// rather than a few hundred painted paths, so a drag costs one primitive per
/// frame and the gradient stays smooth instead of banding by segment count.
fn disc() -> std::sync::Arc<gpui::Image> {
    use std::cell::OnceCell;
    thread_local! {
        static DISC: OnceCell<std::sync::Arc<gpui::Image>> = const { OnceCell::new() };
    }
    DISC.with(|cell| cell.get_or_init(disc_image).clone())
}

/// Paint `WHEEL_DIAMETER` squared pixels: hue by angle, saturation by radius,
/// transparent past the rim.
fn disc_image() -> std::sync::Arc<gpui::Image> {
    const SIZE: u32 = WHEEL_DIAMETER as u32;
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0;
    let mut pixels = image::RgbaImage::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = (x as f32 - centre) / radius;
            let dy = (y as f32 - centre) / radius;
            let distance = (dx * dx + dy * dy).sqrt();
            if distance > 1.0 {
                continue;
            }
            // One pixel of coverage at the rim, so the disc is not a staircase.
            let alpha = ((1.0 - distance) * radius).clamp(0.0, 1.0);
            let hue = gui_theme::hue_from_unit(dx, dy);
            let color = gui_theme::hsv_to_rgb(hue, distance.min(1.0), 1.0);
            pixels.put_pixel(
                x,
                y,
                image::Rgba([
                    (color >> 16) as u8,
                    (color >> 8) as u8,
                    color as u8,
                    (alpha * 255.0) as u8,
                ]),
            );
        }
    }
    let mut png = Vec::new();
    let written = image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    debug_assert!(written.is_ok(), "the wheel is generated, not decoded");
    std::sync::Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png))
}

/// The colour wheel: the hue and saturation disc with a knob on the colour in
/// force. The box is recorded while painting so a press can be turned into a
/// wheel position by the dashboard.
pub fn wheel(hue: f32, saturation: f32, value: f32, area: WheelArea) -> impl IntoElement {
    let knob_color = gui_theme::hsv_to_rgb(hue, saturation.max(0.2), value.max(0.2));
    let (kx, ky) = gui_theme::hue_to_unit(hue);
    let knob_radius = (WHEEL_DIAMETER / 2.0 - 8.0) * saturation.clamp(0.0, 1.0);
    let recorded = area;
    let boxed = canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _, _window, _cx| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let x0 = f32::from(bounds.origin.x);
            let y0 = f32::from(bounds.origin.y);
            recorded
                .borrow_mut()
                .replace([x0, y0, x0 + width, y0 + height]);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size(px(WHEEL_DIAMETER));
    let face = gpui::img(disc())
        .absolute()
        .top_0()
        .left_0()
        .size(px(WHEEL_DIAMETER));
    // Brightness is a veil rather than a second generated disc: compositing
    // black at `1 - value` over the full-brightness colour is exactly what
    // scaling value does, so the disc and the stored colour cannot disagree.
    let veil = div()
        .absolute()
        .top_0()
        .left_0()
        .size(px(WHEEL_DIAMETER))
        .rounded(px(WHEEL_DIAMETER / 2.0))
        .bg(rgba(((1.0 - value.clamp(0.0, 1.0)) * 255.0).round() as u32));
    let knob = div()
        .absolute()
        .left(px(WHEEL_DIAMETER / 2.0 + kx * knob_radius - 7.0))
        .top(px(WHEEL_DIAMETER / 2.0 + ky * knob_radius - 7.0))
        .size(px(14.0))
        .rounded(px(7.0))
        .border_2()
        .border_color(rgb(0xffffff))
        .bg(rgb(knob_color));
    div()
        .id("theme-wheel")
        .relative()
        .flex_none()
        .size(px(WHEEL_DIAMETER))
        .child(face)
        .child(veil)
        .child(boxed)
        .child(knob)
}

/// The custom accent's controls: the wheel, the swatch it is painting, the hex
/// the window stores, and the two tracks that finish the colour off. The ring
/// sets the hue by angle and the saturation by distance; the tracks set either
/// value exactly, which a ring cannot do near its centre.
pub fn accent_row(
    hue: f32,
    saturation: f32,
    value: f32,
    hex: &str,
    shown: Palette,
    areas: AccentAreas,
) -> Div {
    let swatch = gui_theme::hsv_to_rgb(hue, saturation, value);
    let faint = shown.text_faint;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .w_full()
        .p_3()
        .rounded(px(concentric_radius(6.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .child(wheel(hue, saturation, value, areas.wheel))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(22.0))
                                .rounded(px(6.0))
                                .bg(rgb(swatch))
                                .border_1()
                                .border_color(rgb(shown.track)),
                        )
                        .child(text(hex.to_string(), Size::Xs, shown.text)),
                )
                .child(track(
                    "saturation",
                    saturation,
                    gui_theme::hsv_to_rgb(hue, 0.0, value),
                    gui_theme::hsv_to_rgb(hue, 1.0, value),
                    faint,
                    areas.saturation,
                ))
                .child(track(
                    "brightness",
                    value,
                    0x000000,
                    gui_theme::hsv_to_rgb(hue, saturation, 1.0),
                    faint,
                    areas.value,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child("drag the ring: angle picks the hue, distance the depth"),
                ),
        )
}

/// One horizontal track: the colours it can produce, painted left to right,
/// with a marker at the value in force. The box is recorded while painting, so
/// a press can be turned into a value by the dashboard the way the wheel's is.
fn track(label: &str, fraction: f32, from: u32, to: u32, faint: u32, area: SliderArea) -> Div {
    const SEGMENTS: u32 = 24;
    const WIDTH: f32 = 148.0;
    const HEIGHT: f32 = 10.0;
    let recorded = area;
    let painted = canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _, window, _cx| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let x0 = f32::from(bounds.origin.x);
            let y0 = f32::from(bounds.origin.y);
            recorded
                .borrow_mut()
                .replace([x0, y0, x0 + width, y0 + height]);
            for step in 0..SEGMENTS {
                let a = x0 + width * step as f32 / SEGMENTS as f32;
                let b = x0 + width * (step + 1) as f32 / SEGMENTS as f32;
                let color = gui_theme::mix(from, to, step * 100 / (SEGMENTS - 1), 100);
                let mut builder = PathBuilder::fill();
                builder.move_to(point(px(a), px(y0)));
                builder.line_to(point(px(b), px(y0)));
                builder.line_to(point(px(b), px(y0 + height)));
                builder.line_to(point(px(a), px(y0 + height)));
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, rgb(color));
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full();
    let marker_x = (WIDTH - 3.0) * fraction.clamp(0.0, 1.0);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .child(
            div().flex_none().w(px(64.0)).child(
                div()
                    .text_xs()
                    .text_color(rgb(faint))
                    .child(label.to_string()),
            ),
        )
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(WIDTH))
                .h(px(HEIGHT))
                .rounded(px(5.0))
                .overflow_hidden()
                .child(painted)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(marker_x))
                        .w(px(3.0))
                        .h(px(HEIGHT))
                        .bg(rgb(0xffffff)),
                ),
        )
}

/// The boxes the accent controls painted this frame, so a press can be turned
/// into a value without guessing at the layout.
#[derive(Clone, Default)]
pub struct AccentAreas {
    pub wheel: WheelArea,
    pub saturation: SliderArea,
    pub value: SliderArea,
}

/// The font row under the cards (#2786): the family in force, and, while the
/// list is open, the machine's families in a column that scrolls.
///
/// It sits inside the card, so a click on a row never reaches the scrim's
/// dismiss handler: the card stops propagation. The rows arrive already built,
/// because only the dashboard's context can mint a click listener.
pub fn font_row(
    current: &str,
    open: bool,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    rows: Vec<Stateful<Div>>,
) -> Div {
    let shown = crate::palette::current();
    let mut column = div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .p_3()
        .rounded(px(concentric_radius(6.0)))
        .border_1()
        .border_color(rgb(shown.track))
        .bg(rgb(shown.bg))
        .child(
            div()
                .id("theme-font-head")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .py_1()
                .cursor_pointer()
                .on_click(toggle)
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child("Font"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text))
                                .child(SharedString::from(gui_font::row_label(current))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(shown.text_faint))
                                .child(if open { "close" } else { "change" }),
                        ),
                ),
        );
    if open {
        // A bounded height with a scroll, because the catalog is every family
        // the machine has: a few hundred on a stock macOS install.
        column = column.child(
            div()
                .id("theme-font-list")
                .flex()
                .flex_col()
                .w_full()
                .max_h(px(150.0))
                .overflow_y_scroll()
                .border_1()
                .border_color(rgb(shown.panel_hi))
                .rounded(px(concentric_radius(10.0)))
                .p_1()
                .children(rows),
        );
    }
    column
}

/// The modal itself: a scrim that dismisses, and a centred card that does
/// not. The body scrolls inside the shell, so every control stays reachable
/// on a short window. `accent` is present only while Custom is in force.
pub fn modal(
    cards: Vec<Stateful<Div>>,
    base: Div,
    transparency: Stateful<Div>,
    accent: Option<AnyElement>,
    fonts: Div,
    footer: &str,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let shown = crate::palette::current();
    let mut rows = div().flex().flex_row().gap_3().justify_center();
    for card in cards {
        rows = rows.child(card);
    }
    let mut body = div()
        .id("theme-modal-body")
        .flex()
        .flex_col()
        .gap_3()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(rows)
        .child(base)
        .child(transparency);
    if let Some(accent) = accent {
        body = body.child(accent);
    }
    let body = body.child(fonts);
    crate::modal_shell::render(
        crate::modal_shell::ModalView {
            id: "theme-modal",
            title: "Theme".to_string(),
            subtitle: String::new(),
            actions: None,
            header_close: true,
            body: body.into_any_element(),
            footer: Some(
                div()
                    .text_xs()
                    .text_color(rgb(shown.text_muted))
                    .child(SharedString::from(footer.to_string()))
                    .into_any_element(),
            ),
            width: 620.0,
            max_height: 840.0,
        },
        dismiss,
    )
}

/// The box a wheel records, as the dashboard reads it.
pub type WheelArea = Rc<RefCell<Option<[f32; 4]>>>;

/// The box a slider track records, as the dashboard reads it.
pub type SliderArea = Rc<RefCell<Option<[f32; 4]>>>;
