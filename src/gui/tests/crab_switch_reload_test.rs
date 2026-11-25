//! What a crab swap reads again, and what the window stops reading from this
//! machine while it holds a crab elsewhere (P7).
//!
//! The usage view, mission control and the channel chip are per crab: a swap
//! that left them up showed this machine's numbers under another crab's name.
//! Where the rule is about where a read goes, the source is pinned, the way
//! the other window guards pin theirs. A remote crab is a `RemoteCrab` on a
//! port nothing listens on.

use std::path::PathBuf;
use std::sync::Arc;

use opencrabs::backend::cap::Cap;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::channel_chip::{
    channel_chip_line_from, channel_names, channels_named, configured_channels,
};

const WIRING: &str = include_str!("../dashboard_instances.rs");
const SCREENS: &str = include_str!("../dashboard_screens.rs");
const CHIP: &str = include_str!("../channel_chip.rs");
const NAV: &str = include_str!("../dashboard_nav.rs");
const APP: &str = include_str!("../app.rs");
const CONFIG: &str = include_str!("../dashboard_config.rs");
const FINDER: &str = include_str!("../dashboard_finder.rs");
const RAIL: &str = include_str!("../dashboard_rail.rs");
const SESSIONS: &str = include_str!("../dashboard_sessions.rs");
const TOPICS: &str = include_str!("../dashboard_topics.rs");

fn remote() -> Backend {
    Backend::Remote(Arc::new(RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    )))
}

fn local() -> Backend {
    Backend::local(PathBuf::from("unused.db"))
}

