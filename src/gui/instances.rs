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

/// Longest label the strip will draw before cutting it. Chosen against the
/// 56 px strip: ten characters plus an ellipsis is what fits without the
/// badge growing and shoving its neighbours.
pub const STRIP_LABEL_MAX: usize = 10;

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

    /// What the strip draws under the crab.
    pub fn strip_label(&self) -> String {
        strip_label(self.label())
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

    pub fn entries(&self) -> &[InstanceEntry] {
        &self.entries
    }

    pub fn get(&self, host_id: &str) -> Option<&InstanceEntry> {
        self.entries.iter().find(|e| e.host_id() == host_id)
    }

    /// Record a probe result. An id we no longer hold is ignored: the probe
    /// was in flight when the store was read again.
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
}

/// Cut a label to what the strip can draw, on a character boundary, with a
/// single ellipsis. Whitespace is collapsed first: a label pasted from a
/// hostname list can carry a tab and the strip would draw a gap.
pub fn strip_label(label: &str) -> String {
    let collapsed: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = collapsed.chars().count();
    if count <= STRIP_LABEL_MAX {
        return collapsed;
    }
    let mut out: String = collapsed
        .chars()
        .take(STRIP_LABEL_MAX.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// "last seen" for the badge's title, from a pair of unix timestamps.
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
