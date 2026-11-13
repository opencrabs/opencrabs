//! Pure presentation for the projects screen. Actions stay in the dashboard.

use crate::palette::Palette;
use crate::projects::ProjectRow;
use crate::theme::{Size, text};
use gpui::{Div, SharedString, Stateful, div, prelude::*, px, rgb};
use opencrabs::db::models::Session;
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};

pub fn project_card(row: &ProjectRow, selected: bool, shown: Palette) -> Stateful<Div> {
    let project = &row.project;
    let mut card = div()
        .id(SharedString::from(format!("project-{}", project.id)))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(if selected { shown.focus } else { shown.panel_hi }))
        .bg(glass_fill(if selected { shown.panel_hi } else { shown.panel }))
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(text(project.name.clone(), Size::Sm, shown.text).flex_1().min_w_0().truncate())
                .child(text(
                    format!("{} sessions  {} files", row.sessions, row.files),
                    Size::Xs,
                    shown.text_muted,
                )),
        );
    if let Some(description) = project.description.as_deref().filter(|s| !s.is_empty()) {
        card = card.child(text(description.to_string(), Size::Xs, shown.text_muted));
    }
    let origin = project.repo_remote.as_deref().unwrap_or("no repository recorded");
    card.child(
        div()
            .flex()
            .flex_row()
            .justify_between()
            .gap_2()
            .child(text(origin.to_string(), Size::Xs, shown.text_faint).flex_1().min_w_0().truncate())
            .child(text(
                project.created_at.format("%Y-%m-%d").to_string(),
                Size::Xs,
                shown.text_faint,
            )),
    )
}

pub fn session_card(
    session: &Session,
    selected: bool,
    assign: bool,
    shown: Palette,
) -> Stateful<Div> {
    let prefix = if assign { "assign" } else { "project-session" };
    let title = session.title.as_deref().unwrap_or("Untitled Session");
    let model = session.model.as_deref().unwrap_or("model not recorded");
    let mut card = div()
        .id(SharedString::from(format!("{prefix}-{}", session.id)))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(if selected { shown.focus } else { shown.panel_hi }))
        .bg(glass_fill(if selected { shown.panel_hi } else { shown.panel }))
        .cursor_pointer()
        .child(text(title.to_string(), Size::Sm, shown.text))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(text(model.to_string(), Size::Xs, shown.text_muted).flex_1().min_w_0().truncate())
                .child(text(
                    format!(
                        "{}  {} tokens",
                        session.updated_at.format("%Y-%m-%d %H:%M"),
                        token_count(session.token_count)
                    ),
                    Size::Xs,
                    shown.text_faint,
                )),
        );
    if assign && let Some(directory) = session.working_directory.as_deref() {
        card = card.child(text(directory.to_string(), Size::Xs, shown.text_faint));
    }
    card
}

pub fn action_button(id: &'static str, label: &'static str, danger: bool, shown: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(if danger { shown.red } else { shown.panel_hi }))
        .bg(rgb(shown.panel))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(shown.panel_hi)))
        .child(text(label, Size::Xs, if danger { shown.red } else { shown.text }))
}

pub fn create_panel(input: &str, shown: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.green))
        .bg(glass_fill(shown.panel))
        .child(text("New project name", Size::Xs, shown.text_muted))
        .child(text(format!("{input}|"), Size::Sm, shown.green))
        .child(text("enter create  esc cancel", Size::Xs, shown.text_faint))
}

pub fn delete_panel(name: &str, shown: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(shown.red))
        .bg(glass_fill(shown.panel))
        .child(text(format!("Delete '{name}'?"), Size::Sm, shown.red))
        .child(text(
            "Sessions stay in the ledger and become unassigned. This cannot be undone.",
            Size::Xs,
            shown.text_muted,
        ))
        .child(text("d or enter confirm  esc cancel", Size::Xs, shown.text_faint))
}

pub fn empty(message: &'static str, hint: &'static str, shown: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .items_center()
        .justify_center()
        .p_6()
        .child(text(message, Size::Sm, shown.text_muted))
        .child(text(hint, Size::Xs, shown.text_faint))
}

pub fn token_count(count: i64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}
