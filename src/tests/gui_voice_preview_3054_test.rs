//! Source pins for the shared, seekable voice preview (#3054).
//!
//! The GUI has no headless window harness. These pins protect the wiring and
//! the playback seams; the owner runs the visual and device-level checks.

const EDITOR: &str = include_str!("../gui/editor.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const PREVIEW: &str = include_str!("../gui/voice_preview.rs");
const PLAY: &str = include_str!("../gui/voice_play.rs");
const MEDIA: &str = include_str!("../gui/voice_media.rs");
const DASHBOARD: &str = include_str!("../gui/dashboard_turns.rs");

fn region(source: &'static str, start: &str, end: &str) -> &'static str {
    source
        .split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} is defined"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

#[test]
fn every_audio_surface_uses_the_same_horizontal_player() {
    let history = region(
        TRANSCRIPT,
        "fn history_attachments(",
        "fn attachment_cell(",
    );
    assert!(history.contains("crate::voice_preview::player("));
    assert!(history.contains(".max_w(px(420.0))"));
    assert!(
        history.contains("attachment_view::plan(files.len(), content_width)"),
        "only non-audio files enter the square tile plan"
    );

    let attachments = region(EDITOR, "fn attachment_strip(", "fn paperclip(");
    let audio = attachments
        .find("if attachment.is_audio() {")
        .expect("picked audio has its own player branch");
    let overlay = attachments
        .find("let open_preview =")
        .expect("ordinary attachments retain their overlay");
    assert!(audio < overlay, "audio returns before the overlay is built");
    assert!(attachments.contains("crate::voice_preview::player("));

    let draft = region(
        EDITOR,
        "fn voice_strip(",
        "/// The keybindings the editor needs",
    );
    assert!(draft.contains("crate::voice_preview::player("));
    assert!(draft.contains("Some(draft.seconds)"));
    assert!(draft.contains("&draft.levels"));
}

#[test]
fn the_player_pauses_resumes_and_seeks_by_pointer() {
    assert!(PREVIEW.contains("voice_play::toggle(&toggle_path)"));
    assert!(PREVIEW.contains("voice_play::seek(path, fraction)"));
    assert!(PREVIEW.contains(".on_mouse_move("));
    assert!(PREVIEW.contains(".on_mouse_up_out("));
    assert!(
        PREVIEW.contains("static TRACKS:") && PREVIEW.contains("static DRAG:"),
        "track bounds and drag identity survive repaints"
    );
    assert!(PREVIEW.contains("let played ="), "played bars use the accent");

    assert!(PLAY.contains("sink.pause();"));
    assert!(PLAY.contains("sink.play();"));
    assert!(PLAY.contains("sink.try_seek(duration.mul_f32(fraction))"));
    assert!(PLAY.contains("position: sink.get_pos().min(duration)"));
    assert!(
        PLAY.contains("Command::Stop") && PLAY.contains("pub(crate) fn stop("),
        "discarded audio has a real stop, not another pause toggle"
    );
}

#[test]
fn waveform_decodes_once_off_the_ui_thread_and_repaints_when_ready() {
    assert!(MEDIA.contains(".name(\"gui-voice-waveform\".into())"));
    assert!(
        MEDIA.contains("CachedVisual::Loading")
            && MEDIA.contains("CachedVisual::Ready")
            && MEDIA.contains("CachedVisual::Failed")
    );
    assert!(MEDIA.contains("peaks: Arc::new(peaks(&samples))"));
    assert!(
        !PREVIEW.contains("voice_media::decode("),
        "the paint path never decodes a clip"
    );
    assert!(DASHBOARD.contains("crate::voice_play::paint_state()"));
    assert!(PLAY.contains("pending_visuals: voice_media::pending()"));
    assert!(
        PLAY.contains("visual_generation: voice_media::generation()"),
        "a decode that finishes before the first poll still triggers a repaint"
    );
}

#[test]
fn removing_a_picked_recording_stops_its_playback() {
    let remove = region(EDITOR, "fn remove_attachment(", "pub fn attach_reply(");
    assert!(remove.contains("let removed = self.attachments.remove(index)"));
    assert!(remove.contains("if removed.is_audio()"));
    assert!(remove.contains("crate::voice_play::stop(&removed.path)"));
}
