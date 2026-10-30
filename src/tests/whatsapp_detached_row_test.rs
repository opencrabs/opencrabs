//! One updating row per detached task on WhatsApp (#2150).
//!
//! WhatsApp is the one surface with a real constraint: a message can only be
//! edited for 15 minutes. A detached task outlives the turn that started it,
//! so a long build finishes outside that window. The block then has to stop
//! editing, stop posting running ticks, and settle with exactly one fresh row
//! that does not look live.
//!
//! The network half (`flow_message.rs`) owns a live client, so the decision
//! lives in `WindowPlan` and these tests drive a whole sequence of ticks
//! through it, counting the posts the way the bot would send them.

use std::time::Duration;

use crate::brain::agent::ProgressEvent;
use crate::channels::whatsapp::flow_render::{
    FlushAction, Phase, WindowPlan, fold, render, within_edit_window,
};

fn task_started(task_id: &str, label: &str) -> ProgressEvent {
    ProgressEvent::DetachedTaskStarted {
        task_id: task_id.to_string(),
        label: label.to_string(),
    }
}

fn task_finished(task_id: &str, success: bool) -> ProgressEvent {
    ProgressEvent::DetachedTaskFinished {
        task_id: task_id.to_string(),
        label: String::new(),
        success,
        elapsed_secs: 12.0,
    }
}

/// The successful network path of `Block::flush`, with the sends counted
/// instead of performed. A flush that decides `Post` and lands is a new
/// message on the chat; one that decides `Edit` and lands changes the row
/// already there.
struct Counter {
    plan: WindowPlan,
    posts: usize,
    edits: usize,
}

impl Counter {
    fn new() -> Self {
        Self {
            plan: WindowPlan::default(),
            posts: 0,
            edits: 0,
        }
    }

    fn flush(&mut self, text: &str, settled: bool, window_open: bool) -> FlushAction {
        let action = self.plan.decide(text, settled, window_open);
        match action {
            FlushAction::Post => {
                self.plan.landed(text);
                self.posts += 1;
            }
            FlushAction::Edit => {
                self.plan.landed(text);
                self.edits += 1;
            }
            FlushAction::Skip => {}
        }
        action
    }
}

#[test]
fn an_update_inside_the_window_edits_the_row_it_posted() {
    let mut counter = Counter::new();

    assert_eq!(
        counter.flush("running 0:05", false, true),
        FlushAction::Post
    );
    assert_eq!(counter.posts, 1, "the first render is the row");

    assert_eq!(
        counter.flush("running 0:10", false, true),
        FlushAction::Edit
    );
    assert_eq!(counter.posts, 1, "inside the window the row is edited");
    assert_eq!(counter.edits, 1);

    // The same text twice is a no-op, so a tick cannot cost an edit.
    assert_eq!(
        counter.flush("running 0:10", false, true),
        FlushAction::Skip
    );
    assert_eq!(counter.edits, 1);
}

#[test]
fn a_detached_row_updates_in_place_while_the_window_is_open() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));

    let mut counter = Counter::new();
    let running = render(&steps, Phase::Running, Duration::from_secs(5));
    assert_eq!(counter.flush(&running, false, true), FlushAction::Post);
    assert!(running.contains("cargo test"), "{running}");

    // A later tick while the task still runs: same message, new clock.
    let later = render(&steps, Phase::Running, Duration::from_secs(30));
    assert_eq!(counter.flush(&later, false, true), FlushAction::Edit);
    assert_eq!(counter.posts, 1, "one row for the whole task");
}

#[test]
fn after_the_window_closes_running_ticks_stop_posting() {
    let mut counter = Counter::new();
    assert_eq!(
        counter.flush("running 0:05", false, true),
        FlushAction::Post
    );

    // The task outlives the window: every later running tick is a no-op,
    // because a post here would restart the clock and split the row.
    for tick in 1..=20 {
        let text = format!("running 16:{tick:02}");
        assert_eq!(counter.flush(&text, false, false), FlushAction::Skip);
    }

    assert_eq!(
        counter.posts, 1,
        "a closed window posts nothing while running"
    );
    assert_eq!(counter.edits, 0);

    // Expiry is visible in what the next flush does, not in a private field:
    // a settled render now posts a fresh row instead of editing the old one.
    assert_eq!(counter.flush("done 16:30", true, false), FlushAction::Post);
    assert_eq!(counter.edits, 0, "an expired plan never edits again");
}

