//! The app icon: the crab logo in the window banner and, on macOS, the real
//! `icon.icns` in the Dock. A bare binary (`cargo run`, no .app bundle) gets
//! the system's generic executable icon in the Dock, which is the black
//! "exec" block the owner rejected. The bundle-free fix is runtime: hand the
//! running NSApplication the shipped icon file. Shipping a real .app bundle
//! stays a later distribution concern.

use gpui::{Image, ImageFormat, IntoElement, ObjectFit, Styled, StyledImage, img, px, rems};

/// The canonical brand mark, embedded at compile time so the binary has no
/// runtime dependency on the checkout it was built from. Hard rule: the
/// meme/brand image must be the real OpenCrabs logo, never a generated crab.
pub const LOGO_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/opencrabs-logo-no-bg.png"
));

/// The shipped macOS app icon (multi-resolution .icns). This is the file the
/// owner named: the Dock must show it, not a single PNG scaled up to Dock
/// size, which is what looked distorted.
#[cfg(target_os = "macos")]
pub const APP_ICON_ICNS: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/icon.icns"));

/// The crab logo for the window banner: contained so the aspect ratio is
/// never stretched, with the slight corner radius the owner asked for.
pub fn banner_logo() -> impl IntoElement {
    img(std::sync::Arc::new(Image::from_bytes(
        ImageFormat::Png,
        LOGO_PNG.to_vec(),
    )))
    .w(rems(1.75))
    .h(rems(1.75))
    .rounded(px(5.0))
    .object_fit(ObjectFit::Contain)
}

/// Set the Dock icon from the embedded `.icns`. macOS only: the Dock is an
/// AppKit concept, and the ugly "exec" block is macOS's generic-executable
/// icon. Runs on the main thread, right after `Application::run` starts.
#[cfg(target_os = "macos")]
pub fn apply_dock_icon() {
    use objc2::AnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    // Sound: this is called from the `Application::run` closure, which gpui
    // runs on the main thread only.
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let data = NSData::with_bytes(APP_ICON_ICNS);
    // SAFETY: AppKit calls from the main thread with a valid, embedded icns.
    unsafe {
        if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
            app.setApplicationIconImage(Some(&icon));
        }
    }
}

/// Non-macOS: no Dock, nothing to set. The banner logo still shows.
#[cfg(not(target_os = "macos"))]
pub fn apply_dock_icon() {}
