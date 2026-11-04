//! Crash Recovery Dialog
//!
//! When the TUI crashes or fails to start (including stdout bleed), this module
//! shows a raw-terminal dialog that lets users select an older version to roll
//! back to. Detects the install method (source, cargo install, pre-built binary)
//! and uses the appropriate upgrade strategy.

use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::utils::install::{InstallMethod, binary_name, platform_suffix};

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/opencrabs/opencrabs/releases";

/// A single release entry from GitHub.
#[derive(Debug)]
struct ReleaseEntry {
    version: String,
    tag: String,
    download_url: Option<String>,
    published: String,
}

/// Fetch recent releases from GitHub that have a binary for this platform.
async fn fetch_available_versions() -> Result<Vec<ReleaseEntry>> {
    let suffix = platform_suffix();
    let is_windows = std::env::consts::OS == "windows";
    let ext = if is_windows { "zip" } else { "tar.gz" };

    let client = reqwest::Client::new();
    let releases: Vec<serde_json::Value> = client
        .get(GITHUB_RELEASES_API)
        .query(&[("per_page", "15")])
        .header("User-Agent", format!("opencrabs/{}", crate::VERSION))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;

    let mut entries = Vec::new();

    for release in &releases {
        let tag = match release["tag_name"].as_str() {
            Some(t) => t.to_string(),
            None => continue,
        };
        let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
        let published = release["published_at"]
            .as_str()
            .unwrap_or("unknown")
            .chars()
            .take(10) // YYYY-MM-DD
            .collect::<String>();

        // Find matching binary asset (only for pre-built binary installs)
        let download_url = if let Some(suffix) = suffix {
            let assets = release["assets"].as_array();
            let versioned_name = format!("opencrabs-{}-{}.{}", tag, suffix, ext);
            let legacy_name = format!("opencrabs-{}.{}", suffix, ext);

            assets.and_then(|arr| {
                arr.iter().find_map(|a| {
                    let name = a["name"].as_str()?;
                    if name == versioned_name || name == legacy_name {
                        a["browser_download_url"].as_str().map(String::from)
                    } else {
                        None
                    }
                })
            })
        } else {
            None
        };

        entries.push(ReleaseEntry {
            version,
            tag,
            download_url,
            published,
        });
    }

    Ok(entries)
}

/// Download a pre-built release binary and swap it into the current executable path.
async fn download_and_install_binary(url: &str, version: &str) -> Result<()> {
    let orange = "\x1b[38;2;215;100;20m";
    let reset = "\x1b[0m";

    print!("  {}Downloading v{}...{}", orange, version, reset);
    io::stdout().flush()?;

    let client = reqwest::Client::new();
    let archive_bytes = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    println!(" done ({:.1} MB)", archive_bytes.len() as f64 / 1_048_576.0);

    let is_windows = std::env::consts::OS == "windows";
    let bin_name = binary_name();

    let binary_data = if is_windows {
        extract_from_zip(&archive_bytes, bin_name)?
    } else {
        extract_from_tar_gz(&archive_bytes, bin_name)?
    };

    swap_binary(&binary_data, version).await
}

/// Install a specific version via `cargo install opencrabs@version`.
async fn cargo_install_version(version: &str) -> Result<()> {
    let orange = "\x1b[38;2;215;100;20m";
    let reset = "\x1b[0m";

    println!(
        "  {}Installing v{} via cargo install...{}",
        orange, version, reset
    );

    let status = tokio::process::Command::new("cargo")
        .args(["install", "opencrabs", "--version", version, "--force"])
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("cargo install failed with exit code {}", status);
    }

    println!(
        "  {}Successfully installed v{} via cargo{}",
        orange, version, reset
    );
    Ok(())
}

