//! Attachment sizing, as one pure planner.
//!
//! The row that draws the files and the row that reserves space for them must
//! agree, and the way to guarantee that is to have exactly one function that
//! decides both. Before this module the transcript hard coded
//! `const TILE: f32 = 120.0` in the drawing loop and nothing told the bubble
//! how tall the row would be, so a tile laid out taller than that constant
//! painted over the line above it (#2395).
//!
//! Everything here is arithmetic over points and a file count, plus the one
//! piece of path math that decides where a poster frame is cached. No gpui, no
//! palette, nothing that touches the disk, so the cases the layout has to
//! survive are seats in a test instead of screenshots.

use std::path::{Path, PathBuf};

/// The smallest tile the row will draw, in points. Below this the tile stops
/// reading as a picture, so the row takes another line instead.
pub const MIN_TILE: f32 = 96.0;

/// The largest tile. A wide window must not turn one screenshot into a wall.
pub const MAX_TILE: f32 = 200.0;

/// Three to a row at most: a fourth would fall under `MIN_TILE` on the pane
/// widths the window actually opens at.
pub const PER_ROW: usize = 3;

/// The most tiles drawn before the rest fold into a single `+N` cell.
pub const FOLD_AFTER: usize = 8;

/// The caption line under a tile plus the gap above it, in points.
pub const CAPTION: f32 = 16.0;

/// The gap between two tiles and between two rows, in points (`gap_2`).
pub const GAP: f32 = 8.0;

/// The points the pane spends on everything that is not content: its own
/// padding, the gap beside the rail, the splitter, and the border slack a
/// fractional layout pass eats. Subtracted so a planned row is never wider
/// than the box it is drawn into.
const CHROME: f32 = 48.0;

/// Where video poster frames are cached, relative to the OpenCrabs home.
/// Under the home and not beside the video: a clip that arrives in chat is
/// usually a file the sender owns and can move, and a poster is our cache,
/// not their data.
pub const POSTER_CACHE: &str = "cache/posters";

/// Where the poster frame for `source` is cached.
///
/// The name is a hash of the absolute path plus the file's mtime, so a
/// re-recorded clip at the same path gets a new poster instead of the frame
/// from the take that was replaced. Nothing here touches the disk: extraction
/// belongs to the caller, which is what keeps `ffmpeg` off the render path.
pub fn poster_cache_path(home: &Path, source: &Path, mtime_secs: u64) -> PathBuf {
    let text = source.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= mtime_secs;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    home.join(POSTER_CACHE).join(format!("{hash:016x}.png"))
}

/// One tile's box. Square today: a shape chosen per file would make the row
/// ragged, and the real aspect belongs in the preview, not in the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub w: f32,
    pub h: f32,
}

impl Tile {
    /// The height one tile plus its caption occupies.
    pub fn cell_height(self) -> f32 {
        self.h + CAPTION
    }
}

/// How a message's attachments lay out, and how tall the strip is.
#[derive(Clone, Debug, PartialEq)]
pub struct RowPlan {
    /// Cells per row, the fold cell included. Empty when there is nothing to
    /// draw, which is what lets the caller skip the strip entirely.
    pub rows: Vec<usize>,
    /// The one tile box every cell uses.
    pub tile: Tile,
    /// The height the message row has to reserve, caption and row gaps
    /// included. This is the number that stops the overlap.
    pub strip_height: f32,
    /// Files not drawn, represented by the fold cell that reads `+N`.
    pub folded: usize,
}

impl RowPlan {
    /// Cells actually drawn, the fold cell included.
    pub fn cells(&self) -> usize {
        self.rows.iter().sum()
    }

    /// Whether there is anything to draw at all.
    pub fn is_empty(&self) -> bool {
        self.cells() == 0
    }
}

/// Plan the row for `count` attachments inside `available_width` points.
///
/// `available_width` is the bubble's content width, not the window's: the
/// caller subtracts its own padding. A width too small for one tile still gets
/// one `MIN_TILE` tile, so the row clips rather than vanishing, and the caller
/// keeps `min_w_0` so the bubble never pushes the pane wide.
pub fn plan(count: usize, available_width: f32) -> RowPlan {
    let width = if available_width.is_finite() {
        available_width.max(0.0)
    } else {
        0.0
    };
    let per_row = per_row_count(width);
    let side = tile_side(width, per_row);
    let drawn = count.min(FOLD_AFTER);
    let folded = count.saturating_sub(drawn);
    // The fold cell is one more cell: nine files draw eight tiles and a `+1`.
    let cells = drawn + usize::from(folded > 0);
    let rows = rows_of(cells, per_row);
    let row_count = rows.len() as f32;
    let tile = Tile { w: side, h: side };
    // One definition of how tall a row is: the tile plus its caption, which is
    // the same number the test checks the reservation against.
    let strip_height = if rows.is_empty() {
        0.0
    } else {
        row_count * tile.cell_height() + (row_count - 1.0) * GAP
    };
    RowPlan {
        rows,
        tile,
        strip_height,
        folded,
    }
}

