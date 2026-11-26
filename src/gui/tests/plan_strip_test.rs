//! The plan strip's cards, drawn from a real plan (#3124).
//!
//! These lived in an inline `mod tests` at the bottom of `plan_strip.rs` until
//! #3124. The layout guard (#3062) freezes inline modules under `gui/` and says
//! where their tests go: beside the module they exercise, reached through
//! `crate::` paths. The file moved whole, and the guard's frozen entry went
//! with it.

use crate::plan_strip::{LineKind, PaneDensity, pane_density, pane_lines, visible_window};
use opencrabs::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};

fn plan_with(tasks: usize, done: usize, running: Option<usize>) -> PlanDocument {
    let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
    plan.status = PlanStatus::Active;
    for i in 0..tasks {
        let mut task = PlanTask::new(i + 1, format!("task {i}"), String::new(), TaskType::Edit);
        if i < done {
            task.status = TaskStatus::Completed;
        }
        plan.add_task(task);
    }
    if let Some(i) = running {
        plan.tasks[i].status = TaskStatus::InProgress;
    }
    plan
}

/// #2691: the GUI had no chime call site at all, so the notification the
/// terminal rings was silence in the window. The window's pass reads the
/// strips through `document()` and dedupes with one shared seen set. This
/// pins the data half of that wiring against a real plan file: a settled
/// task counts once, an unchanged re-read counts zero, and the same plan
/// drawn by two strips still counts once.
#[test]
fn a_settled_task_rings_once_across_every_strip_showing_it() {
    use crate::plan_strip::PlanStripState;
    use opencrabs::utils::completion_sound::newly_resolved;
    use std::collections::HashSet;

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plan.json");
    let mut plan = plan_with(2, 1, Some(1));
    std::fs::write(
        &path,
        serde_json::to_string(&plan).expect("plan serializes"),
    )
    .expect("plan file written");

    let mut window_strip = PlanStripState::default();
    window_strip.set_path(Some(path.clone()));
    let mut pane_strip = PlanStripState::default();
    pane_strip.set_path(Some(path.clone()));
    assert!(
        window_strip.document().is_some(),
        "the strip loads the plan the chime pass reads"
    );

    // One seen set for the window, the union the Dashboard's ledger keeps
    // in memory and carries to disk per session (#3004).
    let mut seen: HashSet<uuid::Uuid> = HashSet::new();
    assert_eq!(
        newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
        1,
        "the same settled task in two strips is one chime, not two"
    );
    window_strip.refresh();
    pane_strip.refresh();
    assert_eq!(
        newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
        0,
        "an unchanged re-read must not ring again"
    );

    // The second task settles and the tool rewrites the file.
    plan.tasks[1].status = TaskStatus::Completed;
    std::fs::write(
        &path,
        serde_json::to_string(&plan).expect("plan serializes"),
    )
    .expect("plan file rewritten");
    window_strip.refresh();
    pane_strip.refresh();
    assert_eq!(
        newly_resolved(&mut seen, &[window_strip.document(), pane_strip.document()]),
        1,
        "the newly settled task rings once"
    );
    assert_eq!(seen.len(), 2, "both settled tasks are recorded");
}

/// #2624: the pane card was a wide box of ellipses in a split. Two faults
/// stacked. A column sized from its own truncating content made gpui
/// measure each line against the width it was about to define, and each
/// line was a width-full block div holding truncating text, which gpui
/// lays out once at a zero flex basis and caches as an ellipsis (#2642).
/// Both halves are pinned: the card owns its width, and each row is a flex
/// row with a growing, shrinkable, truncated child.
#[test]
fn the_pane_card_owns_its_width_and_click_affordance() {
    const SRC: &str = include_str!("../plan_strip.rs");
    let body = SRC
        .split_once("pub fn pane_view(")
        .expect("pane_view exists")
        .1
        .split_once("/// How much of a plan one pane shows")
        .expect("pane_view ends")
        .0;
    assert!(
        body.contains(".w_full()"),
        "the pane card must own its width, not derive it from the text it truncates"
    );
    assert!(
        body.contains(".cursor_pointer()") && body.contains(".hover("),
        "the active plan card looks clickable in both pane render paths"
    );
    assert!(
        body.contains(".when(index == 0") && body.contains("disclosure_icon(color, 1.0)"),
        "the disclosure arrow uses the progress bar's teal on the summary row"
    );
    // The loop builds the row and then pushes it into the column, so the pin
    // reads the builder itself: splitting on the push would leave nothing but
    // the store call in the region (#3124).
    let row = body
        .split_once("let row = div()")
        .expect("pane rows are built in a loop")
        .1;
    let flat: String = row.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(".flex_row()"),
        "each line is a flex row: {flat}"
    );
    assert!(
        flat.contains(".flex_1()"),
        "the text child grows and shrinks inside that row: {flat}"
    );
    assert!(
        !flat.contains(".w_full() .min_w_0() .overflow_hidden() .truncate()"),
        "the block-div truncate shape is back: gpui lays it out one character wide"
    );
}

