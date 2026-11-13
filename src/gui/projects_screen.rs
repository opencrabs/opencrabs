//! Themes-style project screen rendered from completed service snapshots.

use super::*;
use crate::projects::ProjectsMode;
use opencrabs::ui_core::card::concentric_radius;

impl Dashboard {
    pub(crate) fn projects_screen(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let shown = palette::current();
        let mode = self.projects.mode;
        let project = mode
            .project_id()
            .and_then(|id| self.projects.project(id))
            .cloned();
        let title = match mode {
            ProjectsMode::List => "Projects".to_string(),
            ProjectsMode::Detail(_) => project
                .as_ref()
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "Project".to_string()),
            ProjectsMode::Assign(_) => "Assign a session".to_string(),
            ProjectsMode::AssignSession(_) => "Assign project".to_string(),
        };
        let subtitle = match mode {
            ProjectsMode::List => "Group related sessions and their tracked files".to_string(),
            ProjectsMode::Detail(_) => project
                .as_ref()
                .and_then(|project| project.description.clone())
                .unwrap_or_else(|| "Sessions in this project".to_string()),
            ProjectsMode::Assign(_) => format!(
                "Unassigned sessions for {}",
                project
                    .as_ref()
                    .map(|project| project.name.as_str())
                    .unwrap_or("this project")
            ),
            ProjectsMode::AssignSession(_) => {
                "Choose a project for this session and every topic inside it".to_string()
            }
        };
        let mut actions = div().flex().flex_row().items_center().gap_2();
        match mode {
            ProjectsMode::List => {
                actions = actions.child(
                    projects_view::action_button("project-new", "new", false, shown).on_click(
                        cx.listener(|this, _ev, _window, cx| this.begin_project_create(cx)),
                    ),
                );
                if self.projects.selected_project().is_some() {
                    actions = actions.child(
                        projects_view::action_button("project-assign", "assign", false, shown)
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                if let Some(id) =
                                    this.projects.selected_project().map(|project| project.id)
                                {
                                    this.begin_project_assign(id, cx);
                                }
                            })),
                    );
                    actions = actions.child(
                        projects_view::action_button("project-delete", "delete", true, shown)
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.request_project_delete(cx)
                            })),
                    );
                }
            }
            ProjectsMode::Detail(project_id) => {
                actions = actions.child(
                    projects_view::action_button("project-back", "back", false, shown).on_click(
                        cx.listener(|this, _ev, _window, cx| this.projects_escape(cx)),
                    ),
                );
                actions = actions.child(
                    projects_view::action_button("project-detail-assign", "assign", false, shown)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.begin_project_assign(project_id, cx)
                        })),
                );
                actions = actions.child(
                    projects_view::action_button("project-detail-delete", "delete", true, shown)
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.request_project_delete(cx)
                        })),
                );
            }
            ProjectsMode::Assign(_) | ProjectsMode::AssignSession(_) => {
                actions = actions.child(
                    projects_view::action_button("project-assign-back", "back", false, shown)
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.projects_escape(cx)
                        })),
                );
            }
        }

        let mut content = div()
            .id("projects-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .overflow_y_scroll();
        if self.projects.creating {
            content = content.child(projects_view::create_panel(
                &self.projects.create_input,
                shown,
            ));
        }
        if self.projects.delete_confirm {
            let name = project
                .as_ref()
                .map(|project| project.name.clone())
                .or_else(|| {
                    self.projects
                        .selected_project()
                        .map(|project| project.name.clone())
                })
                .unwrap_or_else(|| "this project".to_string());
            content = content.child(projects_view::delete_panel(&name, shown));
            let cancel = projects_view::action_button(
                "project-delete-cancel",
                "cancel",
                false,
                shown,
            )
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.projects.delete_confirm = false;
                cx.notify();
            }));
            let confirm = projects_view::action_button(
                "project-delete-confirm",
                "confirm delete",
                true,
                shown,
            )
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.confirm_project_delete(cx)
            }));
            content = content.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            );
        }
        if let Some(error) = self.projects.error.clone() {
            content = content.child(text(error, Size::Xs, shown.red));
        }
        if let Some(notice) = self.projects.notice.clone() {
            content = content.child(text(notice, Size::Xs, shown.green));
        }

        let count = match mode {
            ProjectsMode::List | ProjectsMode::AssignSession(_) => self.projects.rows.len(),
            ProjectsMode::Detail(_) => self.projects.sessions.len(),
            ProjectsMode::Assign(_) => self.projects.unassigned.len(),
        };
        if count == 0 {
            let (message, hint) = if self.projects.loading {
                ("Reading projects...", "the shared project service is loading")
            } else {
                match mode {
                    ProjectsMode::List => ("No projects yet", "press n to create one"),
                    ProjectsMode::Detail(_) => {
                        ("No sessions assigned", "press a to assign a session")
                    }
                    ProjectsMode::Assign(_) => (
                        "No unassigned sessions",
                        "every session already belongs to a project",
                    ),
                    ProjectsMode::AssignSession(_) => {
                        ("No projects yet", "open /projects to create one first")
                    }
                }
            };
            content = content.child(projects_view::empty(message, hint, shown));
            if mode == ProjectsMode::List && !self.projects.loading {
                content = content.child(text(
                    format!("Project files live in {}", self.projects.directory.display()),
                    Size::Xs,
                    shown.text_faint,
                ));
            }
        } else {
            match mode {
                ProjectsMode::List | ProjectsMode::AssignSession(_) => {
                    for (index, row) in self.projects.rows.iter().enumerate() {
                        let id = row.project.id;
                        let card = projects_view::project_card(
                            row,
                            index == self.projects.selected,
                            shown,
                        )
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.projects.selected = index;
                            if matches!(mode, ProjectsMode::AssignSession(_)) {
                                this.assign_session_to_project(id, cx);
                            } else {
                                this.open_project(id, cx);
                            }
                        }));
                        content = content.child(card);
                    }
                }
                ProjectsMode::Detail(_) => {
                    for (index, session) in self.projects.sessions.iter().enumerate() {
                        let id = session.id;
                        let card = projects_view::session_card(
                            session,
                            index == self.projects.session_selected,
                            false,
                            shown,
                        )
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.projects.session_selected = index;
                            this.open_session(id, cx);
                        }));
                        content = content.child(card);
                    }
                }
                ProjectsMode::Assign(project_id) => {
                    for (index, session) in self.projects.unassigned.iter().enumerate() {
                        let id = session.id;
                        let card = projects_view::session_card(
                            session,
                            index == self.projects.session_selected,
                            true,
                            shown,
                        )
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.projects.session_selected = index;
                            this.assign_project_session(project_id, id, cx);
                        }));
                        content = content.child(card);
                    }
                }
            }
        }

        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(text(title, Size::Xl, shown.text))
                    .child(text(subtitle, Size::Xs, shown.text_muted)),
            )
            .child(actions);
        let panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_4()
            .rounded(px(concentric_radius(18.0)))
            .border_1()
            .border_color(rgb(shown.panel_hi))
            .bg(rgb(shown.panel))
            .child(header)
            .child(content)
            .into_any_element();
        let hints = match mode {
            ProjectsMode::List => "up/down select  enter open  n new  a assign  d delete  esc close",
            ProjectsMode::Detail(_) => {
                "up/down select  enter open  a assign  u unassign  d delete  esc back"
            }
            ProjectsMode::Assign(_) => "up/down select  enter assign  esc back",
            ProjectsMode::AssignSession(_) => "up/down select  enter assign  esc cancel",
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .child(text(format!("{count} rows"), Size::Xs, shown.text_faint))
            .child(text(hints, Size::Xs, shown.text_faint))
            .into_any_element();
        (panel, footer)
    }
}
