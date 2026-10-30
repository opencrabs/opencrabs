//! What Ctrl+V / Cmd+V should read from the OS clipboard (#1811, #2174).
//!
//! A screenshot copied to the clipboard carries image classes and no text
//! class at all, so a text-only read comes back empty. Text still goes first:
//! a Finder file copy holds both the filename and an icon image, and the
//! filename is what the user meant to paste.
//!
//! The probe used to be macOS-only, which meant Linux and Windows users never
//! saw the "an image is on your clipboard" hint even though image paste itself
//! already worked there (#2174). Each platform now answers the same question
//! in its own cheap way: `clipboard info` on macOS, a MIME-type list on Linux,
//! and one .NET clipboard query on Windows.

/// What the clipboard currently holds, as far as the hint cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipboardKinds {
    pub image: bool,
    pub text: bool,
}

impl ClipboardKinds {
    /// True for the exact shape of a screenshot: image bytes and no text at
    /// all, so pasting text would come back empty (#1816).
    pub(crate) fn is_image_only(self) -> bool {
        self.image && !self.text
    }
}

/// Stable fingerprint of a clipboard probe, used to tell "same image still
/// sitting there" from "a new screenshot". Collapses whitespace so class/size
/// pairs compare equal across repeated probes.
pub(crate) fn clipboard_info_fingerprint(info: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    info.split_whitespace()
        .collect::<Vec<_>>()
        .hash(&mut hasher);
    hasher.finish()
}

/// AppleScript `clipboard info` class fragments that mean image bytes are on
/// the clipboard. "picture" covers the TIFF/JPEG/GIF picture classes; the
/// rest are raw `«class ...»` codes.
const IMAGE_CLASS_MARKERS: &[&str] = &[
    "PNGf", "picture", "TIFF", "JPEG", "GIF", "BMP", "jp2", "AVIF", "8BPS", "TPIC", "PICT",
];

/// Class fragments that carry text. Any of these means the user can paste
/// text right now, so an image hint would be noise: Finder and browser
/// copies carry a text flavor next to the image.
const TEXT_CLASS_MARKERS: &[&str] = &["string", "utf8", "ut16", "utxt", "Unicode text"];

/// True when any known image class appears in `clipboard info` output.
pub(crate) fn clipboard_info_has_image(info: &str) -> bool {
    IMAGE_CLASS_MARKERS.iter().any(|m| info.contains(m))
}

/// True when any known text class appears in `clipboard info` output.
pub(crate) fn clipboard_info_has_text(info: &str) -> bool {
    TEXT_CLASS_MARKERS.iter().any(|m| info.contains(m))
}

/// Classify a newline-separated MIME-type list (`wl-paste --list-types`,
/// `xclip -t TARGETS -o`). X11 spells plain text `UTF8_STRING` or `STRING`
/// instead of `text/plain`, so both spellings count.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
pub(crate) fn kinds_from_mime_list(list: &str) -> ClipboardKinds {
    let mut kinds = ClipboardKinds {
        image: false,
        text: false,
    };
    for line in list.lines() {
        let line = line.trim();
        if line.starts_with("image/") {
            kinds.image = true;
        } else if line.starts_with("text/") || line == "UTF8_STRING" || line == "STRING" {
            kinds.text = true;
        }
    }
    kinds
}

/// Classify the macOS `clipboard info` string.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn kinds_from_clipboard_info(info: &str) -> ClipboardKinds {
    ClipboardKinds {
        image: clipboard_info_has_image(info),
        text: clipboard_info_has_text(info),
    }
}

/// Probe the clipboard's contents, returning the kinds plus the raw text used
/// for change detection. `None` means the probe could not run at all, so the
/// hint stays silent rather than guessing. Read-only: it never touches the
/// clipboard.
pub(crate) fn read_clipboard_kinds() -> Option<(ClipboardKinds, String)> {
    #[cfg(target_os = "macos")]
    {
        let out = run_probe("osascript", &["-e", "clipboard info"])?;
        let kinds = kinds_from_clipboard_info(&out);
        Some((kinds, out))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Wayland first, matching the read path (`read_clipboard`).
        let out = run_probe("wl-paste", &["--list-types"])
            .or_else(|| run_probe("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"]))?;
        let kinds = kinds_from_mime_list(&out);
        Some((kinds, out))
    }
    #[cfg(windows)]
    {
        let out = run_probe(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $i = [System.Windows.Forms.Clipboard]::ContainsImage(); \
                 $t = [System.Windows.Forms.Clipboard]::ContainsText(); \
                 Write-Output \"image=$i text=$t\"",
            ],
        )?;
        let kinds = kinds_from_windows_probe(&out);
        Some((kinds, out))
    }
}

/// Parse `image=True text=False` as printed by the Windows probe.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn kinds_from_windows_probe(out: &str) -> ClipboardKinds {
    let flag = |key: &str| {
        out.split_whitespace()
            .any(|tok| tok.eq_ignore_ascii_case(&format!("{key}=True")))
    };
    ClipboardKinds {
        image: flag("image"),
        text: flag("text"),
    }
}

/// Run a probe command and return its stdout, or `None` when it is missing or
/// fails. A missing tool is normal (no Wayland, no X11), not an error worth
/// surfacing.
fn run_probe(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        // No console flash on a probe that runs on focus and tick.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        tracing::debug!(program, "clipboard probe failed");
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    (!text.trim().is_empty()).then_some(text)
}

/// Where a clipboard paste goes, decided from the clipboard's text alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardRoute<'a> {
    /// Usable text: a file path to attach or prose to insert.
    Text(&'a str),
    /// No usable text: try the clipboard's raw image bytes instead.
    Image,
}

/// Route on the clipboard text. `None` (every text backend failed) and
/// whitespace-only text both mean the clipboard holds no text to paste.
pub(crate) fn route_for_clipboard_text(text: Option<&str>) -> ClipboardRoute<'_> {
    match text {
        Some(t) if !t.trim().is_empty() => ClipboardRoute::Text(t),
        _ => ClipboardRoute::Image,
    }
}
