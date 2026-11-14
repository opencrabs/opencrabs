//! The Help Center is four section cards, not one long column (#2893).
//!
//! The window still has no headless harness, so these read the renderer's
//! source the same way the other GUI guards do. What they pin is the shape
//! that would be expensive to lose silently: one data source, one section
//! order, one close hint, and counts that come from the rows rather than from
//! a number somebody typed.

const HELP_VIEW: &str = include_str!("../gui/help_view.rs");
const OVERLAYS: &str = include_str!("../gui/overlays.rs");
const GUI_MAIN: &str = include_str!("../gui/main.rs");

#[test]
fn the_cards_read_the_shared_table_and_its_order() {
    // One source for the rows, one for their order. The view must not hold a
    // shortcut or a section name of its own, or the terminal and the window
    // drift apart again the way they did in #2319.
    assert!(
        HELP_VIEW.contains("help_rows()"),
        "the cards must paint the shared rows"
    );
    assert!(
        HELP_VIEW.contains("HelpSection::ALL"),
        "the cards must follow the declared section order"
    );
    for title in ["GLOBAL", "CHAT", "INPUT EDITING", "SESSIONS"] {
        assert!(
            !HELP_VIEW.contains(&format!("\"{title}\"")),
            "the card view spells {title} instead of reading it from the section"
        );
    }
}

#[test]
fn every_shared_section_reaches_the_cards_exactly_once() {
    use crate::ui_core::help_keys::{self, HelpSection};

    // The cards iterate `HelpSection::ALL`, so a section missing from ALL
    // would vanish from the window, and a section whose rows the window
    // cannot answer would paint an empty card.
    let painted: Vec<HelpSection> = help_keys::gui_rows().map(|row| row.section).collect();
    for section in HelpSection::ALL {
        assert!(
            painted.contains(&section),
            "{} has no row the window answers",
            section.title()
        );
    }
    // Grouping must neither drop nor duplicate: the flat collection the view
    // reads has to equal the sum of what each card would print.
    let per_section: usize = HelpSection::ALL
        .iter()
        .map(|section| {
            help_keys::section_rows(*section)
                .filter(|row| row.gui.is_some())
                .count()
        })
        .sum();
    assert_eq!(
        painted.len(),
        per_section,
        "a row is missing from the cards or printed twice"
    );
}

#[test]
fn the_old_single_column_list_is_gone() {
    // The old renderer printed a heading into the same scroller as the rows
    // and remembered the last heading it had seen. Both markers have to be
    // gone, together with the function that painted them.
    assert!(!HELP_VIEW.contains("printed: Option<"));
    assert!(!OVERLAYS.contains("fn help_overlay"));
    assert!(!OVERLAYS.contains("struct HelpView"));
    assert!(HELP_VIEW.contains("fn section_card"));
}

#[test]
fn the_close_hint_is_printed_once() {
    // #2888 fixed the duplicated close affordance on Projects; the Help
    // Center had the same defect, once in the header and once in the footer.
    assert_eq!(
        HELP_VIEW.matches("\"esc\"").count(),
        1,
        "esc is printed more than once"
    );
    assert!(
        !HELP_VIEW.contains("child(SharedString::from(\"esc\"))"),
        "esc is bare text again, not a keycap"
    );
}

#[test]
fn the_screen_keeps_its_layer_contract() {
    // The screen covers the window and takes the pointer, the background is
    // the palette's, and the arrows still drive the page scroll.
    for needle in [
        ".id(\"help-screen\")",
        ".occlude()",
        ".size_full()",
        ".bg(rgb(shown.bg))",
        "overflow_y_scroll()",
        "track_scroll(&view.scroll)",
    ] {
        assert!(HELP_VIEW.contains(needle), "the help screen lost {needle}");
    }
}

#[test]
fn the_counts_are_derived_from_the_rows() {
    // A literal total goes stale the day a chord is added, and a literal per
    // card count goes stale even sooner.
    assert!(
        HELP_VIEW.contains("groups.iter().map(HelpGroup::count).sum()"),
        "the total must be summed from the groups"
    );
    assert!(
        HELP_VIEW.contains("group.count().to_string()"),
        "each card must print its own row count"
    );
    assert!(
        !HELP_VIEW.contains("36"),
        "the painted row total is hardcoded"
    );
}

#[test]
fn the_grid_collapses_on_a_narrow_window() {
    assert!(
        HELP_VIEW.contains("if view.narrow"),
        "the grid has no single-column fallback"
    );
    assert!(
        GUI_MAIN.contains("narrow: f32::from(window.viewport_size().width)"),
        "the window never tells the screen how wide it is"
    );
    assert!(
        GUI_MAIN.contains("root = root.child(help_screen(&view))"),
        "the help screen is not attached to the render"
    );
}
