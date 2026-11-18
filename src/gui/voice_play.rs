//! Playback for the window's voice messages (#3044).
//!
//! A voice note is an OGG/Opus file: that is what a channel would have
//! delivered, and it is what the window saves, so the card in the transcript is
//! the same artifact the agent received. Nothing on the desktop plays OGG/Opus
//! out of the box, so this module decodes it with symphonia and hands a WAV to
//! the system player, the route `preview_voice` already takes for the Piper
//! sample.
//!
//! One file sounds at a time, and the global below is the whole of that state:
//! a card asks whether its own path is the one playing, and pressing it again
//! stops it. The thread that owns the player clears the slot when the clip ends
//! on its own, so a card can never stay stuck showing "playing".

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::voice_input;

/// How often the playback thread looks at the stop flag and the player.
const POLL: Duration = Duration::from_millis(40);

/// Opus always reports its granule position at 48 kHz, whatever the file was
/// resampled from, so a duration is a division by this and nothing else.
const OPUS_GRANULE_RATE: u64 = 48_000;

/// The file currently sounding, if any.
struct Active {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

/// The one player. A `Mutex` around an `Option` is enough: pressing a card is a
/// UI-thread event, and the only other writer is the thread that owns the clip.
fn slot() -> &'static Mutex<Option<Active>> {
    static SLOT: Mutex<Option<Active>> = Mutex::new(None);
    &SLOT
}

/// Whether `path` is the file that is sounding right now.
pub(crate) fn is_playing(path: &Path) -> bool {
    match slot().lock() {
        Ok(guard) => guard.as_ref().is_some_and(|active| active.path == path),
        Err(_) => false,
    }
}

/// The file that is sounding right now, if any (#3044).
///
/// A clip that ends by itself clears the slot with no event behind it, so the
/// window asks this on its poll tick and repaints when the answer changes.
/// Without it a card that finished playing would keep showing a stop square
/// over a silence, which is a control that no longer does what it says.
pub(crate) fn playing_path() -> Option<PathBuf> {
    match slot().lock() {
        Ok(guard) => guard.as_ref().map(|active| active.path.clone()),
        Err(poisoned) => poisoned
            .into_inner()
            .as_ref()
            .map(|active| active.path.clone()),
    }
}