/// #2755: `Editing` is the state that owns the approval, and the window
/// drew nothing at all in it while the terminal and Telegram grew an
/// Approve button for the same plan.
#[test]
fn an_editing_plan_gets_a_card_naming_the_plan_and_the_commands() {
    let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "Ship the card".to_string());
    plan.status = PlanStatus::Editing;
    let lines = crate::plan_strip::editing_lines(&plan);
    assert_eq!(lines[0].0, LineKind::Title);
    assert!(
        lines[0].1.contains("Editing plan") && lines[0].1.contains("Ship the card"),
        "{:?}",
        lines[0]
    );
    assert_eq!(
        lines[1],
        (LineKind::Next, crate::plan_strip::APPROVE_HINT.to_string())
    );
    assert!(crate::plan_strip::APPROVE_HINT.contains("/execute"));
    assert!(crate::plan_strip::APPROVE_HINT.contains("/discard"));

    // A plan with work says how much of it is done, not just how many rows
    // it has; an empty one still says it waits (#2970).
    let sized = plan_with(3, 0, None);
    assert!(
        crate::plan_strip::editing_lines(&sized)[0]
            .1
            .ends_with("0/3"),
        "{:?}",
        crate::plan_strip::editing_lines(&sized)[0]
    );
    assert!(
        crate::plan_strip::editing_lines(&plan_with(3, 2, None))[0]
            .1
            .ends_with("2/3"),
        "the count is progress, so a half-done plan says so"
    );
    assert!(
        crate::plan_strip::editing_lines(&plan)[0]
            .1
            .starts_with("\u{270d}\u{fe0f} Editing plan"),
        "an empty plan is still waiting: {:?}",
        crate::plan_strip::editing_lines(&plan)[0]
    );
    assert!(
        !crate::plan_strip::editing_lines(&plan)[0].1.contains('/'),
        "a design plan has nothing to count, so it prints no fraction: {:?}",
        crate::plan_strip::editing_lines(&plan)[0]
    );
}

/// #2970: the approval card named a count and stopped, so a reader asked to
/// approve a six-task checklist could not see the progress or a single row
/// of it. Telegram draws every row whatever its status, Discord draws the
/// rows beside `done/total`, and the window's own live card draws
/// `done/total`, the bar and the windowed rows. The one card that carries
/// the Approve button was the only surface that showed none of them.
#[test]
fn the_approval_card_shows_the_progress_and_the_rows_it_asks_to_approve() {
    let src = include_str!("../plan_strip.rs");
    let body = src
        .split("pub fn editing_view(")
        .nth(1)
        .and_then(|rest| rest.split("/// The plan as a split pane draws it").next())
        .expect("the editing view body");
    assert!(
        body.contains("progress_header_row(plan, head)"),
        "a checklist plan's header keeps the metrics the live card prints"
    );
    assert!(
        body.contains("task_rows(col, plan)"),
        "the six-row window the live card uses draws here too"
    );
    assert!(
        body.contains("if plan.tasks.is_empty()"),
        "only a plan with nothing to count falls back to the bare header"
    );
    assert_eq!(
        crate::plan_strip::editing_lines(&plan_with(6, 0, None))[0].1,
        "\u{270d}\u{fe0f} Editing plan · Landing · 0/6"
    );
}

