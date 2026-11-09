//! #2729: the desktop notification transport must decide, not guess.
//!
//! No banner is delivered here. CI has no notification daemon and the point of
//! the split is that every decision is a pure function: the config gate, the
//! focus suppression, the dedupe key and the string handling are all testable
//! without an OS transport. Delivery itself is the owner's manual test, and
//! the JSON parses below need no config file at all, so nothing in this file
//! can read or write the live config.
//!
//! Tests search the source rather than slicing it: `&SRC[a..b]` is what the
//! string-slice ratchet forbids, and these checks are about presence, not
//! offsets.

use std::collections::HashSet;

use uuid::Uuid;

use crate::config::AgentConfig;
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::desktop_notify::{
    NotificationEvent, enabled, first_time, fresh_settled_tasks, should_raise, summary_line,
    truncate, xml_escape,
};

const SOURCE: &str = include_str!("../utils/desktop_notify.rs");

fn plan_with(title: &str, statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(Uuid::new_v4(), title.to_string());
    plan.status = PlanStatus::Active;
    for (i, status) in statuses.iter().enumerate() {
        let mut task = PlanTask::new(
            i + 1,
            format!("task {}", i + 1),
            String::new(),
            TaskType::Other("test".to_string()),
        );
        task.status = status.clone();
        plan.add_task(task);
    }
    plan
}

#[test]
fn opt_out_accepts_only_the_documented_spellings() {
    for value in ["1", "true", "TRUE", "yes", "on", " on "] {
        assert!(!enabled(Some(value)), "{value:?} must disable banners");
    }
    for value in ["", "0", "false", "no", "off", "banana"] {
        assert!(enabled(Some(value)), "{value:?} must leave banners on");
    }
    assert!(enabled(None), "an unset variable means enabled");
}

#[test]
fn truncate_never_splits_a_character() {
    assert_eq!(truncate("short", 10), "short");
    let long = "á".repeat(300);
    let cut = truncate(&long, 20);
    assert_eq!(cut.chars().count(), 20, "cut on a character boundary");
    assert!(cut.ends_with('…'), "a truncated body says so");
}

#[test]
fn summary_line_takes_the_first_real_line() {
    let text = "\n\n## Done, mostly\nsecond line\n";
    assert_eq!(summary_line(text), "Done, mostly");
    assert_eq!(
        summary_line("   \n\t\n"),
        "",
        "a tool-only turn has no line"
    );
}

#[test]
fn xml_escape_covers_the_five_metacharacters() {
    assert_eq!(
        xml_escape(r#"a & b < c > d " e ' f"#),
        "a &amp; b &lt; c &gt; d &quot; e &apos; f"
    );
    assert_eq!(xml_escape("plain"), "plain");
}

#[test]
fn plan_and_task_events_dedupe_but_turns_do_not() {
    let plan = NotificationEvent::PlanCompleted {
        title: "Ship it".to_string(),
        tasks: 3,
    };
    assert_eq!(plan.dedupe_key().as_deref(), Some("plan:Ship it"));

    let task = NotificationEvent::TaskCompleted {
        label: "Write the transport".to_string(),
        success: true,
        elapsed_secs: 4,
    };
    assert_eq!(
        task.dedupe_key().as_deref(),
        Some("task:Write the transport")
    );

    // Asking the same thing twice deserves two banners, so turns carry no key.
    assert_eq!(
        NotificationEvent::TurnCompleted {
            summary: "all set".to_string()
        }
        .dedupe_key(),
        None
    );
    assert_eq!(
        NotificationEvent::TurnFailed {
            reason: "boom".to_string()
        }
        .dedupe_key(),
        None
    );
}

#[test]
fn a_key_is_recorded_only_once() {
    let key = format!("task:{}", Uuid::new_v4());
    assert!(first_time(&key), "the first sighting announces");
    assert!(!first_time(&key), "a re-render must stay silent");
}

#[test]
fn should_raise_refuses_without_consent() {
    let event = NotificationEvent::TurnFailed {
        reason: "boom".to_string(),
    };
    assert!(
        !should_raise(&event, Some("1"), true),
        "the env opt-out wins over every config switch"
    );
    assert!(
        !should_raise(&event, None, false),
        "an unset config switch means no banner"
    );
}

#[test]
fn settled_tasks_are_recorded_once_across_plans() {
    let plan = plan_with(
        "plan",
        &[
            TaskStatus::Completed,
            TaskStatus::Skipped,
            TaskStatus::Pending,
        ],
    );
    let mut seen: HashSet<Uuid> = HashSet::new();
    let fresh = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert_eq!(fresh.len(), 2, "completed and skipped both settled");

    // A skip is recorded but never reported as a success.
    let skipped = fresh
        .iter()
        .find(|(id, _, _)| *id == plan.tasks[1].id)
        .expect("the skipped task is in the fresh set");
    assert!(!skipped.2, "a skip is not a success");

    let again = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert!(again.is_empty(), "a second read announces nothing");
}

#[test]
fn the_four_switches_default_off_and_parse() {
    let default = AgentConfig::default();
    assert!(!default.notify_on_plan_completed);
    assert!(!default.notify_on_task_completed);
    assert!(!default.notify_on_turn_completed);
    assert!(!default.notify_on_turn_failed);

    let empty: AgentConfig = serde_json::from_str("{}").expect("empty agent section parses");
    assert!(!empty.notify_on_plan_completed, "absent key stays silent");

    let on: AgentConfig = serde_json::from_str(
        r#"{"notify_on_plan_completed": true, "notify_on_task_completed": true,
            "notify_on_turn_completed": true, "notify_on_turn_failed": true}"#,
    )
    .expect("explicit keys parse");
    assert!(on.notify_on_plan_completed);
    assert!(on.notify_on_task_completed);
    assert!(on.notify_on_turn_completed);
    assert!(on.notify_on_turn_failed);
}

#[test]
fn focus_suppression_and_dedupe_are_wired_into_the_transport() {
    // Structural checks on the source: the transport must keep a focus state,
    // an announced set, and must consult both before dispatching. A behaviour
    // test would have to mutate process-wide statics and race the suite.
    for needle in [
        "static SURFACE_FOCUS",
        "static ANNOUNCED",
        "pub fn set_surface_focused",
        "fn should_raise",
        "first_time",
    ] {
        assert!(SOURCE.contains(needle), "desktop_notify.rs lost {needle:?}");
    }
}
