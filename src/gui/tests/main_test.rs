use crate::composer_view::{composer_hint_for, queue_rows};
use crate::dashboard_messages::{MSG_MENU_TTL, msg_menu_expiry_due};
use crate::dashboard_source::{DASHBOARD, production_half};
use crate::launch::{Launch, launch_intent, pick_initial_session, remember_open_session};
use crate::overlays::cd_entries_for;
use crate::pane_chrome::CHAT_MEASURE_PX;
use crate::status::help_rows;
use crate::topic_bar_view::topic_label;
use crate::window_input::{QUIT_WINDOW, WHEEL_LINE_PX};
use opencrabs::brain::mission_control::McAnalytics;
use opencrabs::ui_core::hints::HintState;

/// The card's line, as the window paints it (#3102).
///
/// The window reads the line with its id, because the card can be dismissed and
/// a dismissal names which one. These tests only care about the words, so they
/// drop the id here rather than reaching into a second door that would have to
/// be kept in step with the first.
fn card_line(state: &HintState) -> Option<String> {
    crate::composer_view::task_hint_for(state).map(|hint| hint.text)
}

/// A leaf is its own view now (#2805): its state and the transcript it
/// draws are scanned here, not in the crate root.
const PANE_VIEW: &str = include_str!("../pane_view.rs");

/// `--version` is answered from the arguments alone (#2841). The flag used
/// to be ignored, so a packaging check booted the whole app: ledger,
/// channels and the A2A port, which is a second instance on one bot token.
#[test]
fn a_version_flag_is_answered_before_the_window_boots() {
    assert_eq!(launch_intent(["--version".to_string()]), Launch::Version);
    assert_eq!(launch_intent(["-V".to_string()]), Launch::Version);
    assert_eq!(launch_intent(["--help".to_string()]), Launch::Help);
    assert_eq!(launch_intent(["-h".to_string()]), Launch::Help);
    // A bare word is the protocol launch's activation URI (#2751), not a
    // flag: that launch still has to open the window.
    assert_eq!(
        launch_intent(["opencrabs://notify".to_string()]),
        Launch::Window
    );
    assert_eq!(launch_intent(Vec::<String>::new()), Launch::Window);
    // A flag nothing answers fails loudly instead of starting the app.
    assert_eq!(
        launch_intent(["--nonsense".to_string()]),
        Launch::Refused("--nonsense".to_string())
    );
}

/// The parse has to run before the log, the ledger or any connector, or a
/// version check still opens the database and binds the port (#2841).
#[test]
fn the_version_flag_is_read_before_the_log_or_the_ledger() {
    let parse = DASHBOARD
        .find("launch_intent(std::env::args()")
        .expect("the launch intent is still read from the arguments");
    let log = DASHBOARD
        .find("logging::init();")
        .expect("the window still initialises its log");
    assert!(
        parse < log,
        "the flag has to be answered before the log and the ledger open"
    );
    let ledger = DASHBOARD
        .find("let db_path = ledger_path();")
        .expect("the window still resolves its ledger");
    assert!(
        parse < ledger,
        "the flag has to be answered before the ledger"
    );
}

/// A live turn repaints on every fourth poll tick, not on every one. The
/// spinner and the clock are the only things moving in that branch, and at
/// 20 Hz each repaint rebuilt every pane's transcript (#2688).
#[test]
fn a_live_turn_repaints_on_every_fourth_poll_tick() {
    assert!(crate::busy_repaint_due(4));
    assert!(crate::busy_repaint_due(0));
    assert!(!crate::busy_repaint_due(1));
    assert!(!crate::busy_repaint_due(2));
    assert!(!crate::busy_repaint_due(3));
    assert!(crate::busy_repaint_due(8));
}

/// A detached finish keeps its runtime on every surface that folds a live
/// turn (#2952). Panes, parked turns and rail cards all fold through here,
/// and this arm used to mark the row done while dropping elapsed_secs, so
/// only the window's own card ever showed how long the command ran.
#[test]
fn a_detached_finish_keeps_its_runtime_when_folded() {
    use crate::transcript;
    use opencrabs::brain::agent::ProgressEvent;

    let mut live = transcript::LiveTurn::default();
    let at = std::time::Instant::now();
    crate::fold_live(
        &mut live,
        ProgressEvent::DetachedTaskStarted {
            task_id: "bg-1".to_string(),
            label: "cargo test".to_string(),
        },
        at,
    );
    crate::fold_live(
        &mut live,
        ProgressEvent::DetachedTaskFinished {
            task_id: "bg-1".to_string(),
            label: "cargo test".to_string(),
            success: true,
            elapsed_secs: 12.5,
        },
        at,
    );

    let row = live
        .tools
        .iter()
        .find(|c| c.task_id.as_deref() == Some("bg-1"))
        .expect("the start drew a row");
    assert!(row.done, "the finish must close the row");
    assert!(row.success, "the finish carries the outcome");
    assert!(
        row.description.contains("12.5s"),
        "the folded row dropped the runtime: {}",
        row.description
    );

    // A finish whose row is already gone is not an error: the turn may have
    // settled and reloaded. It must not invent a row.
    let before = live.tools.len();
    crate::fold_live(
        &mut live,
        ProgressEvent::DetachedTaskFinished {
            task_id: "bg-unknown".to_string(),
            label: "cargo test".to_string(),
            success: false,
            elapsed_secs: 3.0,
        },
        at,
    );
    assert_eq!(live.tools.len(), before, "an unmatched finish adds no row");
}

/// #3021: a strip folded into a card drops the text the service discarded.
/// Panes, parked turns and rail cards fold through here, and the arm used to
/// fall through the catch-all, so a phantom iteration's working-out stayed on
/// screen as the answer until the turn settled and the reload erased it.
#[test]
fn a_stripped_iteration_leaves_the_folded_card() {
    use crate::transcript;
    use opencrabs::brain::agent::ProgressEvent;

    let mut live = transcript::LiveTurn::default();
    let at = std::time::Instant::now();
    crate::fold_live(
        &mut live,
        ProgressEvent::StreamingChunk {
            text: "I cannot use tools.".to_string(),
        },
        at,
    );
    assert_eq!(
        live.answer(),
        Some("I cannot use tools."),
        "the chunk is what the bubble drew"
    );
    crate::fold_live(
        &mut live,
        ProgressEvent::StripStreamedContent {
            bytes: usize::MAX,
            reason: "phantom self-heal narration discarded".to_string(),
        },
        at,
    );
    assert_eq!(
        live.answer(),
        None,
        "the folded card kept drawing the discarded text"
    );
}

/// The conversation caps at a readable measure in a single pane, and any
/// pane narrower than the cap draws at its own width (#2586).
#[test]
fn chat_measure_caps_a_wide_single_pane_only() {
    assert_eq!(crate::chat_measure(2192.0, false), CHAT_MEASURE_PX);
    assert_eq!(
        crate::chat_measure(CHAT_MEASURE_PX + 1.0, false),
        CHAT_MEASURE_PX
    );
    assert_eq!(crate::chat_measure(CHAT_MEASURE_PX, false), CHAT_MEASURE_PX);
    assert_eq!(crate::chat_measure(612.0, false), 612.0);
    assert_eq!(crate::chat_measure(0.0, false), 0.0);
}

/// A split window is exempt: a pane there takes the full span it is
/// given, wide or narrow, so the cap can never narrow a pane (#2586).
#[test]
fn chat_measure_leaves_a_split_pane_alone() {
    assert_eq!(crate::chat_measure(2192.0, true), 2192.0);
    assert_eq!(crate::chat_measure(996.0, true), 996.0);
    assert_eq!(crate::chat_measure(320.0, true), 320.0);
    assert_eq!(crate::chat_measure(CHAT_MEASURE_PX, true), CHAT_MEASURE_PX);
}

/// The measure belongs to the single pane and to nothing else (#2586). A
/// split leaf draws at the span its own pane owns, so the pane renderer is
/// asserted free of the cap: a split can never be narrowed by it, whatever
/// later changes do to the single-pane branch.
#[test]
fn no_measure_cap_reaches_a_split_pane() {
    let src = DASHBOARD;
    // The leaf's own render, from the pane's frame down to its helpers: a
    // split pane builds its whole box there (#2805).
    let (_, rest) = PANE_VIEW
        .split_once(concat!("impl Render for ", "PaneView {"))
        .expect("the pane draws itself");
    assert!(
        !rest.contains("chat_measure") && !rest.contains("CHAT_MEASURE_PX"),
        "a split pane draws at its own full width, never at the measure"
    );

    let (_, primary) = src
        .split_once("let primary = transcript::view(")
        .expect("primary Chat transcript call exists");
    let (call, _) = primary
        // The call ends at the erase site, which now sits in the width
        // branch rather than on the call itself (#2981): a semicolon no
        // longer follows the call, and matching one runs the slice past
        // unrelated code.
        .split_once(".into_any_element()")
        .expect("primary Chat transcript call ends");
    assert!(
        call.contains("measure,"),
        "a single pane draws at the measured width"
    );
    assert!(
        src.contains(
            &[
                "let chat_measure_px = chat_measure(",
                "primary_width, split_window);",
            ]
            .concat()
        ),
        "the measure comes from the pane width, exempting a split window"
    );
}

/// The primary Chat screen must share its mouse selection state with the
/// transcript. The split view already did this; Chat silently passed no
/// state and rendered no selectable rows (#2562).
#[test]
fn primary_chat_transcript_enables_drag_copy() {
    let src = DASHBOARD;
    let (_, after) = src
        .split_once("let primary = transcript::view(")
        .expect("primary Chat transcript call exists");
    let (call, _) = after
        // The erase site, for the same reason as the test above (#2981).
        .split_once(".into_any_element()")
        .expect("primary Chat transcript call ends");
    assert!(
        call.contains(&["drag: Some(self.copy_", "drag.clone()),"].concat()),
        "primary Chat must pass its shared CopyDrag state"
    );
    assert!(
        !call.contains(&["drag:", " None,"].concat()),
        "primary Chat must not disable transcript selection"
    );
}

/// The zoom chords: they need cmd or ctrl, both spellings of plus zoom in,
/// both of minus zoom out, zero resets to the base rem, and any other key
/// does nothing even chorded. Each case is one arm of the handler's own
/// rule, so a later edit cannot drop one silently (#2425).
#[test]
fn zoom_keys_need_the_chord_and_cover_in_out_and_reset() {
    use crate::theme::{BASE_REM, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

    let rem = BASE_REM * 1.15;
    // Without the chord these keys stay text in the composer.
    assert_eq!(crate::zoom_step_for("=", rem, false), None);
    assert_eq!(crate::zoom_step_for("0", rem, false), None);
    // With it, both spellings of each direction act, and zero resets.
    assert_eq!(
        crate::zoom_step_for("=", rem, true),
        Some((rem * ZOOM_STEP).min(BASE_REM * ZOOM_MAX))
    );
    assert_eq!(
        crate::zoom_step_for("+", rem, true),
        crate::zoom_step_for("=", rem, true)
    );
    assert_eq!(
        crate::zoom_step_for("-", rem, true),
        Some((rem / ZOOM_STEP).max(BASE_REM * ZOOM_MIN))
    );
    assert_eq!(
        crate::zoom_step_for("_", rem, true),
        crate::zoom_step_for("-", rem, true)
    );
    assert_eq!(crate::zoom_step_for("0", rem, true), Some(BASE_REM));
    // A key that is not a zoom key stays a keystroke, chord or not.
    assert_eq!(crate::zoom_step_for("a", rem, true), None);
}

#[test]
fn mission_schedule_duration_matches_the_terminal_format() {
    assert_eq!(crate::mission_control_view::mc_schedule_duration(0), "0s");
    assert_eq!(crate::mission_control_view::mc_schedule_duration(42), "42s");
    assert_eq!(
        crate::mission_control_view::mc_schedule_duration(61),
        "1m 1s"
    );
    assert_eq!(
        crate::mission_control_view::mc_schedule_duration(3661),
        "61m 1s"
    );
}

#[test]
fn mission_top_tool_bar_tracks_relative_usage() {
    assert_eq!(
        crate::mission_control_view::mc_tool_bar(0, 100),
        "░".repeat(12)
    );
    assert_eq!(
        crate::mission_control_view::mc_tool_bar(50, 100),
        format!("{}{}", "█".repeat(6), "░".repeat(6))
    );
    assert_eq!(
        crate::mission_control_view::mc_tool_bar(100, 100),
        "█".repeat(12)
    );
    assert_eq!(
        crate::mission_control_view::mc_tool_bar(200, 100),
        "█".repeat(12)
    );
}

#[test]
fn mission_mc_phantoms_empty_breakdown_has_no_rows() {
    let mut analytics = McAnalytics::default();
    analytics.phantom.total = 1;
    assert!(
        crate::mission_control_view::mc_phantoms_rows(&analytics, palette::current()).is_empty(),
        "an empty model breakdown must reach the panel's empty state"
    );

    analytics.phantom.by_model.push(("gpt-5".into(), 1, 0));
    assert_eq!(
        crate::mission_control_view::mc_phantoms_rows(&analytics, palette::current()).len(),
        2,
        "a populated breakdown keeps its totals and model rows"
    );
}

/// A tile opens a preview of the file it stands for, Esc closes it, and
/// the overlay is painted above the panes. Read off the source because a
/// click and a key need a live window: each of these is one line that a
/// later edit could drop without any test noticing (#2395).
#[test]
fn preview_opens_from_a_tile_and_escape_closes_it() {
    let src = DASHBOARD;
    // Every needle is split in two: `include_str!` puts this test's own
    // text inside `src`, so a whole literal would match itself right here
    // and pin nothing at all.
    assert!(
        src.contains(&["this.open_pre", "view(path.clone(), cx)"].concat()),
        "a tile opens the file it stands for"
    );
    assert!(
        src.contains(&["let open = preview::Pre", "view::of(path);"].concat()),
        "opening decides the kind from the path"
    );
    assert!(
        src.contains(&["if self.preview.is", "_some() {"].concat()),
        "Esc closes the preview before anything under it"
    );
    assert!(
        src.contains(&["root = root.child(preview::over", "lay("].concat()),
        "the preview is painted over both panes"
    );
}

/// The seams the attachment fix rests on, pinned by shape so a later
/// refactor cannot quietly drop one (#2395): a tile click reaches the
/// preview, the overlay is painted after the panes, and nothing on a
/// render path shells out to ffmpeg.
#[test]
fn the_attachment_seams_stay_where_they_are() {
    let main = DASHBOARD;
    let transcript = include_str!("../transcript.rs");
    let preview_src = include_str!("../preview.rs");

    // 1. A tile is addressable by the path it stands for, and its click
    //    is the handler the view minted for that path: a picture opens the
    //    file it shows, not whichever tile was planned last.
    assert!(
        transcript.contains(&["format!(\"attach", "ment-{key}\")"].concat()),
        "a tile is addressable by the path it stands for"
    );
    assert!(
        transcript.contains(
            &[
                ".on_click(move |event, window, cx| ",
                "handler(event, window, cx))"
            ]
            .concat()
        ),
        "the tile click forwards into that path's handler"
    );
    assert!(
        main.contains(&["this.open_pre", "view(path.clone(), cx)"].concat()),
        "and that handler lands on the preview"
    );
    // A path can appear in more than one message, so a tile clones its
    // handler instead of taking it: taking it would leave the second tile
    // of the same file drawn but dead.
    assert!(
        transcript.contains(&["handlers.get(&key).clon", "ed()"].concat()),
        "every tile of the same path keeps its click"
    );
    assert!(
        transcript.contains(&["pub type ClickHandler = std::rc::", "Rc<dyn Fn"].concat()),
        "one handler per path, cloned per tile"
    );

    // 2. Painting order is z order in gpui: the overlay has to be a child
    //    added after the panes, or the transcript would take the clicks
    //    meant for it.
    let panes = main.find(&["root = root.child(ri", "ght)"].concat());
    let overlay = main.find(&["root = root.child(preview::over", "lay("].concat());
    let help = main.find(&["root = root.child(help_", "screen("].concat());
    let (Some(panes), Some(overlay), Some(help)) = (panes, overlay, help) else {
        panic!("the paint order changed shape: panes={panes:?} overlay={overlay:?} help={help:?}");
    };
    assert!(overlay > panes, "the preview is painted over the panes");
    assert!(
        overlay < help,
        "the key reference still paints above the preview"
    );

    // 3. A poster is extracted on the click, never while drawing. The
    //    transcript has no business with a subprocess, and the overlay body
    //    only ever receives a frame that was already extracted for it.
    assert!(
        !transcript.contains("ffmpeg"),
        "the transcript never extracts a poster frame"
    );
    let render_path = preview_src
        .split(&["pub fn bo", "dy("].concat())
        .nth(1)
        .and_then(|rest| rest.split(&["fn file_", "body("].concat()).next())
        .expect("the overlay body is one function");
    assert!(
        !render_path.contains("ffmpeg") && !render_path.contains("Command::"),
        "the overlay draws the frame it was handed, it never runs a command to get one"
    );
    assert_eq!(
        main.matches(&["VideoFacts::", "probe("].concat()).count(),
        1,
        "a clip is probed on the click and nowhere else"
    );
}

/// The theme modal is a centred overlay over the conversation: a scrim
/// that dismisses, four cards, and no Keep button, because a pick saves at
/// once. The sidebar, its row helper and its preview history went with the
/// screen that hosted them (#2619).
///
/// A source scan, because the rule is about wiring: the modal needs a
/// window and a click to exercise.
#[test]
fn theme_modal_is_centred_and_saves_on_the_pick() {
    let src = DASHBOARD;
    let view = include_str!("../themes_view.rs");
    // The scrim and the close control belong to the shell now (#2893): it
    // builds both ids from the prefix the modal hands it, so the themes
    // view names the modal and the shell owns what dismisses it.
    let shell = include_str!("../modal_shell.rs");
    let scrim = ["format!(\"{prefix}", "-scrim\")"].concat();
    let modal = ["themes_view::", "modal("].concat();
    let card = [
        "themes_view::card(mode, label, hint, self.themes_card == ",
        "index, accent, shown)",
    ]
    .concat();
    let cards = [
        "for (index, &(mode, label, hint)) in themes_view::CARDS",
        ".iter().enumerate()",
    ]
    .concat();
    let close = ["format!(\"{prefix}", "-close\")"].concat();
    assert!(
        view.contains("id: \"theme-modal\""),
        "the modal names itself, and the shell derives the scrim from it"
    );
    assert!(shell.contains(&scrim), "the scrim is what dismisses");
    assert!(
        shell.contains(&close),
        "the header carries a close control, as the README says"
    );
    assert!(src.contains(&modal), "the modal is built in one place");
    assert!(src.contains(&card), "a card is drawn from the pick table");
    assert!(src.contains(&cards), "four cards, and only four");

    // The sidebar is gone, and so is the draft-and-keep flow it carried.
    for gone in [
        [".id(\"themes-", "sidebar\")"].concat(),
        ["themes_view::", "row("].concat(),
        ["themes_view::history_", "row"].concat(),
        [".id(\"themes-", "keep\")"].concat(),
        ["remember_", "preview"].concat(),
    ] {
        assert!(!src.contains(&gone), "leftover: {gone}");
    }
}

/// One dialog, one delete path. The bridge that erases the row has exactly
/// one call site, and that site is the dialog's own confirm handler. A
/// second call site would mean a gesture can delete a conversation
/// without ever asking, which is the footgun this dialog exists to close.
///
/// A source scan, because the rule is about wiring: the paths themselves
/// need a window and a click to exercise.
#[test]
fn only_the_confirm_dialog_reaches_the_delete_bridge() {
    let src = DASHBOARD;
    // Built in halves so this test's own source does not contain the
    // needle it is counting: the file it reads includes this module.
    let needle = ["spawn_delete", "_session"].concat();

    let sites: Vec<usize> = src
        .match_indices(needle.as_str())
        .map(|(at, _)| at)
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "one delete path only, found {}",
        sites.len()
    );

    let call = sites[0];
    let head = src.get(..call).expect("a call site is a char boundary");
    let owner = head
        .rfind("fn delete_session")
        .expect("the call sits inside delete_session");
    let between = src
        .get(owner..call)
        .expect("a fn header is a char boundary");
    assert!(
        !between.contains("\n    pub(crate) fn "),
        "the bridge must be reached from delete_session itself, not a neighbour"
    );

    // Two arming paths: the row's menu, and ctrl+d on the
    // cursor row (#2352). Both ask the dialog; arming is not deleting.
    // The needles are split so the scan cannot count its own source text.
    let strip_arm = ["this.request", "_delete(id, cx)"].concat();
    let chord_arm = ["self.request", "_delete(target, cx);"].concat();
    assert!(src.contains(&strip_arm), "the row menu must arm the dialog");
    assert!(
        src.contains(&chord_arm),
        "ctrl+d must arm the dialog through the resolved target"
    );
    // A chord that erased on its own would be a second path to the
    // bridge. The dialog's confirm stays the only caller.
    let key_erase = ["this.", "delete_session(id, cx)"].concat();
    assert_eq!(
        src.matches(key_erase.as_str()).count(),
        1,
        "only the dialog's confirm may call delete_session"
    );
    // Esc is the dialog's cancel, and it must answer the dialog before
    // the row menu or the turn it would otherwise abort. The dialog takes
    // its id out to answer, so the branch is the `if let` that owns both
    // keys, and the ladder is the one below it (#2456).
    let dialog = src
        .find("if let Some(id) = self.confirm_delete {")
        .expect("the dialog branch owns the keys");
    let ladder = src
        .find("if ev.keystroke.key == \"escape\" {")
        .expect("the Esc ladder is in the handler");
    assert!(dialog < ladder, "Esc must cancel the dialog first");
}

