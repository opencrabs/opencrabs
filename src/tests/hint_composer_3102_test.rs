//! The card above the composer: where it sits, what it says, and why it does
//! not move on its own (#3102).
//!
//! The row started as the field's placeholder, which made it a promise that
//! vanished on the first keystroke. It moved out of the field, then it started
//! rotating through everything true on a timer, and both were wrong for the
//! same reason: it was answering a question nobody had. It is a thin card on
//! top of the composer now, it has one intent per moment, and the two keys that
//! act on the field stay in the field where the caret is.
//!
//! These tests pin the shared answer in [`crate::ui_core::hints`], which both
//! surfaces draw. The desktop app filters it through its own command table and
//! the terminal through the dispatcher, so a line either surface would bounce
//! is dropped before it is ever painted.

use crate::ui_core::hints::{HintState, task_line};

/// A conversation with nothing to say: no turn, no queue, no rail, no offer.
fn quiet() -> HintState {
    HintState::default()
}

/// The terminal's filter: it dispatches the whole shared table, so any name the
/// table knows is a name it answers.
fn tui(command: &str) -> bool {
    crate::ui_core::commands::lookup(command).is_some()
}

/// The desktop's filter, the narrower of the two: a command the window does not
/// answer must never be printed there.
fn gui(command: &str) -> bool {
    crate::ui_core::commands::support_gui(command) == crate::ui_core::commands::Support::Runs
}

/// The line for a state, through the terminal's filter.
fn line(state: &HintState) -> Option<String> {
    task_line(state, tui)
}

/// A quiet conversation paints no card at all.
///
/// The card is not reserved. It appears when the work has something to ask and
/// disappears when it does not, which is what keeps it from becoming chrome
/// that is always there and therefore never read.
#[test]
fn a_quiet_conversation_paints_no_card() {
    assert_eq!(line(&quiet()), None, "nothing to ask, nothing to draw");
}

/// A running turn leaves the card to the field.
///
/// Esc stops the turn and typing queues a message, and both are already written
/// inside the field. Repeating them on a card directly above the field would be
/// the same sentence twice in one glance.
#[test]
fn a_running_turn_leaves_the_card_to_the_field() {
    let busy = HintState {
        processing: true,
        ..quiet()
    };
    assert_eq!(
        line(&busy),
        None,
        "the field already says Esc stops and typing queues"
    );
}

/// An offer waiting is the card's intent, because the arrows reach it instead
/// of the composer history (#3101).
#[test]
fn a_waiting_offer_names_the_arrows() {
    let single = line(&HintState {
        offers: 1,
        ..quiet()
    })
    .expect("one offer earns the card");
    assert!(
        single.contains('↑') && single.contains("enter sends"),
        "the card says how to reach the offer: {single}"
    );
    // Down past the only row is the way out, so the line names it (#3114).
    // The card holds the keyboard while it is focused, and nothing used to say
    // how to leave it.
    assert!(
        single.contains('↓') && single.contains("back to the field"),
        "one offer still has a way out, and the line names it: {single}"
    );

    let many = line(&HintState {
        offers: 3,
        ..quiet()
    })
    .expect("three offers");
    assert!(
        many.contains('↓') && many.contains('3'),
        "a list is walked both ways and says how long it is: {many}"
    );
    assert!(
        many.contains("past the last") && many.contains("esc"),
        "a list that ends has to say what the end does: {many}"
    );
}

/// The rail outranks everything, because while it holds the keyboard the
/// composer receives nothing at all.
#[test]
fn the_rail_names_its_own_keys_and_silences_the_rest() {
    let filtering = line(&HintState {
        rail_focused: true,
        offers: 2,
        turns: 40,
        update_version: Some("0.7.0".to_string()),
        ..quiet()
    })
    .expect("the rail always has something to say");
    assert_eq!(
        filtering, "type to filter · enter opens · esc back",
        "the list owns the keys, so its keys are the card"
    );

    // A split pick reads the same keys for a different promise: Enter splits
    // the layout rather than opening the conversation.
    let splitting = line(&HintState {
        rail_focused: true,
        rail_split: true,
        ..quiet()
    })
    .expect("a split pick");
    assert_eq!(
        splitting, "type to find · enter splits · esc cancels",
        "one word apart is the difference between a shortcut and a surprise"
    );
}

/// A split pick silences the composer even before the list takes focus: the
/// typed character is already spoken for.
#[test]
fn a_split_pick_speaks_before_focus_lands() {
    let splitting = line(&HintState {
        rail_split: true,
        offers: 2,
        ..quiet()
    })
    .expect("the pick is already asking for a name");
    assert!(
        splitting.contains("enter splits"),
        "the split promise is what the card says: {splitting}"
    );
}

/// A queue is the one rung that is also true while a turn runs.
#[test]
fn a_queue_says_where_the_message_goes() {
    let draining = line(&HintState {
        queued: 2,
        ..quiet()
    })
    .expect("a queue on its way out");
    assert!(
        draining.contains("2 queued") && draining.contains("sending now"),
        "an idle field with a queue says the queue is on its way: {draining}"
    );

    let waiting = line(&HintState {
        processing: true,
        queued: 2,
        ..quiet()
    })
    .expect("a queue on a running turn");
    assert!(
        waiting.contains("2 queued") && waiting.contains("turn ends"),
        "and while a turn runs it says when the queue goes out: {waiting}"
    );
}

