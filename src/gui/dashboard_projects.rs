//! Project-screen actions and keyboard routing for the dashboard.

use super::*;
use crate::projects::{ProjectAction, ProjectsMode};

impl Dashboard {
    /// `/projects` and alt+p open the same service-backed project list.
    pub(crate) fn open_projects(&mut self, cx: &mut Context<Self>) {
        self.open_projects_mode(ProjectsMode::List, cx);
    }

    /// A top-level session owns its project. Topics inside it share that
    /// assignment, so only the parent session can open this picker.
    pub(crate) fn open_session_project_assignment(
        &mut self,
        session_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.parents.contains_key(&session_id) {
            return;
        }
        self.open_projects_mode(ProjectsMode::AssignSession(session_id), cx);
    }

    fn open_projects_mode(&mut self, mode: ProjectsMode, cx: &mut Context<Self>) {
        self.onboard = None;
        self.screen = Screen::Projects;
        self.sessions.rail_menu = None;
        self.projects.mode = mode;
        self.projects.sessions.clear();
        self.projects.unassigned.clear();
        self.projects.selected = 0;
        self.projects.session_selected = 0;
        self.projects.creating = false;
        self.projects.create_input.clear();
        self.projects.delete_confirm = false;
        self.projects.notice = None;
        self.projects.directory = opencrabs::services::ProjectService::projects_dir();
        self.request_projects(ProjectAction::Refresh { detail: None }, cx);
        self.refocus_screen();
        cx.notify();
    }

    pub(crate) fn close_projects(&mut self, cx: &mut Context<Self>) {
        self.projects.next_generation();
        self.projects.loading = false;
        self.projects.creating = false;
        self.projects.delete_confirm = false;
        self.screen = Screen::Chat;
        self.refocus_screen();
        cx.notify();
    }

    /// Escape resolves the nearest surface before leaving the screen.
    pub(crate) fn projects_escape(&mut self, cx: &mut Context<Self>) {
        if self.projects.creating {
            self.projects.creating = false;
            self.projects.create_input.clear();
            cx.notify();
            return;
        }
        if self.projects.delete_confirm {
            self.projects.delete_confirm = false;
            cx.notify();
            return;
        }
        if matches!(self.projects.mode, ProjectsMode::AssignSession(_)) {
            self.close_projects(cx);
            return;
        }
        if self.projects.mode != ProjectsMode::List {
            self.projects.next_generation();
            self.projects.loading = false;
            self.projects.mode = ProjectsMode::List;
            self.projects.sessions.clear();
            self.projects.unassigned.clear();
            self.projects.session_selected = 0;
            cx.notify();
            return;
        }
        self.close_projects(cx);
    }

