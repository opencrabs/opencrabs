//! The single GPUI model-picker renderer used by `/models` and Brain Fuel.

use super::theme::{Size, text};
use super::{Dashboard, models, palette};
use gpui::{Context, Div, SharedString, div, prelude::*, px, rgb};
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::model_layout::ModelRowLayout;
use opencrabs::ui_core::text_field::TextField;

pub struct ModelPickerView<'a> {
    pub rows: &'a [models::Row],
    pub query: &'a str,
    /// The live field behind `query` (#3186): with it the renderer paints the
    /// caret where it actually is and shows the selection, instead of a bar
    /// welded to the end. The onboarding wizard keeps its plain `String`
    /// filter, so this stays optional.
    pub field: Option<&'a TextField>,
    pub selected: usize,
    pub fetching: bool,
    pub pending: usize,
    pub error: Option<&'a str>,
    pub active_provider: &'a str,
    pub onboarding: bool,
    pub search_focused: bool,
    pub max_models: usize,
    pub row_layout: ModelRowLayout,
    pub provider_label_width: f32,
    pub show_provider_labels: bool,
}

pub struct RenderedModelPicker {
    pub body: Div,
    pub footer: Div,
}

pub fn render(cx: &mut Context<Dashboard>, view: ModelPickerView<'_>) -> RenderedModelPicker {
    let shown = palette::current();
    let models::Panel {
        search,
        rows,
        matched,
        total,
        above,
        below,
    } = models::panel(view.rows, view.query, view.selected, view.max_models);
    let onboarding = view.onboarding;
    let search_focused = view.search_focused;
    let search_field = div()
        .id(if onboarding {
            "onboard-search"
        } else {
            "models-search-input"
        })
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .min_w_0()
        .px_3()
        .py_2()
        .rounded(px(concentric_radius(12.0)))
        .bg(rgb(if search_focused {
            shown.panel_hi
        } else {
            shown.panel
        }))
        .border_1()
        .border_color(rgb(if search_focused {
            shown.focus
        } else {
            shown.track
        }))
        .child({
            // The caret and the selection come from the field itself (#3186),
            // the same span split the composer and the pairing form paint.
            // Without a field the wizard keeps its single-string label.
            let mut row = div().flex().flex_row().items_center().flex_1().min_w_0();
            match view.field {
                Some(field) if field.text().is_empty() => {
                    row = row.child(text(search, Size::Sm, shown.text_faint).flex_1().min_w_0());
                }
                Some(field) => {
                    // Owned, because the spans borrow from the field and the
                    // field borrows from `view`: a `&str` here would escape
                    // the function with a non-'static lifetime.
                    let (before, selected, after) = field.render_spans();
                    let (before, selected, after) =
                        (before.to_string(), selected.to_string(), after.to_string());
                    row = row
                        .child(text(before, Size::Sm, shown.text))
                        .when(!selected.is_empty(), |row| {
                            row.child(div().bg(rgb(shown.selection)).child(text(
                                selected,
                                Size::Sm,
                                shown.text,
                            )))
                        })
                        .child(div().flex_none().w(px(1.5)).h(px(14.0)).bg(rgb(
                            if search_focused {
                                shown.accent
                            } else {
                                shown.track
                            },
                        )))
                        .child(text(after, Size::Sm, shown.text));
                }
                None => {
                    row = row.child(
                        text(
                            search,
                            Size::Sm,
                            if view.query.is_empty() {
                                shown.text_faint
                            } else {
                                shown.text
                            },
                        )
                        .flex_1()
                        .min_w_0()
                        .truncate(),
                    );
                }
            }
            row
        })
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            if onboarding {
                if let Some(ob) = this.onboard.as_mut() {
                    ob.field = ob.search_field();
                    ob.wizard.ps.model_filter = this.models_query.text().to_string();
                }
            } else {
                this.refocus_screen();
            }
            cx.notify();
        }));

    let mut list = div().flex().flex_col().gap_1().w_full().min_w_0();
    // The walk's last selectable index, shared by both fold rows (#3184): the
    // keyboard clamps at the last row, plus one more step when the custom-id
    // row is painted, so a click must land in the same place.
    let custom_offered = models::manual_pair(view.query, view.active_provider).is_some()
        && (!onboarding || matched == 0);
    let walk_last = matched.saturating_sub(1) + usize::from(custom_offered);
    if above > 0 {
        // The fold row is a control, not text (#3184): clicking it pages the
        // selection up one window, the same distance the window slid.
        let window = view.max_models;
        list = list.child(
            div()
                .id("models-page-up")
                .px_3()
                .py_1()
                .rounded(px(concentric_radius(6.0)))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(shown.panel_hi)))
                .child(text(
                    format!("\u{2191} {above} more"),
                    Size::Xs,
                    shown.text_muted,
                ))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.models_index = this.models_index.saturating_sub(window);
                    cx.notify();
                })),
        );
    }
    for line in rows {
        match line {
            models::PanelRow::Header {
                provider,
                name,
                count,
            } => {
                list = list.child(
                    div()
                        .id(SharedString::from(format!("model-provider-{provider}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .px_3()
                        .pt_3()
                        .pb_1()
                        .child(
                            text(name, Size::Xs, shown.text_muted)
                                .flex_1()
                                .min_w_0()
                                .truncate(),
                        )
                        .child(text(
                            format!("{count} model{}", if count == 1 { "" } else { "s" }),
                            Size::Xs,
                            shown.text_muted,
                        )),
                );
            }
            models::PanelRow::Model {
                row,
                index,
                selected,
            } => {
                let pair = row.pair();
                let marker = || {
                    text(
                        if row.current { "\u{25cf}" } else { " " },
                        Size::Sm,
                        if row.current {
                            shown.accent
                        } else {
                            shown.text_muted
                        },
                    )
                };
                let mut entry = div()
                    .id(SharedString::from(format!("model-row-{index}")))
                    .flex()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .rounded(px(concentric_radius(12.0)))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(shown.panel_hi)));
                entry = match view.row_layout {
                    ModelRowLayout::Columns => entry
                        .flex_row()
                        .items_center()
                        .child(marker())
                        .child(
                            text(row.model.clone(), Size::Sm, shown.text)
                                .flex_1()
                                .min_w_0()
                                .truncate(),
                        )
                        .child(
                            text(row.provider_name.clone(), Size::Xs, shown.text_muted)
                                .w(px(view.provider_label_width))
                                .flex_none()
                                .truncate(),
                        ),
                    ModelRowLayout::Stacked => entry
                        .flex_col()
                        .items_start()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .min_w_0()
                                .child(marker())
                                .child(
                                    text(row.model.clone(), Size::Sm, shown.text)
                                        .flex_1()
                                        .min_w_0()
                                        .truncate(),
                                ),
                        )
                        .when(view.show_provider_labels, |entry| {
                            entry.child(
                                div().flex().flex_row().w_full().min_w_0().pl_5().child(
                                    text(row.provider_name.clone(), Size::Xs, shown.text_muted)
                                        .flex_1()
                                        .min_w_0()
                                        .truncate(),
                                ),
                            )
                        }),
                };
                if selected {
                    entry = entry.bg(rgb(shown.panel_hi));
                }
                list = list.child(entry.on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.models_index = index;
                    this.activate_model_pair(pair.clone(), onboarding, cx);
                })));
            }
        }
    }
    if matched == 0 {
        let manual = models::manual_pair(view.query, view.active_provider);
        if manual.is_none() {
            list = list.child(div().px_3().py_2().child(text(
                format!("no models match \"{}\"", view.query),
                Size::Sm,
                shown.text_muted,
            )));
        }
    }
    // The typed id is always offered on the standalone picker (#3180): a
    // fuzzy half-match must not hide the exact id the user typed. The
    // wizard keeps its zero-match-only row and its own layout.
    if let Some(pair) = models::manual_pair(view.query, view.active_provider)
        .filter(|_| !onboarding || matched == 0)
    {
        let selected = !onboarding && view.selected >= matched;
        let mut entry = div()
            .id("models-use-custom-id")
            .flex()
            .px_3()
            .py_2()
            .rounded(px(concentric_radius(12.0)))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(shown.panel_hi)))
            .child(text(
                format!("Use custom model: {pair}"),
                Size::Sm,
                shown.accent,
            ));
        if selected {
            entry = entry.bg(rgb(shown.panel_hi));
        }
        list = list.child(entry.on_click(cx.listener(move |this, _ev, _window, cx| {
            this.activate_model_pair(pair.clone(), onboarding, cx);
        })));
    }
    if below > 0 {
        // The fold row is a control, not text (#3184): clicking it pages the
        // selection down one window, landing on the first row the fold hid,
        // clamped where the keyboard walk clamps.
        let window = view.max_models;
        list = list.child(
            div()
                .id("models-page-down")
                .px_3()
                .py_1()
                .rounded(px(concentric_radius(6.0)))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(shown.panel_hi)))
                .child(text(
                    format!("\u{2193} {below} more"),
                    Size::Xs,
                    shown.text_muted,
                ))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.models_index = (this.models_index + window).min(walk_last);
                    cx.notify();
                })),
        );
    }
    let mut results = div()
        .id(if onboarding {
            "onboard-models"
        } else {
            "models-body"
        })
        .w_full()
        .min_w_0()
        .child(list);
    if !onboarding {
        results = results.flex_1().min_h_0().overflow_y_scroll();
    }
    let mut body = div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(search_field);
    if !onboarding {
        // The filter field is also the custom-id field (#3180). Without a
        // visible label it reads as a pure filter and the custom path is
        // undiscoverable: name the field and spell the rule out.
        body = body
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(shown.text_muted))
                            .child(SharedString::from("filter or paste a custom model id")),
                    )
                    .child(div().flex_1().h(px(1.0)).bg(rgb(shown.track))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(shown.text_faint))
                    .whitespace_normal()
                    .child(SharedString::from(
                        "type a name to filter, or any model id and press enter to use it",
                    )),
            );
    }
    body = body.child(results);
    if !onboarding {
        body = body.flex_1().min_h_0();
    }

    let counted = if matched == total {
        format!("{total} model{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{matched} of {total} models · filter: {}", view.query)
    };
    let mut footer = div().flex().flex_col().gap_1().w_full().min_w_0().child(
        div()
            .text_xs()
            .text_color(rgb(shown.text_muted))
            .whitespace_normal()
            .child(SharedString::from(counted)),
    );
    if view.fetching {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_muted))
                .whitespace_normal()
                .child(format!(
                    "fetching live models · {} provider{} remaining",
                    view.pending,
                    if view.pending == 1 { "" } else { "s" }
                )),
        );
    }
    if let Some(error) = view.error {
        let count = error.split("; ").count();
        footer = footer.child(
            div()
                .text_xs()
                .text_color(rgb(shown.amber))
                .min_w_0()
                .whitespace_normal()
                .child(SharedString::from(format!(
                    "{count} provider{} could not refresh; saved models remain available",
                    if count == 1 { "" } else { "s" }
                ))),
        );
    }
    RenderedModelPicker { body, footer }
}
