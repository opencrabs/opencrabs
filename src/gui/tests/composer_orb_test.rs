//! The composer orb's own pixels, exercised without a window (#3092).
//!
//! The block is drawn by `paint`, which takes a slab and the levels, so a test
//! can read the frame it produces instead of waiting for a real clock to tick.
//! These lived in an inline module until #3062's guard caught them: the window
//! keeps one test file per module it exercises, beside the module rather than
//! at the bottom of it.

use crate::composer_orb::{
    MARK_INSET, PlayerBlock, PlayerState, Slab, paint, resample, warm_hue_for,
};
use crate::seeker_orb::{BOWL_HUE, VOICE_SAMPLES, rgb_hsv, smooth_voice};

/// Read one pixel at a point on the block, in units of half the height, and
/// return it as red, green, blue and alpha.
fn sample(bytes: &[u8], slab: &Slab, x: f32, y: f32) -> (f32, f32, f32, u8) {
    let column =
        (((x / slab.aspect + 1.0) * 0.5 * slab.width as f32) as usize).min(slab.width as usize - 1);
    let row = (((y + 1.0) * 0.5 * slab.height as f32) as usize).min(slab.height as usize - 1);
    let index = (row * slab.width as usize + column) * 4;
    (
        bytes[index + 2] as f32,
        bytes[index + 1] as f32,
        bytes[index] as f32,
        bytes[index + 3],
    )
}

fn luma(colour: (f32, f32, f32, u8)) -> f32 {
    0.2126 * colour.0 + 0.7152 * colour.1 + 0.0722 * colour.2
}

/// One frame of a take whose peaks are all `peak`, drawn at a phase and an
/// energy the test chooses.
///
/// `paint` is called directly rather than through `PlayerBlock::render`
/// because the block integrates its own clock, and a test that has to wait
/// for real time to pass is a test that will flake on a loaded machine.
fn frame_with(
    size: (u32, u32),
    peak: f32,
    state: PlayerState,
    phase: f32,
    level: f32,
) -> (Slab, Vec<u8>) {
    let slab = Slab::new(size.0, size.1);
    let levels = smooth_voice(resample(&[peak; 112]));
    let image = paint(&slab, &levels, 0xffbf47, state, phase, level);
    (slab, image.as_bytes(0).expect("one frame").to_vec())
}

/// The same at rest: the take is not moving, so nothing in the glass is.
fn frame_at(size: (u32, u32), peak: f32, state: PlayerState) -> (Slab, Vec<u8>) {
    frame_with(size, peak, state, 0.0, 0.0)
}

fn small() -> (u32, u32) {
    (120, 32)
}

#[test]
fn the_block_is_not_a_disc() {
    let (slab, bytes) = frame_at(small(), 0.0, PlayerState::default());
    // A disc of this height would have ended at x = 1. The block is still
    // glass out here, which is the whole point of the shape.
    let wide = sample(&bytes, &slab, slab.aspect * 0.85, 0.0).3;
    assert!(
        wide > 0,
        "the block stops short of its own width, alpha {wide}"
    );
    // And the corner it has instead of that disc's arc is clear.
    let corner = sample(&bytes, &slab, slab.aspect * 0.99, 0.97).3;
    assert_eq!(corner, 0, "the corner should be clear, alpha {corner}");
}

#[test]
fn the_page_stays_readable_through_the_glass() {
    let (slab, bytes) = frame_at(small(), 0.0, PlayerState::default());
    // A point in the clear air above a silent take's liquid. The player has
    // to be thin enough to read a message through, and thick enough to read
    // as an object.
    let body = sample(&bytes, &slab, 0.0, -0.55).3 as f32 / 255.0;
    assert!(body > 0.25, "the glass is still an object, alpha {body}");
    assert!(
        body < 0.75,
        "the glass is too solid to read through, alpha {body}"
    );
}

#[test]
fn the_seam_moves_while_the_bowl_and_the_dome_hold_still() {
    let size = small();
    let (slab, quiet) = frame_at(size, 0.0, PlayerState::default());
    let (_, loud) = frame_with(size, 0.9, PlayerState::default(), 0.0, 0.9);
    // Just above the horizon, where the band's shoulder sits. A loud take
    // lifts the band onto this row, which is the whole motion: the light
    // travels and the glass around it does not.
    let still = luma(sample(&quiet, &slab, 0.0, -0.10));
    let lifted = luma(sample(&loud, &slab, 0.0, -0.10));
    assert!(
        lifted > still + 20.0,
        "the seam did not move with the take: {still} then {lifted}"
    );
    // A row well inside the dome and one well inside the bowl. Both are
    // pinned: a block that churns from top to bottom reads as a waveform,
    // which is exactly what this material must not do.
    let dome_before = luma(sample(&quiet, &slab, 0.0, -0.62));
    let dome_after = luma(sample(&loud, &slab, 0.0, -0.62));
    let bowl_before = luma(sample(&quiet, &slab, 0.0, 0.62));
    let bowl_after = luma(sample(&loud, &slab, 0.0, 0.62));
    assert!(
        (dome_after - dome_before).abs() < 6.0,
        "the dome moved with the take: {dome_before} then {dome_after}"
    );
    assert!(
        (bowl_after - bowl_before).abs() < 6.0,
        "the bowl moved with the take: {bowl_before} then {bowl_after}"
    );
}

