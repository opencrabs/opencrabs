//! Regression pins for #2669. The window has no headless GPUI harness, so its
//! wiring is read as source while the shared command, Help and Seeker catalogs
//! are exercised directly.

use crate::tests::gui_dashboard_source::DASHBOARD;

const PROJECTS: &str = include_str!("../gui/projects.rs");
const PROJECTS_SCREEN: &str = include_str!("../gui/projects_screen.rs");
const PROJECTS_VIEW: &str = include_str!("../gui/projects_view.rs");
const GUI_COMMANDS: &str = include_str!("../gui/commands.rs");
const TUI_INPUT: &str = include_str!("../tui/app/input.rs");
const TUI_MESSAGING: &str = include_str!("../tui/app/messaging.rs");

#[test]
fn projects_is_a_real_service_backed_dashboard_screen() {
    for needle in [
        "mod projects;",
        "mod projects_screen;",
        "mod projects_view;",
        "projects: projects::ProjectsState",
        "Screen::Projects => (div().flex_1().into_any_element(), div().into_any_element())",
        "Screen::Projects => right.child(footer)",
        "projects: projects::ProjectsState::default()",
        "self.projects_key(ev, cx)",
        "self.projects_escape(cx)",
    ] {
        assert!(
            DASHBOARD.contains(needle),
            "missing screen wiring: {needle}"
        );
    }
    for needle in [
        "ProjectsMode::List",
        "ProjectsMode::Detail",
        "ProjectsMode::Assign",
        "projects_view::project_card",
        "projects_view::session_card",
        "projects_view::create_panel",
        "projects_view::delete_panel",
        "modal_shell::render(",
        "let (body, footer) = self.projects_screen(cx);",
        "id: \"projects-modal\"",
    ] {
        assert!(
            PROJECTS_SCREEN.contains(needle),
            "missing projects render path: {needle}"
        );
    }
    assert!(PROJECTS_VIEW.contains("glass_fill"));
    assert!(PROJECTS_VIEW.contains("token_count(session.token_count)"));
}

#[test]
fn projects_commands_live_in_the_footer_and_delete_is_neutral_at_rest() {
    assert!(PROJECTS_SCREEN.contains("actions: None"));
    assert!(PROJECTS_SCREEN.contains("header_close: false"));
    assert!(PROJECTS_SCREEN.contains(
        "ProjectsMode::List => \"up/down select  enter open  esc close\""
    ));
    let modal = PROJECTS_SCREEN
        .find("modal_shell::render(")
        .expect("the modal shell renders Projects");
    let commands = PROJECTS_SCREEN
        .rfind("let mut actions =")
        .expect("the footer builds the command actions");
    let footer = PROJECTS_SCREEN
        .rfind("let footer = div()")
        .expect("the footer lays out summary and commands");
    assert!(modal < commands && commands < footer);

    for needle in [
        "action_button(\"project-new\", \"new n\", false, shown)",
        "action_button(\"project-assign\", \"assign a\", false, shown)",
        "action_button(\"project-delete\", \"delete d\", false, shown)",
        "\"confirm delete\",\n                true,",
    ] {
        assert!(
            PROJECTS_SCREEN.contains(needle),
            "missing command styling: {needle}"
        );
    }
}

#[test]
fn assignment_can_repair_sessions_from_other_projects() {
    assert!(PROJECTS.contains("pub fn visible_assignments(&self, target: Uuid)"));
    assert!(PROJECTS.contains("AssignmentFilter::Project(id) => session.project_id == Some(id)"));
    assert!(PROJECTS_SCREEN.contains("Current project: {membership}"));
    assert!(PROJECTS_SCREEN.contains("assignment_rows.iter().enumerate()"));
    let repository = include_str!("../db/repository/project.rs");
    assert!(repository.contains("pub async fn find_assignable_sessions(&self)"));
    assert!(repository.contains("archived_at IS NULL AND parent_id IS NULL"));
}

#[test]
fn bulk_assignment_clear_and_archive_keep_their_own_service_paths() {
    for needle in [
        "AssignMany { project_id: Uuid, session_ids: Vec<Uuid> }",
        "ClearMany { project_id: Uuid, session_ids: Vec<Uuid> }",
        "ArchiveMany { project_id: Uuid, session_ids: Vec<Uuid> }",
        "service.assign_session(*session_id, project_id).await?",
        "service.unassign_session(*session_id).await?",
        "sessions.archive(*session_id).await?",
    ] {
        assert!(PROJECTS.contains(needle), "missing bulk service path: {needle}");
    }
    assert!(PROJECTS_SCREEN.contains("this.projects.toggle_assignment(project_id, id)"));
    assert!(PROJECTS_SCREEN.contains("project-bulk-archive-confirm"));
    assert!(DASHBOARD.contains("self.assign_selected_sessions(project_id, cx)"));
    assert!(DASHBOARD.contains("self.clear_selected_projects(project_id, cx)"));
    assert!(!PROJECTS.contains("session_merge"));
}

