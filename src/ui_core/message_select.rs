//! The rule for selecting whole transcript rows (#2613): entering select
//! mode, toggling one row, extending from an anchor in either direction, and
//! the order a forward takes them in.
//!
//! The window owns a [`Selection`]; this decides what each click means. It is
//! generic over the row key so the transcript's message ids and a test's
//! strings share one implementation, and it never depends on the toolkit.

use std::collections::BTreeSet;

/// Entered select mode plus the rows picked so far.
///
/// The set is keyed, not positional: a row that scrolls out of the viewport,
/// or a refresh that re-renders only the visible window, must not lose what
/// was already picked. That is why this is a set of ids and not a pair of
/// indices.
#[derive(Clone, Debug, Default)]
pub struct Selection<K> {
    active: bool,
    anchor: Option<K>,
    picked: BTreeSet<K>,
}

impl<K: Ord + Clone> Selection<K> {
    pub fn new() -> Self {
        Self {
            active: false,
            anchor: None,
            picked: BTreeSet::new(),
        }
    }

    /// Select mode has been entered from the message menu. Nothing is picked
    /// yet, and the bar shows `0 selected`.
    pub fn begin(&mut self) {
        self.active = true;
    }

    /// Whether the bar should replace the composer.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Leave select mode and drop every pick.
    pub fn cancel(&mut self) {
        self.active = false;
        self.anchor = None;
        self.picked.clear();
    }

    /// Pick or unpick one row, and make it the anchor a later shift-click
    /// extends from.
    pub fn toggle(&mut self, id: K) {
        if !self.picked.remove(&id) {
            self.picked.insert(id.clone());
        }
        self.anchor = Some(id);
    }

    pub fn contains(&self, id: &K) -> bool {
        self.picked.contains(id)
    }

    pub fn count(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    /// Shift-click: union the rows between the anchor and `id` into the pick.
    ///
    /// Union, not replace, because a shift-click in the middle of a
    /// selection is how a second run of rows is added. Without an anchor the
    /// click is a plain toggle of that row. An `id` that is not in `order`
    /// (a row that vanished under a refresh) changes nothing and reports so.
    pub fn extend(&mut self, order: &[K], id: &K) -> bool {
        let Some(anchor) = self.anchor.clone() else {
            self.toggle(id.clone());
            return true;
        };
        let Some((lo, hi)) = span(order, &anchor, id) else {
            return false;
        };
        for row in &order[lo..=hi] {
            self.picked.insert(row.clone());
        }
        self.anchor = Some(id.clone());
        true
    }

    /// The picked rows in transcript order, not in click order. A forward
    /// writes them contiguously in the order the source showed them, so the
    /// destination reads the same conversation.
    pub fn picked_in_order(&self, order: &[K]) -> Vec<K> {
        order
            .iter()
            .filter(|row| self.picked.contains(*row))
            .cloned()
            .collect()
    }
}

/// The inclusive index range `order` covers from `a` to `b`, in either
/// direction. `None` when either end is absent, so a stale anchor cannot
/// produce a range that silently starts somewhere else.
pub fn span<K: PartialEq>(order: &[K], a: &K, b: &K) -> Option<(usize, usize)> {
    let ia = order.iter().position(|row| row == a)?;
    let ib = order.iter().position(|row| row == b)?;
    Some((ia.min(ib), ia.max(ib)))
}

/// The actions the message menu wires, in the order it shows them (#2613).
///
/// The type is the contract, not a label list: `Reply`, `Pin`, `Translate`,
/// `Delete` and `Copy Message Link` each need state or a path that does not
/// exist yet, and this repo never ships a dead menu entry. A caller that
/// builds the menu by iterating [`MessageAction::ALL`] cannot offer an item
/// the app cannot carry out, because there is no variant for it to reach for.
///
/// `Forward` was here and is gone on the owner's instruction: a right press on
/// a history row is not how he wants to send a past message somewhere else
/// (#2827). Bulk forward from the selection bar is untouched, because that is
/// a deliberate flow he opened rather than a menu entry next to Copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAction {
    CopyText,
    /// Put one of the pack's crabs on the row (#2825).
    React,
    Select,
}

impl MessageAction {
    /// Every wired action, in menu order.
    pub const ALL: [MessageAction; 3] = [
        MessageAction::CopyText,
        MessageAction::React,
        MessageAction::Select,
    ];

    /// The label the menu paints.
    pub fn label(self) -> &'static str {
        match self {
            MessageAction::CopyText => "Copy Text",
            MessageAction::React => "React",
            MessageAction::Select => "Select",
        }
    }
}
