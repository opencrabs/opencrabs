//! Audio decoding and cached visual metadata for the GUI voice player.
//!
//! Decoding never runs on the UI thread. The first preview requests a visual,
//! a worker decodes the immutable clip once, and the dashboard's normal poll
//! notices the pending count change when the waveform is ready.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecRegistry, DecoderOptions};
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia_adapter_libopus::OpusDecoder;

use crate::logging;

pub(crate) const WAVE_BARS: usize = 48;
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Everything a horizontal preview needs after the one background decode.
pub(crate) struct Visual {
    pub(crate) peaks: Arc<Vec<f32>>,
    pub(crate) duration: Duration,
}

enum CachedVisual {
    Loading,
    Ready(Arc<Visual>),
    Failed,
}

fn visuals() -> &'static Mutex<HashMap<PathBuf, CachedVisual>> {
    static VISUALS: LazyLock<Mutex<HashMap<PathBuf, CachedVisual>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &VISUALS
}

/// Return the decoded waveform, or start its one background load.
pub(crate) fn visual(path: &Path) -> Option<Arc<Visual>> {
    {
        let mut cache = match visuals().lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        match cache.get(path) {
            Some(CachedVisual::Ready(visual)) => return Some(Arc::clone(visual)),
            Some(CachedVisual::Loading | CachedVisual::Failed) => return None,
            None => {
                cache.insert(path.to_path_buf(), CachedVisual::Loading);
            }
        }
    }

    let owned = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("gui-voice-waveform".into())
        .spawn(move || load_visual(owned));
    if let Err(error) = spawned {
        logging::error(&format!("voice waveform thread: {error}"));
        mark_failed(path);
    }
    None
}

fn load_visual(path: PathBuf) {
    let loaded = match decode(&path) {
        Ok((samples, rate)) if !samples.is_empty() && rate > 0 => {
            let duration = Duration::from_secs_f64(samples.len() as f64 / f64::from(rate));
            Some(Arc::new(Visual {
                peaks: Arc::new(peaks(&samples)),
                duration,
            }))
        }
        Ok(_) => {
            logging::error(&format!("voice waveform: {} holds no audio", path.display()));
            None
        }
        Err(reason) => {
            logging::error(&format!("voice waveform: {reason}"));
            None
        }
    };
    let mut cache = match visuals().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache.insert(
        path,
        match loaded {
            Some(visual) => CachedVisual::Ready(visual),
            None => CachedVisual::Failed,
        },
    );
    drop(cache);
    bump_generation();
}

fn mark_failed(path: &Path) {
    let mut cache = match visuals().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache.insert(path.to_path_buf(), CachedVisual::Failed);
    drop(cache);
    bump_generation();
}

/// Every completed load changes this, even a decode fast enough to finish
/// before the poll ever observed its `Loading` state.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The number of visual loads still running. A change is the dashboard's cue
/// to repaint, including the final transition to zero.
pub(crate) fn pending() -> usize {
    let cache = match visuals().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache
        .values()
        .filter(|state| matches!(state, CachedVisual::Loading))
        .count()
}

/// A clip's whole seconds, once its background decode has finished. The paint
/// path never touches the file: an unknown length draws as `--:--` and the
/// generation bump repaints it the moment the real one is known.
pub(crate) fn duration(path: &Path) -> Option<u64> {
    visual(path).map(|visual| visual.duration.as_secs())
}

/// Real peak amplitude per displayed bar, normalized as one clip so a quiet
/// recording remains visible without changing the relative shape.
fn peaks(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let chunk = samples.len().div_ceil(WAVE_BARS).max(1);
    let mut peaks: Vec<f32> = samples
        .chunks(chunk)
        .take(WAVE_BARS)
        .map(|samples| samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs())))
        .collect();
    let loudest = peaks.iter().fold(0.0_f32, |peak, sample| peak.max(*sample));
    if loudest > 0.0 {
        for peak in &mut peaks {
            *peak = (*peak / loudest).sqrt();
        }
    }
    peaks
}

/// Decode any enabled audio container into mono f32 samples at its own rate.
pub(crate) fn decode(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("{} would not open: {e}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("{} is not audio this build can read: {e}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| format!("{} has no audio track", path.display()))?;
    let track_id = track.id;
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_all::<OpusDecoder>();
    let mut decoder = registry
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("{} needs a codec this build does not carry: {e}", path.display()))?;
    let mut mono = Vec::new();
    let mut rate = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(DecodeError::ResetRequired) => break,
            Err(e) => return Err(format!("{} stopped mid-file: {e}", path.display())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{} could not be decoded: {e}", path.display())),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        let interleaved = buffer.samples();
        if channels == 1 {
            mono.extend_from_slice(interleaved);
        } else {
            for frame in interleaved.chunks_exact(channels) {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }
    }
    Ok((mono, rate))
}