use crate::*;

/// ctrl+d carries a delete, so the chord must never BE the delete: it
/// resolves one row, then asks the same dialog the row's menu asks
/// (#2352). The bridge stays behind the dialog's confirm button.
#[test]
fn ctrl_d_arms_the_confirm_dialog_not_the_delete() {
    let cursor_row = Uuid::from_u128(11);
    let open_chat = Uuid::from_u128(22);

    let mut state = sessions::SessionsState {
        selected: Some(open_chat),
        ..Default::default()
    };
    assert_eq!(
        state.delete_target(),
        Some(open_chat),
        "with nothing armed the chord targets the open chat"
    );

    state.cursor = Some(cursor_row);
    assert_eq!(
        state.delete_target(),
        Some(cursor_row),
        "the row the keyboard is on wins over the open chat"
    );

    state.cursor = None;
    state.selected = None;
    assert_eq!(
        state.delete_target(),
        None,
        "no row and no open chat means nothing to ask about"
    );

    // The chord is bound on the rail, where arrows already land. A bare
    // `d` would need a rail focus mode the window does not have, so the
    // chord is the only key that may carry a delete.
    let src = DASHBOARD;
    let chord = ["\"d\" if ev.keystroke.modifiers", ".control"].concat();
    assert!(src.contains(&chord), "ctrl+d must be bound on the rail");
}

/// #2721: the Seeker's deep search is wired end to end. The item builder
/// folds the cached content hits into snippet rows, a debounced generation
/// guarded read fills that cache, and both typing and the editing chords
/// schedule it only when the query changes. A source scan, because the
/// spawn needs a live event loop to exercise end to end.
#[test]
fn seeker_deep_search_wiring() {
    let production = production_half();
    assert!(
        production.contains("fn schedule_seeker_find"),
        "the debounced content read exists"
    );
    assert!(
        production.contains("fn refresh_seeker_items"),
        "the card rebuild hook exists"
    );
    assert!(
        production.contains("state.refresh_items(items)"),
        "the rebuild swaps the open card's catalog"
    );
    assert!(
        production.contains("Some(snippet) if !seeker_query.is_empty()"),
        "a content hit stands its snippet in for the date"
    );
    assert!(
        production.contains(".with_keywords(&seeker_query)"),
        "the query keyword surfaces a content-only session"
    );
    assert_eq!(
        production
            .matches("self.seeker_find_if_changed(cx);")
            .count(),
        3,
        "typing, the editing chords and a dictated take all schedule on a query change"
    );
    assert!(
        production.contains("this.seeker_find_gen == generation"),
        "a stale read never rebuilds the card"
    );
}

/// A conversation's turn belongs to that conversation, not to the window.
///
/// #2616: the window keeps ONE live turn, so switching away used to throw
/// it out and a running turn went dark the moment the user looked at
/// another chat: no spinner, no tool block, counters frozen. Parking the
/// turn under its own session and handing it back on the way in is what
/// makes the card survive a switch.
#[test]
fn a_turn_travels_with_its_session_across_a_switch() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();

    let mut stashed: std::collections::HashMap<uuid::Uuid, crate::chat_leaf::Turn> =
        std::collections::HashMap::new();

    // A turn running in conversation A, with every piece of state the
    // card needs to look alive. Declared already carrying it, so the test
    // reads as the turn it describes and never as a blank that is filled
    // in later.
    let mut turn = crate::chat_leaf::Turn {
        live: crate::transcript::LiveTurn::default(),
        busy: true,
        cancel: Some(tokio_util::sync::CancellationToken::new()),
        ctx_tokens: Some(1_234),
        settle: crate::chat_leaf::Settle::default(),
    };
    turn.live.begin("ask A".to_string());
    turn.live.thinking = true;
    turn.live.tokens = 42;
    turn.settle.expect = Some(7);
    turn.settle.retries = 2;
    turn.settle.rechecks = 1;
    turn.settle.deadline = Some(std::time::Instant::now());

    crate::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

    assert_eq!(
        stashed.len(),
        1,
        "the running turn is parked under its own session, never dropped"
    );
    assert_eq!(stashed[&a].live.user.as_deref(), Some("ask A"));
    assert!(
        stashed[&a].live.thinking,
        "the spinner keeps its state while parked"
    );
    assert_eq!(stashed[&a].live.tokens, 42, "the counter survives a switch");
    assert!(
        stashed[&a].cancel.is_some(),
        "the parked turn keeps its cancel token"
    );
    assert_eq!(stashed[&a].settle.expect, Some(7));
    // The whole handshake travels now, not just the two fields the old
    // parameter list happened to name: the deadline and the late
    // re-checks belong to the turn that armed them (#2805).
    assert_eq!(stashed[&a].settle.retries, 2);
    assert_eq!(stashed[&a].settle.rechecks, 1);
    assert!(
        stashed[&a].settle.deadline.is_some(),
        "the wait's own budget parked with the turn it was measuring"
    );
    assert!(!turn.busy, "the conversation now on screen is idle");
    assert!(turn.live.user.is_none(), "B never inherits A's card");
    assert_eq!(turn.ctx_tokens, None, "nor A's context counter");

    // B runs its own turn, then the user goes back to A.
    turn.live.begin("ask B".to_string());
    turn.busy = true;
    crate::swap_visible_turn(&mut stashed, Some(b), a, &mut turn);

    assert_eq!(
        turn.live.user.as_deref(),
        Some("ask A"),
        "A's card comes back, so its tool block and counters keep ticking"
    );
    assert!(turn.live.thinking);
    assert_eq!(turn.live.tokens, 42);
    assert!(turn.busy, "A is still running");
    assert!(turn.cancel.is_some(), "A's cancel token came back with it");
    assert_eq!(turn.settle.expect, Some(7));
    assert_eq!(turn.settle.retries, 2);
    assert_eq!(turn.ctx_tokens, Some(1_234));
    assert_eq!(stashed.len(), 1);
    assert_eq!(
        stashed[&b].live.user.as_deref(),
        Some("ask B"),
        "B's turn was parked on the way out"
    );

    // Opening the conversation already on screen is not a switch: nothing
    // is parked and the running turn is left alone.
    crate::swap_visible_turn(&mut stashed, Some(a), a, &mut turn);
    assert_eq!(stashed.len(), 1, "a no-op switch parks nothing");
    assert_eq!(turn.live.user.as_deref(), Some("ask A"));
    assert!(turn.busy);
}

/// #2811: stopping a turn clears `busy` immediately, and the park used to
/// be gated on `busy` alone. A turn the user had just stopped was therefore
/// dropped on the way out of the session and came back as an empty pane,
/// which is why the stop looked like it had never happened.
#[test]
fn a_stopped_card_survives_a_switch_without_claiming_it_is_running() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut stashed: std::collections::HashMap<uuid::Uuid, crate::chat_leaf::Turn> =
        std::collections::HashMap::new();
    let mut turn = crate::chat_leaf::Turn::default();
    turn.live.begin("ask A".to_string());
    turn.live.stream = "half an answer".to_string();
    turn.live.stopped = true;
    // What the stop already did: nothing is running any more.
    turn.busy = false;

    crate::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

    assert_eq!(
        stashed.len(),
        1,
        "the stopped card is parked, never dropped for not running"
    );
    assert!(
        !stashed[&a].busy,
        "it is parked knowing nothing is running, so it cannot spin on return"
    );
    assert!(stashed[&a].live.stopped, "and knowing it was stopped");

    crate::swap_visible_turn(&mut stashed, Some(b), a, &mut turn);

    assert_eq!(
        turn.live.stream, "half an answer",
        "the stream the user saw came back with the card"
    );
    assert!(turn.live.stopped, "and so did the fact it was stopped");
    assert!(
        !turn.busy,
        "a restored stop must not claim the turn is running: that is the frozen spinner"
    );
    assert!(stashed.is_empty(), "the card moved back onto the screen");
}

/// The park condition is about the card, not the turn: an idle
/// conversation still parks nothing (#2616), so switching around a profile
/// cannot accumulate empty entries (#2811).
#[test]
fn an_idle_conversation_parks_nothing() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut stashed: std::collections::HashMap<uuid::Uuid, crate::chat_leaf::Turn> =
        std::collections::HashMap::new();
    let mut turn = crate::chat_leaf::Turn::default();

    crate::swap_visible_turn(&mut stashed, Some(a), b, &mut turn);

    assert!(
        stashed.is_empty(),
        "nothing was on screen, so nothing had to travel"
    );
}

/// The window keeps exactly one live turn, so every paint hands it over
/// directly: no owner check can disagree with what is on screen, because
/// the swap already did the deciding (#2616).
#[test]
fn every_paint_hands_over_the_turn_the_window_holds() {
    let production = production_half();
    assert!(
        !production.contains("live_owner"),
        "ownership is the parking map's job now, not a parallel ledger"
    );
    assert!(
        !production.contains("visible_live("),
        "the paint path no longer gate-keeps the card"
    );
    // The transcript takes the visible turn by reference, not as an
    // Option: the window keeps exactly one and swaps it on a switch
    // (#2616). Four handovers belong to the window chat now, all of them
    // the same turn: the attachment-tile scan, the action rows behind the
    // reader's own live entries (#2925), the edit slot sequence that
    // rewrites one of those entries (#2925), and the body that paints it.
    // A split pane paints from its own `PaneView::live`, and the Themes
    // preview leaf the centered modal replaced is gone (#2619).
    // The fifth is the append, not a paint (#3032): the turn that just
    // finished is converted to a transcript row from the same live card,
    // so the reader sees it without a refetch and without a second turn
    // being read anywhere.
    assert_eq!(
        production.matches("&self.turn.live,").count(),
        5,
        "the attachment scan, the live action rows, the edit sequence, the chat body and the finished turn's append all read the window's turn"
    );
    assert_eq!(
        production.matches("swap_visible_turn(").count(),
        2,
        "one definition, one caller: opening a conversation"
    );
    // A turn that finishes in a conversation the user left retires its
    // parked card on every terminal path: the success and failure arms,
    // and the cancelled settle a stop produces instead of either (#2657).
    assert_eq!(
        production.matches("self.stashed.remove(&session);").count(),
        3,
        "Done, Failed and Cancelled all drop the parked card"
    );
}

/// A turn recovered at boot is a turn like any other, and it is painted on
/// the conversation that owns it (#2627): on screen it takes the window's
/// card, off screen it parks under its session (#2616), so switching to it
/// shows the recovered work instead of an idle pane.
#[test]
fn a_resumed_turn_streams_into_its_pane() {
    let src = DASHBOARD;
    let arm = src
        .split("agent_bridge::TurnEvent::TurnResumed {")
        .nth(1)
        .expect("the resumed-turn arm");
    let arm = arm
        .split("agent_bridge::TurnEvent::")
        .next()
        .expect("the end of the arm");
    assert!(
        arm.contains("self.turn.live.resume(user_message)"),
        "the card names the message the killed process was answering, and says its row is already written"
    );
    assert_eq!(
        arm.matches("resume(user_message)").count(),
        2,
        "both the on-screen card and the parked copy are resumed: each draws a row the ledger already holds"
    );
    assert!(
        !arm.contains("begin(user_message)"),
        "a resumed turn that starts as a fresh one stamps the resume time and paints the question twice (#2740)"
    );
    assert!(
        arm.contains("self.turn.busy = true"),
        "a recovered turn is busy"
    );
    assert!(
        arm.contains("self.turn.cancel = Some(cancel)"),
        "stop has to end a recovered turn too"
    );
    assert!(
        arm.contains("self.stashed.insert("),
        "an off-screen recovery parks under its own session"
    );
    assert!(
        !arm.contains("reload = Some(session)"),
        "a reload landing on a running turn would wipe its card"
    );
}

#[test]
fn the_running_task_strip_sits_right_in_teal() {
    // #2617: the strip reads the same on both surfaces, and the GUI parks
    // it above the composer on the chat's right edge in the palette's
    // cyan. Left-aligned amber was neither.
    //
    // The strip is a module of its own now (#3060) and its functions are
    // `pub(crate) fn`, so a `\nfn ` cut runs past the end of it and the
    // negative below reads amber out of a module that is not the strip.
    // The scan takes the item's own body instead.
    let src = DASHBOARD;
    let strip = fn_body(src, "task_strip");
    assert!(strip.contains(".justify_end()"), "the strip sits right");
    assert!(strip.contains(".w_full()"), "the row owns the column width");
    assert!(
        strip.contains("palette::current().teal"),
        "a running job reads cyan"
    );
    assert!(
        !strip.contains("palette::current().amber"),
        "amber is not the running colour"
    );
}

#[test]
fn the_running_task_strip_keeps_a_gutter_in_a_narrow_pane() {
    // #2622: full width is not the whole story. The label keeps a gutter
    // and a clip, so it stops at the pane's padding instead of touching
    // the border, and a long command truncates instead of painting over
    // the neighbouring leaf.
    let src = DASHBOARD;
    let strip = fn_body(src, "task_strip");
    assert!(strip.contains(".px_2()"), "the strip keeps a gutter");
    assert!(strip.contains(".truncate()"), "a long label ellipsizes");
    assert!(strip.contains(".min_w_0()"), "the label may shrink");
}

#[test]
fn the_running_task_strip_copies_its_label_on_a_drag() {
    // #2678: the strip names the job the user wants to quote back ("that
    // command is stuck"). It is one truncated line with no character grid,
    // so the gesture is the copy: a press, then a release far enough away
    // to be a drag, writes the whole label. A plain click stays a click.
    let src = DASHBOARD;
    let strip = fn_body(src, "task_strip");
    assert!(
        strip.contains(".on_mouse_down(MouseButton::Left"),
        "the press is recorded"
    );
    assert!(
        strip.contains(".on_mouse_up(MouseButton::Left"),
        "the release decides whether it was a drag"
    );
    assert!(
        strip.contains("magnitude() <= 2.0"),
        "a release inside the threshold is a click, not a copy"
    );
    assert!(
        strip.contains("write_to_clipboard(ClipboardItem::new_string"),
        "a drag writes the label to the clipboard"
    );
    assert!(
        strip.contains("let copied = format!("),
        "the whole label is copied, not the truncated part"
    );
}

