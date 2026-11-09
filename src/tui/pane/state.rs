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
    /// Half-typed input owned by this pane. Stashed here when focus moves
    /// away and adopted back when focus returns, so what you were typing
    /// stays with the pane you typed it into (#2494).
    pub input_buffer: String,
    /// Cursor byte offset within `input_buffer`, stashed alongside it.
    pub cursor_position: usize,
    /// Input-history navigation position, stashed alongside the buffer.
    pub input_history_index: Option<usize>,
    /// The draft a recall stashed, carried with the pane. Without it a recall
    /// that spans a focus change loses the sentence it was covering (#2494).
    pub input_history_stash: String,
    /// Plan document cached for this pane's session. Inactive panes keep
    /// theirs, so a split layout can render each pane's own plan without
    /// a disk read per frame (#2494).
    pub plan_document: Option<crate::tui::plan::PlanDocument>,
    /// Resolved plan JSON path for this pane's session, cached with the doc.
    pub plan_file_path: Option<std::path::PathBuf>,
    /// mtime of `plan_file_path` when `plan_document` was read. The tick
    /// compares it against the file so a pane the user is NOT looking at
    /// still notices a task that settled there (#2733).
    pub plan_mtime: Option<std::time::SystemTime>,
}

impl Pane {
    pub fn new(id: PaneId, session_id: Option<Uuid>) -> Self {
        Self {
            id,
            session_id,
            scroll_offset: 0,
            auto_scroll: true,
            input_buffer: String::new(),
            cursor_position: 0,
            input_history_index: None,
            input_history_stash: String::new(),
            plan_document: None,
            plan_file_path: None,
            plan_mtime: None,
        }
    }

    /// Drop the pane's stashed input state.
    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
        self.cursor_position = 0;
        self.input_history_index = None;
        self.input_history_stash.clear();
    }
}

