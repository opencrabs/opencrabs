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
        "Screen::Projects => self.projects_screen(cx)",
        "Screen::Projects => right.child(footer)",
        "projects: projects::ProjectsState::default()",
        "self.projects_key(ev, cx)",
        "self.projects_escape(cx)",
    ] {
        assert!(DASHBOARD.contains(needle), "missing screen wiring: {needle}");
    }
    for needle in [
        "ProjectsMode::List",
        "ProjectsMode::Detail",
        "ProjectsMode::Assign",
        "projects_view::project_card",
        "projects_view::session_card",
        "projects_view::create_panel",
        "projects_view::delete_panel",
        "concentric_radius(18.0)",
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
    let detail = DASHBOARD
        .find("if self.projects.mode != ProjectsMode::List {")
        .expect("Escape returns from detail or assignment next");
    let close = DASHBOARD
        .find("self.close_projects(cx);")
        .expect("Escape leaves the list last");
    assert!(creating < deleting && deleting < detail && detail < close);
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
        rows.iter()
            .filter(|row| row.title == "/projects")
            .count()
            == 1,
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
