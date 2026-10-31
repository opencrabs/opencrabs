//! macOS Dock icon for the TUI (#2280).
//!
//! A TUI launched as a bare Mach-O has no `CFBundleIconFile` and no Dock
//! presence at all, so the crab only showed up when the binary happened to run
//! from `OpenCrabs.app`. The bundle fixes Finder, Launchpad and Activity
//! Monitor, but a terminal process never gets a Dock tile on its own.
//!
//! This module gives the running process one: it asks `NSApplication` for a
//! Regular activation policy (the flag that creates the Dock tile) and hands
//! `NSApp` the embedded `icon.icns`. No new crates: `libobjc` and `AppKit` are
//! already linked on macOS, and the two calls go through `objc_msgSend`.
//!
//! Three rules keep it from surprising anyone:
//! - `OPENCRABS_NO_DOCK_ICON=1` opts out entirely (headless runs, CI, tmux-only
//!   workflows that do not want a Dock tile).
//! - `OPENCRABS_DOCK_ICON=/path/to/icon.icns` overrides the embedded asset.
//! - Install is a no-op when the caller is not on the main thread, and when the
//!   process has no WindowServer session at all (ssh, cron, a container). AppKit
//!   is main-thread and GUI-session only, and touching it in either wrong
//!   context can abort the process, so both cases log and do nothing.
//!
//! Every non-macOS target compiles the helpers (they are pure) and skips the
//! FFI, so the tests below run on the Linux CI runners too.

use std::path::Path;

/// Icon bytes shipped in the binary, the same asset the `.app` bundle uses.
#[cfg(target_os = "macos")]
const EMBEDDED_ICNS: &[u8] = include_bytes!("../assets/icon.icns");

/// Set to a non-empty value other than `0` to skip the Dock icon.
pub const ENV_OPT_OUT: &str = "OPENCRABS_NO_DOCK_ICON";

/// Optional path to a replacement `.icns` file.
pub const ENV_ICON_PATH: &str = "OPENCRABS_DOCK_ICON";

/// Upper bound on an override file. A Dock icon has no business being larger
/// than this, and a runaway path should not cost the process its memory.
const MAX_ICON_BYTES: u64 = 16 * 1024 * 1024;

/// An `.icns` file starts with the ASCII magic `icns` and a big-endian total
/// length that must cover the whole buffer. Checking it here means a truncated
/// or wrong-format override is rejected with a log line instead of handing
/// AppKit garbage.
pub(crate) fn icns_is_valid(bytes: &[u8]) -> bool {
    if bytes.len() < 8 || &bytes[0..4] != b"icns" {
        return false;
    }
    let declared = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    declared >= 8 && declared <= bytes.len()
}

/// Whether the Dock icon should be installed at all. Separated from the FFI so
/// the opt-out is testable on every platform.
pub(crate) fn should_install(opt_out: Option<&str>) -> bool {
    match opt_out {
        None => true,
        Some(v) => {
            let v = v.trim();
            v.is_empty() || v == "0"
        }
    }
}

/// Resolve the icon bytes: the override file when it exists and is a valid
/// icns, else the embedded asset. A bad override is reported and ignored
/// rather than silently producing a blank tile.
#[cfg(target_os = "macos")]
fn icon_bytes(override_path: Option<&str>) -> Vec<u8> {
    if let Some(raw) = override_path {
        let path = Path::new(raw);
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_ICON_BYTES => {
                tracing::warn!(
                    "dock icon: ignoring {ENV_ICON_PATH} ({raw}): {} bytes exceeds the {} byte cap",
                    meta.len(),
                    MAX_ICON_BYTES
                );
            }
            Ok(_) => match std::fs::read(path) {
                Ok(bytes) if icns_is_valid(&bytes) => return bytes,
                Ok(_) => tracing::warn!(
                    "dock icon: ignoring {ENV_ICON_PATH} ({raw}): not a valid .icns file"
                ),
                Err(e) => {
                    tracing::warn!("dock icon: ignoring {ENV_ICON_PATH} ({raw}): {e}");
                }
            },
            Err(e) => {
                tracing::warn!("dock icon: ignoring {ENV_ICON_PATH} ({raw}): {e}");
            }
        }
    }
    EMBEDDED_ICNS.to_vec()
}

