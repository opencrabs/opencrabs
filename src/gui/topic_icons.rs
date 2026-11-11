//! The topic icons and the composer's crab set, embedded at compile time
//! (#2478).
//!
//! Two sets, two jobs. The owner keeps them apart on purpose:
//!
//! - [`TOPIC_ICONS`] are the illustrated crab badges under
//!   `src/assets/emojis/topic-icons/`. They belong to topic creation: a topic
//!   tab draws one before its name, the way Telegram's topic bar does.
//! - [`CRAB_ICONS`] are the reaction-style crabs under
//!   `src/assets/emojis/`. They are the input's emoji set, and they are
//!   embedded here so nothing has to load a file at run time.
//!
//! Every byte is `include_bytes!`, so the binary carries its icons and never
//! depends on the checkout it was built from, the same contract as the brand
//! mark in `icon.rs`.

use gpui::{
    AnyElement, Image, ImageFormat, IntoElement, ObjectFit, ParentElement, SharedString, Styled,
    StyledImage, div, img, rems,
};
use std::ops::Range;
use std::sync::Arc;

/// The icon a topic tab shows, as the picker offers it (#2414, #2478).
///
/// The stored value is [`TOPIC_PREFIX`] plus the name here, so a row that
/// already holds a plain emoji still reads as text and nothing in an existing
/// database has to be rewritten.
pub const TOPIC_ICONS: &[(&str, &[u8])] = &[
    (
        "general",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-general.png"
        )),
    ),
    (
        "development",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-development.png"
        )),
    ),
    (
        "ideas",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-ideas.png"
        )),
    ),
    (
        "bug-reports",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-bug-reports.png"
        )),
    ),
    (
        "releases",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-releases.png"
        )),
    ),
    (
        "showcase",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-showcase.png"
        )),
    ),
    (
        "ops",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-ops.png"
        )),
    ),
    (
        "off-topic",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/topic-icons/opencrabs-topic-off-topic.png"
        )),
    ),
];

/// The composer's crab set: one picture per shortcode, the way the terminal
/// prints `:ok:` as a glyph.
///
/// The input itself writes plain characters, so the set is embedded for the
/// surfaces that draw a picture: a tab resolves either set by name, and the
/// GUI reactions (owner-deferred, a later feature) read this table directly.
pub const CRAB_ICONS: &[(&str, &[u8])] = &[
    (
        "100",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/100.png"
        )),
    ),
    (
        "bolt",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/bolt.png"
        )),
    ),
    (
        "bug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/bug.png"
        )),
    ),
    (
        "clap",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/clap.png"
        )),
    ),
    (
        "coffee",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/coffee.png"
        )),
    ),
    (
        "cry",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/cry.png"
        )),
    ),
    (
        "dead",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/dead.png"
        )),
    ),
    (
        "eyes",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/eyes.png"
        )),
    ),
    (
        "fire",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/fire.png"
        )),
    ),
    (
        "gg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/gg.png"
        )),
    ),
    (
        "laugh",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/laugh.png"
        )),
    ),
    (
        "loading",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/loading.png"
        )),
    ),
    (
        "merge",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/merge.png"
        )),
    ),
    (
        "mindblow",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/mindblow.png"
        )),
    ),
    (
        "no",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/no.png"
        )),
    ),
    (
        "ok",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/ok.png"
        )),
    ),
    (
        "party",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/party.png"
        )),
    ),
    (
        "please",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/please.png"
        )),
    ),
    (
        "rocket",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/rocket.png"
        )),
    ),
    (
        "salute",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/salute.png"
        )),
    ),
    (
        "shrug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/shrug.png"
        )),
    ),
    (
        "smug",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/smug.png"
        )),
    ),
    (
        "sweat",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/sweat.png"
        )),
    ),
    (
        "think",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/think.png"
        )),
    ),
    (
        "trophy",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/trophy.png"
        )),
    ),
    (
        "yes",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/emojis/yes.png"
        )),
    ),
];

