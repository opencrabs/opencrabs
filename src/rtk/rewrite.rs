//! RTK command rewriting functionality
//!
//! RTK works as a direct proxy: you run `rtk git status` instead of `git status`.
//! RTK intercepts the command, runs it internally, filters/compresses the output,
//! and returns the token-optimized version.
//!
//! This module maintains the list of supported commands and handles the rewriting.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::OnceCell;

/// RTK release pinned for auto-download. Keep in sync with the version bundled
/// by `.github/workflows/release.yml` so a source build auto-installs the same
/// RTK that prebuilt releases ship.
pub(crate) const RTK_VERSION: &str = "0.40.0";

/// User-facing guidance shown by `/rtk` only when the auto-download itself
/// failed (no network, GitHub unreachable, unsupported platform). The happy
/// path installs RTK silently on first use, so this is the rare failure note.
pub const RTK_NOT_INSTALLED_HELP: &str = "⚡ *Token Optimization (RTK)*\n\n\
    RTK is not installed and the automatic download did not complete (no \
    network, GitHub unreachable, or an unsupported platform). OpenCrabs retries \
    the download on first use, so check connectivity and try `/rtk` again after \
    a restart. Source: https://github.com/rtk-ai/rtk";

/// Commands that RTK supports as a proxy (from `rtk --help`).
/// When the first token of a bash command matches one of these, we prepend `rtk`.
///
/// Meta-commands (gain, config, init, session, telemetry, learn, discover) are
/// excluded because they're RTK's own management commands, not proxies.
const RTK_SUPPORTED_COMMANDS: &[&str] = &[
    "git",
    "gh",
    "glab",
    "aws",
    "psql",
    "pnpm",
    "npm",
    "npx",
    "cargo",
    "docker",
    "kubectl",
    "grep",
    "find",
    "ls",
    "tree",
    "diff",
    "curl",
    "wget",
    "jest",
    "vitest",
    "prisma",
    "tsc",
    "next",
    "dotnet",
    "playwright",
    "prettier",
    "eslint",
    // Sysadmin / system-inspection family — added 2026-06-01 after
    // an RTK-savings audit showed 0% compression on these because
    // they were bypassing RTK entirely. fast-rlm's stats show
    // `ps auxww` compressing to 97.9% via RTK's TOML filter, so the
    // agent's process / network / log inspections were leaking
    // verbose output to the model at full size. Each entry below is
    // a command whose default output is verbose (full process table,
    // socket list, log lines, DNS resolution chain) and that RTK
    // can either filter natively or via a TOML rule. RTK's
    // `is_rtk_supported` upstream gate ensures we only forward if
    // RTK actually knows how to handle the subcommand.
    "ps",
    "top",
    "lsof",
    "netstat",
    "ss",
    "journalctl",
    "dmesg",
    "dig",
    "nslookup",
    "host",
    "traceroute",
];

/// Commands that should NEVER be rewritten even if they match the list above.
/// These are either too interactive, have side effects we don't want RTK to
/// intercept, or are already RTK meta-commands.
const RTK_BLOCKLIST: &[&str] = &[
    "rtk",       // Don't double-prepend
    "sudo",      // Don't rewrite elevated commands
    "ssh",       // Interactive, needs TTY
    "scp",       // Binary transfer
    "sftp",      // Interactive
    "rsync",     // Binary transfer
    "vim",       // Editor
    "vi",        // Editor
    "nvim",      // Editor
    "nano",      // Editor
    "emacs",     // Editor
    "less",      // Pager
    "more",      // Pager
    "man",       // Pager
    "python",    // REPL
    "python3",   // REPL
    "node",      // REPL
    "mysql",     // REPL
    "redis-cli", // REPL
    "psql",      // REPL (even though rtk supports it, we handle it via tool)
];

/// Result of RTK command rewriting
#[derive(Debug, Clone)]
pub struct RtkResult {
    /// The rewritten command (with rtk prefix)
    pub rewritten_command: String,
    /// Whether the command was actually rewritten
    pub was_rewritten: bool,
    /// Original command for reference
    pub original_command: String,
}

/// Cached RTK binary path lookup.
///
/// Uses `tokio::sync::OnceCell` so the async initialization never blocks the
/// tokio runtime. The previous `std::sync::OnceLock` + `std::process::Command`
/// combination blocked the worker thread on the first call (issue #125).
static RTK_BINARY: OnceCell<Option<String>> = OnceCell::const_new();

/// The RTK binary filename for the current platform.
pub(crate) fn rtk_bin_filename() -> &'static str {
    if cfg!(windows) { "rtk.exe" } else { "rtk" }
}

/// Ensure a directory is in PATH so bare binary names resolve at exec time.
/// Called once during `find_rtk_binary` init (single writer via OnceCell).
fn ensure_dir_in_path(dir: &std::path::Path) {
    let dir_str = dir.to_string_lossy().to_string();
    if let Ok(path) = std::env::var("PATH")
        && !path.split(':').any(|p| p == dir_str)
    {
        // SAFETY: called once during OnceCell init; no concurrent writers.
        unsafe {
            std::env::set_var("PATH", format!("{}:{}", dir_str, path));
        }
    }
}

