//! The spoken summary of a reply.
//!
//! A voice message deserves a voice answer, but a voice answer must not be the
//! answer. The written reply is a report: headings, paths, tables, the lot. Read
//! aloud it takes minutes and lands as noise. So the model writes the report as
//! it always did and appends one short block that is meant for the ear:
//!
//! ```text
//! Finished the composer takeover, all committed. One thing I need from you:
//! should the discard button stay outside the glass?
//! <<SPOKEN:Composer takeover is done and committed. I need one answer from
//! you: should the discard button stay outside the glass?>>
//! ```
//!
//! This module owns both halves of that shape so six surfaces cannot drift
//! apart: [`split`] takes the block out of the text and hands it back, and
//! [`visible`] is the same thing for the callers that only draw.
//!
//! The marker reuses the `<<KIND:payload>>` form the transcript already knows
//! (`<<IMG:>>`, `<<VID:>>`, `<<AUD:>>`), so `editor::strip_markers` drops it
//! from a reloaded row with no second parser to keep in step. It is a marker
//! and not a fenced tag on purpose: one token has no terminator to lose, and a
//! half written fence cannot swallow the rest of the answer.

use crate::channels::voice::text_cleanup::clean_for_tts;
use std::borrow::Cow;

/// The most a spoken reply may say, in characters.
///
/// Two to four sentences is the target; this is the ceiling for a model that
/// ignores the instruction. At roughly 150 characters a sentence that is still
/// about half a minute of speech, which is already long for a chat reply.
pub const MAX_SPOKEN_CHARS: usize = 600;

/// Where a spoken block opens. The payload runs to the first `>>`.
const OPEN: &str = "<<SPOKEN:";

/// The end of every marker, spoken or not.
const CLOSE: &str = ">>";

/// What the agent is told about a message that began as speech.
///
/// The transcript arrives as plain words, indistinguishable from typing, and a
/// model that cannot tell the two apart answers a voice note in text. This
/// prefix is the only signal it gets, so every surface that transcribes one has
/// to add it (#3092).
pub const VOICE_PREFIX: &str = "[voice note]";

/// Mark a message as having come from a recording.
///
/// Added at the edge, after the transcript is in hand and before the turn
/// starts, so the marker never reaches a screen or the stored history. It is
/// idempotent because the Telegram transcription path writes its own note into
/// the text first: marking that one twice would tell the model nothing new and
/// clutter the prompt with a duplicate.
pub fn voice_prefix(text: &str) -> String {
    let marked = text
        .get(..VOICE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(VOICE_PREFIX));
    if marked {
        text.to_string()
    } else {
        format!("{VOICE_PREFIX} {text}")
    }
}

/// The words a marked message carries, without the note in front of them.
///
/// The twin of [`voice_prefix`], for the surfaces that DRAW a message instead
/// of sending it (#3092). The note exists so a model can tell a transcript from
/// typing; the person who spoke the words already knows, so every rendered
/// surface strips it. A resumed turn is where this matters: the ledger row a
/// channel wrote carries the note in its content, and the card draws that row.
///
/// Borrowed, so a paint path that asks on every frame copies nothing.
pub fn without_prefix(text: &str) -> &str {
    let marked = text
        .get(..VOICE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(VOICE_PREFIX));
    if !marked {
        return text;
    }
    let rest = text
        .get(VOICE_PREFIX.len()..)
        .unwrap_or_default()
        .trim_start();
    // The Telegram path writes `[voice note]: words`, so the separator belongs
    // to the note rather than to the sentence.
    rest.strip_prefix(':').unwrap_or(rest).trim_start()
}

/// What the model is told when a message came from a recording (#3222).
///
/// This used to sit in the system preamble, which is sent on every turn. A
/// typed question then paid for the rule, and some models obeyed it and wrote a
/// block nobody would ever play. It rides with the user message instead, and
/// only when that message carries [`VOICE_PREFIX`], so a typed turn never sees
/// it.
pub const SPOKEN_INSTRUCTION: &str = "\
VOICE TURNS GET A SPOKEN SUMMARY: this message began with `[voice note]`, so it was spoken, and it is answered with speech as well: end your reply with one `<<SPOKEN:...>>` block holding two to four plain sentences, one for what you did and one for what the user has to do next, in the language they wrote in. No markdown, no file paths, no lists, no code, and never a `>>` inside it, because that closes the block early. Everything above the block stays exactly as full as the work deserves: the block is what gets read aloud, the rest is what gets read. The block is stripped from every surface before display, so it never reaches a screen, and a reply that arrives without one is spoken from its first paragraph instead.";

