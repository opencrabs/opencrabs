//! Parent-session project assignment is GUI wiring, so these checks read the
//! window source. The topic filter also has a real database regression test in
//! `db_repository_project_test.rs`.

use crate::tests::gui_dashboard_source::DASHBOARD;

const MENU: &str = include_str!("../gui/menu.rs");
const PROJECTS: &str = include_str!("../gui/projects.rs");
const PROJECT_REPOSITORY: &str = include_str!("../db/repository/project.rs");
const SESSIONS: &str = include_str!("../gui/sessions.rs");

fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source.split_once(start).expect("source block starts");
    let (body, _) = tail.split_once(end).expect("source block ends");
    body
}

#[test]
fn only_parent_session_cards_offer_project_assignment() {
    assert!(MENU.contains("AssignProject"));
    let entries = block(
        SESSIONS,
        "pub fn rail_menu_entries(",
        "/// What a rail row shows",
    );
    let parent_gate = entries.find("if !is_topic {").expect("parent gate");
    let action = entries
        .find("kind: MenuKind::AssignProject")
        .expect("project action");
    let label = entries
        .find("label: \"Assign project...\"")
        .expect("project label");
    assert!(parent_gate < action && action < label);

    for needle in [
        "let is_topic = self.sessions.parents.contains_key(&id);",
        "sessions::rail_menu_entries(favorite, topics_on, is_topic)",
        "sessions::MenuKind::AssignProject => {",
        "this.open_session_project_assignment(id, cx)",
        "if self.sessions.parents.contains_key(&session_id) {",
        "self.open_projects_mode(ProjectsMode::AssignSession(session_id), cx);",
    ] {
        assert!(
            DASHBOARD.contains(needle),
            "missing parent wiring: {needle}"
        );
    }
}

#[test]
fn session_picker_reuses_the_service_and_returns_after_success() {
    for needle in [
        "AssignSession(Uuid)",
        "ProjectsMode::List | ProjectsMode::AssignSession(_) => self.rows.len()",
        "ProjectsMode::List | ProjectsMode::AssignSession(_) => &mut self.selected",
        "service.assign_session(session_id, project_id).await?",
    ] {
        assert!(PROJECTS.contains(needle), "missing picker model: {needle}");
    }
    for needle in [
        "this.assign_session_to_project(id, cx);",
        "self.assign_session_to_project(project_id, cx);",
        "ProjectsMode::AssignSession(expected_session)",
        "Some((*session_id, *project_id))",
        "session.project_id = Some(project_id);",
        "ProjectsMode::AssignSession(_) => \"up/down select  enter assign  esc cancel\"",
    ] {
        assert!(
            DASHBOARD.contains(needle),
            "missing picker action: {needle}"
        );
    }

    let success = block(
        DASHBOARD,
        "Ok(data) => {\n                        this.projects.apply(data);",
        "Err(error) => this.projects.error = Some(error),",
    );
    assert!(success.contains("session.project_id = Some(project_id);"));
    assert!(success.contains("this.close_projects(cx);"));

    let escape = block(
        DASHBOARD,
        "if matches!(self.projects.mode, ProjectsMode::AssignSession(_)) {",
        "if self.projects.mode != ProjectsMode::List {",
    );
    assert!(escape.contains("self.close_projects(cx);"));
}

#[test]
fn unassigned_project_picks_exclude_topics_at_the_query_boundary() {
    assert!(
        PROJECT_REPOSITORY
            .contains("WHERE project_id IS NULL AND archived_at IS NULL AND parent_id IS NULL")
    );
}