/// The ladder only joins when the field can actually take a command.
///
/// While a turn runs a slash command would be queued as text and read by the
/// agent rather than dispatched, and while the rail holds the keys the composer
/// receives nothing at all: naming a command in either state teaches a habit
/// that bounces.
#[test]
fn the_ladder_waits_for_a_field_that_can_take_a_command() {
    let long_session = HintState {
        turns: 40,
        update_version: Some("0.7.0".to_string()),
        ..quiet()
    };
    let rung = line(&long_session).expect("a release waiting is news");
    assert!(
        rung.contains("/evolve"),
        "the ladder reaches the card when the field can take it: {rung}"
    );

    // The window's filter is narrower than the terminal's, and the same
    // conversation has to read the same way there.
    let gui_line = task_line(&long_session, gui).expect("the window answers /evolve");
    assert!(
        gui_line.contains("/evolve"),
        "the window says the same thing about the same conversation: {gui_line}"
    );
}

/// The card never repeats what the field already says (#3102).
///
/// The recording chord and Esc stopping the turn live in the field. A card that
/// repeated them would be the same sentence twice, and the reader would stop
/// reading both.
#[test]
fn the_card_never_repeats_the_fields_own_keys() {
    let states = [
        quiet(),
        HintState {
            processing: true,
            ..quiet()
        },
        HintState {
            processing: true,
            queued: 1,
            ..quiet()
        },
        HintState {
            offers: 2,
            ..quiet()
        },
        HintState {
            rail_focused: true,
            ..quiet()
        },
        HintState {
            rail_split: true,
            ..quiet()
        },
        HintState {
            turns: 40,
            update_version: Some("0.7.0".to_string()),
            ..quiet()
        },
    ];
    for state in &states {
        let Some(text) = line(state) else {
            continue;
        };
        assert!(
            !text.contains("ctrl+alt+r"),
            "the recording chord belongs to the field, not the card: {text}"
        );
        assert!(
            !text.contains("esc stops"),
            "so does Esc stopping the turn: {text}"
        );
    }
}

/// The card does not move on its own.
///
/// It used to cycle through every true fact on a timer, and a line that changes
/// by itself reads as noise: the reader cannot tell a new fact from the same
/// fact coming round again. A rotation needs a clock, so the absence of one is
/// the cheapest honest pin.
#[test]
fn the_card_does_not_rotate() {
    let hints = include_str!("../ui_core/hints.rs");
    assert!(
        !hints.contains("HintClock"),
        "no clock, no rotation: the card changes when the work changes"
    );
    assert!(
        !hints.contains("HINT_HOLD_SECS"),
        "and no hold timer to advance it"
    );
    // Nothing keeps a rotation alive on either surface either.
    assert!(
        !include_str!("../gui/dashboard_turns.rs").contains("hint_clock"),
        "the window does not tick a rotation"
    );
    assert!(
        !include_str!("../tui/render/tips.rs").contains("hint_clock"),
        "and neither does the terminal"
    );
}

/// The card rides on top of the composer on both surfaces.
///
/// In the window it is a thin strip flush against the composer, separated by a
/// single hairline, so the pair reads as one surface. In the terminal it takes
/// the row directly above the input box.
#[test]
fn the_card_sits_on_top_of_the_composer() {
    let render = include_str!("../gui/dashboard_render.rs");
    let stack = render
        .find("let mut shell = div()")
        .expect("the card and the composer share one shell");
    let dock = render
        .find("dock.child(central(footer, centred_chat, chat_measure_px))")
        .expect("the composer card is what it rides on");
    assert!(
        stack < dock,
        "the card is added to the stack before the composer"
    );
    // One surface, and the shell clips it. The pair used to share one radius
    // (#3102); the composer is a flat footer now (#3112), so the shell owns a
    // hairline on top instead of a curve and the clip stays, because the card
    // must not paint past the surface it rides on.
    let shell_body = render
        .get(stack..)
        .expect("the shell starts on a char boundary")
        // The shell ends where the card joins the column. The marker used to
        // name `central`, which the session surface replaced (#3110), and a
        // stale marker made this slice run to the end of the file: the
        // assertion below would then have been reading every other card.
        .split("col = col.child(")
        .next()
        .expect("the shell ends where it joins the column");
    assert!(
        shell_body.contains(".border_t(px(1.))"),
        "the shell owns the one seam the pair shares"
    );
    assert!(
        !shell_body.contains(".rounded("),
        "the composer is flat, so the shell carries no curve: {shell_body}"
    );
    assert!(
        shell_body.contains(".overflow_hidden()"),
        "and clips, so nothing paints past the surface"
    );

    // The terminal lays the card out before the input, in that order.
    let frame = include_str!("../tui/render/frame.rs");
    let card = frame
        .find("Constraint::Length(hint_height),   // [3] Hint row")
        .expect("the card has a row of its own");
    let input = frame
        .find("Constraint::Length(input_height),  // [4] Input")
        .expect("the input follows it");
    assert!(
        card < input,
        "the card is drawn above the field it describes"
    );
}

/// The card is painted in the theme's accent on both surfaces, never a literal
/// and never the muted chrome the footer uses.
///
/// Its whole job is to be noticed by someone who does not yet know the keys.
/// The quiet ink that makes the key hints recede is exactly the wrong colour for
/// that, and a hardcoded colour would ignore the theme the reader picked.
#[test]
fn the_card_is_painted_in_the_theme_accent() {
    let tips = include_str!("../tui/render/tips.rs");
    assert!(
        tips.contains("theme::role(Role::Accent)"),
        "the terminal takes the accent from the theme, not from a literal"
    );

    let render = include_str!("../gui/dashboard_render.rs");
    let start = render
        .find("fn hint_card(line: &SharedString) -> Div {")
        .expect("the window's card");
    let body = render
        .get(start..)
        .expect("the card starts on a char boundary")
        .split("\n}")
        .next()
        .expect("the card ends");
    assert!(
        body.contains("shown.accent"),
        "the window's card is painted in the palette's accent"
    );
    assert!(
        !body.contains("0x"),
        "and never in a literal colour, which would ignore the theme"
    );
}
