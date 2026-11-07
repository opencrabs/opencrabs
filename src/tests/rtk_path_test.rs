//! Tests for the PATH handling in `src/rtk/rewrite.rs`.
//!
//! `ensure_dir_in_path` puts a directory on PATH so the bare `rtk` name
//! resolves at exec time. PATH is a *list*, and its separator is `:` on unix
//! but `;` on Windows. Joining with the wrong one does not add an entry — it
//! fuses the new directory to the first existing one, so a Windows host ends
//! up with `C:\Users\me\bin:C:\WINDOWS\system32` as a single bogus entry and
//! loses `C:\Windows\system32` (`where.exe`, `findstr.exe`, `tasklist.exe`)
//! for every process that inherits the environment.
//!
//! These tests assert on the *shape* of the result through
//! [`std::env::split_paths`], so they carry no hardcoded separator and mean
//! the same thing on every host. They are the reason the implementation may
//! not format the joined value by hand: the wrong separator shows up here as
//! a lost entry, not on a user's machine as a missing `where.exe`.

use crate::rtk::rewrite::prepend_rtk_dir;

/// A directory shaped for the host, so the expectations read naturally.
fn dir() -> String {
    if cfg!(windows) {
        r"C:\tools\rtk".to_string()
    } else {
        "/opt/rtk/bin".to_string()
    }
}

/// A pre-existing PATH entry shaped for the host.
fn existing_entry() -> String {
    if cfg!(windows) {
        r"C:\Windows\system32".to_string()
    } else {
        "/usr/bin".to_string()
    }
}

/// The host's PATH separator, for building inputs the same way the OS does.
fn sep() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

/// The entries of a PATH-shaped string, as the platform splits them.
fn entries(path: &str) -> Vec<std::path::PathBuf> {
    std::env::split_paths(path).collect()
}

#[test]
fn prepended_dir_becomes_its_own_path_entry() {
    let dir = dir();
    let existing = existing_entry();

    let new_path = prepend_rtk_dir(&existing, &dir).expect("an absent dir is prepended");

    let found = entries(&new_path);
    assert_eq!(
        found.len(),
        2,
        "PATH must gain exactly one entry, got {found:?} from {new_path:?}"
    );
    assert_eq!(found[0], std::path::Path::new(&dir));
    assert_eq!(found[1], std::path::Path::new(&existing));
}

#[test]
fn dir_already_on_path_is_not_prepended_again() {
    let dir = dir();
    let existing = existing_entry();

    let once = prepend_rtk_dir(&existing, &dir).expect("the first call prepends");
    assert_eq!(
        prepend_rtk_dir(&once, &dir),
        None,
        "the dir is already an entry of {once:?}"
    );
}

#[test]
fn empty_path_yields_the_dir_alone() {
    let dir = dir();
    let new_path = prepend_rtk_dir("", &dir).expect("an empty PATH still takes the dir");
    assert_eq!(entries(&new_path), vec![std::path::PathBuf::from(&dir)]);
}

#[test]
fn every_existing_entry_survives_the_prepend() {
    // Several entries, so a wrong separator cannot be excused as "one entry
    // fused and nothing else lost": the count itself is the assertion, and
    // order is the second half of it.
    let dir = dir();
    let existing = [
        existing_entry(),
        if cfg!(windows) {
            r"C:\Windows".to_string()
        } else {
            "/bin".to_string()
        },
        if cfg!(windows) {
            r"C:\Windows\System32\Wbem".to_string()
        } else {
            "/usr/local/bin".to_string()
        },
    ];
    let original = existing.join(sep());

    let new_path = prepend_rtk_dir(&original, &dir).expect("an absent dir is prepended");
    let found = entries(&new_path);

    assert_eq!(
        found.len(),
        existing.len() + 1,
        "PATH must gain exactly one entry, got {found:?} from {new_path:?}"
    );
    assert_eq!(found[0], std::path::Path::new(&dir));
    assert_eq!(
        &found[1..],
        &entries(&original)[..],
        "existing entries keep their order: {found:?}"
    );
}

#[test]
fn trailing_separator_does_not_create_an_entry() {
    // A trailing separator is legal PATH shape meaning "no further entry";
    // the result must still carry exactly the dir plus the real entry, not an
    // empty path that a child process would resolve against its cwd.
    let dir = dir();
    let existing = existing_entry();
    let trailing = format!("{existing}{}", sep());

    let new_path = prepend_rtk_dir(&trailing, &dir).expect("an absent dir is prepended");

    assert_eq!(
        entries(&new_path),
        vec![
            std::path::PathBuf::from(&dir),
            std::path::PathBuf::from(&existing)
        ],
        "got {new_path:?}"
    );
}