/// The prefix the topic picker writes in front of an icon's name (#2478).
///
/// It keeps a stored picture apart from a stored emoji: a row written before
/// the picture set existed holds a glyph, and one written now holds
/// `topic:development`. Both render, and neither is guessed at.
pub const TOPIC_PREFIX: &str = "topic:";

/// The stored value for a topic icon name, e.g. `topic:development`.
pub fn store_name(name: &str) -> String {
    format!("{TOPIC_PREFIX}{name}")
}

/// The bytes for a topic icon, by name, with or without [`TOPIC_PREFIX`].
pub fn topic_icon(name: &str) -> Option<&'static [u8]> {
    let name = name.strip_prefix(TOPIC_PREFIX).unwrap_or(name);
    TOPIC_ICONS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, bytes)| *bytes)
}

/// The bytes for a crab, by shortcode.
pub fn crab_icon(name: &str) -> Option<&'static [u8]> {
    CRAB_ICONS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, bytes)| *bytes)
}

/// The pack name of a picture's own bytes (#2794).
///
/// The scanner hands back the bytes rather than the name, and a caller that
/// caches a decoded picture needs the name to key it. The bytes are the same
/// `&'static` slice the pack holds, so the match is by address: no PNG is
/// compared byte by byte.
pub fn crab_name(bytes: &'static [u8]) -> Option<&'static str> {
    CRAB_ICONS
        .iter()
        .find(|(_, candidate)| std::ptr::eq(*candidate, bytes))
        .map(|(name, _)| *name)
}

/// The `:name:` tokens in `text` that name a crab, left to right, with the
/// bytes of the picture each one stands for (#2771).
///
/// The name is matched the way the composer matches it, without regard to
/// case, and it has to look like a shortcode: letters, digits, `_` and `-`,
/// nothing else. A token the pack does not know is left as text, and the scan
/// resumes after the colon that opened it so `:a:b:` still finds `:b:`.
pub fn crab_tokens(text: &str) -> Vec<(Range<usize>, &'static [u8])> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while let Some(open) = text[cursor..].find(':') {
        let open = cursor + open;
        let Some(close) = text[open + 1..].find(':') else {
            break;
        };
        let close = open + 1 + close;
        let name = &text[open + 1..close];
        let shortcode = name.len() <= 32
            && !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        match shortcode.then(|| crab_icon(name)).flatten() {
            Some(bytes) => {
                tokens.push((open..close + 1, bytes));
                cursor = close + 1;
            }
            None => cursor = open + 1,
        }
    }
    tokens
}

/// One piece of a line that a picture splits (#2771).
pub struct InlinePiece {
    /// The bytes of the line this piece covers. Empty for the spacer that only
    /// exists so the selection offsets stay honest.
    pub range: Range<usize>,
    /// The picture to draw, or None for a run of text (and for a spacer).
    pub crab: Option<&'static [u8]>,
    /// What the selection accounting must consume for this piece.
    ///
    /// Drawing a line as several segments costs one separator per segment, and
    /// a picture takes bytes out of the text, so the lengths the transcript
    /// reports cannot be the plain string lengths. `accounted` is the length
    /// that makes the segment offsets of a split line equal the offsets the
    /// unsplit line had, which is what drag-select and copy read.
    pub accounted: usize,
}