/// Install the Dock icon for this process. Safe to call more than once and
/// safe to call on any platform; it returns without doing anything when it
/// cannot, logging why.
pub fn install() {
    if !should_install(std::env::var(ENV_OPT_OUT).ok().as_deref()) {
        tracing::debug!("dock icon: skipped, {ENV_OPT_OUT} is set");
        return;
    }
    platform::install_icon();
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ENV_ICON_PATH, icns_is_valid, icon_bytes};
    use std::ffi::c_void;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const u8) -> *mut c_void;
        fn sel_registerName(name: *const u8) -> *mut c_void;
        fn objc_msgSend();
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        /// Non-null only when this process belongs to a GUI login session.
        /// NULL over ssh, from cron, and inside a container, which is exactly
        /// where an AppKit call would abort instead of returning an error.
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// `NSApplicationActivationPolicyRegular`: the policy that creates a Dock
    /// tile. Accessory (1) and Prohibited (2) do not.
    const ACTIVATION_POLICY_REGULAR: i64 = 0;

    fn class(name: &str) -> *mut c_void {
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: `bytes` is a NUL-terminated C string for the duration of the
        // call, and the name comes from a `const` in this module.
        unsafe { objc_getClass(bytes.as_ptr()) }
    }

    fn selector(name: &str) -> *mut c_void {
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: same contract as `class`; `sel_registerName` copies the name.
        unsafe { sel_registerName(bytes.as_ptr()) }
    }

    /// `[receiver someNoArgSelector]` returning an object pointer.
    ///
    /// SAFETY: the caller guarantees the selector exists on the receiver and
    /// that it takes no arguments and returns an `id`.
    unsafe fn send(receiver: *mut c_void, sel: &str) -> *mut c_void {
        if receiver.is_null() {
            return std::ptr::null_mut();
        }
        let sel = selector(sel);
        // SAFETY: `objc_msgSend` is declared as a generic symbol because its
        // real signature varies per selector; this transmute is the shape the
        // call site promises (`(id, SEL) -> id`), which is how Objective-C
        // dispatch works for a no-argument object-returning selector.
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { f(receiver, sel) }
    }

    fn on_main_thread() -> bool {
        // SAFETY: `pthread_main_np` is a plain query with no preconditions.
        unsafe { libc::pthread_main_np() == 1 }
    }

    fn has_window_server() -> bool {
        // SAFETY: `CGSessionCopyCurrentDictionary` takes no arguments and
        // returns a Copy-rule CoreFoundation object we release immediately;
        // the null case is the "no GUI session" answer, not an error.
        unsafe {
            let dict = CGSessionCopyCurrentDictionary();
            if dict.is_null() {
                return false;
            }
            CFRelease(dict);
            true
        }
    }

    pub(super) fn install_icon() {
        if !on_main_thread() {
            tracing::debug!(
                "dock icon: skipped, not on the main thread (AppKit is main-thread only)"
            );
            return;
        }
        if !has_window_server() {
            tracing::debug!("dock icon: skipped, no WindowServer session (headless run)");
            return;
        }

        let bytes = icon_bytes(std::env::var(ENV_ICON_PATH).ok().as_deref());
        if !icns_is_valid(&bytes) {
            tracing::warn!("dock icon: embedded icon.icns is not a valid .icns file");
            return;
        }

        // SAFETY: every call below runs on the main thread with non-null
        // receivers checked before use, and each selector's signature matches
        // the `objc_msgSend` shape it is transmuted to:
        // - `+sharedApplication` -> id
        // - `-setActivationPolicy:` -> BOOL (NSInteger arg)
        // - `+dataWithBytes:length:` -> id
        // - `-initWithData:` -> id
        // - `-setApplicationIconImage:` -> void
        unsafe {
            let app = send(class("NSApplication"), "sharedApplication");
            if app.is_null() {
                tracing::warn!("dock icon: NSApplication was unavailable");
                return;
            }

            let set_policy: unsafe extern "C" fn(*mut c_void, *mut c_void, i64) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let ok = set_policy(
                app,
                selector("setActivationPolicy:"),
                ACTIVATION_POLICY_REGULAR,
            );
            if !ok {
                tracing::warn!("dock icon: NSApplication refused the Regular activation policy");
                return;
            }

            let make_data: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const u8,
                usize,
            ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
            let data = make_data(
                class("NSData"),
                selector("dataWithBytes:length:"),
                bytes.as_ptr(),
                bytes.len(),
            );
            if data.is_null() {
                tracing::warn!("dock icon: NSData refused the icon bytes");
                return;
            }

            let alloc = send(class("NSImage"), "alloc");
            let init_with_data: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
            let image = init_with_data(alloc, selector("initWithData:"), data);
            if image.is_null() {
                tracing::warn!("dock icon: NSImage could not decode the icon");
                return;
            }

            let set_icon: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(objc_msgSend as *const ());
            set_icon(app, selector("setApplicationIconImage:"), image);

            tracing::debug!("dock icon: installed ({} bytes)", bytes.len());
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn install_icon() {
        // Every other platform draws the icon from the terminal or the
        // launcher, never from the process.
    }
}
