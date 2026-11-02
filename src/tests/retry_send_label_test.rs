//! The retry label counts sends, not retries (#2284). The first retry is the
//! second of five sends, so it must read "Retry attempt 2 of 5", and the label
//! keeps the `Retry ` prefix Telegram uses to replace retry lines in place.

use crate::utils::retry::{in_place_attempt_budget, send_label};

#[test]
fn first_retry_is_the_second_send_of_the_budget() {
    let budget = in_place_attempt_budget();
    // The provider reports the retry ceiling, which is one less than the budget.
    assert_eq!(send_label(1, budget - 1), "Retry attempt 2 of 5");
}

#[test]
fn the_last_retry_reads_the_budget_total() {
    let budget = in_place_attempt_budget();
    assert_eq!(
        send_label(budget - 1, budget - 1),
        format!("Retry attempt {budget} of {budget}")
    );
}

#[test]
fn the_label_keeps_the_retry_prefix_for_in_place_replacement() {
    assert!(send_label(1, 4).starts_with("Retry "));
}