/// Find the RTK binary path (async, cached).
///
/// Checks in order:
/// 1. Bundled binary in the same directory as the OpenCrabs executable
/// 2. Bundled binary in `bin/` subdirectory relative to the executable
/// 3. System PATH via `which rtk` (spawned as a non-blocking tokio child)
/// 4. Auto-download the pinned RTK release for this platform (first-use install)
///
/// Result is cached after the first call. Concurrent callers await the same
/// initialization — no thundering herd, no blocking.
async fn find_rtk_binary() -> Option<String> {
    RTK_BINARY
        .get_or_init(|| async {
            let bin = rtk_bin_filename();

            // Check for bundled binary in the same directory as the executable
            if let Ok(exe_path) = std::env::current_exe()
                && let Some(exe_dir) = exe_path.parent()
            {
                // Check ./rtk (same directory)
                let bundled_path = exe_dir.join(bin);
                if bundled_path.exists() && bundled_path.is_file() {
                    tracing::info!("RTK binary found bundled at: {:?}", bundled_path);
                    ensure_dir_in_path(exe_dir);
                    return Some("rtk".to_string());
                }

                // Check ./bin/rtk (bin subdirectory)
                let bin_path = exe_dir.join("bin").join(bin);
                if bin_path.exists() && bin_path.is_file() {
                    tracing::info!("RTK binary found bundled at: {:?}", bin_path);
                    ensure_dir_in_path(&exe_dir.join("bin"));
                    return Some("rtk".to_string());
                }
            }

            // Fall back to PATH lookup — async so we never block the runtime.
            let which_bin = if cfg!(windows) { "where" } else { "which" };
            match tokio::process::Command::new(which_bin)
                .arg("rtk")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    tracing::info!("RTK binary found in PATH: {}", path);
                    // Store the bare name; tokio::process::Command resolves via PATH at exec time.
                    return Some("rtk".to_string());
                }
                Ok(_) => {
                    tracing::info!("RTK binary not found bundled or in PATH; auto-downloading");
                }
                Err(_) => {
                    tracing::info!("RTK PATH lookup failed; auto-downloading the bundled release");
                }
            }

            // Last resort: download the pinned RTK release for this platform and
            // install it next to the executable (or in ~/.local/bin). This is
            // the "auto-download on first use" path for source builds and any
            // install where RTK was never bundled.
            download_and_install_rtk().await
        })
        .await
        .clone()
}

/// GitHub release asset name for RTK on the current platform, mirroring the
/// mapping in `.github/workflows/release.yml`. `None` on an unsupported target.
pub(crate) fn rtk_asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("rtk-x86_64-unknown-linux-musl.tar.gz"),
        ("linux", "aarch64") => Some("rtk-aarch64-unknown-linux-gnu.tar.gz"),
        ("macos", "x86_64") => Some("rtk-x86_64-apple-darwin.tar.gz"),
        ("macos", "aarch64") => Some("rtk-aarch64-apple-darwin.tar.gz"),
        ("windows", "x86_64") => Some("rtk-x86_64-pc-windows-msvc.zip"),
        _ => None,
    }
}

/// Writable install locations for the RTK binary, in preference order: next to
/// the OpenCrabs executable first (so `find_rtk_binary` picks it up with zero
/// PATH dependency), then `~/.local/bin` as a PATH-friendly fallback.
fn rtk_install_targets() -> Vec<PathBuf> {
    let bin = rtk_bin_filename();
    let mut targets = Vec::new();
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        targets.push(exe_dir.join(bin));
    }
    if let Some(home) = std::env::var_os("HOME") {
        targets.push(PathBuf::from(home).join(".local").join("bin").join(bin));
    }
    targets
}