#[test]
fn every_pane_owns_its_drag_to_copy_state() {
    // #2623: a pane transcript was built with `ViewHooks::default()`,
    // which leaves `drag: None`, so dragging over a split pane selected
    // nothing and copied nothing. The window's state cannot be shared:
    // row text is stored by ordinal, and two panes plus the window all
    // count rows from zero.
    let src = DASHBOARD;
    assert!(
        !PANE_VIEW.contains("ViewHooks::default()"),
        "a pane must hand its transcript a drag state"
    );
    // The hooks are the leaf's own now (#2805), built where the leaf
    // draws rather than where the walk used to.
    assert!(
        PANE_VIEW.contains("drag: Some(self.copy_drag.clone())"),
        "the pane's own drag state is wired to its transcript"
    );
    let state = PANE_VIEW
        .split("pub(crate) struct PaneView {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("the pane view");
    assert!(
        state.contains("copy_drag: Rc<CopyDrag>"),
        "each pane carries its own drag state"
    );
    // The pane's composer is the one that acknowledges the copy, because
    // the transcript cannot reach an editor (#2587).
    assert!(
        src.contains("pane_copy.set_copied_listener"),
        "the pane's own composer says the copy landed"
    );
    assert!(
        src.contains("pane_editor.downgrade()"),
        "the listener targets the pane's editor"
    );
}

/// The Seeker prints the window-only rows and routes them into the doors
/// that already exist: the group modal and the split pick (#2712, #2713).
#[test]
fn the_seeker_prints_the_window_only_rows_and_routes_them() {
    let code = production_half();
    assert!(
        code.contains("items.extend(actions::gui_items());"),
        "the window adds the rows that need a window"
    );
    let finder = fn_body(code, "finder_items");
    assert!(
        finder.contains("actions::gui_items()"),
        "the picker carries them"
    );

    // The arm is read from the glyph that follows the needle, never from
    // the needle itself: the needle starts with `PaletteAction::`, so a
    // bare `find` on the tail returns zero and proves nothing.
    let group_pick = "PaletteAction::GroupTopicsOn(id) =>";
    let handler = code.find(group_pick).expect("the group pick has a handler");
    let tail = code
        .get(handler + group_pick.len()..)
        .expect("the arm starts on a glyph");
    let arm = tail.split("PaletteAction::").next().expect("the arm body");
    assert!(
        arm.contains("start_group_topics(Uuid::from_u128(id), cx)"),
        "the pick opens the same modal the rail menu opens"
    );

    let split_row = "PaletteAction::BeginSplit(layout) =>";
    let split = code.find(split_row).expect("the split row has a handler");
    let tail = code
        .get(split + split_row.len()..)
        .expect("the arm starts on a glyph");
    let arm = tail.split("PaletteAction::").next().expect("the arm body");
    assert!(
        arm.contains("self.begin_split(layout, cx)"),
        "the row walks the same split door the chord walks"
    );

    // The microphone (#3044): the card cannot reach the composer's own
    // state, so the row hands the job to the field the keyboard owns,
    // through the same call the control's click and the chord make.
    let mic_row = "PaletteAction::ToggleDictation =>";
    let mic = code
        .find(mic_row)
        .expect("the microphone row has a handler");
    let tail = code
        .get(mic + mic_row.len()..)
        .expect("the arm starts on a glyph");
    let arm = tail.split("PaletteAction::").next().expect("the arm body");
    assert!(
        arm.contains("self.composer_entity(cx)"),
        "the row targets the field the keyboard owns, not the window's"
    );
    assert!(
        arm.contains("editor.toggle_dictation(crate::editor::TakeEnd::Park, cx)"),
        "the row parks the words: a query is worth reading before it runs"
    );
    // The chord is the one caller that sends (#3092): the microphone's click
    // and the block's click park, so the words wait for Enter exactly as they
    // always did, and only the fast path spends the take.
    let editor_src = include_str!("../editor.rs");
    let chord = fn_body(editor_src, "toggle_voice");
    assert!(
        chord.contains("self.toggle_dictation(TakeEnd::Send, cx)"),
        "the chord sends the moment the words land"
    );
    assert!(
        fn_body(editor_src, "mic").contains("this.toggle_dictation(TakeEnd::Park, cx)"),
        "the idle control parks: a click is not a decision to send"
    );
    assert!(
        fn_body(editor_src, "voice_block").contains("this.toggle_dictation(TakeEnd::Park, cx)"),
        "the block parks: stopping the take is not sending it"
    );

    // The field's own microphone (#3044): the card records in place. It
    // owns the capture, so the words land in the field on screen instead of
    // a composer hidden behind the card.
    let dictate = "pub(crate) fn seeker_dictate(";
    let handler = code
        .find(dictate)
        .expect("the field's microphone has a handler");
    let tail = code.get(handler..).expect("the handler starts on a glyph");
    let body = tail.split("\n    }\n").next().expect("the handler body");
    assert!(
        body.contains("voice_input::Capture::start()"),
        "the card opens the device itself instead of handing the take to the composer"
    );
    assert!(
        !body.contains("close_finder(cx)") && !body.contains("toggle_dictation"),
        "the card stays up: a take handed to the composer records behind it, unseen"
    );
    assert!(
        body.contains("self.seeker_voice_send = true;"),
        "the chord asks for the words to be searched the moment they land"
    );

    // The other way out of a take (#3044): clicking the glass lands the
    // words without searching them, so a half-heard query can be read and
    // corrected first.
    let edit = "pub(crate) fn seeker_stop_for_edit(";
    let stop = code.find(edit).expect("the glass click has a handler");
    let tail = code.get(stop..).expect("the handler starts on a glyph");
    let stop_body = tail.split("\n    }\n").next().expect("the stop body");
    assert!(
        stop_body.contains("self.seeker_voice_send = false;"),
        "stopping from the bubble hands the words back instead of sending them"
    );

    // Both paths come back to the Seeker (#3044): the landing reopens the
    // card rather than writing into the composer behind it.
    let land = fn_body(code, "land_seeker_voice");
    assert!(
        land.contains("self.open_finder(cx)"),
        "a take that outlived its card reopens the card"
    );
    assert!(
        !land.contains("composer_entity"),
        "the words never go to the composer: the Seeker is where they are read and sent"
    );
}

#[test]
fn a_wheel_tick_up_releases_the_follow_it_does_not_fight_the_scroll() {
    // #2698: the window follows its own bottom until the reader's hand
    // says otherwise. GPUI applies the wheel to the viewport itself, so
    // the listener may only drop the follow; fighting the scroll there
    // would double-apply it. Both surfaces mint their own tick, and each
    // one drops only its own follow.
    // Read the production half only: this test lives in the same file it
    // reads, so its own needles would count as hits and prove nothing.
    let code = production_half();
    // The pane's own tick lives with the leaf's state now (#2805), so the
    // two halves of #2698 are read where each one lives.
    let view = PANE_VIEW;
    assert!(
        code.contains("wheel_releases_follow(wheel_dy(&ev.delta))"),
        "the wheel is judged by the shared rule, never a second one"
    );
    assert!(
        code.contains("this.release_transcript_follow();"),
        "a tick over the conversation releases the window's follow"
    );
    assert!(
        view.contains("this.release_pane_follow(session, cx);"),
        "a tick over a pane releases that pane's follow, not the window's"
    );
    assert_eq!(
        view.matches("wheel: Some(Rc::new(wheel))").count(),
        1,
        "every pane boxes the tick it mints"
    );
    assert_eq!(
        code.matches("wheel: Some(wheel),").count(),
        1,
        "the conversation hands over the tick it already owns"
    );
}

#[test]
fn a_wheel_tick_reads_the_same_in_pixels_and_in_lines() {
    let pixels = ScrollDelta::Pixels(gpui::point(px(0.0), px(-30.0)));
    assert_eq!(wheel_dy(&pixels), -30.0);
    assert!(!wheel_releases_follow(wheel_dy(&pixels)));
    let lines = ScrollDelta::Lines(gpui::point(0.0, 3.0));
    assert_eq!(wheel_dy(&lines), 3.0 * WHEEL_LINE_PX);
    assert!(wheel_releases_follow(wheel_dy(&lines)));
    // A trackpad tick that only moves sideways scrolls nothing vertically.
    let sideways = ScrollDelta::Pixels(gpui::point(px(-12.0), px(0.0)));
    assert_eq!(wheel_dy(&sideways), 0.0);
    assert!(!wheel_releases_follow(wheel_dy(&sideways)));
}

#[test]
fn page_up_releases_the_follow_and_page_down_gives_it_back_at_the_end() {
    // #2698: the page keys are the same gesture as the wheel, and the
    // terminal binds them the same way. Down re-arms only when the page
    // lands on the end, so a row that lands mid-page cannot drag a reader
    // back down a screen they just left.
    //
    // #2959: both answers come from `page_landing`, whose arithmetic is
    // tested in `ui_core::scroll`. Asking whether the clamped offset had
    // reached the maximum was true by construction, so every key re-armed
    // and PageUp was snapped straight back to the bottom.
    let src = DASHBOARD;
    let branch = src
        .split("matches!(key, \"pageup\" | \"pagedown\")")
        .nth(1)
        .and_then(|rest| rest.split("if key == \"k\"").next())
        .expect("the page key branch");
    assert!(
        branch.contains("page_landing("),
        "the landing and whether it is the end come from page_landing"
    );
    assert!(
        branch.contains("match (target, landing.at_end)"),
        "the follow is decided by the landing page_landing reported"
    );
    assert!(
        !branch.contains("y >= -max"),
        "a clamped offset always reaches the maximum, so comparing them says the end every time"
    );
    assert!(
        branch.contains("(None, true) => self.rearm_transcript_follow()"),
        "a page down that reaches the end re-arms the conversation"
    );
    assert!(
        branch.contains("(Some(id), false) => self.release_pane_follow(id, cx)"),
        "a page up releases the pane it scrolled"
    );
}

#[test]
fn a_panel_that_closes_hands_the_chat_back_at_its_newest_row() {
    // #2698: coming back from a panel is a return, not a resume. The
    // screen transition is the one place every panel funnels through, so
    // the re-arm lives there instead of in each close path.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    assert!(
        render.contains("self.screen == Screen::Chat && self.last_screen != Screen::Chat"),
        "the transition into the chat is detected"
    );
    assert!(
        render.contains("self.rearm_transcript_follow();"),
        "and it hands the reader the newest row"
    );
    assert!(
        src.contains("last_screen: Screen::Chat,"),
        "the window starts on the chat, so the first frame is not a return"
    );
    // Sending is the other half of the contract: a send follows its own
    // answer whatever the reader was doing before. Since #2756 the send is
    // two functions, the entry and the one that runs a turn, so the follow
    // is pinned in the half that actually sends.
    assert!(
        fn_body(src, "send_message").contains("self.send_message_showing("),
        "a send reaches the path that runs a turn"
    );
    assert!(
        fn_body(src, "send_message_showing").contains("self.rearm_transcript_follow();"),
        "a send re-arms the follow"
    );
}

