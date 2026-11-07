//! The pane tree (#2472): the window's panes as a recursive structure,
//! replacing the one-secondary-pane model. A leaf holds a session and its
//! share of its parent's span; a split lays its children along one axis and
//! holds the shares. A tree of one leaf is the unsplit window, so today's
//! plain chat is the base case, not a special one.

use crate::ui_core::split::{EVEN_SHARE, Layout, pane_floor};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// One node of the pane tree.
///
/// `Serialize`/`Deserialize` exist so a client can put the tree on disk and
/// get it back on the next launch: the terminal saves it to `layout.json`,
/// the window to its own `gui_layout.json` (#2675).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaneTree {
    /// A pane showing one session. `share` is this pane's fraction of its
    /// parent split's span along the split axis; a root leaf's share is 1.
    Leaf { session: Uuid, share: f32 },
    /// Children laid out along one axis, in draw order. `share` is this
    /// split's fraction of its parent split's span, exactly like a leaf's;
    /// siblings' shares sum to 1, and the root's share is 1.
    Split {
        layout: Layout,
        share: f32,
        children: Vec<PaneTree>,
    },
}

fn set_share(node: &mut PaneTree, share: f32) {
    match node {
        PaneTree::Leaf { share: s, .. } => *s = share,
        PaneTree::Split { share: s, .. } => *s = share,
    }
}

fn add_share(node: &mut PaneTree, by: f32) {
    match node {
        PaneTree::Leaf { share: s, .. } => *s += by,
        PaneTree::Split { share: s, .. } => *s += by,
    }
}

/// Lift every child share under `floor` up to it, taking the room off the
/// largest share. The sum stays at 1 and every floor holds, because the
/// deficit moves instead of being divided away by a renormalize.
fn clamp_floors(children: &mut [PaneTree], floor: f32) {
    for _ in 0..children.len() {
        let Some(small) = (0..children.len()).find(|&i| children[i].share() < floor - 1e-6) else {
            return;
        };
        let deficit = floor - children[small].share();
        set_share(&mut children[small], floor);
        let big = (0..children.len())
            .max_by(|&x, &y| children[x].share().total_cmp(&children[y].share()))
            .expect("children is non-empty");
        add_share(&mut children[big], -deficit);
    }
}

/// Bring a split's children's shares back to a sum of 1.
fn normalize_shares(children: &mut [PaneTree]) {
    if children.is_empty() {
        return;
    }
    let total: f32 = children.iter().map(|child| child.share()).sum();
    if !total.is_finite() || total <= 0.0 {
        let each = 1.0 / children.len() as f32;
        for child in children.iter_mut() {
            set_share(child, each);
        }
        return;
    }
    for child in children.iter_mut() {
        set_share(child, child.share() / total);
    }
}

impl PaneTree {
    /// The unsplit window: one pane, the whole span.
    pub fn leaf(session: Uuid) -> Self {
        PaneTree::Leaf {
            session,
            share: 1.0,
        }
    }

    /// This node's share of its parent split's span. The root returns 1.
    pub fn share(&self) -> f32 {
        match self {
            PaneTree::Leaf { share, .. } => *share,
            PaneTree::Split { share, .. } => *share,
        }
    }

    /// Split the leaf showing `session` in two along `layout`, the newcomer
    /// sitting after it. Both halves start even; `false` when no leaf holds
    /// that session, or when the tree is one leaf and the newcomer IS that
    /// session, which would split a session with itself.
    pub fn split(&mut self, session: Uuid, layout: Layout, new_session: Uuid) -> bool {
        if self.leaves().len() == 1 && self.contains(new_session) {
            return false;
        }
        match self {
            PaneTree::Leaf { session: s, share } if *s == session => {
                let half = *share / 2.0;
                *self = PaneTree::Split {
                    layout,
                    share: *share,
                    children: vec![
                        PaneTree::Leaf {
                            session: *s,
                            share: half,
                        },
                        PaneTree::Leaf {
                            session: new_session,
                            share: half,
                        },
                    ],
                };
                true
            }
            PaneTree::Split { children, .. } => children
                .iter_mut()
                .any(|child| child.split(session, layout, new_session)),
            PaneTree::Leaf { .. } => false,
        }
    }