/// The card is drawn for Editing only, in both draw paths, and its two
/// taps send the literal commands the terminal sends rather than calling
/// an approval implementation of their own.
#[test]
fn the_approval_card_is_drawn_where_the_taps_can_be_pressed() {
    let src = include_str!("../plan_strip.rs");
    let body = src
        .split("pub fn editing_view(")
        .nth(1)
        .and_then(|rest| rest.split("/// The plan as a split pane draws it").next())
        .expect("the editing view body");
    assert!(
        body.contains("matches!(plan.status, PlanStatus::Editing)"),
        "only a waiting plan gets the approval card"
    );
    assert!(
        body.contains("actions.on_approve") && body.contains("actions.on_discard"),
        "the card renders both taps it is handed"
    );

    // The strip is called from three places after the split (#2822): the
    // split pane's body, the tap builder, and the window's own render for
    // the unsplit case. The order below is the order the one file had.
    let main = concat!(
        include_str!("../dashboard_panes.rs"),
        include_str!("../dashboard_turns.rs"),
        include_str!("../dashboard_render.rs"),
    );
    let panes = main
        .split("fn panes_body(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("panes body");
    assert!(
        panes.contains("editing_view(Some(&actions), window)"),
        "a split pane draws the waiting plan, not just the running one"
    );
    // The unsplit card moved out of the strip block and into the
    // conversation's trailing rows (#3139): the hint, the offers and the
    // plan card are rows under the last turn now, and a split stands them
    // down because its panes draw their own (#3145). The slice starts at
    // that function and ends where the next one's doc begins, so the card
    // the unsplit window builds is inside it.
    let unsplit = main
        .split("fn chat_trailing(")
        .nth(1)
        .and_then(|rest| {
            rest.split("/// One card, on the bubbles' own column")
                .next()
        })
        .expect("the trailing rows block");
    assert!(
        unsplit.contains("editing_view(actions.as_ref(), window)"),
        "the unsplit window draws the waiting plan"
    );

    let taps = main
        .split("fn plan_actions(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("the tap builder");
    assert!(
        taps.contains("\"/execute\".to_string()"),
        "Approve sends the command the terminal sends"
    );
    assert!(
        taps.contains("\"/discard\".to_string()"),
        "Discard sends the command the terminal sends"
    );
    assert!(
        taps.contains("send_turn_to(session,"),
        "a tap goes through the composer path that parses the command"
    );
}

#[test]
fn a_wide_pane_shows_the_running_task_and_the_next_two() {
    // #2624: three panes at a readable width keep the detail the user
    // asked for: what is running, and the one or two tasks after it.
    let lines = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Tasks, false);
    assert_eq!(lines.len(), 4, "summary, running, two next: {lines:?}");
    // The bar is a drawn track (#3131), so the data line is the count and
    // the view paints track and percentage beside it.
    assert_eq!(lines[0], (LineKind::Bar, "3/8".to_string()));
    assert_eq!(lines[1], (LineKind::Running, "▸ task 4".to_string()));
    assert_eq!(lines[2].0, LineKind::Next);
    assert!(lines[2].1.starts_with("· task 5"), "{:?}", lines[2]);
    assert!(lines[3].1.starts_with("· task 6"), "{:?}", lines[3]);
    assert!(
        !lines.iter().any(|(kind, _)| *kind == LineKind::Title),
        "a pane spends its rows on the work, not on the plan's title"
    );
}

#[test]
fn a_split_grid_separates_the_disclosure_arrow_from_its_summary() {
    // The renderer owns the disclosure arrow, while the data keeps only
    // the readable summary that belongs beside it.
    let lines = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Bar, false);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0], (LineKind::Bar, "3/8".to_string()));
    // Four to seven keeps the same progress and running task.
    let summary = pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Summary, false);
    assert_eq!(summary.len(), 2, "{summary:?}");
    assert_eq!(summary[0], (LineKind::Bar, "3/8".to_string()));
    assert_eq!(summary[1], (LineKind::Running, "▸ task 4".to_string()));
    assert!(
        !summary.iter().any(|(kind, _)| *kind == LineKind::Title),
        "a summary pane spends its rows on the progress and the task"
    );
}

