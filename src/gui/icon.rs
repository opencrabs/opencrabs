//! The app icon: the crab logo in the window banner and, on macOS, the real
//! `icon.icns` in the Dock. A bare binary (`cargo run`, no .app bundle) gets
//! the system's generic executable icon in the Dock, which is the black
//! "exec" block the owner rejected. The bundle-free fix is runtime: hand the
//! running NSApplication the shipped icon file. Shipping a real .app bundle
//! stays a later distribution concern.

use gpui::{Image, ImageFormat, IntoElement, ObjectFit, Styled, StyledImage, img, px, rems};

/// The canonical filled app icon, embedded at compile time so the binary has
/// no runtime dependency on the checkout it was built from. Every sidebar
/// state uses this same asset, never a generated or transparent substitute.
pub const LOGO_PNG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/icon.png"));

/// The shipped macOS app icon (multi-resolution .icns). This is the file the
/// owner named: the Dock must show it, not a single PNG scaled up to Dock
/// size, which is what looked distorted.
#[cfg(target_os = "macos")]
pub const APP_ICON_ICNS: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/icon.icns"));

/// The filled app icon for every sidebar state, clipped to a rounded square.
pub fn banner_logo() -> impl IntoElement {
    img(std::sync::Arc::new(Image::from_bytes(
        ImageFormat::Png,
        LOGO_PNG.to_vec(),
    )))
    .w(rems(1.5))
    .h(rems(1.5))
    .rounded(px(5.0))
    .object_fit(ObjectFit::Cover)
}

/// The pencil the transcript's edit button draws (#2826). Embedded for the
/// same reason as the marks in `platform_icons.rs`: the owner asked for an
/// icon and got the word "Edit" in a box, because a label is not a picture.
/// The asset carries the placeholder grey where the stroke goes, so one file
/// draws in whatever colour the row's theme asks for.
pub const EDIT_SVG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/edit.svg"));

/// The literal the vector carries where the stroke colour goes.
const ICON_PLACEHOLDER: &str = "#8a8a8a";

/// The copy mark the transcript's copy button draws (#2750), the same
/// placeholder-grey asset trick as the pencil above.
pub const COPY_SVG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/copy.svg"));

/// The pencil in `colour`, sized in rems (#2826).
pub fn edit_icon(colour: u32, size: f32) -> impl IntoElement {
    let svg = String::from_utf8_lossy(EDIT_SVG)
        .replace(ICON_PLACEHOLDER, &format!("#{colour:06x}"))
        .into_bytes();
    img(std::sync::Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        svg,
    )))
    .w(rems(size))
    .h(rems(size))
    .flex_none()
    .object_fit(ObjectFit::Contain)
}

/// The copy mark in `colour`, sized in rems (#2750).
pub fn copy_icon(colour: u32, size: f32) -> impl IntoElement {
    let svg = String::from_utf8_lossy(COPY_SVG)
        .replace(ICON_PLACEHOLDER, &format!("#{colour:06x}"))
        .into_bytes();
    img(std::sync::Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        svg,
    )))
    .w(rems(size))
    .h(rems(size))
    .flex_none()
    .object_fit(ObjectFit::Contain)
}

/// GPUI 0.2.2 decodes an in-memory SVG to RGBA but uploads it as BGRA. Its
/// other image formats swap the red and blue channels first; the SVG arm does
/// not. Cancel that swap here so the colour the caller passes is the colour
/// the window paints instead of turning orange accents blue.
fn gpui_svg_colour(colour: u32) -> u32 {
    (colour & 0x00ff00) | ((colour & 0xff0000) >> 16) | ((colour & 0x0000ff) << 16)
}

/// Telegram's disclosure control: one rounded, full-size downward chevron.
/// The head keeps this direction while the card is open, and the footer
/// carries `collapse_svg` instead, so the way back is visible from the end of
/// the block the reader just unfolded.
fn disclosure_svg(colour: u32) -> Vec<u8> {
    let colour = gpui_svg_colour(colour);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none"><path d="m6 9 6 6 6-6" stroke="#{colour:06x}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"##
    )
    .into_bytes()
}

/// The same control pointing back up: the fold that closes an open card.
fn collapse_svg(colour: u32) -> Vec<u8> {
    let colour = gpui_svg_colour(colour);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none"><path d="m18 15-6-6-6 6" stroke="#{colour:06x}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"##
    )
    .into_bytes()
}