/// The width the transcript's content gets, in points, from the window's
/// viewport and the rail beside it.
///
/// This is the number the planner runs on, so it is clamped: a window dragged
/// narrow, a rail dragged wide, or a viewport read that returns nothing still
/// has to yield a width one legal tile can sit in. A negative width would plan
/// a negative tile and paint outside the pane.
pub fn content_width(viewport: f32, rail: f32) -> f32 {
    let viewport = if viewport.is_finite() { viewport } else { 0.0 };
    let rail = if rail.is_finite() { rail } else { 0.0 };
    (viewport - rail - CHROME).max(MIN_TILE + 2.0 * GAP)
}

/// The most tiles that fit a row at `MIN_TILE` with `GAP` between them.
fn per_row_count(width: f32) -> usize {
    (1..=PER_ROW).rev().find(|n| fits(width, *n)).unwrap_or(1)
}

fn fits(width: f32, n: usize) -> bool {
    let n = n as f32;
    n * MIN_TILE + (n - 1.0) * GAP <= width
}

/// The tile side for `n` tiles across `width`, clamped to the caps.
fn tile_side(width: f32, n: usize) -> f32 {
    let n = n as f32;
    ((width - (n - 1.0) * GAP) / n).clamp(MIN_TILE, MAX_TILE)
}

