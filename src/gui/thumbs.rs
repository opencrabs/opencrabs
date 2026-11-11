//! Square thumbnails for attachment pictures.
//!
//! `img(..).object_fit(ObjectFit::Cover)` cannot be clipped to a rounded box.
//! gpui builds the painted quad from the fit (`style.rs`, `ObjectFit::get_bounds`),
//! so a cover fit paints a rectangle LARGER than the box and centres it: a 16:9
//! picture in a 112pt tile paints a 199x112 quad starting at x = -43.5. The
//! corner radii are then clamped to that inflated quad's size and painted on it
//! (`elements/img.rs`, `Img::paint`), so the picture's arc sits outside the
//! tile's arc and every pixel between the two paints over the card.
//!
//! Nothing on the gpui side can fix that from the outside: `ContentMask` carries
//! bounds only, no radius, so `overflow_hidden()` on the tile is a plain
//! rectangle and the tile's own `rounded` shapes its border and nothing else.
//!
//! So the crop happens once, on disk, and the tile paints a square picture with
//! `Fill`, where the painted quad equals the box and one radius covers both.
//! The cache file is an ordinary PNG, so gpui decodes it on its own asset thread
//! like every other picture; only the one-time crop runs on ours.

use gpui::prelude::*;
use gpui::{Img, ObjectFit, Pixels, img};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Where square thumbnails are cached, relative to the OpenCrabs home. Under
/// the home and not beside the picture: a file that arrives in chat belongs to
/// the sender and can move, and a thumbnail is our cache, not their data.
pub const THUMB_CACHE: &str = "cache/thumbs";

/// The cached side, in pixels. Two and a half times the largest tile
/// (`attachment_view::MAX_TILE`), so a retina window draws it 1:1 at 2x and only
/// mildly soft at 3x. One size for every tile keeps one file per picture.
pub const THUMB_SIDE: u32 = 512;

/// How many thumbnails the cache keeps. It is regenerable by definition, so the
/// cap is a disk budget, not a promise: the oldest files go when a new crop
/// lands past it.
pub const MAX_CACHED: usize = 300;

/// Pictures already known to have no crop, so a format the decoder cannot read
/// is attempted once per session instead of on every frame it is drawn.
static FAILED: OnceLock<Mutex<HashSet<(PathBuf, u64)>>> = OnceLock::new();

/// The picture for one attachment, at `radius`, inside a box that owns the
/// size.
///
/// A square crop paints with `Fill`: the quad is the box, so the radius lands
/// where the tile's border does. A picture we cannot crop falls back to
/// `Contain` on the original, which also never paints outside the box: it
/// letterboxes instead of bleeding. Both branches pin both sides, which is what
/// keeps gpui from deriving a height from the aspect ratio (#2395).
pub fn picture(path: &Path, radius: Pixels) -> Img {
    match square_from_home(path) {
        Some(square) => img(square)
            .size_full()
            .rounded(radius)
            .object_fit(ObjectFit::Fill),
        None => img(path.to_path_buf())
            .size_full()
            .rounded(radius)
            .object_fit(ObjectFit::Contain),
    }
}

/// The square thumbnail for `source`, cropping it on first use. `None` when the
/// file is gone or the decoder cannot read it.
fn square_from_home(source: &Path) -> Option<PathBuf> {
    square_for(&opencrabs::config::opencrabs_home(), source)
}

/// The square thumbnail for `source` under `home`, cropping it on first use.
pub fn square_for(home: &Path, source: &Path) -> Option<PathBuf> {
    if !source.is_file() {
        return None;
    }
    let stamp = modified_secs(source);
    let dest = thumb_cache_path(home, source, stamp);
    if is_usable(&dest) {
        return Some(dest);
    }
    if failed_before(source, stamp) {
        return None;
    }
    if !write_square(source, &dest) {
        remember_failure(source, stamp);
        return None;
    }
    prune(home, MAX_CACHED);
    Some(dest)
}