/// Bytes of an inline SVG as a fixed-size element, sized in rems. The
/// chevrons and the composer's paperclip all go through it, so one place
/// decides how an inline mark is sized and fitted.
fn svg_icon(svg: Vec<u8>, size: f32) -> impl IntoElement {
    img(std::sync::Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        svg,
    )))
    .w(rems(size))
    .h(rems(size))
    .flex_none()
    .object_fit(ObjectFit::Contain)
}

/// The disclosure arrow in `colour`, sized in rems.
pub fn disclosure_icon(colour: u32, size: f32) -> impl IntoElement {
    svg_icon(disclosure_svg(colour), size)
}

/// The fold arrow in `colour`, sized in rems. It sits in the footer of an
/// expanded card, so a block with many calls ends in a control that says the
/// click folds it back instead of leaving the reader to guess.
pub fn collapse_icon(colour: u32, size: f32) -> impl IntoElement {
    svg_icon(collapse_svg(colour), size)
}

/// The composer's attachment control (#3037): Telegram's monoline paperclip,
/// drawn from bytes so the theme colour reaches it. The field used to draw the
/// native 📎 emoji, and an emoji carries its own shading: no text colour could
/// turn it into the quiet grey outline the messaging apps use, so it read as a
/// sticker sitting on the card instead of a control belonging to the row.
fn paperclip_svg(colour: u32) -> Vec<u8> {
    let colour = gpui_svg_colour(colour);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" stroke="#{colour:06x}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"##
    )
    .into_bytes()
}

/// The attachment paperclip in `colour`, sized in rems.
pub fn paperclip_icon(colour: u32, size: f32) -> impl IntoElement {
    svg_icon(paperclip_svg(colour), size)
}

/// The composer's dictation control (#3044): the same monoline microphone the
/// messaging apps put beside their field, at the paperclip's weight so the two
/// ends of the row read as one line of controls. The capsule is the diaphragm,
/// the arc is the pickup, the stem is the stand.
fn mic_svg(colour: u32) -> Vec<u8> {
    let colour = gpui_svg_colour(colour);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none"><rect x="9" y="2" width="6" height="12" rx="3" stroke="#{colour:06x}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="M19 10v1a7 7 0 0 1-14 0v-1" stroke="#{colour:06x}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="M12 18v3" stroke="#{colour:06x}" stroke-width="1.8" stroke-linecap="round"/></svg>"##
    )
    .into_bytes()
}

/// The dictation microphone in `colour`, sized in rems.
pub fn mic_icon(colour: u32, size: f32) -> impl IntoElement {
    svg_icon(mic_svg(colour), size)
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

#[cfg(test)]
mod tests {
    use super::{collapse_svg, disclosure_svg};

    #[test]
    fn the_disclosure_arrow_is_a_full_size_rounded_vector() {
        let svg = String::from_utf8(disclosure_svg(0x12ab34)).unwrap();
        assert!(svg.contains(r#"d="m6 9 6 6 6-6""#));
        assert!(!svg.contains(r#"d="m9 6 6 6-6 6""#));
        // GPUI uploads SVG without its usual RGBA-to-BGRA conversion.
        assert!(svg.contains("stroke=\"#34ab12\""));
        assert!(svg.contains("stroke-width=\"2.2\""));
        assert!(svg.contains("stroke-linecap=\"round\""));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
    }

    /// The footer of an open card carries the same control pointing back up,
    /// so the fold that closes the block is drawn instead of guessed.
    #[test]
    fn the_fold_arrow_points_back_up() {
        let svg = String::from_utf8(collapse_svg(0x12ab34)).unwrap();
        assert!(svg.contains(r#"d="m18 15-6-6-6 6""#));
        assert!(!svg.contains(r#"d="m6 9 6 6 6-6""#));
        // The same colour cancellation the disclosure arrow needs.
        assert!(svg.contains("stroke=\"#34ab12\""));
        assert!(svg.contains("stroke-width=\"2.2\""));
        assert!(svg.contains("stroke-linecap=\"round\""));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
    }

    /// The composer's paperclip is a thin monoline outline, not a filled mark
    /// (#3037): the control has to sit in the row like the messaging apps'
    /// does, and a heavy glyph would pull the eye off the text it belongs to.
    #[test]
    fn the_paperclip_is_a_thin_themed_outline() {
        let svg = String::from_utf8(paperclip_svg(0x12ab34)).unwrap();
        assert!(svg.contains("stroke-width=\"1.8\""));
        assert!(
            svg.contains("fill=\"none\""),
            "an outline, not a filled shape"
        );
        assert!(svg.contains("stroke-linecap=\"round\""));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        // Same channel cancellation the chevrons need, so the caller's colour
        // is the colour the window paints.
        assert!(svg.contains("stroke=\"#34ab12\""));
    }
}
