//! The single GPUI model-picker renderer used by `/models` and Brain Fuel.

use super::theme::{Size, text};
use super::{Dashboard, agent_bridge, models, palette};
use gpui::{Context, Div, SharedString, div, prelude::*, px, rgb};
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::model_layout::ModelRowLayout;

pub struct ModelPickerView<'a> {
    pub rows: &'a [models::Row],
    pub query: &'a str,
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
        .child(
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
        )
        .child(text(
            if search_focused { "\u{258f}" } else { "" },
            Size::Sm,
            shown.accent,
        ))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            if onboarding {
                if let Some(ob) = this.onboard.as_mut() {
                    ob.field = ob.search_field();
                    ob.wizard.ps.model_filter = this.models_query.clone();
                }
            } else {
                this.refocus_screen();
            }
            cx.notify();
        }));

    let mut list = div().flex().flex_col().gap_1().w_full().min_w_0();
    if above > 0 {
        list = list.child(div().px_3().child(text(
            format!("\u{2191} {above} more"),
            Size::Xs,
            shown.text_muted,
        )));
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
        let active = if onboarding {
            view.active_provider.to_string()
        } else {
            agent_bridge::bridge()
                .map(|bridge| bridge.service.provider_name())
                .unwrap_or_default()
        };
        if let Some(pair) = models::manual_pair(view.query, &active) {
            list = list.child(
                div()
                    .id("models-use-custom-id")
                    .px_3()
                    .py_2()
                    .child(text(
                        format!("Use custom model: {pair}"),
                        Size::Sm,
                        shown.accent,
                    ))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.activate_model_pair(pair.clone(), onboarding, cx);
                    })),
            );
        } else {
            list = list.child(div().px_3().py_2().child(text(
                format!("no models match \"{}\"", view.query),
                Size::Sm,
                shown.text_muted,
            )));
        }
    }
    if below > 0 {
        list = list.child(div().px_3().child(text(
            format!("\u{2193} {below} more"),
            Size::Xs,
            shown.text_muted,
        )));
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
        .child(search_field)
        .child(results);
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
