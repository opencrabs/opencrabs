//! Hash computation for hashline editing.
//!
//! Each line in a file gets a [`HASH_LEN`]-character content hash tag computed
//! via FNV-1a. The hash alphabet uses 16 visually-distinct letters (no O/0,
//! I/l confusion), giving `16^HASH_LEN` possible hash values per line.
//!
//! The tag width matters a lot. History, and the reason it is now 6:
//!
//! - A 2-char tag (256 values) made a 20-line read collide ~54% of the time and
//!   a 60-line read ~99.9%, so `hashline_edit` refused most real edits (#573).
//! - A 4-char tag (65,536 values) fixed small files but not large ones: false
//!   collisions grow as `n^2 / (2 * 65536)`, which is ~418 expected pairs in a
//!   7,400-line file. Two logged failures had nine lines sharing one tag at
//!   evenly spaced positions, which is a narrow hash, not identical content.
//! - A 6-char tag (16,777,216 values) drops that to ~1.6 expected pairs in the
//!   same file, for two extra characters per line of `read_file` output (#2202).
//!
//! Identical-content lines (blank lines, a repeated `}`) still share a tag at
//! any width. That is inherent, and it is handled by the collision path, which
//! points the caller at `edit_file`.

/// 16 visually-distinct uppercase letters for the hash alphabet.
/// Excludes: O (looks like 0), I (looks like l/1), A (looks like 4 in some fonts),
/// C (looks like G), D (looks like 0 in some fonts), E (looks like F),
/// G (looks like 6), H (looks like N in some fonts), L (looks like 1).
///
/// Kept: Z P M Q V R W S N K T X J B Y U
/// Actually let's use the same alphabet as oh-my-pi for consistency:
/// Z P M Q V R W S N K T X J B Y H
pub(crate) const HASH_ALPHABET: &[u8; 16] = b"ZPMQVRWSNKTXJBYH";

/// Number of characters in a line-hash tag. Widened from 2 to 4 in #573 (a
/// 2-char tag has only 256 values, so distinct lines collided constantly), and
/// from 4 to 6 in #2202 (65,536 values still collides ~418 times in a
/// 7,400-line file). 6 chars gives 16,777,216 values.
pub(crate) const HASH_LEN: usize = 6;

// The tag is sliced out of a 32-bit FNV-1a hash, one nibble per character, so
// widening past 8 would shift out of the integer and panic at runtime. The
// width is a tuning knob now (#2202), so the limit is checked at compile time.
const _: () = assert!(
    HASH_LEN <= 8,
    "HASH_LEN exceeds the 32 bits of the FNV-1a hash; widen the hash first"
);

/// FNV-1a 32-bit offset basis.
const FNV_OFFSET_BASIS: u32 = 2_166_136_261;

/// FNV-1a 32-bit prime.
const FNV_PRIME: u32 = 16_777_619;

/// Compute FNV-1a 32-bit hash over the given bytes.
fn fnv1a_32(data: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in data {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Compute a [`HASH_LEN`]-character hash for a line.
///
/// The hash is computed over the line content only (stateless).
/// This ensures **reference stability**: inserting or deleting lines
/// at the top of a file does not invalidate hashes for the rest.
///
/// Identical content at different positions produces the same hash.
/// Ambiguity is handled by the edit tool via lazy context escalation
/// (see issue #105).
///
/// # Arguments
/// * `content` - the line content (without newline)
///
/// # Returns
/// A [`HASH_LEN`]-character string from HASH_ALPHABET.
pub fn hash_line(content: &str) -> String {
    let h = fnv1a_32(content.as_bytes());

    // Take one 4-bit nibble per tag character, from the high nibbles down, so
    // the tag uses the most-mixed bits of the FNV hash first.
    let mut tag = String::with_capacity(HASH_LEN);
    for i in 0..HASH_LEN {
        let shift = 4 * (HASH_LEN - 1 - i) as u32;
        let nibble = ((h >> shift) & 0xF) as usize;
        tag.push(HASH_ALPHABET[nibble] as char);
    }
    tag
}

/// Compute hashes for all lines in a file content.
///
/// Returns a Vec of (1-indexed line number, [`HASH_LEN`]-char hash) pairs.
pub fn hash_all_lines(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, hash_line(line)))
        .collect()
}

/// Format a line with its hash tag: `ID|content`
pub fn format_hashline(_line_number: usize, hash: &str, content: &str) -> String {
    format!("{}|{}", hash, content)
}
