//! Standalone model browser. Onboarding keeps its own wizard layout.

use super::*;

impl Dashboard {
    pub(crate) fn models_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let shown = palette::current();
        let visible = models::filtered_rows(&self.models_rows, &self.models_query);
        let providers = models::providers(&visible);
        let live = agent_bridge::bridge().map(|bridge| match self.models_session.or(self.chat_session) {
            Some(sid) => (
                bridge.service.provider_name_for_session(sid),
                bridge.service.provider_model_for_session(sid),
            ),
            None => (bridge.service.provider_name(), bridge.service.provider_model()),
        });
        let pair = live
            .as_ref()
            .map(|(provider, model)| format!("{provider} / {model}"))
            .unwrap_or_else(|| "Choose a model for this session".to_string());
        let active_provider = live
            .as_ref()
            .map(|(provider, _)| provider.as_str())
            .unwrap_or_default();
        let selected_provider = self.models_provider.clone();
        let mut provider_list = div().flex().flex_col().gap_1().w_full();
        for row in providers {
            let id = row.provider.clone();
            let selected = selected_provider.as_deref() == Some(id.as_str());
            let single = models::single_model_pair(&visible, &row);
            let mut entry = div()
                .id(SharedString::from(format!("models-provider-{id}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(px(8.0))
                .cursor_pointer()
                .child(text(row.name.clone(), Size::Sm, shown.text).flex_1().min_w_0().truncate())
                .child(text(row.count.to_string(), Size::Xs, shown.text_muted));
            if selected {
                entry = entry.bg(rgb(shown.panel_hi)).border_1().border_color(rgb(shown.accent));
            }
            provider_list = provider_list.child(entry.on_click(cx.listener(
                move |this, _ev, _window, cx| {
                    this.models_provider = Some(id.clone());
                    this.models_index = 0;
                    if let Some(pair) = single.clone() {
                        this.switch_model(pair, cx);
                    }
                    cx.notify();
                },
            )));
        }
        let model_rows = self.visible_model_rows();
        let picker = model_picker::render(
            cx,
            model_picker::ModelPickerView {
                rows: &model_rows,
                query: &self.models_query,
                selected: self.models_index,
                fetching: self.models_fetching,
                pending: self.models_pending.len(),
                error: self.models_fetch_error.as_deref(),
                active_provider,
                onboarding: false,
                search_focused: true,
                max_models: 24,
                row_layout: opencrabs::ui_core::model_layout::ModelRowLayout::Stacked,
                provider_label_width: 0.0,
                show_provider_labels: false,
            },
        );
        let body = div()
            .flex()
            .flex_row()
            .gap_3()
            .w_full()
            .h(px(440.0))
            .min_h_0()
            .child(
                div()
                    .id("models-providers")
                    .w(px(180.0))
                    .flex_none()
                    .h_full()
                    .overflow_y_scroll()
                    .pr_2()
                    .child(provider_list),
            )
            .child(div().flex_1().min_w_0().h_full().child(picker.body));
        let footer = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(picker.footer)
            .child(modal_shell::footer(
                "type filter or custom id  enter switch  esc close".to_string(),
            ));
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.screen = Screen::Chat;
            this.refocus_screen();
            cx.notify();
        });
        modal_shell::render(
            modal_shell::ModalView {
                id: "models-modal",
                title: "Models".to_string(),
                subtitle: pair,
                actions: None,
                body: body.into_any_element(),
                footer: Some(footer.into_any_element()),
                width: 880.0,
                max_height: 620.0,
            },
            dismiss,
        )
        .into_any_element()
    }
}