/// The body of `fn name(` up to the next item at the same indent.
fn fn_body(src: &'static str, name: &str) -> &'static str {
    item_body(src, &format!("fn {name}("))
}

/// The Dashboard's window render. Two impls in this file define `render`,
/// and only this signature is the one that holds the window (#2741).
fn dashboard_render(src: &'static str) -> &'static str {
    // Scoped to the window's own `Render` impl first: the rail is a view of
    // its own now (#2933) and its render carries the same signature, so a
    // bare split would read the rail's body instead of the window's.
    item_body(
        item_body(src, "impl Render for Dashboard"),
        "fn render(&mut self, window: &mut Window, cx: &mut Context<Self>)",
    )
}

/// The body of one item, from its own definition to its closing brace.
///
/// Two things this gets right that a plain `split` does not: only a real
/// definition counts, because the scans below quote the names they look
/// for inside their own strings, and the body ends at the brace that
/// closes it rather than at the next method, so a top level function in
/// another module file reads the same as a `Dashboard` method (#2741).
fn item_body(src: &'static str, needle: &str) -> &'static str {
    let at = src
        .match_indices(needle)
        .map(|(at, _)| at)
        .find(|&at| {
            let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
            let prefix = src[line..at].trim();
            prefix.is_empty() || prefix.starts_with("pub")
        })
        .unwrap_or_else(|| panic!("{needle} not found"));
    let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let indent = src[line..at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let close = format!("\n{}}}", " ".repeat(indent));
    let rest = &src[at..];
    rest.split(&close).next().unwrap_or(rest)
}

#[test]
fn a_running_turn_says_what_it_is_doing_on_its_card() {
    // #2718: the card drew only the last persisted message, so a
    // conversation running its first tool calls claimed to be idle, and a
    // sub-agent working with no pane at all said nothing anywhere.
    let mut live = transcript::LiveTurn::default();
    assert_eq!(
        rail_activity_line(&live),
        "",
        "a turn with nothing to report leaves the preview in its slot"
    );
    live.thinking = true;
    assert_eq!(
        rail_activity_line(&live),
        "OpenCrabs is thinking...",
        "the pane's own wording, not a second one for the rail"
    );
    live.thinking = false;
    live.tools.push(transcript::ToolCall::running(
        "bash".to_string(),
        "cargo test".to_string(),
    ));
    let line = rail_activity_line(&live);
    assert!(
        line.contains("cargo test"),
        "the call in flight is what the card names: {line}"
    );

    let src = DASHBOARD;
    // A session with no leaf keeps its turn here, folded by the same
    // function the panes use, so the two cannot describe it differently.
    let fold = fn_body(src, "fold_pane_progress");
    assert!(
        fold.contains("self.rail_live.get_mut(&session)"),
        "the event is kept, not just a streaming flag"
    );
    assert!(
        fold.contains("fold_live(live, progress, at)"),
        "the pane's own fold gives the card its line"
    );
    // The card takes the activity over the preview while it lasts, and
    // the render supplies it from the row map.
    let sessions = include_str!("../sessions.rs");
    let row = fn_body(sessions, "row");
    assert!(
        row.contains("let preview = activity.or(preview);"),
        "the live line takes the preview's own slot"
    );
    assert!(
        row.contains("live_line"),
        "and reads in the accent colour while it is live"
    );
    // The frame the rail is handed, resolved in the rail's own module
    // (#2933): still the same owner lookup, one step away from the paint.
    let frame = src
        .split("flags: rail_view::RailRowFlags {")
        .nth(1)
        .expect("the rail's frame")
        .split("// The channel chip (#2452)")
        .next()
        .expect("the frame ends at the footer");
    assert!(
        frame.contains("activity: self.rail_activity_for(progress_id, cx)"),
        "the frame feeds the card from whichever owner holds the turn"
    );
    let lookup = fn_body(src, "rail_activity_for");
    for source in [
        // The split left the leaf in one map and the parked ones in
        // another, so the lookup asks `pane_view` for both (#2871).
        "self.pane_view(id)",
        "self.stashed.get(&id)",
        // The open session's turn reads through the hoisted state since
        // the chat leaf moved its session out of the window (#2805).
        "self.turn.busy && self.chat_state.session() == Some(id)",
        "self.rail_live.get(&id)",
    ] {
        assert!(
            lookup.contains(source),
            "a turn can be held by any of the four owners: {source}"
        );
    }
    // Every way a turn ends retires the line, or a card would keep
    // claiming work after it stopped.
    let poll = fn_body(src, "poll");
    let retires = poll.matches("self.rail_live.remove(&session);").count();
    assert_eq!(retires, 3, "Done, Failed and Cancelled each clear it");
}

/// #2816: the card's live line leads with the paragraph the turn spoke and
/// names the call after it. Narration is occasional, so the call still owns
/// the line on the turns that never speak.
#[test]
fn a_speaking_turn_puts_its_paragraph_before_the_call() {
    let mut live = transcript::LiveTurn::default();
    live.push_narration("The stamp reads the parent row, not the topics.");
    live.tools.push(transcript::ToolCall::running(
        "bash".to_string(),
        "sed -n 1791p sessions.rs".to_string(),
    ));
    let line = rail_activity_line(&live);
    let prose = line
        .find("The stamp reads the parent row")
        .expect("the paragraph is on the line");
    let call = line
        .find("sed -n 1791p")
        .expect("the call is on the line too");
    assert!(prose < call, "the paragraph leads the call: {line}");
}

/// Prose with no call yet is the whole line: a second "thinking" label
/// beside the sentence the turn just said tells the reader nothing.
#[test]
fn a_speaking_turn_with_no_call_yet_drops_the_thinking_placeholder() {
    let mut live = transcript::LiveTurn::default();
    live.push_narration("Reading the rail row now.");
    let line = rail_activity_line(&live);
    assert!(line.contains("Reading the rail row now."), "{line}");
    assert!(!line.contains("thinking"), "no second label: {line}");
}

/// Narration alone is enough to put a line on the card, and a turn that has
/// said nothing at all still leaves the preview in its slot.
#[test]
fn narration_alone_is_enough_to_show_a_card_line() {
    let mut live = transcript::LiveTurn::default();
    assert_eq!(rail_activity_line(&live), "");
    live.push_narration("Found the column.");
    assert_eq!(rail_activity_line(&live), "Found the column.");
}

#[test]
fn the_menu_opens_at_the_pointer_that_asked_for_it() {
    // #2690: the press point is the anchor. The card used to be placed
    // beside the rail at the press height, which is not where the click
    // happened: a chip sits mid-row, and in a split the rail edge is not
    // the pane the user was looking at.
    let src = DASHBOARD;
    let builder = fn_body(src, "rail_menu_popover_for");
    assert!(
        builder
            .split_whitespace()
            .collect::<String>()
            .contains("self.rail_menu_at"),
        "the press the row recorded is what places the card"
    );
    assert!(
        !builder.contains("rail_width"),
        "the rail's width no longer decides where the card opens"
    );
    assert!(
        builder.contains("sessions::menu_popover(\"rail-context-menu\", items, at)"),
        "the card goes through the one pointer-anchored popover"
    );
}

#[test]
fn the_rail_menu_is_anchored_at_the_press_through_one_popover() {
    // #2690: the card was absolute inside the row at `right_1` / `top_1`
    // (#2742), then placed beside the rail at the press height. Both
    // answered "where is the row"; the pointer is what the user clicked.
    let menu = include_str!("../menu.rs");
    let popover = fn_body(menu, "menu_popover");
    assert!(
        popover.contains("anchored()"),
        "the card is anchored in window coordinates, not absolute in the row"
    );
    assert!(
        popover.contains(".position(at)"),
        "the press point is the anchor it opens from"
    );
    assert!(
        popover.contains(".snap_to_window_with_margin("),
        "a card near the window edge is pulled back inside it"
    );
    let src = DASHBOARD;
    // The window render builds it at the root: a row's own frame would
    // translate the window coordinates to that row's corner.
    let render = dashboard_render(src);
    assert!(
        render.contains("gpui::deferred(menu).with_priority(2)"),
        "the menu is attached at the window root, above the content"
    );
    assert!(
        render.contains("self.rail_menu_popover_for(id, cx)"),
        "the root render is what builds the open row's card"
    );
    // Every entry point records where the press landed: a row, a chip,
    // and the swipe (#2797). Counted inside the render, because this
    // scan's own string holds the same text and a whole-file count would
    // read one more.
    // The swipe still lands here, in the window's own move handler. The
    // row and the chip are built by the rail's view now, so they ask the
    // window by name and it records the point it is handed (#2933).
    let asks = dashboard_render(src)
        .matches("this.rail_menu_at = Some(ev.position);")
        .count();
    assert_eq!(
        asks, 1,
        "the swipe records its press where it happens; the other two ask"
    );
    // Split so this scan cannot count its own text.
    let recorded = ["self.rail_menu_at = Some", "(at);"].concat();
    assert_eq!(
        src.matches(&recorded).count(),
        2,
        "the row and the chip each record the point the view handed them"
    );
    let builder = fn_body(src, "rail_menu_popover_for");
    assert!(
        builder
            .split_whitespace()
            .collect::<String>()
            .contains("self.rail_menu_at"),
        "the card opens at the point the press recorded"
    );
    assert!(
        !builder.contains("rail_width"),
        "and not at the rail's edge, which is not where the click landed"
    );
}

#[test]
fn rail_slide_opens_the_menu_not_a_strip() {
    // #2797: the slide used to open a flex_none strip beside the body,
    // which clipped the title instead of shrinking. It now opens the
    // same rail menu the right-click opens, and the strip's state is
    // gone for good, not parked behind a flag. The needle is split so
    // the scan cannot count its own source text.
    let src = DASHBOARD;
    let dead = ["rail_", "actions"].concat();
    assert!(
        !src.contains(&dead),
        "the strip's state is removed from every site"
    );
    let slide = dashboard_render(src)
        .split("sessions::slide_reveals(from_x")
        .nth(1)
        .expect("the slide handler")
        .split("cx.notify();")
        .next()
        .expect("the slide block");
    assert!(
        slide.contains("&& this.sessions.rail_menu != Some(id)"),
        "a slide on the row whose menu is already open changes nothing"
    );
    assert!(
        slide.contains("this.sessions.rail_menu = Some(id);"),
        "a slide past the threshold opens the row's menu"
    );
    assert!(
        slide.contains("this.rail_menu_at = Some(ev.position);"),
        "the slide anchors the menu where the pointer landed"
    );
    assert!(
        slide.contains("this.rail_slide_opened = Some(id);"),
        "the release that ends a slide must not pick the row"
    );
}

#[test]
fn a_release_clears_the_slide_before_a_chip_can_swallow_it() {
    // #2911: the chip, the "+N" pill and the collapse pill all stop the
    // bubble in their click listeners, so the root's mouse-up never ran for
    // a press on any of them. The armed swipe outlived the click and the
    // next pointer move opened the card's menu on a plain left click.
    // Capture runs root first, so no child can swallow the clear.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    let clear = render
        .split("capture_any_mouse_up")
        .nth(1)
        .expect("the window root clears the swipe in the capture phase")
        .split("});")
        .next()
        .expect("the capture listener");
    assert!(
        clear.contains("this.rail_swipe = None;"),
        "a release disarms the slide before any child can stop the bubble"
    );
    // The three listeners that swallow the bubble are why it has to be
    // capture. If one of them stops doing that, this test re-reads why the
    // clear sits in the capture phase rather than the bubble. They are
    // built by the rail's own view now (#2933), so the scan follows them
    // there: the root's capture still runs before any of them, because
    // capture walks down from the window whatever the child's depth.
    let rail = include_str!("../rail_view.rs");
    for action in [
        "dash.rail_chip_pick(",
        "dash.rail_expand_chips(",
        "dash.rail_collapse_chips(",
    ] {
        let listener = rail
            .split("cx.listener(")
            .find(|chunk| chunk.contains(action))
            .unwrap_or_else(|| panic!("{action} is still a listener"));
        assert!(
            listener.contains("cx.stop_propagation();"),
            "{action} is why the release has to be caught on the way down"
        );
    }
}

#[test]
fn a_row_menu_split_grows_from_the_focused_pane() {
    // #2625: the row menu named the session, so opening it as the chat
    // first and then asking again grew the split from the first pane.
    let src = DASHBOARD;
    // Rows and topic chips share one popover builder (#2701), so the
    // split arms live there instead of inside the window render.
    let popover = fn_body(src, "rail_menu_popover_for");
    let menu = popover
        .split("sessions::MenuKind::SplitSide")
        .nth(1)
        .and_then(|rest| rest.split("MenuKind::Delete").next())
        .expect("the split arms");
    assert!(
        menu.contains("this.split_row(id, Layout::Side, cx)"),
        "a side split from a row answers the pick with that row"
    );
    assert!(
        menu.contains("this.split_row(id, Layout::Stack, cx)"),
        "a stack split does the same"
    );
    assert!(
        !menu.contains("open_session(id, cx)"),
        "opening the row first moved the chat leaf and flattened the tree"
    );
    let split_row = fn_body(src, "split_row");
    assert!(
        split_row.contains("self.split_origin()"),
        "the pane being split is the one holding the caret"
    );
    assert!(
        split_row.contains("self.choose_split(id, cx)"),
        "the row already answered the pick"
    );
}

#[test]
fn a_topic_chip_answers_the_split_pick() {
    // #2760: a chip stops propagation, so the parent row's picker never
    // ran and the topic opened in the chat leaf while the split waited
    // for a session nobody was going to pick.
    // Only the render counts for the chip: the whole file holds this
    // test's own quoted names, so a plain scan reads its own strings as
    // the chip's (#2741). The helper is defined outside the render, so the
    // body scan below still needs the whole file.
    let src = DASHBOARD;
    // The chip's listener moved with the rail (#2933) and asks the window
    // by name, so the pick is pinned on the method it calls.
    let chip_pick = fn_body(src, "rail_chip_pick");
    assert!(
        !chip_pick.contains("self.open_session("),
        "the chip must not open the topic directly"
    );
    assert!(
        chip_pick.contains("self.pick_or_open(id, cx)"),
        "the chip answers the picker the way a row does"
    );
    let helper = fn_body(src, "pick_or_open");
    assert!(
        helper.contains("self.split_pick.is_some()"),
        "the helper asks whether a pane is waiting"
    );
    assert!(
        helper.contains("self.choose_split(id, cx)"),
        "and fills it instead of opening the conversation"
    );
}

#[test]
fn a_chat_topic_chip_answers_the_split_pick() {
    // #3097: the rail's chips answered the pick (#2760) but the row at the top
    // of the chat kept its own path, so a press opened the conversation in the
    // chat leaf and left the waiting pane armed on a session nobody chose.
    let src = DASHBOARD;
    let tabs = fn_body(src, "topic_tabs");
    assert!(
        tabs.contains("if this.split_pick.is_some()"),
        "the chat chip asks whether a pane is waiting"
    );
    assert!(
        tabs.contains("this.choose_split(id, cx)"),
        "and fills it instead of opening the topic"
    );
    assert!(
        tabs.contains("this.chat_topics_expanded = false"),
        "the row was expanded to pick with, so the pick folds it back"
    );
}

#[test]
fn the_split_pick_scrim_leaves_the_topic_row_bright() {
    // #3097: the scrim is `.occlude()`, which blocks the mouse for everything
    // behind its hitbox, and the row sat inside the band it covered. The row
    // was therefore dead while a pick waited, and the only way to choose a
    // topic was the sidebar, which the scrim never covered. The band is
    // measured now, so a row on any edge keeps its place instead of only the
    // top one: a side row is centred beside the conversation, so a constant
    // put the band in the wrong place for every edge but the top.
    let overlays = include_str!("../overlays.rs");
    let scrim = fn_body(overlays, "split_pick_overlay");
    assert!(
        scrim.contains("hole: Option<topic_tabs::StripRect>"),
        "the scrim is told where the row it has to leave alone actually sat"
    );
    assert!(
        scrim.contains("topic_tabs::scrim_band(hole, f32::from(rail_px))"),
        "and asks the shared band function which way to split"
    );
    assert!(
        scrim.contains(".when(vertical, |d| d.flex_row())"),
        "a side row splits the scrim left to right"
    );
    assert!(
        scrim.contains(".when(!vertical, |d| d.flex_col())"),
        "a top or bottom row splits it top to bottom"
    );
    // A band that blocked the mouse would leave the row as dead as before.
    let band = fn_body(overlays, "pick_row");
    assert!(
        !band.contains(".occlude()"),
        "the bright band must not take the pointer"
    );
    assert!(
        !band.contains(".bg("),
        "and must not dim the row it exists to leave bright"
    );
    // The render measures the row rather than deriving it.
    let render = dashboard_render(DASHBOARD);
    assert!(
        render.contains("on_children_prepainted({"),
        "the row records where it landed on each paint"
    );
    assert!(
        render.contains("let pick_hole = if split_draws_its_own"),
        "a split's own strips take a constant, the window row its measurement"
    );
    assert!(
        render.contains("split_pick_overlay(layout, rail_px, pick_hole)"),
        "and the measured band is handed to the scrim"
    );
}

#[test]
fn the_seeker_and_the_filter_answer_the_split_pick() {
    // #2760: the Seeker's OpenSession arm and the rail filter's Enter both
    // opened directly, so a pick from either left the waiting pane armed.
    let src = DASHBOARD;
    assert!(
        src.contains(
            "PaletteAction::OpenSession(id) => self.pick_or_open(Uuid::from_u128(id), cx)"
        ),
        "a Seeker row answers the picker"
    );
    assert!(
        fn_body(src, "on_key").contains("self.pick_or_open(id, cx)"),
        "the filter's Enter answers it too"
    );
}

#[test]
fn a_split_keeps_the_topics_bar_on_the_pane_that_carries_it() {
    // #2761: every topic pane drew the whole family strip, so a wall of
    // topics repeated one bar per pane and each bar claimed topics that
    // belong to a different pane.
    let src = DASHBOARD;
    // The rule reads `pane_carries_topics`, where the strip is gated, so the
    // scan takes that body instead of hoping the concat happens to spell the
    // condition the way an older revision did (#3060). The predicate moved out
    // of `pane_topics` so the split pick's scrim can ask the same question
    // (#3097): a band left bright over a row that is not drawn is a stripe
    // across the chat.
    let carries = fn_body(src, "pane_carries_topics");
    let flat: String = carries.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("let carries_topics = leaves.len() == 1 || pane_root == session;"),
        "only the pane carrying the topics draws the strip"
    );
    assert!(
        flat.contains("carries_topics && self.sessions.is_topic_enabled(pane_root)"),
        "and the strip is gated on it"
    );
    assert!(
        flat.contains("&& self.family_has_topics(pane_root)"),
        "a family with no topics left carries no strip (#2863)"
    );
    assert!(
        fn_body(src, "pane_topics").contains("if !self.pane_carries_topics(session)"),
        "the strip itself asks the shared predicate"
    );
}

#[test]
fn the_split_picker_starts_on_the_pane_being_split() {
    // #2625: the picker's cursor was the chat's row, so with the caret in
    // a right-hand pane the list opened on the wrong session.
    let src = DASHBOARD;
    let body = fn_body(src, "begin_split");
    assert!(
        body.contains("self.sessions.cursor = Some(target);"),
        "the cursor starts on the pane being split"
    );
    assert!(
        !body.contains("self.sessions.cursor = self.chat_session;"),
        "the chat's row is not the pane being split"
    );
}

#[test]
fn begin_split_targets_the_focused_leaf() {
    // #2625: the chord door resolves its target, so a split grows from the
    // leaf holding the caret rather than from whichever leaf the tree
    // happens to reach first. #2757: the resolution lives in
    // `split_origin`, which answers with the caret leaf the RENDER drew
    // instead of refusing on a stale `pane_focus`.
    let src = DASHBOARD;
    let body = fn_body(src, "begin_split");
    assert!(
        body.contains("let Some(target) = self.split_origin() else"),
        "the chord door splits the pane that holds the caret"
    );
    assert!(
        !body.contains("self.pane_focus.or(self.chat_session)"),
        "the resolution stays in `split_origin`, not a second copy here"
    );
    let origin = fn_body(src, "split_origin");
    assert!(
        origin.contains("resolve_split_focus(&leaves, self.pane_focus, chat)"),
        "a stale caret id falls back the way the render falls back"
    );
    assert!(
        origin.contains(".filter(|id| leaves.contains(id))"),
        "a leaf the tree no longer holds is not a split target"
    );
    assert!(
        origin.contains("self.panes = Some(PaneTree::leaf(chat));"),
        "a window with no tree starts from the chat's own leaf"
    );
    assert!(
        body.contains("self.split_target = Some(target);"),
        "the picker answers into the leaf the caret was on"
    );
    // The chord is the door this test pins, so it has to reach the
    // resolution above rather than a path of its own.
    let keys = fn_body(src, "on_key");
    assert!(
        keys.contains("if let Some(layout) = split_chord(ks)"),
        "the split chord is what names the layout"
    );
    assert!(
        keys.contains("self.begin_split(layout, cx)"),
        "the chord opens the pick through `begin_split`"
    );
}

#[test]
fn a_split_pane_draws_the_plans_density_shape() {
    // #2622, #2624: a pane is a fraction of the window, so its plan is
    // the density shape (`plan_strip::pane_view`), never the six-row
    // checklist that clipped mid-word there, and never a shrinkable card
    // that a short pane squeezed out of sight.
    let src = DASHBOARD;
    let body = src
        .split("fn panes_body(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("panes body");
    assert!(
        body.contains("self.plan.pane_view(density, expanded, window)"),
        "the chat leaf in a split draws the compact strip"
    );
    // A split pane builds its own strip in its own render (#2805), so the
    // density shape is read where that leaf draws.
    assert!(
        PANE_VIEW.contains("self.plan.pane_view(density, expanded, window)"),
        "every pane draws the compact strip"
    );
    assert!(
        body.contains("plan_strip::pane_density(count, pane_width)"),
        "the shape follows the pane count and the pane's own width"
    );
    assert!(
        PANE_VIEW.contains("plan_strip::pane_density(panes, self.width)"),
        "a pane reads the same shape from its own width"
    );
    assert!(
        body.contains("toggle_pane_plan"),
        "a click expands the checklist for that pane alone"
    );
    assert!(
        !body.contains(".plan.view(window)"),
        "no split pane draws the full checklist"
    );
}

#[test]
fn a_click_anywhere_in_a_pane_takes_the_keyboard() {
    // #2758: the pane took the keyboard in the BUBBLE phase, and a
    // transcript text row stops the press on its way up, so a click on
    // text never moved the caret and ctrl+| split whatever leaf the
    // window still called focused instead of the pane just clicked.
    let body = PANE_VIEW
        .split_once("let mut node = div()")
        .map(|(_, leaf)| leaf)
        .expect("the leaf builds its own box");
    assert!(
        body.contains(".capture_any_mouse_down(take_keyboard)"),
        "the pane takes the keyboard before a row can stop the press"
    );
    assert!(
        !body.contains(".on_mouse_down(MouseButton::Left, take_keyboard)"),
        "a bubble-phase listener never runs under a transcript row"
    );
}

#[test]
fn the_drop_band_only_follows_the_pane_under_the_pointer() {
    // #2759: gpui hands every `on_drag_move` listener every move while a
    // drag is live, hovered or not, so each pane answered with its own
    // bounds and the last pane painted put the band on itself.
    let src = PANE_VIEW;
    let body = src
        .split(".on_drag_move::<PaneDrag>(")
        .nth(1)
        .and_then(|rest| rest.split(".on_drop::<PaneDrag>(").next())
        .expect("the drag-move listener");
    assert!(
        body.contains("x >= bounds[0] && x <= bounds[2]"),
        "the pane checks the pointer is inside its own bounds"
    );
    assert!(
        body.contains("&& y <= bounds[3];"),
        "the check covers both axes"
    );
    assert!(
        body.contains("if inside {"),
        "only a pane the pointer is over answers with a half"
    );
    assert!(
        body.contains("drop_half(bounds, x, y)"),
        "the half still comes from the same bounds and pointer"
    );
    assert!(
        body.contains(".is_some_and(|(target, _)| target == session)"),
        "a pane clears only the band it owns"
    );
}

#[test]
fn every_pane_keeps_its_strips_at_their_own_height() {
    // #2624: the transcript takes the slack and the strips keep their
    // height. Without this the plan card was squeezed to zero in a short
    // pane, which is why it only ever vanished in a split.
    let body = PANE_VIEW
        .split_once("let mut node = div()")
        .map(|(_, leaf)| leaf)
        .expect("the leaf builds its own box");
    // #2980 made the chrome stack shrinkable and clipped, so the rows
    // inside give the room and nothing paints past the pane's bottom edge.
    // #2985 moved the floor onto the wrapper, so the body chain carries
    // `min_h(px(CONVERSATION_FLOOR))` rather than `min_h_0` (#3000).
    // #3119 spends the transcript's own 8px between the conversation and the
    // cards under it, so the gap is part of the chain the pin holds. It is
    // asserted as two runs because the comment explaining the gap sits inside
    // the chain, and a flattened region keeps its comments.
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(".flex_shrink() .min_h_0() .min_w_0()"),
        "the chrome stack is not shrinkable"
    );
    assert!(
        flat.contains(".pt_2() .overflow_hidden() .child(chrome),"),
        "the stack does not carry the gap above the cards, or does not clip what does not fit"
    );
    assert!(
        flat.contains(
            "div() .flex() .flex_col() .flex_1() .min_w_0() .min_h(px(CONVERSATION_FLOOR))"
        ),
        "the body wrapper does not carry the conversation floor"
    );
    assert!(
        flat.contains(".child(body),"),
        "the body is not the child the wrapper shrinks"
    );
}

/// The field keeps the two keys that act on it, and the line above carries the
/// work instead (#3102).
///
/// These two never merge. Esc stops the turn being written into the field and
/// the chord records a message into it, so they belong where the caret is. The
/// card has an intent of its own: what the screen is asking for right now.
#[test]
fn the_field_keeps_its_own_keys_and_the_line_carries_the_work() {
    let render = include_str!("../dashboard_render.rs");
    assert!(
        render
            .contains("fn hint_line(hint: &Hint, cx: &mut Context<Dashboard>) -> impl IntoElement"),
        "the line is its own element, drawn from the hint it carries"
    );
    // The composer's shell is the field and its footer, and nothing else: a
    // line of advice welded to the top of the box you type into reads as part
    // of the input, and it had no way to be answered.
    assert!(
        render.contains("let shell = div()"),
        "the shell holds the field and the footer only"
    );
    assert!(
        !render.contains("shell = shell.child(hint_line"),
        "and the line is no longer a strip of the composer's own box"
    );
    // The card OPENS the stack above the composer, directly under the last
    // card of the conversation (#3124). It used to close it, under the plans,
    // the queue and the offers, and a tip printed under a plan read as advice
    // about the plan: the line comments on the turn that just finished, so it
    // belongs under that turn, and everything below it is about what is
    // waiting to happen next.
    let card = render
        .find("if let Some(hint) = self.task_hint()")
        .expect("the card is drawn from the hint the screen is showing");
    let card = &render[card..];
    let card = card
        .split("above_used = true;")
        .next()
        .expect("the card is one row of the stack");
    assert!(
        card.contains("session_row(") && card.contains("hint_line(&hint, cx)"),
        "the card takes the conversation's column, like every strip above it: {card}"
    );
    // The order is the point of #3124: the hint sits above the plan card, not
    // under it. Both are found in the render, and the hint comes first.
    let hint_at = render
        .find("if let Some(hint) = self.task_hint()")
        .expect("the hint block");
    let plan_at = render
        .find("let open = self.chat_session;")
        .expect("the plan block");
    assert!(
        hint_at < plan_at,
        "the hint is drawn under the conversation and above the plan card"
    );
    // The shell owns the single edge and the single curve (#3114): the composer
    // is a card of a message's size, and it is now the only thing in that box.
    let shell = &render[render
        .find("let shell = div()")
        .expect("the composer still builds a shell")..];
    let shell = shell
        .split("col = col.child(")
        .next()
        .expect("the shell ends where it joins the column");
    assert!(
        shell.contains(".border_1()"),
        "one edge for the field and its footer: {shell}"
    );
    assert!(
        shell.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "and the messages' curve, so the card matches the bubbles: {shell}"
    );
    // The dock lost the surface it used to paint (#2987): the shell owns it, so
    // there is exactly one card on screen.
    let dock = render
        .split("let dock = div()")
        .nth(1)
        .expect("the composer dock is still built");
    let dock = dock
        .split(".child(composer)")
        .next()
        .expect("the dock still starts with the field");
    assert!(
        !dock.contains("glass_fill"),
        "the dock paints no surface of its own: the shell above owns it"
    );
    assert!(
        !dock.contains(".rounded("),
        "and no radius of its own, or the two corners would fight"
    );
    // The field's own hint still reaches every composer, panes included.
    assert!(
        include_str!("../editor.rs").contains("fn set_placeholder("),
        "the field still paints its own hint"
    );
    assert!(
        include_str!("../chat_leaf.rs").contains("fn sync_composer_hint("),
        "and the window still paints it on the field and every pane"
    );
    // The card never repeats them: a line already on screen does not need a
    // second home.
    let quiet = card_line(&HintState::default());
    assert!(
        quiet.is_none(),
        "a quiet conversation paints no card at all: {quiet:?}"
    );
}

/// The card has an intent: one line naming what the screen is asking for, and
/// nothing else (#3102).
///
/// It is not a rotation. A line that changes on its own reads as noise, because
/// the reader cannot tell a new fact from the same fact coming round again.
#[test]
fn the_card_says_what_the_screen_is_asking_for() {
    // An offer waiting is the most actionable thing on screen, and the arrows
    // reach it instead of the composer history.
    let offered = card_line(&HintState {
        offers: 1,
        ..HintState::default()
    })
    .expect("an offer on screen earns the card");
    assert!(
        offered.contains('↑') && offered.contains("enter sends"),
        "the card says how to reach the offer: {offered}"
    );
    // The card holds the keyboard while it is focused, so it has to name the
    // way back out or the reader is stuck with Esc and a guess (#3114).
    assert!(
        offered.contains("esc back to the field"),
        "and how to leave it: {offered}"
    );

    let many = card_line(&HintState {
        offers: 3,
        ..HintState::default()
    })
    .expect("three offers");
    assert!(
        many.contains('↓') && many.contains('3'),
        "a list is walked both ways and says how long it is: {many}"
    );
    assert!(
        many.contains("past the last") && many.contains("esc back to the field"),
        "the end of the list is a door, and the card says so: {many}"
    );

    // The rail holds the keyboard, so the card names its keys and nothing the
    // composer would never receive.
    let rail = card_line(&HintState {
        rail_focused: true,
        offers: 2,
        ..HintState::default()
    })
    .expect("the rail always has something to say");
    assert_eq!(
        rail, "type to filter · enter opens · esc back · the list will not read itself",
        "the list outranks the offer, because it owns the keys"
    );

    // A split pick reads the same keys for a different promise, and it ends on
    // the line the shared hint carries, not on the shorter one it replaced
    // (#3124): the card draws `task_line_hint` verbatim.
    let split = card_line(&HintState {
        rail_focused: true,
        rail_split: true,
        ..HintState::default()
    })
    .expect("a split pick");
    assert_eq!(
        split, "type to find · enter splits · esc cancels · yes, you can have two",
        "one word apart is the difference between a shortcut and a surprise"
    );

    // A queue is the field's, so the card stays empty while a turn runs: the
    // same number in two places is one sentence drawn twice (#3124).
    // Nothing is suggested while the answer is still being written: the field
    // already says Esc stops it.
    let busy = card_line(&HintState {
        processing: true,
        ..HintState::default()
    });
    assert!(
        busy.is_none(),
        "a running turn leaves the card to the field, which says Esc stops: {busy:?}"
    );
}

/// The card never repeats what the field already says, so the two keys that
/// act on the field appear in exactly one place (#3102).
#[test]
fn the_card_never_repeats_the_fields_own_keys() {
    let states = [
        HintState::default(),
        HintState {
            processing: true,
            ..HintState::default()
        },
        HintState {
            offers: 2,
            ..HintState::default()
        },
        HintState {
            rail_focused: true,
            ..HintState::default()
        },
        HintState {
            turns: 40,
            update_version: Some("0.7.0".to_string()),
            ..HintState::default()
        },
    ];
    for state in &states {
        let Some(line) = card_line(state) else {
            continue;
        };
        assert!(
            !line.contains("ctrl+alt+r"),
            "the recording chord lives in the field, not on the card: {line}"
        );
        assert!(
            !line.contains("esc stops"),
            "so does Esc stopping the turn: {line}"
        );
    }
}

/// The field's own hint still names both keys, and the microphone it offers is
/// the one the window can actually serve (#3044, #3102).
#[test]
fn the_field_hint_names_the_two_keys_that_act_on_it() {
    let busy = composer_hint_for(true, 0, true);
    assert!(
        busy.contains("esc stops") && busy.contains("typing queues"),
        "a running turn says Esc stops it and that typing is not lost: {busy}"
    );
    let ready = composer_hint_for(false, 0, true);
    assert!(
        ready.contains("ctrl+alt+r") && ready.contains("same key to send"),
        "a configured window names the chord and what it does: {ready}"
    );
    let cold = composer_hint_for(false, 0, false);
    assert!(
        cold.contains("/voice") && cold.contains("settings"),
        "an unconfigured window names the settings screen for what it is: {cold}"
    );
    assert!(
        !cold.contains("ctrl+alt+r"),
        "a chord that would only print an error is not offered: {cold}"
    );
}

/// One Esc stops the turn and hands the queue to the next one: the branch
/// cancels, reads idle at once, and the post-batch drain sends what is
/// queued as the cancellation is acknowledged (#2631).
#[test]
fn esc_cancels_the_turn_and_sends_the_queue() {
    let production = production_half();
    let branch = production
        .split("// One Esc stops the turn, and the queue becomes the next turn.")
        .nth(1)
        .and_then(|rest| rest.split("// Page keys scroll the transcript").next())
        .expect("the Esc branch for a running turn");
    assert!(branch.contains("cancel.cancel()"), "Esc cancels the turn");
    assert!(
        branch.contains("self.turn.busy = false;"),
        "the pane stops claiming a turn is running the moment Esc lands"
    );
    assert!(
        branch.contains("Sending the {waiting} queued."),
        "the stop says the queue is next"
    );
    assert!(
        !branch.contains("self.spawn_turn("),
        "no second turn starts while the cancel is still unwinding"
    );
    assert!(
        production.contains("if !self.turn.busy {\n            self.turn.cancel = None;\n            self.drain_pending(cx);"),
        "the turn's own event drains the queue once the cancel is acknowledged"
    );
}

/// With nothing queued Esc is only a stop: the wording stays the plain
/// cancellation and the drain finds nothing to send (#2631).
#[test]
fn esc_with_no_queue_only_cancels() {
    let production = production_half();
    // rustfmt wraps a long call chain across lines, so compare the code
    // with its whitespace removed: the same call, however it is laid out.
    // Both sides are flattened, because the message itself holds a space.
    let flat: String = production.chars().filter(|c| !c.is_whitespace()).collect();
    let plain = "self.turn.live.notices.push(\"Operation cancelled.\".to_string());"
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    assert!(
        flat.contains(&plain),
        "an empty queue keeps the plain wording"
    );
    let drain = production
        .split("    pub(crate) fn drain_pending(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Create a conversation").next())
        .expect("drain_pending");
    assert!(
        drain.contains("if waiting.is_empty() {\n            return;\n        }"),
        "an empty queue sends nothing"
    );
}

/// A pane's turn-end paths flush that leaf's own queue (#2695): settle,
/// fail and the Cancelled branch all hand the session to
/// `drain_pane_pending`, which takes all, joins all and spawns one turn,
/// the window's contract. Before this the leaf had no drain at all, so
/// messages 2 and 3 sat in the strip after the stop.
#[test]
fn esc_flush_pane_turn_end_drains_the_leaf_queue() {
    let production = production_half();
    let settle = production
        .split("    pub(crate) fn settle_pane(")
        .nth(1)
        .and_then(|rest| rest.split("    /// The pane's turn failed").next())
        .expect("settle_pane");
    assert!(
        settle.contains("self.drain_pane_pending(session, cx);"),
        "a settled pane flushes its queue"
    );
    let fail = production
        .split("    pub(crate) fn fail_pane(")
        .nth(1)
        .and_then(|rest| rest.split("    /// A pane's turn ended").next())
        .expect("fail_pane");
    assert!(
        fail.contains("self.drain_pane_pending(session, cx);"),
        "a failed pane flushes its queue"
    );
    let cancelled = production
        .split("agent_bridge::TurnEvent::Cancelled { session } =>")
        .nth(1)
        .and_then(|rest| rest.split("agent_bridge::TurnEvent::TurnReverted").next())
        .expect("the Cancelled branch");
    assert!(
        cancelled.contains("self.drain_pane_pending(session, cx);"),
        "a stopped pane flushes when the cancellation lands, the #2631 ordering"
    );
    let drain = production
        .split("    pub(crate) fn drain_pane_pending(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
        .expect("drain_pane_pending");
    assert!(
        drain.contains("if pane.read(cx).busy {"),
        "never a second spawn into a running or unwinding turn"
    );
    assert!(
        drain.contains("agent_bridge::queue_take(session)"),
        "the flush takes the whole queue, not one message"
    );
    assert!(
        drain.contains("agent_bridge::PendingMessage::join(&msgs)"),
        "the flush joins every message into one turn"
    );
    assert!(
        drain.contains("agent_bridge::spawn_turn("),
        "the flush sends the joined turn"
    );
}

/// #2811: the cancel arm is the only place a stop is recorded, so three
/// things have to stay true there. The card says it was stopped (otherwise
/// a spinner turns over a turn nobody is running), the stream the user saw
/// is left alone (the dropped future cannot promise the ledger has it, so a
/// reload would wipe it), and the row that makes the verdict survive a
/// reload is written.
#[test]
fn the_cancel_arm_records_the_stop_and_keeps_the_stream() {
    let production = production_half();
    let cancelled = production
        .split("agent_bridge::TurnEvent::Cancelled { session } =>")
        .nth(1)
        .and_then(|rest| rest.split("agent_bridge::TurnEvent::TurnReverted").next())
        .expect("the Cancelled branch");
    assert!(
        cancelled.contains("self.turn.live.stop();"),
        "the card must say the turn was stopped instead of spinning, \
         through the one call that also freezes its clock (#2849)"
    );
    assert!(
        cancelled.contains("agent_bridge::spawn_record_turn_stopped("),
        "and the stop must outlive the card as a row"
    );
    assert!(
        !cancelled.contains("reload = Some(session);"),
        "no reload here: the dropped future cannot promise the ledger has the text"
    );
}

/// Chrome may not refuse a turn the window minted for a named session
/// (#2795).
///
/// `open_overlays()` is window state with no session in it, so the overlay
/// gate in `send_from_pane` used to refuse every pane send while any
/// overlay was open. The plan card's Approve and Discard taps, and a
/// command's own reply turn, are exactly that shape: a click for one
/// session, refused because another part of the window held the keyboard.
/// The plan then sat in Editing with a dead button, and the composer of
/// every other session stayed dead behind the same overlay until restart.
/// The gate now reads the send's origin, and the window's own path names
/// itself.
#[test]
fn the_overlay_gate_only_refuses_a_composers_own_send() {
    let production = production_half();
    let send = production
        .split("    pub(crate) fn send_from_pane(")
        .nth(1)
        .and_then(|rest| {
            rest.split("        // A send ends that pane's offer")
                .next()
        })
        .expect("send_from_pane");
    assert!(
        send.contains("origin: SendOrigin,"),
        "the send carries who asked for it"
    );
    assert!(
        send.contains("origin == SendOrigin::Composer"),
        "the overlay gate is keyed on the composer, not on the window"
    );
    let turn = production
        .split("    pub(crate) fn send_turn_to(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Dispatch the one turn").next())
        .expect("send_turn_to");
    assert!(
        turn.contains("SendOrigin::Window"),
        "a turn the window minted is never refused by chrome"
    );
    assert!(
        production.contains("None, SendOrigin::Composer, cx)"),
        "a pane composer's own send keeps the gate"
    );
}

/// The reload a settle kicks off must not wipe the card of the turn the
/// flush just started (#2695): the live copy retires only while the leaf
/// is idle, the same guard the window's reload keeps.
#[test]
fn esc_flush_reload_keeps_the_drained_turns_card() {
    let production = production_half();
    let reload = production
        .split("    pub(crate) fn reload_pane(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Send one turn to the session").next())
        .expect("reload_pane");
    let guarded = reload
        .split("if !pane.busy {")
        .nth(1)
        .expect("the reload guards the live clear with the busy flag");
    assert!(
        guarded.trim_start().starts_with("pane.live.clear();"),
        "the clear sits inside the guard, so a drained turn keeps its card"
    );
}

/// The flush leaves a readable trail (#2695): every push and take names
/// its session and count, both drains log the joined send, and a pane
/// stop names what is still waiting. A live first-only repro is then a
/// log read, not a guess.
#[test]
fn esc_flush_receipts_log_the_queue_flow() {
    let bridge = include_str!("../agent_bridge.rs");
    let push = bridge
        .split("pub fn queue_push(")
        .nth(1)
        .and_then(|rest| rest.split("/// What is waiting").next())
        .expect("queue_push");
    assert!(
        push.contains("crate::logging::info("),
        "a push leaves a receipt"
    );
    let take = bridge
        .split("pub fn queue_take(")
        .nth(1)
        .and_then(|rest| rest.split("/// The newest message").next())
        .expect("queue_take");
    assert!(
        take.contains("crate::logging::info("),
        "a take leaves a receipt"
    );
    let production = production_half();
    let window_drain = production
        .split("    pub(crate) fn drain_pending(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Create a conversation").next())
        .expect("drain_pending");
    assert!(
        window_drain.contains("queue flush: session"),
        "the window's flush logs its joined send"
    );
    let pane_drain = production
        .split("    pub(crate) fn drain_pane_pending(")
        .nth(1)
        .and_then(|rest| rest.split("    /// Re-read one pane's rows").next())
        .expect("drain_pane_pending");
    assert!(
        pane_drain.contains("pane queue flush: session"),
        "the pane's flush logs its joined send"
    );
    let stop = production
        .split("    pub(crate) fn cancel_focused_pane_turn(")
        .nth(1)
        .and_then(|rest| rest.split("    /// `/stop`").next())
        .expect("cancel_focused_pane_turn");
    assert!(
        stop.contains("agent_bridge::queue_snapshot(id).len()"),
        "a pane stop names how much is still queued"
    );
}

