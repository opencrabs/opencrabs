//! Pane state — individual pane identity and the manager that tracks all panes.

use super::layout::{SplitDirection, SplitNode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a pane within the split layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub u32);

impl PaneId {
    /// Sentinel value for "no pane" or "root only".
    pub const ROOT: Self = Self(0);
}

/// A single pane — maps to one session displayed in a portion of the screen.
#[derive(Debug, Clone)]
pub struct Pane {
    pub id: PaneId,
    /// The session shown in this pane (`None` = waiting for session selection).
    pub session_id: Option<Uuid>,
    /// Per-pane scroll offset for the chat area.
    pub scroll_offset: usize,
    /// Auto-scroll to bottom on new content.
    pub auto_scroll: bool,
}

impl Pane {
    pub fn new(id: PaneId, session_id: Option<Uuid>) -> Self {
        Self {
            id,
            session_id,
            scroll_offset: 0,
            auto_scroll: true,
        }
    }
}

/// Serializable snapshot of the pane layout (for persistence across restarts).
#[derive(Serialize, Deserialize)]
struct LayoutSnapshot {
    root: Option<SplitNode>,
    panes: Vec<PaneSnapshot>,
    focused: PaneId,
    next_id: u32,
}

#[derive(Serialize, Deserialize)]
struct PaneSnapshot {
    id: PaneId,
    session_id: Option<Uuid>,
}

/// Manages all panes and the split layout tree.
#[derive(Debug, Clone)]
pub struct PaneManager {
    /// The layout tree root. `None` means single-pane mode (no splits).
    pub root: Option<SplitNode>,
    /// All live panes, keyed by ID.
    pub panes: Vec<Pane>,
    /// Which pane currently has focus (receives input).
    pub focused: PaneId,
    /// Monotonic counter for generating unique PaneIds.
    next_id: u32,
}

impl Default for PaneManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneManager {
    pub fn new() -> Self {
        let root_pane = Pane::new(PaneId::ROOT, None);
        Self {
            root: None,
            panes: vec![root_pane],
            focused: PaneId::ROOT,
            next_id: 1,
        }
    }

    /// Allocate a new unique PaneId.
    fn alloc_id(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Get the focused pane.
    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == self.focused)
    }

    /// Get the focused pane mutably.
    pub fn focused_pane_mut(&mut self) -> Option<&mut Pane> {
        let focused = self.focused;
        self.panes.iter_mut().find(|p| p.id == focused)
    }

    /// Get a pane by ID.
    pub fn get(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// Get a pane mutably by ID.
    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    /// Returns true if we're in split mode (more than one pane).
    pub fn is_split(&self) -> bool {
        self.panes.len() > 1
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Split the focused pane in a given direction. Returns the new pane's ID.
    /// The new pane starts with no session (awaiting selection).
    pub fn split(&mut self, direction: SplitDirection) -> PaneId {
        let new_id = self.alloc_id();
        let new_pane = Pane::new(new_id, None);
        self.panes.push(new_pane);

        let current = self.focused;
        match self.root.take() {
            None => {
                // First split: create root node from the single pane.
                self.root = Some(SplitNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(SplitNode::Leaf(current)),
                    second: Box::new(SplitNode::Leaf(new_id)),
                });
            }
            Some(tree) => {
                // Replace the focused pane's leaf with a split.
                self.root = Some(tree.replace_leaf(current, direction, new_id));
            }
        }

        // Focus moves to the new pane (for session selection).
        self.focused = new_id;
        new_id
    }

    /// Close the focused pane and return focus to a sibling.
    /// Returns `true` if a pane was closed, `false` if only one pane remains.
    pub fn close_focused(&mut self) -> bool {
        if self.panes.len() <= 1 {
            return false;
        }

        let closing = self.focused;
        self.panes.retain(|p| p.id != closing);

        // Remove from layout tree.
        if let Some(tree) = self.root.take() {
            let simplified = tree.remove_leaf(closing);
            match simplified {
                SplitNode::Leaf(id) => {
                    // Back to single pane.
                    self.root = None;
                    self.focused = id;
                }
                other => {
                    // Find a leaf to focus.
                    let new_focus = other.first_leaf();
                    self.root = Some(other);
                    self.focused = new_focus;
                }
            }
        } else {
            // No tree somehow — focus first remaining.
            self.focused = self.panes.first().map(|p| p.id).unwrap_or(PaneId::ROOT);
        }

        true
    }

    /// Close panes until one is left. Returns how many were closed. The pane
    /// that remains is the one the closing leaves focused, and the caller
    /// loads its session.
    pub fn close_all_but_focused(&mut self) -> usize {
        let mut closed = 0;
        while self.close_focused() {
            closed += 1;
        }
        closed
    }

    /// Cycle focus to the next pane in the given direction.
    pub fn focus_next(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }
        let idx = self
            .panes
            .iter()
            .position(|p| p.id == self.focused)
            .unwrap_or(0);
        let next = (idx + 1) % self.panes.len();
        self.focused = self.panes[next].id;
    }

    /// Cycle focus to the previous pane.
    pub fn focus_prev(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }
        let idx = self
            .panes
            .iter()
            .position(|p| p.id == self.focused)
            .unwrap_or(0);
        let prev = if idx == 0 {
            self.panes.len() - 1
        } else {
            idx - 1
        };
        self.focused = self.panes[prev].id;
    }

    /// Collect all pane IDs in tree order (for rendering).
    pub fn pane_ids_in_order(&self) -> Vec<PaneId> {
        match &self.root {
            None => vec![self.panes.first().map(|p| p.id).unwrap_or(PaneId::ROOT)],
            Some(tree) => tree.leaves(),
        }
    }

    // ── Layout persistence ───────────────────────────────────────────────

    /// Path to the layout persistence file.
    fn layout_path() -> std::path::PathBuf {
        crate::config::opencrabs_home().join("layout.json")
    }

    /// Save current layout to disk.
    pub fn save_layout(&self) {
        // Don't persist single-pane (default) state — removing the file
        // signals "no custom layout" so startup stays fast.
        if !self.is_split() {
            if let Err(e) = std::fs::remove_file(Self::layout_path())
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("Failed to remove layout.json: {}", e);
            }
            return;
        }
        let snapshot = LayoutSnapshot {
            root: self.root.clone(),
            panes: self
                .panes
                .iter()
                .map(|p| PaneSnapshot {
                    id: p.id,
                    session_id: p.session_id,
                })
                .collect(),
            focused: self.focused,
            next_id: self.next_id,
        };
        if let Ok(json) = serde_json::to_string_pretty(&snapshot)
            && let Err(e) = std::fs::write(Self::layout_path(), json)
        {
            tracing::warn!("Failed to save layout.json: {}", e);
        }
    }

    /// Restore layout from disk. Returns default single-pane if file is
    /// missing or corrupt.
    pub fn load_layout() -> Self {
        let path = Self::layout_path();
        let Ok(data) = std::fs::read_to_string(&path) else {
            return Self::new();
        };
        let Ok(snapshot) = serde_json::from_str::<LayoutSnapshot>(&data) else {
            tracing::warn!("Corrupt layout.json — starting with single pane");
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("Failed to remove corrupt layout.json: {}", e);
            }
            return Self::new();
        };
        let panes: Vec<Pane> = snapshot
            .panes
            .into_iter()
            .map(|s| Pane::new(s.id, s.session_id))
            .collect();
        if panes.is_empty() {
            return Self::new();
        }
        Self {
            root: snapshot.root,
            panes,
            focused: snapshot.focused,
            next_id: snapshot.next_id,
        }
    }
}
