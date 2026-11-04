// Witness for #2260: the Windows one-liner installed two files out of an
// archive that ships six.
//
// The served script copied `opencrabs.exe` and `rtk.exe` into the install
// directory, then deleted the temp tree it had extracted into. The four MSVC
// runtime DLLs beside the exes were never copied, so the installed binary died
// on `VCRUNTIME140_1.dll was not found` on any machine without the
// redistributable installed, which is every machine that never ran a Visual
// Studio installer. The archive itself was fine (#2257 had just made it
// self-contained); the installer threw half of it away.
//
// The checks are structural because the script talks to GitHub, which no test
// should depend on. The one check that needs a parser runs only where `pwsh`
// exists, and CI's windows-latest job is where it actually fires.
//
// Witness file: src/tests/install_ps1_test.rs, registered in src/tests/mod.rs.

use std::fs;

const SCRIPT: &str = "src/scripts/install.ps1";

fn script() -> String {
    fs::read_to_string(SCRIPT).expect("install.ps1 readable")
}

/// Code lines only, so the comments explaining the rules cannot trip them.
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

#[test]
fn the_installer_parses_as_powershell() {
    // pwsh is not on a stock macOS box. When it is present, the file is parsed
    // for real instead of trusting a balance check.
    let Ok(probe) = std::process::Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
        .output()
    else {
        eprintln!("pwsh is not installed here; the windows runner parses this file");
        return;
    };
    assert!(
        probe.status.success(),
        "pwsh is installed but exited nonzero: {}",
        String::from_utf8_lossy(&probe.stderr)
    );

    let path = fs::canonicalize(SCRIPT).expect("canonical path");
    let program = format!(
        "$e=$null;$t=$null;\
         [void][System.Management.Automation.Language.Parser]::ParseFile('{}',[ref]$t,[ref]$e);\
         if ($e.Count -gt 0) {{ $e | ForEach-Object {{ $_.Message }}; exit 1 }}",
        path.display()
    );
    let out = std::process::Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", &program])
        .output()
        .expect("pwsh runs");
    assert!(
        out.status.success(),
        "install.ps1 does not parse: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_installer_never_waits_for_input() {
    // `iwr | iex` has no terminal to answer a prompt, so a prompt is a hang.
    for line in code_lines() {
        let t = line.trim();
        assert!(
            !t.starts_with("Read-Host"),
            "install.ps1 prompts for input, which hangs an unattended run: {line}"
        );
    }
}

#[test]
fn the_installer_verifies_the_checksum_before_unpacking() {
    let src = script();
    let verify = src
        .find("SHA256SUMS")
        .expect("install.ps1 must fetch SHA256SUMS");
    let extract = src
        .find("Expand-Archive")
        .expect("install.ps1 must extract the archive");
    assert!(
        verify < extract,
        "the checksum must be checked before the archive is unpacked, otherwise \
         a corrupt download is already on disk when it fails"
    );
    assert!(
        src.contains("checksum mismatch"),
        "a mismatch must be a hard failure with a named reason"
    );
}

#[test]
fn the_installer_copies_every_exe_and_dll_the_archive_ships() {
    // The regression: two explicit Copy-Item calls for two named files. The
    // archive carries the runtime DLLs beside the exes, and the install is only
    // complete when all of them land.
    let src = script();
    assert!(
        src.contains("Get-ChildItem -LiteralPath $extractDir"),
        "the file list must come from the extracted archive, not from names \
         written into this script"
    );
    assert!(
        src.contains("Extension -ieq '.exe'") && src.contains("Extension -ieq '.dll'"),
        "the installer must take the exes and the runtime DLLs out of the archive"
    );
    assert!(
        src.contains("Copy-Item -LiteralPath $item.FullName"),
        "every file in that list must be copied, not a hand-picked subset"
    );

    // A hardcoded runtime list would silently drop the DLL a future release
    // adds, and would fail outright on an older archive that ships none.
    let lower = src.to_lowercase();
    for dll in ["msvcp140.dll", "vcruntime140.dll", "vcomp140.dll"] {
        assert!(
            !lower.contains(dll),
            "install.ps1 names {dll} literally: the runtime set must come from \
             the archive, or the next layout change breaks the install again"
        );
    }
}

#[test]
fn the_installer_fails_when_a_shipped_file_did_not_land() {
    // Copying is not proof of arrival. The archive's own file list is the
    // contract, so each shipped name is checked on disk afterwards.
    let src = script();
    let compare = src
        .find("$missing = @($shipped")
        .expect("install.ps1 must compare what shipped against what landed");
    let fail = src
        .find("but the install does not have it")
        .expect("a short install must fail with a named reason");
    assert!(
        compare < fail,
        "the comparison must exist before the failure that reports it"
    );
    assert!(
        src.contains("Test-Path -LiteralPath (Join-Path $Dir $_.Name)"),
        "the comparison must test each shipped name in the install directory"
    );
    // The failure must run before the temp tree is deleted, or the evidence is
    // gone and the installer has to download the archive again to explain.
    let cleanup = src
        .find("Remove-Item -LiteralPath $workDir")
        .expect("install.ps1 cleans up its temp tree");
    assert!(
        fail < cleanup,
        "the short-install failure must fire inside the try block, before cleanup"
    );
}

#[test]
fn the_copy_happens_before_the_temp_tree_is_deleted() {
    // This is the shape of the original bug: the files were only ever reachable
    // inside $workDir, and the cleanup removed the whole tree.
    let src = script();
    let copy = src
        .find("Copy-Item -LiteralPath $item.FullName")
        .expect("install.ps1 copies out of the extract dir");
    let cleanup = src
        .find("Remove-Item -LiteralPath $workDir")
        .expect("install.ps1 cleans up its temp tree");
    assert!(
        copy < cleanup,
        "the copy must run before the cleanup, or the files are deleted with the tree"
    );
}

#[test]
fn the_unblock_covers_every_installed_file() {
    // Invoke-WebRequest sets the mark-of-the-web and Expand-Archive can carry it
    // onto the extracted files. A blocked runtime DLL fails the same way a
    // blocked exe does, so the unblock has to walk the installed list.
    let src = script();
    assert!(
        src.contains("foreach ($name in $installed)"),
        "the unblock must iterate every installed file, not two named exes"
    );
    assert!(
        src.contains("Unblock-File -LiteralPath (Join-Path $Dir $name)"),
        "each installed file must be unblocked at its installed path"
    );
}

#[test]
fn the_installer_still_installs_an_archive_with_no_rtk() {
    // Releases before #2255 ship no rtk. Warning instead of dying keeps the
    // pinned-version path working, which the Windows CI job exercises with
    // -Version 0.5.11.
    let src = script();
    assert!(
        src.contains("rebase tooling will be unavailable"),
        "a missing rtk must warn, not fail: older archives do not ship it"
    );
}

#[test]
fn the_installer_offers_a_dry_run_and_the_documented_flags() {
    let src = script();
    for flag in ["$Version", "$Dir", "$DryRun", "$NoPathUpdate", "$Help"] {
        assert!(
            src.contains(flag),
            "install.ps1 must accept {flag}: a caller that cannot inspect what \
             will happen before it happens cannot verify anything"
        );
    }
    let dry = src
        .find("dry run: nothing downloaded")
        .expect("dry run branch");
    let download = src.find("Downloading...").expect("download step");
    assert!(
        dry < download,
        "the dry run must exit before the download, not after it"
    );
}

#[test]
fn the_readme_stops_claiming_the_installer_drops_the_dlls() {
    // The caveat added with #2257 told users to unpack the zip by hand because
    // the one-liner lost the runtime. Once the installer installs the archive,
    // that instruction is a bug of its own.
    let readme = fs::read_to_string("README.md").expect("README.md readable");
    assert!(
        !readme.contains("copies only the two exes out of the archive today"),
        "the README still tells users the Windows one-liner drops the runtime DLLs"
    );
}