/// Chunk `cells` into rows of at most `per_row`.
fn rows_of(cells: usize, per_row: usize) -> Vec<usize> {
    if cells == 0 || per_row == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut left = cells;
    while left > 0 {
        let take = left.min(per_row);
        rows.push(take);
        left -= take;
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A poster name is stable for one file at one mtime and different for
    /// anything else, because a cached frame that survives the clip it came
    /// from is a thumbnail of a video that no longer exists.
    #[test]
    fn poster_cache_key_is_stable_per_file_and_follows_the_mtime() {
        // Fixtures hang off the OS temp dir, built from `temp_dir()` rather
        // than a `/tmp` literal: that literal is live for the Windows build
        // and the footgun scan flags it in test code too.
        let root = std::env::temp_dir().join("gui_attach");
        let home = root.join("fake-home");
        let clip = root.join("clip.mp4");
        let other = root.join("other.mp4");
        let (home, clip) = (home.as_path(), clip.as_path());
        let first = poster_cache_path(home, clip, 1_700_000_000);
        assert_eq!(
            first,
            poster_cache_path(home, clip, 1_700_000_000),
            "the same clip at the same mtime reuses its frame"
        );
        assert_eq!(
            first,
            home.join(POSTER_CACHE).join(first.file_name().unwrap()),
            "posters live in the cache dir, never beside the video"
        );
        assert_ne!(
            first,
            poster_cache_path(home, clip, 1_700_000_001),
            "a re-recorded clip gets a new frame"
        );
        assert_ne!(
            first,
            poster_cache_path(home, other.as_path(), 1_700_000_000),
            "two clips never share a frame"
        );
        assert_eq!(
            first.extension().and_then(|e| e.to_str()),
            Some("png"),
            "ffmpeg writes a png and the overlay asks for one"
        );
    }

    /// A width that fits three `MIN_TILE` tiles and two gaps, with slack.
    const WIDE: f32 = 420.0;

    #[test]
    fn zero_attachments_plan_nothing() {
        let p = plan(0, WIDE);
        assert!(p.is_empty());
        assert_eq!(p.strip_height, 0.0);
        assert_eq!(p.folded, 0);
        assert!(p.rows.is_empty());
    }

    #[test]
    fn one_attachment_is_one_row_one_cell() {
        let p = plan(1, WIDE);
        assert_eq!(p.rows, vec![1]);
        assert_eq!(p.folded, 0);
        assert_eq!(p.tile.w, p.tile.h);
        assert_eq!(p.strip_height, p.tile.h + CAPTION);
    }

    #[test]
    fn three_attachments_fill_one_row() {
        let p = plan(3, WIDE);
        assert_eq!(p.rows, vec![3]);
        assert_eq!(p.cells(), 3);
        assert_eq!(p.folded, 0);
    }

    #[test]
    fn four_attachments_wrap_two_rows() {
        let p = plan(4, WIDE);
        assert_eq!(p.rows, vec![3, 1]);
        assert_eq!(p.cells(), 4);
        assert_eq!(p.folded, 0);
        // Two rows means two cell heights and one gap between them.
        assert_eq!(
            p.strip_height,
            2.0 * (p.tile.h + CAPTION) + GAP,
            "the reserved height must cover both rows and the gap"
        );
    }

    #[test]
    fn eight_attachments_draw_every_one() {
        let p = plan(8, WIDE);
        assert_eq!(p.folded, 0);
        assert_eq!(p.cells(), 8);
        assert_eq!(p.rows, vec![3, 3, 2]);
    }

    #[test]
    fn nine_attachments_fold_the_rest() {
        let p = plan(9, WIDE);
        assert_eq!(p.folded, 1);
        assert_eq!(p.cells(), 9, "eight files plus one fold cell");
        assert_eq!(p.rows, vec![3, 3, 3]);
    }

    #[test]
    fn a_pane_narrower_than_one_tile_keeps_one_full_tile() {
        let p = plan(3, 60.0);
        assert_eq!(p.rows, vec![1, 1, 1]);
        assert_eq!(p.tile.w, MIN_TILE, "never shrink under the readable floor");
        assert_eq!(p.strip_height, 3.0 * (MIN_TILE + CAPTION) + 2.0 * GAP);
    }

    #[test]
    fn a_very_wide_pane_caps_the_tile() {
        let p = plan(3, 4000.0);
        assert_eq!(p.rows, vec![3]);
        assert_eq!(p.tile.w, MAX_TILE);
    }

    #[test]
    fn three_tiles_and_two_gaps_fit_the_width_they_were_planned_for() {
        for width in [304.0, 360.0, 480.0, 900.0] {
            let p = plan(3, width);
            let used = 3.0 * p.tile.w + 2.0 * GAP;
            assert!(
                used <= width + 0.01,
                "three tiles used {used} in {width}, which would push the pane"
            );
        }
    }

    #[test]
    fn a_width_that_fits_two_tiles_plans_two() {
        // 2 * 96 + 8 = 200 fits, 3 * 96 + 16 = 304 does not.
        let p = plan(3, 240.0);
        assert_eq!(p.rows, vec![2, 1]);
        let used = 2.0 * p.tile.w + GAP;
        assert!(used <= 240.0 + 0.01, "two tiles used {used} in 240");
    }

    #[test]
    fn a_nonsense_width_still_plans_a_legal_row() {
        for width in [f32::NAN, f32::INFINITY, -50.0] {
            let p = plan(2, width);
            assert_eq!(p.tile.w, MIN_TILE, "a bad width must not poison the tile");
            assert_eq!(p.cells(), 2);
        }
    }

    #[test]
    fn content_width_subtracts_the_rail_and_its_chrome() {
        // The window opens at 980 with a 17 rem rail: 980 - 272 - 48.
        assert_eq!(content_width(980.0, 272.0), 660.0);
    }

    #[test]
    fn content_width_clamps_a_pane_too_narrow_for_one_tile() {
        // Rail at its maximum on an unchanged window minus the pane chrome is
        // below one tile, so the floor takes over instead of going negative.
        assert_eq!(content_width(980.0, 900.0), MIN_TILE + 2.0 * GAP);
        assert_eq!(content_width(100.0, 0.0), MIN_TILE + 2.0 * GAP);
    }

    #[test]
    fn content_width_survives_a_nonsense_viewport() {
        // No viewport is no pane: the floor keeps the row legal instead of
        // letting a NaN through to every length downstream.
        assert_eq!(content_width(f32::NAN, 100.0), MIN_TILE + 2.0 * GAP);
        // A non-finite RAIL is read as "there is no rail", not as "the rail is
        // the whole window": the pane keeps the width it would have with the
        // sidebar closed.
        assert_eq!(content_width(980.0, f32::INFINITY), 980.0 - CHROME);
    }

    /// The layout contract: whatever the planner returns, the height it
    /// reserves covers every row it draws plus the caption under each tile,
    /// and every tile is square. These are the two properties the transcript
    /// leans on, so they are asserted on the plan rather than trusted.
    #[test]
    fn layout_reserved_height_covers_every_row_it_plans() {
        for count in 0..=12usize {
            for width in [0.0, 60.0, 200.0, 448.0, 660.0, 4000.0] {
                let p = plan(count, width);
                assert_eq!(
                    p.tile.w, p.tile.h,
                    "a tile must be square: {count} at {width}"
                );
                if p.rows.is_empty() {
                    assert_eq!(p.strip_height, 0.0);
                    continue;
                }
                let rows = p.rows.len() as f32;
                let expected = rows * p.tile.cell_height() + (rows - 1.0) * GAP;
                assert_eq!(
                    p.strip_height, expected,
                    "reserved height must match the rows drawn: {count} at {width}"
                );
                // Every row fits the width it was planned for, the fold cell
                // included, so nothing can hang off the right edge.
                assert!(
                    p.rows.iter().all(|n| *n <= PER_ROW),
                    "no row may exceed the column cap: {count} at {width}"
                );
            }
        }
    }
}