    /// Close the leaf showing `session`: its share is shared out evenly
    /// among the siblings it leaves behind, and a split down to one child
    /// collapses into that child, which keeps the space the split held.
    /// Returns the first surviving sibling, or `None` when no leaf holds
    /// the session.
    pub fn close(&mut self, session: Uuid) -> Option<Uuid> {
        let kept_share = self.share();
        let PaneTree::Split { children, .. } = self else {
            return None;
        };
        // A direct child leaf closes here: remove it, spread its share,
        // collapse a split left with one child.
        if let Some(at) = children
            .iter()
            .position(|child| matches!(child, PaneTree::Leaf { session: s, .. } if *s == session))
        {
            let removed = children[at].share();
            children.remove(at);
            let each = removed / children.len() as f32;
            let absorbed = children
                .iter()
                .flat_map(|child| child.leaves())
                .next()
                .expect("a split keeps at least one leaf");
            for sibling in children.iter_mut() {
                add_share(sibling, each);
            }
            if children.len() == 1 {
                let mut survivor = children.remove(0);
                set_share(&mut survivor, kept_share);
                *self = survivor;
            } else {
                normalize_shares(children);
            }
            return Some(absorbed);
        }
        // Otherwise the session lives under one child split: recurse there.
        // A child that collapsed to a leaf stays in its slot, so the draw
        // order never moves under a close, and it kept the share the split
        // held, so the siblings' sum stays at 1 with no renormalize.
        let at = children.iter().position(|child| child.contains(session))?;
        let absorbed = children[at].close(session)?;
        Some(absorbed)
    }

    /// Every session in visual order: depth first, children in draw order,
    /// so cycling focus and drawing dividers both walk the same sequence.
    pub fn leaves(&self) -> Vec<Uuid> {
        match self {
            PaneTree::Leaf { session, .. } => vec![*session],
            PaneTree::Split { children, .. } => {
                children.iter().flat_map(|child| child.leaves()).collect()
            }
        }
    }

    /// Whether any leaf shows this session.
    pub fn contains(&self, session: Uuid) -> bool {
        match self {
            PaneTree::Leaf { session: s, .. } => *s == session,
            PaneTree::Split { children, .. } => {
                children.iter().any(|child| child.contains(session))
            }
        }
    }

    /// Clamp every split's children so no pane can sit under the pixel floor
    /// of its own span (#2472, #2684). `span` is this node's span along its
    /// split axis in px; the root passes the pane area. The floor is
    /// [`pane_floor`]: the fixed [`crate::ui_core::split::MIN_PANE_PX`] while
    /// the span can afford it, a proportional one when it cannot. A span too
    /// small to hold every child at that minimum leaves the shares alone:
    /// shrinking a pane below the floor would be the one outcome a resize
    /// must never produce.
    pub fn normalize(&mut self, span: f32) {
        if !span.is_finite() || span <= 0.0 {
            return;
        }
        let PaneTree::Split { children, .. } = self else {
            return;
        };
        normalize_shares(children);
        let floor = pane_floor(span, children.len());
        // Below an even share the floor still leaves a range to trade in. At
        // or above it every child already sits under the floor, and lifting
        // them would be the only outcome a resize must never produce.
        if children.len() >= 2 && floor < EVEN_SHARE {
            clamp_floors(children, floor);
        }
        let child_spans: Vec<f32> = children.iter().map(|child| child.share() * span).collect();
        for (child, child_span) in children.iter_mut().zip(child_spans) {
            child.normalize(child_span);
        }
    }

    /// The fraction of the window's width a leaf covers, with the splits that
    /// stack above it contributing nothing to the product: a stacked pane
    /// spans the full width of the split it sits in (#2472, #2473). `None`
    /// when no leaf shows that session.
    pub fn leaf_width_share(&self, session: Uuid) -> Option<f32> {
        match self {
            PaneTree::Leaf { session: s, .. } if *s == session => Some(1.0),
            PaneTree::Leaf { .. } => None,
            PaneTree::Split {
                layout, children, ..
            } => {
                let mut total = 0.0;
                for child in children {
                    let under = match child.leaf_width_share(session) {
                        Some(fraction) => fraction,
                        None => continue,
                    };
                    total += if layout.is_stacked() {
                        under
                    } else {
                        under * child.share()
                    };
                }
                (total > 0.0).then_some(total)
            }
        }
    }

