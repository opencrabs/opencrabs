//! The follow-up offer card's shape and its keyboard (#3114).
//!
//! Three things were wrong on one surface, and all three were invisible to a
//! test that only asked what the hint said: the rows drew side by side, the
//! last row was a wall the arrows could not pass, and nothing on screen named
//! a way out of a card that holds the keyboard.
//!
//! There is no headless window to press a key in, so the behaviour is pinned at
//! the source. That is the same shape the rest of the desktop app's layout
//! tests use (`gui_voice_note_row_test`), and it is honest about what it
//! proves: the arm exists and does what it says, not that gpui dispatched it.

use crate::ui_core::hints::{HintState, task_line};

const COMPOSER_VIEW: &str = include_str!("../gui/composer_view.rs");
const DASHBOARD_SESSIONS: &str = include_str!("../gui/dashboard_sessions.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");
const PANE_CHROME: &str = include_str!("../gui/pane_chrome.rs");
const DASHBOARD_RENDER: &str = include_str!("../gui/dashboard_render.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const PLAN_STRIP: &str = include_str!("../gui/plan_strip.rs");

/// The slice of a file between two anchors, so a pin cannot match a string
/// somewhere else in the same file by accident.
fn region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let from = source
        .find(start)
        .unwrap_or_else(|| panic!("missing start anchor {start:?}"));
    let rest = &source[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("missing end anchor {end:?}"));
    &rest[..to]
}

/// The offer rows stack, one per line (#3114).
///
/// They used to wrap side by side, so two labels split the card's width and a
/// long one was cut while a row of its own had the room. The column is also
/// what the arrows describe: a list, not a paragraph.
#[test]
fn the_offer_rows_stack_one_above_the_other() {
    let strip = region(
        COMPOSER_VIEW,
        "pub(crate) fn options_strip(",
        "/// Whether a speech-to-text engine is configured",
    );
    assert!(
        strip.contains("flex_col().items_start().gap_1()"),
        "the buttons are a column that hugs its labels, so one offer owns a line"
    );
    assert!(
        !strip.contains("flex_wrap"),
        "nothing wraps sideways any more"
    );
    assert!(
        strip.contains("flex_row()"),
        "the row keeps its icon-and-label layout inside the column"
    );
}

/// The offers are buttons, not a card (#3124).
///
/// The strip was a card of its own: a 12-radius panel in the message fill,
/// padded 12, holding a column of rows. Two complaints arrived as one. The card
/// was as wide as the column while its rows were short, so most of it was empty
/// surface; and at 12 points its words were smaller than the answer directly
/// above them, which made the one control that wants a click read like a
/// footnote. The buttons now stand bare at the answer's own body size.
#[test]
fn the_offers_are_buttons_rather_than_a_card() {
    let strip = region(
        COMPOSER_VIEW,
        "pub(crate) fn options_strip(",
        "/// Whether a speech-to-text engine is configured",
    );
    assert!(
        !strip.contains("glass_fill"),
        "no fill behind the buttons: a card around them is surface the rows \
         never fill: {strip}"
    );
    assert!(
        !strip.contains("shadow_"),
        "and no shadow lifting a card that is not there"
    );
    assert!(
        strip.contains(".text_size(px(transcript::PLAIN_TEXT_SIZE))"),
        "an offer reads at the answer's body size: {strip}"
    );
    assert!(
        !strip.contains("text_xs"),
        "the small print size is what made the offers look like a footnote"
    );
}