/// Where the square thumbnail for `source` lives.
///
/// The name is a hash of the absolute path, the file's mtime and the cached
/// side, so a picture replaced at the same path gets a new thumbnail instead of
/// the crop of the file that was there before, and a future change to
/// `THUMB_SIDE` cannot draw a stale one. Nothing here touches the disk beyond
/// the stat the caller already paid for.
pub fn thumb_cache_path(home: &Path, source: &Path, mtime_secs: u64) -> PathBuf {
    let text = source.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= mtime_secs;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    hash ^= u64::from(THUMB_SIDE);
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    home.join(THUMB_CACHE)
        .join(format!("{hash:016x}-{THUMB_SIDE}.png"))
}

/// The centred square of `source`, scaled to `THUMB_SIDE`, written to `dest`.
///
/// The square is the shorter edge, taken from the middle, which is what makes
/// the tile read as a cover crop instead of a stretch. The write goes through a
/// `.part` sibling and a rename, so a killed process never leaves a half PNG
/// that the next run would happily draw.
fn write_square(source: &Path, dest: &Path) -> bool {
    let Ok(opened) = image::open(source) else {
        return false;
    };
    let (width, height) = (opened.width(), opened.height());
    if width == 0 || height == 0 {
        return false;
    }
    let edge = width.min(height);
    let square = opened
        .crop_imm((width - edge) / 2, (height - edge) / 2, edge, edge)
        .resize_exact(
            THUMB_SIDE,
            THUMB_SIDE,
            image::imageops::FilterType::Triangle,
        );
    let Some(parent) = dest.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let partial = dest.with_extension("part");
    if square
        .save_with_format(&partial, image::ImageFormat::Png)
        .is_err()
    {
        let _ = std::fs::remove_file(&partial);
        return false;
    }
    if std::fs::rename(&partial, dest).is_err() {
        let _ = std::fs::remove_file(&partial);
        return false;
    }
    true
}

