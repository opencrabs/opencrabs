// Witness for #2142: the one-line installer is what an agent pipes into bash,
// so it must be safe to run with nobody watching. Two defects lived here.
//
// 1. It could block forever on a sudo password prompt. An agent has no
//    terminal, so the prompt never gets answered and the run hangs with no
//    timeout. It now fails fast and names the writable-dir fix instead.
// 2. On macOS it built the asset name from `uname -s`, which is `Darwin`, so
//    it asked for `opencrabs-v0.5.11-darwin-arm64.tar.gz`. Every release
//    publishes `macos-arm64`, so the download 404'd and the installer had
//    never worked on a Mac. Confirmed against the real v0.5.11 assets.
//
// The checks are structural because the script talks to GitHub, which no test
// should depend on. Anything provable without a network belongs here.

use std::fs;

const SCRIPT: &str = "src/scripts/install.sh";

fn script() -> String {
    fs::read_to_string(SCRIPT).expect("install.sh readable")
}

/// Code lines only, so the doc comment explaining the rule cannot trip it.
fn code_lines() -> Vec<String> {
    script()
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && !t.is_empty()
        })
        .map(|l| l.to_string())
        .collect()
}

// Unix only: this one shells out to `bash -n`, and `bash` is not guaranteed
// on a Windows host. Every other test in this file is a string assertion
// against install.sh, so they stay portable.
#[cfg(unix)]
#[test]
fn the_installer_parses_as_bash() {
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(SCRIPT)
        .output()
        .expect("bash available");
    assert!(
        out.status.success(),
        "install.sh does not parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_installer_never_waits_for_input() {
    // `read` blocks on a pipe that never sends anything. A prompt is the same
    // hang wearing a friendlier face.
    for line in code_lines() {
        let t = line.trim();
        assert!(
            !t.starts_with("read "),
            "install.sh calls `read`, which blocks forever with no stdin: {line}"
        );
        assert!(
            !t.contains("read -p") && !t.contains("read -r"),
            "install.sh prompts for input, which hangs an unattended run: {line}"
        );
    }
}

#[test]
fn sudo_never_prompts_unless_a_terminal_is_present() {
    // A bare `sudo` reads /dev/tty. With no terminal that is a wait that never
    // ends. It is allowed only on the branch guarded by the INTERACTIVE check.
    let src = script();
    assert!(
        src.contains("INTERACTIVE=1"),
        "install.sh must decide whether a terminal exists before using sudo"
    );
    assert!(
        src.contains("sudo -n"),
        "the unattended path must use `sudo -n`, which fails instead of prompting"
    );

    for line in code_lines() {
        let t = line.trim();
        // Only a line that STARTS with sudo is a command. "passwordless sudo
        // is not available" is prose inside an error message.
        if !t.starts_with("sudo ") || t.starts_with("sudo -n") {
            continue;
        }
        // The only permitted bare sudo is inside the as_root() interactive arm,
        // which is unreachable when INTERACTIVE=0.
        assert!(
            t == "sudo \"$@\"",
            "install.sh runs a bare `sudo` outside the guarded interactive arm, \
             which hangs an unattended run: {line}"
        );
    }
    assert!(
        src.contains("if [ -t 2 ] && [ -r /dev/tty ]"),
        "the interactive decision must check for a real terminal, not just stdin \
         (stdin is the curl pipe in `curl | bash`)"
    );
}

#[test]
fn the_macos_asset_name_matches_what_the_releases_publish() {
    // The regression that made macOS installs impossible: `uname -s` says
    // Darwin, the release assets say macos.
    let src = script();
    assert!(
        src.contains(r#"darwin) OS="macos""#),
        "install.sh must map darwin to macos before building the asset name, or \
         the download 404s on every Mac"
    );
    // The asset name must be built from the mapped value. The releases publish
    // macos-*, so a literal darwin anywhere in a filename is the old bug.
    assert!(
        src.contains(r#"FILENAME="opencrabs-${TAG}-${OS}-${ARCH}.tar.gz""#),
        "the asset name must come from the mapped OS, not from uname"
    );
    assert!(
        !src.contains("darwin-${ARCH}") && !src.contains("darwin-amd64"),
        "install.sh builds a darwin asset name, which no release publishes"
    );
}

#[test]
fn the_installer_verifies_the_download_before_unpacking() {
    let src = script();
    let verify = src
        .find("SHA256SUMS")
        .expect("install.sh must fetch SHA256SUMS");
    let extract = src
        .find("tar xzf")
        .expect("install.sh must extract the archive");
    assert!(
        verify < extract,
        "the checksum must be checked before the archive is unpacked, otherwise \
         a corrupt download is already on disk when it fails"
    );
    assert!(
        src.contains("Checksum mismatch"),
        "a mismatch must be a hard failure with a named reason"
    );
}

#[test]
fn the_installer_installs_the_helper_binary_beside_the_agent() {
    // Every archive ships rtk next to opencrabs. Installing only the agent
    // leaves a feature flag promising a helper that is not on disk.
    let src = script();
    assert!(
        src.contains("install -m 755 \"${TMPDIR}/rtk\""),
        "install.sh must install rtk, which every release archive ships"
    );
}

#[test]
fn the_installer_handles_both_archive_layouts() {
    // Up to v0.5.11 the macOS archive carries a loose binary. #2115 changed it
    // to OpenCrabs.app, so the installer has to accept either rather than
    // assume the one that happens to be current.
    let src = script();
    assert!(
        src.contains("OpenCrabs.app"),
        "install.sh must handle the bundled macOS layout from #2115"
    );
    assert!(
        src.contains("\"${TMPDIR}/opencrabs\""),
        "install.sh must still handle the loose-binary layout of older releases"
    );
}

#[test]
fn the_installer_offers_a_dry_run_and_a_dependency_opt_out() {
    let src = script();
    for flag in ["--dry-run", "--no-deps", "--dir", "--version"] {
        assert!(
            src.contains(flag),
            "install.sh must accept {flag}: a caller that cannot inspect what \
             will happen before it happens is a caller that cannot verify"
        );
    }
    // --dry-run must exit before anything is written.
    let dry = src.find("Dry run: would install").expect("dry run branch");
    let download = src.find("Downloading ${TAG}").expect("download step");
    assert!(
        dry < download,
        "the dry run must exit before the download, not after it"
    );
}

// #2203: the installer used to finish by printing an `export PATH` line and
// calling that installed. The user closes the terminal and the command is
// gone, which is the path hunting a one-line installer exists to spare them.
// A target directory that is off PATH now gets an entry written once.

#[test]
fn the_installer_creates_a_path_entry_when_the_target_is_off_path() {
    let src = script();
    assert!(
        src.contains(r#"export PATH=\"${INSTALL_DIR}:\$PATH\""#),
        "install.sh must build the PATH line it writes, or an install into a \
         directory off PATH leaves no callable command"
    );
    // The entry is useless in a file the user's shell never reads, so each
    // shell has to map to the startup file it actually sources.
    for rc in [".zshrc", ".bashrc", ".bash_profile", ".profile"] {
        assert!(
            src.contains(rc),
            "install.sh must know about {rc}, otherwise it writes the PATH entry \
             into a file the user's shell never reads"
        );
    }
    assert!(
        src.contains(">>\"$RC\""),
        "install.sh must append the PATH line to the shell startup file"
    );
}

#[test]
fn the_installer_does_not_duplicate_the_path_entry() {
    let src = script();
    assert!(
        src.contains(r#"grep -Fq "$PATH_LINE" "$RC""#),
        "the PATH entry must be guarded by a check, or every reinstall appends \
         another copy of the same line"
    );
    assert!(
        src.contains("already puts ${INSTALL_DIR} on PATH"),
        "a second run must report the entry as already present instead of \
         writing it again"
    );
}

#[test]
fn the_installer_can_skip_the_path_entry() {
    let src = script();
    assert!(
        src.contains("--no-path"),
        "install.sh must accept --no-path for callers that manage PATH themselves"
    );
    assert!(
        src.contains("WITH_PATH_ENTRY=0"),
        "--no-path must actually switch the PATH write off"
    );
    assert!(
        src.contains("--no-path was given"),
        "the skipped case must say so, or a caller cannot tell why the command \
         is still missing"
    );
}

#[test]
fn the_installer_verifies_the_command_resolves_before_reporting_success() {
    let src = script();
    let verify = src
        .find("command -v opencrabs")
        .expect("install.sh must check that the command a user types resolves");
    let done = src
        .find("Run 'opencrabs' to get started!")
        .expect("install.sh must print the closing line");
    assert!(
        verify < done,
        "the resolution check must run before the success line, otherwise a \
         broken PATH still reports a finished install"
    );
}

// #2203: on a fresh Mac `/usr/local/bin` does not exist (it is listed in
// /etc/paths, so it is on the default PATH once created, but nothing creates
// it). `ln -sf` into a missing directory fails with "No such file or
// directory", so the documented macOS one-liner died on its last step after
// the user had already copied the app.

#[test]
fn the_readme_one_liners_create_the_link_directory_before_linking() {
    let readme = fs::read_to_string("README.md").expect("README.md readable");
    let mut checked = 0;

    for line in readme.lines() {
        // Only the download-and-install one-liners, which write into
        // /usr/local/bin as part of a single piped command.
        if !line.starts_with("curl -sL \"$(curl -s https://api.github.com") {
            continue;
        }
        checked += 1;

        let mkdir = line.find("mkdir -p /usr/local/bin");
        assert!(
            mkdir.is_some(),
            "a README one-liner writes into /usr/local/bin without creating it \
             first, which fails on a fresh Mac: {line}"
        );
        assert!(
            line.trim_end().ends_with("command -v opencrabs"),
            "a README one-liner must verify the command resolves, or a broken \
             install looks finished: {line}"
        );
    }

    assert_eq!(
        checked, 4,
        "expected the four platform one-liners in README.md, found {checked}: \
         the check above silently covers nothing if the block is renamed"
    );
}

// #2203 follow-up: the README shipped binaries for macOS and Linux with no
// Windows command at all, even though the release publishes a Windows zip.
// The Windows path is served by opencrabs.com, so the test pins the exact
// command a reader copies and the PATH outcome it promises, not the script.

#[test]
fn the_readme_documents_a_windows_one_liner_that_updates_path() {
    let readme = fs::read_to_string("README.md").expect("README.md readable");

    assert!(
        readme.contains("iwr -useb https://opencrabs.com/install.ps1 | iex"),
        "README.md has no Windows install command, so a Windows reader has \
         nothing to copy even though the release ships a Windows archive"
    );
    assert!(
        readme.contains("Get-Command opencrabs"),
        "the Windows section must tell the reader how to confirm the command \
         resolves, or a PATH-less install looks finished"
    );
    assert!(
        readme.contains("%LOCALAPPDATA%\\OpenCrabs\\bin"),
        "the Windows section must name the install directory, which is what \
         the reader has to add to PATH if they pass -NoPathUpdate"
    );
    assert!(
        readme.contains("-NoPathUpdate"),
        "the Windows section must document the escape hatch for readers who \
         manage PATH themselves"
    );
}

#[test]
fn the_readme_documents_the_no_path_flag() {
    let readme = fs::read_to_string("README.md").expect("README.md readable");

    assert!(
        readme.contains(
            "| `--no-path` | Install without touching your shell startup file, \
             for example when you manage PATH yourself |"
        ),
        "install.sh accepts --no-path but the README flag table does not list \
         it, so the only way to find it is to read the script"
    );
}

#[test]
fn the_readme_linux_one_liners_install_rtk() {
    let readme = fs::read_to_string("README.md").expect("README.md readable");
    let mut checked = 0;

    for line in readme.lines() {
        if !line.starts_with("curl -sL \"$(curl -s https://api.github.com") {
            continue;
        }
        if !line.contains("linux-") {
            continue;
        }
        checked += 1;

        assert!(
            line.contains("mv opencrabs rtk /usr/local/bin/"),
            "a Linux one-liner moves only opencrabs, so the rtk binary the \
             archive ships is left behind in the working directory and the \
             feature has to download itself again at runtime: {line}"
        );
    }

    assert_eq!(
        checked, 2,
        "expected two Linux one-liners in README.md, found {checked}: the \
         check above silently covers nothing if the block is renamed"
    );
}
