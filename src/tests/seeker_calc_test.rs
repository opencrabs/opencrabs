//! The arithmetic half of Seeker: a query that is a calculation answers
//! itself, and a query that is not keeps its ordinary search behaviour.
//!
//! These pin the contract the card promises: the printed shapes, the
//! operators, and the cases that must not produce a row at all.

use crate::ui_core::calc::{self, MAX_INPUT};

fn answer(input: &str) -> Option<String> {
    calc::evaluate(input)
}

/// The case from the request: `200/7.1` prints a number, not a chat offer.
#[test]
fn evaluates_division_with_decimals() {
    assert_eq!(answer("200/7.1").as_deref(), Some("28.1690140845"));
    assert_eq!(answer("1/3").as_deref(), Some("0.3333333333"));
    assert_eq!(answer("10/4").as_deref(), Some("2.5"));
}

#[test]
fn respects_precedence_and_parentheses() {
    assert_eq!(answer("2+3*4").as_deref(), Some("14"));
    assert_eq!(answer("(2+3)*4").as_deref(), Some("20"));
    assert_eq!(answer("1-(2-3)").as_deref(), Some("2"));
    assert_eq!(answer("((2+3)*(4-1))/5").as_deref(), Some("3"));
}

/// The power operator reads right to left, the way it is written.
#[test]
fn power_is_right_associative() {
    assert_eq!(answer("2^3^2").as_deref(), Some("512"));
    assert_eq!(answer("2^10").as_deref(), Some("1024"));
    assert_eq!(answer("2^-1").as_deref(), Some("0.5"));
}

#[test]
fn unary_minus_binds_inside_parentheses() {
    assert_eq!(answer("-(2+3)").as_deref(), Some("-5"));
    assert_eq!(answer("2-(-3)").as_deref(), Some("5"));
    assert_eq!(answer("3.5*-2").as_deref(), Some("-7"));
}

/// `%` is modulo, which is what a developer expects from `200 % 7`.
#[test]
fn percent_is_modulo() {
    assert_eq!(answer("200%7").as_deref(), Some("4"));
    assert_eq!(answer("2--3").as_deref(), Some("5"));
}

/// A bare number and ordinary words are searches, not sums: the card keeps
/// its normal behaviour and prints no answer row.
#[test]
fn rejects_prose_and_bare_numbers() {
    for query in [
        "deploy", "200", "0", "-200", "(-5)", "200 usd", "1e5", "2 3", "2)", "", "   ",
    ] {
        assert_eq!(answer(query), None, "{query:?} is not arithmetic");
    }
}

#[test]
fn rejects_division_by_zero() {
    assert_eq!(answer("1/0"), None);
    assert_eq!(answer("5%0"), None);
    assert_eq!(answer("1/(2-2)"), None);
    // A result that is not a finite number has no row either.
    assert_eq!(answer("2^2000"), None);
}

/// Ten decimals are what kills the float noise, and a whole number keeps no
/// decimal point.
#[test]
fn formats_float_noise_away() {
    assert_eq!(answer("0.1+0.2").as_deref(), Some("0.3"));
    assert_eq!(answer("20/5").as_deref(), Some("4"));
    assert_eq!(answer("2^40").as_deref(), Some("1099511627776"));
    // Past the usable range the answer prints in scientific form rather than
    // dragging a line of digits across the card.
    assert_eq!(answer("2^200").as_deref(), Some("1.606938044e60"));
}

#[test]
fn caps_input_length() {
    let long = format!("1{}1", "+".repeat(MAX_INPUT));
    assert!(long.len() > MAX_INPUT);
    assert_eq!(answer(&long), None);
    // A long but legal expression still evaluates.
    let ok = format!("1{}", "+1".repeat((MAX_INPUT - 1) / 2));
    assert_eq!(ok.len(), MAX_INPUT - 1);
    assert_eq!(answer(&ok).as_deref(), Some("64"));
}

#[test]
fn caps_nesting_depth() {
    let fine = format!("{}1+1{}", "(".repeat(32), ")".repeat(32));
    assert_eq!(answer(&fine).as_deref(), Some("2"));
    let deep = format!("{}1+1{}", "(".repeat(33), ")".repeat(33));
    assert_eq!(answer(&deep), None, "nesting past the cap is not a sum");
}