#[test]
fn a_pane_draws_the_track_instead_of_typing_the_bar() {
    // #3131: the glyph string followed the platform font, so the bar read
    // as noise on Linux and as a track on macOS from the same data. The
    // renderer paints the drawn track and the percentage; no glyph ever
    // reaches the pane again.
    let src = include_str!("../plan_strip.rs");
    let panes = src
        .split("pub fn pane_view(")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub fn ").next())
        .expect("pane view body");
    assert!(
        panes.contains("progress_bar_view(percent, color)"),
        "the pane bar row paints the drawn track"
    );
    assert!(
        !pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Tasks, false)
            .iter()
            .any(|(_, line)| line.contains('█') || line.contains('░')),
        "the pane data never carries glyphs again"
    );
    assert!(
        !pane_lines(&plan_with(8, 3, Some(4)), PaneDensity::Tasks, true)
            .iter()
            .any(|(_, line)| line.contains('█') || line.contains('░')),
        "the expanded pane data never carries glyphs either"
    );
    let track = src
        .split("pub(crate) fn progress_bar_view(")
        .nth(1)
        .expect("track helper")
        .split("}")
        .next()
        .expect("track helper body");
    assert!(
        !track.contains("█") && !track.contains("░"),
        "the drawn track carries no glyph text"
    );
}

#[test]
fn the_density_follows_the_pane_count_and_the_pane_width() {
    // The count decides first, then the measured width, so a narrow
    // column of a three-pane window compacts like a grid leaf.
    assert_eq!(pane_density(2, 600.0), PaneDensity::Tasks);
    assert_eq!(pane_density(3, 400.0), PaneDensity::Tasks);
    assert_eq!(pane_density(4, 400.0), PaneDensity::Summary);
    assert_eq!(pane_density(7, 400.0), PaneDensity::Summary);
    assert_eq!(pane_density(8, 400.0), PaneDensity::Bar);
    assert_eq!(pane_density(2, 300.0), PaneDensity::Summary);
    assert_eq!(pane_density(2, 200.0), PaneDensity::Bar);
}

#[test]
fn an_expanded_pane_lists_every_task() {
    // A click expands the pane's checklist. Every task is listed, in
    // order, and the card scrolls (#3124): the six-row window it used to
    // borrow from the full card cut the plan off in the one place the user
    // had just asked to read all of it.
    let lines = pane_lines(&plan_with(20, 3, Some(4)), PaneDensity::Bar, true);
    assert_eq!(lines[0].0, LineKind::Title);
    assert_eq!(lines[0].1, "Landing · 3/20");
    assert_eq!(lines[1].0, LineKind::Bar);
    assert_eq!(lines.len(), 22, "header, bar and all twenty tasks");
    assert!(lines[2].1.starts_with("1. "), "{:?}", lines[2]);
    assert!(lines[21].1.starts_with("20. "), "{:?}", lines[21]);
    assert!(lines.iter().skip(2).any(|(k, _)| *k == LineKind::Running));
}

#[test]
fn a_pane_names_the_idle_states() {
    let complete = plan_with(4, 4, None);
    let lines = pane_lines(&complete, PaneDensity::Tasks, false);
    assert_eq!(lines[1].1, "✓ complete");
    let waiting = plan_with(4, 1, None);
    assert_eq!(
        pane_lines(&waiting, PaneDensity::Tasks, false)[1].1,
        "▸ waiting"
    );
    let mut seed = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
    seed.status = PlanStatus::Active;
    let lines = pane_lines(&seed, PaneDensity::Tasks, false);
    assert_eq!(lines.len(), 2, "a plan with no checklist is two lines");
    assert_eq!(lines[0].1, "Landing");
    assert!(lines[1].1.contains("checklist not seeded"));
}