/// The queue keeps to two rows until the user opens it, so a burst of
/// background reports cannot own the conversation's column (#2629).
#[test]
fn the_queue_strip_folds_to_two_rows_until_it_is_opened() {
    assert_eq!(queue_rows(0, false), 0);
    assert_eq!(queue_rows(2, false), 2);
    assert_eq!(queue_rows(5, false), 2);
    assert_eq!(queue_rows(5, true), 5);
}

#[test]
fn a_stale_pane_focus_hands_the_keyboard_back_to_the_chat() {
    // #2620: the id outlives the leaf it names. Read literally it marked
    // every pane unfocused, so no frame painted a composer and a Finder
    // drop had nothing to land in.
    let chat = Uuid::new_v4();
    let other = Uuid::new_v4();
    let leaves = [chat, other];
    assert_eq!(
        resolve_split_focus(&leaves, Some(Uuid::new_v4()), Some(chat)),
        None,
        "a stale id falls back to the chat leaf"
    );
    assert_eq!(
        resolve_split_focus(&leaves, Some(other), Some(chat)),
        Some(other),
        "a live id keeps the pane holding the caret"
    );
    assert_eq!(
        resolve_split_focus(&leaves, Some(chat), Some(chat)),
        None,
        "the chat leaf names itself None"
    );
}

