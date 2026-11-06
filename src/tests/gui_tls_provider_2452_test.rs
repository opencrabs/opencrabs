//! The window installs the TLS provider the terminal binary installs (#2452).
//!
//! Both binaries serve the same channels. rustls refuses to pick a provider
//! from the crate features when more than one is enabled, so the Slack
//! connector panicked inside `SlackAgent::start` on every GUI boot while the
//! terminal binary, which had installed the provider, kept connecting. The
//! window has no headless harness, so the wiring is read from source.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const TERMINAL_MAIN: &str = include_str!("../main.rs");

const INSTALL: &str = "rustls::crypto::ring::default_provider().install_default()";

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let after = source
        .split_once(start)
        .map(|(_, rest)| rest)
        .expect("start marker");
    after
        .split_once(end)
        .map(|(body, _)| body)
        .expect("end marker")
}

/// The window's boot installs the provider before it starts logging, and the
/// step is gated on the feature that pulls rustls in, exactly like the
/// terminal binary.
#[test]
fn the_window_installs_the_provider_before_it_serves_a_channel() {
    let boot = between(GUI_MAIN, "fn main() {", "let db_path = ledger_path();");
    assert!(
        boot.contains(INSTALL),
        "the window must install the rustls provider before any TLS connection"
    );
    assert!(
        boot.contains("#[cfg(feature = \"slack\")]"),
        "the install is gated on the feature that pulls rustls, like src/main.rs"
    );

    let install = boot.find(INSTALL).expect("install");
    let logging = boot.find("logging::init()").expect("logging");
    assert!(
        install < logging,
        "the provider is installed before logging starts"
    );
}

/// One line, two binaries. If either side renames or drops it, the other stops
/// being the mirror this test exists to keep.
#[test]
fn both_binaries_write_the_same_install_line() {
    assert!(
        TERMINAL_MAIN.contains(INSTALL),
        "the terminal binary's install moved"
    );
    assert!(GUI_MAIN.contains(INSTALL), "the window's install moved");
}