/// Install a specific version by checking out a git tag and building from source.
async fn source_install_version(project_root: &Path, tag: &str, version: &str) -> Result<()> {
    let orange = "\x1b[38;2;215;100;20m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    println!("  {}Building v{} from source...{}", orange, version, reset);

    // Fetch tags and checkout
    let fetch = tokio::process::Command::new("git")
        .args(["fetch", "--tags"])
        .current_dir(project_root)
        .output()
        .await?;

    if !fetch.status.success() {
        anyhow::bail!("git fetch --tags failed");
    }

    let checkout = tokio::process::Command::new("git")
        .args(["checkout", tag])
        .current_dir(project_root)
        .output()
        .await?;

    if !checkout.status.success() {
        anyhow::bail!(
            "git checkout {} failed: {}",
            tag,
            String::from_utf8_lossy(&checkout.stderr)
        );
    }

    println!(
        "  {}Building (this may take a few minutes)...{}",
        dim, reset
    );

    let build = tokio::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(project_root)
        .status()
        .await?;

    if !build.success() {
        // Try to go back to previous state
        let _ = tokio::process::Command::new("git")
            .args(["checkout", "-"])
            .current_dir(project_root)
            .output()
            .await;
        anyhow::bail!("cargo build --release failed");
    }

    println!(
        "  {}Successfully built v{} from source{}",
        orange, version, reset
    );
    Ok(())
}

/// Write binary data to a temp file, verify, and atomically swap into the exe path.
async fn swap_binary(binary_data: &[u8], version: &str) -> Result<()> {
    let orange = "\x1b[38;2;215;100;20m";
    let reset = "\x1b[0m";

    let exe_path = std::env::current_exe()?;
    let tmp_path = exe_path.with_extension("rollback_tmp");
    std::fs::write(&tmp_path, binary_data)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Backup current binary
    let backup_path = exe_path.with_extension("rollback_backup");
    if let Err(e) = std::fs::copy(&exe_path, &backup_path) {
        tracing::warn!("Could not create backup: {}", e);
    }

    // Atomic rename
    if let Err(e) = std::fs::rename(&tmp_path, &exe_path) {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!("Failed to replace binary: {}", e);
    }

    // Post-swap verification
    if let Err(reason) = verify_binary(&exe_path).await {
        if backup_path.exists() {
            std::fs::rename(&backup_path, &exe_path)?;
            anyhow::bail!("New binary failed verification ({}). Rolled back.", reason);
        }
        anyhow::bail!("New binary failed verification: {}", reason);
    }

    let _ = std::fs::remove_file(&backup_path);

    println!("  {}Successfully installed v{}{}", orange, version, reset);
    Ok(())
}

/// Quick verification: run the binary with --version and check it exits cleanly.
async fn verify_binary(path: &Path) -> std::result::Result<(), String> {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(path)
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(format!("exited with status {}", output.status)),
        Ok(Err(e)) => Err(format!("failed to spawn: {}", e)),
        Err(_) => Err("timed out after 10s".into()),
    }
}

/// Extract a named file from a .tar.gz archive in memory.
fn extract_from_tar_gz(data: &[u8], file_name: &str) -> Result<Vec<u8>> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.file_name().and_then(|n| n.to_str()) == Some(file_name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    anyhow::bail!("'{}' not found in archive", file_name)
}