/// The mtime of a plan file, or `None` when it cannot be read.
///
/// This is the reload gate for a pane's cached plan: compare it with the
/// stored stamp and reload only on a real change.
fn plan_stamp(path: Option<&std::path::Path>) -> Option<std::time::SystemTime> {
    let path = path?;
    std::fs::metadata(path).ok()?.modified().ok()
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

    /// Stash the app's working input into the currently focused pane,
    /// leaving the working fields empty. Call BEFORE moving focus.
    pub fn stash_input(
        &mut self,
        buffer: &mut String,
        cursor: &mut usize,
        history: &mut Option<usize>,
        history_stash: &mut String,
    ) {
        if let Some(pane) = self.focused_pane_mut() {
            pane.input_buffer = std::mem::take(buffer);
            pane.cursor_position = *cursor;
            *cursor = 0;
            pane.input_history_index = history.take();
            pane.input_history_stash = std::mem::take(history_stash);
        }
    }

    /// Adopt the focused pane's stashed input into the app's working
    /// fields. Call AFTER moving focus.
    pub fn adopt_input(
        &mut self,
        buffer: &mut String,
        cursor: &mut usize,
        history: &mut Option<usize>,
        history_stash: &mut String,
    ) {
        if let Some(pane) = self.focused_pane() {
            *buffer = pane.input_buffer.clone();
            *cursor = pane.cursor_position;
            *history = pane.input_history_index;
            *history_stash = pane.input_history_stash.clone();
        }
    }

    /// Replace the focused pane's cached plan with a freshly loaded pair.
    pub fn set_focused_plan(
        &mut self,
        path: Option<std::path::PathBuf>,
        doc: Option<crate::tui::plan::PlanDocument>,
    ) {
        let focused = self.focused;
        self.set_pane_plan(focused, path, doc);
    }

    /// Replace one pane's cached plan with a freshly loaded pair.
    ///
    /// The mtime is stamped here, at the read, because it is the only moment
    /// that knows which version of the file the document came from.
    pub fn set_pane_plan(
        &mut self,
        id: PaneId,
        path: Option<std::path::PathBuf>,
        doc: Option<crate::tui::plan::PlanDocument>,
    ) {
        if let Some(pane) = self.get_mut(id) {
            pane.plan_mtime = plan_stamp(path.as_deref());
            pane.plan_file_path = path;
            pane.plan_document = doc;
        }
    }

    /// Reload every pane's plan document whose file changed since it was read.
    ///
    /// Returns how many documents were replaced. The mtime is the gate and it
    /// matters: `load_plan_from_path` retires legacy terminal plans from disk
    /// (archives Completed, deletes Cancelled), so reloading on every tick
    /// would mutate plan files for no reason. An untouched file costs one
    /// `stat` per pane.
    pub fn refresh_plans_changed_on_disk(&mut self) -> usize {
        let mut reloaded = 0;
        for pane in &mut self.panes {
            let Some(path) = pane.plan_file_path.clone() else {
                continue;
            };
            let stamp = plan_stamp(Some(path.as_path()));
            // A vanished file keeps the last document: there is nothing newer
            // to draw, and clearing it would erase a finished checklist the
            // user may still be reading.
            let Some(stamp) = stamp else { continue };
            if pane.plan_mtime == Some(stamp) {
                continue;
            }
            pane.plan_document = crate::utils::plan_files::load_plan_from_path(&path);
            pane.plan_mtime = Some(stamp);
            reloaded += 1;
        }
        reloaded
    }

    /// Resolve a plan file for every pane that shows a session but has no
    /// path yet.
    ///
    /// A split restored from `layout.json` comes back with session ids and an
    /// empty plan cache, and only the focused pane resolves a path when its
    /// session loads. Without this a background pane can never watch its own
    /// plan, so its settle waits for the click that finally loads it (#2733).
    pub async fn bind_missing_plan_paths(&mut self) -> usize {
        let missing: Vec<(PaneId, Uuid)> = self
            .panes
            .iter()
            .filter(|pane| pane.plan_file_path.is_none())
            .filter_map(|pane| pane.session_id.map(|session| (pane.id, session)))
            .collect();
        let mut bound = 0;
        for (id, session_id) in missing {
            let path = crate::utils::plan_files::plan_json_read_path(session_id).await;
            // Stamp before the read: a write that lands mid-read leaves the
            // doc newer than the stamp, so the next tick reloads instead of
            // trusting a document it cannot date.
            let stamp = plan_stamp(Some(path.as_path()));
            let doc = crate::utils::plan_files::load_plan_from_path(&path);
            if let Some(pane) = self.get_mut(id) {
                pane.plan_file_path = Some(path);
                pane.plan_document = doc;
                pane.plan_mtime = stamp;
                bound += 1;
            }
        }
        bound
    }

    /// Null the focused pane's cached plan document, keeping the resolved
    /// path so a later reload still knows where to look.
    pub fn clear_focused_plan(&mut self) {
        if let Some(pane) = self.focused_pane_mut() {
            pane.plan_document = None;
        }
    }

    /// The focused pane's cached plan document.
    pub fn focused_plan(&self) -> Option<&crate::tui::plan::PlanDocument> {
        self.focused_pane().and_then(|p| p.plan_document.as_ref())
    }

    /// The focused pane's resolved plan JSON path.
    pub fn focused_plan_path(&self) -> Option<&std::path::PathBuf> {
        self.focused_pane().and_then(|p| p.plan_file_path.as_ref())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::pane::layout::SplitDirection;
    use crate::tui::plan::PlanDocument;

    fn doc(title: &str) -> PlanDocument {
        serde_json::from_str(&format!(r#"{{"tasks":[],"title":"{title}"}}"#))
            .expect("minimal plan document parses")
    }

    #[test]
    fn stash_and_adopt_round_trip_keeps_buffers_per_pane() {
        let mut mgr = PaneManager::new();

        // The real `|` sequence: the user typed into the working buffer while
        // ROOT was focused, then the split handler stashes BEFORE splitting
        // (split moves focus to the new pane).
        let (mut buf, mut cur, mut hist) = ("alpha".to_string(), 5usize, Some(2usize));
        let mut stash = "alpha pre-recall".to_string();
        mgr.stash_input(&mut buf, &mut cur, &mut hist, &mut stash);
        mgr.split(SplitDirection::Horizontal);
        mgr.adopt_input(&mut buf, &mut cur, &mut hist, &mut stash);

        // The new pane starts empty; ROOT kept what we typed.
        assert_eq!(buf, "");
        assert_eq!(cur, 0);
        assert_eq!(hist, None);
        assert_eq!(mgr.get(PaneId::ROOT).unwrap().input_buffer, "alpha");
        assert_eq!(mgr.get(PaneId::ROOT).unwrap().cursor_position, 5);
        assert_eq!(mgr.get(PaneId::ROOT).unwrap().input_history_index, Some(2));
        assert_eq!(
            mgr.get(PaneId::ROOT).unwrap().input_history_stash,
            "alpha pre-recall"
        );
        assert!(stash.is_empty(), "the stash moved into the pane");

        // Type here, cycle back, and the first pane's buffer comes home.
        // The real Tab handler order: stash the working buffer into the pane
        // being left, THEN move focus, THEN adopt the destination's buffer.
        buf = "beta".to_string();
        cur = 4;
        hist = Some(1);
        stash = "beta pre-recall".to_string();
        mgr.stash_input(&mut buf, &mut cur, &mut hist, &mut stash); // pane[1] keeps beta
        mgr.focus_next(); // wraps to the first pane
        mgr.adopt_input(&mut buf, &mut cur, &mut hist, &mut stash); // buf = alpha
        assert_eq!(buf, "alpha");
        assert_eq!(cur, 5);
        assert_eq!(hist, Some(2));
        assert_eq!(stash, "alpha pre-recall", "the recall draft comes home");
        assert_eq!(mgr.get(mgr.focused).unwrap().input_buffer, "alpha");
        // And the second pane still holds its own text.
        assert_eq!(mgr.get(mgr.panes[1].id).unwrap().input_buffer, "beta");
    }

    #[test]
    fn inactive_pane_keeps_its_plan_after_focus_moves() {
        let mut mgr = PaneManager::new();
        mgr.set_focused_plan(
            Some(std::path::PathBuf::from("plans/a.json")),
            Some(doc("first")),
        );
        mgr.split(SplitDirection::Horizontal);
        mgr.set_focused_plan(
            Some(std::path::PathBuf::from("plans/b.json")),
            Some(doc("second")),
        );

        // The focused (new) pane reads the fresh pair...
        assert_eq!(mgr.focused_plan().map(|d| d.title.as_str()), Some("second"));
        assert_eq!(
            mgr.focused_plan_path().map(|p| p.display().to_string()),
            Some("plans/b.json".to_string())
        );
        // ...while the inactive pane still owns its own plan, so a split
        // render can show it without a reload (#2494).
        assert_eq!(
            mgr.get(PaneId::ROOT)
                .unwrap()
                .plan_document
                .as_ref()
                .map(|d| d.title.as_str()),
            Some("first")
        );
    }

    #[test]
    fn clear_focused_plan_keeps_the_resolved_path() {
        let mut mgr = PaneManager::new();
        mgr.set_focused_plan(
            Some(std::path::PathBuf::from("plans/a.json")),
            Some(doc("first")),
        );
        mgr.clear_focused_plan();
        assert!(mgr.focused_plan().is_none());
        assert_eq!(
            mgr.focused_plan_path().map(|p| p.display().to_string()),
            Some("plans/a.json".to_string())
        );
    }

    #[test]
    fn clear_input_resets_buffer_cursor_and_history() {
        let mut pane = Pane::new(PaneId::ROOT, None);
        pane.input_buffer = "half typed".to_string();
        pane.cursor_position = 10;
        pane.input_history_index = Some(1);
        pane.clear_input();
        assert!(pane.input_buffer.is_empty());
        assert_eq!(pane.cursor_position, 0);
        assert_eq!(pane.input_history_index, None);
    }

    #[test]
    fn single_pane_stash_adopt_round_trip_preserves_input() {
        let mut mgr = PaneManager::new();
        let (mut buf, mut cur, mut hist) = ("solo".to_string(), 4usize, None);
        let mut stash = "solo pre-recall".to_string();
        mgr.stash_input(&mut buf, &mut cur, &mut hist, &mut stash);
        assert_eq!(mgr.get(PaneId::ROOT).unwrap().input_buffer, "solo");
        mgr.adopt_input(&mut buf, &mut cur, &mut hist, &mut stash);
        assert_eq!(buf, "solo");
        assert_eq!(cur, 4);
        assert_eq!(hist, None);
        assert_eq!(stash, "solo pre-recall");
    }
}