/// Down past the last row hands the keyboard back to the field (#3114).
///
/// Up from the composer reaches the card (#3101). Without the mirror, the last
/// row was a wall: only Esc, or a key that meant "back to typing", got the
/// caret back, and the screen never named either.
#[test]
fn down_past_the_last_row_returns_the_caret_to_the_field() {
    let keys = region(
        DASHBOARD_SESSIONS,
        "pub(crate) fn options_key(",
        "pub(crate) fn drain_pending(",
    );
    assert!(
        keys.contains("\"down\" if at + 1 >= offers.len() => {"),
        "the last row is a door, not a wall: {keys}"
    );
    let exit = region(
        keys,
        "\"down\" if at + 1 >= offers.len() => {",
        "\"down\" => self.options_focus = Some((session, at + 1)),",
    );
    assert!(
        exit.contains("self.options_focus = None;"),
        "the ring leaves the card"
    );
    assert!(
        exit.contains("self.request_composer_caret(cx);"),
        "and the caret comes back to the field"
    );
    // The clamp that made the last row a wall is gone: the plain arm advances
    // and nothing else.
    assert!(
        !keys.contains(".min(offers.len() - 1)"),
        "no clamp is left behind to swallow the key"
    );
}

/// Both offer lines name the way out (#3114).
///
/// The card holds the keyboard while it is focused, so a reader who followed
/// the line into it needs the exit in the same breath.
#[test]
fn the_offer_line_names_the_way_back_to_the_field() {
    let single = task_line(
        &HintState {
            offers: 1,
            ..HintState::default()
        },
        |_| true,
    )
    .expect("one offer earns the card");
    assert!(
        single.contains("back to the field"),
        "one offer is also the exit: {single}"
    );

    let many = task_line(
        &HintState {
            offers: 3,
            ..HintState::default()
        },
        |_| true,
    )
    .expect("three offers");
    assert!(
        many.contains("past the last") && many.contains("back to the field"),
        "the end of a list says what the end does: {many}"
    );
    assert!(many.contains('3'), "and how long the list is: {many}");
}

/// The composer's own inset matches the column the conversation sits on
/// (#3114).
///
/// A message bubble's text lands 24 from the session edge: its row pads 8 and
/// the bubble pads 16. The composer is a card of a message's size again, placed
/// by `session_row`, which already spent half a rem of that 24 on each side, so
/// the rows inside it pad the rest (`card_inset`) and their words land on the
/// column. A focused split pane draws the field bare, with no card around it, so
/// there the field is the whole column and pads all of `conversation_inset`. A
/// uniform `p_3` on the field left its text, its paperclip and its mic at 12,
/// and the task strip leaned on a gutter that no longer exists and sat at 16, so
/// three rows of one surface started on three different columns.
#[test]
fn the_field_pads_the_same_sides_as_the_card_around_it() {
    let field = region(EDITOR, ".when(!self.docked, |d| {", ".line_height(px(22.))");
    assert!(
        field.contains(".px(if self.docked {") && field.contains("transcript::card_inset()"),
        "the docked field pads the card's own column"
    );
    assert!(
        field.contains("transcript::conversation_inset()"),
        "and a bare pane's field pads the whole column, since no card is around it"
    );
    assert!(
        field.contains(".py_3()"),
        "the vertical step stays where it was"
    );
    assert!(
        !field.contains(".p_3()"),
        "one uniform inset is what put the text out of line"
    );
    // The dim row in a split keeps the same shape, or a draft jumps sideways
    // the moment the keyboard leaves the pane.
    assert!(
        PANE_CHROME.contains(".px(transcript::conversation_inset())\n        .py_3()"),
        "the unfocused pane's composer matches the focused one"
    );
    assert!(
        !PANE_CHROME.contains(".p_3()"),
        "and keeps no uniform inset of its own"
    );
    // The hint is a line of the conversation's column, not a card (#3139). It
    // still pads the column every row of the chat starts on, and it paints no
    // surface of its own: the theme's accent already carries the words, so a
    // fill and an edge only added a third panel to a window that has two.
    let line = region(DASHBOARD_RENDER, "fn hint_line(", "\n}\n");
    assert!(
        line.contains(".px(transcript::card_inset())"),
        "the line's words land on the conversation's column"
    );
    assert!(
        !line.contains(".p_3()"),
        "a uniform inset is what put its words four pixels left of everything else"
    );
    assert!(
        !line.contains(".rounded(px(transcript::MESSAGE_RADIUS))")
            && !line.contains(".bg(glass_fill("),
        "a tip is text, so it draws no card of its own: {line}"
    );
    assert!(
        !line.contains(".border_1()"),
        "and no edge, which is what made one sentence read as a panel"
    );
    assert!(
        !line.contains(".px(transcript::card_row_inset())"),
        "it is no longer a row of the composer's card"
    );
    // The footer row under the hairline is the last of those rows, and it
    // padded a flat 12 while everything above it moved (#3114).
    let footer = region(
        DASHBOARD_RENDER,
        ".justify_between()",
        ".into_any_element()",
    );
    assert!(
        footer.contains(".px(transcript::card_row_inset())"),
        "the footer row pads the card's column minus the pill"
    );
    assert_eq!(
        footer
            .matches(".px(transcript::hover_pill_inset())")
            .count(),
        2,
        "and both of its labels pad the pill, so every glyph starts on the column"
    );
}