/// How to draw `text` when it carries crab tokens: one piece per run of words
/// and one per picture, in source order. None when there is nothing to draw
/// but the text itself.
pub fn inline_pieces(text: &str) -> Option<Vec<InlinePiece>> {
    let tokens = crab_tokens(text);
    if tokens.is_empty() {
        return None;
    }
    // The runs of text between the tokens.
    let mut runs = Vec::with_capacity(tokens.len() + 1);
    let mut cursor = 0;
    for (token, _) in &tokens {
        if cursor < token.start {
            runs.push(cursor..token.start);
        }
        cursor = token.end;
    }
    if cursor < text.len() {
        runs.push(cursor..text.len());
    }
    // Runs and tokens in source order: a run always sits before the token that
    // follows it, and no run ever spans one.
    let mut ordered: Vec<(Range<usize>, Option<&'static [u8]>)> = Vec::new();
    let mut runs = runs.into_iter().peekable();
    for (token, bytes) in tokens {
        if runs.peek().is_some_and(|run| run.start < token.start)
            && let Some(run) = runs.next()
        {
            ordered.push((run, None));
        }
        ordered.push((token, Some(bytes)));
    }
    ordered.extend(runs.map(|run| (run, None)));
    let mut pieces: Vec<InlinePiece> = ordered
        .into_iter()
        .map(|(range, crab)| InlinePiece {
            range,
            crab,
            accounted: 0,
        })
        .collect();

    // A line that opens on a picture has bytes ahead of its first run, and a
    // line that is nothing but pictures has no run at all. Both still have to
    // move the accounting past those bytes, so one empty piece carries them.
    let first_start = pieces
        .iter()
        .find(|piece| piece.crab.is_none())
        .map(|piece| piece.range.start);
    match first_start {
        Some(0) => {}
        Some(_) => pieces.insert(
            0,
            InlinePiece {
                range: 0..0,
                crab: None,
                accounted: 0,
            },
        ),
        None => pieces.insert(
            0,
            InlinePiece {
                range: 0..0,
                crab: None,
                accounted: text.len(),
            },
        ),
    }
    // Every piece that draws text is one segment: it runs up to the next one,
    // less the separator, and the last one runs to the end of the line.
    let calls: Vec<usize> = pieces
        .iter()
        .enumerate()
        .filter(|(_, piece)| piece.crab.is_none())
        .map(|(index, _)| index)
        .collect();
    for (position, &index) in calls.iter().enumerate() {
        let start = pieces[index].range.start;
        pieces[index].accounted = match calls.get(position + 1) {
            Some(&next) => pieces[next].range.start.saturating_sub(start + 1),
            None => text.len().saturating_sub(start),
        };
    }
    Some(pieces)
}

const ORIENTATION_ICON_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/topic-orientation.svg"
));

/// The topic strip's orientation control. An embedded vector keeps the glyph
/// crisp at the larger size and avoids depending on a checkout at run time.
pub fn orientation_icon(size: f32, colour: u32) -> impl IntoElement {
    let svg = String::from_utf8_lossy(ORIENTATION_ICON_SVG)
        .replace("#8a8a8a", &format!("#{colour:06x}"))
        .into_bytes();
    img(Arc::new(Image::from_bytes(ImageFormat::Svg, svg)))
        .w(rems(size))
        .h(rems(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// A stored topic icon resolved to a picture, or `None` when it is a plain
/// emoji and has to stay text (#2478).
///
/// Both embedded sets are consulted: a tab draws whatever it holds, so moving
/// a row from the crab set to the topic set, or back, needs no migration.
pub fn tab_icon(stored: &str, size: f32) -> Option<AnyElement> {
    let bytes = topic_icon(stored).or_else(|| crab_icon(stored))?;
    Some(picture(bytes, size).into_any_element())
}

/// A legacy emoji topic icon in the same centered slot as an embedded picture.
/// Keeping it out of the label preserves the icon-above-name geometry and
/// stops a color emoji from wrapping beside or beyond the topic name.
pub fn tab_glyph(stored: &str, size: f32) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(rems(size))
        .text_size(rems(size * 0.8))
        .child(SharedString::from(stored.to_string()))
        .into_any_element()
}

/// A picture for a tab, a picker cell or the badge in the topic prompt.
/// Contained, never stretched, with the same rounding the banner logo uses.
pub fn picture(bytes: &'static [u8], size: f32) -> impl IntoElement {
    img(Arc::new(Image::from_bytes(
        ImageFormat::Png,
        bytes.to_vec(),
    )))
    .w(rems(size))
    .h(rems(size))
    .flex_none()
    .rounded(gpui::px(size * 2.0))
    .object_fit(ObjectFit::Contain)
}

/// The prefix a custom image icon writes in front of its stored filename
/// (#2790). The file itself lives under `gui-icons/` in the OpenCrabs home;
/// only a bare filename is ever stored, so a value can never name a path
/// outside the store.
pub const IMG_PREFIX: &str = "img:";

/// The stored value for a custom image filename, e.g. `img:<uuid>.png`.
pub fn img_store_name(filename: &str) -> String {
    format!("{IMG_PREFIX}{filename}")
}

/// The directory custom icon images live in: `gui-icons/` under the
/// OpenCrabs home.
pub fn image_store_dir() -> std::path::PathBuf {
    opencrabs::config::opencrabs_home().join("gui-icons")
}

/// Resolve a stored `img:` value to its file path, or `None` when the value
/// is not an image reference or names anything but a bare filename. Path
/// separators and `..` are refused, so a stored value can never escape the
/// store (#2790).
pub fn stored_image_path(stored: &str) -> Option<std::path::PathBuf> {
    let name = stored.strip_prefix(IMG_PREFIX)?;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    Some(image_store_dir().join(name))
}

/// The extensions GPUI can decode for a custom icon (#2790).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "svg"];

/// The largest file the icon picker accepts: 5MB (#2790).
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Whether a picked file can become an icon: a decodable extension and a
/// size under the cap. Pure, so the picker's refusal rules are testable
/// without a window.
pub fn image_pick_allowed(path: &std::path::Path, bytes: u64) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let known = ext.is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()));
    known && bytes <= MAX_IMAGE_BYTES
}