#[test]
fn a_pane_keeps_the_whole_task_title() {
    // #3124: the pane cut every title at 32 or 34 characters while most of
    // its card stood empty, because the card was as wide as the column and
    // the cut was a guess rather than a measurement. The lines carry the
    // title whole; the card measures the longest one, and a row only cuts
    // when the pane is genuinely narrower than its own text.
    let mut plan = plan_with(3, 1, Some(1));
    plan.tasks[1].title = "x".repeat(120);
    plan.tasks[2].title = "y".repeat(120);
    let lines = pane_lines(&plan, PaneDensity::Tasks, false);
    assert_eq!(lines[1].0, LineKind::Running);
    assert_eq!(lines[1].1, format!("▸ {}", "x".repeat(120)));
    assert_eq!(lines[2].1, format!("· {}", "y".repeat(120)));
    // The collapsed bar carries the count; the percentage is the view's own
    // job now (#3131), so the pane line stops at "1/3".
    assert_eq!(
        lines[0],
        (LineKind::Bar, "1/3".to_string()),
        "{:?}",
        lines[0]
    );
}

#[test]
fn window_centres_on_the_running_task_and_never_overruns() {
    assert_eq!(visible_window(3, Some(1)), (0, 3));
    assert_eq!(visible_window(20, Some(0)), (0, 6));
    assert_eq!(visible_window(20, Some(10)), (7, 6));
    assert_eq!(visible_window(20, None), (0, 6));
    // Every case stays inside the task list, so no row is ever dropped
    // by a window that starts past the end.
    for total in 1..40 {
        for current in [None, Some(0), Some(total / 2), Some(total - 1)] {
            let (start, len) = visible_window(total, current);
            assert!(start + len <= total, "{total} {current:?} -> {start} {len}");
        }
    }
}

#[test]
fn the_checklist_header_gives_the_title_the_slack() {
    // #2496: the header row packed to the left, so a wide card carried a
    // dead band on the right and a long title pushed the bar out of it.
    // The title takes the slack and truncates; the metrics keep their
    // width at the trailing edge. The row is one helper now, because the
    // approval card prints the same metrics as the live card (#2970).
    let src = include_str!("../plan_strip.rs");
    let body = src
        .split("fn progress_header_row(plan: &PlanDocument, text: String) -> Div {")
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .expect("the header row body");
    assert!(body.contains(".flex_1()"), "the title takes the slack");
    assert!(
        body.contains(".flex_none()"),
        "the metric group keeps its width"
    );
    assert!(body.contains(".truncate()"), "a long title is cut");
    assert!(
        src.contains("progress_header_row(plan, head)")
            && src.contains("progress_header_row(plan, header)"),
        "the live card and the approval card both draw through it"
    );
}

