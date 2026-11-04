//! #2078: the release workflow must not publish a commit whose CI is not green.
//!
//! v0.5.10 was tagged and published six seconds after the CI run on the same
//! commit started, and that run concluded `failure`. Nothing stopped it,
//! because `wait-for-ci` had no CI check at all: it verified the commit was on
//! main and then printed "Ready to release." whatever that commit's CI said.
//! The polling had been deleted, and nothing failed when it was.
//!
//! This is the guard the issue asks for. It cannot prove the workflow behaves
//! correctly when it runs (that is the shell script's job, verified against
//! real runs), but it fails loudly if the gate is deleted, so removing it can
//! never be a silent change again.

/// The release workflow, read at compile time so the test cannot pass by
/// finding a stale copy somewhere else in the tree.
const RELEASE_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/release.yml"
));

/// The `wait-for-ci` job body, from its own header to the job that follows it.
///
/// Scoped deliberately: a bare `contains` over the whole file would pass on a
/// string that lives in some other job, which is not where the gate has to be.
fn wait_for_ci_job() -> &'static str {
    let start = RELEASE_YML
        .find("  wait-for-ci:")
        .expect("the wait-for-ci job must exist (#2078)");
    let rest = &RELEASE_YML[start..];
    let end = rest
        .find("\n  publish-crate:")
        .expect("publish-crate must follow wait-for-ci in release.yml");
    &rest[..end]
}

#[test]
fn the_release_reads_the_ci_run_for_the_tagged_commit() {
    let job = wait_for_ci_job();
    assert!(
        job.contains("--workflow ci.yml"),
        "wait-for-ci must look up the ci.yml run for the tagged commit (#2078)"
    );
    assert!(
        job.contains("--commit \"$SHA\""),
        "the CI run must be looked up by the tagged commit, not by branch (#2078)"
    );
    assert!(
        job.contains("actions: read"),
        "reading Actions runs needs the actions: read permission (#2078)"
    );
}

#[test]
fn a_red_ci_run_stops_the_release() {
    let job = wait_for_ci_job();
    assert!(
        job.contains("Refusing to publish a red commit"),
        "a non-success conclusion must refuse the release (#2078)"
    );
    assert!(
        !RELEASE_YML.contains("Proceeding anyway"),
        "the old escape hatch must stay gone: an unverifiable commit fails, it does not warn (#2078)"
    );
}

#[test]
fn a_missing_ci_run_stops_the_release() {
    let job = wait_for_ci_job();
    assert!(
        job.contains("no ci.yml run found"),
        "a commit with no ci.yml run must fail the release, never skip the check (#2078)"
    );
    assert!(
        job.contains("expected a full 40-character commit SHA"),
        "an abbreviated SHA makes `gh run list --commit` return nothing, so it must be rejected (#2078)"
    );
}