/// Play `path`, or stop it when it is already the one playing.
pub(crate) fn toggle(path: &Path) -> Result<(), String> {
    // Take whatever is sounding out of the slot first. Doing it in one step is
    // what keeps the old thread from clearing an entry that is already the new
    // clip's: it compares the stop flag it was given, not just the path.
    let previous = match slot().lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(active) = previous {
        active.stop.store(true, Ordering::Relaxed);
        if active.path == path {
            // A second press on the same card is a stop, and nothing restarts.
            return Ok(());
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    match slot().lock() {
        Ok(mut guard) => {
            *guard = Some(Active {
                path: path.to_path_buf(),
                stop: Arc::clone(&stop),
            });
        }
        Err(poisoned) => {
            *poisoned.into_inner() = Some(Active {
                path: path.to_path_buf(),
                stop: Arc::clone(&stop),
            });
        }
    }

    let owned = path.to_path_buf();
    std::thread::Builder::new()
        .name("gui-voice-play".into())
        .spawn(move || run(owned, stop))
        .map_err(|e| format!("the playback thread did not start: {e}"))?;
    Ok(())
}

/// Stop whatever is sounding. Inert when nothing is.
pub(crate) fn stop() {
    let previous = match slot().lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(active) = previous {
        active.stop.store(true, Ordering::Relaxed);
    }
}

/// How long the clip is, in seconds, read from the OGG's own granule positions.
///
/// The container carries the sample count at the end of the stream, so the card
/// can print a duration without decoding a packet. Pages are walked rather than
/// searched for, because an Opus packet is binary and may well contain the
/// capture pattern by chance.
///
/// Memoised, because the card that prints it is painted on every frame the
/// transcript draws: reading a whole recording per frame would be the same
/// mistake #2805 removed from the row text. A clip is written once and never
/// changes, so the first read is the only read.
pub(crate) fn duration(path: &Path) -> Option<u64> {
    if let Ok(cache) = lengths().lock()
        && let Some((_, known)) = cache.iter().find(|(cached, _)| cached == path)
    {
        return *known;
    }
    let seconds = read_duration(path);
    let entry = (path.to_path_buf(), seconds);
    match lengths().lock() {
        Ok(mut cache) => cache.push(entry),
        Err(poisoned) => poisoned.into_inner().push(entry),
    }
    seconds
}

/// The lengths already read. A `Vec` rather than a map: a conversation shows a
/// handful of clips, and a linear walk over a handful is cheaper than hashing.
fn lengths() -> &'static Mutex<Vec<(PathBuf, Option<u64>)>> {
    static LENGTHS: Mutex<Vec<(PathBuf, Option<u64>)>> = Mutex::new(Vec::new());
    &LENGTHS
}

/// Walk the OGG pages and take the last granule position.
fn read_duration(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut at = 0usize;
    let mut granule = 0u64;
    while at + 27 <= bytes.len() && &bytes[at..at + 4] == b"OggS" {
        let segments = bytes[at + 26] as usize;
        let table = bytes.get(at + 27..at + 27 + segments)?;
        let body: usize = table.iter().map(|n| *n as usize).sum();
        granule = u64::from_le_bytes(bytes.get(at + 6..at + 14)?.try_into().ok()?);
        at += 27 + segments + body;
    }
    if granule == 0 {
        return None;
    }
    Some(granule / OPUS_GRANULE_RATE)
}

/// The playback thread: decode, hand the player a WAV, wait, clean up.
fn run(path: PathBuf, stop: Arc<AtomicBool>) {
    if let Err(reason) = play(&path, &stop) {
        logging::error(&format!("voice playback: {reason}"));
    }
    // Clear the slot only when it is still this clip: a press on another card
    // while this one was finishing must not be wiped out by it.
    if let Ok(mut guard) = slot().lock() {
        let ours = guard
            .as_ref()
            .is_some_and(|active| active.path == path && Arc::ptr_eq(&active.stop, &stop));
        if ours {
            *guard = None;
        }
    }
}

fn play(path: &Path, stop: &Arc<AtomicBool>) -> Result<(), String> {
    let (samples, rate) = decode(path)?;
    if samples.is_empty() || rate == 0 {
        return Err(format!("{} holds no audio", path.display()));
    }
    // The decoded samples go out as the same PCM16 WAV the recorder produces,
    // so one writer serves both directions and the player gets a format every
    // platform already understands.
    let wav = voice_input::wav_bytes(&samples, rate);
    let temp = std::env::temp_dir().join(format!(
        "opencrabs-voice-{}-{}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::write(&temp, &wav)
        .map_err(|e| format!("the decoded audio would not be written: {e}"))?;

    let mut child = spawn_player(&temp)?;
    loop {
        // A card pressed twice has to go quiet at once, not when the clip ends.
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                let _ = child.kill();
                let _ = std::fs::remove_file(&temp);
                return Err(format!("the player could not be watched: {e}"));
            }
        }
    }
    let _ = child.wait();
    let _ = std::fs::remove_file(&temp);
    Ok(())
}

/// Hand the file to the platform's own player.
///
/// `afplay` on macOS and the WAV `SoundPlayer` on Windows; elsewhere `paplay`,
/// which PulseAudio and PipeWire both provide. A spawn failure is reported
/// rather than swallowed: silence with no explanation is the one outcome the
/// user cannot debug.
fn spawn_player(file: &Path) -> Result<Child, String> {
    let mut command = if cfg!(target_os = "windows") {
        let mut command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                file.display()
            ),
        ]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("afplay");
        command.arg(file);
        command
    } else {
        let mut command = Command::new("paplay");
        command.arg(file);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("no audio player answered: {e}"))
}

/// Decode the file into mono f32 samples at its own rate.
fn decode(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("{} would not open: {e}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
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

    // The default registry covers the containers' own codecs (PCM, Vorbis) and
    // libopus is registered on top of it: symphonia has no first-party Opus
    // decoder, and Opus is the only codec a voice note ever uses.
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_all::<OpusDecoder>();
    let mut decoder = registry
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| {
            format!(
                "{} needs a codec this build does not carry: {e}",
                path.display()
            )
        })?;

    let mut mono: Vec<f32> = Vec::new();
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
            // One damaged packet is not a reason to refuse the whole clip.
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{} could not be decoded: {e}", path.display())),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        // A fresh buffer per packet: packet sizes vary, and a buffer sized for
        // the first one would be the wrong size for the next.
        // SampleBuffer counts a duration in frames, so the decoder's capacity
        // has to cross from usize into u64. A packet never gets near the
        // width of either type, so the cast cannot lose a frame.
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