#[test]
fn a_stale_pane_focus_picks_a_surviving_leaf_when_the_chat_is_gone() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let leaves = [a, b];
    assert_eq!(
        resolve_split_focus(&leaves, Some(Uuid::new_v4()), None),
        Some(a),
        "the first surviving leaf owns the keyboard"
    );
}

#[test]
fn one_leaf_is_never_a_split() {
    let only = Uuid::new_v4();
    assert_eq!(resolve_split_focus(&[only], Some(only), Some(only)), None);
    assert_eq!(resolve_split_focus(&[], Some(only), Some(only)), None);
}

#[test]
fn the_pane_footer_never_repeats_the_running_task_strip() {
    // #2620: the strip above the composer is the single source (#2617).
    // Printing the roster again in the one-line footer ran it past the
    // pane's edge and over its neighbour.
    let src = DASHBOARD;
    let footer = src
        .split("fn pane_footer_text(")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub(crate) fn ").next())
        .expect("pane footer text");
    assert!(
        !footer.contains('\u{23f3}') && !footer.contains('\u{231b}'),
        "the footer carries no task strip"
    );
}

#[test]
fn the_pane_footer_clips_instead_of_overflowing() {
    // The row moved to its own module (#2817 follow-up), so the scan
    // follows it there.
    let src = include_str!("../pane_chrome.rs");
    let footer = src
        .split("fn pane_footer(")
        .nth(1)
        .and_then(|rest| rest.split("\n/// ").next())
        .expect("pane footer");
    assert!(footer.contains(".max_w_full()"), "the row is bounded");
    assert!(footer.contains(".overflow_hidden()"), "the row clips");
    assert!(
        footer.contains(".whitespace_nowrap()"),
        "the row is one line"
    );
    assert!(footer.contains(".truncate()"), "the text ellipsizes");
}

#[test]
fn a_topic_tab_shows_two_words_and_elides_the_rest() {
    assert_eq!(topic_label("Refactor the split"), "Refactor the\u{2026}");
    assert_eq!(topic_label("Two words"), "Two words");
    assert_eq!(topic_label("OpenCrabs"), "OpenCrabs");
    // Runs of whitespace collapse instead of becoming empty words, so a
    // double space does not eat the second word's slot.
    assert_eq!(topic_label("  too   many   spaces  "), "too many\u{2026}");
    // The tab is shortened, the name is not: the rest of the window still
    // shows the whole thing (#2477).
    let stored = String::from("Refactor the split pane");
    assert_eq!(topic_label(&stored), "Refactor the\u{2026}");
    assert_eq!(stored, "Refactor the split pane");
}

#[test]
fn the_help_screen_paints_one_row_per_entry() {
    use opencrabs::ui_core::help_keys::{self, HelpSection};

    // One painted row per entry of the collection the window reads, and
    // no row the window cannot answer. A blank chord or a blank
    // description is a row that costs space and says nothing.
    let painted = help_rows();
    let shared: Vec<_> = help_keys::gui_rows().collect();
    assert_eq!(
        painted.len(),
        shared.len(),
        "the overlay must paint every shared row with a window chord"
    );
    assert!(
        painted.len() >= 8,
        "the window answers more chords than the {} listed",
        painted.len()
    );
    for (section, row) in &painted {
        let chord = row.gui.expect("every painted row has a chord");
        assert!(!chord.is_empty(), "{} has no chord", row.description);
        assert!(!row.description.is_empty(), "{chord} explains nothing");
        assert_eq!(*section, row.section, "{chord} is under the wrong heading");
    }
    // Sections print in the order the shared table declares them, and a
    // heading never repeats mid-list.
    let mut seen: Vec<HelpSection> = Vec::new();
    for (section, _) in &painted {
        if seen.last() != Some(section) {
            seen.push(*section);
        }
    }
    let declared: Vec<HelpSection> = HelpSection::ALL
        .into_iter()
        .filter(|s| seen.contains(s))
        .collect();
    assert_eq!(seen, declared, "a section printed twice or out of order");
    // The door the panel hangs off is itself one of the rows (#2320).
    assert_eq!(help_keys::find("help").and_then(|r| r.gui), Some("ctrl-h"));
}

#[test]
fn help_lists_every_rail_chord() {
    use opencrabs::ui_core::help_keys::{self, HelpSection};

    // The rail's commands are only discoverable if the table lists
    // them: each one appears exactly once, under SESSIONS, with a
    // window chord (#2355). ctrl+u is absent on purpose: the usage
    // ledger already documents it once, in GLOBAL. ctrl+s likewise:
    // GLOBAL's "List sessions" row is that command. Projects uses alt+p
    // so the rail can keep bare p for its filter (#2669, #2812). z stays
    // absent since the fold tree became chips (#2701).
    let wanted = [
        ("up/down", "Move the highlight in the list"),
        ("enter", "Open the highlighted session"),
        ("ctrl+f", "Find a session"),
        ("ctrl+shift+p", "Pin the selected session"),
        ("ctrl+d", "Delete session (asks first)"),
        ("ctrl+g", "Session files"),
        ("p", "Browse projects"),
        ("esc", "Cancel or step back"),
    ];
    let sessions: Vec<_> = help_keys::section_rows(HelpSection::Sessions).collect();
    for (chord, description) in wanted {
        let hits: Vec<_> = sessions.iter().filter(|row| row.neutral == chord).collect();
        assert_eq!(hits.len(), 1, "{chord} must be listed exactly once");
        let row = hits[0];
        assert_eq!(row.description, description, "{chord} changed its words");
        assert_eq!(
            row.gui,
            Some(match chord {
                "up/down" => "up/down",
                "ctrl+f" => "ctrl-f",
                "ctrl+shift+p" => "ctrl-shift-p",
                "ctrl+d" => "ctrl-d",
                "ctrl+s" => "ctrl-s",
                "ctrl+g" => "ctrl-g",
                "p" => "alt-p",
                _ => chord,
            }),
            "{chord} must carry the window's spelling"
        );
    }
    // r, t and c are terminal-only now (#2812): the window spends its bare
    // characters on the rail's find field, so a typed name cannot double as
    // a command. Each row stays in the table, with no window chord, so the
    // terminal keeps its keys and the window points at the row menu.
    for (chord, description) in [
        ("r", "Rename the selected session"),
        ("t", "Topics on, then a new topic"),
        ("c", "Close or reopen the selected topic"),
    ] {
        let hits: Vec<_> = sessions.iter().filter(|row| row.neutral == chord).collect();
        assert_eq!(hits.len(), 1, "{chord} must be listed exactly once");
        let row = hits[0];
        assert_eq!(row.description, description, "{chord} changed its words");
        assert_eq!(row.gui, None, "{chord} must not advertise a window chord");
    }
    // Rows with a window chord are the rail's commands, and those are what
    // must match the list above. Terminal-only rows (the topic keys in
    // /sessions) carry no window chord and are checked by their own tests.
    let rail: Vec<_> = sessions.iter().filter(|row| row.gui.is_some()).collect();
    assert_eq!(rail.len(), wanted.len(), "SESSIONS lists nothing else");
}

#[test]
fn a_typed_character_on_the_rail_opens_the_filter() {
    // A source scan, because the handler needs a window and a key event to
    // exercise. The rule here is wiring (#2812): the first printable
    // character arms the field and types itself, and the window no longer
    // spends bare letters on commands that would swallow it.
    let src = DASHBOARD;
    let block = src
        .split_once("if self.screen == Screen::Chat && self.rail_open {")
        .expect("the rail key block moved")
        .1;
    let block = block
        .split_once("if self.screen == Screen::Chat {")
        .expect("the rail key block lost its end")
        .0;
    assert!(
        block.contains("_ if self.rail_focused =>"),
        "a typed character must reach the rail's find field"
    );
    assert!(
        block.contains("self.arm_rail_filter(cx);"),
        "typing must arm the same field ctrl+f arms"
    );
    assert!(
        block.contains("self.sessions.filter.push(ch);"),
        "the character that opened the field must also land in it"
    );
    for key in ["\"r\"", "\"t\"", "\"c\""] {
        assert!(
            !block.contains(&format!("{key} if self.rail_focused")),
            "{key} must not answer bare on the window's rail"
        );
    }
}

#[test]
fn the_help_screen_opens_on_ctrl_h_and_cmd_question() {
    // A source scan, because the handler needs a window and a key event
    // to exercise. The rule here is wiring: both doors land on the same
    // toggle, Esc closes the panel before it reaches anything behind it,
    // and the panel reads the shared table instead of its own copy of the
    // chords.
    // The two doors (ctrl+h and Cmd+?) are pinned where the chords
    // live, in `screen_keys`; this pins the wiring from that table to
    // the panel, the half that lives in this file (#2356).
    let src = DASHBOARD;
    assert!(
        src.contains("ScreenAction::ToggleHelp => self.toggle_help(cx)"),
        "the chord table's help action must land on toggle_help"
    );
    assert!(src.contains("fn toggle_help"));
    assert!(
        src.contains("self.help_open = false;"),
        "Esc must close the panel"
    );
    // The table read is in `status.rs`, which sits outside the dashboard
    // concat, so the scan reads it where it is written.
    let rows = include_str!("../status.rs");
    assert!(rows.contains("help_keys::section_rows"));
    assert!(rows.contains("filter(|row| row.gui.is_some())"));
    // The panel's body moved to its own module (#2817 follow-up) and then
    // to the card view (#2893), so the row loop is read where it is
    // written.
    let panel = include_str!("../help_view.rs");
    assert!(
        panel.contains("help_rows()"),
        "the renderer must paint the list the test checks"
    );
}

/// The seeker's pick has exactly three meanings, and none of them is a
/// silent drop (#2604, #2605).
#[test]
fn topic_pick_names_the_three_cases() {
    assert_eq!(topic_pick(true, true), TopicPick::Warn);
    assert_eq!(topic_pick(true, false), TopicPick::Warn);
    assert_eq!(topic_pick(false, true), TopicPick::Draft);
    assert_eq!(topic_pick(false, false), TopicPick::EnableThenDraft);
}

/// The arm must route all three cases: a warning, a prompt, and the flag
/// write that ends in the same prompt. The needles are split so the scan
/// cannot count its own source text.
#[test]
fn seeker_topic_pick_never_drops_silently() {
    let src = DASHBOARD;
    assert!(
        src.contains(&["TopicPick::", "EnableThenDraft => {"].concat()),
        "a conversation with topics off must take the enabling path"
    );
    assert!(
        src.contains(&["self.enable_topics", "_then(id,"].concat()),
        "the enabling path must write the flag before opening the prompt"
    );
    assert!(
        src.contains(&["self.topic_warning = Some(", "title);"].concat()),
        "a topic pick must arm the warning with the row it named"
    );
    // The warning is a card of its own in `overlays.rs`, outside the
    // dashboard concat, so its header is read where it is painted.
    let warning = include_str!("../overlays.rs");
    assert!(
        warning.contains("OpenCrabs - Warning"),
        "the warning carries the header bar the report asked for"
    );
}

#[test]
fn delete_dialog_answers_enter_and_escape() {
    // The confirmation is a modal with two answers, so the keyboard gets
    // both (#2456). It sits before the Esc ladder, because a dialog that
    // owns the keyboard has to answer every key itself, and `delete` runs
    // from the same branch the button's click runs.
    let src = DASHBOARD;
    let branch = [
        "if let Some(id) = self.confirm_delete {\n            match ev.keystroke.key.as_str() {\n                \"enter\" => {",
        "self.delete_session(id, cx);",
    ];
    assert!(
        src.contains(branch[0]),
        "Enter must confirm the delete dialog"
    );
    assert!(
        src.contains(branch[1]),
        "the confirm path must delete the row it names"
    );
    let gate = src
        .find(branch[0])
        .expect("the dialog branch is in the handler");
    let esc_ladder = src
        .find("if ev.keystroke.key == \"escape\" {")
        .expect("the Esc ladder is in the handler");
    assert!(
        gate < esc_ladder,
        "the dialog answers before the ladder, not after it"
    );
}

#[test]
fn seeker_owns_the_keyboard_before_the_composer_does() {
    // The card is only a launcher if it gets the characters: its branch
    // must run before the composer gate, or a focused editor types into
    // itself what the user meant for the field (#2458).
    let src = DASHBOARD;
    // The gate's chain is wrapped by rustfmt, so the scan reads on_key
    // flat and compares positions in one string.
    let on_key = fn_body(src, "on_key");
    let flat: String = on_key.split_whitespace().collect::<Vec<_>>().join(" ");
    let hook = flat
        .find("self.finder_key(ks, cx)")
        .expect("the card must answer keys in on_key");
    let gate = flat
        .find("if self .composer .editor .read(cx) .focus_handle .is_focused(window)")
        .expect("the composer gate is in the handler");
    assert!(
        hook < gate,
        "the card reads keys before the composer, not after it"
    );
    for needle in [
        "fn finder_key",
        "fn open_finder",
        "fn run_finder_pick",
        "fn finder_items",
        "OverlayId::CommandPalette",
    ] {
        assert!(src.contains(needle), "{needle} is part of the wiring");
    }
}

/// The recording chord belongs to the card while it is up (#3044).
///
/// The editor behind the card has `ctrl-alt-r` bound to its own action, and
/// a press used to reach it: the take opened the composer's microphone and
/// landed in whichever session was focused behind the card while the card
/// drew nothing. The chord has to be answered in `finder_key`, ahead of the
/// translation that turns modified keys into edits, and it has to stop
/// there or the action fires anyway.
#[test]
fn the_recording_chord_records_into_the_card() {
    let keys = fn_body(include_str!("../dashboard_finder.rs"), "finder_key");
    let chord = keys
        .find("ks.key == \"r\"")
        .expect("the card answers the recording chord");
    assert!(
        keys.contains("cx.stop_propagation()"),
        "an unanswered chord falls through to the editor's own action"
    );
    assert!(
        keys.contains("self.seeker_dictate(cx)"),
        "the chord does what the card's microphone does"
    );
    let editing = keys
        .find("text_field::editing_chord(")
        .expect("the editing chords are still translated");
    assert!(
        chord < editing,
        "the chord is a control, not text: it reads before the translation"
    );
}

#[test]
fn the_seeker_hint_is_visible_and_clickable() {
    // #2747: the chord that opens the Seeker was invisible unless you
    // already knew it. The chat screen draws no header (#2596), so the
    // hint sits on the footer row that already carries the session name,
    // and it is a click target, not a hover tooltip: hover-only hints are
    // invisible to exactly the people who need them.
    let src = DASHBOARD;
    let start = src
        .find("id(\"seeker-hint\")")
        .expect("the chat footer carries the Seeker hint");
    let block = src
        .get(start..)
        .expect("find returns a char boundary, so the tail is sliceable");
    let end = block
        .find(".child(\"\u{b7} seek\")")
        .expect("the hint names what it opens");
    let hint = block
        .get(..end)
        .expect("find returns a char boundary, so the head is sliceable");
    assert!(
        hint.contains(".on_click(cx.listener("),
        "the hint must be a click target, not plain text"
    );
    assert!(
        hint.contains("this.open_finder(cx)"),
        "clicking the hint must open the Seeker"
    );
    assert!(
        hint.contains("seeker_chord_label()"),
        "the chord printed must come from the shared label, not a literal"
    );
    // The pill shares its row with the identity, which paints `text_faint`.
    // At `text_muted` the chord was the loudest ink in a footer of chrome, so
    // the eye landed on the key before the session it described (#3146). The
    // resting token is the row's own; the hover wash stays the door's cue.
    assert!(
        hint.contains(".text_color(rgb(palette::current().text_faint))"),
        "the pill rests on the same ink as the live state beside it"
    );
    assert!(
        !hint.contains("text_muted"),
        "a chord label must not out-shine the live state it shares the row with"
    );
}

#[test]
fn tui_chords_bind_the_window_like_the_terminal() {
    // The three commands the window spelled differently: new session,
    // sessions, fold newest turn. The terminal's chord is the one chord,
    // because the window has to answer on every platform and the command
    // key only exists on a Mac (#2319). ctrl+l stays as the alias the
    // session list has always answered.
    let src = DASHBOARD;
    for needle in [
        ["ev.keystroke.key == ", "\"n\""].concat(),
        ["modifiers.control && (ev.keystroke.key == ", "\"s\""].concat(),
        ["ev.keystroke.key == ", "\"o\""].concat(),
    ] {
        assert!(
            src.contains(&needle),
            "the window must answer {needle} as the terminal does"
        );
    }
    // And the command key answers none of the three, so no habit rides a
    // chord that only exists on one platform.
    for key in ["n", "o", "s"] {
        let banned = format!("modifiers.platform && ev.keystroke.key == \"{key}\"");
        assert!(
            !src.contains(&banned),
            "the command key must not be the chord for {key}"
        );
    }
}

