// Witness for #1621: README.md and src/docs/ shipped a heartbeat subsystem
// the binary never implemented — intervals (`heartbeat.every`, JSON5
// `heartbeat: { every }`), `HEARTBEAT_OK` ack suppression, `ackMaxChars`,
// and config paths under `agents.defaults.*` that the schema does not
// accept. None of it exists. Periodic checks are cron jobs (cron_manage)
// whose prompt reads HEARTBEAT.md; that file is a plain checklist.
//
// This test scans the shipped docs so the phantom claims cannot return.

use std::fs;
use std::path::Path;

/// Phrases that advertise the nonexistent subsystem or schema-invalid
/// config paths. Plain mentions of the HEARTBEAT.md brain file are fine.
const BANNED: &[&str] = &[
    "heartbeat.every",
    "heartbeat: {",
    "HEARTBEAT_OK",
    "ackMaxChars",
    "agents.defaults",
];

/// Fiction tokens from the imported OPENCRABS.md doc (#1650): CLI commands
/// the binary never had, JSON5 config keys, invented media syntax, and
/// wrong log paths. Banned tree-wide (README + src/docs/).
const BANNED_IMPORTED_DOC: &[&str] = &[
    "Pi agent",
    "gateway.auth",
    "allowFrom",
    "{{MediaPath}}",
    "MEDIA:",
    "/tmp/opencrabs.log",
    "/tmp/opencrabs.err",
    "resetTriggers",
    "thinkingDefault",
    "health --json",
    "opencrabs setup",
    // Commands from the same imported doc. They used to need a per-file
    // carve-out because WIZARD.md still carried them; that file is gone
    // (#2153), so they are banned everywhere like the rest.
    "opencrabs gateway",
    "opencrabs dashboard",
];

/// Environment variables the binary has never read. They came from the
/// imported docs, where they looked plausible enough to be copied into a
/// systemd unit and an installation page. The real set is whatever
/// `std::env::var` actually asks for, so these are banned tree-wide: a
/// dead variable in a unit file is a service that quietly ignores it.
const BANNED_ENV_VARS: &[&str] = &[
    "OPENCRABS_HOME",
    "OPENCRABS_STATE_DIR",
    "OPENCRABS_CONFIG_PATH",
];

fn scanned_docs() -> Vec<(String, String)> {
    let mut out = vec![(
        "README.md".to_string(),
        fs::read_to_string("README.md").expect("README.md readable"),
    )];
    collect_markdown(Path::new("src/docs"), &mut out);
    out
}

fn collect_markdown(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("src/docs readable") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let name = path.display().to_string();
            let content = fs::read_to_string(&path).unwrap_or_default();
            out.push((name, content));
        }
    }
}

#[test]
fn docs_never_advertise_the_phantom_heartbeat_subsystem() {
    let docs = scanned_docs();
    assert!(
        docs.len() > 10,
        "doc scan found suspiciously few files ({}) — did the layout move?",
        docs.len()
    );
    for (name, content) in docs {
        for banned in BANNED {
            assert!(
                !content.contains(banned),
                "{name} advertises '{banned}' — the heartbeat subsystem does not \
                 exist in the binary and `agents.defaults.*` is not a valid config \
                 path (see #1621). Periodic checks are cron jobs reading \
                 HEARTBEAT.md."
            );
        }
    }
}

#[test]
fn docs_never_carry_the_imported_doc_fiction() {
    let docs = scanned_docs();
    for (name, content) in &docs {
        for banned in BANNED_IMPORTED_DOC {
            assert!(
                !content.contains(banned),
                "{name} contains '{banned}' — imported-doc fiction from another \
                 product (see #1650). The binary has no such command, config key, \
                 media syntax, or log path."
            );
        }
    }
}

#[test]
fn docs_never_name_environment_variables_the_binary_does_not_read() {
    let docs = scanned_docs();
    for (name, content) in &docs {
        for banned in BANNED_ENV_VARS {
            assert!(
                !content.contains(banned),
                "{name} names {banned}, but the binary never reads it (see \
                 #2142). A dead variable in a systemd unit is a service that \
                 silently ignores what it was told. Real ones: \
                 OPENCRABS_PROFILE, OPENCRABS_DB_PATH, OPENCRABS_LOG_LEVEL, \
                 OPENCRABS_LOG_FILE."
            );
        }
    }
}

// ── the macOS GUI app doc family (#2154) ────────────────────────────────────
//
// `ONBOARDING.md` and `ONBOARDING_OVERVIEW.md` described a macOS app with TCC
// permission prompts, a Local vs Remote gateway picker and OAuth flows. The
// repo has no GUI: the first run goes straight to chat on the built-in
// provider, and the wizard is a terminal flow under `src/tui/onboarding/`.
// Both files are deleted (#2154), after WIZARD.md (#2153), GETTING_STARTED.md
// (#2142) and the OPENCRABS.md rewrite (#1650). These tokens are what the
// family kept claiming, so they are banned tree-wide: a doc that describes an
// app that does not exist is worse than no doc at all.

/// Fiction from the deleted macOS-app onboarding docs. None of it names a
/// file, command, permission request or URL this repo contains.
const BANNED_MACOS_APP_DOC: &[&str] = &[
    "macos-onboarding",
    "Onboarding (macOS App)",
    "Gateway auth tip",
    "TCC permissions",
    "Local vs Remote",
    "/start/bootstrapping",
    "/start/onboarding",
    "/start/wizard",
    "/cli/onboard",
];

#[test]
fn docs_never_describe_the_macos_app_onboarding_flow() {
    let docs = scanned_docs();
    for (name, content) in &docs {
        for banned in BANNED_MACOS_APP_DOC {
            assert!(
                !content.contains(banned),
                "{name} contains '{banned}': that is the macOS GUI onboarding \
                 doc family, deleted in #2154 (after #2153, #2142, #1650). This \
                 repo ships no GUI: a fresh install goes straight to chat, and \
                 `opencrabs onboard` is the terminal wizard."
            );
        }
    }
}

/// The deleted docs were the only thing pointing at themselves. README links
/// the live page instead, so the wizard stays reachable from the docs index.
#[test]
fn the_deleted_onboarding_docs_stay_deleted() {
    for gone in [
        "src/docs/start/ONBOARDING.md",
        "src/docs/start/ONBOARDING_OVERVIEW.md",
    ] {
        assert!(
            !Path::new(gone).exists(),
            "{gone} is back. It documents a macOS app this repo does not \
             contain (see #2154); the live page is \
             https://docs.opencrabs.com/getting-started/onboarding.html"
        );
    }
    let readme = fs::read_to_string("README.md").expect("README.md readable");
    assert!(
        readme.contains("https://docs.opencrabs.com/getting-started/onboarding.html"),
        "README must link the live onboarding page now that the local doc is gone"
    );
}
