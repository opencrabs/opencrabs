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
fn a_bare_editor_is_an_empty_buffer() {
    let cmd = editor_command("vi").expect("vi is an editor");
    assert_eq!(cmd.program, "vi");
    assert_eq!(cmd.path, None);
    assert_eq!(cmd.line, None);
    assert!(!cmd.read_only);
}

#[test]
fn an_editor_with_a_path_opens_that_file() {
    let cmd = editor_command("nano notes.md").expect("nano is an editor");
    assert_eq!(cmd.program, "nano");
    assert_eq!(cmd.path.as_deref(), Some("notes.md"));
}

#[test]
fn the_line_and_the_read_only_flag_are_read() {
    let cmd = editor_command("vi -R +12 src/main.rs").expect("vi is an editor");
    assert_eq!(cmd.path.as_deref(), Some("src/main.rs"));
    assert_eq!(cmd.line, Some(12));
    assert!(cmd.read_only);
}

#[test]
fn a_quoted_path_stays_one_argument() {
    let cmd = editor_command("vi \"my notes.md\"").expect("vi is an editor");
    assert_eq!(cmd.path.as_deref(), Some("my notes.md"));
}

#[test]
fn an_editor_path_is_found_after_the_flags() {
    let cmd = editor_command("vim -u NONE -- notes.md").expect("vim is an editor");
    assert_eq!(cmd.path.as_deref(), Some("notes.md"));
    assert!(cmd.flags.contains(&"-u".to_string()));
    assert!(cmd.flags.contains(&"NONE".to_string()));
}

#[test]
fn the_file_is_the_last_plain_argument() {
    let cmd = editor_command("vim -u NONE notes.md").expect("vim is an editor");
    assert_eq!(cmd.path.as_deref(), Some("notes.md"));
}

#[test]
fn a_command_that_is_not_an_editor_has_no_editor_request() {
    assert!(editor_command("ls -la").is_none());
    assert!(editor_command("").is_none());
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