#[test]
fn chat_footer_points_at_the_seeker() {
    // The footer stopped being a hand-maintained keymap (#2321). It carries
    // live state (provider, model, directory, branch, ctx budget) and one door:
    // the Seeker pill. The chords that used to sit beside it (#2877, #2894) are
    // gone from THIS row (#3143): the pill already names alt+space and is
    // clickable, ctrl+s has its own hint-line tip, and the rail has two click
    // doors of its own, so the labels spent the identity string's width on
    // advice the hint line gives for free and the reader can dismiss.
    //
    // The row reads identity first, door last (#3144): live state starts on
    // the left edge where the eye lands, and the pill closes the row on the
    // right, so the chords never push session, provider, model and path off
    // the column the reader starts from.
    //
    // The Usage and Files screens keep both labels. Neither has a Seeker pill
    // or a rail of its own to click, so the sentence is the only thing naming
    // either chord there. That is why the counts below are 2 and not 3 or 0.
    // The rest of the retired strip taught keys the window no longer binds and
    // went stale the moment a binding moved, so it stays banned.
    let src = DASHBOARD;
    // The needles are split so the scan cannot count this test's own source.
    let seeker_door = ["\"alt+space · ", "seek help\""].concat();
    assert_eq!(
        src.matches(&seeker_door).count(),
        2,
        "usage and files carry the static Seeker sentence"
    );
    let sidebar_label = ["\"ctrl+s · ", "sidebar\""].concat();
    assert_eq!(
        src.matches(&sidebar_label).count(),
        2,
        "usage and files carry the sidebar chord; the chat dock does not"
    );
    // And the chat dock's row really is the one without it, rather than this
    // count happening to match some other pair: the row opens on the identity
    // string and closes after the Seeker pill, so slice from the first to the
    // element that ends it (#3144).
    let identity_start = src
        .find("SharedString::from(identity)")
        .expect("the identity string opens the dock row");
    let dock_start = src
        .find("id(\"seeker-hint\")")
        .expect("the chat footer still carries the Seeker pill");
    assert!(
        identity_start < dock_start,
        "the live identity leads the row and the Seeker pill closes it on the right"
    );
    let dock_tail = src
        .get(identity_start..)
        .expect("a find lands on a char boundary");
    let dock_end = dock_tail
        .find(".into_any_element()")
        .expect("the row ends after the Seeker pill");
    let dock = dock_tail
        .get(..dock_end)
        .expect("a find lands on a char boundary");
    assert!(
        !dock.contains(&sidebar_label),
        "the chat dock prints no chord label beside the Seeker pill"
    );
    assert!(
        dock.contains(".on_click(cx.listener("),
        "the one door on the row stays: the Seeker pill is a click target"
    );
    assert!(
        dock.contains("palette::seeker_chord_label()"),
        "the chat door names the chord the window answers"
    );
    // And ctrl+s is taught by the line built for teaching one thing at a time,
    // where the reader can hide it for a week.
    let engine = include_str!("../../ui_core/hints.rs");
    assert!(
        engine.contains("id: \"sidebar\"") && engine.contains("ctrl+s opens the sessions rail"),
        "the sidebar chord is taught by the hint line, not the footer"
    );
    assert!(
        engine.contains("alt+space finds anything"),
        "the Seeker chord keeps its own tip"
    );
    assert!(
        !src.contains(&["back to ", "chat"].concat()),
        "no footer advertises a chord the window does not bind"
    );
    // None of the eight entries of the retired strip survives. The needles
    // are split so the scan cannot count this test's own source text.
    let retired = [
        ["cmd-u · ", "usage"],
        ["cmd-f · ", "find chat"],
        ["cmd-g · ", "files"],
        ["cmd-n · ", "new chat"],
        ["cmd-o · ", "fold turn"],
        ["cmd-b · ", "sidebar"],
        ["cmd-shift-f · ", "pin"],
        ["cmd-⌫ · ", "delete chat"],
    ];
    for parts in retired {
        let needle = parts.concat();
        let quoted = format!("\"{needle}\"");
        assert!(
            !src.contains(&quoted),
            "the footer must not advertise {needle} any more"
        );
    }
    // The identity line survives: it is live state, which the strip was not.
    // It lives in `status.rs` now (#2817 follow-up), so the scan names that
    // file. The ctx budget that used to sit beside it is gone on purpose
    // (#2628): the tool block's own footer already prints it. The streaming
    // rate is gone too (#3143): a number that changes several times a second
    // is a measurement of the turn, not of who is answering.
    let status = include_str!("../status.rs");
    assert!(status.contains("fn status_identity"));
    assert!(
        !status.contains("tok/s"),
        "the identity line carries no streaming rate"
    );
}

/// The manager is born in ONE setter, and the bridge calls it. A second
/// call site would mean two construction paths, which is how the two
/// surfaces drifted before (#2322). The needle is built in halves so this
/// test's own source does not count itself.
#[test]
fn the_bridge_creates_the_background_manager() {
    let src = include_str!("../agent_bridge.rs");
    let needle = [".with_message_enqueue", "_callback(Some("].concat();
    let sites = src.match_indices(needle.as_str()).count();
    assert_eq!(
        sites, 1,
        "exactly one manager setter, found {sites}: the GUI had none"
    );
    let head = src
        .get(..src.find(needle.as_str()).expect("the call site exists"))
        .expect("a call site is a char boundary");
    assert!(
        head.contains("fn build"),
        "the setter belongs to the bridge's build fn"
    );
}

/// A completion on a session no channel owns has to have somewhere to
/// report, which is what the local route is for (#1036). One call only.
#[test]
fn the_bridge_registers_the_local_route() {
    // One route for the whole window (#1036), counted in the shipped
    // source: a test that names the call in its own body is not a second
    // registration, and counting the whole file made the result depend on
    // how a test writes its needle (#2673).
    let src = include_str!("../agent_bridge.rs");
    let shipped = src
        .split("#[cfg(test)]")
        .next()
        .expect("the shipped source before the tests");
    let needle = ["register_local", "_route("].concat();
    let sites = shipped.match_indices(needle.as_str()).count();
    assert_eq!(sites, 1, "one local route registration, found {sites}");
}

/// Two sessions on the rail, newest first, the way `SessionRepository::list`
/// returns them (#2444).
fn rail() -> Vec<opencrabs::db::models::Session> {
    let newest = opencrabs::db::models::Session::new(Some("newest".into()), None, None);
    let older = opencrabs::db::models::Session::new(Some("older".into()), None, None);
    vec![newest, older]
}

/// #2444: a launch opens the conversation the user was in, not whatever the
/// rail happens to list first. A pointer at a session that is gone, or no
/// pointer at all, falls back to the newest instead of opening nothing.
#[test]
fn a_launch_prefers_the_last_session_and_falls_back_to_the_newest() {
    let items = rail();
    let newest = items[0].id;
    let older = items[1].id;

    assert_eq!(
        pick_initial_session(&items, Some(older)),
        Some(older),
        "the conversation in flight last time wins over the rail's order"
    );
    assert_eq!(
        pick_initial_session(&items, None),
        Some(newest),
        "a first run opens the newest"
    );
    assert_eq!(
        pick_initial_session(&items, Some(uuid::Uuid::new_v4())),
        Some(newest),
        "a pointer at a session that no longer exists falls back, never blanks the pane"
    );
    assert_eq!(
        pick_initial_session(&[], Some(older)),
        None,
        "an empty rail opens nothing rather than inventing an id"
    );
}

/// #2444: opening a conversation records it for the next launch, through
/// the one helper both surfaces share. A scratch home keeps the owner's own
/// pointer out of `cargo test`.
#[test]
fn opening_a_session_remembers_it_for_the_next_launch() {
    let home = std::env::temp_dir().join(format!(
        "opencrabs-gui-last-session-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&home).expect("scratch home");
    let id = uuid::Uuid::new_v4();
    opencrabs::config::profile::with_home_override(home, || {
        remember_open_session(id);
        assert_eq!(
            opencrabs::utils::last_session::read(),
            Some(id),
            "the pointer the next launch reads names this conversation"
        );
    });

    // And the wiring: `open_session` is the door every open goes through.
    // Since #2452 the door delegates to `begin_conversation` and the
    // record lives one layer down there, so both links of the composed
    // path are pinned: the door routes through the helper and the helper
    // does the recording. Halves, so this test cannot match itself.
    let src = DASHBOARD;
    let call = ["remember_open", "_session(id)"].concat();
    let delegate = ["begin_conversation", "(id, cx)"].concat();
    let (_, after) = src
        .split_once(
            ["fn open_", "session(&mut self, id: Uuid"]
                .concat()
                .as_str(),
        )
        .expect("open_session exists");
    let (door, _) = after
        .split_once("\n    pub(crate) fn ")
        .expect("the next method ends it");
    // #2665 split the door in two: `open_session` commits and
    // `open_session_handing_back` makes the keyboard decision. The open
    // still lands in `begin_conversation`, one link further down.
    let handing_back = ["open_session_", "handing_back(id, true, cx)"].concat();
    assert!(
        door.contains(&handing_back),
        "open_session commits through open_session_handing_back"
    );
    let (_, back) = src
        .split_once(["fn open_session_", "handing_back("].concat().as_str())
        .expect("open_session_handing_back exists");
    let (back_body, _) = back
        .split_once("\n    pub(crate) fn ")
        .expect("the next method ends it");
    assert!(
        back_body.contains(&delegate),
        "open_session_handing_back routes through begin_conversation"
    );
    let (_, helper) = src
        .split_once(["fn begin_", "conversation(&mut self"].concat().as_str())
        .expect("begin_conversation exists");
    let (recording, _) = helper
        .split_once("\n    pub(crate) fn ")
        .expect("the next method ends it");
    assert!(
        recording.contains(&call),
        "begin_conversation records the conversation it opens"
    );
}

/// The window and every split pane walk ONE history, and it is the file
/// the terminal reads, so recall shows the same past whichever composer
/// has the caret (#2494).
#[test]
fn gui_composer_history_is_one_shared_list() {
    let src = DASHBOARD;
    let shared = ["composer::new_shared_", "history("].concat();
    assert!(src.contains(&shared), "the window loads one shared list");
    let pane = ["editor::Editor::new_with_", "history(history"].concat();
    assert!(
        src.contains(&pane),
        "a split pane takes the window's handle, never a private list"
    );
    let field = ["history: composer::", "SharedHistory,"].concat();
    assert!(src.contains(&field), "the dashboard owns the handle");
}

/// Switching conversations files the draft under the one being left and
/// puts the destination's own draft back. A pane whose editor is dropped
/// files its text before it goes (#2494).
#[test]
fn gui_switching_conversations_keeps_the_draft() {
    let src = DASHBOARD;
    let (_, helper) = src
        .split_once(&["fn begin_", "conversation(&mut self, id: Uuid"].concat())
        .expect("begin_conversation exists");
    let (body, _) = helper
        .split_once("\n    pub(crate) fn ")
        .expect("the next method ends it");
    let filed = ["self.composer.drafts.", "store(previous, draft)"].concat();
    assert!(body.contains(&filed), "leaving files the draft");
    let back = ["self.composer.drafts.", "take(id)"].concat();
    assert!(body.contains(&back), "arriving takes the stored draft");
    let restore = ["ed.restore_", "state(draft, cx)"].concat();
    assert!(body.contains(&restore), "the returned draft is put back");
    let pane = ["self.stash_pane_", "draft(session, cx)"].concat();
    assert!(
        src.contains(&pane),
        "a closing pane files its draft before its editor is dropped"
    );
}

/// The completion event resumes the session that started the task, never
/// only the conversation on screen (#2704). Halves again: the file this
/// reads includes this test.
#[test]
fn the_completion_event_resumes_its_own_session() {
    let src = DASHBOARD;
    let event = ["TurnEvent::", "BackgroundTaskDone"].concat();
    assert!(
        src.contains(&event),
        "the poll loop must handle the completion event"
    );
    let arm = src.split(&event).nth(1).expect("the completion arm");
    let arm = arm
        .split("agent_bridge::TurnEvent::")
        .next()
        .expect("the end of the arm");
    assert!(
        arm.contains("self.running_here(session_id, cx)"),
        "a session already working takes the receipt in its queue"
    );
    assert!(
        arm.contains("self.resume_off_screen_task(session_id"),
        "a completion for a conversation nobody is looking at starts its turn"
    );
    assert!(
        !arm.contains("background task finished on session"),
        "the receipt is resumed, not logged away"
    );
}

/// A conversation longer than the display window still settles (#2705).
///
/// The old expectation was `turns.len() + 1`, taken from the newest 200
/// rows: past that the count stops growing, so a real turn could never be
/// seen to land and its ctx and rate were thrown away. Measured on the
/// session's row total, the same turn lands on the first reload.
#[test]
fn a_session_past_the_window_still_settles() {
    let rows_before = 335usize;
    let expect = Some(rows_before + 1);
    assert!(
        !settle_landed(expect, rows_before as i64),
        "the turn's row is not in the ledger yet"
    );
    assert!(
        settle_landed(expect, (rows_before + 1) as i64),
        "the row lands on the first reload, however long the conversation is"
    );
    assert!(
        settle_landed(None, 0),
        "a reload with nothing to wait for is never short"
    );
}

/// The /cd picker's listing (#2527): the parent entry first, then the
/// subdirectories sorted by name, files never listed.
#[test]
fn cd_entries_list_parent_dirs_only_sorted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir(root.join("zebra")).expect("zebra");
    std::fs::create_dir(root.join("alpha")).expect("alpha");
    std::fs::write(root.join("file.txt"), "x").expect("file");

    let entries = cd_entries_for(root, false);
    // The parent row is a `..`-terminated path, which Rust's file_name()
    // reports as None: name it directly instead of unwrapping.
    let names: Vec<String> = entries
        .iter()
        .map(|p| {
            if p.ends_with("..") {
                "..".to_string()
            } else {
                p.file_name().unwrap().to_string_lossy().to_string()
            }
        })
        .collect();
    // Parent first, then children alphabetical, files skipped.
    assert_eq!(names.first().map(String::as_str), Some(".."));
    assert_eq!(
        names,
        vec!["..".to_string(), "alpha".to_string(), "zebra".to_string()]
    );
}

/// Dotfile directories stay hidden until the `.` toggle turns them on,
/// the same rule the terminal's dialog uses (#2527).
#[test]
fn cd_entries_hide_dotfiles_until_toggled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir(root.join(".secret")).expect("secret");
    std::fs::create_dir(root.join("plain")).expect("plain");

    let hidden_off = cd_entries_for(root, false);
    let names_off: Vec<String> = hidden_off
        .iter()
        .filter(|p| !p.ends_with(".."))
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(
        !names_off.contains(&".secret".to_string()),
        "dotfile dirs are hidden by default"
    );
    let hidden_on = cd_entries_for(root, true);
    let names_on: Vec<String> = hidden_on
        .iter()
        .filter(|p| !p.ends_with(".."))
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(
        names_on.contains(&".secret".to_string()),
        "the toggle lists them"
    );
}

/// The window's /cd wiring (#2527), read off the source because a bare
/// `/cd` needs a live window to open the screen: bare `/cd` opens the
/// picker (never a spawned text command, so no thinking spinner), the
/// keys match the terminal's dialog, and select-here returns to chat
/// before the apply task spawns so a slow write cannot hold the screen.
#[test]
fn bare_cd_opens_the_picker_screen_and_keys_match_the_terminal() {
    let src = DASHBOARD;
    // Bare `/cd` opens the screen in run_text_command, with no turn
    // thread in the arm: that thread was the phantom spinner (#2527).
    let (_, after) = src
        .split_once(["C::Cd => {", "\n"].concat().as_str())
        .expect("run_text_command handles Cd");
    // Bound the arm at its own closing brace: everything to end-of-file
    // would sweep in later arms that legitimately spawn text commands.
    let (cd_arm, _) = after.split_once("\n            }").unwrap_or((after, ""));
    assert!(
        cd_arm.contains("self.open_cd_picker(cx)"),
        "bare /cd opens the picker screen"
    );
    let (bare, rest) = cd_arm
        .split_once("self.open_cd_picker(cx)")
        .expect("the bare arm exists");
    assert!(
        !bare.is_empty() && rest.contains("spawn_apply_operator_cd"),
        "with a path the change applies off the UI thread on the service"
    );
    assert!(
        !rest.contains("cx.spawn("),
        "the /cd apply never runs on gpui's executor: the row write needs a \
         Tokio reactor and gpui has none, so awaiting it there aborted the \
         window"
    );
    assert!(
        !cd_arm.contains("spawn_text_command"),
        "no text-command thread in the /cd arm, so no spinner"
    );
    // The screen owns the keyboard with the terminal's bindings.
    assert!(
        src.contains("\"tab\" | \"space\" => self.cd_select_here(cx),"),
        "Tab and Space select here, as in the terminal"
    );
    assert!(
        src.contains("\"enter\" => self.cd_descend(self.cd_selected.min(last), cx),"),
        "Enter descends, as in the terminal"
    );
    // Esc cancels from the screen, alongside the other screens.
    assert!(
        src.contains("if self.screen == Screen::DirectoryPicker {")
            && src.contains("self.close_cd_picker(cx);"),
        "Esc cancels the picker"
    );
    // Select-here switches to chat FIRST, the ordering #2526 just fixed
    // on the terminal: a slow apply must never hold the screen open.
    let (_, after) = src
        .split_once("fn cd_select_here")
        .expect("cd_select_here exists");
    let (body, _) = after
        .split_once("\n    pub(crate) fn ")
        .unwrap_or((after, ""));
    let switch = body
        .find("self.screen = Screen::Chat;")
        .expect("select switches to chat");
    let apply = body
        .find("agent_bridge::spawn_apply_operator_cd")
        .expect("select applies off the UI thread");
    assert!(
        switch < apply,
        "the conversation returns before the apply is handed off"
    );
    assert!(
        !body.contains("cx.spawn"),
        "the picker's apply never runs on gpui's executor: the row write \
         needs a Tokio reactor and gpui has none"
    );
    // #3036: the picker names its session when it opens and applies to
    // that same one. Both ends used to read `chat_session`, so in a split
    // the directory changed in the conversation the window held while the
    // reader was standing in a pane, and a switch made while the screen
    // was open moved the target out from under them.
    let (_, picker) = src
        .split_once("fn open_cd_picker")
        .expect("open_cd_picker exists");
    let (picker, _) = picker
        .split_once("\n    pub(crate) fn ")
        .unwrap_or((picker, ""));
    assert!(
        picker.contains("self.focused_pane()"),
        "the picker opens on the leaf the reader is in"
    );
    assert!(
        !picker.contains("self.chat_session"),
        "and never on the window's chat leaf, which is a different \
         session in a split"
    );
    assert!(
        picker.contains("self.cd_session = Some(session)"),
        "the picker pins the session it was opened for"
    );
    assert!(
        body.contains("self.cd_session.or_else(|| self.focused_pane())"),
        "select-here applies to the pinned session, not to whatever the \
         window holds when the key lands"
    );
}