/// Extract a named file from a .zip archive in memory.
fn extract_from_zip(data: &[u8], file_name: &str) -> Result<Vec<u8>> {
    use std::io::Read;

    let reader = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name()?.into_owned();
        if name.ends_with(file_name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    anyhow::bail!("'{}' not found in zip", file_name)
}

/// Show the crash recovery dialog on the raw terminal.
///
/// Called when `tui::run()` fails or the TUI panics. Detects the install
/// method and presents appropriate rollback options.
pub async fn show_crash_recovery(error_msg: &str) -> Result<CrashRecoveryAction> {
    let orange = "\x1b[38;2;215;100;20m";
    let red = "\x1b[38;2;220;50;50m";
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    let install_method = InstallMethod::detect();

    println!();
    println!("{}{}  OpenCrabs crashed during startup{}", red, bold, reset);
    println!();
    println!("  {}{}{}", dim, error_msg, reset);
    println!(
        "  {}Install method: {}{}",
        dim,
        install_method.description(),
        reset
    );
    println!();

    // Fetch available versions from GitHub
    print!("  {}Checking available versions...{}", dim, reset);
    io::stdout().flush()?;

    let versions = match fetch_available_versions().await {
        Ok(v) if !v.is_empty() => {
            println!(" found {} release(s)", v.len());
            v
        }
        Ok(_) => {
            println!(" no releases found");
            println!("\n  No releases available for rollback.");
            return Ok(CrashRecoveryAction::Quit);
        }
        Err(e) => {
            println!(" failed");
            println!("\n  {}Could not reach GitHub: {}{}", dim, e, reset);
            println!("  Check your internet connection and try again.");
            return Ok(CrashRecoveryAction::Quit);
        }
    };

    let current = crate::VERSION;

    println!();
    println!(
        "  {}{}Select a version to install:{}\n",
        orange, bold, reset
    );

    // Show versions — availability depends on install method
    let mut selectable: Vec<(usize, &ReleaseEntry)> = Vec::new();
    for (i, entry) in versions.iter().enumerate() {
        let is_current = entry.version == current;
        let num = i + 1;

        if is_current {
            println!(
                "  {}  {}. v{} ({}) — current version{}",
                dim, num, entry.version, entry.published, reset
            );
            continue;
        }

        let available = match &install_method {
            // Pre-built binary: needs a downloadable asset for this platform
            InstallMethod::PrebuiltBinary => entry.download_url.is_some(),
            // cargo install and source: any tagged release can be installed
            InstallMethod::CargoInstall | InstallMethod::Source(_) => true,
            // Homebrew serves whatever its formula currently points at, so a
            // specific older tag is not selectable here. The single Homebrew
            // entry is offered below instead of a per-version list (#963).
            InstallMethod::Homebrew => false,
        };

        if available {
            println!(
                "  {}{}. v{} ({}){}",
                orange, num, entry.version, entry.published, reset
            );
            selectable.push((num, entry));
        } else {
            println!(
                "  {}  {}. v{} ({}) — no binary for this platform{}",
                dim, num, entry.version, entry.published, reset
            );
        }
    }

    if selectable.is_empty() {
        println!("\n  No compatible versions available for rollback.");
        return Ok(CrashRecoveryAction::Quit);
    }

    println!();
    println!("  {}q. Quit without changes{}", dim, reset);
    println!("  {}r. Retry starting OpenCrabs{}", dim, reset);
    println!();

    // Read user choice
    loop {
        print!("  {}Enter choice: {}", orange, reset);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().lock().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input == "q" || input == "quit" || input.is_empty() {
            return Ok(CrashRecoveryAction::Quit);
        }

        if input == "r" || input == "retry" {
            return Ok(CrashRecoveryAction::Retry);
        }

        if let Ok(num) = input.parse::<usize>()
            && let Some((_, entry)) = selectable.iter().find(|(n, _)| *n == num)
        {
            println!();
            let result = match &install_method {
                InstallMethod::PrebuiltBinary => {
                    if let Some(ref url) = entry.download_url {
                        download_and_install_binary(url, &entry.version).await
                    } else {
                        Err(anyhow::anyhow!("No binary available"))
                    }
                }
                InstallMethod::CargoInstall => cargo_install_version(&entry.version).await,
                InstallMethod::Source(root) => {
                    source_install_version(root, &entry.tag, &entry.version).await
                }
                // Unreachable: a Homebrew install marks every entry
                // unselectable above, so nothing lands in `selectable`. Kept
                // explicit rather than a catch-all so a future variant is a
                // compile error here instead of silently downloading a binary
                // over a package manager's file (#963).
                InstallMethod::Homebrew => Err(anyhow::anyhow!(
                    "This build is managed by Homebrew. Reinstall with \
                     `brew reinstall opencrabs`, or `brew upgrade opencrabs` for the latest."
                )),
            };

            match result {
                Ok(()) => {
                    println!();
                    println!(
                        "  {}Restart OpenCrabs to use v{}.{}",
                        orange, entry.version, reset
                    );
                    return Ok(CrashRecoveryAction::Installed(entry.version.clone()));
                }
                Err(e) => {
                    println!("\n  {}Installation failed: {}{}", red, e, reset);
                    println!("  Try a different version or quit.\n");
                    continue;
                }
            }
        }

        println!("  {}Invalid choice. Try again.{}", red, reset);
    }
}

/// What the user chose in the crash recovery dialog.
pub enum CrashRecoveryAction {
    /// User quit without changes.
    Quit,
    /// User wants to retry launching the TUI.
    Retry,
    /// A version was successfully installed.
    Installed(String),
}
