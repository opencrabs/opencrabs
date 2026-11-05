//! Read-only presentation of a durably completed plan, shared by frontends.

use crate::tui::plan::PlanDocument;

pub const COMPLETED_PLAN_FOOTER: &str = "Plan completed and archived.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPlanTaskView {
    pub order: usize,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPlanView {
    pub title: String,
    pub tasks: Vec<CompletedPlanTaskView>,
}

impl CompletedPlanView {
    pub fn new(plan: &PlanDocument) -> Self {
        Self {
            title: plan.title.clone(),
            tasks: plan
                .tasks
                .iter()
                .map(|task| CompletedPlanTaskView {
                    order: task.order,
                    title: task.title.clone(),
                })
                .collect(),
        }
    }

    pub fn header(&self) -> String {
        let suffix = if self.tasks.len() == 1 {
            "task"
        } else {
            "tasks"
        };
        format!(
            "Plan · {} · {} {} · 100%",
            self.title,
            self.tasks.len(),
            suffix
        )
    }

    /// Human text for copy actions, never the stored JSON representation.
    pub fn copy_text(&self) -> String {
        let mut lines = vec![format!("☑ {}", self.header())];
        lines.extend(
            self.tasks
                .iter()
                .map(|task| format!("  ☑ {}. {}", task.order, task.title)),
        );
        lines.push(COMPLETED_PLAN_FOOTER.to_string());
        lines.join("\n")
    }
}