/// The terminal's Ctrl+C ladder, as a decision (#2308): scrolled up the
/// press only gives the bottom back, and it never costs the window.
#[test]
fn ctrl_c_gives_the_bottom_back_before_it_takes_the_window() {
    let now = std::time::Instant::now();
    let armed = Some(now - std::time::Duration::from_secs(1));
    for pending in [None, armed] {
        assert_eq!(
            crate::ctrl_c_action(false, true, pending, now),
            crate::CtrlCAction::SnapBottom,
            "a reader scrolled up gets the bottom, never the quit"
        );
        assert_eq!(
            crate::ctrl_c_action(false, false, pending, now),
            crate::CtrlCAction::SnapBottom,
            "and a draft in the field does not change that"
        );
    }
}

/// The first press throws the draft away and arms; the second inside the
/// three second window leaves (#2308).
#[test]
fn ctrl_c_quits_on_a_second_press_inside_the_window_only() {
    let now = std::time::Instant::now();
    assert_eq!(
        crate::ctrl_c_action(true, false, None, now),
        crate::CtrlCAction::ClearAndArm,
        "nothing armed yet: the press only clears"
    );
    assert_eq!(
        crate::ctrl_c_action(
            true,
            true,
            Some(now - std::time::Duration::from_secs(1)),
            now
        ),
        crate::CtrlCAction::Quit,
        "a second press one second later quits"
    );
    assert_eq!(
        crate::ctrl_c_action(true, true, Some(now - QUIT_WINDOW), now),
        crate::CtrlCAction::ClearAndArm,
        "the window is three seconds wide and it is spent at the edge"
    );
    assert_eq!(
        crate::ctrl_c_action(
            true,
            true,
            Some(now + std::time::Duration::from_secs(1)),
            now
        ),
        crate::CtrlCAction::ClearAndArm,
        "a clock that moved backwards never quits"
    );
}

/// Text typed between the two presses is a fresh start, never a
/// confirmation: the press throws it away and arms again (#2308).
#[test]
fn ctrl_c_clears_a_draft_typed_since_the_first_press() {
    let now = std::time::Instant::now();
    assert_eq!(
        crate::ctrl_c_action(
            true,
            false,
            Some(now - std::time::Duration::from_secs(1)),
            now
        ),
        crate::CtrlCAction::ClearAndArm,
        "a draft that came back is thrown away, not read as consent"
    );
}

/// The branch sits ahead of the composer gate, so the key answers while
/// the caret is in the field (#2308).
#[test]
fn ctrl_c_branch_runs_ahead_of_the_composer_gate() {
    let src = DASHBOARD;
    // rustfmt wrapped the gate's method chain at 100 columns, so the scan
    // reads the body flat: the rule is about where the gate sits, not
    // about where the breaks landed.
    let on_key = fn_body(src, "on_key");
    let flat: String = on_key.split_whitespace().collect::<Vec<_>>().join(" ");
    let branch = flat
        .find("self.ctrl_c_press(cx);")
        .expect("the Ctrl+C branch is in on_key");
    let gate = flat
        .find("if self .composer .editor .read(cx) .focus_handle .is_focused(window)")
        .expect("the composer gate is in on_key");
    assert!(
        branch < gate,
        "Ctrl+C is judged before the editor takes the keyboard"
    );
}

/// The composer's own ctrl-c binding would take the key before any window
/// listener, so an interceptor hands it over first, and a selection still
/// reaches the copy chord (#2308, #2587).
#[test]
fn ctrl_c_reaches_the_window_ahead_of_the_copy_chord() {
    let src = DASHBOARD;
    assert!(
        src.contains("cx.intercept_keystrokes("),
        "the quit lane is judged before keymap dispatch"
    );
    assert!(
        src.contains("cx.stop_propagation();"),
        "the press is handed to the window instead of the copy action"
    );
    assert!(
        src.contains("editor.has_selection()"),
        "a composer selection keeps its copy"
    );
    assert!(
        include_str!("../editor.rs").contains("KeyBinding::new(\"ctrl-c\", Copy, None)"),
        "the editor still binds the key to copy"
    );
}

/// A pane that switches conversation keeps the turn it was running (#2854).
/// The switch used to file the draft and drop the whole view, so the live
/// turn lost its tool blocks, its thinking card and its budget, and no
/// reload could bring them back. Read off the source because a switch needs
/// a live window: each of these is one line a later edit could drop without
/// any test noticing.
#[test]
fn a_pane_switch_parks_the_view_it_leaves_behind() {
    // Every needle is split in two: `include_str!` puts this test's own
    // text inside the source it scans, so a whole literal would match
    // itself right here and pin nothing at all.
    let parks = ["self.park_p", "ane("].concat();
    let drops = ["pane_states", ".remove"].concat();

    for door in ["open_topic_in_pane", "open_picked_in_focused_pane"] {
        let body = fn_body(DASHBOARD, door);
        assert!(body.contains(&parks), "{door} parks the pane it leaves");
        assert!(
            !body.contains(&drops),
            "{door} must not drop the view it leaves"
        );
    }

    // The parking lot itself: the view moves under its session and comes
    // back into the drawn map, restored rather than rebuilt.
    let park = fn_body(DASHBOARD, "park_pane");
    assert!(
        park.contains(&["parked_panes", ".insert("].concat()),
        "a parked view is kept under its session"
    );
    let open = fn_body(DASHBOARD, "open_pane_state");
    assert!(
        open.contains(&["parked_panes", ".remove(&id)"].concat()),
        "opening a pane takes the parked view back"
    );
    assert!(
        open.contains(&["pane_states", ".insert(id, view)"].concat()),
        "the parked view is re-inserted, not rebuilt"
    );
}

/// A turn settles where it ran, not where the user is looking (#2854).
/// `settle_pane` returned early whenever the pane was off screen, so a turn
/// that finished after a switch recorded no metrics, reloaded no rows and
/// never drained its queue.
#[test]
fn a_pane_settles_while_it_is_off_screen() {
    let settle = fn_body(DASHBOARD, "settle_pane");
    assert!(
        settle.contains(&["self.pane_view(", "session)"].concat()),
        "settling resolves the pane, drawn or parked"
    );
    assert!(
        !settle.contains(&["pane_states", ".get(&session)"].concat()),
        "settling must not read only the drawn map"
    );
}

/// The leaf's live card carries its own turn (#2854): it was handed a
/// hardcoded `false` and a default budget, so a running pane drew no live
/// card and printed a zero window.
#[test]
fn a_pane_live_card_carries_its_own_turn() {
    assert!(
        PANE_VIEW.contains(&["            self.b", "usy,\n"].concat()),
        "the pane passes its own busy flag to the transcript"
    );
    assert!(
        PANE_VIEW.contains(&["ctx_used: self.ctx_u", "sed,"].concat()),
        "the pane's card reads the pane's own ctx"
    );
    assert!(
        PANE_VIEW.contains(&["ctx_max: self.ctx_m", "ax,"].concat()),
        "the pane's card reads the pane's own window"
    );
}

/// The pencil opens a field in the row itself. Neither send path can fall
/// back to replacing the composer's draft or consuming a composer target.
#[test]
fn an_edit_is_confirmed_inside_its_own_card() {
    let messages = include_str!("../dashboard_messages.rs");
    let begin = messages
        .split("fn msg_edit_begin(")
        .nth(1)
        .and_then(|rest| rest.split("/// Send the rewrite open").next())
        .expect("window edit begin");
    assert!(begin.contains("self.new_inline_editor(text, cx)"));
    assert!(begin.contains("self.edit_row = Some(chat_leaf::InlineEdit"));
    assert!(!begin.contains("self.composer.editor"));
    assert!(!begin.contains("show_editing"));
    let confirm = messages
        .split("fn msg_edit_confirm(")
        .nth(1)
        .and_then(|rest| rest.split("/// Close the rewrite").next())
        .expect("window edit confirm");
    assert!(confirm.contains("self.edit_row.take()"));
    assert!(confirm.contains("self.send_edited(open.target, text, cx)"));
    let transcript = include_str!("../transcript.rs");
    assert!(transcript.contains("Some(editing) => inline_edit_box(editing)"));
    assert!(transcript.contains("msg-edit-confirm-{id}"));
    assert!(transcript.contains("msg-edit-cancel-{id}"));
    let sends = include_str!("../dashboard_turns.rs");
    assert!(!sends.contains("self.composer.edit.take()"));
    assert!(!sends.contains("pane.edit.take()"));
    assert!(PANE_VIEW.contains("this.pane_edit_confirm(session, text, cx)"));
}

/// Copy, React, Reply and Forward are resolved from the row that was
/// pressed, on either role and in either kind of leaf. Reply keeps the
/// draft intact and Forward keeps the destination picker in that leaf.
#[test]
fn every_row_menu_uses_the_pressed_row_and_leaf() {
    let messages = include_str!("../dashboard_messages.rs");
    let menu = messages
        .split("fn msg_menu_items(")
        .nth(1)
        .and_then(|rest| rest.split("/// Toggle one row").next())
        .expect("message menu");
    for action in ["CopyMarkdown", "React", "Reply", "Forward", "Select"] {
        assert!(
            menu.contains(&format!("MessageAction::{action}.label()")),
            "{action} is not offered"
        );
    }
    assert!(menu.contains("self.msg_menu_row.clone()"));
    assert!(!menu.contains("is_answer"));
    assert!(!menu.contains("row.role"));
    assert!(PANE_VIEW.contains("msg_menu_open(row.clone(), Some(session)"));
    let reply = messages
        .split("fn reply_rows(")
        .nth(1)
        .and_then(|rest| rest.split("/// The composer a reply chip").next())
        .expect("reply attachment");
    assert!(reply.contains("editor.attach_reply(author, preview, quote, cx)"));
    assert!(!reply.contains("restore_draft"));
    let forward = messages
        .split("fn forward_open(")
        .nth(1)
        .and_then(|rest| rest.split("/// Back out of the picker").next())
        .expect("forward open");
    assert!(forward.contains("self.forward_leaf = leaf"));
    assert!(PANE_VIEW.contains("win.forward_picker(cx)"));
}

/// History menus close on an outside press even when text selection stops
/// the bubble event, and only the current menu's five-second timer fires.
#[test]
fn message_menu_dismisses_outside_and_expires_only_current_menu() {
    assert_eq!(MSG_MENU_TTL, std::time::Duration::from_secs(5));
    assert!(msg_menu_expiry_due(true, 8, 8));
    assert!(!msg_menu_expiry_due(false, 8, 8));
    assert!(!msg_menu_expiry_due(true, 9, 8));

    let messages = include_str!("../dashboard_messages.rs");
    assert!(messages.contains("cx.background_executor().timer(MSG_MENU_TTL)"));
    assert_eq!(
        messages.matches("self.msg_menu_arm_timeout(cx);").count(),
        2,
        "the row menu and its reaction picker both expire"
    );
    let menu = include_str!("../menu.rs");
    assert!(
        menu.contains("menu_card(id, items).on_mouse_down_out(dismiss)"),
        "outside presses must dismiss during capture, before text stops them"
    );
    let transcript = include_str!("../transcript.rs");
    assert!(
        transcript
            .contains("dismissible_menu_popover(\"message-context-menu\", rows, at, dismiss)")
    );
    assert!(
        transcript
            .contains("dismissible_menu_popover(\"react-picker\", vec![grid_row], at, dismiss)")
    );
    assert!(DASHBOARD.contains("this.msg_menu_close(cx)"));
}

/// A side strip rides the conversation's measure (#2948). It used to be a
/// root sibling of the pane column, so it sat against the window edge while
/// the text centred at the measure and the whole centring slack opened
/// between the two. The strip and the column centre as one block now.
#[test]
fn a_side_strip_centres_with_the_pane_it_belongs_to() {
    let render = dashboard_render(DASHBOARD);
    let side = render
        .split("match topic_side {")
        .nth(1)
        .expect("the side strip arm")
        .split("None => root = root.child(right),")
        .next()
        .expect("the arm before the no-strip case");
    assert!(
        side.contains(".justify_center()"),
        "the strip and the column centre as one block, not as two siblings"
    );
    assert!(
        !side.contains("root = root.child(strip)"),
        "the strip is never a root sibling of the pane again"
    );
    // The column pads itself with `px_4` a side, so its own width is the
    // measure plus 2rem. Capping it there puts its children back on the
    // axis they always centred on.
    assert!(
        side.contains("chat_measure_px + 2.0 * self.rem"),
        "the cap is the measure plus the column's own padding"
    );
    assert!(
        side.contains(".when(centred_chat, |d| d.max_w(px(pane_px)))"),
        "the cap only applies while the chat is centred, so a narrow pane keeps its width"
    );
    assert!(
        side.contains("row.child(strip).child(pane)")
            && side.contains("row.child(pane).child(strip)"),
        "both edges put the strip beside the pane, never above or below it"
    );
}

/// The footer names the open conversation's own place (#3036).
///
/// It printed `launch_cwd()`, the directory the process started in, which
/// nothing ever moves: a `/cd` changed where the agent worked and left the
/// row naming the old project. The branch was read from that same directory
/// once at startup, so it was just as stale.
#[test]
fn the_footer_names_the_open_conversations_directory() {
    let place = DASHBOARD
        .split("fn footer_place(")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub(crate) fn ").next())
        .expect("the footer's place is resolved from the session");
    assert!(
        place.contains("get_working_directory_for_session"),
        "the directory comes from the conversation, not the process"
    );
    assert!(
        place.contains("git_branch(&dir)"),
        "the branch is read from that same directory"
    );
    assert!(
        place.contains("place.dir != dir"),
        "and only re-read when the directory changes, because git is a subprocess"
    );
    let reads = ["let (cwd, branch) = self.", "footer_place();"].concat();
    assert!(
        DASHBOARD.contains(&reads),
        "the identity line prints that place"
    );
    let launch = ["tilde_path(&opencrabs::utils::cwd::", "launch_cwd())"].concat();
    assert!(
        !DASHBOARD.contains(&launch),
        "the launch directory can never name a `/cd`"
    );
}

/// A receipt is parked with the conversation that asked for it, never
/// painted on another chat (#3036).
///
/// A `/cd` from the picker answers through this door. A switch to another
/// session parks the live turn, so the fallback printed the receipt on the
/// conversation the reader had moved to while the one that asked saw
/// nothing. The line now waits in the parked turn, and the visible one is
/// scrolled into view so a reader parked in history still sees it.
#[test]
fn a_notice_is_parked_with_the_conversation_that_asked() {
    let notice = DASHBOARD
        .split("fn push_session_notice(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// ").next())
        .expect("the notice door");
    let parked = [".entry(", "session)"].concat();
    assert!(
        notice.contains(&parked),
        "the receipt is parked with its own turn"
    );
    assert!(
        notice.contains("rearm_transcript_follow()"),
        "and a visible one is scrolled into view"
    );
    assert!(
        notice.contains("rearm_pane_follow(session, cx)"),
        "a pane's own transcript is scrolled into view too"
    );
}

/// The composer is a card, and the ground under it is square (#3114).
///
/// #3112 took the composer edge to edge and flat, and took the session surface
/// with it: the ground is the whole shot now, so it carries no corner. The owner
/// then sent the composer back to a card the size of a message, which is where
/// the two shapes part company: the card has a curve because it floats on the
/// ground, and the ground has none because it IS the window.
#[test]
fn the_composer_is_a_card_on_a_square_ground() {
    let render = include_str!("../dashboard_render.rs");
    let shell = &render[render
        .find("let shell = div()")
        .expect("the composer still builds a shell")..];
    let shell = shell
        .split("col = col.child(")
        .next()
        .expect("the shell ends where it joins the column");
    assert!(
        shell.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "the card wears the messages' curve, not a flat edge: {shell}"
    );
    let surface = &render[render
        .find("let mut session = div()")
        .expect("the chat builds a session surface")..];
    let surface = surface
        .split(".overflow_hidden();")
        .next()
        .expect("the surface clips");
    assert!(
        !surface.contains(".rounded("),
        "the ground is the shot, so it carries no corner either: {surface}"
    );
}

/// The hint line breaks instead of cutting its sentence (#3162).
///
/// The line used to carry `truncate()`, so a narrow pane put an ellipsis where
/// the tail of the hint used to be. The tail is where the second command and
/// the joke live, so the whole sentence has to survive: the text wraps onto a
/// second or third row, the label stays on the first row of it, and the row
/// takes the width it has left to break against.
#[test]
fn the_hint_line_wraps_instead_of_truncating() {
    let render = include_str!("../dashboard_render.rs");
    let line = fn_body(render, "hint_line");
    assert!(
        !line.contains(".truncate()"),
        "the hint sentence is never cut: {line}"
    );
    assert!(
        line.contains(".whitespace_normal()"),
        "the sentence wraps instead of being cut: {line}"
    );
    assert!(
        line.contains(".flex_1()"),
        "the sentence has a width to break against: {line}"
    );
    assert!(
        line.contains(".items_start()"),
        "the label stays on the first row of a wrapped sentence: {line}"
    );
    // The dismiss control is still there: a line that cannot be hidden is a
    // line nobody can get rid of.
    assert!(
        line.contains("hint-dismiss"),
        "the line can still be dismissed: {line}"
    );
}
