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
    ACTIVATION_SCHEME, MAX_PENDING_ACTIVATIONS, NotificationEvent, activation_from_arg,
    bannerable_settles, enabled, first_time, fresh_settled_tasks, post_activation,
    post_activation_from_args, should_raise, summary_line, take_activation, toast_xml, truncate,
    xml_escape,
};

const SOURCE: &str = include_str!("../utils/desktop_notify.rs");

/// The window's poll tick, where a click is taken off the queue.
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");

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
fn one_finish_keeps_one_key_and_turns_carry_none() {
    // #2752: a plan finishing and its last task settling are ONE piece of
    // news. The plan event owns the key, so the second sighting of that
    // finish is suppressed instead of becoming a second card.
    let plan = NotificationEvent::PlanCompleted {
        title: "Ship it".to_string(),
        tasks: 3,
        session: None,
    };
    let key = format!("plan:Ship it {}", Uuid::new_v4());
    assert!(
        key.starts_with("plan:"),
        "the plan event owns the finish key"
    );
    assert!(first_time(&key), "the first sighting announces the finish");
    assert!(!first_time(&key), "a re-render must not announce it again");
    assert_eq!(
        plan.dedupe_key().as_deref(),
        Some("plan:Ship it"),
        "the plan event's key is the one the finish is remembered by"
    );

    // Asking the same thing twice deserves two banners, so turns carry no key.
    assert_eq!(
        NotificationEvent::TurnCompleted {
            summary: "all set".to_string(),
            session: None,
        }
        .dedupe_key(),
        None
    );
    assert_eq!(
        NotificationEvent::TurnFailed {
            reason: "boom".to_string(),
            session: None,
        }
        .dedupe_key(),
        None
    );
}

#[test]
fn a_finished_plan_raises_one_banner_not_two() {
    // The plan's last task settles in the same pass the plan finishes. The
    // plan banner carries that news, so the task banner for that pass stands
    // down (#2752).
    let plan = plan_with("Ship it", &[TaskStatus::Completed, TaskStatus::Completed]);
    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(plan.tasks[0].id);
    let fresh = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert_eq!(fresh.len(), 1, "only the last task is fresh");

    assert!(
        bannerable_settles(&plan, &fresh, true).is_empty(),
        "one banner per finished plan: the plan banner already says it"
    );

    // With plan banners off, the task banner is the only one left.
    assert_eq!(
        bannerable_settles(&plan, &fresh, false).len(),
        1,
        "the task switch still speaks when the plan switch is off"
    );
}

#[test]
fn a_plan_owing_only_its_delivery_step_counts_as_finished() {
    // The trailing delivery step is not real work (#737), so the pass that
    // settles everything before it is the pass the plan finishes.
    let plan = plan_with("Ship it", &[TaskStatus::Completed, TaskStatus::Pending]);
    assert!(plan.finishes_on_this_settle());

    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(plan.tasks[0].id);
    let fresh = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert!(
        bannerable_settles(&plan, &fresh, true).is_empty(),
        "the delivery step does not earn the finish a second banner"
    );
}

#[test]
fn a_task_settling_mid_plan_keeps_its_own_banner() {
    // Two tasks still owe work, so this settle is not a finish and the task
    // switch is exactly what should speak.
    let plan = plan_with(
        "Ship it",
        &[
            TaskStatus::Completed,
            TaskStatus::Pending,
            TaskStatus::Pending,
        ],
    );
    assert!(!plan.finishes_on_this_settle());

    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(plan.tasks[0].id);
    let fresh = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert_eq!(fresh.len(), 1);
    assert_eq!(
        bannerable_settles(&plan, &fresh, true).len(),
        1,
        "a task that settles on its own still announces itself"
    );
}