/// Whether a turn's message came from a recording (#3222).
///
/// The note opens the transcript, but a channel puts its own banner in front of
/// the text, so the note can sit on a later line. Any line that opens with it
/// counts. A mention in the middle of a sentence does not: that is someone
/// talking about voice notes, not sending one.
pub fn is_voice_turn(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .get(..VOICE_PREFIX.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(VOICE_PREFIX))
    })
}

/// Split a reply into what is read and what is spoken.
///
/// Returns the visible answer with the block removed and its blank lines
/// collapsed, plus the spoken text when the reply carried one. `None` on the
/// second half is the ordinary case: most replies have no block, either because
/// the turn was not a voice turn or because the model left it out. The caller
/// decides what to do then, and saying nothing is a reasonable answer.
///
/// An unterminated block leaves the text exactly as it was. A reply is more
/// valuable than a tidy parse, so a model that forgot the `>>` gets its words
/// through with the marker visible rather than an empty message.
pub fn split(text: &str) -> (String, Option<String>) {
    let Some((start, end)) = locate(text) else {
        return (collapse(text), None);
    };
    let head = text.get(..start).unwrap_or_default();
    let tail = text.get(end..).unwrap_or_default();
    let joined = join_block(head, tail);
    let visible = collapse(&joined);
    let payload = text
        .get(start + OPEN.len()..end - CLOSE.len())
        .unwrap_or_default();
    let spoken = flatten(payload);
    if spoken.is_empty() {
        return (visible, None);
    }
    (visible, Some(cap(spoken)))
}

/// The reply as the reader sees it, block and all removed.
pub fn visible(text: &str) -> String {
    split(text).0
}

/// The text a surface may DRAW while the reply is still streaming (#3222).
///
/// [`split`] removes a block from a finished reply, and it cannot help here:
/// for most of a turn the block has no closing `>>` yet, so it is not a block
/// by that parse and the raw `<<SPOKEN:` lands on screen in the card and in the
/// bubble until the rows reload. This hides the three shapes a stream produces:
///
/// - a closed block, the same range [`locate`] returns;
/// - an unclosed one, everything from its opener to the end of the stream;
/// - a trailing partial opener (`<<SPOK`, `<<SPOKE`, ...), a block whose name
///   has not finished arriving.
///
/// Borrowed when there is nothing to hide, so a paint path that asks on every
/// frame copies nothing in the ordinary case. The raw stream is never touched:
/// the synthesizer still finds the block when the turn ends.
pub fn while_streaming(text: &str) -> Cow<'_, str> {
    let Some(start) = block_start(text) else {
        return Cow::Borrowed(text);
    };
    let head = text.get(..start).unwrap_or_default();
    let Some(end) = close_of(text, start) else {
        // Unclosed or still arriving: everything from the opener is the block.
        return Cow::Borrowed(head.trim_end());
    };
    let joined = join_block(head, text.get(end..).unwrap_or_default());
    // A model that appended twice leaves a second block behind the first, and
    // each pass strictly shortens the text, so this cannot run away.
    Cow::Owned(match block_start(&joined) {
        Some(_) => while_streaming(&joined).into_owned(),
        None => joined,
    })
}

/// Where a spoken block opens in `text`, when one is there or still arriving.
///
/// [`locate`] needs the closing `>>`, which a block spends most of its life
/// without. This finds the opener alone, and it matches a partial one too.
fn block_start(text: &str) -> Option<usize> {
    let mut offset = 0usize;
    while let Some(found) = text.get(offset..)?.find("<<") {
        let start = offset + found;
        if opens_spoken(text.get(start + 2..).unwrap_or_default()) {
            return Some(start);
        }
        offset = start + 2;
    }
    None
}

/// Whether the text right after `<<` reads as a `SPOKEN:` kind.
///
/// Case-insensitive, per character, and true when the stream runs out before
/// the kind does: a partial token is still our block, and the bytes already on
/// screen are the ones that must not be.
fn opens_spoken(after: &str) -> bool {
    const KIND: &str = "SPOKEN:";
    let mut expected = KIND.chars();
    for ch in after.chars() {
        let Some(want) = expected.next() else {
            // More arrived than a kind holds, so this is `<<SPOKENX...`.
            return false;
        };
        if !ch.eq_ignore_ascii_case(&want) {
            return false;
        }
        if want == ':' {
            return true;
        }
    }
    true
}

