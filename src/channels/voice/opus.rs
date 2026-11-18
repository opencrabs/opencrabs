//! OGG/Opus encoding, shared by the channel voice replies and the window.
//!
//! Telegram, WhatsApp and the rest take a spoken reply as an OGG/Opus file, so
//! every reply the tree speaks is encoded here before it leaves. The window
//! needs the same encoder for the recordings it makes locally, which is why
//! this is its own module behind `any(local-tts, gui)` instead of a private
//! detail of the Piper path: the container and the codec have nothing to do
//! with which voice said the words, and a GUI build that ships without Piper
//! still has to save a playable voice note.
//!
//! The OGG writer below is a minimal RFC 3533 / RFC 7845 one. It exists because
//! the tree needs a few hundred bytes of container code, not a media stack, and
//! the one part that has to be exactly right is the CRC.

use anyhow::Result;

/// Encode i16 PCM samples into an OGG/Opus byte stream (RFC 7845).
///
/// `sample_rate` is the rate of `samples`; Opus accepts 8, 12, 16, 24 and
/// 48 kHz, and the file always plays back at 48 kHz whatever it was recorded
/// at. Public to the crate rather than to the module because the window is its
/// own crate and saves the same voice note the channels send (#3044).
pub fn encode_ogg_opus(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    use opusic_sys::*;

    const FRAME_MS: usize = 20; // 20 ms frames
    let frame_size = (sample_rate as usize * FRAME_MS) / 1000; // 960 samples per frame

    // Create Opus encoder
    let mut error: libc::c_int = 0;
    let encoder = unsafe {
        opus_encoder_create(
            sample_rate as i32,
            1, // mono
            OPUS_APPLICATION_VOIP,
            &mut error,
        )
    };
    if error != OPUS_OK || encoder.is_null() {
        anyhow::bail!("Failed to create Opus encoder: error {error}");
    }

    // Encode all frames
    let mut opus_packets: Vec<Vec<u8>> = Vec::new();
    let mut encode_buf = vec![0u8; 4000]; // max opus frame is ~4000 bytes
    let mut pos = 0;

    while pos + frame_size <= samples.len() {
        let frame = &samples[pos..pos + frame_size];
        let encoded_len = unsafe {
            opus_encode(
                encoder,
                frame.as_ptr(),
                frame_size as libc::c_int,
                encode_buf.as_mut_ptr(),
                encode_buf.len() as i32,
            )
        };
        if encoded_len < 0 {
            unsafe { opus_encoder_destroy(encoder) };
            anyhow::bail!("Opus encode failed: error {encoded_len}");
        }
        opus_packets.push(encode_buf[..encoded_len as usize].to_vec());
        pos += frame_size;
    }

    // Encode remaining samples (pad with silence)
    if pos < samples.len() {
        let mut padded = vec![0i16; frame_size];
        let remaining = &samples[pos..];
        padded[..remaining.len()].copy_from_slice(remaining);
        let encoded_len = unsafe {
            opus_encode(
                encoder,
                padded.as_ptr(),
                frame_size as libc::c_int,
                encode_buf.as_mut_ptr(),
                encode_buf.len() as i32,
            )
        };
        if encoded_len > 0 {
            opus_packets.push(encode_buf[..encoded_len as usize].to_vec());
        }
    }

    unsafe { opus_encoder_destroy(encoder) };

    // Build OGG container (RFC 7845)
    let serial: u32 = rand::random();
    let mut ogg = OggWriter::new(serial);

    // Page 1: OpusHead header
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead"); // magic
    head.push(1); // version
    head.push(1); // channel count (mono)
    head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
    head.extend_from_slice(&sample_rate.to_le_bytes()); // input sample rate
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family
    ogg.write_page(&[&head], 0, OggPageFlag::BOS);

    // Page 2: OpusTags header
    let vendor = b"opencrabs";
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    ogg.write_page(&[&tags], 0, OggPageFlag::None);

    // Audio pages: one Opus packet per OGG page
    let mut granule: u64 = 0;
    for (i, packet) in opus_packets.iter().enumerate() {
        granule += frame_size as u64;
        let flag = if i == opus_packets.len() - 1 {
            OggPageFlag::EOS
        } else {
            OggPageFlag::None
        };
        ogg.write_page(&[packet], granule, flag);
    }

    Ok(ogg.into_bytes())
}

// ─── Minimal OGG page writer (RFC 3533 / RFC 7845) ───────────────────────────

#[derive(Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
enum OggPageFlag {
    None,
    BOS, // beginning of stream
    EOS, // end of stream
}

struct OggWriter {
    serial: u32,
    page_seq: u32,
    buf: Vec<u8>,
}

impl OggWriter {
    fn new(serial: u32) -> Self {
        Self {
            serial,
            page_seq: 0,
            buf: Vec::with_capacity(64 * 1024),
        }
    }

    /// Write one OGG page containing the given segments.
    fn write_page(&mut self, segments: &[&[u8]], granule_pos: u64, flag: OggPageFlag) {
        // Build segment table: each segment max 255 bytes; 255 = continuation, <255 = end
        let mut seg_table = Vec::new();
        for seg in segments {
            let mut remaining = seg.len();
            while remaining >= 255 {
                seg_table.push(255u8);
                remaining -= 255;
            }
            seg_table.push(remaining as u8);
        }

        let header_type = match flag {
            OggPageFlag::BOS => 0x02,
            OggPageFlag::EOS => 0x04,
            OggPageFlag::None => 0x00,
        };

        // Page header (27 bytes + segment table)
        let header_start = self.buf.len();
        self.buf.extend_from_slice(b"OggS"); // capture pattern
        self.buf.push(0); // stream structure version
        self.buf.push(header_type); // header type flag
        self.buf.extend_from_slice(&granule_pos.to_le_bytes()); // granule position
        self.buf.extend_from_slice(&self.serial.to_le_bytes()); // serial number
        self.buf.extend_from_slice(&self.page_seq.to_le_bytes()); // page sequence number
        self.buf.extend_from_slice(&0u32.to_le_bytes()); // CRC placeholder
        self.buf.push(seg_table.len() as u8); // number of segments
        self.buf.extend_from_slice(&seg_table); // segment table

        // Page data
        for seg in segments {
            self.buf.extend_from_slice(seg);
        }

        // Calculate and patch CRC-32
        let page_end = self.buf.len();
        let crc = ogg_crc32(&self.buf[header_start..page_end]);
        let crc_offset = header_start + 22;
        self.buf[crc_offset..crc_offset + 4].copy_from_slice(&crc.to_le_bytes());

        self.page_seq += 1;
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// OGG CRC-32 (polynomial 0x04C11DB7, no pre/post inversion).
pub(crate) fn ogg_crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for i in 0..256u32 {
            let mut r = i << 24;
            for _ in 0..8 {
                r = if r & 0x80000000 != 0 {
                    (r << 1) ^ 0x04C11DB7
                } else {
                    r << 1
                };
            }
            t[i as usize] = r;
        }
        t
    });

    let mut crc: u32 = 0;
    for &byte in data {
        let idx = ((crc >> 24) ^ byte as u32) as usize;
        crc = (crc << 8) ^ table[idx];
    }
    crc
}