#[test]
fn a_trailing_task_with_criteria_is_real_work_not_a_finish() {
    // #1932: a criteria contract on the last task means the plan is not done
    // when the task before it settles.
    let mut plan = plan_with("Ship it", &[TaskStatus::Completed, TaskStatus::Pending]);
    plan.tasks[1]
        .acceptance_criteria
        .push("cargo test reports 0 failed".to_string());
    assert!(!plan.finishes_on_this_settle());

    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(plan.tasks[0].id);
    let fresh = fresh_settled_tasks(&mut seen, &[Some(&plan)]);
    assert_eq!(
        bannerable_settles(&plan, &fresh, true).len(),
        1,
        "the settled task is not the finish, so it keeps its banner"
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
        session: None,
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

#[test]
fn every_banner_names_the_session_it_is_about() {
    // #2751: a click on the banner has to land on the conversation that
    // finished. The id rides on the event itself so the transport never looks
    // it up, and a variant that forgot it would ship a banner going nowhere.
    let session = Uuid::new_v4();
    let events = [
        NotificationEvent::PlanCompleted {
            title: "Ship it".to_string(),
            tasks: 3,
            session: Some(session),
        },
        NotificationEvent::TaskCompleted {
            label: "write the test".to_string(),
            success: true,
            elapsed_secs: 12,
            session: Some(session),
        },
        NotificationEvent::TurnCompleted {
            summary: "all set".to_string(),
            session: Some(session),
        },
        NotificationEvent::TurnFailed {
            reason: "boom".to_string(),
            session: Some(session),
        },
    ];
    for event in &events {
        assert_eq!(
            event.session(),
            Some(session),
            "{:?} lost the session it is about",
            event.title()
        );
    }

    // A banner with nothing to open stays informational and invents nothing.
    let bare = NotificationEvent::TurnCompleted {
        summary: "all set".to_string(),
        session: None,
    };
    assert_eq!(bare.session(), None);
}

#[test]
fn a_click_waits_for_the_window_and_the_queue_stays_bounded() {
    // The transport runs off the UI thread, so a click cannot call the window:
    // it is queued and drained on the surface's own tick (#2751). One test for
    // the whole static, because two tests posting into the same queue would
    // race each other under the parallel harness.
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    post_activation(first);
    post_activation(second);
    assert_eq!(
        take_activation(),
        Some(first),
        "the oldest click is taken first: the person clicks the banner in front of them"
    );
    assert_eq!(take_activation(), Some(second));
    assert_eq!(take_activation(), None, "the queue empties");

    // A window nobody looks at must not turn every banner into a leaked id.
    for _ in 0..(MAX_PENDING_ACTIVATIONS + 4) {
        post_activation(Uuid::new_v4());
    }
    let mut taken = 0;
    while take_activation().is_some() {
        taken += 1;
    }
    assert_eq!(
        taken, MAX_PENDING_ACTIVATIONS,
        "the queue caps at its bound instead of growing"
    );
}

#[test]
fn a_settled_task_banner_points_at_its_plan() {
    // The session comes off the plan file, which is written by the turn that
    // owns the conversation. A plan from before the hook stamped its session
    // carries the nil id, and the nil id is not a click target (#2751).
    assert!(
        SOURCE.contains("(plan.session_id != Uuid::nil()).then_some(plan.session_id)"),
        "a settled task must take its session from the plan"
    );
}

#[test]
fn a_click_has_a_way_back_on_linux() {
    // `notify-send` reports a click as the action name on stdout, and only
    // when it is still alive to hear it: `--wait` is the difference between a
    // clickable banner and a banner that leaves nothing behind (#2751).
    for needle in [
        "--action",
        "--wait",
        "ACTION_OPEN",
        "post_activation(session)",
        "stdout(Stdio::piped())",
    ] {
        assert!(
            SOURCE.contains(needle),
            "the linux transport lost {needle:?}"
        );
    }
}

#[test]
fn the_window_takes_a_click_and_drops_a_dead_session() {
    // The drain sits above the empty-events early return: an idle window is
    // exactly when a banner gets clicked, so a click must not wait for the
    // next turn to arrive (#2751).
    let drain = DASHBOARD_TURNS
        .find("take_activation()")
        .expect("the poll tick drains the click queue");
    let early_return = DASHBOARD_TURNS
        .find("if events.is_empty()")
        .expect("the poll tick still has its idle path");
    assert!(
        drain < early_return,
        "the click is drained before the idle path returns"
    );
    for needle in [
        "cx.activate(true)",
        "self.sessions.items.iter().any(|row| row.id == session)",
        "self.open_session(session, cx)",
        "the session is gone",
    ] {
        assert!(
            DASHBOARD_TURNS.contains(needle),
            "the poll tick lost {needle:?}"
        );
    }
}

#[test]
fn an_activation_uri_names_exactly_one_session() {
    // #2751: a protocol launch is the only thing a fresh process learns from a
    // banner click, so the URI has to round-trip and nothing else may be taken
    // for one. Anything unrecognised stays unrecognised rather than becoming a
    // queue entry the window would then fail to open.
    let session = Uuid::new_v4();
    let uri = format!("{ACTIVATION_SCHEME}{session}");
    assert_eq!(activation_from_arg(&uri), Some(session));
    assert_eq!(activation_from_arg(&format!("  {uri}  ")), Some(session));

    for junk in [
        "",
        "opencrabs",
        "opencrabs://session/",
        "opencrabs://session/not-a-uuid",
        "https://example.com/session/",
        "opencrabs://topic/8f1f0b1e-2f7c-4f0e-9a3a-5d0f0f0f0f0f",
    ] {
        assert_eq!(
            activation_from_arg(junk),
            None,
            "{junk:?} must not be read as a session"
        );
    }

    // An argv with no activation queues nothing: the same call runs on every
    // launch, including the ones a person started from a terminal.
    assert!(!post_activation_from_args(["--help", "something-else"]));
}

#[test]
fn a_toast_about_a_session_offers_a_way_back() {
    // #2867: the toast was text only, so Windows had nobody to tell. It now
    // names the activation URI on the toast itself and on one action, and both
    // use `protocol` because the process that shows the toast exits long before
    // a click arrives: a `foreground` activation would be delivered to nobody.
    let session = Uuid::new_v4();
    let uri = format!("{ACTIVATION_SCHEME}{session}");
    let xml = toast_xml("Plan complete: ship it", "3 tasks settled", Some(session));

    assert!(xml.starts_with("<toast"), "the document is a toast");
    assert!(
        xml.contains(&format!("activationType=\"protocol\" launch=\"{uri}\"")),
        "the toast names its session: {xml}"
    );
    assert!(
        xml.contains("<actions><action content=\"Open session\""),
        "the toast offers an action: {xml}"
    );
    assert!(
        xml.contains(&format!("arguments=\"{uri}\" activationType=\"protocol\"")),
        "the action names the same session: {xml}"
    );
    assert!(xml.contains("<text id=\"1\">Plan complete: ship it</text>"));
    assert!(xml.contains("<text id=\"2\">3 tasks settled</text>"));
    assert!(xml.ends_with("</actions></toast>"), "the document closes");

    // A banner with no session stays informational: no action, no launch, and
    // nothing for a click to land on.
    let bare = toast_xml("Turn complete", "all set", None);
    assert!(!bare.contains("activationType"), "no session, no launch");
    assert!(!bare.contains("<actions>"), "no session, no button");
    assert!(bare.contains("<text id=\"2\">all set</text>"));

    // The title is escaped, so a label containing markup cannot rewrite the
    // document it travels in.
    let hostile = toast_xml("a <b> & \"c\"", "d", Some(session));
    assert!(hostile.contains("<text id=\"1\">a &lt;b&gt; &amp; &quot;c&quot;</text>"));
}

#[test]
fn both_clickable_platforms_are_wired_and_the_gui_takes_the_uri() {
    // The transport has to hand the session to the two arms that can use it,
    // and the window has to drain the URI a protocol launch hands it (#2751).
    assert!(
        SOURCE.contains("fn dispatch(title: &str, body: &str, session: Option<Uuid>)"),
        "both clickable arms take the session"
    );
    for needle in [
        "$env:OC_NOTIFY_XML",
        "fn ensure_activation_handler()",
        "URL Protocol",
        "HKCU\\Software\\Classes\\opencrabs",
        "CREATE_NO_WINDOW",
    ] {
        assert!(SOURCE.contains(needle), "the windows arm lost {needle:?}");
    }
    let gui = include_str!("../gui/main.rs");
    assert!(
        gui.contains("post_activation_from_args(std::env::args().skip(1))"),
        "the window takes the activation URI off argv"
    );
}