/// The space under the last card is the space between two cards (#3114).
///
/// Two boxes were each minding their own rhythm: the session stepped 12 between
/// its rows, and the transcript added its own 8 of bottom padding on top of
/// that, so the conversation closed on a band of 20 while the cards above it
/// were 8 apart. The owner read it as the composer being pushed away from the
/// history. The transcript now owns no bottom padding at all, so the session's
/// single step is the whole gap, and the strips above the composer use that
/// same step instead of a second, tighter grid.
#[test]
fn the_gap_under_the_last_card_is_the_gap_between_cards() {
    let root = region(
        TRANSCRIPT,
        ".track_scroll(scroll)",
        ".rounded(px(MESSAGE_RADIUS))",
    );
    assert!(
        root.contains(".px_2()\n        .pt_2()"),
        "the transcript pads its top and sides, never its bottom: {root}"
    );
    assert!(
        !root.contains(".pb_"),
        "a bottom pad here stacks on the session's own step"
    );
    assert!(
        root.contains(".gap_2()"),
        "and its rows step by the same 8 the session uses"
    );
    let session = region(
        DASHBOARD_RENDER,
        "let mut session = div()",
        "// Height, not `flex_1`",
    );
    assert!(
        session.contains(".gap_2()"),
        "the session steps 8, the transcript's own row rhythm: {session}"
    );
    let above = region(
        DASHBOARD_RENDER,
        ".overflow_hidden()\n                    // The same 8 the session uses",
        "let mut above_used = false;",
    );
    assert!(
        above.contains(".gap_2()"),
        "and the strips between history and field take that step too: {above}"
    );
}

/// A button is the size of its words, not of the card (#3114).
///
/// The offer card's column stretched every row to the card's width, so a label
/// like "yes" sat at the head of a bar the width of the window with an empty
/// half that read as part of the button. The owner asked for the space to fit
/// the text. `items_start` on the button column makes it take its own width,
/// and because that column still stretches its own children, the widest label
/// is the width all of them share: one size, decided by the text. The card
/// around the column went away in #3124, and the column kept the rule.
#[test]
fn the_offer_buttons_fit_their_labels() {
    let strip = region(
        COMPOSER_VIEW,
        "pub(crate) fn options_strip(",
        "/// Whether a speech-to-text engine is configured",
    );
    assert!(
        strip.contains(".items_start()"),
        "the card must not stretch its buttons to its own width: {strip}"
    );
    assert!(
        !strip.contains("w_full"),
        "no row asks for the card's whole width any more"
    );
    // The plan approval card already sizes its chips to their labels; the pin
    // is here so the two cards cannot drift apart again.
    let card = region(
        PLAN_STRIP,
        "pub fn editing_view(",
        "/// The plan as a split pane draws it",
    );
    assert!(
        card.contains("flex_row()") && card.contains(".gap_2()"),
        "the two decisions sit side by side: {card}"
    );
    let chip = region(
        PLAN_STRIP,
        "fn action_chip(",
        "/// How much of a plan one pane shows",
    );
    assert!(
        chip.contains(".flex_none()"),
        "and each chip keeps its own width instead of growing: {chip}"
    );
}

