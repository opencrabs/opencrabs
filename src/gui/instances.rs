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

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

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
        if let Some(existing) = self.entries.iter_mut().find(|e| e.host_id() == known.host_id) {
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

    pub fn selected(&self) -> Option<&InstanceEntry> {
        let id = self.selected.as_deref()?;
        self.get(id)
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

/// Cut a label to what the strip can draw, on a character boundary, with a
/// single ellipsis. Whitespace is collapsed first: a label pasted from a
/// hostname list can carry a tab and the strip would draw a gap.
pub fn strip_label(label: &str) -> String {
    let collapsed: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = collapsed.chars().count();
    if count <= STRIP_LABEL_MAX {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(STRIP_LABEL_MAX.saturating_sub(1)).collect();
    out.push('…');
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn known(id: &str, label: &str) -> KnownInstance {
        KnownInstance {
            host_id: id.to_string(),
            label: label.to_string(),
            url: format!("http://{id}:7878"),
            token: "ocpair_test".to_string(),
            added_at: 100,
            last_ok: 0,
        }
    }

    fn file(ids: &[(&str, &str)]) -> InstanceFile {
        InstanceFile {
            version: 1,
            instances: ids.iter().map(|(id, l)| known(id, l)).collect(),
        }
    }

    #[test]
    fn from_file_keeps_order_and_starts_unknown() {
        let set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
        assert_eq!(set.len(), 2);
        assert_eq!(set.entries()[0].label(), "alpha");
        assert_eq!(set.entries()[1].label(), "beta");
        assert!(set.entries().iter().all(|e| e.health == Health::Unknown));
        assert_eq!(set.selected_host_id(), None);
    }

    #[test]
    fn health_never_reorders() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
        set.record_probe("b:2", Ok(12), 500);
        set.record_probe("a:1", Err(()), 500);
        assert_eq!(set.entries()[0].label(), "alpha");
        assert!(set.entries()[0].health.is_down());
        assert!(set.entries()[1].health.is_up());
    }

    #[test]
    fn upsert_keeps_position_and_health() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
        set.record_probe("a:1", Ok(3), 500);
        let mut renamed = known("a:1", "alpha nova");
        renamed.url = "http://10.0.0.5:7878".to_string();
        set.upsert(renamed);
        assert_eq!(set.len(), 2);
        assert_eq!(set.entries()[0].label(), "alpha nova");
        assert_eq!(set.entries()[0].url(), "http://10.0.0.5:7878");
        assert!(set.entries()[0].health.is_up());
    }

    #[test]
    fn remove_clears_a_selection_that_pointed_at_it() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
        assert!(set.select("b:2"));
        assert!(set.remove("b:2"));
        assert_eq!(set.selected_host_id(), None);
        assert!(!set.remove("b:2"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn probe_for_a_removed_instance_is_ignored() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
        set.remove("a:1");
        set.record_probe("a:1", Ok(5), 900);
        assert!(set.is_empty());
    }

    #[test]
    fn select_step_wraps_both_ways() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
        assert_eq!(set.select_step(1), Some("a:1"));
        assert_eq!(set.select_step(1), Some("b:2"));
        assert_eq!(set.select_step(1), Some("a:1"));
        assert_eq!(set.select_step(-1), Some("b:2"));
        let mut empty = Instances::default();
        assert_eq!(empty.select_step(1), None);
    }

    #[test]
    fn select_refuses_an_unknown_id() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
        assert!(!set.select("zzz:9"));
        assert_eq!(set.selected_host_id(), None);
    }

    #[test]
    fn strip_label_collapses_and_cuts_on_char_boundaries() {
        assert_eq!(strip_label("server i7"), "server i7");
        assert_eq!(strip_label("  server   i7  "), "server i7");
        assert_eq!(strip_label("portátil do norte"), "portátil …");
        // Multi-byte throughout: the cut must not split a code point.
        assert_eq!(strip_label("açãoçãoçãoção"), "açãoçãoç…");
        assert_eq!(strip_label(""), "");
    }

    #[test]
    fn age_label_buckets() {
        assert_eq!(age_label(1_000, 0), "nunca");
        assert_eq!(age_label(1_000, 990), "agora");
        assert_eq!(age_label(1_000, 700), "5 min");
        assert_eq!(age_label(10_000, 1_000), "2 h");
        assert_eq!(age_label(200_000, 1_000), "2 d");
        // A clock that went backwards must not print a huge number.
        assert_eq!(age_label(500, 900), "agora");
    }

    #[test]
    fn badge_title_names_the_state() {
        let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
        set.record_probe("a:1", Ok(7), 100);
        let title = badge_title(&set.entries()[0], 100);
        assert!(title.contains("alpha"));
        assert!(title.contains("http://a:1:7878"));
        assert!(title.contains("7 ms"));
        assert!(badge_title(&Instances::from_file(&file(&[("b:2", "b")])).entries()[0], 5)
            .contains("por verificar"));
    }
}