    /// Point the leaf showing `from` at `to` instead, which is how the chat
    /// moves to another conversation without the tree losing its shape
    /// (#2472). `false` when no leaf shows `from`.
    pub fn rename_leaf(&mut self, from: Uuid, to: Uuid) -> bool {
        match self {
            PaneTree::Leaf { session, .. } if *session == from => {
                *session = to;
                true
            }
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { children, .. } => {
                children.iter_mut().any(|child| child.rename_leaf(from, to))
            }
        }
    }

    /// Put the pair around `divider` back to an even share of what they hold
    /// together, which is what a double click on the rule asks for (#2476).
    pub fn even_divider(&mut self, path: &[usize], divider: usize) -> bool {
        let Some(node) = self.at(path) else {
            return false;
        };
        let PaneTree::Split { children, .. } = node else {
            return false;
        };
        if divider + 1 >= children.len() {
            return false;
        }
        let pair = children[divider].share() + children[divider + 1].share();
        set_share(&mut children[divider], pair / 2.0);
        set_share(&mut children[divider + 1], pair / 2.0);
        true
    }

    /// Move the divider after child `divider` by `travelled` px across the
    /// split's `span` px along its own axis (#2472, #2476). The two children
    /// the divider separates keep their combined share, so the rest of the
    /// split never moves, and [`crate::ui_core::split::share_after_drag`]
    /// holds the pixel floor on both sides. `false` when the path is not a
    /// split or the divider has no pair to move.
    pub fn drag_divider(
        &mut self,
        path: &[usize],
        divider: usize,
        travelled: f32,
        span: f32,
    ) -> bool {
        let Some(node) = self.at(path) else {
            return false;
        };
        let PaneTree::Split { children, .. } = node else {
            return false;
        };
        if divider + 1 >= children.len() {
            return false;
        }
        let pair = children[divider].share() + children[divider + 1].share();
        let pair_span = pair * span;
        if pair <= 0.0 || !pair_span.is_finite() || pair_span <= 0.0 {
            return false;
        }
        let left = children[divider].share();
        let moved = crate::ui_core::split::share_after_drag(left / pair, travelled, pair_span);
        set_share(&mut children[divider], moved * pair);
        set_share(&mut children[divider + 1], (1.0 - moved) * pair);
        true
    }

    /// The node at a child-index path from this node, for drag addressing.
    pub fn at(&mut self, path: &[usize]) -> Option<&mut PaneTree> {
        let mut node = self;
        for step in path {
            node = match node {
                PaneTree::Split { children, .. } => children.get_mut(*step)?,
                PaneTree::Leaf { .. } => return None,
            };
        }
        Some(node)
    }

    /// Sessions this tree holds more than once, which no state machine of
    /// ours should ever produce: a session lives in one pane only.
    pub fn duplicates(&self) -> Vec<Uuid> {
        let mut seen = HashSet::new();
        let mut dupes = Vec::new();
        for session in self.leaves() {
            if !seen.insert(session) {
                dupes.push(session);
            }
        }
        dupes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_core::split::Layout;
    use uuid::uuid;

    fn a() -> Uuid {
        uuid!("00000000-0000-0000-0000-00000000000a")
    }
    fn b() -> Uuid {
        uuid!("00000000-0000-0000-0000-00000000000b")
    }
    fn c() -> Uuid {
        uuid!("00000000-0000-0000-0000-00000000000c")
    }

    /// A session split in two: both halves even, both present, draw order
    /// puts the old pane first.
    #[test]
    fn splitting_a_leaf_makes_two_even_siblings() {
        let mut tree = PaneTree::leaf(a());
        assert!(tree.split(a(), Layout::Side, b()));
        assert_eq!(tree.leaves(), vec![a(), b()]);
        let PaneTree::Split { children, .. } = &tree else {
            panic!("root must be a split now");
        };
        assert_eq!(children[0].share(), 0.5);
        assert_eq!(children[1].share(), 0.5);
    }

    /// Splitting a session with itself is refused: one session, one pane.
    #[test]
    fn splitting_the_only_leaf_with_itself_is_refused() {
        let mut tree = PaneTree::leaf(a());
        assert!(!tree.split(a(), Layout::Side, a()));
        assert!(matches!(tree, PaneTree::Leaf { .. }));
    }

    /// Closing one pane of a two-pane split collapses the tree to the
    /// survivor, which takes the closed share with it. That one-leaf tree
    /// is the unsplit window again.
    #[test]
    fn closing_one_of_two_leaves_collapses_to_the_survivor() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        // Drag the split to 0.75/0.25 first, so the share handover shows.
        if let PaneTree::Split { children, .. } = &mut tree {
            set_share(&mut children[0], 0.75);
            set_share(&mut children[1], 0.25);
        }
        let absorbed = tree.close(b()).expect("the survivor absorbs");
        assert_eq!(absorbed, a());
        assert_eq!(tree.leaves(), vec![a()]);
        assert_eq!(tree.share(), 1.0, "the survivor holds the whole span");
    }