/// The text of `fn name` in `src`, from its signature to the next method of
/// the same impl. Split on lines, not sliced at byte offsets.
fn fn_body(src: &str, name: &str) -> String {
    let head = format!("fn {name}(");
    let (_, rest) = src
        .split_once(head.as_str())
        .unwrap_or_else(|| panic!("no fn {name} in the source"));
    let mut body = String::new();
    for line in rest.lines() {
        let next_method = line.starts_with("    pub(crate) fn ") || line.starts_with("    fn ");
        if !body.is_empty() && next_method {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// The probe block in the window's constructor, from its comment to the
/// next loop's.
fn chip_probe() -> &'static str {
    let (_, rest) = APP
        .split_once("// The channel chip's slow probe (#2452)")
        .expect("the chip probe");
    rest.split_once("// The rail's ledger probe")
        .map(|(probe, _)| probe)
        .expect("the probe ends where the rail's starts")
}

#[test]
fn a_swap_reads_the_usage_of_the_new_crab_for_the_period_on_screen() {
    let switch = fn_body(WIRING, "instance_switch");
    let swapped = switch.find("self.backend = backend;").expect("swap");
    let usage = switch.find("self.reload_usage(cx);").expect("usage reload");
    assert!(
        usage > swapped,
        "the reload asks the crab the window now holds"
    );

    // The reload is the period loader without the "did it change" gate: a
    // swap keeps the period and changes the ledger.
    let reload = fn_body(SCREENS, "reload_usage");
    assert!(reload.contains("let period = self.dash.period;"));
    assert!(reload.contains("let backend = self.backend.clone();"));
    assert!(reload.contains("load_view(backend, period).await"));
    assert!(reload.contains("pending == this.pending"));
    let period = fn_body(SCREENS, "set_usage_period");
    assert!(period.contains("if !self.dash.set_period(period)"));
    assert!(period.contains("self.reload_usage(cx);"));
}

#[test]
fn a_swap_drops_the_mission_control_snapshot_and_rereads_an_open_screen() {
    let switch = fn_body(WIRING, "instance_switch");
    assert!(switch.contains("self.mc = None;"));
    assert!(switch.contains("self.mc_loading = false;"));
    assert!(switch.contains("if self.screen == Screen::MissionControl"));
    assert!(switch.contains("self.fetch_mission_control(cx);"));
}

#[test]
fn a_swap_rereads_the_chip_from_the_new_crab() {
    let switch = fn_body(WIRING, "instance_switch");
    assert!(switch.contains("self.channel_chip = None;"));
    assert!(switch.contains("self.chip_channel_names.clear();"));
    assert!(switch.contains("self.refresh_chip_names(cx);"));
    // Still nothing in the swap itself touches the network.
    assert!(!switch.contains(".call("));
    assert!(!switch.contains("block_on"));

    let names = fn_body(CHIP, "refresh_chip_names");
    assert!(names.contains("self.backend.clone()"));
    assert!(names.contains("configured_channels(backend).await"));
    assert!(names.contains("this.instance_epoch != epoch"));
    assert!(names.contains("this.refresh_channel_chip(cx);"));
}

#[test]
fn the_chip_never_asks_this_machine_behind_the_backend() {
    let nav = fn_body(NAV, "refresh_channel_chip");
    for (file, src) in [
        ("channel_chip.rs", CHIP),
        ("dashboard_nav.rs refresh_channel_chip", nav.as_str()),
        ("app.rs chip probe", chip_probe()),
        ("dashboard_finder.rs", FINDER),
    ] {
        assert!(
            !src.contains("ask_crab("),
            "{file} asks this machine's crab"
        );
        assert!(
            !src.contains("configured_channels()"),
            "{file} reads the channels without a backend"
        );
    }
    // Every read in the chip takes the backend it asks.
    for head in [
        "async fn configured_channels(backend: Backend)",
        "async fn channel_ownership(backend: Backend)",
        "async fn channel_chip_line(backend: Backend)",
    ] {
        assert!(CHIP.contains(head), "{head}");
    }
    assert!(CHIP.contains("backend.call(ChannelStatus { configured: true })"));
    assert!(CHIP.contains(".call(ChannelStatus { configured: false })"));
}

#[test]
fn the_probe_asks_the_crab_on_screen_and_drops_a_stale_answer() {
    let probe = chip_probe();
    assert!(probe.contains("this.backend.clone()"));
    assert!(probe.contains("this.instance_epoch"));
    assert!(probe.contains("this.chip_channel_names.clone()"));
    assert!(probe.contains("channel_chip_line_from(backend, names).await"));
    assert!(probe.contains("if this.instance_epoch != epoch"));

    let now = fn_body(NAV, "refresh_channel_chip");
    assert!(now.contains("let backend = self.backend.clone();"));
    assert!(now.contains("if this.instance_epoch != epoch"));
}

#[test]
fn a_crab_elsewhere_answers_for_its_own_channels() {
    // A remote crab that does not answer has no channels the window can
    // name: its silence is never filled in with this machine's config.
    let channels = futures::executor::block_on(configured_channels(remote()));
    assert!(channels.is_empty(), "{channels:?}");
    // No names, no read and no chip.
    assert_eq!(
        futures::executor::block_on(channel_chip_line_from(remote(), Vec::new())),
        None
    );
}

#[test]
fn the_chip_names_round_trip_to_the_finder_channels() {
    let names = vec!["Telegram".to_string(), "nope".to_string()];
    let channels = channels_named(&names);
    assert_eq!(channel_names(&channels), vec!["Telegram".to_string()]);
}

#[test]
fn a_reload_of_this_machines_config_reaches_the_chip_only_for_this_machines_crab() {
    let reload = fn_body(CONFIG, "refresh_from_config");
    assert!(reload.contains(".lacks(opencrabs::backend::cap::Cap::Setup)"));
    assert!(reload.contains("if this_machine && names != self.chip_channel_names"));
    assert!(reload.contains("if this_machine && self.finder.is_some()"));
    // The decision is the backend's, so a remote crab never takes the names.
    assert!(remote().lacks(Cap::Setup).is_some());
    assert_eq!(local().lacks(Cap::Setup), None);
}

#[test]
fn start_on_a_crab_elsewhere_installs_nothing_here() {
    let start = fn_body(NAV, "start_daemon");
    let (before_install, _) = start
        .split_once("install_daemon_service()")
        .expect("the local install is still there");
    let (_, refused) = before_install
        .split_once(".lacks(opencrabs::backend::cap::Cap::Setup)")
        .expect("start_daemon asks the backend before the installer");
    assert!(refused.contains("self.daemon_error = Some(SharedString::from(why));"));
    assert!(
        refused.contains("return;"),
        "a refusal does not fall through"
    );
    // The reason the chip shows is the backend's own.
    let why = remote().lacks(Cap::Setup).expect("remote lacks setup");
    assert!(why.contains("this machine"), "{why}");
}

#[test]
fn a_refetch_with_a_stale_epoch_leaves_the_rail_alone() {
    let refetch = fn_body(RAIL, "refresh_rail");
    assert!(refetch.contains("let epoch = self.instance_epoch;"));
    let stale = refetch
        .find("if this.instance_epoch != epoch")
        .expect("the refetch checks the epoch");
    let rows = refetch
        .find("this.sessions.items = items;")
        .expect("the refetch writes the rows");
    let panes = refetch
        .find("this.notify_panes(cx);")
        .expect("the refetch tells the panes");
    assert!(stale < rows, "a stale answer never reaches `sessions`");
    assert!(stale < panes, "nor `follow_crab`, through `notify_panes`");
}

/// Where `needle` sits in `body`, or a panic naming what was looked for.
fn at(body: &str, needle: &str, what: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("{what}: no `{needle}`"))
}

/// An arrival that writes per-crab state takes the epoch when it leaves, and
/// on landing returns before its first write when the epoch moved. The guard
/// is `!=` and a `return`, so an answer from the crab still on screen goes on
/// to the write.
fn assert_dropped_when_stale(src: &str, name: &str, first_write: &str) {
    let body = fn_body(src, name);
    let taken = at(&body, "let epoch = self.instance_epoch;", name);
    let spawned = at(&body, "cx.spawn(", name);
    assert!(
        taken < spawned,
        "{name}: the epoch is taken before the read leaves"
    );
    let guard = at(&body, "if this.instance_epoch != epoch {", name);
    let (_, after_guard) = body.split_at(guard);
    let first_line = after_guard
        .lines()
        .nth(1)
        .map(str::trim)
        .unwrap_or_default();
    assert_eq!(first_line, "return;", "{name}: a stale answer returns");
    let write = at(&body, first_write, name);
    assert!(
        guard < write,
        "{name}: a stale answer never reaches `{first_write}`"
    );
}

#[test]
fn a_mission_control_read_of_the_crab_left_behind_touches_nothing() {
    // Not even the loading flag: the swap cleared it and may have the new
    // crab's read out, which a stale answer must not unlock.
    assert_dropped_when_stale(SCREENS, "fetch_mission_control", "this.mc_loading = false;");
    assert_dropped_when_stale(
        SCREENS,
        "fetch_mission_control",
        "this.mc = Some(snapshot);",
    );
}

#[test]
fn a_mission_control_decision_on_the_crab_left_behind_paints_no_notice() {
    assert_dropped_when_stale(SCREENS, "decide_selected", "this.mc_notice = Some(");
    assert_dropped_when_stale(
        SCREENS,
        "decide_selected",
        "this.fetch_mission_control(cx);",
    );
}

#[test]
fn a_usage_read_of_the_crab_left_behind_is_dropped() {
    assert_dropped_when_stale(SCREENS, "reload_usage", "this.state = state;");
}

#[test]
fn pins_and_topic_state_of_the_crab_left_behind_are_dropped() {
    assert_dropped_when_stale(SESSIONS, "refresh_favorites", "this.sessions.favorites =");
    assert_dropped_when_stale(SESSIONS, "enable_topics_then", "this.sessions.topics =");
    assert_dropped_when_stale(SESSIONS, "enable_topics_then", "after(this, cx);");
    assert_dropped_when_stale(TOPICS, "refresh_topic_state", "this.sessions.topics =");
    // The topic refresh tells the panes too, the same road `follow_crab`
    // takes from the rail's refetch.
    assert_dropped_when_stale(TOPICS, "refresh_topic_state", "this.notify_panes(cx);");
}

#[test]
fn rail_previews_of_the_crab_left_behind_are_not_kept_but_the_queue_still_drains() {
    let body = fn_body(RAIL, "refresh_previews");
    let taken = at(
        &body,
        "let epoch = self.instance_epoch;",
        "refresh_previews",
    );
    assert!(taken < at(&body, "cx.spawn(", "refresh_previews"));
    let guard = at(
        &body,
        "if this.instance_epoch == epoch {",
        "refresh_previews",
    );
    let write = at(&body, "this.sessions.previews = pairs;", "refresh_previews");
    let drain = at(&body, "if dirty.replace(false) {", "refresh_previews");
    assert!(guard < write, "the write sits inside the epoch check");
    // The request the swap queued behind the stale read is still served.
    assert!(write < drain, "the queued request runs whatever the epoch");
    let (_, inside) = body.split_at(guard);
    let closed = inside
        .lines()
        .skip(1)
        .find(|line| line.trim() == "}")
        .expect("the check closes");
    let (guarded, _) = inside.split_once(closed).expect("the guarded block");
    assert!(
        !guarded.contains("dirty.replace"),
        "the drain is outside the check"
    );
}

#[test]
fn every_arrival_pinned_here_reads_the_epoch_the_swap_moves() {
    let switch = fn_body(WIRING, "instance_switch");
    let bumped = at(
        &switch,
        "self.instance_epoch = self.instance_epoch.wrapping_add(1);",
        "instance_switch",
    );
    for reread in [
        "self.refresh_rail(cx);",
        "self.reload_usage(cx);",
        "self.fetch_mission_control(cx);",
    ] {
        assert!(
            bumped < at(&switch, reread, "instance_switch"),
            "{reread} leaves with the new epoch"
        );
    }
}

/// A swap closes the conversation on screen before the new crab comes in:
/// the transcript, the selection and the composer's session all belonged to
/// the crab being left, and a send from a window still holding them named a
/// session the new crab never had.
#[test]
fn a_swap_leaves_the_open_conversation_before_taking_the_new_crab() {
    let switch = fn_body(WIRING, "instance_switch");
    let left = switch
        .find("self.leave_conversation_for_swap(cx);")
        .expect("a swap leaves the conversation on screen");
    let swapped = switch.find("self.backend = backend;").expect("swap");
    assert!(
        left < swapped,
        "the conversation is left while the window still holds its crab"
    );
    let leave = fn_body(WIRING, "leave_conversation_for_swap");
    for cleared in [
        "self.set_chat_session(None);",
        "self.sessions.selected = None;",
        "self.sync_composer_session(cx);",
        "self.chat_state.transcript.rows = Vec::new();",
        "self.chat_state.transcript.turns = Vec::new();",
    ] {
        assert!(leave.contains(cleared), "a swap must run `{cleared}`");
    }
    assert!(
        leave.contains("self.stashed.insert(from"),
        "a turn still running on the crab being left is parked, not dropped"
    );
}
