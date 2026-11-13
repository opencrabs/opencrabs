//! Project content inside the shared window-level modal.

use super::*;
use crate::projects::{AssignmentFilter, ProjectsMode};

impl Dashboard {
    pub(crate) fn projects_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            ProjectsMode::Assign(_) => "Assign sessions".to_string(),
            ProjectsMode::AssignSession(_) => "Assign project".to_string(),
        };
        let subtitle = match mode {
            ProjectsMode::List => "Group related sessions and their tracked files".to_string(),
            ProjectsMode::Detail(_) => project
                .as_ref()
                .and_then(|project| project.description.clone())
                .unwrap_or_else(|| "Sessions in this project".to_string()),
            ProjectsMode::Assign(_) => format!(
                "Choose sessions for {}",
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
                    projects_view::action_button("project-new", "new", false, palette::current())
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.begin_project_create(cx)
                        })),
                );
                if self.projects.selected_project().is_some() {
                    actions = actions.child(
                        projects_view::action_button(
                            "project-assign",
                            "assign",
                            false,
                            palette::current(),
                        )
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            if let Some(id) =
                                this.projects.selected_project().map(|project| project.id)
                            {
                                this.begin_project_assign(id, cx);
                            }
                        })),
                    );
                    actions = actions.child(
                        projects_view::action_button(
                            "project-delete",
                            "delete",
                            true,
                            palette::current(),
                        )
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.request_project_delete(cx)
                        })),
                    );
                }
            }
            ProjectsMode::Detail(project_id) => {
                actions = actions.child(
                    projects_view::action_button("project-back", "back", false, palette::current())
                        .on_click(cx.listener(|this, _ev, _window, cx| this.projects_escape(cx))),
                );
                actions = actions.child(
                    projects_view::action_button(
                        "project-detail-assign",
                        "assign",
                        false,
                        palette::current(),
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.begin_project_assign(project_id, cx)
                    })),
                );
            }
            ProjectsMode::Assign(_) | ProjectsMode::AssignSession(_) => {
                actions = actions.child(
                    projects_view::action_button(
                        "project-assign-back",
                        "back",
                        false,
                        palette::current(),
                    )
                    .on_click(cx.listener(|this, _ev, _window, cx| this.projects_escape(cx))),
                );
            }
        }
        let (body, footer) = self.projects_screen(cx);
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.close_projects(cx);
        });
        modal_shell::render(
            modal_shell::ModalView {
                id: "projects-modal",
                title,
                subtitle,
                actions: Some(actions.into_any_element()),
                body,
                footer: Some(footer),
                width: 760.0,
                max_height: 650.0,
            },
            dismiss,
        )
        .into_any_element()
    }

    pub(crate) fn projects_screen(&mut self, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let shown = palette::current();
        let mode = self.projects.mode;
        let project = mode
            .project_id()
            .and_then(|id| self.projects.project(id))
            .cloned();
        let mut content = div()
            .id("projects-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .h(px(410.0))
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
            let cancel =
                projects_view::action_button("project-delete-cancel", "cancel", false, shown)
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
            .on_click(cx.listener(|this, _ev, _window, cx| this.confirm_project_delete(cx)));
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
        if let ProjectsMode::Assign(project_id) = mode {
            let selected_count = self.projects.selected_sessions.len();
            if selected_count > 0 {
                let mut bulk = div().flex().flex_row().flex_wrap().gap_2();
                bulk = bulk.child(
                    projects_view::action_button("project-bulk-assign", "assign selected", false, shown)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.assign_selected_sessions(project_id, cx);
                        })),
                );
                bulk = bulk.child(
                    projects_view::action_button("project-bulk-clear", "clear project", false, shown)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.clear_selected_projects(project_id, cx);
                        })),
                );
                bulk = bulk.child(
                    projects_view::action_button("project-bulk-archive", "archive", true, shown)
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.request_selected_archive(cx);
                        })),
                );
                content = content.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(text(
                            format!("{selected_count} selected"),
                            Size::Xs,
                            shown.accent,
                        ))
                        .child(bulk),
                );
            }
            if self.projects.archive_confirm {
                content = content.child(projects_view::archive_panel(selected_count, shown));
                content = content.child(
                    projects_view::action_button(
                        "project-bulk-archive-confirm",
                        "confirm archive",
                        true,
                        shown,
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.confirm_selected_archive(project_id, cx);
                    })),
                );
            }
        }
        let assignment_rows = match mode {
            ProjectsMode::Assign(project_id) => self.projects.visible_assignments(project_id),
            _ => Vec::new(),
        };
        if let ProjectsMode::Assign(_) = mode {
            let mut filters = div().flex().flex_row().flex_wrap().gap_2();
            for (filter, id, label) in [
                (AssignmentFilter::All, "all".to_string(), "All".to_string()),
                (
                    AssignmentFilter::Unassigned,
                    "unassigned".to_string(),
                    "Unassigned".to_string(),
                ),
            ] {
                let selected = self.projects.assignment_filter == filter;
                filters = filters.child(
                    projects_view::filter_button(
                        format!("project-filter-{id}"),
                        label,
                        selected,
                        shown,
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.projects.assignment_filter = filter;
                        this.projects.session_selected = 0;
                        cx.notify();
                    })),
                );
            }
            for row in &self.projects.rows {
                let filter = AssignmentFilter::Project(row.project.id);
                let selected = self.projects.assignment_filter == filter;
                filters = filters.child(
                    projects_view::filter_button(
                        format!("project-filter-{}", row.project.id),
                        row.project.name.clone(),
                        selected,
                        shown,
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.projects.assignment_filter = filter;
                        this.projects.session_selected = 0;
                        cx.notify();
                    })),
                );
            }
            content = content.child(filters);
        }
        let count = match mode {
            ProjectsMode::List | ProjectsMode::AssignSession(_) => self.projects.rows.len(),
            ProjectsMode::Detail(_) => self.projects.sessions.len(),
            ProjectsMode::Assign(_) => assignment_rows.len(),
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
                        "No sessions available",
                        "every visible session already belongs to this project",
                    ),
                    ProjectsMode::AssignSession(_) => {
                        ("No projects yet", "open /projects to create one first")
                    }
                }
            };
            content = content.child(projects_view::empty(message, hint, shown));
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
                    for (index, session) in assignment_rows.iter().enumerate() {
                        let id = session.id;
                        let checked = self.projects.selected_sessions.contains(&id);
                        let membership = session
                            .project_id
                            .and_then(|project_id| self.projects.project(project_id))
                            .map(|project| project.name.clone())
                            .unwrap_or_else(|| "Unassigned".to_string());
                        let card = projects_view::session_card(
                            session,
                            index == self.projects.session_selected,
                            true,
                            shown,
                        )
                        .child(text(
                            format!("{} Current project: {membership}", if checked { "[x]" } else { "[ ]" }),
                            Size::Xs,
                            shown.amber,
                        ))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.projects.session_selected = index;
                            this.projects.toggle_assignment(project_id, id);
                            cx.notify();
                        }));
                        content = content.child(card);
                    }
                }
            }
        }
        let hints = match mode {
            ProjectsMode::List => "up/down select  enter open  n new  a assign  d delete  esc close",
            ProjectsMode::Detail(_) => "up/down select  enter open  a assign  u unassign  esc back",
            ProjectsMode::Assign(_) => {
                "space check  enter assign  c clear project  d archive  esc back"
            }
            ProjectsMode::AssignSession(_) => "up/down select  enter assign  esc cancel",
        };
        let footer = modal_shell::footer(format!("{count} rows  ·  {hints}")).into_any_element();
        (content.into_any_element(), footer)
    }
}