    /// Closing the first pane lets the second keep the window: the tree
    /// collapses to the session that stays, whatever its position.
    #[test]
    fn closing_the_first_leaf_hands_the_window_to_the_second() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        let absorbed = tree.close(a()).expect("the survivor absorbs");
        assert_eq!(absorbed, b());
        assert_eq!(tree.leaves(), vec![b()]);
    }

    /// A nested split: close a deep leaf and its sibling takes the room,
    /// the outer tree staying a split because other panes remain, and the
    /// surviving nested leaf keeping the slot the split held.
    #[test]
    fn closing_inside_a_nested_split_gives_the_room_to_the_sibling() {
        let mut tree = PaneTree::leaf(a());
        assert!(tree.split(a(), Layout::Side, b()));
        // Split the SECOND pane again: a beside a b-over-c stack.
        assert!(tree.split(b(), Layout::Stack, c()));
        assert_eq!(tree.leaves(), vec![a(), b(), c()]);
        let absorbed = tree.close(b()).expect("the sibling absorbs");
        assert_eq!(absorbed, c());
        assert_eq!(tree.leaves(), vec![a(), c()]);
        // The outer split's first child still holds a's half; the second
        // child, once the b/c split, is now c's leaf with the same share.
        let PaneTree::Split { children, .. } = &tree else {
            panic!("the outer split must survive");
        };
        assert_eq!(children[0].share(), 0.5, "a keeps its half");
        assert_eq!(children[1].share(), 0.5, "the stack's slot keeps its half");
        let PaneTree::Leaf {
            session: survivor, ..
        } = &children[1]
        else {
            panic!("the collapsed child is a leaf now");
        };
        assert_eq!(*survivor, c());
    }

    /// `contains` answers for leaves at any depth.
    #[test]
    fn contains_reaches_nested_leaves() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        assert!(tree.contains(a()));
        assert!(tree.contains(b()));
        assert!(!tree.contains(c()));
    }

    /// Normalize lifts any pane under the floor pixel minimum and takes
    /// the room off the largest sibling, keeping the sum at 1.
    #[test]
    fn normalize_clamps_shares_to_the_pixel_floor() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        if let PaneTree::Split { children, .. } = &mut tree {
            set_share(&mut children[0], 0.99);
            set_share(&mut children[1], 0.01);
        }
        // A 2000px span: the floor is 240/2000 = 0.12 per pane.
        tree.normalize(2000.0);
        let PaneTree::Split { children, .. } = &tree else {
            panic!("still a split");
        };
        assert!(
            children[1].share() >= 0.12 - 1e-4,
            "small pane lifted to the floor, got {}",
            children[1].share()
        );
        let total: f32 = children.iter().map(|child| child.share()).sum();
        assert!((total - 1.0).abs() < 1e-4, "shares still sum to 1");
    }

    /// A span too small for two minimum panes is left alone: no share
    /// change, because the only outcomes would be overlap or collapse.
    #[test]
    fn normalize_leaves_a_span_too_small_to_split_alone() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        if let PaneTree::Split { children, .. } = &mut tree {
            set_share(&mut children[0], 0.7);
            set_share(&mut children[1], 0.3);
        }
        tree.normalize(100.0);
        let PaneTree::Split { children, .. } = &tree else {
            panic!("still a split");
        };
        assert_eq!(children[0].share(), 0.7);
        assert_eq!(children[1].share(), 0.3);
    }

    /// A session appears in exactly one pane, and the tree can prove it.
    #[test]
    fn duplicates_reports_a_session_held_twice() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        assert!(tree.duplicates().is_empty());
        // Force a duplicate the way a bug would: replace a leaf's session.
        if let PaneTree::Split { children, .. } = &mut tree {
            children[1] = PaneTree::Leaf {
                session: a(),
                share: 0.5,
            };
        }
        assert_eq!(tree.duplicates(), vec![a()]);
    }

    /// Dragging the divider moves the pair it separates and nothing else:
    /// the two children keep their combined share, so a three-pane split
    /// leaves the third pane exactly where it was (#2472, #2476).
    #[test]
    fn dragging_a_divider_moves_only_its_pair() {
        // Three panes at a quarter, a quarter, a half, so the third pane is
        // not a divider neighbour of the drag and must not move. The span is
        // wide enough that the 240px floor does not clip the drag: the pair
        // holds 1200px of it, so 120px of travel is a tenth of the pair.
        let mut tree = PaneTree::Split {
            layout: Layout::Side,
            share: 1.0,
            children: vec![
                PaneTree::Leaf {
                    session: a(),
                    share: 0.25,
                },
                PaneTree::Leaf {
                    session: b(),
                    share: 0.25,
                },
                PaneTree::Leaf {
                    session: c(),
                    share: 0.5,
                },
            ],
        };
        assert!(tree.drag_divider(&[], 0, 120.0, 2400.0));
        let PaneTree::Split { children, .. } = &tree else {
            panic!("still a split");
        };
        assert!((children[0].share() - 0.3).abs() < 1e-4);
        assert!((children[1].share() - 0.2).abs() < 1e-4);
        assert!(
            (children[2].share() - 0.5).abs() < 1e-4,
            "the third pane stays"
        );
    }

    /// The drag holds the floor: a hard pull stops at the minimum pane
    /// instead of collapsing the neighbour.
    #[test]
    fn dragging_a_divider_stops_at_the_pixel_floor() {
        let mut tree = PaneTree::leaf(a());
        tree.split(a(), Layout::Side, b());
        assert!(tree.drag_divider(&[], 0, -5000.0, 1000.0));
        let PaneTree::Split { children, .. } = &tree else {
            panic!("still a split");
        };
        let floor = crate::ui_core::split::MIN_PANE_PX / 1000.0;
        assert!((children[0].share() - floor).abs() < 1e-4);
        assert!((children[1].share() - (1.0 - floor)).abs() < 1e-4);
    }

    /// A nested split takes its own path, and a divider index past the last
    /// pair is refused rather than wrapping.
    #[test]
    fn dragging_addresses_a_nested_split_by_path() {
        let mut tree = PaneTree::leaf(a());
        assert!(tree.split(a(), Layout::Side, b()));
        assert!(tree.split(b(), Layout::Stack, c()));
        assert!(
            !tree.drag_divider(&[], 1, 10.0, 2400.0),
            "no second divider"
        );
        assert!(
            !tree.drag_divider(&[0], 0, 10.0, 2400.0),
            "that child is a leaf"
        );
        // The nested pair holds half the window, 1200px of a 2400px span, so
        // 120px of travel is a tenth of the pair and stays above the floor.
        assert!(tree.drag_divider(&[1], 0, 120.0, 2400.0));
        let Some(PaneTree::Split { children, .. }) = tree.at(&[1]) else {
            panic!("the nested split is still there");
        };
        let (PaneTree::Leaf { share: top, .. }, PaneTree::Leaf { share: bottom, .. }) =
            (&children[0], &children[1])
        else {
            panic!("the nested children are leaves");
        };
        assert!((top - 0.3).abs() < 1e-4, "top got {top}");
        assert!((bottom - 0.2).abs() < 1e-4, "bottom got {bottom}");
    }

    /// Width planning reads the shares down the side splits and ignores the
    /// stacked ones, because a stacked pane spans its split's full width.
    #[test]
    fn width_share_multiplies_side_splits_and_skips_stacked_ones() {
        let mut tree = PaneTree::leaf(a());
        assert!(tree.split(a(), Layout::Side, b()));
        assert!(tree.split(b(), Layout::Stack, c()));
        let a_share = tree.leaf_width_share(a()).expect("a is a leaf");
        assert!((a_share - 0.5).abs() < 1e-4, "a got {a_share}");
        let c_share = tree.leaf_width_share(c()).expect("c is a leaf");
        assert!((c_share - 0.5).abs() < 1e-4, "c got {c_share}");
        assert!(tree.leaf_width_share(Uuid::nil()).is_none());
    }
}
