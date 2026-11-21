//! The instance rail's model: which remote crabs this window can reach, and
//! how fresh the last health check is.
//!
//! Pure data and pure functions, no `gpui` in here, so every rule below is
//! testable without a window. The drawing lives in `instance_strip.rs` and
//! reads this model; nothing in this file knows what a pixel is.
//!
//! Two decisions worth stating, because they are the ones a later change will
//! want to undo:
//!
//! 1. **Order is insertion order, never health.** A strip that reshuffles
//!    while the pointer is travelling towards it is a strip you miss. Health
//!    changes a dot; it never moves a crab.
//! 2. **The label is the owner's, the id is the remote's.** `host_id` comes
//!    from the remote's announcement and is what the store is keyed by. The
//!    label is what the human typed when pairing ("server i7"). Falling back
//!    to the URL host is fine; falling back to the host_id is not, it is
//!    noise nobody recognises.

use opencrabs::remote::instance_store::{InstanceFile, KnownInstance};

/// What the last probe of a remote told us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Never probed, or probed so long ago the answer means nothing.
    Unknown,
    /// Answered `/a2a/health` in `latency_ms`.
    Up { latency_ms: u64 },
    /// Refused, timed out, or rejected our token, at `since`.
    Down { since: u64 },
}

// Test-only: the strip matches on `Health` directly, so these two are dead in
// the bin target and clippy's dead_code pass fires there. `cfg(test)` keeps
// them out of that target instead of silencing the lint with an allow.
#[cfg(test)]
impl Health {
    pub fn is_up(self) -> bool {
        matches!(self, Health::Up { .. })
    }

    pub fn is_down(self) -> bool {
        matches!(self, Health::Down { .. })
    }
}

/// One crab in the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceEntry {
    /// The store record, credentials included. Held whole rather than copied
    /// field by field so there is one source of truth for the token.
    pub known: KnownInstance,
    pub health: Health,
}

impl InstanceEntry {
    pub fn host_id(&self) -> &str {
        &self.known.host_id
    }

    pub fn label(&self) -> &str {
        &self.known.label
    }

    pub fn url(&self) -> &str {
        &self.known.url
    }

    /// What the badge draws: the owner's mark when they set one, otherwise the
    /// initial of the label.
    pub fn glyph(&self) -> String {
        match self.known.glyph.as_deref() {
            Some(chosen) => {
                normalise_glyph(chosen).unwrap_or_else(|| badge_glyph(&self.known.label))
            }
            None => badge_glyph(&self.known.label),
        }
    }
}

/// The set of known remotes plus which one the window is looking at.
#[derive(Debug, Clone, Default)]
pub struct Instances {
    entries: Vec<InstanceEntry>,
    selected: Option<String>,
}

impl Instances {
    /// Build from the credential store. Health starts `Unknown`: the file
    /// says who we may call, not who is up.
    pub fn from_file(file: &InstanceFile) -> Self {
        Self {
            entries: file
                .instances
                .iter()
                .map(|known| InstanceEntry {
                    known: known.clone(),
                    health: Health::Unknown,
                })
                .collect(),
            selected: None,
        }
    }

    // Test-only: the strip asks `entries()` for everything it needs. Kept out
    // of the bin target so clippy's dead_code pass does not fire on helpers the
    // tests legitimately want.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[InstanceEntry] {
        &self.entries
    }

    pub fn get(&self, host_id: &str) -> Option<&InstanceEntry> {
        self.entries.iter().find(|e| e.host_id() == host_id)
    }

    /// Insert or replace a record, keeping its position when it already
    /// exists. Health survives a replace: re-pairing a crab that is up must
    /// not blank its dot until the next probe.
    pub fn upsert(&mut self, known: KnownInstance) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.host_id() == known.host_id)
        {
            existing.known = known;
            return;
        }
        self.entries.push(InstanceEntry {
            known,
            health: Health::Unknown,
        });
    }

    /// Drop a crab. If it was the selected one the selection is cleared, so
    /// the window never renders a panel for an instance that is gone.
    pub fn remove(&mut self, host_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.host_id() != host_id);
        let removed = self.entries.len() != before;
        if removed && self.selected.as_deref() == Some(host_id) {
            self.selected = None;
        }
        removed
    }

    /// Record a probe result. An id we no longer hold is ignored: the probe
    /// was in flight when the crab was removed.
    pub fn record_probe(&mut self, host_id: &str, result: Result<u64, ()>, now: u64) {
        let Some(entry) = self.entries.iter_mut().find(|e| e.host_id() == host_id) else {
            return;
        };
        entry.health = match result {
            Ok(latency_ms) => Health::Up { latency_ms },
            Err(()) => Health::Down { since: now },
        };
    }

    pub fn selected_host_id(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Select a crab we actually hold. Selecting an unknown id is a no-op
    /// rather than an error: the caller is a click handler and has nothing
    /// useful to do with a failure.
    pub fn select(&mut self, host_id: &str) -> bool {
        if self.get(host_id).is_none() {
            return false;
        }
        self.selected = Some(host_id.to_string());
        true
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Move the selection by `delta` places, wrapping. From no selection,
    /// forward lands on the first crab and backward on the last.
    pub fn select_step(&mut self, delta: isize) -> Option<&str> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|id| self.entries.iter().position(|e| e.host_id() == id));
        let next = match current {
            Some(index) => (index as isize + delta).rem_euclid(len as isize) as usize,
            None if delta >= 0 => 0,
            None => len - 1,
        };
        let id = self.entries[next].host_id().to_string();
        self.selected = Some(id);
        self.selected.as_deref()
    }
}

