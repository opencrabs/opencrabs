//! #3032: a finished turn joins the transcript instead of refetching it.
//!
//! The window used to answer every `Done` with a full reload: a new thread, a
//! fresh current-thread runtime, its own `Database::connect`, a 200-row read
//! and a rebuild of every turn in the window. Measured on this machine's log
//! the ratio was 44 `turn done (` to 44 `reloading transcript`, so the reload
//! was the steady state, not the edge case.
//!
//! These are source pins, the same shape as the layout-boot pins (#2689): the
//! wiring lives in the gpui dashboard, which has no headless harness here, so
//! the pins hold the route rather than the paint.

const TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The `Done` arm appends. The line that used to name a reload is gone, so the
/// 1:1 ratio the issue measured cannot come back without failing here first.
#[test]
fn a_finished_turn_appends_instead_of_reloading() {
    assert!(
        TURNS.contains("turn appended: {} blocks, anchor {}"),
        "the Done arm must append the finished turn"
    );
    assert!(
        !TURNS.contains("reloading transcript"),
        "the Done log line must not claim a reload any more"
    );
    assert!(
        TURNS.contains("transcript::turn_from_live(&self.turn.live, None)"),
        "the append must be built from the card that painted the turn"
    );
}

/// The reload stays for the reads that need the rows. `Failed` still has no
/// settled turn to append, and a turn the card cannot name falls back to the
/// rows, so exactly one assignment survives and it is not in `Done`.
#[test]
fn the_reload_survives_for_the_reads_that_need_the_rows() {
    let reloads = TURNS.matches("reload = self.chat_state.session();").count();
    assert_eq!(
        reloads, 1,
        "only the Failed arm should still ask for a reload"
    );
    assert!(
        TURNS.contains("None => reload = Some(sid),"),
        "a turn with no question of its own falls back to the rows"
    );
    assert!(
        TURNS.contains("if let Some(sid) = reload {"),
        "the tail still consumes whatever asked for a reload"
    );
}

/// The appended turn claims the anchor the rows would give it, or an opened
/// turn closes itself on the next reload.
#[test]
fn the_appended_turn_claims_the_anchor_the_rows_would_give_it() {
    let builder = TRANSCRIPT
        .split("pub fn turn_from_live(")
        .nth(1)
        .expect("turn_from_live must exist");
    let builder = builder
        .split("/// Whether the persisted rows of one turn")
        .next()
        .expect("the builder must end before the next item");
    assert!(
        builder.contains(r#"anchor: format!("user@{}", sent.timestamp()),"#),
        "the anchor must be spelled from the question row and its second"
    );
    // The spelling has to match what the rebuild produces, or the same turn
    // gets two identities across one reload.
    let rebuild = TRANSCRIPT
        .split("pub fn turns_with_origins(")
        .nth(1)
        .expect("turns_with_origins must exist");
    let rebuild = rebuild
        .split("pub fn turn_from_live(")
        .next()
        .expect("the rebuild must end before the builder");
    assert!(
        rebuild.contains(r#"format!("{}@{}", rows[s].role, rows[s].created_at.timestamp())"#),
        "the rebuild must keep spelling the anchor the same way"
    );
}

/// The settle guarantee is not dropped, it is not owed: the appended path
/// resolves the expectation itself, so a later reload cannot inherit a count
/// nothing read back and turn a short read into a phantom retry (#2705).
#[test]
fn the_append_resolves_the_settle_expectation_it_skips() {
    assert!(
        TURNS.contains("self.turn.settle.expect = None;"),
        "the skipped read must not leave an expectation behind"
    );
    assert!(
        TURNS.contains("self.attach_pending_metrics(sid);"),
        "the numbers recorded at Done belong to the turn that was appended"
    );
    // The live card has to clear, or the answer draws twice: once as the
    // appended turn and once as the card still holding it.
    assert!(
        TURNS.contains("self.turn.live.clear();"),
        "the live card must hand the turn over to the list"
    );
}

/// The builder exists and returns the same shape the rebuild does: one turn
/// with its own blocks, final answer, summary and metrics.
#[test]
fn the_builder_produces_a_whole_turn() {
    assert!(
        TRANSCRIPT.contains(
            "pub fn turn_from_live(live: &LiveTurn, metrics: Option<TurnMetrics>) -> Option<Turn>"
        ),
        "the builder signature is part of the contract"
    );
    let builder = TRANSCRIPT
        .split("pub fn turn_from_live(")
        .nth(1)
        .expect("turn_from_live must exist")
        .split("/// Whether the persisted rows of one turn")
        .next()
        .expect("the builder must end before the next item");
    for field in ["header,", "summary,", "metrics,", "blocks,", "final_idx,"] {
        assert!(
            builder.contains(field),
            "the built turn must carry `{field}` like the rebuild does"
        );
    }
    assert!(
        builder.contains("let header = format_turn_header(summary, steps);"),
        "the header comes from the same formatter the rebuild uses"
    );
}