#[test]
fn the_settled_row_posts_exactly_once_after_the_window_closed() {
    let mut counter = Counter::new();
    assert_eq!(
        counter.flush("running 0:05", false, true),
        FlushAction::Post
    );

    // The window closes while the detached task is still running.
    assert_eq!(
        counter.flush("running 15:01", false, false),
        FlushAction::Skip
    );
    assert_eq!(
        counter.flush("running 15:06", false, false),
        FlushAction::Skip
    );

    // The task finishes: one fresh row, settled, so it never reads as live.
    assert_eq!(counter.flush("done 18:20", true, false), FlushAction::Post);
    assert_eq!(counter.posts, 2, "the start row plus one final row");

    // Nothing after it may post again; a late render edits the final row.
    assert_eq!(counter.flush("done 18:20", true, false), FlushAction::Skip);
    assert_eq!(counter.flush("done 18:21", true, true), FlushAction::Edit);
    assert_eq!(counter.posts, 2, "the final row is never duplicated");
}

#[test]
fn a_finish_after_the_window_renders_a_settled_row_not_a_running_one() {
    let mut steps = Vec::new();
    fold(&mut steps, &task_started("t1", "cargo test"));
    let running = render(&steps, Phase::Running, Duration::from_secs(5));

    let mut counter = Counter::new();
    assert_eq!(counter.flush(&running, false, true), FlushAction::Post);

    // 16 minutes later the build finishes.
    fold(&mut steps, &task_finished("t1", true));
    let settled = render(&steps, Phase::Finished, Duration::from_secs(16 * 60));
    assert_eq!(counter.flush(&settled, true, false), FlushAction::Post);

    assert!(settled.contains("✅"), "{settled}");
    assert!(settled.contains("detached task finished"), "{settled}");
    assert_eq!(counter.posts, 2, "start row, then one settled row");
}

#[test]
fn a_refused_edit_falls_back_to_one_final_row() {
    let mut counter = Counter::new();
    assert_eq!(
        counter.flush("running 0:05", false, true),
        FlushAction::Post
    );

    // The server refuses the edit even though the local clock still looked
    // open (clock skew, a restarted node). Running updates stop here.
    assert_eq!(
        counter.flush("running 0:10", false, true),
        FlushAction::Edit
    );
    counter.plan.edit_refused();
    assert_eq!(
        counter.flush("running 0:15", false, true),
        FlushAction::Skip
    );

    assert_eq!(counter.flush("done 0:20", true, true), FlushAction::Post);
    assert_eq!(
        counter.posts, 2,
        "the refusal costs one final post, not a stream"
    );
}

#[test]
fn a_refused_post_retries_only_when_the_turn_settles() {
    let mut counter = Counter::new();

    assert_eq!(
        counter.flush("running 0:05", false, true),
        FlushAction::Post
    );
    counter.plan.post_refused();
    counter.posts = 0;

    assert_eq!(
        counter.flush("running 0:10", false, true),
        FlushAction::Skip
    );
    assert_eq!(counter.posts, 0, "no retry on every tick");

    assert_eq!(counter.flush("done 0:15", true, true), FlushAction::Post);
    assert_eq!(counter.posts, 1, "the settled state still lands once");
}

#[test]
fn the_local_window_check_matches_the_outbox_window() {
    // The plan is only told whether the window is open, so the clock itself
    // has to agree with the outbox that actually sends the edit.
    assert!(within_edit_window(Duration::from_secs(0)));
    assert!(within_edit_window(Duration::from_secs(14 * 60 + 59)));
    assert!(!within_edit_window(Duration::from_secs(15 * 60)));
}