/// The byte just past the closing `>>` of the block that opens at `start`.
fn close_of(text: &str, start: usize) -> Option<usize> {
    let after = text.get(start + 2..)?;
    let rel = after.find(CLOSE)?;
    Some(start + 2 + rel + CLOSE.len())
}

/// Join the text before a block to the text after it.
///
/// A block belongs on its own line, and that is where the model puts it. If one
/// arrives mid sentence instead, the hole it leaves is closed with a space
/// rather than a line break, because a stray break in the middle of a paragraph
/// is more visible than the join.
fn join_block(head: &str, tail: &str) -> String {
    if head.is_empty() {
        // Nothing came before the block, so there is no paragraph to rejoin and
        // no space of its own to keep.
        tail.trim_start().to_string()
    } else if tail.is_empty() {
        // The block ended the reply, so the answer ends where the block began.
        // The blank lines that separated the two are not part of the answer, and
        // a card that kept them drew a hole under its last line.
        head.trim_end().to_string()
    } else if head.ends_with('\n') || tail.starts_with('\n') {
        format!("{head}\n{tail}")
    } else {
        format!("{} {}", head.trim_end(), tail.trim_start())
    }
}

/// What to speak when the model left the block out.
///
/// The first paragraph is the sentence the model led with, which is the closest
/// thing to a summary an answer contains. Capped by the same rule, so a
/// forgotten block costs a short clip rather than a five minute one: a voice
/// turn is answered in voice either way, and this is what keeps that promise
/// when the instruction was missed.
pub fn first_paragraph(text: &str) -> String {
    let lead = text
        .split("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty())
        .unwrap_or_default();
    if lead.is_empty() {
        cap(flatten(text))
    } else {
        cap(flatten(lead))
    }
}

/// The byte range of the whole block, opening token through closing `>>`.
///
/// The kind is matched without case because a model writing `<<spoken:` means
/// the same thing, and a block left in the text is worse than a lenient parse.
fn locate(text: &str) -> Option<(usize, usize)> {
    let mut rest = text;
    let mut offset = 0usize;
    while let Some(found) = rest.find("<<") {
        let start = offset + found;
        let after = text.get(start + 2..)?;
        let end = after.find(CLOSE)?;
        let marker = after.get(..end)?;
        let is_block = marker
            .split_once(':')
            .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case("SPOKEN"));
        if is_block {
            return Some((start, start + 2 + end + CLOSE.len()));
        }
        offset = start + 2 + end + CLOSE.len();
        rest = text.get(offset..)?;
    }
    None
}

/// One line of plain speech, ready to hand to a synthesizer.
///
/// The shared cleaner already flattens newlines, drops code fences, and strips
/// the markdown a speaker would otherwise pronounce.
///
/// A payload cannot carry `>>` by construction: [`locate`] ends the block at
/// the first one it finds, which is exactly why the instruction to the model
/// forbids it inside the text.
fn flatten(payload: &str) -> String {
    clean_for_tts(payload).trim().to_string()
}

/// Keep the first [`MAX_SPOKEN_CHARS`] characters, ending on a sentence.
///
/// A cut mid clause sounds like a dropped call, so the trim lands on the last
/// full stop that fits. Failing that it lands on the last space, and failing
/// that it cuts where it must. Cutting is worth a warning: it means the model
/// ignored the instruction, and the log is the only place that shows up.
fn cap(text: String) -> String {
    if text.chars().count() <= MAX_SPOKEN_CHARS {
        return text;
    }
    let limit = text
        .char_indices()
        .nth(MAX_SPOKEN_CHARS)
        .map_or(text.len(), |(index, _)| index);
    let head = text.get(..limit).unwrap_or_default();
    let cut = head
        .char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '.' | '!' | '?'))
        .map(|(index, ch)| index + ch.len_utf8())
        .or_else(|| head.rfind(char::is_whitespace))
        .unwrap_or(head.len());
    let kept = text.get(..cut).unwrap_or_default().trim_end();
    let kept = if kept.is_empty() {
        head.trim_end()
    } else {
        kept
    };
    tracing::warn!(
        chars = text.chars().count(),
        kept = kept.chars().count(),
        "spoken summary ran past the cap and was cut"
    );
    kept.to_string()
}