/// The GPUI format a stored image filename decodes as, from its extension.
pub fn image_format(filename: &str) -> Option<ImageFormat> {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "bmp" => Some(ImageFormat::Bmp),
        "tiff" => Some(ImageFormat::Tiff),
        "svg" => Some(ImageFormat::Svg),
        _ => None,
    }
}

/// A custom icon image drawn in the same contained slot as the embedded
/// pictures (#2790). The image is shared: the view reads each file once and
/// the paint only clones the handle, never the bytes.
pub fn cached_picture(image: Arc<Image>, size: f32) -> AnyElement {
    img(image)
        .w(rems(size))
        .h(rems(size))
        .flex_none()
        .rounded(gpui::px(size * 2.0))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The img scheme round-trips: a stored filename resolves back to the
    /// same bare name under the store, and nothing else does (#2790).
    #[test]
    fn img_scheme_round_trips() {
        let stored = img_store_name("crab.png");
        assert_eq!(stored, "img:crab.png");
        let path = stored_image_path(&stored).expect("bare filename resolves");
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("crab.png"));
        assert!(path.starts_with(image_store_dir()));
        // The embedded scheme and a bare name are not image references.
        assert!(stored_image_path("topic:development").is_none());
        assert!(stored_image_path("crab.png").is_none());
    }

    /// A stored value can never name a path outside the store (#2790).
    #[test]
    fn img_scheme_refuses_traversal() {
        for bad in [
            "img:",
            "img:../evil.png",
            "img:a/../../evil.png",
            "img:sub/dir.png",
            "img:back\\slash.png",
            "img:..",
        ] {
            assert!(stored_image_path(bad).is_none(), "{bad} must not resolve");
        }
    }

    /// The picker accepts decodable extensions under the cap and refuses
    /// everything else (#2790). Temp files only, never the live home.
    #[test]
    fn image_pick_validates_extension_and_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = dir.path().join("icon.png");
        std::fs::write(&png, b"not really a png").expect("write");
        assert!(image_pick_allowed(&png, 1024));
        assert!(image_pick_allowed(&png, MAX_IMAGE_BYTES));
        assert!(!image_pick_allowed(&png, MAX_IMAGE_BYTES + 1));
        let exe = dir.path().join("icon.exe");
        std::fs::write(&exe, b"nope").expect("write");
        assert!(!image_pick_allowed(&exe, 16));
        let upper = dir.path().join("ICON.PNG");
        std::fs::write(&upper, b"case").expect("write");
        assert!(image_pick_allowed(&upper, 16));
        let noext = dir.path().join("icon");
        std::fs::write(&noext, b"none").expect("write");
        assert!(!image_pick_allowed(&noext, 16));
    }

    /// Every accepted extension maps to the format GPUI decodes it as
    /// (#2790).
    #[test]
    fn image_format_maps_extensions() {
        assert!(matches!(image_format("img:a.png"), Some(ImageFormat::Png)));
        assert!(matches!(image_format("img:a.JPG"), Some(ImageFormat::Jpeg)));
        assert!(matches!(
            image_format("img:a.jpeg"),
            Some(ImageFormat::Jpeg)
        ));
        assert!(matches!(
            image_format("img:a.webp"),
            Some(ImageFormat::Webp)
        ));
        assert!(matches!(image_format("img:a.svg"), Some(ImageFormat::Svg)));
        assert!(image_format("img:a.exe").is_none());
        assert!(image_format("img:noext").is_none());
    }

    /// A custom reference is not an embedded picture, so the tab resolver
    /// has to try the image store before falling back to text (#2790).
    #[test]
    fn img_reference_is_not_an_embedded_icon() {
        assert!(tab_icon("img:crab.png", 1.0).is_none());
        assert!(topic_icon("img:crab.png").is_none());
    }

    /// A PNG always starts with these four bytes, so an embedded file that is
    /// not one (a stray `.DS_Store`, a renamed JPG) fails here instead of
    /// drawing a blank box in the bar.
    fn is_png(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0x89, b'P', b'N', b'G'])
    }

    #[test]
    fn every_topic_icon_resolves_by_name_and_by_stored_value() {
        for (name, bytes) in TOPIC_ICONS {
            assert!(is_png(bytes), "{name} is not a PNG");
            assert_eq!(topic_icon(name), Some(*bytes), "{name} by name");
            let stored = store_name(name);
            assert_eq!(topic_icon(&stored), Some(*bytes), "{stored} as stored");
        }
    }

    #[test]
    fn the_picker_offers_the_eight_topic_badges() {
        let mut names: Vec<&str> = TOPIC_ICONS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "bug-reports",
                "development",
                "general",
                "ideas",
                "off-topic",
                "ops",
                "releases",
                "showcase"
            ]
        );
    }

    #[test]
    fn the_crab_set_matches_its_shortcodes() {
        assert_eq!(CRAB_ICONS.len(), 26);
        for (name, bytes) in CRAB_ICONS {
            assert!(is_png(bytes), "{name} is not a PNG");
            assert_eq!(crab_icon(name), Some(*bytes), "{name} by shortcode");
        }
        assert_eq!(crab_icon("nope"), None);
    }

    /// A row written before the pictures existed holds an emoji, and it has to
    /// keep drawing as text rather than resolve to a wrong icon (#2478).
    #[test]
    fn a_stored_emoji_is_not_a_picture() {
        assert_eq!(topic_icon("💬"), None);
        assert_eq!(crab_icon("💬"), None);
    }

    #[test]
    fn the_prefix_is_stripped_so_both_forms_find_the_same_picture() {
        let stored = store_name("ops");
        assert_eq!(stored, "topic:ops");
        assert_eq!(topic_icon(&stored), topic_icon("ops"));
        assert_eq!(topic_icon("topic:nope"), None);
    }

    #[test]
    fn topic_style_orientation_icon_is_an_embedded_modern_vector() {
        let svg = std::str::from_utf8(ORIENTATION_ICON_SVG).expect("orientation SVG is UTF-8");
        assert!(svg.starts_with("<svg"));
        // #2542: GPUI rasterizes the vector from its intrinsic size. Keep
        // enough source pixels for the fixed 18px display size to stay sharp.
        assert!(svg.contains("width=\"96\" height=\"96\""));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("stroke-linecap=\"round\""));
        assert!(svg.contains("stroke-linejoin=\"round\""));
        assert!(svg.contains("<path"));
        assert!(!svg.contains("<rect"));
    }

    /// A shortcode written in a message is the emoji, whatever case it was
    /// typed in (#2771).
    #[test]
    fn a_crab_token_is_found_without_case() {
        let tokens = crab_tokens("ship it :Cry:");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, 8..13);
        assert_eq!(Some(tokens[0].1), crab_icon("cry"));
    }

    /// A name the pack does not know stays what it is, text, and a word that
    /// merely holds colons is not a shortcode either.
    #[test]
    fn only_the_pack_turns_into_pictures() {
        assert!(crab_tokens(":nope: and :warning:").is_empty());
        assert!(crab_tokens("ship 10:30:00").is_empty());
        assert!(crab_tokens("no tokens here").is_empty());
    }

    /// Two tokens side by side are two pictures, and the second colon of the
    /// first is not the opener of the second.
    #[test]
    fn adjacent_tokens_do_not_overlap() {
        let tokens = crab_tokens(":ok::cry:");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].0, 0..4);
        assert_eq!(tokens[1].0, 4..9);
    }

    /// The pieces a line is drawn from: words where there are words, a picture
    /// where a token names one, and one empty piece ahead of a line that opens
    /// on a picture.
    #[test]
    fn a_line_splits_into_words_and_pictures() {
        let pieces = inline_pieces("hi :cry: there").expect("a token");
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].range, 0..3);
        assert!(pieces[0].crab.is_none());
        assert_eq!(pieces[1].range, 3..8);
        assert_eq!(Some(pieces[1].crab.unwrap()), crab_icon("cry"));
        assert_eq!(pieces[2].range, 8..14);
        assert!(inline_pieces("no token").is_none());
    }

    /// The reason `accounted` exists: replay the segment accounting the
    /// transcript does and the offsets must land where the whole line puts
    /// them, whatever the split. A drift here silently moves every row's
    /// drag-select and copy by the width of a token.
    #[test]
    fn planned_offsets_match_the_whole_line() {
        for line in [
            "hi :cry: there",
            ":cry: hi",
            "hi :cry:",
            ":ok::cry:",
            "a :ok: b :cry: c",
            ":cry:",
            "plain words only",
        ] {
            let Some(pieces) = inline_pieces(line) else {
                continue;
            };
            let mut offset = 0;
            for (segments, piece) in pieces
                .iter()
                .filter(|piece| piece.crab.is_none())
                .enumerate()
            {
                let start = if segments == 0 { 0 } else { offset + 1 };
                assert_eq!(
                    start, piece.range.start,
                    "{line}: segment starts at {start}, not {}",
                    piece.range.start
                );
                offset = start + piece.accounted;
            }
            assert_eq!(offset, line.len(), "{line}: the line ends short");
        }
    }

    /// A line can hold accented text, an emoji and CJK next to a shortcode, so
    /// every range the scan reports has to land on a char boundary. The
    /// transcript slices the line by those ranges, and an offset one byte into
    /// a multi-byte character panics at paint time rather than failing a test
    /// (#2771, #2298).
    #[test]
    fn multi_byte_lines_report_ranges_on_char_boundaries() {
        for line in [
            "olá :cry: 日本語",
            ":cry: ação",
            "emoji :ok: 🦀 tail",
            "ção :ok::cry: fim",
            "🦀:cry:🦀",
        ] {
            for (range, _) in crab_tokens(line) {
                assert!(
                    line.is_char_boundary(range.start),
                    "{line}: token starts mid-character at {}",
                    range.start
                );
                assert!(
                    line.is_char_boundary(range.end),
                    "{line}: token ends mid-character at {}",
                    range.end
                );
            }
            let Some(pieces) = inline_pieces(line) else {
                continue;
            };
            for piece in pieces {
                assert!(
                    line.is_char_boundary(piece.range.start),
                    "{line}: piece starts mid-character at {}",
                    piece.range.start
                );
                assert!(
                    line.is_char_boundary(piece.range.end),
                    "{line}: piece ends mid-character at {}",
                    piece.range.end
                );
                // The slice the transcript takes of this piece, taken the safe
                // way so the test itself adds no byte-slice site.
                line.get(piece.range)
                    .unwrap_or_else(|| panic!("{line}: piece range is not a slice"));
            }
        }
    }
}
