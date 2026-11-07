//! The turn path never loads the config on the async stack (#2661).
//!
//! `Config::load` is synchronous and builds a large value. Called straight
//! from inside the tool loop it blocked a tokio worker and, in a debug build,
//! overflowed that worker's 2 MB stack, aborting the process mid-turn. Both
//! call sites now go through `load_config_off_stack`, which does the load on
//! the blocking pool and boxes it there.

const BUILDER: &str = include_str!("../brain/agent/service/builder.rs");

const DIRECT_LOAD: &str = "crate::config::Config::load()";
const VIA_HELPER: &str = "load_config_off_stack().await";

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let after = source
        .split_once(start)
        .map(|(_, rest)| rest)
        .expect("start marker");
    after
        .split_once(end)
        .map(|(body, _)| body)
        .expect("end marker")
}

/// The helper does the load where a big frame is harmless and hands back a
/// boxed value, so only a pointer crosses back over the await.
#[test]
fn the_helper_boxes_the_config_on_the_blocking_pool() {
    let helper = between(
        BUILDER,
        "async fn load_config_off_stack()",
        "/// Live-rebuild handle for the system brain",
    );
    assert!(
        helper.contains("tokio::task::spawn_blocking"),
        "the load must run on the blocking pool, not on the async worker"
    );
    assert!(
        helper.contains(DIRECT_LOAD),
        "the helper must be the thing that loads the config"
    );
    assert!(
        helper.contains("Box::new"),
        "the value is boxed on the blocking thread, so the async frame stays small"
    );
}

/// One direct load left anywhere in this file means a future turn can abort
/// the process again, which is exactly the crash this pins against.
#[test]
fn the_file_loads_the_config_in_one_place_only() {
    assert_eq!(
        BUILDER.matches(DIRECT_LOAD).count(),
        1,
        "load the config only inside load_config_off_stack"
    );
}

/// Both plan-mode paths, the apply that runs every turn and the restore on
/// resume, go through the helper.
#[test]
fn both_plan_mode_paths_await_the_helper() {
    assert_eq!(
        BUILDER.matches(VIA_HELPER).count(),
        2,
        "apply_plan_mode_provider and restore_from_plan_mode both await the helper"
    );
}