#[test]
fn project_actions_use_the_shared_service_and_reject_stale_results() {
    for needle in [
        "Database::connect(&db_path).await?",
        "ProjectService::new(ServiceContext::new(db.pool().clone()))",
        "service.list_projects().await?",
        "service.create_project(name, None).await?",
        "service.delete_project(id).await?",
        "service.assign_session(session_id, project_id).await?",
        "service.unassign_session(session_id).await?",
        "service.get_sessions_for_project(id).await?",
        "service.get_unassigned_sessions().await?",
        "service.get_assignable_sessions().await?",
        ".stack_size(4 * 1024 * 1024)",
        ".name(\"gui-projects\".into())",
    ] {
        assert!(PROJECTS.contains(needle), "missing service path: {needle}");
    }
    for needle in [
        "crate::projects::run(db_path, action).await",
        "if generation != this.projects.generation",
        "this.projects.apply(data)",
        "self.projects.next_generation()",
    ] {
        assert!(
            DASHBOARD.contains(needle),
            "missing async generation guard: {needle}"
        );
    }
}

#[test]
fn project_rows_keep_service_order_and_escape_unwinds_the_nearest_surface() {
    let list = PROJECTS
        .find("let projects = service.list_projects().await?;")
        .expect("the worker reads the service list");
    let iterate = PROJECTS
        .find("for project in projects {")
        .expect("rows iterate that list directly");
    let stats = PROJECTS
        .find("service.get_project_stats(project.id).await?")
        .expect("each row loads its own counts");
    let push = PROJECTS
        .find("rows.push(ProjectRow {")
        .expect("the same project and counts form one row");
    assert!(list < iterate && iterate < stats && stats < push);
    assert!(PROJECTS.contains("self.rows = data.rows;"));
    assert!(PROJECTS.contains("self.clamp();"));

    let creating = DASHBOARD
        .find("if self.projects.creating {")
        .expect("Escape closes project creation first");
    let deleting = DASHBOARD
        .find("if self.projects.delete_confirm {")
        .expect("Escape closes delete confirmation next");
    let assign = DASHBOARD
        .find("if matches!(self.projects.mode, ProjectsMode::AssignSession(_)) {")
        .expect("Escape leaves assignment back to the list");
    let detail = DASHBOARD
        .find("if self.projects.mode != ProjectsMode::List {")
        .expect("Escape returns from detail next");
    // The last call is the one that leaves the list: the assignment arm above
    // closes the screen too, so a plain find would land on that one instead.
    let close = DASHBOARD
        .rfind("self.close_projects(cx);")
        .expect("Escape leaves the list last");
    assert!(creating < deleting && deleting < assign && assign < detail && detail < close);
}

#[test]
fn project_counts_and_dates_use_precise_labels() {
    assert!(PROJECTS_VIEW.contains("if row.sessions == 1 { \"\" } else { \"s\" }"));
    assert!(PROJECTS_VIEW.contains("if row.files == 1 { \"\" } else { \"s\" }"));
    assert!(PROJECTS_VIEW.contains("format!(\"created {}\", project.created_at.format"));
    assert!(!PROJECTS_VIEW.contains("{} sessions  {} files"));
    assert!(PROJECTS_SCREEN.contains("\"{count} {noun}{}  ·  {hints}\""));
    assert!(PROJECTS_SCREEN.contains("Choose sessions for {}"));
}

#[test]
fn projects_command_reaches_both_clients_and_seeker() {
    use crate::ui_core::commands::{GUI_SUPPORTED, SLASH_COMMANDS};
    use crate::ui_core::palette::{PaletteAction, Surface, is_surface_command, surface_items};

    assert!(
        SLASH_COMMANDS
            .iter()
            .any(|command| command.name == "/projects")
    );
    assert!(GUI_SUPPORTED.contains(&"/projects"));
    assert!(GUI_COMMANDS.contains("\"/projects\" => GuiAction::OpenProjects"));
    assert!(DASHBOARD.contains("commands::GuiAction::OpenProjects => self.open_projects(cx)"));
    assert!(DASHBOARD.contains("find::Surface::Projects => self.open_projects(cx)"));
    assert!(TUI_MESSAGING.contains("\"/projects\" => {"));
    assert!(TUI_MESSAGING.contains("self.open_projects_view().await"));
    assert!(TUI_INPUT.contains("self.open_projects_view().await?"));

    assert_eq!(Surface::Projects.command(), Some("/projects"));
    assert!(is_surface_command("/projects"));
    let rows = surface_items();
    let row = rows
        .iter()
        .find(|row| row.action == PaletteAction::OpenSurface(Surface::Projects))
        .expect("Seeker lists the Projects destination");
    assert_eq!(row.title, "/projects");
    assert!(
        rows.iter().filter(|row| row.title == "/projects").count() == 1,
        "the command and its destination must not duplicate the Seeker row"
    );
}

#[test]
fn gui_help_and_seeker_expose_the_projects_chord() {
    use crate::ui_core::help_keys::{HelpClient, ROWS, seeker_items};
    use crate::ui_core::palette::{PaletteAction, Surface};

    let row = ROWS
        .iter()
        .find(|row| row.description == "Browse projects")
        .expect("the shared Help table lists Projects");
    assert_eq!(row.tui, Some("P"));
    assert_eq!(row.gui, Some("alt-p"));

    let items = seeker_items(HelpClient::Gui);
    let help = items
        .iter()
        .find(|item| item.title == "/help Browse projects")
        .expect("Seeker includes the GUI Projects Help row");
    assert!(help.subtitle.contains("alt-p"));
    assert_eq!(help.action, PaletteAction::OpenSurface(Surface::Help));
}
