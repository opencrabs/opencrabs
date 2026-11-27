//! Command labels must name the command, not the directory (#790, #791).
//!
//! The indicator on the input border shows one command in 28 characters. Every
//! long command starts by entering the working directory, so a label that keeps
//! the `cd` spends the whole field on the one part that never varies.
//!
//! The second failure is worse than truncation: a multi-line command reaching a
//! single-line renderer loses its newlines, and the tail of one line touches
//! the head of the next to form a token that was never typed.
//!
//! Fixtures use a synthetic home path and carry no user identifiers.

use crate::utils::command_label::{command_label, shrink_paths};

/// The observed shape: enter the directory, then run the real work, separated
/// by a newline rather than `&&`.
const MULTILINE: &str = "cd /Users/dev/srv/rs/opencrabs\necho \"=== --lib (default features) ===\"";

#[test]
fn a_leading_cd_is_dropped() {
    assert_eq!(
        command_label("cd /Users/dev/srv/rs/opencrabs && cargo test -- --list"),
        "cargo test -- --list"
    );
}

#[test]
fn a_newline_separated_cd_is_dropped_too() {
    // The actual bug: `rsplit("&&")` found no separator, so the whole string
    // survived and the label began at `cd `.
    assert_eq!(
        command_label("cd /Users/dev/srv/rs/opencrabs\ncargo test -- --list"),
        "cargo test -- --list"
    );
}

#[test]
fn the_indicator_width_now_shows_the_command() {
    // The regression as the user sees it: at the border's 28 characters the
    // label must have started saying something.
    let label: String = command_label(MULTILINE).chars().take(28).collect();
    assert!(
        label.starts_with("echo"),
        "28 chars of label must name the command, got: {label:?}"
    );
}

#[test]
fn lines_never_fuse_across_a_break() {
    // `opencrabs` + `echo` -> `opencrabsecho` was the reported nonsense token.
    let label = command_label(MULTILINE);
    assert!(
        !label.contains("opencrabsecho"),
        "line break lost, tokens fused: {label:?}"
    );
}

#[test]
fn every_segment_survives() {
    // `rsplit(...).next()` kept only the LAST segment, silently dropping real
    // work from the label.
    assert_eq!(
        command_label("cd /srv/app && cargo build && cargo test"),
        "cargo build; cargo test"
    );
}

#[test]
fn a_trailing_cd_is_kept() {
    // Only LEADING directory changes are noise. One at the end is doing
    // something and dropping it would misdescribe the command.
    assert_eq!(
        command_label("cd /srv/app && make && cd /srv/other"),
        "make; cd /srv/other"
    );
}

#[test]
fn a_separator_inside_quotes_is_not_a_separator() {
    // A naive split would cut this in half and invent a segment.
    assert_eq!(
        command_label("cd /srv/app && echo \"one; two && three\""),
        "echo \"one; two && three\""
    );
}

#[test]
fn a_command_that_is_only_cd_still_renders() {
    // Stripping everything would leave an empty field, which reads as broken.
    assert_eq!(command_label("cd /srv/app"), "cd /srv/app");
}

#[test]
fn whitespace_runs_collapse() {
    // Commands formatted across columns must read as one line.
    assert_eq!(
        command_label("cd /srv/app &&   cargo    test   --all-features"),
        "cargo test --all-features"
    );
}

#[test]
fn an_empty_command_is_empty() {
    // Callers supply their own placeholder rather than getting a fake one.
    assert_eq!(command_label(""), "");
    assert_eq!(command_label("   \n  "), "");
}

#[test]
fn a_plain_command_is_untouched() {
    assert_eq!(
        command_label("cargo clippy --all-features"),
        "cargo clippy --all-features"
    );
}

#[test]
fn semicolon_separated_leading_cd_is_dropped() {
    assert_eq!(command_label("cd /srv/app; cargo test"), "cargo test");
}

#[test]
fn a_long_home_path_keeps_its_file_name() {
    // The observed row (#3204): the whole field went on directories and the
    // label never reached the script name, which is the only part that says
    // what is running.
    assert_eq!(
        shrink_paths("bash \"$HOME/.opencrabs/projects/opencrabs/files/gate-combo.sh\""),
        "bash \"~/…/gate-combo.sh\""
    );
}

#[test]
fn an_absolute_path_keeps_its_first_directory() {
    // A path outside the home directory still has to say where it lives.
    assert_eq!(
        shrink_paths("/Users/dev/srv/rs/opencrabs/scripts/gate.sh"),
        "/Users/…/gate.sh"
    );
}

#[test]
fn a_short_path_is_left_alone() {
    // Folding a path that already fits would cost the reader information for
    // nothing.
    assert_eq!(shrink_paths("tail -f /tmp/x.log"), "tail -f /tmp/x.log");
}

#[test]
fn an_option_value_shrinks_but_its_option_stays() {
    assert_eq!(
        shrink_paths("opencrabs --config=/Users/dev/projects/app/config.toml"),
        "opencrabs --config=/Users/…/config.toml"
    );
}