/// A cache hit is a file with pixels in it. A zero byte thumbnail is what a
/// killed crop leaves behind, and drawing it would show a blank tile forever.
fn is_usable(thumb: &Path) -> bool {
    std::fs::metadata(thumb)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

/// Keep the cache inside its disk budget: everything past `keep`, oldest
/// first. Deleting is safe here and nowhere else, because every file in this
/// directory is one we can make again from the picture it came from.
fn prune(home: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(home.join(THUMB_CACHE)) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    let excess = files.len() - keep;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

fn modified_secs(source: &Path) -> u64 {
    std::fs::metadata(source)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn failed_before(source: &Path, stamp: u64) -> bool {
    FAILED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|seen| seen.contains(&(source.to_path_buf(), stamp)))
        .unwrap_or(false)
}

fn remember_failure(source: &Path, stamp: u64) {
    if let Ok(mut seen) = FAILED.get_or_init(|| Mutex::new(HashSet::new())).lock() {
        seen.insert((source.to_path_buf(), stamp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "opencrabs-gui-thumbs-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_png(
        path: &Path,
        width: u32,
        height: u32,
        pixel: impl Fn(u32, u32) -> image::Rgba<u8>,
    ) {
        let mut buffer = image::RgbaImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                buffer.put_pixel(x, y, pixel(x, y));
            }
        }
        buffer.save(path).expect("write png");
    }

    /// The whole point: a wide picture becomes a square, so `Fill` has nothing
    /// left to overhang.
    #[test]
    fn a_wide_picture_becomes_a_square_cache_file() {
        let dir = scratch("wide");
        let source = dir.join("wide.png");
        write_png(&source, 40, 20, |_, _| image::Rgba([10, 20, 30, 255]));

        let thumb = square_for(&dir, &source).expect("a readable png has a thumbnail");
        assert!(thumb.starts_with(dir.join(THUMB_CACHE)), "{thumb:?}");
        let cropped = image::open(&thumb).expect("the cache file is a png");
        assert_eq!(
            (cropped.width(), cropped.height()),
            (THUMB_SIDE, THUMB_SIDE)
        );
    }

    /// A crop, not a stretch: the middle of the source is the whole square, so
    /// a two tone picture keeps its left colour on the left half.
    #[test]
    fn the_square_is_taken_from_the_centre() {
        let dir = scratch("centre");
        let source = dir.join("halves.png");
        write_png(&source, 4, 2, |x, _| {
            if x < 2 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });

        let thumb = square_for(&dir, &source).expect("a readable png has a thumbnail");
        let cropped = image::open(&thumb).expect("the cache file is a png");
        let left = cropped.get_pixel(10, THUMB_SIDE / 2);
        let right = cropped.get_pixel(THUMB_SIDE - 10, THUMB_SIDE / 2);
        assert!(
            left[0] > left[2],
            "the left half keeps the source's left colour: {left:?}"
        );
        assert!(
            right[2] > right[0],
            "the right half keeps the source's right colour: {right:?}"
        );
    }

    /// One crop per picture. The marker replaces the cache file, so a second
    /// call that re-cropped would overwrite it and fail here.
    #[test]
    fn the_same_picture_is_cropped_once() {
        let dir = scratch("once");
        let source = dir.join("shot.png");
        write_png(&source, 12, 8, |_, _| image::Rgba([1, 2, 3, 255]));

        let first = square_for(&dir, &source).expect("first crop");
        write_png(&first, 3, 3, |_, _| image::Rgba([9, 9, 9, 255]));

        let second = square_for(&dir, &source).expect("cache hit");
        assert_eq!(first, second);
        let kept = image::open(&second).expect("the marker survived");
        assert_eq!((kept.width(), kept.height()), (3, 3), "no second crop ran");
    }

    /// Formats outside the decoder's features, and files that are not there,
    /// get no thumbnail: the caller falls back instead of drawing nothing.
    #[test]
    fn an_unreadable_picture_has_no_thumbnail() {
        let dir = scratch("unreadable");
        let not_a_picture = dir.join("clip.webp");
        std::fs::write(&not_a_picture, b"not really a webp").expect("write");
        assert_eq!(square_for(&dir, &not_a_picture), None);
        assert_eq!(square_for(&dir, &dir.join("gone.png")), None);
    }

    /// A different mtime at the same path is a different picture, so it must
    /// not reuse the thumbnail of the file it replaced.
    #[test]
    fn a_rewritten_picture_gets_its_own_thumbnail() {
        let home = Path::new("home/crabs");
        // Relative fixtures: `thumb_cache_path` hashes the path string and
        // never touches the filesystem, and a POSIX `/tmp` literal in a module
        // that ships on Windows trips the windows-footgun scan.
        let source = Path::new("shots/shot.png");
        assert_ne!(
            thumb_cache_path(home, source, 1_700_000_000),
            thumb_cache_path(home, source, 1_700_000_001)
        );
        assert_ne!(
            thumb_cache_path(home, Path::new("shots/other.png"), 1_700_000_000),
            thumb_cache_path(home, source, 1_700_000_000)
        );
    }

    /// The disk budget is enforced oldest first.
    #[test]
    fn the_cache_keeps_only_the_newest_thumbnails() {
        let home = scratch("prune");
        let dir = home.join(THUMB_CACHE);
        std::fs::create_dir_all(&dir).expect("cache dir");
        for index in 0..4u32 {
            let path = dir.join(format!("{index}-{THUMB_SIDE}.png"));
            write_png(&path, 2, 2, |_, _| image::Rgba([index as u8, 0, 0, 255]));
            // Distinct mtimes, so "oldest" is a fact of the fixture and not of
            // how fast the loop ran.
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(index));
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(when))
                .expect("stamp");
        }

        prune(&home, 2);

        let left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read cache")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.contains(&format!("2-{THUMB_SIDE}.png")), "{left:?}");
        assert!(left.contains(&format!("3-{THUMB_SIDE}.png")), "{left:?}");
    }
}