/// The cards in the conversation's column wear the message card's curve and
/// fill (#3114).
///
/// The offered options were a 32-radius panel and the plan card a 24-radius
/// one, both painted with the raised panel tone, while the bubbles above them
/// sat at 12 on the message surface. Three curves and two tones in one column
/// read as three families of card, and the owner called the plan card out as
/// "mixed with the background" beside bubbles that were not. One radius and one
/// fill, read from the transcript, so the strips cannot drift back.
#[test]
fn the_cards_share_the_message_curve_and_fill() {
    // The offers left this family on purpose (#3124): a card around three
    // short rows was a panel stretched to the column with most of its surface
    // empty, so the buttons now stand bare and only the plan cards wear the
    // bubble's curve and fill. The pin keeps them out of it.
    let strip = region(
        COMPOSER_VIEW,
        "pub(crate) fn options_strip(",
        "/// Whether a speech-to-text engine is configured",
    );
    assert!(
        !strip.contains(".bg(glass_fill("),
        "the offers draw no card of their own: {strip}"
    );
    assert!(
        !strip.contains("SEEKER_RADIUS"),
        "the Seeker's 32 belongs to the command palette, not here"
    );
    assert_eq!(
        PLAN_STRIP.matches(".rounded(px(MESSAGE_RADIUS))").count(),
        5,
        "every plan card: the unseeded row, the approval card, the live \
         checklist, the finished one in the transcript and the split pane's \
         own card (#3117)"
    );
    assert_eq!(
        PLAN_STRIP
            .matches(".bg(glass_fill(message_card_surface()))")
            .count(),
        5,
        "and all five take the message fill"
    );
    // The two approval chips wear the offer rows' chrome (#3124): a control
    // one step inside the message card's curve, with an edge that shows a
    // pointer where to click. They used to be bare text on the raised panel at
    // `CARD_RADIUS`, which read as two words flying loose inside the card
    // rather than as the two decisions it was asking for.
    assert_eq!(
        PLAN_STRIP
            .matches(".rounded(px(MESSAGE_RADIUS - 4.0))")
            .count(),
        1,
        "one chip builder, drawn twice"
    );
    let chip = region(
        PLAN_STRIP,
        "fn action_chip(",
        "/// How much of a plan one pane shows",
    );
    assert!(
        chip.contains(".border_1()"),
        "a chip is a button, so it shows an edge: {chip}"
    );
    // The card's size, not the offer row's (#3140). The chips sit inside the
    // plan card, so they follow the title and the task rows it holds: at the
    // words of a message they read larger than the plan they were approving,
    // which is the one thing on that card a reader should not notice first.
    // The offer rows keep the body size for the opposite reason (#3124): they
    // stand alone, with no card around them to set the measure.
    assert!(
        chip.contains(".text_size(px(SMALL_TEXT_SIZE))"),
        "the chips take the card's own size, not the answer's body size"
    );
    assert!(
        !chip.contains("CARD_RADIUS"),
        "the chips left that curve with the rest of the card family"
    );
}

/// The take's words in the composer sit inside their card, not on its edge
/// (#3114).
///
/// The draft preview padded a flat four, so the caption and the recogniser's
/// transcription started four pixels from the card's own edge and the top row
/// touched the corner: the owner read it as no spacing at all. The same clip
/// reads well once it is sent, and what it has there is the message bubble's
/// inset, so the draft takes those two numbers instead of a third one of its
/// own.
#[test]
fn the_take_preview_pads_like_the_message_it_becomes() {
    let strip = region(
        EDITOR,
        "fn voice_strip(",
        "/// The keybindings the editor needs",
    );
    assert!(
        strip.contains(".px(px(transcript::MESSAGE_PAD_X))"),
        "the sides take the bubble's inset: {strip}"
    );
    assert!(
        strip.contains(".py(px(transcript::MESSAGE_PAD_Y))"),
        "and so does the top and the bottom"
    );
    assert!(
        !strip.contains(".p_1()"),
        "the four-pixel pad is what put the words on the edge"
    );
}