/// The text with runs of blank lines squeezed to one and both ends trimmed.
///
/// Pulling a token out of the middle of an answer leaves a hole where it was,
/// and a model tends to leave two blank lines around the block anyway. A row
/// that keeps both is a row with a gap in it.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blanks > 0 {
                out.push('\n');
            }
        }
        blanks = 0;
        out.push_str(line.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply with no block comes back as it was. Most replies have none, so a
    /// parser that reshaped them would rewrite the history of the app.
    #[test]
    fn a_reply_without_a_block_is_returned_as_it_was() {
        let text = "All committed.\n\nNothing to do next.";
        let (visible, spoken) = split(text);
        assert_eq!(visible, text);
        assert_eq!(spoken, None);
    }

    /// The block sits at the end, which is where the instruction asks for it:
    /// the words survive untouched and the payload comes back on one line,
    /// because a synthesizer reads a line break as a pause.
    #[test]
    fn a_block_at_the_end_leaves_the_answer_and_the_payload() {
        let text = "Shipped it.\n\nOne thing left.\n\n\
                    <<SPOKEN:Shipped it. I need one answer from you.>>";
        let (visible, spoken) = split(text);
        assert_eq!(visible, "Shipped it.\n\nOne thing left.");
        assert_eq!(
            spoken.as_deref(),
            Some("Shipped it. I need one answer from you.")
        );
    }

    /// A block written mid sentence leaves one clean answer. A hole in the
    /// paragraph is what a naive removal produces, and it is more visible than
    /// the block was.
    #[test]
    fn a_block_in_the_middle_leaves_one_clean_answer() {
        let text = "Done <<SPOKEN:Done.>> and pushed.";
        let (visible, spoken) = split(text);
        assert_eq!(visible, "Done and pushed.");
        assert_eq!(spoken.as_deref(), Some("Done."));
    }

    /// An empty block asks for nothing. `None` sends the caller to its
    /// fallback, because silence is the one answer a voice turn must not give.
    #[test]
    fn an_empty_block_asks_for_nothing() {
        let (visible, spoken) = split("Done.\n<<SPOKEN:   >>");
        assert_eq!(visible, "Done.");
        assert_eq!(spoken, None);
    }

    /// Markers of another kind belong to the transcript. Removing one here
    /// would drop an attachment from the row that draws it.
    #[test]
    fn another_kind_of_marker_is_left_alone() {
        let text = "<<AUD:/tmp/take.ogg>>";
        let (visible, spoken) = split(text);
        assert_eq!(visible, text);
        assert_eq!(spoken, None);
    }

    /// A model that forgets the closing `>>` still gets its words through. A
    /// half written marker is a worse outcome than a visible one.
    #[test]
    fn an_unterminated_block_keeps_the_words() {
        let text = "Done.\n<<SPOKEN:Done.";
        let (visible, spoken) = split(text);
        assert_eq!(visible, text);
        assert_eq!(spoken, None);
    }

    /// A payload past the cap is cut on a full stop, because a clip that stops
    /// mid clause sounds like a dropped call rather than a short answer.
    #[test]
    fn a_long_payload_is_cut_at_a_sentence() {
        let payload = format!("{}. {}", "A".repeat(300), "B".repeat(300));
        let (_, spoken) = split(&format!("Done.\n<<SPOKEN:{payload}>>"));
        let spoken = spoken.expect("a capped payload is still spoken");
        assert!(
            spoken.ends_with('.'),
            "the cut must land on the sentence: {spoken:?}"
        );
        assert!(!spoken.contains('B'), "nothing past the cut may survive");
        assert_eq!(spoken.chars().count(), 301);
    }

    /// The cut is measured in characters, not bytes. Slicing a Portuguese or
    /// CJK payload on a byte offset panics instead of speaking.
    #[test]
    fn a_long_payload_in_another_alphabet_is_not_cut_mid_character() {
        let payload = "é".repeat(700);
        let (_, spoken) = split(&format!("<<SPOKEN:{payload}>>"));
        let spoken = spoken.expect("a capped payload is still spoken");
        assert_eq!(spoken.chars().count(), MAX_SPOKEN_CHARS);
        assert!(spoken.chars().all(|ch| ch == 'é'));
    }

    /// The token ends at the first `>>`, so a payload cannot smuggle one in and
    /// run past its own block.
    #[test]
    fn a_payload_cannot_run_past_its_own_close() {
        let text = "<<SPOKEN:first>> second>>";
        let (visible, spoken) = split(text);
        assert_eq!(spoken.as_deref(), Some("first"));
        assert_eq!(visible, "second>>");
    }

    /// `visible` is the same parse with the payload thrown away, so a surface
    /// that only draws cannot print the block by mistake.
    #[test]
    fn visible_is_the_answer_alone() {
        assert_eq!(visible("Done.\n\n<<SPOKEN:Done.>>"), "Done.");
    }

    /// A transcript that already carries the note is not marked twice. The
    /// Telegram transcription path writes its own, and a second one would make
    /// the prompt look like two voice notes arrived.
    #[test]
    fn a_voice_note_is_marked_once() {
        assert_eq!(voice_prefix("turn it up"), "[voice note] turn it up");
        assert_eq!(
            voice_prefix("[voice note] turn it up"),
            "[voice note] turn it up"
        );
    }

    /// The note comes off wherever a message is drawn (#3092). A reader who
    /// spoke the words never needs to be told they were spoken, and a card that
    /// printed the note was the leak this test pins.
    #[test]
    fn a_drawn_message_loses_its_note() {
        assert_eq!(without_prefix("[voice note] turn it up"), "turn it up");
        assert_eq!(without_prefix("[Voice note]: turn it up"), "turn it up");
        assert_eq!(without_prefix("turn it up"), "turn it up");
        // A message that only looks similar keeps every word it has.
        assert_eq!(
            without_prefix("[voice notes] two takes"),
            "[voice notes] two takes"
        );
    }

    /// #3222: the block never reaches a screen, whatever state the stream is in.
    ///
    /// A stream is a range that grows, so this walks every prefix of a reply
    /// that carries a block. At no point may the drawn text hold the opener:
    /// that was the whole bug, the block stayed visible for the seconds it took
    /// to arrive.
    #[test]
    fn a_streaming_block_never_reaches_the_screen_in_any_state() {
        let full = "Shipped it.\n\n<<SPOKEN:Shipped it. I need one answer from you.>>";
        for cut in 0..=full.len() {
            let head = full.get(..cut).unwrap_or_default();
            let expected = match head.find("<<") {
                Some(at) => head.get(..at).unwrap_or_default().trim_end(),
                None => head,
            };
            assert_eq!(while_streaming(head), expected, "cut {cut}");
        }
    }

    /// The parse is case-insensitive, so a model writing `<<spoken:` gets the
    /// same treatment as one writing the token in capitals.
    #[test]
    fn a_lowercase_block_is_hidden_while_it_streams() {
        assert_eq!(while_streaming("Done.\n<<spoken:Done.>>"), "Done.");
        assert_eq!(while_streaming("Done.\n<<SpOkEn:Don"), "Done.");
    }

    /// A block written mid answer leaves one clean sentence, the same join
    /// [`split`] makes once the reply settles.
    #[test]
    fn a_closed_block_mid_answer_leaves_one_sentence() {
        assert_eq!(
            while_streaming("Done <<SPOKEN:Done.>> and pushed."),
            "Done and pushed."
        );
    }

    /// A reply with nothing to hide is borrowed, so the paint path that asks on
    /// every frame copies nothing in the ordinary case.
    #[test]
    fn streaming_text_without_a_block_is_left_alone() {
        let text = "Reading the parser, then I will report.";
        assert!(matches!(while_streaming(text), Cow::Borrowed(_)));
        assert_eq!(while_streaming(text), text);
    }

    /// Other markers belong to the transcript, and a `<<` that opens nothing is
    /// ordinary text. Hiding either would drop an attachment or eat a shift.
    #[test]
    fn a_marker_that_is_not_spoken_is_not_hidden() {
        let attachment = "Take a look <<AUD:/tmp/take.ogg";
        assert_eq!(while_streaming(attachment), attachment);
        let shift = "x << 2 shifts";
        assert_eq!(while_streaming(shift), shift);
    }

    /// #3222: the rule rides with the message, so detection has to survive the
    /// banner a channel puts in front of the transcript.
    #[test]
    fn voice_turn_is_detected_behind_a_channel_banner() {
        assert!(is_voice_turn("[voice note] turn it up"));
        assert!(is_voice_turn("[Voice note]: turn it up"));
        assert!(is_voice_turn(
            "Platform: Group / Dev [chat:1]\n\n[voice note] turn it up"
        ));
        assert!(is_voice_turn("[voice note] áéí óú"));
    }

    /// A typed turn is not a voice turn, and neither is a sentence that only
    /// talks about the note.
    #[test]
    fn typed_turn_is_not_a_voice_turn() {
        assert!(!is_voice_turn("turn it up"));
        assert!(!is_voice_turn("why does the [voice note] prefix exist?"));
        assert!(!is_voice_turn("[voice notes] two takes"));
    }
}