/// #3193: the card leads with its title in bold and the task in flight is
/// cyan. The selectable-text conversion (#2678) dropped the weight silently,
/// and plain body text drowned the running row into the list, so both facts
/// are pinned where a restyle cannot shed them again. `small_bold_style` is
/// the weight-carrying style the completed card draws through, and its own
/// body carries the weight in the style, not on the div.
#[test]
fn the_plan_card_leads_with_bold_and_the_running_task_is_cyan() {
    let src = include_str!("../plan_strip.rs");
    let header = src
        .split("fn progress_header_row(plan: &PlanDocument, text: String) -> Div {")
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .expect("the header row body");
    assert!(
        header.contains(".font_weight(FontWeight::BOLD)"),
        "the card's title is bold"
    );
    let row = src
        .split("fn task_row(task: &PlanTask) -> Div {")
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .expect("the task row body");
    let running = row
        .split("TaskStatus::InProgress) {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("the running branch of the title's colour");
    assert!(
        running.contains("palette::current().teal"),
        "the task in flight is cyan: {running:?}"
    );
    let transcript = include_str!("../transcript.rs");
    let bold = transcript
        .split("pub(crate) fn small_bold_style(size: f32, color: u32) -> TextStyle {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the bold style body");
    assert!(
        bold.contains("font_weight: gpui::FontWeight::BOLD"),
        "the bold style carries its weight in the TextStyle"
    );
    let completed = src
        .split("pub fn completed_plan_card(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the completed card body");
    assert!(
        completed.contains("small_bold_style(SMALL_TEXT_SIZE, palette::current().teal)"),
        "the completed card's header draws through the bold style"
    );
}

/// #2678: a finished plan's card is text the user reads, so each of its
/// lines has to be a selectable segment, spelled exactly as `copy_text`
/// spells it. Otherwise the only thing a drag can copy is the whole card.
#[test]
fn the_completed_card_draws_one_selectable_line_per_copy_line() {
    let src = include_str!("../plan_strip.rs");
    let body = src
        .split("pub fn completed_plan_card(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the completed card body");
    assert_eq!(
        body.matches(["selectable_", "styled_text("].concat().as_str())
            .count(),
        3,
        "the header, the task rows and the footer are all selectable"
    );
    // The draw spells the tick as an escape while `copy_text` spells it as
    // the character, so compare the way the code renders it rather than the
    // bytes it happens to be written with.
    let rendered = body.replace("\\u{2611}", "\u{2611}");
    assert!(
        rendered.contains("\u{2611} {}. {}") && body.contains("task.order, task.title"),
        "a task line is spelled the way copy_text spells it"
    );
    assert!(
        !body.contains(".text_xs()"),
        "a selectable line carries its size in the style, not on the div"
    );
}

/// A plan card is as wide as its words (#3124).
///
/// The cards used to be as wide as the column they sat in, which on a wide
/// window is most of a screen of empty fill behind a two-word plan. Each one now
/// measures its own longest row through the live text system and caps itself
/// there, so the four shapes cannot drift back to a full-width slab one at a
/// time.
#[test]
fn every_plan_card_is_as_wide_as_its_words() {
    let src = include_str!("../plan_strip.rs");
    assert_eq!(
        src.matches(".max_w(px(").count(),
        4,
        "the unseeded row, the approval card, the live checklist and the pane \
         card each cap themselves"
    );
    for (name, anchor, end) in [
        ("unseeded", "pub fn view(", "pub fn editing_view("),
        (
            "approval",
            "pub fn editing_view(",
            "/// The plan as a split pane draws it",
        ),
        (
            "pane",
            "pub fn pane_view(",
            "/// What the approval card calls the plan",
        ),
        ("checklist", "fn checklist(", "/// A historical checklist"),
    ] {
        let card = src
            .split(anchor)
            .nth(1)
            .and_then(|rest| rest.split(end).next())
            .unwrap_or_else(|| panic!("missing the {name} card between {anchor:?} and {end:?}"));
        assert!(
            card.contains(".max_w(px(") && card.contains("card_width("),
            "the {name} card measures its longest row and caps itself there: {card}"
        );
    }
    assert_eq!(
        src.matches("fn fit(").count(),
        0,
        "and no card cuts a title by character count any more"
    );
}

#[test]
fn a_split_draws_the_plan_once_and_the_hint_still_rides_the_chat() {
    // #3145: the chat leaf of a split draws its plan card and its offers in
    // its own chrome (#2530), compact for the width the leaf has (#2624) and
    // pinned so a waiting plan's taps cannot scroll away (#2755). The trailing
    // rows added in #3139 are the unsplit window's copy. Building them in a
    // split as well put two plan cards, and two offer cards, on the chat leaf
    // of every split.
    let src = include_str!("../dashboard_render.rs");
    let body = src
        .split("fn chat_trailing(")
        .nth(1)
        .and_then(|rest| rest.split("fn session_row(").next())
        .expect("chat trailing");
    assert!(
        body.contains("let split = self.window_is_split();"),
        "the trailing rows ask whether the window is split"
    );
    // The hint is built before the gate, because no pane draws one.
    let before_gate = body
        .split("let split = self.window_is_split();")
        .next()
        .expect("the gate");
    assert!(
        before_gate.contains("hint_line(&hint, cx)"),
        "the hint is built for every window, split or not"
    );
    assert_eq!(
        body.matches("if !split").count(),
        2,
        "both the offers card and the plan card are gated on not being split"
    );
    let offers = body.split("if !split").nth(1).expect("the first gate");
    assert!(
        offers.contains("options_strip("),
        "the offers card stands down in a split"
    );
    let plan = body.split("if !split").nth(2).expect("the second gate");
    assert!(
        plan.contains("editing_view("),
        "the plan card stands down in a split"
    );
}