/// Download the pinned RTK release for this platform and install it. Returns
/// the installed path on success. Best-effort: any failure (network, archive,
/// permissions) logs and returns `None` so RTK simply stays disabled.
async fn download_and_install_rtk() -> Option<String> {
    let Some(asset) = rtk_asset_name() else {
        tracing::warn!(
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            "RTK auto-download: no release asset for this platform"
        );
        return None;
    };
    let url = format!("https://github.com/rtk-ai/rtk/releases/download/v{RTK_VERSION}/{asset}");
    tracing::info!(url = %url, "RTK auto-download: fetching release asset");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .ok()?;
    let resp = match client
        .get(&url)
        .header("User-Agent", "opencrabs")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!(status = %r.status(), url = %url, "RTK auto-download: non-2xx response");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, url = %url, "RTK auto-download: request failed");
            return None;
        }
    };
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "RTK auto-download: failed to read response body");
            return None;
        }
    };

    let bin = rtk_bin_filename();
    let extracted = if asset.ends_with(".zip") {
        extract_zip_member(&bytes, bin)
    } else {
        extract_tar_gz_member(&bytes, bin)
    };
    let Some(data) = extracted else {
        tracing::warn!(
            asset = asset,
            "RTK auto-download: binary not found inside archive"
        );
        return None;
    };

    for dest in rtk_install_targets() {
        if let Some(parent) = dest.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            tracing::debug!(dir = %parent.display(), error = %e, "RTK auto-download: mkdir failed, trying next target");
            continue;
        }
        if let Err(e) = tokio::fs::write(&dest, &data).await {
            tracing::debug!(path = %dest.display(), error = %e, "RTK auto-download: write failed, trying next target");
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            {
                tracing::warn!(path = %dest.display(), error = %e, "RTK auto-download: chmod failed");
            }
        }
        tracing::info!(path = %dest.display(), bytes = data.len(), "RTK auto-download: installed");
        if let Some(parent) = dest.parent() {
            ensure_dir_in_path(parent);
        }
        return Some("rtk".to_string());
    }

    tracing::warn!("RTK auto-download: no writable install location found");
    None
}

/// Extract a named member from an in-memory `.tar.gz` archive (matches on the
/// entry's file name, so a leading directory in the archive is fine).
fn extract_tar_gz_member(data: &[u8], file_name: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?.to_path_buf();
        if path.file_name().and_then(|n| n.to_str()) == Some(file_name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

/// Extract a named member from an in-memory `.zip` archive.
fn extract_zip_member(data: &[u8], file_name: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data)).ok()?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).ok()?;
        let name = match file.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                tracing::warn!("zip entry {i} has an unreadable name: {e}");
                continue;
            }
        };
        if name.ends_with(file_name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

/// Check if the rtk binary is available (bundled, in PATH, or auto-downloaded).
/// Async + cached.
pub async fn is_rtk_available() -> bool {
    find_rtk_binary().await.is_some()
}

/// Resolve RTK in the background at startup (triggering the first-use
/// auto-download if needed) so the first bash command never blocks on it.
pub fn warm_up() {
    tokio::spawn(async {
        if is_rtk_available().await {
            tracing::debug!("RTK warm-up complete: binary available");
        } else {
            tracing::info!(
                "RTK warm-up: binary unavailable after auto-download attempt; token savings disabled"
            );
        }
    });
}

/// Extract the first real command token from a shell command string.
///
/// Skips leading env var assignments (`FOO=bar cmd`) and returns the
/// actual command name.
pub(crate) fn first_command_token(command: &str) -> &str {
    for token in command.split_whitespace() {
        // Skip env var assignments like FOO=bar
        if token.contains('=') && !token.starts_with('-') && !token.starts_with('/') {
            continue;
        }
        return token;
    }
    ""
}

/// Check if a command token is supported by RTK for rewriting.
pub(crate) fn is_rtk_supported(token: &str) -> bool {
    // Strip leading path: /usr/bin/git → git
    let basename = token.rsplit('/').next().unwrap_or(token);

    if RTK_BLOCKLIST.contains(&basename) {
        return false;
    }

    RTK_SUPPORTED_COMMANDS.contains(&basename)
}

/// Rewrite a bash command to use RTK as a proxy.
///
/// If the command's first token is RTK-supported (git, cargo, npm, etc.),
/// prepends the RTK binary path to the command. Otherwise returns None.
///
/// The RTK binary path is determined by checking bundled locations first,
/// then falling back to PATH.
///
/// # Example
/// ```rust,ignore
/// use opencrabs::rtk::rewrite_command;
///
/// let result = rewrite_command("git status");
/// // Returns Some(RtkResult { rewritten_command: "/path/to/rtk git status", ... })
///
/// let result = rewrite_command("echo hello");
/// // Returns None (echo is not RTK-supported)
/// ```
pub async fn rewrite_command(command: &str) -> Option<RtkResult> {
    let rtk_binary = find_rtk_binary().await?;

    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Don't rewrite commands that already start with rtk
    if trimmed.starts_with("rtk ") || trimmed == "rtk" {
        return None;
    }

    // Handle chained commands: only rewrite if the FIRST command is supported.
    // For `git status && echo done`, we rewrite to `rtk git status && echo done`.
    // For `echo start && git status`, we don't rewrite (first cmd not supported).
    let first_token = first_command_token(trimmed);

    if !is_rtk_supported(first_token) {
        // Pass-through, not a refusal: the caller runs `command` unmodified.
        // Worded so a model reading its own logs does not report that rtk
        // rejected the command (#1353).
        tracing::debug!(
            "RTK: no rewrite for '{}' (token '{}'), running unmodified",
            command,
            first_token
        );
        return None;
    }

    let rewritten = format!("{} {}", rtk_binary, trimmed);

    tracing::debug!("RTK rewrote: '{}' -> '{}'", command, rewritten);

    Some(RtkResult {
        rewritten_command: rewritten,
        was_rewritten: true,
        original_command: command.to_string(),
    })
}
