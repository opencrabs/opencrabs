//! A press that lands before the composer has been laid out must not move the
//! caret (#3031).
//!
//! `index_for_mouse_position` resolves a press to a byte offset by measuring it
//! against `last_bounds`, which the first prepaint writes. Until that happens
//! there is nothing to measure, and the fallback used to be `0`, so a press
//! parked the caret at the start of the draft. `backspace` then selects
//! `previous_boundary(0)`, which is `0` by construction, deletes a zero-width
//! selection, and goes on doing that until something else moves the caret. The
//! user sees a Backspace key that does nothing, with no log line and no error.

fn body_of<'a>(source: &'a str, marker: &'a str) -> &'a str {
    let rest = source
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("{marker} not found"));
    rest.split("\n    fn ").next().unwrap_or("")
}

/// The unresolved-bounds arm returns the caret it already has, not `0`.
#[test]
fn an_unresolvable_press_leaves_the_caret_where_it_is() {
    let editor = include_str!("../gui/editor.rs");
    let body = body_of(editor, "fn index_for_mouse_position(");
    let arm = body
        .split("let Some(bounds) = self.last_bounds.as_ref() else {")
        .nth(1)
        .expect("the missing-bounds arm is still the fallback");
    let arm = arm.split("};").next().unwrap_or("");
    assert!(
        arm.contains("self.cursor_offset()"),
        "an unresolved press must keep the caret it has, got: {arm}"
    );
    assert!(
        !arm.contains("return 0"),
        "returning 0 parks the caret at the start of the draft, which is the bug"
    );
}

/// The press path really goes through that function, so the arm above is the
/// one a click reaches rather than dead code.
#[test]
fn a_press_resolves_its_offset_through_that_function() {
    let editor = include_str!("../gui/editor.rs");
    let press = body_of(editor, "fn on_mouse_down(");
    assert!(
        press.contains("self.index_for_mouse_position(event.position)"),
        "the press feeds its position to the resolver"
    );
}

/// And a press outside the field does not reach it at all, which keeps the
/// guard above from being papered over by a wider bound.
#[test]
fn the_field_evaluates_bounds_before_the_fallback() {
    let editor = include_str!("../gui/editor.rs");
    let resolver = body_of(editor, "fn index_for_mouse_position(");
    let bounds = resolver
        .find("let Some(bounds) = self.last_bounds.as_ref()")
        .expect("the resolver reads last_bounds");
    let empty = resolver
        .find("if self.content.is_empty()")
        .expect("an empty field short-circuits first");
    assert!(
        empty < bounds,
        "an empty field answers 0 before any layout question is asked"
    );
}
