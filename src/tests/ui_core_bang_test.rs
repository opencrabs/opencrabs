//! The `!` operator's parsing and output text (#2437).

use crate::ui_core::bang::*;

#[test]
fn a_bang_line_yields_its_command() {
    assert_eq!(bang_command("!ls -la"), Some("ls -la"));
    assert_eq!(bang_command("  !  git status "), Some("git status"));
}

#[test]
fn an_empty_bang_or_plain_text_is_not_a_command() {
    assert_eq!(bang_command("!"), None);
    assert_eq!(bang_command("   !   "), None);
    assert_eq!(bang_command("hello !world"), None);
}

#[test]
fn editors_are_recognised_by_basename() {
    assert!(is_editor("vi notes.md"));
    assert!(is_editor("/usr/bin/nano file.txt"));
    assert!(!is_editor("cat notes.md"));
    assert!(!is_editor(""));
}

#[test]
fn output_joins_stdout_and_stderr_and_names_silence() {
    assert_eq!(output_text(b"a\n", b"b\n", true), "a\nb");
    assert_eq!(output_text(b"", b"", true), "(no output)");
    assert_eq!(
        output_text(b"", b"", false),
        "(exited with an error, no output)"
    );
}