    /// Only the newest generation may paint, so stale reads cannot overwrite
    /// a newer action or a screen the user already left.
    pub(crate) fn request_projects(&mut self, action: ProjectAction, cx: &mut Context<Self>) {
        self.projects.loading = true;
        self.projects.error = None;
        self.projects.notice = None;
        self.projects.next_generation();
        let generation = self.projects.generation;
        let db_path = self.db_path.clone();
        let session_assignment = match (self.projects.mode, &action) {
            (
                ProjectsMode::AssignSession(expected_session),
                ProjectAction::Assign {
                    project_id,
                    session_id,
                },
            ) if expected_session == *session_id => Some((*session_id, *project_id)),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let result = crate::projects::run(db_path, action).await;
            let updated = this.update(cx, |this, cx| {
                if generation != this.projects.generation {
                    return;
                }
                this.projects.loading = false;
                match result {
                    Ok(data) => {
                        this.projects.apply(data);
                        if let Some((session_id, project_id)) = session_assignment {
                            if let Some(session) = this
                                .sessions
                                .items
                                .iter_mut()
                                .find(|session| session.id == session_id)
                            {
                                session.project_id = Some(project_id);
                            }
                            this.close_projects(cx);
                        }
                    }
                    Err(error) => this.projects.error = Some(error),
                }
                cx.notify();
            });
            if updated.is_err() {
                logging::error("projects result arrived after the window closed");
            }
        })
        .detach();
    }

    pub(crate) fn open_project(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.projects.creating = false;
        self.projects.delete_confirm = false;
        self.projects.mode = ProjectsMode::Detail(id);
        self.projects.sessions.clear();
        self.projects.session_selected = 0;
        self.request_projects(ProjectAction::Refresh { detail: Some(id) }, cx);
        cx.notify();
    }

    pub(crate) fn begin_project_create(&mut self, cx: &mut Context<Self>) {
        self.projects.delete_confirm = false;
        self.projects.creating = true;
        self.projects.create_input.clear();
        cx.notify();
    }

    pub(crate) fn begin_project_assign(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.projects.creating = false;
        self.projects.delete_confirm = false;
        self.projects.mode = ProjectsMode::Assign(id);
        self.projects.sessions.clear();
        self.projects.unassigned.clear();
        self.projects.session_selected = 0;
        self.request_projects(ProjectAction::Refresh { detail: Some(id) }, cx);
        cx.notify();
    }

    pub(crate) fn request_project_delete(&mut self, cx: &mut Context<Self>) {
        self.projects.creating = false;
        self.projects.delete_confirm = true;
        cx.notify();
    }

    fn submit_project_create(&mut self, cx: &mut Context<Self>) {
        let name = self.projects.create_input.trim().to_string();
        if name.is_empty() {
            self.projects.error = Some("Project name cannot be empty".to_string());
        } else {
            self.projects.creating = false;
            self.request_projects(ProjectAction::Create { name }, cx);
        }
        cx.notify();
    }

    pub(crate) fn confirm_project_delete(&mut self, cx: &mut Context<Self>) {
        let id = self
            .projects
            .mode
            .project_id()
            .or_else(|| self.projects.selected_project().map(|project| project.id));
        self.projects.delete_confirm = false;
        if let Some(id) = id {
            self.projects.mode = ProjectsMode::List;
            self.request_projects(ProjectAction::Delete { id }, cx);
        }
        cx.notify();
    }

    pub(crate) fn assign_project_session(
        &mut self,
        project_id: Uuid,
        session_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        self.projects.mode = ProjectsMode::Detail(project_id);
        self.request_projects(
            ProjectAction::Assign {
                project_id,
                session_id,
            },
            cx,
        );
        cx.notify();
    }

    pub(crate) fn assign_session_to_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        let ProjectsMode::AssignSession(session_id) = self.projects.mode else {
            return;
        };
        self.request_projects(
            ProjectAction::Assign {
                project_id,
                session_id,
            },
            cx,
        );
        cx.notify();
    }

    /// Bare keys belong to this screen and never leak into the composer.
    pub(crate) fn projects_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.key == "escape" {
            self.projects_escape(cx);
            return;
        }
        if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
            return;
        }
        if self.projects.creating {
            match ks.key.as_str() {
                "enter" => self.submit_project_create(cx),
                "backspace" => {
                    self.projects.create_input.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_deref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.projects.create_input.push(ch);
                        cx.notify();
                    }
                }
            }
            return;
        }
        if self.projects.delete_confirm {
            if matches!(ks.key.as_str(), "d" | "enter") {
                self.confirm_project_delete(cx);
            }
            return;
        }
        if self.projects.loading {
            return;
        }
        match self.projects.mode {
            ProjectsMode::List => match ks.key.as_str() {
                "up" => self.projects.move_selection(-1),
                "down" => self.projects.move_selection(1),
                "enter" => {
                    if let Some(id) = self.projects.selected_project().map(|project| project.id) {
                        self.open_project(id, cx);
                    }
                }
                "n" => self.begin_project_create(cx),
                "a" => {
                    if let Some(id) = self.projects.selected_project().map(|project| project.id) {
                        self.begin_project_assign(id, cx);
                    }
                }
                "d" => self.request_project_delete(cx),
                _ => {}
            },
            ProjectsMode::Detail(project_id) => match ks.key.as_str() {
                "up" => self.projects.move_selection(-1),
                "down" => self.projects.move_selection(1),
                "enter" => {
                    if let Some(id) = self.projects.selected_session().map(|session| session.id) {
                        self.open_session(id, cx);
                    }
                }
                "a" => self.begin_project_assign(project_id, cx),
                "d" => self.request_project_delete(cx),
                "u" => {
                    if let Some(session_id) =
                        self.projects.selected_session().map(|session| session.id)
                    {
                        self.request_projects(
                            ProjectAction::Unassign {
                                project_id,
                                session_id,
                            },
                            cx,
                        );
                    }
                }
                _ => {}
            },
            ProjectsMode::Assign(project_id) => match ks.key.as_str() {
                "up" => self.projects.move_selection(-1),
                "down" => self.projects.move_selection(1),
                "enter" => {
                    if let Some(session_id) = self
                        .projects
                        .selected_assignment(project_id)
                        .map(|session| session.id)
                    {
                        self.assign_project_session(project_id, session_id, cx);
                    }
                }
                _ => {}
            },
            ProjectsMode::AssignSession(_) => match ks.key.as_str() {
                "up" => self.projects.move_selection(-1),
                "down" => self.projects.move_selection(1),
                "enter" => {
                    if let Some(project_id) =
                        self.projects.selected_project().map(|project| project.id)
                    {
                        self.assign_session_to_project(project_id, cx);
                    }
                }
                _ => {}
            },
        }
        cx.notify();
    }
}