#[test]
fn the_track_fills_as_the_playhead_advances() {
    let size = small();
    let early = PlayerState {
        progress: 0.3,
        playing: true,
        ..PlayerState::default()
    };
    let late = PlayerState {
        progress: 0.95,
        playing: true,
        ..PlayerState::default()
    };
    let (slab, before) = frame_at(size, 0.4, early);
    let (_, after) = frame_at(size, 0.4, late);
    // The right end of the track is ahead of the playhead at 0.3 and behind
    // it at 0.95, so the seam has to brighten there.
    let dim = luma(sample(&before, &slab, slab.aspect * 0.55, 0.0));
    let lit = luma(sample(&after, &slab, slab.aspect * 0.55, 0.0));
    assert!(lit > dim + 12.0, "the track did not fill: {dim} then {lit}");
    // The left end is behind the playhead in both frames and must not move.
    let steady_before = luma(sample(&before, &slab, -slab.aspect * 0.55, 0.0));
    let steady_after = luma(sample(&after, &slab, -slab.aspect * 0.55, 0.0));
    assert!(
        (steady_after - steady_before).abs() < 6.0,
        "the played part changed: {steady_before} then {steady_after}"
    );
}

#[test]
fn the_mark_is_solid_and_follows_playback() {
    let size = small();
    let (slab, paused) = frame_at(size, 0.0, PlayerState::default());
    let (_, playing) = frame_at(
        size,
        0.0,
        PlayerState {
            playing: true,
            ..PlayerState::default()
        },
    );
    // A point inside the play triangle and inside the gap between the two
    // pause bars: covered while paused, clear while playing.
    let mark_x = -slab.aspect + MARK_INSET;
    let held = sample(&paused, &slab, mark_x + 0.02, 0.0).3;
    let released = sample(&playing, &slab, mark_x + 0.02, 0.0).3;
    assert!(held > 200, "the play mark is not solid, alpha {held}");
    assert!(
        held > released + 50,
        "the mark did not change with playback: {held} then {released}"
    );
}

#[test]
fn the_recording_mark_is_a_stop_square() {
    let size = small();
    let state = PlayerState {
        recording: true,
        playing: true,
        progress: 1.0,
        ..PlayerState::default()
    };
    let (slab, bytes) = frame_at(size, 0.0, state);
    let mark_x = -slab.aspect + MARK_INSET;
    // The centre of a stop square is solid, and the same point is a hole in
    // the play triangle, whose base sits behind it.
    let centre = sample(&bytes, &slab, mark_x, 0.0).3;
    assert!(centre > 200, "the stop mark is not solid, alpha {centre}");
}

#[test]
fn the_band_is_the_brightest_event_in_the_body() {
    let (slab, bytes) = frame_at(small(), 0.5, PlayerState::default());
    // The band sits on the horizon, in the middle of the block, and the
    // body is the row well below it.
    let band = luma(sample(&bytes, &slab, 0.0, 0.0));
    let body = luma(sample(&bytes, &slab, 0.0, 0.55));
    assert!(
        band > body + 40.0,
        "the band stopped being the light: band {band}, body {body}"
    );
}

#[test]
fn a_red_microphone_paints_a_red_block() {
    // The recording strip wears the palette's red, and red sits on the far
    // side of the hue seam from the reference amber. Subtracting the two
    // hues directly put the bowl at 0.276, a green block under a red
    // microphone, so the turn has to go the short way round.
    let red = 0xe5484d;
    let hue = warm_hue_for(rgb_hsv(red).0);
    assert!(
        (hue - BOWL_HUE).abs() < 0.05,
        "a red accent moved the bowl to {hue}, away from the glass"
    );
    // And the pixels agree with the hue: deep in the liquid of a red take
    // the warm channel leads, which a green block could not do.
    let mut block = PlayerBlock::at(small(), red, &[0.9; 112]);
    let image = block.render(PlayerState {
        recording: true,
        playing: true,
        progress: 1.0,
        ..PlayerState::default()
    });
    let slab = Slab::new(small().0, small().1);
    let bytes = image.as_bytes(0).expect("one frame");
    let (red, green, _, alpha) = sample(bytes, &slab, 0.0, 0.62);
    assert!(alpha > 40, "there is no liquid to judge, alpha {alpha}");
    assert!(
        red > green,
        "the red microphone painted a block of green: {red} against {green}"
    );
}

#[test]
fn resample_pools_the_decoder_peaks() {
    // Two samples per bucket, one quiet and one loud: the loud one wins, so
    // a short peak between quiet neighbours survives the pooling.
    let mut peaks = Vec::new();
    for _ in 0..VOICE_SAMPLES {
        peaks.push(0.2);
        peaks.push(0.8);
    }
    let levels = resample(&peaks);
    assert!(
        levels.iter().all(|level| (level - 0.8).abs() < 1e-6),
        "the peak of each bucket is not what survived"
    );
    assert_eq!(resample(&[]), [0.0; VOICE_SAMPLES]);
    assert_eq!(resample(&[0.5]), [0.5; VOICE_SAMPLES]);
}