/// The single character a badge wears.
///
/// The badge is a 32 px circle. A ten-character label inside it does not fit
/// and cannot be made to fit: Daniel saw "lenovo" spilling out of the square on
/// the Lenovo. One initial is what a circle can hold, and the full label stays
/// in the hover title, which is where a name too long for the strip belongs.
///
/// Digits and letters both count, so "i7" reads as `I` and a crab named after
/// a machine still gets a stable glyph. Anything with no alphanumeric at all
/// falls back to `?` rather than drawing an empty circle.
pub fn badge_glyph(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// How many distinct washes a badge can wear. Six is what a column of badges
/// shows without two neighbours reading the same at a glance.
pub const BADGE_TINTS: usize = 6;

/// The longest mark a 32 px circle holds. Two characters fit at the badge's
/// text size; a third pushes the pair off centre.
pub const GLYPH_MAX_CHARS: usize = 2;

/// Whether a mark is a stored picture name rather than characters to draw.
///
/// A crab's mark and a topic's icon are the same pick: the topic pack's names
/// (`topic:<name>`) and a custom image's (`img:<file>`). Both are long strings
/// on purpose, and both mean "draw this picture" to the badge.
pub fn glyph_is_picture(glyph: &str) -> bool {
    let glyph = glyph.trim();
    glyph.starts_with(crate::topic_icons::TOPIC_PREFIX)
        || glyph.starts_with(crate::topic_icons::IMG_PREFIX)
}

/// Clean what the owner typed into a badge mark.
///
/// `None` means "nothing usable, derive it": the field was left empty, or what
/// is in it is whitespace. Two characters is the cap, because the square is
/// 32 px and the badge's text size is fixed; the caller never has to think
/// about that, and a paste of a whole word cannot break the strip.
///
/// A picture name passes through whole. It is not a mark the square has to
/// hold: it is a file the badge draws, and cutting `topic:crab_code` at two
/// characters would store `to` and paint nothing at all.
pub fn normalise_glyph(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if glyph_is_picture(trimmed) {
        // A picture name is a mark only if the badge can draw it. The strip
        // renders the embedded pack's bytes and nothing else, so a name that
        // resolves to no icon (a half-erased `topic:crab_cl`, or any `img:`
        // file, which the strip has no cache to decode) would be painted as
        // its own raw string inside a 32 px square. Falling back to the
        // label's initial is the honest mark.
        let drawable = crate::topic_icons::topic_icon(trimmed)
            .or_else(|| crate::topic_icons::crab_icon(trimmed))
            .is_some();
        return drawable.then(|| trimmed.to_string());
    }
    let mut cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .take(GLYPH_MAX_CHARS)
        .collect();
    // The cap counts code points, but a reader sees graphemes: cutting a
    // multi-codepoint icon at two chars can leave the mark ending on a zero
    // width joiner or a variation selector, an invisible tail that renders the
    // badge off-centre or as a box. Dropping the tail is what makes "two
    // characters" a promise about what shows, not about what is stored.
    while cleaned
        .chars()
        .last()
        .is_some_and(|c| matches!(c, '\u{200d}' | '\u{fe0f}' | '\u{fe0e}'))
    {
        cleaned.pop();
    }
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// A stable colour slot for a crab, from its `host_id`.
///
/// FNV-1a, because it is four lines and it never moves: the same crab is the
/// same colour across runs, restarts and machines, which is the whole point.
/// The finaliser is not decoration: FNV's low bits are weak, and `hash % 6` on
/// the raw value put six different ids in the same slot in the first draft. The
/// multiply-xor step spreads them before the modulo. Nothing here knows a
/// palette; the slot is what the strip maps to a hue.
pub fn badge_tint(host_id: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in host_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    (hash % BADGE_TINTS as u64) as usize
}

/// "last seen" in the panel, from a pair of unix timestamps.
pub fn age_label(now: u64, ts: u64) -> String {
    if ts == 0 {
        return "nunca".to_string();
    }
    let secs = now.saturating_sub(ts);
    match secs {
        0..=59 => "agora".to_string(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// The one-character mark on the badge. The view paints the colour.
pub fn health_mark(health: Health) -> &'static str {
    match health {
        Health::Up { .. } => "●",
        Health::Down { .. } => "✕",
        Health::Unknown => "○",
    }
}

/// Hover/tooltip text for a badge: label, URL and what the probe said.
pub fn badge_title(entry: &InstanceEntry, now: u64) -> String {
    let state = match entry.health {
        Health::Up { latency_ms } => format!("online, {latency_ms} ms"),
        Health::Down { since } => format!("offline há {}", age_label(now, since)),
        Health::Unknown => "por verificar".to_string(),
    };
    format!("{} ({}) , {}", entry.label(), entry.url(), state)
}
