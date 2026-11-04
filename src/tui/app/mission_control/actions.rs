//! Side-effect actions invoked from Mission Control.
//!
//! `open` flips the app into MC mode and pre-fetches every panel's
//! data. `apply_selected` / `reject_selected` route the inbox keys
//! (`a` / `r`) through the same `RsiProposalsTool` machinery the
//! agent uses, so a UI-applied proposal is byte-identical to one
//! applied via `rsi_proposals apply <id>`.

use crate::brain::mission_control::{
    ACTIVITY_LIMIT, activity_service, analytics_service, inbox_service, schedule_service,
};
use crate::brain::tools::dynamic::DynamicToolLoader;
use crate::brain::tools::rsi_proposals::RsiProposalsTool;
use crate::tui::app::App;
use crate::tui::app::mission_control::McPanel;
use crate::tui::events::AppMode;

/// Enter Mission Control mode. Idempotent — re-opening from MC is a
/// no-op (the snapshots stay as they were; call `refresh` explicitly
/// if a re-fetch is required).
pub async fn open(app: &mut App) {
    if app.mode == AppMode::MissionControl {
        return;
    }
    app.mode = AppMode::MissionControl;
    refresh(app).await;
}

/// Re-fetch every panel's data into the cached snapshots in `McState`.
/// Called on `open` and after every `apply` / `reject` so the user
/// sees the inbox shrink immediately.
pub async fn refresh(app: &mut App) {
    app.mc.activity = activity_service::recent(ACTIVITY_LIMIT);
    let pool = app.agent_service.context().pool();
    app.mc.schedule = schedule_service::list(pool.clone()).await;
    app.mc.analytics = analytics_service::summary(pool, app.mc.analytics_window).await;
}

/// Re-fetch only the analytics snapshot through the active D/W/M/All window
/// (#900). Called after the user switches the filter so the panel re-windows
/// without re-reading the activity feed and schedule.
pub async fn refresh_analytics(app: &mut App) {
    let pool = app.agent_service.context().pool();
    app.mc.analytics = analytics_service::summary(pool, app.mc.analytics_window).await;
}

/// Apply the currently selected inbox proposal. Routes to
/// `RsiProposalsTool::apply_tool` or `apply_command` based on kind.
/// On success / failure surfaces the result via the existing
/// notification field (visible in the status bar).
pub async fn apply_selected(app: &mut App) {
    if app.mc.focused_panel != McPanel::Inbox {
        return;
    }
    let items = inbox_service::list();
    let Some(item) = items.get(app.mc.selected_index).cloned() else {
        return;
    };

    let tool = match build_proposals_tool(app) {
        Some(t) => t,
        None => {
            notify(app, "Apply failed: tools.toml path unavailable");
            return;
        }
    };

    // The kind picks the loader inside the tool, so the window's inbox can
    // call the same entry point and install identically.
    let result = tool.apply(&item.id, &item.kind);

    let msg = match result {
        Ok(s) => s,
        Err(e) => format!("Apply failed: {e}"),
    };
    notify(app, &msg);
    finalize_selection_after_action(app);
    refresh(app).await;
}

/// Reject the currently selected inbox proposal — archives without
/// installing.
pub async fn reject_selected(app: &mut App) {
    if app.mc.focused_panel != McPanel::Inbox {
        return;
    }
    let items = inbox_service::list();
    let Some(item) = items.get(app.mc.selected_index).cloned() else {
        return;
    };

    let tool = match build_proposals_tool(app) {
        Some(t) => t,
        None => {
            notify(app, "Reject failed: tools.toml path unavailable");
            return;
        }
    };

    // No reason capture today — a future C-step can wire a small
    // input prompt for this. The archive happily accepts None.
    let result = tool.reject(&item.id, None);
    let msg = match result {
        Ok(s) => s,
        Err(e) => format!("Reject failed: {e}"),
    };
    notify(app, &msg);
    finalize_selection_after_action(app);
    refresh(app).await;
}

/// After applying or rejecting an item, the inbox shrinks by one.
/// Clamp the selection so the next render lands on the row that was
/// previously below the just-removed one (or the new last row when
/// the user was at the bottom). Keeps the caret close to where the
/// user was looking.
fn finalize_selection_after_action(app: &mut App) {
    let count = inbox_service::list().len();
    if count == 0 {
        app.mc.selected_index = 0;
    } else {
        app.mc.selected_index = app.mc.selected_index.min(count - 1);
    }
}

fn build_proposals_tool(app: &App) -> Option<RsiProposalsTool> {
    let registry = app.agent_service.tool_registry().clone();
    let tools_path = DynamicToolLoader::default_path()?;
    let brain_path = app.brain_path.clone();
    Some(RsiProposalsTool::new(registry, tools_path, brain_path))
}

fn notify(app: &mut App, message: &str) {
    app.notification = Some(message.to_string());
    app.notification_shown_at = Some(std::time::Instant::now());
}
